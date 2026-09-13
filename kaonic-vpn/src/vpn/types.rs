//! Public snapshot DTOs returned by `VpnRuntime::snapshot`.
//!
//! Shape is stable for the gateway HTTP API + Leptos VPN page. Keep field
//! names/types unchanged when refactoring internal state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnPeerSnapshot {
    pub destination: String,
    /// The peer will forward traffic for the networks it advertises. Selecting
    /// a peer that will not is the difference between a working route and a
    /// silent black hole, so it is reported per peer.
    #[serde(default)]
    pub is_gateway: bool,
    /// Operator-facing identity, joined in by the gateway.
    #[serde(default)]
    pub identity_hash: String,
    #[serde(default)]
    pub codename: String,
    #[serde(default)]
    pub tag: String,
    pub tunnel_ip: Option<String>,
    pub link_state: String,
    pub announced_routes: Vec<String>,
    pub last_seen_ts: u64,
    pub last_error: Option<String>,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bps: u64,
    pub rx_bps: u64,
    pub last_tx_ts: u64,
    pub last_rx_ts: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnRouteSnapshot {
    pub network: String,
    pub owner: String,
    pub status: String,
    pub last_seen_ts: u64,
    pub installed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnRouteMappingSnapshot {
    pub subnet: String,
    pub tunnel: String,
    pub mapped_subnet: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnSnapshot {
    pub destination_hash: String,
    pub network: String,
    pub local_tunnel_ip: Option<String>,
    pub backend: String,
    pub interface_name: Option<String>,
    pub status: String,
    pub advertised_routes: Vec<String>,
    pub local_routes: Vec<String>,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_bps: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub rx_bps: u64,
    pub drop_packets: u64,
    pub last_tx_ts: u64,
    pub last_rx_ts: u64,
    pub peers: Vec<VpnPeerSnapshot>,
    pub remote_routes: Vec<VpnRouteSnapshot>,
    pub route_mappings: Vec<VpnRouteMappingSnapshot>,
    #[serde(default)]
    pub gateway: VpnGatewaySnapshot,
    #[serde(default)]
    pub uplink: VpnUplinkSnapshot,
    /// The most recent packets over the tunnel, newest first.
    #[serde(default)]
    pub recent: Vec<VpnPacketSnapshot>,
    pub last_error: Option<String>,
}

/// One packet that crossed the tunnel, as a person would read it.
///
/// Deliberately a summary, not a capture: addresses, protocol, size and
/// direction answer "is my traffic actually going over the radio, and what is
/// it", which is the question. Payloads are never retained — they are other
/// people's data, and a node with 390 MB of RAM has nowhere to put them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnPacketSnapshot {
    /// Unix seconds.
    pub ts: u64,
    /// Milliseconds within the second, so a burst does not collapse into one
    /// timestamp.
    pub ms: u32,
    /// "tx" leaving this node, "rx" arriving.
    pub dir: String,
    pub src: String,
    pub dst: String,
    /// "TCP", "UDP", "ICMP" or the raw protocol number.
    pub proto: String,
    /// Transport ports, for the protocols that have them. Together with the
    /// addresses these identify a flow, which is what turns a list of packets
    /// into something you can follow.
    #[serde(default)]
    pub sport: Option<u16>,
    #[serde(default)]
    pub dport: Option<u16>,
    pub len: u32,
}

/// The chosen router, and whether traffic is actually going through it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnUplinkSnapshot {
    pub enabled: bool,
    pub active: bool,
    /// Destination hash of the chosen router.
    pub peer: Option<String>,
    /// Its codename, when a peer with that hash is known.
    pub peer_codename: Option<String>,
    /// Networks reachable through it right now.
    pub routes: Vec<String>,
    /// True when the router shares its whole uplink, so this node's general
    /// internet traffic goes over the radio.
    pub default_route: bool,
    pub detail: Option<String>,
}

/// Gateway mode as it is actually running, which is not always what was asked
/// for: `active` is false when the feature is on but nothing can be forwarded
/// (no egress interface found, no destinations named, or no tun).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnGatewaySnapshot {
    pub enabled: bool,
    pub active: bool,
    /// Interface being forwarded out of, once resolved.
    pub egress_interface: Option<String>,
    pub routes: Vec<String>,
    /// Why it is enabled but not active, when that is the case.
    pub detail: Option<String>,
}
