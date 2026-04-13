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
use reticulum::destination::DestinationName;
use reticulum::hash::AddressHash;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::Transport;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::VpnConfig;

const VPN_ANNOUNCE_PREFIX: &[u8] = b"kvpn1:";
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
pub struct VpnSnapshot {
    pub destination_hash: String,
    pub network: String,
    pub local_tunnel_ip: Option<String>,
    pub backend: String,
    pub interface_name: Option<String>,
    pub status: String,
    pub local_routes: Vec<String>,
    pub peers: Vec<VpnPeerSnapshot>,
    pub remote_routes: Vec<VpnRouteSnapshot>,
    pub last_error: Option<String>,
}

#[derive(Clone)]
struct PeerState {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    destination: AddressHash,
    link_state: String,
    announced_routes: Vec<Ipv4Cidr>,
    last_seen_ts: u64,
    last_error: Option<String>,
}

impl PeerState {
    fn new(destination: AddressHash, link_state: &str) -> Self {
        Self {
            destination,
            link_state: link_state.into(),
            announced_routes: Vec::new(),
            last_seen_ts: 0,
            last_error: None,
        }
    }
}

struct VpnRuntimeState {
    destination_hash: String,
    network: Ipv4Cidr,
    local_tunnel_ip: Ipv4Addr,
    backend: String,
    interface_name: Option<String>,
    status: String,
    local_routes: Vec<Ipv4Cidr>,
    peers: BTreeMap<String, PeerState>,
    installed_routes: BTreeSet<String>,
    conflicted_routes: BTreeSet<String>,
    last_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct VpnAnnounce {
    version: u8,
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
        let peers = parse_configured_peers(&config.peers)?;
        validate_peer_network(config.network)?;

        let local_tunnel_ip = derive_tunnel_ip(config.network, &id.address_hash())?;

        let tun = platform_create_tun()?;
        let interface_name = platform_tun_name(tun.as_ref());
        if let Some(interface_name) = interface_name.as_deref() {
            platform_configure_tun_address(interface_name, local_tunnel_ip, config.network.network_length())?;
        }
        let local_routes = discover_local_routes(interface_name.as_deref());
        if interface_name.is_some() {
            platform_enable_forwarding()?;
        }

        let destination = transport
            .lock()
            .await
            .add_destination(id, DestinationName::new("kaonic", "vpn"))
            .await;
        let destination_hash = destination.lock().await.desc.address_hash;

        let runtime = Arc::new(Self {
            state: Mutex::new(VpnRuntimeState {
                destination_hash: destination_hash.to_hex_string(),
                network: config.network,
                local_tunnel_ip,
                backend: platform_backend_name().into(),
                interface_name: interface_name.clone(),
                status: if interface_name.is_some() {
                    "running".into()
                } else {
                    "mock".into()
                },
                local_routes,
                peers: peers
                    .iter()
                    .map(|peer| {
                        (peer.to_hex_string(), PeerState::new(*peer, "configured"))
                    })
                    .collect(),
                installed_routes: BTreeSet::new(),
                conflicted_routes: BTreeSet::new(),
                last_error: None,
            }),
        });

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
                            let routes = discover_local_routes(interface_name.as_deref());
                            runtime.set_local_routes(routes.clone()).await;
                            match encode_announce(&routes) {
                                Ok(app_data) => transport.lock().await.send_announce(&destination, Some(&app_data)).await,
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
                                    Ok(routes) => {
                                        runtime.ensure_peer(destination.address_hash, "discovered").await;
                                        runtime.update_peer_routes(destination.address_hash, routes).await;
                                        let existing = transport.lock().await.find_out_link(&destination.address_hash).await;
                                        if existing.is_none() {
                                            transport.lock().await.link(destination).await;
                                        }
                                        if let Err(err) = runtime.sync_routes().await {
                                            runtime.record_error(err.to_string()).await;
                                        }
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
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(_) => break,
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
                                    let Some(peer) = runtime.resolve_peer_for_ip(dst_ip).await else { continue; };
                                    let sent = transport.lock().await.send_to_out_links(&peer, &packet).await;
                                    if sent.is_empty() {
                                        transport.lock().await.send_to_in_links(&peer, &packet).await;
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
                    let mut in_link_events = transport.lock().await.in_link_events();
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => break,
                            recv = in_link_events.recv() => match recv {
                                Ok(event) => match event.event {
                                    LinkEvent::Data(payload) if event.address_hash == destination_hash => {
                                        if let Err(err) = tun.write(payload.as_slice()).await {
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

    async fn set_local_routes(&self, routes: Vec<Ipv4Cidr>) {
        let mut state = self.state.lock().await;
        state.local_routes = routes;
        state.last_error = None;
    }

    async fn update_peer_routes(&self, destination: AddressHash, routes: Vec<Ipv4Cidr>) {
        let mut state = self.state.lock().await;
        let peer = state
            .peers
            .entry(destination.to_hex_string())
            .or_insert_with(|| PeerState::new(destination, "discovered"));
        peer.announced_routes = routes;
        peer.last_seen_ts = unix_timestamp_secs();
        peer.last_error = None;
        state.last_error = None;
    }

    async fn record_peer_error(&self, destination: AddressHash, message: String) {
        let mut state = self.state.lock().await;
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
        let peer = state
            .peers
            .entry(destination.to_hex_string())
            .or_insert_with(|| PeerState::new(destination, "discovered"));
        peer.link_state = link_state.into();
        if link_state == "active" {
            peer.last_seen_ts = unix_timestamp_secs();
        }
        if state.status != "mock" {
            state.status = "running".into();
        }
    }

    async fn ensure_peer(&self, destination: AddressHash, link_state: &str) {
        let mut state = self.state.lock().await;
        state
            .peers
            .entry(destination.to_hex_string())
            .or_insert_with(|| PeerState::new(destination, link_state));
    }

    #[cfg(target_os = "linux")]
    async fn resolve_peer_for_ip(&self, address: Ipv4Addr) -> Option<AddressHash> {
        let state = self.state.lock().await;
        let mut best: Option<(u8, AddressHash)> = None;
        for (route, owner) in desired_route_owners(&state).0 {
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

    async fn sync_routes(&self) -> Result<(), VpnRuntimeError> {
        let (interface_name, desired_routes, conflicts, installed_routes) = {
            let state = self.state.lock().await;
            let (desired_routes, conflicts) = desired_route_owners(&state);
            (
                state.interface_name.clone(),
                desired_routes,
                conflicts,
                state.installed_routes.clone(),
            )
        };

        let desired_strings = desired_routes
            .keys()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();

        if let Some(interface_name) = interface_name {
            for route in installed_routes.difference(&desired_strings) {
                platform_delete_route(&interface_name, route);
            }
            for route in &desired_strings {
                platform_replace_route(&interface_name, route)?;
            }
        }

        let mut state = self.state.lock().await;
        state.installed_routes = desired_strings;
        state.conflicted_routes = conflicts;
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

fn build_snapshot(state: &VpnRuntimeState) -> VpnSnapshot {
    let mut peers = state
        .peers
        .iter()
        .map(|(destination, peer)| VpnPeerSnapshot {
            destination: destination.clone(),
            tunnel_ip: assign_tunnel_ip_for_peer(state.network, peer).map(|ip| ip.to_string()),
            link_state: peer.link_state.clone(),
            announced_routes: peer
                .announced_routes
                .iter()
                .map(ToString::to_string)
                .collect(),
            last_seen_ts: peer.last_seen_ts,
            last_error: peer.last_error.clone(),
        })
        .collect::<Vec<_>>();
    peers.sort_by(|a, b| a.destination.cmp(&b.destination));

    let (desired_routes, conflicts) = desired_route_owners(state);
    let mut remote_routes = desired_routes
        .iter()
        .map(|(route, owner)| {
            let peer = state.peers.get(owner);
            VpnRouteSnapshot {
                network: route.to_string(),
                owner: owner.clone(),
                status: if conflicts.contains(&route.to_string()) {
                    "conflict".into()
                } else {
                    "active".into()
                },
                last_seen_ts: peer.map(|peer| peer.last_seen_ts).unwrap_or_default(),
                installed: state.installed_routes.contains(&route.to_string()),
            }
        })
        .collect::<Vec<_>>();

    for route in conflicts {
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

    let mut local_routes = state
        .local_routes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    local_routes.sort();

    VpnSnapshot {
        destination_hash: state.destination_hash.clone(),
        network: state.network.to_string(),
        local_tunnel_ip: Some(state.local_tunnel_ip.to_string()),
        backend: state.backend.clone(),
        interface_name: state.interface_name.clone(),
        status: state.status.clone(),
        local_routes,
        peers,
        remote_routes,
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

fn derive_tunnel_ip(network: Ipv4Cidr, destination: &AddressHash) -> Result<Ipv4Addr, VpnRuntimeError> {
    validate_peer_network(network)?;

    let host_bits = 32 - network.network_length() as u32;
    let usable_hosts = (1u64 << host_bits) - 2;
    let mut seed = 0u64;
    for byte in destination.as_slice() {
        seed = seed.wrapping_mul(131).wrapping_add(u64::from(*byte));
    }
    let host_offset = (seed % usable_hosts) + 1;
    let network_base = u32::from(network.first_address());
    Ok(Ipv4Addr::from(network_base.wrapping_add(host_offset as u32)))
}

fn desired_route_owners(state: &VpnRuntimeState) -> (BTreeMap<Ipv4Cidr, String>, BTreeSet<String>) {
    let mut owners = BTreeMap::<Ipv4Cidr, String>::new();
    let mut conflicts = BTreeSet::<String>::new();

    for (destination, peer) in &state.peers {
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

fn encode_announce(routes: &[Ipv4Cidr]) -> Result<Vec<u8>, VpnRuntimeError> {
    let payload = serde_json::to_vec(&VpnAnnounce {
        version: 1,
        routes: routes.iter().map(ToString::to_string).collect(),
    })?;
    let mut app_data = VPN_ANNOUNCE_PREFIX.to_vec();
    app_data.extend(payload);
    Ok(app_data)
}

fn decode_announce(data: &[u8]) -> Option<Result<Vec<Ipv4Cidr>, VpnRuntimeError>> {
    if !data.starts_with(VPN_ANNOUNCE_PREFIX) {
        return None;
    }

    let payload = match serde_json::from_slice::<VpnAnnounce>(&data[VPN_ANNOUNCE_PREFIX.len()..]) {
        Ok(payload) => payload,
        Err(err) => return Some(Err(err.into())),
    };

    Some(
        payload
            .routes
            .into_iter()
            .map(|route| {
                route.parse::<Ipv4Cidr>().map_err(|err| {
                    VpnRuntimeError::Config(format!("invalid announced route '{route}': {err}"))
                })
            })
            .collect(),
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
        "lo"
            | "docker0"
            | "tailscale0"
            | "zt0"
            | "utun0"
            | "utun1"
            | "utun2"
            | "utun3"
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

fn platform_backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else {
        "mock"
    }
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
