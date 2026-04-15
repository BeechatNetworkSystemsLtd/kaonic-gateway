use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cidr::Ipv4Cidr;
#[cfg(target_os = "linux")]
use etherparse::IpSlice;
use if_addrs::{get_if_addrs, IfAddr};
use reticulum::destination::link::LinkEvent;
use reticulum::destination::DestinationDesc;
use reticulum::destination::DestinationName;
use reticulum::hash::AddressHash;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::Transport;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::VpnConfig;

const VPN_ANNOUNCE_PREFIX: &[u8] = b"kvpn1:";
const VPN_CONTROL_PREFIX: &[u8] = b"kvpc1:";
const PEER_ROUTE_CACHE_GRACE_SECS: u64 = 45;
#[cfg(target_os = "linux")]
const VPN_PACKET_PACING_MS: u64 = 5;
#[cfg(target_os = "linux")]
const DEFAULT_TUN_NAME: &str = "kaonic-vpn%d";
#[cfg(target_os = "linux")]
const DEFAULT_TUN_MTU: usize = 1500;

#[derive(Debug)]
pub enum VpnRuntimeError {
    Config(String),
    Io(std::io::Error),
    Serialization(serde_json::Error),
    Tun(String),
}

impl fmt::Display for VpnRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(message) => write!(f, "{message}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Serialization(err) => write!(f, "{err}"),
            Self::Tun(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for VpnRuntimeError {}

impl From<std::io::Error> for VpnRuntimeError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for VpnRuntimeError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialization(err)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnPeerSnapshot {
    pub destination: String,
    pub tunnel_ip: Option<String>,
    pub link_state: String,
    pub announced_routes: Vec<String>,
    pub last_seen_ts: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnRouteSnapshot {
    pub network: String,
    pub owner: String,
    pub status: String,
    pub last_seen_ts: u64,
    pub installed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnRouteMappingSnapshot {
    pub subnet: String,
    pub tunnel: String,
    pub mapped_subnet: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnSnapshot {
    pub destination_hash: String,
    pub network: String,
    pub local_tunnel_ip: Option<String>,
    pub backend: String,
    pub interface_name: Option<String>,
    pub status: String,
    pub advertised_routes: Vec<String>,
    pub local_routes: Vec<String>,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub drop_packets: u64,
    pub last_tx_ts: u64,
    pub last_rx_ts: u64,
    pub peers: Vec<VpnPeerSnapshot>,
    pub remote_routes: Vec<VpnRouteSnapshot>,
    pub route_mappings: Vec<VpnRouteMappingSnapshot>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
struct PeerState {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    destination: AddressHash,
    destination_desc: Option<DestinationDesc>,
    link_state: String,
    announced_routes: Vec<Ipv4Cidr>,
    last_seen_ts: u64,
    last_error: Option<String>,
    /// Number of consecutive close events — used to compute exponential backoff.
    reconnect_attempts: u32,
    route_expires_ts: u64,
}

impl PeerState {
    fn new(destination: AddressHash, link_state: &str) -> Self {
        Self {
            destination,
            destination_desc: None,
            link_state: link_state.into(),
            announced_routes: Vec::new(),
            last_seen_ts: 0,
            last_error: None,
            reconnect_attempts: 0,
            route_expires_ts: 0,
        }
    }
}

struct VpnRuntimeState {
    destination: AddressHash,
    destination_hash: String,
    network: Ipv4Cidr,
    local_tunnel_ip: Ipv4Addr,
    backend: String,
    interface_name: Option<String>,
    route_aliasing_enabled: bool,
    status: String,
    advertised_routes: Vec<Ipv4Cidr>,
    local_routes: Vec<Ipv4Cidr>,
    tx_packets: u64,
    tx_bytes: u64,
    rx_packets: u64,
    rx_bytes: u64,
    drop_packets: u64,
    last_tx_ts: u64,
    last_rx_ts: u64,
    peers: BTreeMap<String, PeerState>,
    installed_routes: BTreeSet<String>,
    conflicted_routes: BTreeSet<String>,
    last_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct VpnAnnounce {
    version: u8,
}

#[derive(Serialize, Deserialize)]
struct VpnRouteSync {
    version: u8,
    destination: String,
    routes: Vec<String>,
}

pub struct VpnRuntime {
    state: Mutex<VpnRuntimeState>,
}

impl VpnRuntime {
    pub async fn start(
        config: VpnConfig,
        transport: Arc<Mutex<Transport>>,
        id: PrivateIdentity,
        cancel: CancellationToken,
    ) -> Result<Arc<Self>, VpnRuntimeError> {
        let mut peers = parse_configured_peers(&config.peers)?;
        validate_peer_network(config.network)?;

        let destination = transport
            .lock()
            .await
            .add_destination(id, DestinationName::new("kaonic", "vpn"))
            .await;
        let destination_hash = destination.lock().await.desc.address_hash;
        peers.remove(&destination_hash);
        let local_tunnel_ip = derive_tunnel_ip(config.network, &destination_hash)?;

        let tun = platform_create_tun()?;
        let interface_name = platform_tun_name(tun.as_ref());
        let route_aliasing_enabled = platform_supports_route_aliasing();
        if let Some(interface_name) = interface_name.as_deref() {
            platform_configure_tun_address(
                interface_name,
                local_tunnel_ip,
                config.network.network_length(),
            )?;
        }
        let discovered_routes = discover_local_routes(interface_name.as_deref());
        let local_routes = merge_local_routes(&discovered_routes, &config.advertised_routes);
        if interface_name.is_some() {
            platform_enable_forwarding()?;
        }

        let runtime = Arc::new(Self {
            state: Mutex::new(VpnRuntimeState {
                destination: destination_hash,
                destination_hash: destination_hash.to_hex_string(),
                network: config.network,
                local_tunnel_ip,
                backend: platform_backend_name().into(),
                interface_name: interface_name.clone(),
                route_aliasing_enabled,
                status: if interface_name.is_some() {
                    "running".into()
                } else {
                    "mock".into()
                },
                advertised_routes: config.advertised_routes.clone(),
                local_routes,
                tx_packets: 0,
                tx_bytes: 0,
                rx_packets: 0,
                rx_bytes: 0,
                drop_packets: 0,
                last_tx_ts: 0,
                last_rx_ts: 0,
                peers: peers
                    .iter()
                    .map(|peer| (peer.to_hex_string(), PeerState::new(*peer, "configured")))
                    .collect(),
                installed_routes: BTreeSet::new(),
                conflicted_routes: BTreeSet::new(),
                last_error: None,
            }),
        });

        log::info!(
            "vpn start local_tunnel_ip={} network={} backend={} iface={} route_aliasing={}",
            local_tunnel_ip,
            config.network,
            platform_backend_name(),
            interface_name.as_deref().unwrap_or("mock"),
            if route_aliasing_enabled {
                "enabled"
            } else {
                "disabled"
            }
        );
        if interface_name.is_some() && !route_aliasing_enabled {
            log::warn!("vpn route alias translation unavailable; exporting raw local routes");
        }

        runtime.sync_routes().await?;

        {
            let runtime = runtime.clone();
            let transport = transport.clone();
            let destination = destination.clone();
            let cancel = cancel.clone();
            let announce_freq = config.announce_freq_secs.max(1) as u64;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(announce_freq));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let discovered_routes = discover_local_routes(interface_name.as_deref());
                            let routes = runtime.refresh_local_routes(discovered_routes).await;
                            if let Err(err) = runtime.sync_routes().await {
                                runtime.record_error(err.to_string()).await;
                                continue;
                            }
                            match encode_announce() {
                                Ok(app_data) => {
                                    transport.lock().await.send_announce(&destination, Some(&app_data)).await;
                                    if let Err(err) = runtime.broadcast_route_sync(&transport, &routes).await {
                                        runtime.record_error(err.to_string()).await;
                                    }
                                }
                                Err(err) => runtime.record_error(err.to_string()).await,
                            }
                        }
                    }
                }
            });
        }

        {
            let runtime = runtime.clone();
            let transport = transport.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut announce_rx = transport.lock().await.recv_announces().await;
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        recv = announce_rx.recv() => match recv {
                            Ok(announce) => {
                                let destination = announce.destination.lock().await.desc.clone();
                                if destination.address_hash == destination_hash {
                                    continue;
                                }
                                let Some(parsed) = decode_announce(announce.app_data.as_slice()) else {
                                    continue;
                                };
                                match parsed {
                                    Ok(()) => {
                                        let address_hash = destination.address_hash;
                                        runtime.update_peer_destination(destination, "discovered").await;
                                        runtime.request_peer_outbound_link(&transport, address_hash).await;
                                    }
                                    Err(err) => runtime.record_peer_error(destination.address_hash, err.to_string()).await,
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                }
            });
        }

        {
            let runtime = runtime.clone();
            let transport = transport.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut out_link_events = transport.lock().await.out_link_events();
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        recv = out_link_events.recv() => match recv {
                            Ok(event) => {
                                let state = match event.event {
                                    LinkEvent::Activated => "active",
                                    LinkEvent::Closed => "closed",
                                    LinkEvent::Data(_) | LinkEvent::Proof(_) => "active",
                                };
                                runtime.set_peer_link_state(event.address_hash, state).await;
                                if matches!(event.event, LinkEvent::Activated) {
                                    runtime.reset_peer_backoff(event.address_hash).await;
                                    if let Err(err) = runtime
                                        .send_route_sync_to_peer(&transport, event.address_hash)
                                        .await
                                    {
                                        runtime.record_peer_error(event.address_hash, err.to_string()).await;
                                    }
                                }
                                if matches!(event.event, LinkEvent::Closed) {
                                    let delay = runtime.backoff_peer(event.address_hash).await;
                                    log::warn!(
                                        "vpn peer={} link closed; reconnecting in {}s",
                                        event.address_hash, delay
                                    );
                                    let runtime2 = runtime.clone();
                                    let transport2 = transport.clone();
                                    let cancel2 = cancel.clone();
                                    let hash = event.address_hash;
                                    tokio::spawn(async move {
                                        tokio::select! {
                                            _ = cancel2.cancelled() => {}
                                            _ = tokio::time::sleep(Duration::from_secs(delay)) => {
                                                runtime2.request_peer_outbound_link(&transport2, hash).await;
                                            }
                                        }
                                    });
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
                        }
                    }
                }
            });
        }

        {
            let runtime = runtime.clone();
            let transport = transport.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(10));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let routes_changed = runtime.expire_stale_peer_routes().await;
                            if routes_changed {
                                if let Err(err) = runtime.sync_routes().await {
                                    runtime.record_error(err.to_string()).await;
                                }
                            }
                            for destination in runtime.reconnect_watchdog_candidates().await {
                                if transport.lock().await.find_out_link(&destination).await.is_some() {
                                    continue;
                                }
                                log::info!("vpn peer={} reconnect watchdog requesting outbound link", destination);
                                runtime.request_peer_outbound_link(&transport, destination).await;
                            }
                        }
                    }
                }
            });
        }

        #[cfg(target_os = "linux")]
        if let Some(tun) = tun {
            {
                let runtime = runtime.clone();
                let transport = transport.clone();
                let tun = tun.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            read = tun.read() => match read {
                                Ok(packet) => {
                                    let Some(dst_ip) = packet_destination(&packet) else { continue; };
                                    let endpoints = packet_endpoints(&packet);
                                    if runtime.is_local_tunnel_ip(dst_ip).await {
                                        if let Some((src_ip, _)) = endpoints {
                                            log::debug!(
                                                "vpn tx ignore {}B src={} dst={} local={}",
                                                packet.len(),
                                                src_ip,
                                                dst_ip,
                                                dst_ip
                                            );
                                        } else {
                                            log::debug!("vpn tx ignore {}B dst={} local={}", packet.len(), dst_ip, dst_ip);
                                        }
                                        continue;
                                    }
                                    let Some(peer) = runtime.resolve_peer_for_ip(dst_ip).await else {
                                        runtime.record_drop(packet.len()).await;
                                        if let Some((src_ip, _)) = endpoints {
                                            let local_ip = runtime.local_tunnel_ip().await;
                                            log::warn!(
                                                "vpn tx drop {}B src={} dst={} local={} no peer route",
                                                packet.len(),
                                                src_ip,
                                                dst_ip,
                                                local_ip
                                            );
                                        } else {
                                            let local_ip = runtime.local_tunnel_ip().await;
                                            log::warn!(
                                                "vpn tx drop {}B dst={} local={} no peer route",
                                                packet.len(),
                                                dst_ip,
                                                local_ip
                                            );
                                        }
                                        continue;
                                    };
                                    log::info!("vpn tx {}B dst={} peer={}", packet.len(), dst_ip, peer);
                                    runtime.record_tx(packet.len()).await;
                                    let sent = transport.lock().await.send_to_out_links(&peer, &packet).await;
                                    if sent.is_empty() {
                                        runtime.record_drop(packet.len()).await;
                                        log::warn!("vpn tx {}B dst={} peer={} no active out link; requesting link", packet.len(), dst_ip, peer);
                                        runtime.ensure_peer_outbound_link(&transport, peer).await;
                                    } else {
                                        pace_vpn_packet_send().await;
                                    }
                                }
                                Err(err) => {
                                    runtime.record_error(format!("vpn tun read failed: {err}")).await;
                                    break;
                                }
                            }
                        }
                    }
                });
            }

            {
                let runtime = runtime.clone();
                let transport = transport.clone();
                let tun = tun.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    let mut out_link_events = transport.lock().await.out_link_events();
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            recv = out_link_events.recv() => match recv {
                                Ok(event) => {
                                    let LinkEvent::Data(payload) = event.event else { continue; };
                                    if let Err(err) = runtime
                                        .handle_outbound_link_payload(
                                            &tun,
                                            event.address_hash,
                                            event.id,
                                            payload.as_slice(),
                                        )
                                        .await
                                    {
                                        runtime.record_error(format!("vpn tun write failed: {err}")).await;
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(_) => break,
                            }
                        }
                    }
                });
            }

            {
                let runtime = runtime.clone();
                let transport = transport.clone();
                let tun = tun.clone();
                let cancel = cancel.clone();
                let local_destination = destination_hash;
                tokio::spawn(async move {
                    let mut in_link_events = transport.lock().await.in_link_events();
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                                recv = in_link_events.recv() => match recv {
                                    Ok(event) => match event.event {
                                        LinkEvent::Data(payload) if event.address_hash == local_destination => {
                                            if let Err(err) = runtime
                                                .handle_inbound_link_payload(
                                                    &tun,
                                                    event.id,
                                                    payload.as_slice(),
                                                )
                                                .await
                                            {
                                                runtime.record_error(format!("vpn tun write failed: {err}")).await;
                                                break;
                                            }
                                    }
                                    _ => {}
                                },
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(_) => break,
                            }
                        }
                    }
                });
            }
        }

        Ok(runtime)
    }

    pub async fn snapshot(&self) -> VpnSnapshot {
        let state = self.state.lock().await;
        build_snapshot(&state)
    }

    pub async fn replace_advertised_routes(&self, routes: Vec<Ipv4Cidr>) {
        let mut state = self.state.lock().await;
        state.advertised_routes = routes;
        state.local_routes = merge_local_routes(
            &discover_local_routes(state.interface_name.as_deref()),
            &state.advertised_routes,
        );
        state.last_error = None;
        drop(state);
        let _ = self.sync_routes().await;
    }

    async fn refresh_local_routes(&self, discovered_routes: Vec<Ipv4Cidr>) -> Vec<Ipv4Cidr> {
        let mut state = self.state.lock().await;
        state.local_routes = merge_local_routes(&discovered_routes, &state.advertised_routes);
        state.last_error = None;
        exported_local_routes(&state)
    }

    async fn broadcast_route_sync(
        &self,
        transport: &Arc<Mutex<Transport>>,
        routes: &[Ipv4Cidr],
    ) -> Result<(), VpnRuntimeError> {
        let (destination, peers) = {
            let state = self.state.lock().await;
            (
                state.destination,
                state
                    .peers
                    .values()
                    .map(|peer| peer.destination)
                    .collect::<Vec<_>>(),
            )
        };
        let payload = encode_route_sync(&destination, routes)?;
        let transport = transport.lock().await;
        for peer in peers {
            let _ = transport.send_to_out_links(&peer, &payload).await;
        }
        Ok(())
    }

    async fn send_route_sync_to_peer(
        &self,
        transport: &Arc<Mutex<Transport>>,
        destination: AddressHash,
    ) -> Result<(), VpnRuntimeError> {
        let payload = {
            let state = self.state.lock().await;
            encode_route_sync(&state.destination, &exported_local_routes(&state))?
        };
        let _ = transport
            .lock()
            .await
            .send_to_out_links(&destination, &payload)
            .await;
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn handle_outbound_link_payload(
        &self,
        tun: &PlatformTun,
        destination: AddressHash,
        link_id: AddressHash,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        self.handle_link_payload(Some(destination), tun, link_id, data)
            .await
    }

    #[cfg(target_os = "linux")]
    async fn handle_inbound_link_payload(
        &self,
        tun: &PlatformTun,
        link_id: AddressHash,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        self.handle_link_payload(None, tun, link_id, data).await
    }

    #[cfg(target_os = "linux")]
    async fn handle_link_payload(
        &self,
        destination: Option<AddressHash>,
        tun: &PlatformTun,
        link_id: AddressHash,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        let destination = match destination {
            Some(destination) => Some(destination),
            None => self.resolve_payload_peer(data).await,
        };

        if let Some(destination) = destination {
            self.set_peer_link_state(destination, "active").await;
            if self.apply_route_sync_payload(destination, data).await {
                return Ok(());
            }
        }

        log_vpn_rx(data, link_id);
        self.record_rx(data.len()).await;
        tun.write(data).await.map(|_| ())
    }

    #[cfg(target_os = "linux")]
    async fn apply_route_sync_payload(&self, destination: AddressHash, data: &[u8]) -> bool {
        let Some(parsed) = decode_route_sync(data) else {
            return false;
        };

        match parsed {
            Ok((payload_destination, routes)) => {
                if payload_destination != destination {
                    self.record_peer_error(
                        destination,
                        format!(
                            "route sync destination mismatch: expected {}, got {}",
                            destination, payload_destination
                        ),
                    )
                    .await;
                    return true;
                }
                self.update_peer_routes(destination, routes).await;
                if let Err(err) = self.sync_routes().await {
                    self.record_error(err.to_string()).await;
                }
            }
            Err(err) => {
                self.record_peer_error(destination, err.to_string()).await;
            }
        }
        true
    }

    #[cfg(target_os = "linux")]
    async fn resolve_payload_peer(&self, data: &[u8]) -> Option<AddressHash> {
        if let Some(parsed) = decode_route_sync(data) {
            match parsed {
                Ok((destination, _)) => return Some(destination),
                Err(_) => return None,
            }
        }

        let (src_ip, _) = packet_endpoints(data)?;
        self.resolve_peer_for_ip(src_ip).await
    }

    #[cfg(target_os = "linux")]
    async fn record_tx(&self, bytes: usize) {
        let mut state = self.state.lock().await;
        state.tx_packets += 1;
        state.tx_bytes += bytes as u64;
        state.last_tx_ts = unix_timestamp_secs();
    }

    #[cfg(target_os = "linux")]
    async fn record_rx(&self, bytes: usize) {
        let mut state = self.state.lock().await;
        state.rx_packets += 1;
        state.rx_bytes += bytes as u64;
        state.last_rx_ts = unix_timestamp_secs();
    }

    #[cfg(target_os = "linux")]
    async fn record_drop(&self, _bytes: usize) {
        let mut state = self.state.lock().await;
        state.drop_packets += 1;
    }

    #[cfg(target_os = "linux")]
    async fn is_local_tunnel_ip(&self, address: Ipv4Addr) -> bool {
        let state = self.state.lock().await;
        is_local_tunnel_ip(&state, address)
    }

    #[cfg(target_os = "linux")]
    async fn local_tunnel_ip(&self) -> Ipv4Addr {
        let state = self.state.lock().await;
        state.local_tunnel_ip
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    async fn update_peer_routes(&self, destination: AddressHash, routes: Vec<Ipv4Cidr>) {
        let mut state = self.state.lock().await;
        if destination == state.destination {
            return;
        }
        let peer = state
            .peers
            .entry(destination.to_hex_string())
            .or_insert_with(|| PeerState::new(destination, "discovered"));
        peer.announced_routes = routes;
        peer.last_seen_ts = unix_timestamp_secs();
        peer.last_error = None;
        peer.route_expires_ts = 0;
        state.last_error = None;
    }

    async fn record_peer_error(&self, destination: AddressHash, message: String) {
        let mut state = self.state.lock().await;
        if destination == state.destination {
            return;
        }
        let peer = state
            .peers
            .entry(destination.to_hex_string())
            .or_insert_with(|| PeerState::new(destination, "discovered"));
        peer.last_error = Some(message.clone());
        state.last_error = Some(message);
        state.status = "error".into();
    }

    async fn set_peer_link_state(&self, destination: AddressHash, link_state: &str) {
        let mut state = self.state.lock().await;
        if destination == state.destination {
            return;
        }
        let peer = state
            .peers
            .entry(destination.to_hex_string())
            .or_insert_with(|| PeerState::new(destination, "discovered"));
        peer.link_state = link_state.into();
        let now = unix_timestamp_secs();
        if link_state == "active" {
            peer.last_seen_ts = now;
            peer.route_expires_ts = 0;
        } else if link_state == "closed" {
            peer.route_expires_ts = if peer.announced_routes.is_empty() {
                0
            } else {
                now.saturating_add(PEER_ROUTE_CACHE_GRACE_SECS)
            };
        }
        if state.status != "mock" {
            state.status = "running".into();
        }
    }

    async fn request_peer_outbound_link(
        &self,
        transport: &Arc<Mutex<Transport>>,
        destination: AddressHash,
    ) {
        let state = self.state.lock().await;
        if destination == state.destination {
            return;
        }
        let Some(peer) = state.peers.get(&destination.to_hex_string()) else {
            return;
        };
        let Some(destination_desc) = peer.destination_desc else {
            return;
        };
        drop(state);

        if transport
            .lock()
            .await
            .find_out_link(&destination)
            .await
            .is_some()
        {
            return;
        }

        transport.lock().await.link(destination_desc).await;
    }

    async fn reconnect_watchdog_candidates(&self) -> Vec<AddressHash> {
        let state = self.state.lock().await;
        state
            .peers
            .values()
            .filter(|peer| {
                peer.destination_desc.is_some()
                    && should_watchdog_reconnect(peer.link_state.as_str())
                    && (!peer.announced_routes.is_empty() || peer.last_seen_ts > 0)
            })
            .map(|peer| peer.destination)
            .collect()
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    async fn ensure_peer_outbound_link(
        &self,
        transport: &Arc<Mutex<Transport>>,
        destination: AddressHash,
    ) {
        self.request_peer_outbound_link(transport, destination)
            .await;
    }

    /// Increment reconnect_attempts and return the backoff delay in seconds.
    /// Delay is capped at 30 s: 1 → 2 → 4 → 8 → 16 → 30.
    /// If the peer was active in the last few seconds, reconnect immediately.
    async fn backoff_peer(&self, destination: AddressHash) -> u64 {
        let mut state = self.state.lock().await;
        if destination == state.destination {
            return 30;
        }
        let peer = state
            .peers
            .entry(destination.to_hex_string())
            .or_insert_with(|| PeerState::new(destination, "closed"));
        let recently_active =
            peer.last_seen_ts > 0 && unix_timestamp_secs().saturating_sub(peer.last_seen_ts) <= 3;
        let delay = if recently_active {
            0
        } else {
            (1u64 << peer.reconnect_attempts.min(4)).min(30)
        };
        peer.reconnect_attempts = peer.reconnect_attempts.saturating_add(1);
        delay
    }

    /// Reset reconnect backoff counter after a successful link activation.
    async fn reset_peer_backoff(&self, destination: AddressHash) {
        let mut state = self.state.lock().await;
        if destination == state.destination {
            return;
        }
        if let Some(peer) = state.peers.get_mut(&destination.to_hex_string()) {
            peer.reconnect_attempts = 0;
        }
    }

    async fn update_peer_destination(&self, destination: DestinationDesc, link_state: &str) {
        let mut state = self.state.lock().await;
        if destination.address_hash == state.destination {
            return;
        }
        let peer = state
            .peers
            .entry(destination.address_hash.to_hex_string())
            .or_insert_with(|| PeerState::new(destination.address_hash, link_state));
        peer.destination_desc = Some(destination);
        if peer.link_state == "configured" || peer.link_state.is_empty() {
            peer.link_state = link_state.into();
        }
    }

    async fn expire_stale_peer_routes(&self) -> bool {
        let now = unix_timestamp_secs();
        let mut state = self.state.lock().await;
        let mut changed = false;
        for peer in state.peers.values_mut() {
            if !peer.announced_routes.is_empty()
                && peer.route_expires_ts > 0
                && peer.route_expires_ts <= now
            {
                peer.announced_routes.clear();
                peer.route_expires_ts = 0;
                changed = true;
            }
        }
        changed
    }

    #[cfg(target_os = "linux")]
    async fn resolve_peer_for_ip(&self, address: Ipv4Addr) -> Option<AddressHash> {
        let state = self.state.lock().await;
        if let Some(destination) = resolve_peer_tunnel_ip(&state, address) {
            return Some(destination);
        }
        resolve_peer_route(&state, address)
    }

    async fn sync_routes(&self) -> Result<(), VpnRuntimeError> {
        let (interface_name, desired_routes, peer_conflicts, installed_routes, local_translations) = {
            let state = self.state.lock().await;
            let (desired_routes, peer_conflicts) = desired_route_owners(&state);
            (
                state.interface_name.clone(),
                desired_routes,
                peer_conflicts,
                state.installed_routes.clone(),
                local_route_translations(&state),
            )
        };

        let local_conflicts =
            conflicting_local_routes(&local_translations, desired_routes.keys().copied());
        let desired_strings = desired_routes
            .keys()
            .filter(|route| !local_conflicts.contains(&route.to_string()))
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();

        if let Some(interface_name) = interface_name {
            for route in installed_routes.difference(&desired_strings) {
                platform_delete_route(&interface_name, route);
            }
            for route in &desired_strings {
                platform_replace_route(&interface_name, route)?;
            }
            platform_sync_local_translation(&interface_name, &local_translations)?;
        }

        let mut state = self.state.lock().await;
        state.installed_routes = desired_strings;
        state.conflicted_routes = peer_conflicts.union(&local_conflicts).cloned().collect();
        if state.status != "mock" {
            state.status = "running".into();
        }
        state.last_error = None;
        Ok(())
    }

    async fn record_error(&self, message: String) {
        let mut state = self.state.lock().await;
        state.last_error = Some(message);
        state.status = "error".into();
    }
}

#[cfg(target_os = "linux")]
fn resolve_peer_tunnel_ip(state: &VpnRuntimeState, address: Ipv4Addr) -> Option<AddressHash> {
    state.peers.values().find_map(|peer| {
        (assign_tunnel_ip_for_peer(state.network, peer) == Some(address))
            .then_some(peer.destination)
    })
}

#[cfg(target_os = "linux")]
fn resolve_peer_route(state: &VpnRuntimeState, address: Ipv4Addr) -> Option<AddressHash> {
    let mut best: Option<(u8, AddressHash)> = None;
    for (route, owner) in desired_route_owners(state).0 {
        if route.contains(&address) {
            let prefix = route.network_length();
            match &best {
                Some((best_prefix, _)) if *best_prefix >= prefix => {}
                _ => {
                    if let Some(peer) = state.peers.get(&owner) {
                        best = Some((prefix, peer.destination));
                    }
                }
            }
        }
    }
    best.map(|(_, destination)| destination)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_local_tunnel_ip(state: &VpnRuntimeState, address: Ipv4Addr) -> bool {
    state.local_tunnel_ip == address
}

fn build_snapshot(state: &VpnRuntimeState) -> VpnSnapshot {
    let now = unix_timestamp_secs();
    let mut peers = state
        .peers
        .iter()
        .map(|(destination, peer)| VpnPeerSnapshot {
            destination: destination.clone(),
            tunnel_ip: assign_tunnel_ip_for_peer(state.network, peer).map(|ip| ip.to_string()),
            link_state: peer.link_state.clone(),
            announced_routes: if peer_routes_available(peer, now) {
                peer.announced_routes
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            } else {
                Vec::new()
            },
            last_seen_ts: peer.last_seen_ts,
            last_error: peer.last_error.clone(),
        })
        .collect::<Vec<_>>();
    peers.sort_by(|a, b| a.destination.cmp(&b.destination));

    let (desired_routes, peer_conflicts) = desired_route_owners(state);
    let local_conflicts = conflicting_local_routes(
        &local_route_translations(state),
        desired_routes.keys().copied(),
    );
    let mut remote_routes = desired_routes
        .iter()
        .map(|(route, owner)| {
            let peer = state.peers.get(owner);
            VpnRouteSnapshot {
                network: route.to_string(),
                owner: owner.clone(),
                status: if peer_conflicts.contains(&route.to_string())
                    || local_conflicts.contains(&route.to_string())
                {
                    "conflict".into()
                } else {
                    "active".into()
                },
                last_seen_ts: peer.map(|peer| peer.last_seen_ts).unwrap_or_default(),
                installed: state.installed_routes.contains(&route.to_string()),
            }
        })
        .collect::<Vec<_>>();

    for route in peer_conflicts {
        if remote_routes.iter().any(|entry| entry.network == route) {
            continue;
        }
        remote_routes.push(VpnRouteSnapshot {
            network: route,
            owner: "multiple peers".into(),
            status: "conflict".into(),
            last_seen_ts: 0,
            installed: false,
        });
    }
    remote_routes.sort_by(|a, b| a.network.cmp(&b.network));

    let mut local_routes = local_route_translations(state)
        .into_iter()
        .map(|translation| {
            if translation.exported == translation.local {
                translation.local.to_string()
            } else {
                format!("{} -> {}", translation.exported, translation.local)
            }
        })
        .collect::<Vec<_>>();
    local_routes.sort();
    let mut route_mappings = local_route_translations(state)
        .into_iter()
        .map(|translation| VpnRouteMappingSnapshot {
            subnet: translation.local.to_string(),
            tunnel: state.local_tunnel_ip.to_string(),
            mapped_subnet: translation.exported.to_string(),
        })
        .collect::<Vec<_>>();
    route_mappings.sort_by(|a, b| {
        a.subnet
            .cmp(&b.subnet)
            .then(a.mapped_subnet.cmp(&b.mapped_subnet))
    });
    let mut advertised_routes = state
        .advertised_routes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    advertised_routes.sort();

    VpnSnapshot {
        destination_hash: state.destination_hash.clone(),
        network: state.network.to_string(),
        local_tunnel_ip: Some(state.local_tunnel_ip.to_string()),
        backend: state.backend.clone(),
        interface_name: state.interface_name.clone(),
        status: state.status.clone(),
        advertised_routes,
        local_routes,
        tx_packets: state.tx_packets,
        tx_bytes: state.tx_bytes,
        rx_packets: state.rx_packets,
        rx_bytes: state.rx_bytes,
        drop_packets: state.drop_packets,
        last_tx_ts: state.last_tx_ts,
        last_rx_ts: state.last_rx_ts,
        peers,
        remote_routes,
        route_mappings,
        last_error: state.last_error.clone(),
    }
}

fn validate_peer_network(network: Ipv4Cidr) -> Result<(), VpnRuntimeError> {
    if network.network_length() > 30 {
        return Err(VpnRuntimeError::Config(format!(
            "vpn network {network} must have at least 2 host bits for automatic peer IP assignment"
        )));
    }
    Ok(())
}

fn assign_tunnel_ip_for_peer(network: Ipv4Cidr, peer: &PeerState) -> Option<Ipv4Addr> {
    derive_tunnel_ip(network, &peer.destination).ok()
}

fn derive_tunnel_ip(
    network: Ipv4Cidr,
    destination: &AddressHash,
) -> Result<Ipv4Addr, VpnRuntimeError> {
    validate_peer_network(network)?;

    let host_bits = 32 - network.network_length() as u32;
    let usable_hosts = (1u64 << host_bits) - 2;
    let mut seed = 0u64;
    for byte in destination.as_slice() {
        seed = seed.wrapping_mul(131).wrapping_add(u64::from(*byte));
    }
    let host_offset = (seed % usable_hosts) + 1;
    let network_base = u32::from(network.first_address());
    Ok(Ipv4Addr::from(
        network_base.wrapping_add(host_offset as u32),
    ))
}

fn desired_route_owners(state: &VpnRuntimeState) -> (BTreeMap<Ipv4Cidr, String>, BTreeSet<String>) {
    desired_route_owners_at(state, unix_timestamp_secs())
}

fn desired_route_owners_at(
    state: &VpnRuntimeState,
    now: u64,
) -> (BTreeMap<Ipv4Cidr, String>, BTreeSet<String>) {
    let mut owners = BTreeMap::<Ipv4Cidr, String>::new();
    let mut conflicts = BTreeSet::<String>::new();

    for (destination, peer) in &state.peers {
        if !peer_routes_available(peer, now) {
            continue;
        }
        for route in &peer.announced_routes {
            match owners.get(route) {
                Some(existing) if existing != destination => {
                    conflicts.insert(route.to_string());
                }
                Some(_) => {}
                None => {
                    owners.insert(*route, destination.clone());
                }
            }
        }
    }

    owners.retain(|route, _| !conflicts.contains(&route.to_string()));
    (owners, conflicts)
}

fn peer_routes_available(peer: &PeerState, now: u64) -> bool {
    !peer.announced_routes.is_empty() && (peer.route_expires_ts == 0 || peer.route_expires_ts > now)
}

#[derive(Clone, Copy)]
struct LocalRouteTranslation {
    local: Ipv4Cidr,
    exported: Ipv4Cidr,
}

fn local_route_translations(state: &VpnRuntimeState) -> Vec<LocalRouteTranslation> {
    state
        .local_routes
        .iter()
        .map(|route| LocalRouteTranslation {
            local: *route,
            exported: if state.route_aliasing_enabled {
                export_local_route(&state.destination, *route)
            } else {
                *route
            },
        })
        .collect()
}

fn exported_local_routes(state: &VpnRuntimeState) -> Vec<Ipv4Cidr> {
    local_route_translations(state)
        .into_iter()
        .map(|translation| translation.exported)
        .collect()
}

fn export_local_route(destination: &AddressHash, route: Ipv4Cidr) -> Ipv4Cidr {
    if route.network_length() != 24 {
        return route;
    }

    let mut seed = 0u32;
    for byte in destination.as_slice() {
        seed = seed.wrapping_mul(167).wrapping_add(u32::from(*byte));
    }
    for byte in route.first_address().octets() {
        seed = seed.wrapping_mul(131).wrapping_add(u32::from(byte));
    }
    seed = seed
        .wrapping_mul(31)
        .wrapping_add(u32::from(route.network_length()));

    // Export remote LANs as 192.168.100.0/24 ... 192.168.254.0/24 so
    // directly-connected Kaonic AP/USB clients can reach them via their
    // existing default gateway without extra client-side route setup.
    let mut third_octet = 100 + (seed % 155) as u8;
    let local_octets = route.first_address().octets();
    if local_octets[0] == 192 && local_octets[1] == 168 && local_octets[2] == third_octet {
        third_octet = if third_octet == 254 {
            100
        } else {
            third_octet + 1
        };
    }

    Ipv4Cidr::new(Ipv4Addr::new(192, 168, third_octet, 0), 24).unwrap_or(route)
}

fn merge_local_routes(
    discovered_routes: &[Ipv4Cidr],
    advertised_routes: &[Ipv4Cidr],
) -> Vec<Ipv4Cidr> {
    let mut routes = BTreeSet::new();
    routes.extend(discovered_routes.iter().copied());
    routes.extend(advertised_routes.iter().copied());
    routes.into_iter().collect()
}

fn conflicting_local_routes(
    local_translations: &[LocalRouteTranslation],
    routes: impl IntoIterator<Item = Ipv4Cidr>,
) -> BTreeSet<String> {
    routes
        .into_iter()
        .filter(|route| {
            local_translations.iter().any(|local| {
                routes_overlap(*route, local.local) || routes_overlap(*route, local.exported)
            })
        })
        .map(|route| route.to_string())
        .collect()
}

fn routes_overlap(a: Ipv4Cidr, b: Ipv4Cidr) -> bool {
    let (a_start, a_end) = cidr_bounds(a);
    let (b_start, b_end) = cidr_bounds(b);
    a_start <= b_end && b_start <= a_end
}

fn cidr_bounds(route: Ipv4Cidr) -> (u64, u64) {
    let start = u64::from(u32::from(route.first_address()));
    let host_bits = 32u32.saturating_sub(route.network_length() as u32);
    let size = match host_bits {
        0 => 1,
        32 => u64::from(u32::MAX) + 1,
        bits => 1u64 << bits,
    };
    (start, start + size - 1)
}

fn should_watchdog_reconnect(link_state: &str) -> bool {
    !link_state.trim().eq_ignore_ascii_case("active")
}

#[cfg(target_os = "linux")]
async fn pace_vpn_packet_send() {
    tokio::time::sleep(Duration::from_millis(VPN_PACKET_PACING_MS)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hash(hex: &str) -> AddressHash {
        AddressHash::new_from_hex_string(hex).unwrap()
    }

    fn test_state(network: Ipv4Cidr, peer: AddressHash) -> VpnRuntimeState {
        let mut peers = BTreeMap::new();
        peers.insert(peer.to_hex_string(), PeerState::new(peer, "active"));
        VpnRuntimeState {
            destination: test_hash("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            destination_hash: "local".into(),
            network,
            local_tunnel_ip: Ipv4Addr::new(10, 20, 0, 1),
            backend: "mock".into(),
            interface_name: None,
            route_aliasing_enabled: true,
            status: "running".into(),
            advertised_routes: Vec::new(),
            local_routes: Vec::new(),
            tx_packets: 0,
            tx_bytes: 0,
            rx_packets: 0,
            rx_bytes: 0,
            drop_packets: 0,
            last_tx_ts: 0,
            last_rx_ts: 0,
            peers,
            installed_routes: BTreeSet::new(),
            conflicted_routes: BTreeSet::new(),
            last_error: None,
        }
    }

    #[test]
    fn derive_tunnel_ip_is_stable_and_in_network() {
        let network: Ipv4Cidr = "10.20.0.0/16".parse().unwrap();
        let peer = test_hash("fb08aff16ec6f5ccf0d3eb179028e9c3");
        let ip1 = derive_tunnel_ip(network, &peer).unwrap();
        let ip2 = derive_tunnel_ip(network, &peer).unwrap();
        assert_eq!(ip1, ip2);
        assert!(network.contains(&ip1));
        assert_ne!(ip1, network.first_address());
        assert_ne!(ip1, network.last_address());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_peer_prefers_tunnel_ip_mapping() {
        let network: Ipv4Cidr = "10.20.0.0/16".parse().unwrap();
        let peer = test_hash("fb08aff16ec6f5ccf0d3eb179028e9c3");
        let state = test_state(network, peer);
        let tunnel_ip = derive_tunnel_ip(network, &peer).unwrap();
        assert_eq!(resolve_peer_tunnel_ip(&state, tunnel_ip), Some(peer));
        assert_eq!(resolve_peer_route(&state, tunnel_ip), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_and_peer_views_use_same_destination_hash_for_tunnel_ip() {
        let network: Ipv4Cidr = "10.20.0.0/16".parse().unwrap();
        let destination = test_hash("971a7ac9b42ce6e0faa131bb3c2e7852");
        let mut state = test_state(network, destination);
        state.local_tunnel_ip = derive_tunnel_ip(network, &destination).unwrap();
        let peer = state
            .peers
            .get(&destination.to_hex_string())
            .expect("peer state");
        assert_eq!(
            state.local_tunnel_ip,
            peer_tunnel_ip(network, peer).unwrap()
        );
    }

    #[test]
    fn merge_local_routes_unions_discovered_and_configured() {
        let discovered = vec![
            "192.168.10.0/24".parse().unwrap(),
            "10.50.0.0/24".parse().unwrap(),
        ];
        let advertised = vec![
            "10.50.0.0/24".parse().unwrap(),
            "172.16.1.0/24".parse().unwrap(),
        ];
        let merged = merge_local_routes(&discovered, &advertised);
        assert_eq!(
            merged,
            vec![
                "10.50.0.0/24".parse().unwrap(),
                "172.16.1.0/24".parse().unwrap(),
                "192.168.10.0/24".parse().unwrap(),
            ]
        );
    }

    #[test]
    fn export_local_route_is_stable_and_uses_alias_pool() {
        let destination = test_hash("971a7ac9b42ce6e0faa131bb3c2e7852");
        let route: Ipv4Cidr = "192.168.10.0/24".parse().unwrap();
        let exported_a = export_local_route(&destination, route);
        let exported_b = export_local_route(&destination, route);
        assert_eq!(exported_a, exported_b);
        assert_eq!(exported_a.network_length(), 24);
        let octets = exported_a.first_address().octets();
        assert_eq!(octets[0], 192);
        assert_eq!(octets[1], 168);
        assert!((100..=254).contains(&octets[2]));
        assert_ne!(exported_a, route);
    }

    #[test]
    fn export_local_route_avoids_same_192_168_third_octet() {
        let destination = test_hash("971a7ac9b42ce6e0faa131bb3c2e7852");
        let route: Ipv4Cidr = "192.168.100.0/24".parse().unwrap();
        let exported = export_local_route(&destination, route);
        assert_ne!(exported, route);
        let octets = exported.first_address().octets();
        assert_eq!(octets[0], 192);
        assert_eq!(octets[1], 168);
        assert!((100..=254).contains(&octets[2]));
    }

    #[test]
    fn configured_peers_can_drop_local_destination() {
        let local = test_hash("971a7ac9b42ce6e0faa131bb3c2e7852");
        let remote = test_hash("fb08aff16ec6f5ccf0d3eb179028e9c3");
        let mut peers = parse_configured_peers(&[local.to_hex_string(), remote.to_hex_string()])
            .expect("configured peers");

        peers.remove(&local);

        assert!(!peers.contains(&local));
        assert!(peers.contains(&remote));
    }

    #[test]
    fn export_local_route_preserves_non_24_routes() {
        let destination = test_hash("971a7ac9b42ce6e0faa131bb3c2e7852");
        let route: Ipv4Cidr = "10.42.0.0/16".parse().unwrap();
        assert_eq!(export_local_route(&destination, route), route);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_tunnel_ip_is_not_treated_as_peer_route() {
        let network: Ipv4Cidr = "10.20.0.0/16".parse().unwrap();
        let peer = test_hash("fb08aff16ec6f5ccf0d3eb179028e9c3");
        let state = test_state(network, peer);
        assert!(is_local_tunnel_ip(&state, state.local_tunnel_ip));
        assert_eq!(resolve_peer_tunnel_ip(&state, state.local_tunnel_ip), None);
        assert_eq!(resolve_peer_route(&state, state.local_tunnel_ip), None);
    }

    #[test]
    fn conflicting_local_routes_blocks_remote_overlap_with_local_networks() {
        let locals = vec![LocalRouteTranslation {
            local: "192.168.10.0/24".parse().unwrap(),
            exported: "192.168.142.0/24".parse().unwrap(),
        }];

        let conflicts = conflicting_local_routes(
            &locals,
            [
                "192.168.10.0/24".parse().unwrap(),
                "192.168.142.0/24".parse().unwrap(),
                "192.168.177.0/24".parse().unwrap(),
            ],
        );

        assert!(conflicts.contains("192.168.10.0/24"));
        assert!(conflicts.contains("192.168.142.0/24"));
        assert!(!conflicts.contains("192.168.177.0/24"));
    }

    #[test]
    fn board_ap_bridge_subnet_is_protected_from_remote_routes() {
        let locals = vec![LocalRouteTranslation {
            local: "192.168.10.0/24".parse().unwrap(),
            exported: "192.168.142.0/24".parse().unwrap(),
        }];

        let conflicts = conflicting_local_routes(&locals, ["192.168.10.0/24".parse().unwrap()]);
        assert!(conflicts.contains("192.168.10.0/24"));
    }

    #[test]
    fn board_station_wifi_subnet_is_protected_from_remote_routes() {
        let locals = vec![LocalRouteTranslation {
            local: "10.0.0.0/24".parse().unwrap(),
            exported: "192.168.142.0/24".parse().unwrap(),
        }];

        let conflicts = conflicting_local_routes(&locals, ["10.0.0.0/24".parse().unwrap()]);
        assert!(conflicts.contains("10.0.0.0/24"));
    }

    #[test]
    fn watchdog_retries_any_non_active_link_state() {
        assert!(!should_watchdog_reconnect("active"));
        assert!(should_watchdog_reconnect("pending"));
        assert!(should_watchdog_reconnect("closed"));
        assert!(should_watchdog_reconnect("configured"));
        assert!(should_watchdog_reconnect("discovered"));
    }

    #[test]
    fn minimal_announce_does_not_carry_routes() {
        let encoded = encode_announce().expect("encode announce");
        assert!(matches!(decode_announce(&encoded), Some(Ok(()))));
        assert_eq!(
            serde_json::from_slice::<VpnAnnounce>(&encoded[VPN_ANNOUNCE_PREFIX.len()..])
                .expect("announce payload")
                .version,
            1
        );
    }

    #[test]
    fn route_sync_round_trips_routes_inside_link_payload() {
        let destination = test_hash("971a7ac9b42ce6e0faa131bb3c2e7852");
        let routes = vec![
            "192.168.124.0/24".parse().unwrap(),
            "172.16.5.0/24".parse().unwrap(),
        ];
        let encoded = encode_route_sync(&destination, &routes).expect("encode route sync");
        let decoded = decode_route_sync(&encoded)
            .expect("control marker")
            .expect("decode control");
        assert_eq!(decoded, (destination, routes));
    }

    #[test]
    fn closed_peer_routes_expire_after_grace_window() {
        let peer = test_hash("fb08aff16ec6f5ccf0d3eb179028e9c3");
        let network: Ipv4Cidr = "10.20.0.0/16".parse().unwrap();
        let mut state = test_state(network, peer);
        let peer_state = state
            .peers
            .get_mut(&peer.to_hex_string())
            .expect("peer state");
        peer_state.announced_routes = vec!["192.168.124.0/24".parse().unwrap()];
        peer_state.route_expires_ts = 10;

        let before = desired_route_owners_at(&state, 9).0;
        let after = desired_route_owners_at(&state, 10).0;

        assert!(before.contains_key(&"192.168.124.0/24".parse::<Ipv4Cidr>().unwrap()));
        assert!(!after.contains_key(&"192.168.124.0/24".parse::<Ipv4Cidr>().unwrap()));
    }
}

fn encode_announce() -> Result<Vec<u8>, VpnRuntimeError> {
    let payload = serde_json::to_vec(&VpnAnnounce { version: 1 })?;
    let mut app_data = VPN_ANNOUNCE_PREFIX.to_vec();
    app_data.extend(payload);
    Ok(app_data)
}

fn decode_announce(data: &[u8]) -> Option<Result<(), VpnRuntimeError>> {
    if !data.starts_with(VPN_ANNOUNCE_PREFIX) {
        return None;
    }

    let payload = match serde_json::from_slice::<VpnAnnounce>(&data[VPN_ANNOUNCE_PREFIX.len()..]) {
        Ok(payload) => payload,
        Err(err) => return Some(Err(err.into())),
    };

    if payload.version != 1 {
        return Some(Err(VpnRuntimeError::Config(format!(
            "unsupported vpn announce version {}",
            payload.version
        ))));
    }

    Some(Ok(()))
}

fn encode_route_sync(
    destination: &AddressHash,
    routes: &[Ipv4Cidr],
) -> Result<Vec<u8>, VpnRuntimeError> {
    let payload = serde_json::to_vec(&VpnRouteSync {
        version: 1,
        destination: destination.to_hex_string(),
        routes: routes.iter().map(ToString::to_string).collect(),
    })?;
    let mut app_data = VPN_CONTROL_PREFIX.to_vec();
    app_data.extend(payload);
    Ok(app_data)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn decode_route_sync(data: &[u8]) -> Option<Result<(AddressHash, Vec<Ipv4Cidr>), VpnRuntimeError>> {
    if !data.starts_with(VPN_CONTROL_PREFIX) {
        return None;
    }

    let payload = match serde_json::from_slice::<VpnRouteSync>(&data[VPN_CONTROL_PREFIX.len()..]) {
        Ok(payload) => payload,
        Err(err) => return Some(Err(err.into())),
    };
    if payload.version != 1 {
        return Some(Err(VpnRuntimeError::Config(format!(
            "unsupported vpn control version {}",
            payload.version
        ))));
    }

    let destination = match AddressHash::new_from_hex_string(&payload.destination) {
        Ok(destination) => destination,
        Err(err) => {
            return Some(Err(VpnRuntimeError::Config(format!(
                "invalid synced destination '{}': {err:?}",
                payload.destination
            ))))
        }
    };

    Some(
        payload
            .routes
            .into_iter()
            .map(|route| {
                route.parse::<Ipv4Cidr>().map_err(|err| {
                    VpnRuntimeError::Config(format!("invalid synced route '{route}': {err}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|routes| (destination, routes)),
    )
}

fn parse_configured_peers(peers: &[String]) -> Result<HashSet<AddressHash>, VpnRuntimeError> {
    peers
        .iter()
        .map(|peer| {
            AddressHash::new_from_hex_string(peer)
                .map_err(|err| VpnRuntimeError::Config(format!("invalid peer '{peer}': {err:?}")))
        })
        .collect()
}

fn discover_local_routes(exclude_interface: Option<&str>) -> Vec<Ipv4Cidr> {
    let mut routes = BTreeSet::new();
    for interface in get_if_addrs().unwrap_or_default() {
        if interface.is_loopback() || interface.is_link_local() {
            continue;
        }
        if should_skip_interface(&interface.name, exclude_interface) {
            continue;
        }
        let IfAddr::V4(addr) = interface.addr else {
            continue;
        };
        if addr.prefixlen == 0 {
            continue;
        }
        if let Ok(route) = Ipv4Cidr::new(addr.ip, addr.prefixlen) {
            routes.insert(route);
        }
    }
    routes.into_iter().collect()
}

fn should_skip_interface(name: &str, exclude_interface: Option<&str>) -> bool {
    if exclude_interface.is_some_and(|exclude| exclude == name) {
        return true;
    }
    matches!(
        name,
        "lo" | "docker0" | "tailscale0" | "zt0" | "utun0" | "utun1" | "utun2" | "utun3"
    ) || name.starts_with("tun")
        || name.starts_with("tap")
        || name.starts_with("docker")
        || name.starts_with("veth")
        || name.starts_with("br-")
}

#[cfg(target_os = "linux")]
fn packet_destination(packet: &[u8]) -> Option<Ipv4Addr> {
    match IpSlice::from_slice(packet).ok()?.destination_addr() {
        std::net::IpAddr::V4(address) => Some(address),
        std::net::IpAddr::V6(_) => None,
    }
}

#[cfg(target_os = "linux")]
fn packet_endpoints(packet: &[u8]) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let slice = IpSlice::from_slice(packet).ok()?;
    match (slice.source_addr(), slice.destination_addr()) {
        (std::net::IpAddr::V4(src), std::net::IpAddr::V4(dst)) => Some((src, dst)),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn log_vpn_rx(packet: &[u8], link_id: AddressHash) {
    if let Some((src_ip, dst_ip)) = packet_endpoints(packet) {
        log::info!(
            "vpn rx {}B src={} dst={} link={}",
            packet.len(),
            src_ip,
            dst_ip,
            link_id
        );
    } else {
        log::info!("vpn rx {}B link={}", packet.len(), link_id);
    }
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(target_os = "linux")]
type PlatformTun = Arc<LinuxTun>;

#[cfg(not(target_os = "linux"))]
type PlatformTun = ();

#[cfg(target_os = "linux")]
struct LinuxTun {
    tun: riptun::TokioTun,
    read_buf: Mutex<[u8; DEFAULT_TUN_MTU]>,
}

#[cfg(target_os = "linux")]
impl LinuxTun {
    fn create() -> Result<(Arc<Self>, String), VpnRuntimeError> {
        let tun = riptun::TokioTun::new(DEFAULT_TUN_NAME, 1)
            .map_err(|err| VpnRuntimeError::Tun(err.to_string()))?;
        let name = tun.name().to_string();
        run_command("ip", &["link", "set", "dev", &name, "up"])?;
        Ok((
            Arc::new(Self {
                tun,
                read_buf: Mutex::new([0u8; DEFAULT_TUN_MTU]),
            }),
            name,
        ))
    }

    async fn read(&self) -> Result<Vec<u8>, std::io::Error> {
        let mut buf = self.read_buf.lock().await;
        let bytes = self.tun.recv(&mut buf[..]).await?;
        Ok(buf[..bytes].to_vec())
    }

    async fn write(&self, packet: &[u8]) -> Result<usize, std::io::Error> {
        self.tun.send(packet).await
    }
}

#[cfg(target_os = "linux")]
fn platform_create_tun() -> Result<Option<PlatformTun>, VpnRuntimeError> {
    let (tun, _) = LinuxTun::create()?;
    Ok(Some(tun))
}

#[cfg(not(target_os = "linux"))]
fn platform_create_tun() -> Result<Option<PlatformTun>, VpnRuntimeError> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn platform_tun_name(tun: Option<&PlatformTun>) -> Option<String> {
    tun.map(|tun| tun.tun.name().to_string())
}

#[cfg(not(target_os = "linux"))]
fn platform_tun_name(_tun: Option<&PlatformTun>) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn platform_enable_forwarding() -> Result<(), VpnRuntimeError> {
    run_command("sysctl", &["-w", "net.ipv4.ip_forward=1"]).map(|_| ())
}

#[cfg(not(target_os = "linux"))]
fn platform_enable_forwarding() -> Result<(), VpnRuntimeError> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_configure_tun_address(
    interface_name: &str,
    address: Ipv4Addr,
    prefix_len: u8,
) -> Result<(), VpnRuntimeError> {
    let cidr = format!("{address}/{prefix_len}");
    run_command("ip", &["addr", "replace", &cidr, "dev", interface_name]).map(|_| ())
}

#[cfg(not(target_os = "linux"))]
fn platform_configure_tun_address(
    _interface_name: &str,
    _address: Ipv4Addr,
    _prefix_len: u8,
) -> Result<(), VpnRuntimeError> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_replace_route(interface_name: &str, route: &str) -> Result<(), VpnRuntimeError> {
    run_command("ip", &["route", "replace", route, "dev", interface_name]).map(|_| ())
}

#[cfg(not(target_os = "linux"))]
fn platform_replace_route(_interface_name: &str, _route: &str) -> Result<(), VpnRuntimeError> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_delete_route(interface_name: &str, route: &str) {
    let _ = run_command("ip", &["route", "del", route, "dev", interface_name]);
}

#[cfg(not(target_os = "linux"))]
fn platform_delete_route(_interface_name: &str, _route: &str) {}

#[cfg(target_os = "linux")]
fn platform_sync_local_translation(
    interface_name: &str,
    translations: &[LocalRouteTranslation],
) -> Result<(), VpnRuntimeError> {
    const PREROUTING_CHAIN: &str = "KAONIC_VPN_PREROUTING";
    const POSTROUTING_CHAIN: &str = "KAONIC_VPN_POSTROUTING";
    let Some(iptables) = resolve_iptables_command() else {
        return Ok(());
    };

    ensure_iptables_chain(iptables, "nat", PREROUTING_CHAIN)?;
    ensure_iptables_chain(iptables, "nat", POSTROUTING_CHAIN)?;
    ensure_iptables_jump(
        iptables,
        "nat",
        "PREROUTING",
        "-i",
        interface_name,
        PREROUTING_CHAIN,
    )?;
    ensure_iptables_jump(
        iptables,
        "nat",
        "POSTROUTING",
        "-o",
        interface_name,
        POSTROUTING_CHAIN,
    )?;
    run_command(iptables, &["-t", "nat", "-F", PREROUTING_CHAIN]).map(|_| ())?;
    run_command(iptables, &["-t", "nat", "-F", POSTROUTING_CHAIN]).map(|_| ())?;

    for translation in translations {
        if translation.local == translation.exported {
            continue;
        }
        let exported = translation.exported.to_string();
        let local = translation.local.to_string();
        run_command(
            iptables,
            &[
                "-t",
                "nat",
                "-A",
                PREROUTING_CHAIN,
                "-d",
                &exported,
                "-j",
                "NETMAP",
                "--to",
                &local,
            ],
        )
        .map(|_| ())?;
        run_command(
            iptables,
            &[
                "-t",
                "nat",
                "-A",
                POSTROUTING_CHAIN,
                "-s",
                &local,
                "-j",
                "NETMAP",
                "--to",
                &exported,
            ],
        )
        .map(|_| ())?;
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn platform_sync_local_translation(
    _interface_name: &str,
    _translations: &[LocalRouteTranslation],
) -> Result<(), VpnRuntimeError> {
    Ok(())
}

fn platform_backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else {
        "mock"
    }
}

#[cfg(target_os = "linux")]
fn platform_supports_route_aliasing() -> bool {
    resolve_iptables_command().is_some()
}

#[cfg(not(target_os = "linux"))]
fn platform_supports_route_aliasing() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn run_command(command: &str, args: &[&str]) -> Result<std::process::Output, VpnRuntimeError> {
    let output = std::process::Command::new(command).args(args).output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(VpnRuntimeError::Config(format!(
            "{command} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(target_os = "linux")]
fn ensure_iptables_chain(command: &str, table: &str, chain: &str) -> Result<(), VpnRuntimeError> {
    match run_command(command, &["-t", table, "-N", chain]) {
        Ok(_) => Ok(()),
        Err(VpnRuntimeError::Config(message)) if message.contains("Chain already exists") => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(target_os = "linux")]
fn ensure_iptables_jump(
    command: &str,
    table: &str,
    parent_chain: &str,
    iface_flag: &str,
    interface_name: &str,
    target_chain: &str,
) -> Result<(), VpnRuntimeError> {
    if run_command(
        command,
        &[
            "-t",
            table,
            "-C",
            parent_chain,
            iface_flag,
            interface_name,
            "-j",
            target_chain,
        ],
    )
    .is_ok()
    {
        return Ok(());
    }

    run_command(
        command,
        &[
            "-t",
            table,
            "-A",
            parent_chain,
            iface_flag,
            interface_name,
            "-j",
            target_chain,
        ],
    )
    .map(|_| ())
}

#[cfg(target_os = "linux")]
fn resolve_iptables_command() -> Option<&'static str> {
    for command in ["iptables", "iptables-nft", "iptables-legacy"] {
        if command_supports_netmap(command) {
            return Some(command);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn command_supports_netmap(command: &str) -> bool {
    let Ok(output) = std::process::Command::new(command)
        .args(["-j", "NETMAP", "-h"])
        .output()
    else {
        return false;
    };

    if output.status.success() {
        return true;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout.contains("NETMAP") || stderr.contains("NETMAP")
}
