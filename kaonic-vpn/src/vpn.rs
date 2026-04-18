//! VPN facade. Internal modules hold the actual runtime — this file only
//! wires the module tree together and re-exports the public API.

mod codec;
mod links;
mod metrics;
mod peer;
mod platform;
mod router;
mod runtime;
mod tun;
mod types;

pub use runtime::{VpnRuntime, VpnRuntimeError};
pub use types::{VpnPeerSnapshot, VpnRouteMappingSnapshot, VpnRouteSnapshot, VpnSnapshot};
