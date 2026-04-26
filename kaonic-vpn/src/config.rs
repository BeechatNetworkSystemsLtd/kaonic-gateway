use cidr::Ipv4Cidr;
use serde::{Deserialize, Serialize};

fn default_announce_freq_secs() -> u32 {
    5
}

fn default_allow_all_peers() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnConfig {
    /// Transit network used for deterministic tunnel IP assignment.
    pub network: Ipv4Cidr,
    #[serde(default = "default_allow_all_peers")]
    pub allow_all_peers: bool,
    /// Remote Kaonic Reticulum destination hashes that should participate in the VPN.
    pub peers: Vec<String>,
    /// Extra local subnets to advertise even when they are not auto-detected from interfaces.
    #[serde(default)]
    pub advertised_routes: Vec<Ipv4Cidr>,
    #[serde(default = "default_announce_freq_secs")]
    pub announce_freq_secs: u32,
}
