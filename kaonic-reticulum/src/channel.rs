//! The SDK surface for programs that use the radio: connect to the daemon,
//! open channels, and — when a program wants Reticulum semantics — run a
//! Reticulum interface over a channel.
//!
//! Everything a plugin needs is reachable from this module; it never has to
//! name a `kaonic-radio` crate. The channel layer itself is re-exported
//! unchanged, so there is exactly one `Channel` type in the system.

use std::net::SocketAddr;
use std::sync::Arc;

use kaonic_ctrl::client::Client;
use kaonic_ctrl::error::ControllerError;
use kaonic_ctrl::protocol::{Message, MessageCoder, RADIO_FRAME_SIZE};
use kaonic_ctrl::radio::RadioClient;
use reticulum::buffer::{InputBuffer, OutputBuffer};
use reticulum::hash::AddressHash;
use reticulum::iface::{Interface, InterfaceContext, RxMessage};
use reticulum::packet::Packet;
use reticulum::serde::Serialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use kaonic_ctrl::channel::{
    Channel, ChannelBuilder, ChannelError, ChannelEvent, ChannelId, ChannelLimits, ChannelRx,
    ChannelStats, ChannelTx, CodingSpec, DispatchSpec, DropReason, Priority, ProfileSpec,
    QueuePolicy, Received, Runtime, RxInfo, Tdd,
};
pub use kaonic_fec::{Adaptation, FecCode, FecSelector, FixedCode, PeerKey, TrafficClass, TxHint};

/// Ergonomic constructors for the daemon's profiles; each converts into the
/// [`ProfileSpec`] the builder sends.
pub mod profiles {
    use std::time::Duration;

    use super::{FecCode, ProfileSpec, TrafficClass};

    /// TM2048 on every frame, blocking queue, interactive priority. What every
    /// node does on the air today.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Robust;

    /// Adaptive code per peer, frames filled by bundling, long bursts, and
    /// yields to anything interactive.
    #[derive(Debug, Clone, Copy)]
    pub struct Bulk {
        pub class: TrafficClass,
    }

    /// A fixed light code, newest packet wins, real-time priority, and one
    /// frame per packet interval so several codec packets share a frame.
    #[derive(Debug, Clone, Copy)]
    pub struct Voice {
        pub code: FecCode,
        pub packet_interval: Duration,
    }

    /// No framing, no coding: one payload is one PSDU.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Raw;

    impl Default for Bulk {
        fn default() -> Self {
            Self {
                class: TrafficClass::Auto,
            }
        }
    }

    impl Default for Voice {
        fn default() -> Self {
            Self {
                code: FecCode::Tm1280,
                packet_interval: Duration::from_millis(20),
            }
        }
    }

    impl From<Robust> for ProfileSpec {
        fn from(_: Robust) -> Self {
            ProfileSpec::Robust
        }
    }

    impl From<Bulk> for ProfileSpec {
        fn from(bulk: Bulk) -> Self {
            ProfileSpec::Bulk { class: bulk.class }
        }
    }

    impl From<Voice> for ProfileSpec {
        fn from(voice: Voice) -> Self {
            ProfileSpec::Voice {
                code: voice.code,
                packet_interval: voice.packet_interval,
            }
        }
    }

    impl From<Raw> for ProfileSpec {
        fn from(_: Raw) -> Self {
            ProfileSpec::Raw
        }
    }
}

/// Shared handle to the radio daemon; what the gateway already passes around.
pub type SharedRadioClient = Arc<Mutex<RadioClient>>;

/// Connection to the local radio daemon. Cheap to clone; one per process is
/// enough for any number of channels.
#[derive(Clone)]
pub struct Radio {
    client: SharedRadioClient,
}

impl Radio {
    /// Connects to `daemon` (`127.0.0.1:9090` on a node).
    pub async fn connect(daemon: SocketAddr, cancel: CancellationToken) -> Result<Self, ControllerError> {
        let client = Client::<Message>::connect::<1400, 5, MessageCoder<1400, 5>>(
            "0.0.0.0:0".parse().expect("wildcard address"),
            daemon,
            MessageCoder::new(),
            cancel.clone(),
        )
        .await?;
        let client = RadioClient::new(client, cancel).await?;
        Ok(Self {
            client: Arc::new(Mutex::new(client)),
        })
    }

    /// Wraps a client the program already holds.
    pub fn from_shared(client: SharedRadioClient) -> Self {
        Self { client }
    }

    /// The underlying client, for module configuration and frame-level use.
    pub fn client(&self) -> SharedRadioClient {
        self.client.clone()
    }

    /// Starts opening a channel on module 0.
    pub async fn channel(&self, id: ChannelId) -> ChannelBuilder {
        self.client.lock().await.channel(id)
    }

    /// Starts opening a channel bonded across every radio the node has: one
    /// payload's frames are spread over all of them, so throughput scales with
    /// the radios and the channel's MTU grows with them too.
    ///
    /// Both ends must open the channel the same way. On a node with a single
    /// radio this is exactly [`channel`](Self::channel), so a program can ask
    /// for bonding unconditionally.
    pub async fn bonded_channel(&self, id: ChannelId) -> Result<ChannelBuilder, ControllerError> {
        let mut client = self.client.lock().await;
        let modules = client.get_info().await?.module_count.max(1);
        Ok(client.channel(id).modules(0..modules))
    }
}

