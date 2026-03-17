use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use cidr::Ipv4Cidr;
use radio_common::{Modulation, RadioConfig};
use serde::{Deserialize, Serialize};

fn default_announce_freq_secs() -> u32 { 1 }

/// Radio interface connection settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KaonicCtrlConfig {
    pub listen_addr: SocketAddr,
    pub server_addr: SocketAddr,
    #[serde(default)]
    pub module: usize,
    pub radio_config: Option<RadioConfig>,
    pub modulation: Option<Modulation>,
}

impl Default for KaonicCtrlConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            server_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 10, 1)), 9090),
            module: 0,
            radio_config: None,
            modulation: None,
        }
    }
}

/// Unified gateway configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    pub network: Ipv4Cidr,
    pub peers: Vec<String>,
    #[serde(default = "default_announce_freq_secs")]
    pub announce_freq_secs: u32,
    /// Per-module radio interface configs (one entry per RF215 module).
    #[serde(default)]
    pub kaonic_ctrl_configs: Vec<KaonicCtrlConfig>,
}
