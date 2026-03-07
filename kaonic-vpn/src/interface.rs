#![cfg(feature = "run")]

use std::sync::Arc;

use kaonic_ctrl::client::Client;
use kaonic_ctrl::error::ControllerError;
use kaonic_ctrl::protocol::{Message, MessageCoder, RADIO_FRAME_SIZE};
use kaonic_ctrl::radio::RadioClient;
use kaonic_frame::frame::Frame;
use reticulum::buffer::{InputBuffer, OutputBuffer};
use reticulum::iface::{Interface, InterfaceContext, RxMessage};
use reticulum::packet::Packet;
use reticulum::serde::Serialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::{FrameRecord, KaonicCtrlConfig};

const MTU: usize = 1400;
const R: usize = 5;

/// Reticulum interface that forwards packets through the kaonic radio hardware
/// via the kaonic-ctrl UDP control protocol.
pub struct KaonicCtrlInterface {
    radio_client: Arc<Mutex<RadioClient>>,
    module: usize,
    pub frame_tx: Option<tokio::sync::mpsc::UnboundedSender<FrameRecord>>,
}

impl KaonicCtrlInterface {
    /// Connect to the kaonic-ctrl daemon and create the interface.
    pub async fn connect(
        config: &KaonicCtrlConfig,
        cancel: CancellationToken,
    ) -> Result<Self, ControllerError> {
        let client = Client::<Message>::connect::<MTU, R, MessageCoder<MTU, R>>(
            config.listen_addr,
            config.server_addr,
            MessageCoder::new(),
            cancel.clone(),
        )
        .await?;

        let radio_client = RadioClient::new(client, cancel).await?;

        Ok(Self {
            radio_client: Arc::new(Mutex::new(radio_client)),
            module: config.module,
            frame_tx: None,
        })
    }

    /// Spawn the interface tasks. Matches the pattern used by other Reticulum interfaces.
    pub async fn spawn(context: InterfaceContext<Self>) {
        let (radio_client, module, frame_tx) = {
            let inner = context.inner.lock().unwrap();
            (inner.radio_client.clone(), inner.module, inner.frame_tx.clone())
        };

        let iface_address = context.channel.address;
        let (rx_channel, tx_channel) = context.channel.split();
        let cancel = context.cancel;

        let mut rx_recv = radio_client.lock().await.module_receive();

        let tx_channel = Arc::new(Mutex::new(tx_channel));

        let rx_task = {
            let cancel = cancel.clone();
            let rx_channel = rx_channel.clone();

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(recv_module) = rx_recv.recv() => {
                            let bytes = recv_module.frame.as_slice();

                            if let Some(tx) = &frame_tx {
                                let preview: String = bytes.iter()
                                    .take(32)
                                    .map(|b| format!("{b:02x}"))
                                    .collect();
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs() as i64;
                                let _ = tx.send(FrameRecord {
                                    id: 0,
                                    timestamp: now,
                                    module: recv_module.module,
                                    size: bytes.len(),
                                    data_hex: preview,
                                });
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

            tokio::spawn(async move {
                const BUF_SIZE: usize = reticulum::packet::PACKET_MDU * 2;
                let mut tx_buffer = [0u8; BUF_SIZE];

                loop {
                    let mut tx_ch = tx_channel.lock().await;

                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Some(message) = tx_ch.recv() => {
                            drop(tx_ch);

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

