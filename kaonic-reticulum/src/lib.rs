use std::net::SocketAddr;
use std::sync::Arc;

use kaonic_ctrl::client::Client;
use kaonic_ctrl::error::ControllerError;
use kaonic_ctrl::protocol::{Message, MessageCoder, RADIO_FRAME_SIZE};
use kaonic_frame::frame::{Frame, FrameSegment};
use kaonic_net::{
    coder::LdpcPacketCoder, error::NetworkError as KaonicNetError,
    network::Network as KaonicNetNetwork,
};
use rand::rngs::OsRng;
use reticulum::buffer::{InputBuffer, OutputBuffer};
use reticulum::hash::AddressHash;
use reticulum::iface::{Interface, InterfaceContext, RxMessage, TxMessage};
use reticulum::packet::Packet;
use reticulum::serde::Serialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub use kaonic_ctrl::radio::RadioClient;

pub use kaonic_fec::{CoderStats, FecCode, TrafficClass};

/// Channels: the payload-level API every radio user builds on. The gateway's
/// Reticulum interface, plugins and tools all go through here.
pub mod channel;
pub use channel::{profiles, Radio, ReticulumChannel};

/// FEC adaptation keyed by Reticulum destination address. The policy itself
/// lives in `kaonic-fec` next to the radio, and knows nothing about Reticulum.
pub type FecSelector = kaonic_fec::FecSelector<AddressHash>;

pub type TxObserver = Arc<dyn Fn(usize, &[u8]) + Send + Sync>;
pub type ErrorObserver = Arc<dyn Fn(usize, InterfaceErrorKind) + Send + Sync>;
/// Observes every successfully reassembled and deserialized Reticulum packet
/// together with the RSSI of the radio frame that completed it:
/// `(module, rssi_dbm, packet)`.
pub type RxObserver = Arc<dyn Fn(usize, i8, &Packet) + Send + Sync>;
const LDPC_SEGMENTS_PER_PACKET: usize = 3;
const LDPC_REASSEMBLY_QUEUE: usize = 32;

type RadioPacketCoder = LdpcPacketCoder<RADIO_FRAME_SIZE>;
type RadioNetwork = KaonicNetNetwork<
    RADIO_FRAME_SIZE,
    LDPC_SEGMENTS_PER_PACKET,
    LDPC_REASSEMBLY_QUEUE,
    RadioPacketCoder,
>;
type RadioSegmentBuffer = FrameSegment<RADIO_FRAME_SIZE, LDPC_SEGMENTS_PER_PACKET>;

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
    error_observer: Option<ErrorObserver>,
    rx_observer: Option<RxObserver>,
    fec: Arc<FecSelector>,
}

