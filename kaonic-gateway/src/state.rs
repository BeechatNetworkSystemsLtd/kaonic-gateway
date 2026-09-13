use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::{broadcast, Mutex as TokioMutex};

use kaonic_vpn::VpnRuntime;

use crate::app_types::{NetworkPortStatusDto, RxFrameDto, ServiceStatusDto, WsStatusEvent};
use crate::audio::AudioService;
use crate::gateway_reticulum::SharedGatewayReticulum;
use crate::network::NetworkService;
use crate::radio::{SharedRadioClient, SharedTxObserver};
use crate::media_bridge::MediaBridge;
use crate::remote::SharedRemote;
use kaonic_reticulum::FecSelector;
use crate::settings::Settings;

pub type SharedAudioService = Arc<AudioService>;
pub type SharedNetworkService = Arc<NetworkService>;
pub type SharedSettings = Arc<Mutex<Settings>>;
pub type RxFrameBuffer = Arc<TokioMutex<VecDeque<RxFrameDto>>>;
pub type SharedFrameStats = Arc<FrameStats>;

pub const RX_BUF_SIZE: usize = 256;

fn empty_rx_buffer() -> RxFrameBuffer {
    Arc::new(TokioMutex::new(VecDeque::new()))
}

fn empty_frame_stats() -> SharedFrameStats {
    Arc::new(FrameStats::default())
}

fn ws_event_bus() -> broadcast::Sender<WsStatusEvent> {
    let (tx, _) = broadcast::channel(256);
    tx
}

#[derive(Default)]
struct FrameRateState {
    last_sample: Option<Instant>,
    last_rx_bytes: u64,
    last_tx_bytes: u64,
    rx_bps: u64,
    tx_bps: u64,
}

pub struct FrameStats {
    pub rx_frames: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub tx_frames: AtomicU64,
    pub tx_bytes: AtomicU64,
    pub last_rssi: AtomicI32,
    rate: Mutex<FrameRateState>,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self {
            rx_frames: AtomicU64::new(0),
            rx_bytes: AtomicU64::new(0),
            tx_frames: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            last_rssi: AtomicI32::new(0),
            rate: Mutex::new(FrameRateState::default()),
        }
    }
}

impl FrameStats {
    pub fn rates(&self, rx_bytes: u64, tx_bytes: u64) -> (u64, u64) {
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();

        if let Some(last_sample) = rate.last_sample {
            let elapsed = now.duration_since(last_sample).as_secs_f64();
            if elapsed >= 0.2 {
                rate.rx_bps =
                    ((rx_bytes.saturating_sub(rate.last_rx_bytes)) as f64 / elapsed) as u64;
                rate.tx_bps =
                    ((tx_bytes.saturating_sub(rate.last_tx_bytes)) as f64 / elapsed) as u64;
                rate.last_sample = Some(now);
                rate.last_rx_bytes = rx_bytes;
                rate.last_tx_bytes = tx_bytes;
            }
        } else {
            rate.last_sample = Some(now);
            rate.last_rx_bytes = rx_bytes;
            rate.last_tx_bytes = tx_bytes;
        }

        (rate.rx_bps, rate.tx_bps)
    }
}

/// Clients reaching the API from inside the VPN are remote peers, not the
/// device's operator; endpoints that change what a peer may do are refused
/// for them.
pub fn is_mesh_client(state: &AppState, addr: std::net::SocketAddr) -> bool {
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => ip,
            None => return false,
        },
    };
    is_mesh_client_ip(state.vpn_network, ip)
}

fn is_mesh_client_ip(vpn_network: Option<cidr::Ipv4Cidr>, ip: std::net::Ipv4Addr) -> bool {
    vpn_network.is_some_and(|network| network.contains(&ip))
}

/// Rewrites the chosen router to the identifier the operator uses.
///
/// Inside the tunnel a peer is its VPN destination hash; on the Remote page and
/// in every list a person reads, it is its identity hash. Those are different
/// values, so a snapshot handed to the UI has to be translated — otherwise the
/// UI compares an identity hash against a tunnel hash, finds no match, and the
/// selection silently never appears to take.
/// Joins operator-facing names onto the tunnel's peer list.
///
/// The VPN layer knows destinations; people know codenames and their own
/// labels. Doing the join here means every surface — page, websocket, API —
/// shows the same names without each one repeating the lookup.
pub fn present_peer_names(state: &AppState, snapshot: &mut kaonic_vpn::VpnSnapshot) {
    let paired = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_remote_paired()
        .unwrap_or_default();
    let tags = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_remote_tags()
        .unwrap_or_default();
    for peer in snapshot.peers.iter_mut() {
        let Some(node) = paired.iter().find(|node| {
            crate::remote::vpn_destination_hash(node)
                .map(|hash| hash.to_hex_string() == peer.destination)
                .unwrap_or(false)
        }) else {
            continue;
        };
        peer.identity_hash = node.identity_hash.clone();
        peer.codename = node.codename.clone();
        peer.tag = tags
            .iter()
            .find(|(hash, _)| *hash == node.identity_hash)
            .map(|(_, tag)| tag.clone())
            .unwrap_or_default();
    }
}

pub fn present_uplink_peer(state: &AppState, snapshot: &mut kaonic_vpn::VpnSnapshot) {
    if snapshot.uplink.peer.is_none() {
        return;
    }
    let stored = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_vpn_uplink()
        .ok()
        .and_then(|saved| saved.peer);
    snapshot.uplink.peer = stored;
}

