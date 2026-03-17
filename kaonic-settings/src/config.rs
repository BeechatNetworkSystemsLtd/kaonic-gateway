use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use cidr::Ipv4Cidr;
use radio_common::{Modulation, RadioConfig};
use serde::{Deserialize, Serialize};

fn default_announce_freq_secs() -> u32 {
    1
}

/// A single received radio frame stored for the dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameRecord {
    pub id: i64,
    pub timestamp: i64, // Unix seconds
    pub module: usize,
    pub size: usize,
    pub data_hex: String, // first 32 bytes as hex
}

/// Radio interface connection settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KaonicCtrlConfig {
    /// Local UDP address to bind the kaonic-ctrl client socket.
    pub listen_addr: SocketAddr,
    /// UDP address of the kaonic-ctrl server daemon.
    pub server_addr: SocketAddr,
    /// Radio module index to use for TX/RX.
    #[serde(default)]
    pub module: usize,
    /// RF frequency and channel settings.
    pub radio_config: Option<RadioConfig>,
    /// Modulation scheme and parameters.
    pub modulation: Option<Modulation>,
}

impl Default for KaonicCtrlConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0), // OS picks port
            server_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 10, 1)), 9090),
            module: 0,
            radio_config: None,
            modulation: None,
        }
    }
}

/// Unified gateway configuration. Does **not** depend on `rns-vpn` or Linux-only crates.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    pub network: Ipv4Cidr,
    pub peers: Vec<String>,
    #[serde(default = "default_announce_freq_secs")]
    pub announce_freq_secs: u32,
    pub kaonic_ctrl_config: Option<KaonicCtrlConfig>,
}
