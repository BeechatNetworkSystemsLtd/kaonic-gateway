//! Kaonic Remote — zero-trust remote control of Kaonic nodes over Reticulum.
//!
//! * [`protocol`] — compact wire format tuned for single-frame radio packets.
//! * [`trust`] — paired identities and the operator-verified pairing model.
//! * [`nodes`] — registry of announced nodes with hop/RSSI reachability.
//! * [`transfer`] — resumable chunked blob transfer (plugin packages).
//! * [`handler`] — host-side command execution trait.
//! * [`runtime`] — the announce/link/session engine in both roles.

pub mod erasure;
pub mod handler;
pub mod media;
pub mod nodes;
pub mod protocol;
pub mod runtime;
pub mod transfer;
pub mod trust;
pub mod types;

pub use handler::{
    Command, CommandHandler, LinkClass, LinkPolicy, NoopHandler, RemoteError, Reply,
};
pub use nodes::sibling_destination;
pub use runtime::{LocalInfo, RemoteConfig, RemoteRuntime};
pub use trust::{MemoryTrustStore, PairedNode, PairingDirection, PairingRecord, TrustStore};
pub use types::*;