#[derive(Clone, Copy, Debug)]
pub enum InterfaceErrorKind {
    RxLdpcDecode,
    RxReassembly,
    RxDeserialize,
    TxLdpcEncode,
    TxTransmit,
    TxSerialize,
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
        error_observer: Option<ErrorObserver>,
    ) -> Self {
        Self {
            radio_client,
            module,
            tx_observer,
            error_observer,
            rx_observer: None,
            fec: Arc::new(FecSelector::default()),
        }
    }

    /// Share a [`FecSelector`] so applications can steer the code per
    /// destination at runtime (default: wire-compatible TM2048 everywhere).
    pub fn with_fec_selector(mut self, fec: Arc<FecSelector>) -> Self {
        self.fec = fec;
        self
    }

    /// Attach an observer that sees every received packet with its RSSI.
    pub fn with_rx_observer(mut self, rx_observer: RxObserver) -> Self {
        self.rx_observer = Some(rx_observer);
        self
    }

    /// Spawn the interface tasks. Matches the pattern used by other Reticulum interfaces.
    pub async fn spawn(context: InterfaceContext<Self>) {
        let (radio_client, module, tx_observer, error_observer, rx_observer, fec) = {
            let inner = context.inner.lock().unwrap();
            (
                inner.radio_client.clone(),
                inner.module,
                inner.tx_observer.clone(),
                inner.error_observer.clone(),
                inner.rx_observer.clone(),
                inner.fec.clone(),
            )
        };

        let iface_address = context.channel.address;
        let (rx_channel, mut tx_channel) = context.channel.split();
        let cancel = context.cancel;

        let mut rx_recv = radio_client.lock().await.module_receive();

        let rx_task = {
            let cancel = cancel.clone();
            let rx_channel = rx_channel.clone();
            let error_observer = error_observer.clone();
            let rx_observer = rx_observer.clone();
            let fec = fec.clone();

            tokio::spawn(async move {
                // LDPC decode costs ~20 ms/frame on this single-core SoC, so
                // it runs on a blocking thread; only finished packets return
                // to the async side.
                let mut rx_state: Option<Box<RxState>> =
                    Some(Box::new((build_radio_network(), RadioSegmentBuffer::new())));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        recv = rx_recv.recv() => {
                            let recv_module = match recv {
                                Ok(recv_module) => recv_module,
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    log::warn!("kaonic_ctrl: rx frame stream lagged by {n} frames (module {module})");
                                    continue;
                                }
                                Err(_) => break,
                            };
                            if recv_module.module != module {
                                continue;
                            }
                            let current_time = network_time_now();
                            let rssi = recv_module.rssi;
                            let mut frame = Frame::<RADIO_FRAME_SIZE>::new();
                            frame.copy_from_slice(recv_module.frame.as_slice());
                            let frame_len = frame.len();
                            let preview = frame_preview(frame.as_slice());
                            let state = rx_state.take().expect("rx state");
                            let (state, outcome) = tokio::task::spawn_blocking(move || {
                                decode_frame(state, current_time, &frame)
                            })
                            .await
                            .expect("rx decode task");
                            fec.record_stats(state.0.coder().stats());
                            rx_state = Some(state);

                            match outcome {
                                Err(RxFailure::Decode(err)) => {
                                    notify_error(&error_observer, module, InterfaceErrorKind::RxLdpcDecode);
                                    log::warn!(
                                        "kaonic_ctrl: rx ldpc decode failed module={} len={} preview={} err={err:?}",
                                        module,
                                        frame_len,
                                        preview
                                    );
                                }
                                Err(RxFailure::Reassembly(err)) => {
                                    notify_error(&error_observer, module, InterfaceErrorKind::RxReassembly);
                                    log::warn!(
                                        "kaonic_ctrl: rx ldpc reassembly failed module={} len={} preview={} err={err:?}",
                                        module,
                                        frame_len,
                                        preview
                                    );
                                }
                                Ok(assembled) => {
                                    for bytes in assembled {
                                        let mut input = InputBuffer::new(&bytes);
                                        match Packet::deserialize(&mut input) {
                                            Ok(packet) => {
                                                log::trace!(
                                                    "kaonic_ctrl: rx module={} rssi={} {}",
                                                    module,
                                                    rssi,
                                                    packet_log_summary(&packet)
                                                );
                                                fec.observe_rssi(packet.destination, rssi);
                                                if let Some(observer) = rx_observer.as_ref() {
                                                    observer(module, rssi, &packet);
                                                }
                                                let _ = rx_channel
                                                    .send(RxMessage { address: iface_address, packet })
                                                    .await;
                                            }
                                            Err(err) => {
                                                notify_error(&error_observer, module, InterfaceErrorKind::RxDeserialize);
                                                log::warn!(
                                                    "kaonic_ctrl: rx deserialize failed module={} len={} preview={} err={err:?}",
                                                    module,
                                                    bytes.len(),
                                                    frame_preview(&bytes)
                                                );
                                            }
                                        }
                                    }
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
            let tx_observer = tx_observer.clone();
            let error_observer = error_observer.clone();
            let fec = fec.clone();

            tokio::spawn(async move {
                const BUF_SIZE: usize = reticulum::packet::PACKET_MDU * 2;
                let mut tx_state: Option<Box<TxState>> = Some(Box::new((
                    build_radio_network(),
                    [Frame::<RADIO_FRAME_SIZE>::new(); LDPC_SEGMENTS_PER_PACKET],
                    vec![0u8; BUF_SIZE],
                )));

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Some(message) = tx_channel.recv() => {
                            let code = fec.select(message.packet.destination);
                            transmit_message(
                                &radio_client,
                                module,
                                &tx_observer,
                                &error_observer,
                                &mut tx_state,
                                code,
                                message,
                            ).await;
                        }
                        else => break,
                    }
                }
            })
        };

        let _ = tokio::join!(rx_task, tx_task);
    }
}

type RxState = (RadioNetwork, RadioSegmentBuffer);

enum RxFailure {
    Decode(KaonicNetError),
    Reassembly(KaonicNetError),
}

/// LDPC-decode one radio frame into the reassembly network and drain every
/// packet that became complete. Runs on a blocking thread.
fn decode_frame(
    mut state: Box<RxState>,
    current_time: u128,
    frame: &Frame<RADIO_FRAME_SIZE>,
) -> (Box<RxState>, Result<Vec<Vec<u8>>, RxFailure>) {
    let (rx_network, rx_frame) = &mut *state;
    if let Err(err) = rx_network.receive(current_time, frame) {
        return (state, Err(RxFailure::Decode(err)));
    }
    let mut assembled = Vec::new();
    loop {
        match rx_network.process(current_time, rx_frame) {
            Ok(packet) => assembled.push(packet.as_slice().to_vec()),
            Err(KaonicNetError::TryAgain) => break,
            Err(err) => return (state, Err(RxFailure::Reassembly(err))),
        }
    }
    (state, Ok(assembled))
}

fn packet_log_summary(packet: &Packet) -> String {
    format!(
        "type={:?} ctx={:?} dst={} len={}",
        packet.header.packet_type,
        packet.context,
        packet.destination,
        packet.data.len()
    )
}

fn frame_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

async fn transmit_message(
    radio_client: &Arc<Mutex<RadioClient>>,
    module: usize,
    tx_observer: &Option<TxObserver>,
    error_observer: &Option<ErrorObserver>,
    tx_state: &mut Option<Box<TxState>>,
    code: FecCode,
    message: TxMessage,
) {
    let summary = packet_log_summary(&message.packet);
    // Serialization + LDPC encode (~5 ms) run off the async worker so the
    // transport keeps servicing links while a burst is being encoded.
    let mut state = tx_state.take().expect("tx state");
    let (state, encoded) = tokio::task::spawn_blocking(move || {
        let (tx_network, tx_frames, tx_buffer) = &mut *state;
        tx_network.coder_mut().set_tx_fec(code);
        let mut output = OutputBuffer::new(tx_buffer.as_mut_slice());
        let result = match message.packet.serialize(&mut output) {
            Ok(_) => {
                let bytes = output.as_slice();
                let payload_len = bytes.len();
                match tx_network.transmit(bytes, OsRng, tx_frames) {
                    Ok(frames) => Ok((payload_len, frames.to_vec())),
                    Err(err) => Err((
                        InterfaceErrorKind::TxLdpcEncode,
                        format!("{err:?}"),
                        payload_len,
                    )),
                }
            }
            Err(_) => Err((InterfaceErrorKind::TxSerialize, String::new(), 0)),
        };
        (state, result)
    })
    .await
    .expect("tx encode task");
    *tx_state = Some(state);

    match encoded {
        Ok((payload_len, frames)) => {
            log::trace!(
                "kaonic_ctrl: tx module={} {} payload_len={} encoded_frames={} fec={}",
                module,
                summary,
                payload_len,
                frames.len(),
                code.name()
            );

            let mut radio_client = radio_client.lock().await;

            match radio_client.transmit_batch(module, &frames).await {
                Ok((_, 0)) => {
                    if let Some(observer) = tx_observer {
                        for frame in frames.iter() {
                            observer(module, frame.as_slice());
                        }
                    }
                }
                Ok((sent, errors)) => {
                    notify_error(error_observer, module, InterfaceErrorKind::TxTransmit);
                    log::warn!(
                        "kaonic_ctrl: tx batch partial module={} {} payload_len={} sent={} errors={}",
                        module,
                        summary,
                        payload_len,
                        sent,
                        errors
                    );
                }
                Err(err) => {
                    notify_error(error_observer, module, InterfaceErrorKind::TxTransmit);
                    log::warn!(
                        "kaonic_ctrl: tx batch failed module={} {} payload_len={} err={err:?}",
                        module,
                        summary,
                        payload_len
                    );
                }
            }
        }
        Err((kind, err, payload_len)) => {
            notify_error(error_observer, module, kind);
            log::warn!(
                "kaonic_ctrl: tx {} failed module={} {} payload_len={} err={err}",
                match kind {
                    InterfaceErrorKind::TxSerialize => "serialize",
                    _ => "ldpc encode",
                },
                module,
                summary,
                payload_len
            );
        }
    }
    // Under sustained transmit load, explicitly yield so Reticulum
    // maintenance tasks get time to refresh links and process control traffic.
    tokio::task::yield_now().await;
}

type TxState = (
    RadioNetwork,
    [Frame<RADIO_FRAME_SIZE>; LDPC_SEGMENTS_PER_PACKET],
    Vec<u8>,
);

fn notify_error(observer: &Option<ErrorObserver>, module: usize, kind: InterfaceErrorKind) {
    if let Some(observer) = observer {
        observer(module, kind);
    }
}

fn build_radio_network() -> RadioNetwork {
    KaonicNetNetwork::new(LdpcPacketCoder::new())
}

fn network_time_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
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
    fn ldpc_radio_network_round_trips_full_reticulum_payload() {
        let original = [0x5a; reticulum::packet::PACKET_MDU];
        let mut tx_network = build_radio_network();
        let mut rx_network = build_radio_network();
        let mut tx_frames = [Frame::<RADIO_FRAME_SIZE>::new(); LDPC_SEGMENTS_PER_PACKET];
        let mut rx_frame = RadioSegmentBuffer::new();

        let frames = tx_network
            .transmit(&original, OsRng, &mut tx_frames)
            .expect("encoded ldpc frames");

        let mut recovered = None;
        for (idx, frame) in frames.iter().enumerate() {
            let ts = idx as u128;
            rx_network.receive(ts, frame).expect("accepted ldpc frame");
            if let Ok(packet) = rx_network.process(ts, &mut rx_frame) {
                recovered = Some(packet.as_slice().to_vec());
            }
        }

        assert_eq!(recovered.as_deref(), Some(original.as_slice()));
    }
}
