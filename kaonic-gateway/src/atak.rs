use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};

use rand::rngs::OsRng;
use reticulum::destination::DestinationName;
use reticulum::hash::AddressHash;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::Transport;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// ATAK Situational Awareness multicast group (standard ATAK SA address).
const MCAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 2, 3, 1);

/// Live packet counters for one ATAK bridge instance.
pub struct BridgeMetrics {
    pub port: u16,
    pub dest_hash: OnceLock<String>,
    pub rx_packets: AtomicU64,
    pub tx_packets: AtomicU64,
}

impl BridgeMetrics {
    pub fn new(port: u16) -> Arc<Self> {
        Arc::new(Self {
            port,
            dest_hash: OnceLock::new(),
            rx_packets: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
        })
    }
}

/// Bridges ATAK UDP broadcast traffic (port 6969 by default) to/from a
/// Reticulum destination. Every UDP packet received is forwarded to all
/// established incoming Reticulum links; every data packet received over
/// those links is re-broadcast on the local UDP network.
pub struct AtakBridge {
    transport: Arc<Mutex<Transport>>,
    identity: PrivateIdentity,
    udp_port: u16,
    metrics: Arc<BridgeMetrics>,
}

impl AtakBridge {
    pub fn new(
        transport: Arc<Mutex<Transport>>,
        identity: PrivateIdentity,
        udp_port: u16,
        metrics: Arc<BridgeMetrics>,
    ) -> Self {
        Self { transport, identity, udp_port, metrics }
    }

    pub async fn run(
        self,
        cancel: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let port_bytes = self.udp_port.to_be_bytes(); // app_data tag = port as 2 big-endian bytes
        let dest_name = format!("atak.{}", self.udp_port);

        let destination = self.transport.lock().await
            .add_destination(self.identity, DestinationName::new("kaonic", &dest_name))
            .await;

        let dest_hash = destination.lock().await.desc.address_hash;
        let _ = self.metrics.dest_hash.set(dest_hash.to_hex_string());

        // Announce with port in app_data so peers can identify same-kind bridges.
        let announce = destination.lock().await.announce(OsRng, Some(&port_bytes))
            .map_err(|e| format!("announce error: {e:?}"))?;
        self.transport.lock().await.send_packet(announce).await;

        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, self.udp_port);
        let recv_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        recv_sock.set_reuse_address(true)?;
        recv_sock.set_nonblocking(true)?;
        recv_sock.bind(&bind_addr.into())?;
        recv_sock.join_multicast_v4(&MCAST_GROUP, &Ipv4Addr::UNSPECIFIED)?;
        let udp_rx = Arc::new(UdpSocket::from_std(recv_sock.into())?);

        let send_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        send_sock.set_nonblocking(true)?;
        send_sock.set_multicast_loop_v4(false)?;
        send_sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0).into())?;
        let udp_tx = Arc::new(UdpSocket::from_std(send_sock.into())?);

        let multicast_target: SocketAddr =
            SocketAddrV4::new(MCAST_GROUP, self.udp_port).into();

        // Set of peer destination hashes we have opened outgoing links to.
        let peers: Arc<Mutex<HashSet<AddressHash>>> = Arc::new(Mutex::new(HashSet::new()));

        log::info!(
            "atak-bridge: ready — multicast {}:{}, dest {}",
            MCAST_GROUP, self.udp_port, dest_hash
        );

        // Auto-link: watch for announces with matching app_data (same port) and open links.
        let auto_link = {
            let transport = self.transport.clone();
            let peers = peers.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                let mut announce_rx = transport.lock().await.recv_announces().await;
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(event) = announce_rx.recv() => {
                            if event.app_data.as_slice() != port_bytes {
                                continue; // different port or unrelated announce
                            }
                            let peer_desc = event.destination.lock().await.desc.clone();
                            let peer_hash = peer_desc.address_hash;

                            if peer_hash == dest_hash {
                                continue; // our own announce
                            }

                            if peers.lock().await.contains(&peer_hash) {
                                continue; // already linked
                            }

                            log::info!("atak-bridge: auto-linking to peer {peer_hash}");
                            transport.lock().await.link(peer_desc).await;
                            peers.lock().await.insert(peer_hash);
                        }
                    }
                }
            })
        };

        // Reticulum → UDP
        let rns_to_udp = {
            let transport = self.transport.clone();
            let cancel = cancel.clone();
            let metrics = self.metrics.clone();

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
                                    data.len(), hex_preview(data),
                                );
                                if let Err(err) = udp_tx.send_to(data, multicast_target).await {
                                    log::warn!("atak-bridge: udp send error: {err}");
                                } else {
                                    metrics.tx_packets.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }
            })
        };

        // UDP → Reticulum (in-links for connected clients + out-links for auto-linked peers)
        let udp_to_rns = {
            let transport = self.transport.clone();
            let cancel = cancel.clone();
            let metrics = self.metrics.clone();

            tokio::spawn(async move {
                let mut buf = vec![0u8; 65535];
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok((len, src)) = udp_rx.recv_from(&mut buf) => {
                            let data = &buf[..len];
                            log::debug!(
                                "atak-bridge: udp→rns {} bytes from {src} | {}",
                                len, hex_preview(data),
                            );
                            metrics.rx_packets.fetch_add(1, Ordering::Relaxed);

                            let mut t = transport.lock().await;
                            // Forward to any ATAK clients that connected to us.
                            t.send_to_in_links(&dest_hash, data).await;
                            // Forward to auto-linked peers.
                            for &peer_hash in peers.lock().await.iter() {
                                t.send_to_out_links(&peer_hash, data).await;
                            }
                        }
                    }
                }
            })
        };

        // Periodic re-announce (include port in app_data every time)
        let reannounce = {
            let transport = self.transport.clone();
            let cancel = cancel.clone();

            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            match destination.lock().await.announce(OsRng, Some(&port_bytes)) {
                                Ok(announce) => {
                                    transport.lock().await.send_packet(announce).await;
                                }
                                Err(err) => {
                                    log::warn!("atak-bridge: re-announce failed: {err:?}");
                                }
                            }
                        }
                    }
                }
            })
        };

        let _ = tokio::join!(auto_link, rns_to_udp, udp_to_rns, reannounce);
        log::info!("atak-bridge: stopped");
        Ok(())
    }
}

/// Format up to the first 16 bytes of `data` as a hex string for log previews.
fn hex_preview(data: &[u8]) -> String {
    let n = data.len().min(16);
    let hex: String = data[..n].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
    if data.len() > 16 {
        format!("{hex} …")
    } else {
        hex
    }
}
