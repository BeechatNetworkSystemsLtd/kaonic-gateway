pub mod config;

#[cfg(feature = "run")]
pub mod vpn;

pub use config::{VpnConfig, VpnGatewayConfig, VpnUplinkConfig};

#[cfg(feature = "run")]
pub use vpn::{
    DenyAll, PeerAuthority, VpnGatewaySnapshot, VpnPeerSnapshot, VpnRouteSnapshot, VpnRuntime,
    VpnRuntimeError, VpnSnapshot, VpnUplinkSnapshot,
};
