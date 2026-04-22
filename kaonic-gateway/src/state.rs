use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::{broadcast, Mutex as TokioMutex};

use kaonic_vpn::VpnRuntime;

use crate::app_types::{NetworkPortStatusDto, RxFrameDto, ServiceStatusDto, WsStatusEvent};
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
    pub ws_events: broadcast::Sender<WsStatusEvent>,
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
