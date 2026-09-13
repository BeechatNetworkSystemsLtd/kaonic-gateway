use cidr::Ipv4Cidr;
use serde::{Deserialize, Serialize};

fn default_announce_freq_secs() -> u32 {
    5
}

/// Off. Accepting any node that can reach us over the radio would let a
/// stranger take a tunnel address and advertise routes into this node — the
/// VPN's membership has to be a decision, not a default. Nodes paired through
/// the remote feature are admitted without this; see `PeerAuthority`.
///
/// Existing installations keep whatever value is in their database; this only
/// governs a node that has never been configured.
fn default_allow_all_peers() -> bool {
    false
}

fn default_gateway_enabled() -> bool {
    false
}

/// Acting as a gateway for other nodes: forwarding their tunnel traffic onto
/// a local network or the internet, with this node's own address as the
/// source so the far end needs no route back into the tunnel.
///
/// Off unless the operator turns it on, and even then it forwards only to the
/// destinations they name. Sharing a network is a decision, not a default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnGatewayConfig {
    #[serde(default = "default_gateway_enabled")]
    pub enabled: bool,
    /// Interface to forward out of. `None` uses whichever carries the default
    /// route, which is the right answer on a node with one uplink.
    #[serde(default)]
    pub egress_interface: Option<String>,
    /// Destinations peers may reach through this node, as they exist locally
    /// (`192.168.5.0/24` for a LAN with a TAK server on it; `0.0.0.0/0` for
    /// full internet access). Empty forwards nothing.
    #[serde(default)]
    pub routes: Vec<Ipv4Cidr>,
}

impl Default for VpnGatewayConfig {
    fn default() -> Self {
        Self {
            enabled: default_gateway_enabled(),
            egress_interface: None,
            routes: Vec::new(),
        }
    }
}

/// Using another node as this node's router.
///
/// One choice, deliberately: a node has one way out, and letting several peers
/// each claim a slice of the routing table is how you get a mesh nobody can
/// reason about. Selecting a router is also what permits that peer — and only
/// that peer — to hand this node a default route.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnUplinkConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Reticulum destination hash of the node to route through.
    #[serde(default)]
    pub peer: Option<String>,
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
    #[serde(default)]
    pub gateway: VpnGatewayConfig,
    #[serde(default)]
    pub uplink: VpnUplinkConfig,
}
