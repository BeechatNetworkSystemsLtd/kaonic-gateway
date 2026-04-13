use cidr::Ipv4Cidr;
use serde::{Deserialize, Serialize};

fn default_announce_freq_secs() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnConfig {
    /// Transit network used for deterministic tunnel IP assignment.
    pub network: Ipv4Cidr,
    /// Remote Kaonic Reticulum destination hashes that should participate in the VPN.
    pub peers: Vec<String>,
    #[serde(default = "default_announce_freq_secs")]
    pub announce_freq_secs: u32,
}