/// Shared application state — injected as leptos context for server functions.
#[derive(Clone)]
pub struct AppState {
    pub audio: SharedAudioService,
    pub network: SharedNetworkService,
    pub settings: SharedSettings,
    pub vpn_hash: String,
    pub vpn: Option<Arc<VpnRuntime>>,
    pub kaonic_ctrl_server_addr: SocketAddr,
    pub http_addr: SocketAddr,
    pub radio_tx_observer: Option<SharedTxObserver>,
    pub radio_client: Option<SharedRadioClient>,
    pub reticulum: SharedGatewayReticulum,
    pub remote: Option<SharedRemote>,
    pub fec: Option<Arc<FecSelector>>,
    /// Tunnel subnet, used to distinguish local operators from mesh peers.
    pub vpn_network: Option<cidr::Ipv4Cidr>,
    pub media: Option<Arc<MediaBridge>>,
    pub serial: String,
    pub ws_events: broadcast::Sender<WsStatusEvent>,
    /// Ring buffers of recent frame events, one per module (index 0 = A, 1 = B).
    pub rx_buffers: [RxFrameBuffer; 2],
    pub frame_stats: [SharedFrameStats; 2],
}

impl AppState {
    pub fn new(
        settings: SharedSettings,
        vpn_hash: String,
        vpn: Option<Arc<VpnRuntime>>,
        kaonic_ctrl_server_addr: SocketAddr,
        http_addr: SocketAddr,
        radio_tx_observer: Option<SharedTxObserver>,
        radio_client: Option<SharedRadioClient>,
        reticulum: SharedGatewayReticulum,
        remote: Option<SharedRemote>,
        fec: Option<Arc<FecSelector>>,
        vpn_network: Option<cidr::Ipv4Cidr>,
        media: Option<Arc<MediaBridge>>,
        serial: String,
    ) -> Self {
        Self {
            audio: Arc::new(AudioService::new()),
            network: Arc::new(NetworkService::new()),
            settings,
            vpn_hash,
            vpn,
            kaonic_ctrl_server_addr,
            http_addr,
            radio_tx_observer,
            radio_client,
            reticulum,
            remote,
            fec,
            vpn_network,
            media,
            serial,
            ws_events: ws_event_bus(),
            rx_buffers: [empty_rx_buffer(), empty_rx_buffer()],
            frame_stats: [empty_frame_stats(), empty_frame_stats()],
        }
    }

    pub fn network_ports(&self, services: &[ServiceStatusDto]) -> Vec<NetworkPortStatusDto> {
        let commd_active = service_active(services, "kaonic-commd.service");
        let gateway_active = service_active(services, "kaonic-gateway.service");
        let commd_status = service_label(services, "kaonic-commd.service");
        let gateway_status = service_label(services, "kaonic-gateway.service");

        let mut ports = vec![
            NetworkPortStatusDto {
                name: "kaonic-commd gRPC".into(),
                protocol: "TCP".into(),
                port: 50051,
                service: "kaonic-commd.service".into(),
                status: if commd_active {
                    "listening".into()
                } else {
                    commd_status.clone()
                },
                details: "Radio daemon gRPC API".into(),
            },
            NetworkPortStatusDto {
                name: "kaonic-commd control".into(),
                protocol: "UDP".into(),
                port: self.kaonic_ctrl_server_addr.port(),
                service: "kaonic-commd.service".into(),
                status: if commd_active {
                    "reachable".into()
                } else {
                    commd_status.clone()
                },
                details: self.kaonic_ctrl_server_addr.ip().to_string(),
            },
            NetworkPortStatusDto {
                name: "kaonic-gateway HTTP".into(),
                protocol: "TCP".into(),
                port: self.http_addr.port(),
                service: "kaonic-gateway.service".into(),
                status: if gateway_active {
                    "listening".into()
                } else {
                    gateway_status.clone()
                },
                details: "Dashboard, API, WebSocket".into(),
            },
        ];

        ports.sort_by(|a, b| {
            a.port
                .cmp(&b.port)
                .then_with(|| a.protocol.cmp(&b.protocol))
        });
        ports
    }
}

fn service_active(services: &[ServiceStatusDto], unit: &str) -> bool {
    services
        .iter()
        .find(|service| service.unit == unit)
        .map(|service| service.load_state == "loaded" && service.active_state == "active")
        .unwrap_or(false)
}

fn service_label(services: &[ServiceStatusDto], unit: &str) -> String {
    services
        .iter()
        .find(|service| service.unit == unit)
        .map(|service| {
            if service.load_state != "loaded" {
                service.load_state.clone()
            } else {
                service.active_state.clone()
            }
        })
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::is_mesh_client_ip;
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    #[test]
    fn mesh_clients_are_only_those_inside_the_tunnel() {
        let vpn = cidr::Ipv4Cidr::from_str("10.20.0.0/16").ok();
        assert!(is_mesh_client_ip(vpn, Ipv4Addr::new(10, 20, 124, 1)));
        assert!(!is_mesh_client_ip(vpn, Ipv4Addr::new(192, 168, 1, 20)));
        assert!(!is_mesh_client_ip(vpn, Ipv4Addr::LOCALHOST));
        // With no VPN configured nothing is treated as a mesh client.
        assert!(!is_mesh_client_ip(None, Ipv4Addr::new(10, 20, 124, 1)));
    }
}
