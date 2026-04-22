//! Public snapshot DTOs returned by `VpnRuntime::snapshot`.
//!
//! Shape is stable for the gateway HTTP API + Leptos VPN page. Keep field
//! names/types unchanged when refactoring internal state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnPeerSnapshot {
    pub destination: String,
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
    pub last_error: Option<String>,
}
