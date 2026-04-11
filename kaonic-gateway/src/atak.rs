use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use rand::rngs::OsRng;
use reticulum::destination::link::LinkEvent;
use reticulum::destination::DestinationName;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::Transport;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub const ATAK_PORTS: &[(u16, Ipv4Addr)] = &[
    (6969, Ipv4Addr::new(239, 2, 3, 1)),
    (17012, Ipv4Addr::new(224, 10, 10, 1)),
];

/// Multicast group for each well-known ATAK port.
pub fn mcast_group_for_port(port: u16) -> Ipv4Addr {
    ATAK_PORTS
        .iter()
        .find(|(p, _)| *p == port)
        .map(|(_, g)| *g)
        .unwrap_or(Ipv4Addr::new(239, 2, 3, 1))
}

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

pub struct AtakBridge {
    pub transport: Arc<Mutex<Transport>>,
    pub identity: PrivateIdentity,
    pub port: u16,
    pub metrics: Arc<BridgeMetrics>,
}

impl AtakBridge {
    pub fn new(
        transport: Arc<Mutex<Transport>>,
        identity: PrivateIdentity,
        port: u16,
        metrics: Arc<BridgeMetrics>,
    ) -> Self {
        Self {
            transport,
            identity,
            port,
            metrics,
        }
    }

    pub async fn run(
        self,
        cancel: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let port = self.port;
        let port_tag = port.to_be_bytes();
        let dest_name = format!("atak.{port}");

        // Register Reticulum destination
        let destination = self
            .transport
            .lock()
            .await
            .add_destination(self.identity, DestinationName::new("kaonic", &dest_name))
            .await;

        let dest_hash = destination.lock().await.desc.address_hash;
        let _ = self.metrics.dest_hash.set(dest_hash.to_hex_string());

        // Announce so peers can discover us
        if let Ok(pkt) = destination.lock().await.announce(OsRng, Some(&port_tag)) {
            self.transport.lock().await.send_packet(pkt).await;
        }

        log::info!("atak-bridge:{port}: starting, dest={dest_hash}");

        // ── UDP receive socket ────────────────────────────────────────────
        // Join multicast on the first interface with a 192.168.10.x address.
        // INADDR_ANY resolves to the TUN device on this system (ENODEV).
        let rx_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        rx_sock.set_reuse_address(true)?;
        rx_sock.set_nonblocking(true)?;
        rx_sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;

        let local_addr = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .find_map(|i| match i.addr.ip() {
                IpAddr::V4(a)
                    if a.octets()[0] == 192 && a.octets()[1] == 168 && a.octets()[2] == 10 =>
                {
                    Some(a)
                }
                _ => None,
            })
            .unwrap_or(Ipv4Addr::UNSPECIFIED); // fallback — may fail but won't abort

        match rx_sock.join_multicast_v4(&mcast_group_for_port(port), &local_addr) {
            Ok(_) => log::info!(
                "atak-bridge:{port}: joined multicast {} via {local_addr}",
                mcast_group_for_port(port)
            ),
            Err(e) => log::warn!("atak-bridge:{port}: multicast join on {local_addr} failed: {e}"),
        }

        let udp_rx = Arc::new(UdpSocket::from_std(rx_sock.into())?);

        // ── UDP send sockets — one per non-loopback IPv4 interface ──────
        // Setting IP_MULTICAST_IF tells the OS which interface to egress on.
        // We create one socket per interface so the packet goes out on all of them.
        let mcast_target: std::net::SocketAddr =
            SocketAddrV4::new(mcast_group_for_port(port), port).into();
        let udp_tx_sockets: Arc<Vec<UdpSocket>> = Arc::new(
            if_addrs::get_if_addrs()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|i| match i.addr.ip() {
                    IpAddr::V4(a) if !a.is_loopback() => Some(a),
                    _ => None,
                })
                .filter_map(|local_ip| {
                    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).ok()?;
                    s.set_nonblocking(true).ok()?;
                    s.set_multicast_loop_v4(false).ok()?;
                    s.set_multicast_if_v4(&local_ip).ok()?;
                    s.bind(&SocketAddrV4::new(local_ip, 0).into()).ok()?;
                    let sock = UdpSocket::from_std(s.into()).ok()?;
                    log::info!("atak-bridge:{port}: tx socket on {local_ip}");
                    Some(sock)
                })
                .collect(),
        );

        log::info!("atak-bridge:{port}: ready, dest={dest_hash}");

        // ── Task 1: UDP → Reticulum ───────────────────────────────────────
        let udp_to_rns = {
            let transport = self.transport.clone();
            let metrics = self.metrics.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65535];
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok((len, src)) = udp_rx.recv_from(&mut buf) => {
                            let data = &buf[..len];
                            log::info!("atak-bridge:{port}: udp -> rns {len}B from {src}");
                            metrics.rx_packets.fetch_add(1, Ordering::Relaxed);
                            let _ = transport.lock().await.send_to_in_links(&dest_hash, data).await;
                        }
                    }
                }
            })
        };

        // ── Task 2: Reticulum → UDP ───────────────────────────────────────
        let rns_to_udp = {
            let transport = self.transport.clone();
            let metrics = self.metrics.clone();
            let cancel = cancel.clone();
            let sockets = udp_tx_sockets.clone();
            tokio::spawn(async move {
                let mut in_rx = transport.lock().await.out_link_events();
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(ev) = in_rx.recv() => {
                            if let LinkEvent::Data(payload) = ev.event {
                                let data = payload.as_slice();
                                log::info!("atak-bridge:{port}: rns -> udp {}B (in-link={})", data.len(), ev.id);
                                for sock in sockets.iter() {
                                    if let Err(e) = sock.send_to(data, mcast_target).await {
                                        log::warn!("atak-bridge:{port}: udp send error: {e}");
                                    }
                                }
                                metrics.tx_packets.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            })
        };

        // ── Task 3: auto-link peers announcing same port ──────────────────
        let auto_link = {
            let transport = self.transport.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut ann_rx = transport.lock().await.recv_announces().await;
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        Ok(ev) = ann_rx.recv() => {
                            if ev.app_data.as_slice() != port_tag { continue; }
                            let peer = ev.destination.lock().await.desc.clone();
                            if peer.address_hash == dest_hash { continue; }
                            // Skip if an outgoing link to this peer already exists.
                            let t = transport.lock().await;
                            if t.find_out_link(&peer.address_hash).await.is_some() {
                                continue;
                            }
                            log::info!("atak-bridge:{port}: auto-link -> {}", peer.address_hash);
                            t.link(peer).await;
                        }
                    }
                }
            })
        };

        // ── Task 4: periodic re-announce ─────────────────────────────────
        let reannounce = {
            let transport = self.transport.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(10));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tick.tick() => {
                            if let Ok(pkt) = destination.lock().await.announce(OsRng, Some(&port_tag)) {
                                transport.lock().await.send_packet(pkt).await;
                            }
                        }
                    }
                }
            })
        };

        let _ = tokio::join!(udp_to_rns, rns_to_udp, auto_link, reannounce);
        log::info!("atak-bridge:{port}: stopped");
        Ok(())
    }
}
