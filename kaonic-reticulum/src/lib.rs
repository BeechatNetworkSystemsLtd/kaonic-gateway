use std::net::SocketAddr;
use std::sync::Arc;

use kaonic_ctrl::client::Client;
use kaonic_ctrl::error::ControllerError;
use kaonic_ctrl::protocol::{Message, MessageCoder, RADIO_FRAME_SIZE};
use kaonic_frame::frame::Frame;
use reticulum::buffer::{InputBuffer, OutputBuffer};
use reticulum::iface::{Interface, InterfaceContext, RxMessage, TxMessage};
use reticulum::packet::{Packet, PacketContext, PacketType};
use reticulum::serde::Serialize;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

pub use kaonic_ctrl::radio::RadioClient;

pub type TxObserver = Arc<dyn Fn(usize, &[u8]) + Send + Sync>;

const HIGH_PRIORITY_TX_CAPACITY: usize = 64;
const NORMAL_PRIORITY_TX_CAPACITY: usize = 256;
const HIGH_PRIORITY_BURST: usize = 8;

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
        let (high_tx, mut high_rx) = mpsc::channel(HIGH_PRIORITY_TX_CAPACITY);
        let (normal_tx, mut normal_rx) = mpsc::channel(NORMAL_PRIORITY_TX_CAPACITY);

        let mut rx_recv = radio_client.lock().await.module_receive();

        let rx_task = {
            let cancel = cancel.clone();
            let rx_channel = rx_channel.clone();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(recv_module) = rx_recv.recv() => {
                            if recv_module.module == module {
                                let bytes = recv_module.frame.as_slice();
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
                }
            })
        };

        let tx_classifier_task = {
            let cancel = cancel.clone();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        maybe_message = tx_channel.recv() => match maybe_message {
                            Some(message) => {
                                let target = if is_high_priority(&message.packet) {
                                    &high_tx
                                } else {
                                    &normal_tx
                                };
                                if target.send(message).await.is_err() {
                                    log::warn!("kaonic_ctrl: priority tx queue closed");
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                }
            })
        };

        let tx_task = {
            let cancel = cancel.clone();
            let radio_client = radio_client.clone();
            let tx_observer = tx_observer.clone();

            tokio::spawn(async move {
                const BUF_SIZE: usize = reticulum::packet::PACKET_MDU * 2;
                let mut tx_buffer = [0u8; BUF_SIZE];

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Some(message) = high_rx.recv() => {
                            transmit_message(&radio_client, module, &tx_observer, &mut tx_buffer, message).await;
                            for _ in 1..HIGH_PRIORITY_BURST {
                                let Ok(message) = high_rx.try_recv() else { break; };
                                transmit_message(&radio_client, module, &tx_observer, &mut tx_buffer, message).await;
                            }
                            if let Ok(message) = normal_rx.try_recv() {
                                transmit_message(&radio_client, module, &tx_observer, &mut tx_buffer, message).await;
                            }
                        }
                        Some(message) = normal_rx.recv() => {
                            transmit_message(&radio_client, module, &tx_observer, &mut tx_buffer, message).await;
                        }
                        else => break,
                    }
                }
            })
        };

        let _ = tokio::join!(rx_task, tx_classifier_task, tx_task);
    }
}

fn is_high_priority(packet: &Packet) -> bool {
    matches!(
        (packet.header.packet_type, packet.context),
        (PacketType::LinkRequest, _)
            | (PacketType::Proof, PacketContext::LinkRequestProof)
            | (PacketType::Data, PacketContext::KeepAlive)
            | (PacketType::Data, PacketContext::LinkRTT)
            | (PacketType::Data, PacketContext::LinkClose)
    )
}

async fn transmit_message(
    radio_client: &Arc<Mutex<RadioClient>>,
    module: usize,
    tx_observer: &Option<TxObserver>,
    tx_buffer: &mut [u8],
    message: TxMessage,
) {
    let mut output = OutputBuffer::new(tx_buffer);
    if let Ok(_) = message.packet.serialize(&mut output) {
        let bytes = output.as_slice();
        let mut frame = Frame::<RADIO_FRAME_SIZE>::new();
        frame.copy_from_slice(bytes);

        if let Err(err) = radio_client.lock().await.transmit(module, &frame).await {
            log::warn!("kaonic_ctrl: tx error: {err:?}");
        } else if let Some(observer) = tx_observer {
            observer(module, bytes);
        }
    }
    // Under sustained transmit load, explicitly yield so Reticulum
    // maintenance tasks get time to refresh links and process control traffic.
    tokio::task::yield_now().await;
}

impl Interface for KaonicCtrlInterface {
    fn mtu() -> usize {
        RADIO_FRAME_SIZE
    }
}
