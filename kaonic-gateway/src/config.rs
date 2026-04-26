use cidr::Ipv4Cidr;
use serde::{Deserialize, Serialize};

use crate::radio::HardwareRadioConfig;

fn default_announce_freq_secs() -> u32 {
    5
}

fn default_allow_all_peers() -> bool {
    true
}

fn default_advertised_routes() -> Vec<Ipv4Cidr> {
    vec!["192.168.10.0/24"
        .parse()
        .expect("valid default Kaonic route")]
}

/// Full application configuration: VPN settings + radio hardware config.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    pub network: Ipv4Cidr,
    #[serde(default = "default_allow_all_peers")]
    pub allow_all_peers: bool,
    pub peers: Vec<String>,
    #[serde(default = "default_advertised_routes")]
    pub advertised_routes: Vec<Ipv4Cidr>,
    #[serde(default = "default_announce_freq_secs")]
    pub announce_freq_secs: u32,
    #[serde(default)]
    pub radio: HardwareRadioConfig,
}
