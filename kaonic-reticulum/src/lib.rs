use std::net::SocketAddr;
use std::sync::Arc;

use kaonic_ctrl::client::Client;
use kaonic_ctrl::error::ControllerError;
use kaonic_ctrl::protocol::{Message, MessageCoder, RADIO_FRAME_SIZE};
use kaonic_ctrl::radio::RadioClient;
use kaonic_frame::frame::Frame;
use radio_common::{Modulation, RadioConfig};
use reticulum::buffer::{InputBuffer, OutputBuffer};
use reticulum::iface::{Interface, InterfaceContext, RxMessage};
use reticulum::packet::Packet;
use reticulum::serde::Serialize;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

/// Reticulum interface that forwards packets through the kaonic radio hardware
/// via the kaonic-ctrl UDP control protocol.
pub struct KaonicCtrlInterface {
    radio_client: Arc<Mutex<RadioClient>>,
    module: usize,
    /// Send a new `RadioConfig` to apply it on the running interface.
    pub radio_config_tx: mpsc::Sender<RadioConfig>,
    /// Send a new `Modulation` to apply it on the running interface.
    pub modulation_tx: mpsc::Sender<Modulation>,
    radio_config_rx: Option<mpsc::Receiver<RadioConfig>>,
    modulation_rx: Option<mpsc::Receiver<Modulation>>,
}

impl KaonicCtrlInterface {
    /// Connect to the kaonic-ctrl daemon and create the interface.
    pub async fn connect<const MTU: usize, const R: usize>(
        listen_addr: SocketAddr,
        server_addr: SocketAddr,
        module: usize,
        cancel: CancellationToken,
    ) -> Result<Self, ControllerError> {
        let client = Client::<Message>::connect::<MTU, R, MessageCoder<MTU, R>>(
            listen_addr,
            server_addr,
            MessageCoder::new(),
            cancel.clone(),
        )
        .await?;

        let radio_client = RadioClient::new(client, cancel).await?;

        let (radio_config_tx, radio_config_rx) = mpsc::channel(8);
        let (modulation_tx, modulation_rx) = mpsc::channel(8);

        Ok(Self {
            radio_client: Arc::new(Mutex::new(radio_client)),
            module,
            radio_config_tx,
            modulation_tx,
            radio_config_rx: Some(radio_config_rx),
            modulation_rx: Some(modulation_rx),
        })
    }

    /// Spawn the interface tasks. Matches the pattern used by other Reticulum interfaces.
    pub async fn spawn(context: InterfaceContext<Self>) {
        let (radio_client, module, radio_config_rx, modulation_rx) = {
            let mut inner = context.inner.lock().unwrap();
            (
                inner.radio_client.clone(),
                inner.module,
                inner.radio_config_rx.take().expect("spawn called twice"),
                inner.modulation_rx.take().expect("spawn called twice"),
            )
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

        let cfg_task = {
            let cancel = cancel.clone();
            let radio_client = radio_client.clone();
            let mut radio_config_rx = radio_config_rx;
            let mut modulation_rx = modulation_rx;

            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Some(config) = radio_config_rx.recv() => {
                            if let Err(err) = radio_client.lock().await
                                .set_radio_config(module, config)
                                .await
                            {
                                log::warn!("kaonic_ctrl: set_radio_config error: {err:?}");
                            }
                        }
                        Some(modulation) = modulation_rx.recv() => {
                            if let Err(err) = radio_client.lock().await
                                .set_modulation(module, modulation)
                                .await
                            {
                                log::warn!("kaonic_ctrl: set_modulation error: {err:?}");
                            }
                        }
                    }
                }
            })
        };

        let _ = tokio::join!(rx_task, tx_task, cfg_task);
    }
}

impl Interface for KaonicCtrlInterface {
    fn mtu() -> usize {
        RADIO_FRAME_SIZE
    }
}
