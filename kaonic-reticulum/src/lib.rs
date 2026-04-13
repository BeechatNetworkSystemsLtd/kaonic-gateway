use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kaonic_ctrl::client::Client;
use kaonic_ctrl::error::ControllerError;
use kaonic_ctrl::protocol::{Message, MessageCoder, RADIO_FRAME_SIZE};
use kaonic_frame::frame::Frame;
use reticulum::buffer::{InputBuffer, OutputBuffer};
use reticulum::iface::{Interface, InterfaceContext, RxMessage};
use reticulum::packet::Packet;
use reticulum::serde::Serialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use kaonic_ctrl::radio::RadioClient;

pub type TxObserver = Arc<dyn Fn(usize, &[u8]) + Send + Sync>;

/// Reticulum interface that forwards packets through the kaonic radio hardware
/// via the kaonic-ctrl UDP control protocol.
///
/// A single `RadioClient` connection handles all hardware modules; the module
/// index is passed as a parameter on every call. Create one `RadioClient` via
/// `connect_client`, then build one `KaonicCtrlInterface` per module with `new`.
pub struct KaonicCtrlInterface {
    radio_client: Arc<Mutex<RadioClient>>,
    module: usize,
    tx_observer: Option<TxObserver>,
}

const TX_ECHO_WINDOW: Duration = Duration::from_millis(500);
const TX_ECHO_CACHE_SIZE: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecentFrame {
    fingerprint: u64,
    len: usize,
    recorded_at: Instant,
}

#[derive(Default)]
struct RecentTransmitCache {
    entries: VecDeque<RecentFrame>,
}

impl RecentTransmitCache {
    fn remember(&mut self, bytes: &[u8], now: Instant) {
        self.prune(now);
        self.entries.push_back(RecentFrame {
            fingerprint: fingerprint(bytes),
            len: bytes.len(),
            recorded_at: now,
        });
        while self.entries.len() > TX_ECHO_CACHE_SIZE {
            self.entries.pop_front();
        }
    }

    fn matches_recent_echo(&mut self, bytes: &[u8], now: Instant) -> bool {
        self.prune(now);
        let fingerprint = fingerprint(bytes);
        self.entries
            .iter()
            .any(|entry| entry.len == bytes.len() && entry.fingerprint == fingerprint)
    }

    fn prune(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|entry| now.duration_since(entry.recorded_at) > TX_ECHO_WINDOW)
        {
            self.entries.pop_front();
        }
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

impl KaonicCtrlInterface {
    /// Connect to the kaonic-ctrl daemon and return the shared client.
    /// One connection is sufficient for all hardware modules.
    pub async fn connect_client<const MTU: usize, const R: usize>(
        listen_addr: SocketAddr,
        server_addr: SocketAddr,
        cancel: CancellationToken,
    ) -> Result<Arc<Mutex<RadioClient>>, ControllerError> {
        let client = Client::<Message>::connect::<MTU, R, MessageCoder<MTU, R>>(
            listen_addr,
            server_addr,
            MessageCoder::new(),
            cancel.clone(),
        )
        .await?;
        Ok(Arc::new(Mutex::new(
            RadioClient::new(client, cancel).await?,
        )))
    }

    /// Create an interface for `module` using an already-connected `RadioClient`.
    pub fn new(
        radio_client: Arc<Mutex<RadioClient>>,
        module: usize,
        tx_observer: Option<TxObserver>,
    ) -> Self {
        Self {
            radio_client,
            module,
            tx_observer,
        }
    }

    /// Spawn the interface tasks. Matches the pattern used by other Reticulum interfaces.
    pub async fn spawn(context: InterfaceContext<Self>) {
        let (radio_client, module, tx_observer) = {
            let inner = context.inner.lock().unwrap();
            (
                inner.radio_client.clone(),
                inner.module,
                inner.tx_observer.clone(),
            )
        };

        let iface_address = context.channel.address;
        let (rx_channel, mut tx_channel) = context.channel.split();
        let cancel = context.cancel;
        let recent_tx = Arc::new(Mutex::new(RecentTransmitCache::default()));

        let mut rx_recv = radio_client.lock().await.module_receive();

        let rx_task = {
            let cancel = cancel.clone();
            let rx_channel = rx_channel.clone();
            let recent_tx = recent_tx.clone();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(recv_module) = rx_recv.recv() => {

                            let bytes = recv_module.frame.as_slice();
                            let now = Instant::now();
                            if recent_tx.lock().await.matches_recent_echo(bytes, now) {
                                log::trace!("kaonic_ctrl: dropped echoed tx frame on module {module}");
                                continue;
                            }

                            let mut input = InputBuffer::new(bytes);
                            match Packet::deserialize(&mut input) {
                                Ok(packet) => {
                                    let _ = rx_channel
                                        .send(RxMessage { address: iface_address, packet })
                                        .await;
                                }
                                Err(err) => {
                                    log::warn!("kaonic_ctrl: failed to deserialize packet: {err:?}");
                                }
                            }
                        }
                    }
                }
            })
        };

        let tx_task = {
            let cancel = cancel.clone();
            let radio_client = radio_client.clone();
            let recent_tx = recent_tx.clone();
            let tx_observer = tx_observer.clone();

            tokio::spawn(async move {
                const BUF_SIZE: usize = reticulum::packet::PACKET_MDU * 2;
                let mut tx_buffer = [0u8; BUF_SIZE];

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Some(message) = tx_channel.recv() => {
                            let mut output = OutputBuffer::new(&mut tx_buffer);
                            if let Ok(_) = message.packet.serialize(&mut output) {
                                let bytes = output.as_slice();
                                let mut frame = Frame::<RADIO_FRAME_SIZE>::new();
                                frame.copy_from_slice(bytes);
                                recent_tx.lock().await.remember(bytes, Instant::now());

                                if let Err(err) = radio_client.lock().await
                                    .transmit(module, &frame)
                                    .await
                                {
                                    log::warn!("kaonic_ctrl: tx error: {err:?}");
                                } else if let Some(observer) = &tx_observer {
                                    observer(module, bytes);
                                }
                            }
                        }
                    }
                }
            })
        };

        let _ = tokio::join!(rx_task, tx_task);
    }
}

impl Interface for KaonicCtrlInterface {
    fn mtu() -> usize {
        RADIO_FRAME_SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_transmit_cache_matches_identical_frame_within_window() {
        let mut cache = RecentTransmitCache::default();
        let now = Instant::now();
        let frame = b"hello";
        cache.remember(frame, now);
        assert!(cache.matches_recent_echo(frame, now + Duration::from_millis(100)));
    }

    #[test]
    fn recent_transmit_cache_expires_frames_after_window() {
        let mut cache = RecentTransmitCache::default();
        let now = Instant::now();
        let frame = b"hello";
        cache.remember(frame, now);
        assert!(!cache.matches_recent_echo(frame, now + TX_ECHO_WINDOW + Duration::from_millis(1)));
    }

    #[test]
    fn recent_transmit_cache_does_not_match_different_payload() {
        let mut cache = RecentTransmitCache::default();
        let now = Instant::now();
        cache.remember(b"hello", now);
        assert!(!cache.matches_recent_echo(b"world", now + Duration::from_millis(50)));
    }
}