/// Sees every Reticulum packet received on the channel, with its RSSI.
pub type PacketObserver = Arc<dyn Fn(&Packet, i8) + Send + Sync>;
/// Sees the serialised bytes of every packet handed to the channel.
pub type TxBytesObserver = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// Reticulum `Interface` over one channel. Serialises packets out through the
/// channel's transmit half and parses what the receive half delivers; the
/// daemon does the rest.
pub struct ReticulumChannel {
    tx: ChannelTx,
    rx: std::sync::Mutex<Option<ChannelRx>>,
    observer: Option<PacketObserver>,
    tx_observer: Option<TxBytesObserver>,
    /// Hand packets to the daemon without waiting for its answer.
    posted: bool,
}

impl ReticulumChannel {
    pub fn new(channel: Channel) -> Self {
        let (tx, rx) = channel.split();
        Self {
            tx,
            rx: std::sync::Mutex::new(Some(rx)),
            observer: None,
            tx_observer: None,
            posted: false,
        }
    }

    /// Send without waiting for the daemon to accept each packet. Every
    /// `send` is otherwise a round trip over the ctrl link, which caps a
    /// stream at a couple of hundred packets a second and, past that, lets
    /// Reticulum's outbound queue grow without bound — seconds of latency
    /// with nothing dropped. For a real-time channel on a `DropOldest` queue
    /// the daemon already decides what to keep, so there is nothing to wait
    /// for.
    pub fn with_posted_sends(mut self) -> Self {
        self.posted = true;
        self
    }

    /// Attach an observer that sees the bytes of every packet sent.
    pub fn with_tx_observer(mut self, observer: TxBytesObserver) -> Self {
        self.tx_observer = Some(observer);
        self
    }

    /// Attach an observer that sees every received packet with its RSSI;
    /// the remote node map and the dashboard use it.
    pub fn with_observer(mut self, observer: PacketObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn tx(&self) -> &ChannelTx {
        &self.tx
    }

    /// Runs the two forwarding tasks; matches the pattern of the other
    /// Reticulum interfaces.
    pub async fn spawn(context: InterfaceContext<Self>) {
        let (tx, mut rx, observer, tx_observer, posted) = {
            let inner = context.inner.lock().unwrap_or_else(|e| e.into_inner());
            let rx = inner
                .rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .expect("a ReticulumChannel spawns once");
            (
                inner.tx.clone(),
                rx,
                inner.observer.clone(),
                inner.tx_observer.clone(),
                inner.posted,
            )
        };
        let address = context.channel.address;
        let (to_reticulum, mut from_reticulum) = context.channel.split();
        let cancel = context.cancel;

        // Reticulum → air
        {
            let tx = tx.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; reticulum::packet::PACKET_MDU * 2];
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Some(message) = from_reticulum.recv() => {
                            let mut output = OutputBuffer::new(buffer.as_mut_slice());
                            if message.packet.serialize(&mut output).is_err() {
                                log::warn!("reticulum channel: packet serialize failed");
                                continue;
                            }
                            let bytes = output.as_slice();
                            if let Some(observer) = tx_observer.as_ref() {
                                observer(bytes);
                            }
                            let hint = TxHint {
                                peer: Some(peer_key(&message.packet.destination)),
                                class: None,
                                len: bytes.len(),
                            };
                            let sent = if posted {
                                tx.try_send_with(bytes, hint)
                            } else {
                                tx.send_with(bytes, hint).await
                            };
                            if let Err(err) = sent {
                                log::warn!("reticulum channel: send failed: {err}");
                            }
                        }
                    }
                }
            });
        }

        // air → Reticulum
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    received = rx.recv() => {
                        let received = match received {
                            Ok(received) => received,
                            Err(err) => {
                                log::warn!("reticulum channel: receive stopped: {err}");
                                break;
                            }
                        };
                        let mut input = InputBuffer::new(&received.payload);
                        let packet = match Packet::deserialize(&mut input) {
                            Ok(packet) => packet,
                            Err(_) => {
                                log::debug!("reticulum channel: not a reticulum packet ({} B)", received.payload.len());
                                continue;
                            }
                        };
                        // What we heard from that station drives the code we use toward it.
                        tx.observe_peer(peer_key(&packet.destination), Some(received.info.rssi), None);
                        if let Some(observer) = observer.as_ref() {
                            observer(&packet, received.info.rssi);
                        }
                        let _ = to_reticulum.send(RxMessage { address, packet }).await;
                    }
                }
            }
        });
    }
}

impl Interface for ReticulumChannel {
    fn mtu() -> usize {
        RADIO_FRAME_SIZE
    }
}

/// The adaptation layer keys peers by an opaque 16-byte value; a Reticulum
/// address hash is exactly that.
pub fn peer_key(address: &AddressHash) -> PeerKey {
    let mut key = [0u8; 16];
    let bytes = address.as_slice();
    let n = bytes.len().min(16);
    key[..n].copy_from_slice(&bytes[..n]);
    key
}
