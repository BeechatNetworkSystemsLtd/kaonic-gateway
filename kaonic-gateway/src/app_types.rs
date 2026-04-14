use radio_common::{
    modulation::{Modulation, OfdmModulation},
    RadioConfig,
};
use serde::{Deserialize, Serialize};
use kaonic_vpn::VpnSnapshot;

// ── Radio ────────────────────────────────────────────────────────────────────

/// WASM-safe mirror of `crate::radio::RadioModuleConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadioModuleConfigDto {
    pub radio_config: RadioConfig,
    pub modulation: Modulation,
}

impl Default for RadioModuleConfigDto {
    fn default() -> Self {
        Self {
            radio_config: radio_common::RadioConfigBuilder::new().build(),
            modulation: Modulation::Ofdm(OfdmModulation::default()),
        }
    }
}

// ── Received frames ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RxFrameDto {
    pub module: usize,
    pub direction: String,
    pub rssi: i8,
    pub len: u16,
    pub hex: String, // hex preview of first 8 bytes
    pub ts: u64,     // unix timestamp (seconds)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrameStatsDto {
    pub rx_frames: u64,
    pub rx_bytes: u64,
    pub tx_frames: u64,
    pub tx_bytes: u64,
    pub last_rssi: Option<i8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtakBridgeStatusDto {
    pub port: u16,
    pub dest_hash: String,
    pub rx_packets: u64,
    pub tx_packets: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemStatusDto {
    pub cpu_percent: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub fs_free_mb: u64,
    pub fs_total_mb: u64,
    pub os_details: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceStatusDto {
    pub unit: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkPortStatusDto {
    pub name: String,
    pub protocol: String,
    pub port: u16,
    pub service: String,
    pub status: String,
    pub details: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReticulumLinkDto {
    pub id: String,
    pub destination: String,
    pub status: String,
    pub last_event: String,
    pub packets: u64,
    pub bytes: u64,
    pub rtt_ms: Option<u64>,
    pub last_seen_ts: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReticulumEventDto {
    pub ts: u64,
    pub direction: String,
    pub kind: String,
    pub link_id: String,
    pub destination: String,
    pub details: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReticulumSnapshotDto {
    pub incoming_links: Vec<ReticulumLinkDto>,
    pub outgoing_links: Vec<ReticulumLinkDto>,
    pub events: Vec<ReticulumEventDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayStatusDto {
    pub serial: String,
    pub vpn_hash: String,
    pub atak_bridges: Vec<AtakBridgeStatusDto>,
    pub network_ports: Vec<NetworkPortStatusDto>,
    pub system: SystemStatusDto,
    pub services: Vec<ServiceStatusDto>,
    pub radio_modules: Vec<RadioModuleConfigDto>,
    pub reticulum: ReticulumSnapshotDto,
    pub vpn: VpnSnapshot,
}

impl Default for GatewayStatusDto {
    fn default() -> Self {
        Self {
            serial: String::new(),
            vpn_hash: String::new(),
            atak_bridges: vec![],
            network_ports: vec![],
            system: SystemStatusDto::default(),
            services: vec![],
            radio_modules: vec![],
            reticulum: ReticulumSnapshotDto::default(),
            vpn: VpnSnapshot::default(),
        }
    }
}

// ── Network ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiStatusDto {
    pub mode: String,
    pub configured_ssid: Option<String>,
    pub connected_ssid: Option<String>,
    pub wlan0_ip: Option<String>,
    pub hostapd_status: String,
    pub wpa_supplicant_status: String,
    pub link_details: String,
}

impl Default for WifiStatusDto {
    fn default() -> Self {
        Self {
            mode: "ap".into(),
            configured_ssid: None,
            connected_ssid: None,
            wlan0_ip: None,
            hostapd_status: String::new(),
            wpa_supplicant_status: String::new(),
            link_details: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSnapshotDto {
    pub backend: String,
    pub interface_source: String,
    pub interface_details: String,
    pub wifi: WifiStatusDto,
}

impl Default for NetworkSnapshotDto {
    fn default() -> Self {
        Self {
            backend: String::new(),
            interface_source: String::new(),
            interface_details: String::new(),
            wifi: WifiStatusDto::default(),
        }
    }
}

// ── Settings ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySettingsDto {
    /// CIDR network string, e.g. `"10.0.0.0/24"`.
    pub network: String,
    pub peers: Vec<String>,
    pub advertised_routes: Vec<String>,
    pub announce_freq_secs: u32,
    /// Exactly 2 radio modules.
    pub radio_modules: [RadioModuleConfigDto; 2],
}

impl Default for GatewaySettingsDto {
    fn default() -> Self {
        Self {
            network: "10.0.0.0/24".into(),
            peers: vec![],
            advertised_routes: vec![],
            announce_freq_secs: 1,
            radio_modules: [
                RadioModuleConfigDto::default(),
                RadioModuleConfigDto::default(),
            ],
        }
    }
}

use crate::radio::{HardwareRadioConfig, RadioModuleConfig};

impl From<RadioModuleConfig> for RadioModuleConfigDto {
    fn from(c: RadioModuleConfig) -> Self {
        Self {
            radio_config: c.radio_config,
            modulation: c.modulation,
        }
    }
}

impl From<RadioModuleConfigDto> for RadioModuleConfig {
    fn from(d: RadioModuleConfigDto) -> Self {
        Self {
            radio_config: d.radio_config,
            modulation: d.modulation,
        }
    }
}

impl From<crate::config::GatewayConfig> for GatewaySettingsDto {
    fn from(c: crate::config::GatewayConfig) -> Self {
        Self {
            network: c.network.to_string(),
            peers: c.peers,
            advertised_routes: c
                .advertised_routes
                .into_iter()
                .map(|route| route.to_string())
                .collect(),
            announce_freq_secs: c.announce_freq_secs,
            radio_modules: [
                c.radio.module_configs[0].clone().into(),
                c.radio.module_configs[1].clone().into(),
            ],
        }
    }
}

impl TryFrom<GatewaySettingsDto> for crate::config::GatewayConfig {
    type Error = String;

    fn try_from(d: GatewaySettingsDto) -> Result<Self, Self::Error> {
        let network = d
            .network
            .parse()
            .map_err(|e| format!("invalid network CIDR '{}: {e}", d.network))?;
        let advertised_routes = d
            .advertised_routes
            .into_iter()
            .map(|route| {
                route.parse().map_err(|e| format!("invalid advertised route '{route}': {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::config::GatewayConfig {
            network,
            peers: d.peers,
            advertised_routes,
            announce_freq_secs: d.announce_freq_secs,
            radio: HardwareRadioConfig {
                module_configs: [
                    RadioModuleConfig::from(d.radio_modules[0].clone()),
                    RadioModuleConfig::from(d.radio_modules[1].clone()),
                ],
            },
        })
    }
}
