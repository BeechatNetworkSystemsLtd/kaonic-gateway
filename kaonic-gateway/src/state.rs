use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as TokioMutex;

use kaonic_vpn::VpnRuntime;

use crate::app_types::{NetworkPortStatusDto, RxFrameDto, ServiceStatusDto};
use crate::atak::BridgeMetrics;
use crate::audio::AudioService;
use crate::gateway_reticulum::SharedGatewayReticulum;
use crate::network::NetworkService;
use crate::radio::{SharedRadioClient, SharedTxObserver};
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

#[derive(Default)]
pub struct FrameStats {
    pub rx_frames: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub tx_frames: AtomicU64,
    pub tx_bytes: AtomicU64,
    pub last_rssi: AtomicI32,
}

/// Shared application state — injected as leptos context for server functions.
#[derive(Clone)]
pub struct AppState {
    pub audio: SharedAudioService,
    pub network: SharedNetworkService,
    pub settings: SharedSettings,
    pub atak_metrics: Vec<Arc<BridgeMetrics>>,
    pub vpn_hash: String,
    pub vpn: Option<Arc<VpnRuntime>>,
    pub kaonic_ctrl_server_addr: SocketAddr,
    pub http_addr: SocketAddr,
    pub radio_tx_observer: Option<SharedTxObserver>,
    pub radio_client: Option<SharedRadioClient>,
    pub reticulum: SharedGatewayReticulum,
    pub serial: String,
    /// Ring buffers of recent frame events, one per module (index 0 = A, 1 = B).
    pub rx_buffers: [RxFrameBuffer; 2],
    pub frame_stats: [SharedFrameStats; 2],
}

impl AppState {
    pub fn new(
        settings: SharedSettings,
        atak_metrics: Vec<Arc<BridgeMetrics>>,
        vpn_hash: String,
        vpn: Option<Arc<VpnRuntime>>,
        kaonic_ctrl_server_addr: SocketAddr,
        http_addr: SocketAddr,
        radio_tx_observer: Option<SharedTxObserver>,
        radio_client: Option<SharedRadioClient>,
        reticulum: SharedGatewayReticulum,
        serial: String,
    ) -> Self {
        Self {
            audio: Arc::new(AudioService::new()),
            network: Arc::new(NetworkService::new()),
            settings,
            atak_metrics,
            vpn_hash,
            vpn,
            kaonic_ctrl_server_addr,
            http_addr,
            radio_tx_observer,
            radio_client,
            reticulum,
            serial,
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

        for bridge in &self.atak_metrics {
            ports.push(NetworkPortStatusDto {
                name: format!("ATAK Bridge {}", bridge.port),
                protocol: "UDP".into(),
                port: bridge.port,
                service: "kaonic-gateway.service".into(),
                status: if gateway_active && bridge.dest_hash.get().is_some() {
                    "linked".into()
                } else if gateway_active {
                    "listening".into()
                } else {
                    gateway_status.clone()
                },
                details: format!("Multicast bridge :{}", bridge.port),
            });
        }

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
