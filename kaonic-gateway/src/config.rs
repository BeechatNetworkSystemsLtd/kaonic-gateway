use cidr::Ipv4Cidr;
use serde::{Deserialize, Serialize};

use crate::radio::HardwareRadioConfig;

fn default_announce_freq_secs() -> u32 { 1 }

/// Full application configuration: VPN settings + radio hardware config.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    pub network: Ipv4Cidr,
    pub peers: Vec<String>,
    #[serde(default = "default_announce_freq_secs")]
    pub announce_freq_secs: u32,
    #[serde(default)]
    pub radio: HardwareRadioConfig,
}
