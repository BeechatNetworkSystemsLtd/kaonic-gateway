use std::net::SocketAddr;
use std::sync::Arc;

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

/// Reticulum interface that forwards packets through the kaonic radio hardware
/// via the kaonic-ctrl UDP control protocol.
///
/// A single `RadioClient` connection handles all hardware modules; the module
/// index is passed as a parameter on every call. Create one `RadioClient` via
/// `connect_client`, then build one `KaonicCtrlInterface` per module with `new`.
pub struct KaonicCtrlInterface {
    radio_client: Arc<Mutex<RadioClient>>,
    module: usize,
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
    pub fn new(radio_client: Arc<Mutex<RadioClient>>, module: usize) -> Self {
        Self {
            radio_client,
            module,
        }
    }

    /// Spawn the interface tasks. Matches the pattern used by other Reticulum interfaces.
    pub async fn spawn(context: InterfaceContext<Self>) {
        let (radio_client, module) = {
            let inner = context.inner.lock().unwrap();
            (inner.radio_client.clone(), inner.module)
        };

        let iface_address = context.channel.address;
        let (rx_channel, mut tx_channel) = context.channel.split();
        let cancel = context.cancel;

        let mut rx_recv = radio_client.lock().await.module_receive();

        let rx_task = {
            let cancel = cancel.clone();
            let rx_channel = rx_channel.clone();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(recv_module) = rx_recv.recv() => {

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
            })
        };

        let tx_task = {
            let cancel = cancel.clone();
            let radio_client = radio_client.clone();

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

                                if let Err(err) = radio_client.lock().await
                                    .transmit(module, &frame)
                                    .await
                                {
                                    log::warn!("kaonic_ctrl: tx error: {err:?}");
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
