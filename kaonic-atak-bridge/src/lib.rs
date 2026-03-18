use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;

use rand_core::OsRng;
use reticulum::destination::DestinationName;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::Transport;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Bridges ATAK UDP broadcast traffic (port 6969 by default) to/from a
/// Reticulum destination. Every UDP packet received is forwarded to all
/// established incoming Reticulum links; every data packet received over
/// those links is re-broadcast on the local UDP network.
pub struct AtakBridge {
    transport: Arc<Mutex<Transport>>,
    identity: PrivateIdentity,
    udp_port: u16,
}

impl AtakBridge {
    pub fn new(transport: Arc<Mutex<Transport>>, identity: PrivateIdentity, udp_port: u16) -> Self {
        Self {
            transport,
            identity,
            udp_port,
        }
    }

    pub async fn run(
        self,
        cancel: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let destination = self
            .transport
            .lock()
            .await
            .add_destination(self.identity, DestinationName::new("kaonic", "atak"))
            .await;

        let dest_hash = destination.lock().await.desc.address_hash;

        let announce = destination
            .lock()
            .await
            .announce(OsRng, None)
            .map_err(|e| format!("announce error: {e:?}"))?;
        self.transport.lock().await.send_packet(announce).await;
        log::info!("atak-bridge: announced destination {dest_hash}");

        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, self.udp_port);
        let udp = Arc::new(UdpSocket::bind(bind_addr).await?);
        udp.set_broadcast(true)?;
        log::info!("atak-bridge: listening on UDP :{}", self.udp_port);

        let broadcast_target: SocketAddr =
            SocketAddrV4::new(Ipv4Addr::BROADCAST, self.udp_port).into();

        // Reticulum → UDP
        let rns_to_udp = {
            let transport = self.transport.clone();
            let udp = udp.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                let mut data_rx = transport.lock().await.received_data_events();
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(received) = data_rx.recv() => {
                            if received.destination == dest_hash {
                                let data = received.data.as_slice();
                                log::debug!(
                                    "atak-bridge: rns→udp {} bytes | {}",
                                    data.len(),
                                    hex_preview(data),
                                );
                                if let Err(err) = udp.send_to(data, broadcast_target).await {
                                    log::warn!("atak-bridge: udp send error: {err}");
                                }
                            }
                        }
                    }
                }
            })
        };

        // UDP → Reticulum
        let udp_to_rns = {
            let transport = self.transport.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                let mut buf = vec![0u8; 65535];
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok((len, src)) = udp.recv_from(&mut buf) => {
                            let data = &buf[..len];
                            log::debug!(
                                "atak-bridge: udp -> rns {} bytes from {src}",
                                len,
                            );
                            transport.lock().await
                                .send_to_in_links(&dest_hash, data)
                                .await;
                        }
                    }
                }
            })
        };

        // Periodic re-announce
        let reannounce = {
            let transport = self.transport.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));

                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            if let Ok(announce) = destination.lock().await.announce(OsRng, None) {
                                transport.lock().await.send_packet(announce).await;
                                log::debug!("atak-bridge: re-announced destination");
                            }
                        }
                    }
                }
            })
        };

        let _ = tokio::join!(rns_to_udp, udp_to_rns, reannounce);
        Ok(())
    }
}

/// Format up to the first 16 bytes of `data` as a hex string for log previews.
fn hex_preview(data: &[u8]) -> String {
    let n = data.len().min(16);
    let hex: String = data[..n]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    if data.len() > 16 {
        format!("{hex} …")
    } else {
        hex
    }
}
