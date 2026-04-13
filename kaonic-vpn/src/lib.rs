pub mod config;

#[cfg(feature = "run")]
pub mod vpn;

pub use config::VpnConfig;

#[cfg(feature = "run")]
pub use vpn::{VpnPeerSnapshot, VpnRouteSnapshot, VpnRuntime, VpnRuntimeError, VpnSnapshot};
