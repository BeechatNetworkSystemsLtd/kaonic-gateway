use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, AtomicU64};
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as TokioMutex;

use crate::app_types::RxFrameDto;
use crate::atak::BridgeMetrics;
use crate::audio::AudioService;
use crate::network::NetworkService;
use crate::radio::SharedRadioClient;
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
    pub radio_client: Option<SharedRadioClient>,
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
        radio_client: Option<SharedRadioClient>,
        serial: String,
    ) -> Self {
        Self {
            audio: Arc::new(AudioService::new()),
            network: Arc::new(NetworkService::new()),
            settings,
            atak_metrics,
            vpn_hash,
            radio_client,
            serial,
            rx_buffers: [empty_rx_buffer(), empty_rx_buffer()],
            frame_stats: [empty_frame_stats(), empty_frame_stats()],
        }
    }
}
