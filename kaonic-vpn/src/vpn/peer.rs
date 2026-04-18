//! Peer registry.
//!
//! Peers are keyed by `AddressHash` (16-byte, Copy, Hash). Each Peer holds
//! mutable bookkeeping fields behind atomics + short-lived locks; the hot
//! TX/RX path never needs to lock the whole registry while doing I/O.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use cidr::Ipv4Cidr;
use parking_lot::RwLock;
use reticulum::destination::DestinationDesc;
use reticulum::hash::AddressHash;

use super::metrics::{now_secs, Metrics};

/// Peer link state. Stored as a small enum so the hot path doesn't touch strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Configured,
    Discovered,
    Pending,
    Active,
    Closed,
    Error,
}

impl LinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Discovered => "discovered",
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Closed => "closed",
            Self::Error => "error",
        }
    }
}

pub struct Peer {
    pub hash: AddressHash,
    pub tunnel_ip: Ipv4Addr,
    pub desc: RwLock<Option<DestinationDesc>>,
    pub link_state: RwLock<LinkState>,
    pub routes: RwLock<Vec<Ipv4Cidr>>,
    pub last_seen_ts: AtomicU64,
    pub route_expires_ts: AtomicU64,
    pub last_tx_ts: AtomicU64,
    pub reconnect_attempts: AtomicU32,
    pub last_error: RwLock<Option<String>>,
    /// Per-peer traffic counters + cached tx_bps/rx_bps.
    pub metrics: Metrics,
}

impl Peer {
    pub fn new(hash: AddressHash, tunnel_ip: Ipv4Addr, state: LinkState) -> Arc<Self> {
        Arc::new(Self {
            hash,
            tunnel_ip,
            desc: RwLock::new(None),
            link_state: RwLock::new(state),
            routes: RwLock::new(Vec::new()),
            last_seen_ts: AtomicU64::new(0),
            route_expires_ts: AtomicU64::new(0),
            last_tx_ts: AtomicU64::new(0),
            reconnect_attempts: AtomicU32::new(0),
            last_error: RwLock::new(None),
            metrics: Metrics::default(),
        })
    }

    pub fn set_state(&self, state: LinkState) {
        *self.link_state.write() = state;
    }

    pub fn state(&self) -> LinkState {
        *self.link_state.read()
    }

    pub fn mark_seen(&self) {
        self.last_seen_ts.store(now_secs(), Ordering::Relaxed);
    }

    pub fn mark_tx(&self) {
        self.last_tx_ts.store(now_secs(), Ordering::Relaxed);
    }

    pub fn set_error(&self, msg: impl Into<String>) {
        *self.last_error.write() = Some(msg.into());
    }

    pub fn clear_error(&self) {
        *self.last_error.write() = None;
    }

    pub fn routes_clone(&self) -> Vec<Ipv4Cidr> {
        self.routes.read().clone()
    }

    pub fn set_routes(&self, routes: Vec<Ipv4Cidr>) {
        *self.routes.write() = routes;
    }

    pub fn set_desc(&self, desc: DestinationDesc) {
        *self.desc.write() = Some(desc);
    }

    pub fn desc(&self) -> Option<DestinationDesc> {
        *self.desc.read()
    }
}

pub struct PeerRegistry {
    peers: RwLock<HashMap<AddressHash, Arc<Peer>>>,
}

impl PeerRegistry {
    pub fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, peer: Arc<Peer>) {
        self.peers.write().insert(peer.hash, peer);
    }

    pub fn get(&self, hash: &AddressHash) -> Option<Arc<Peer>> {
        self.peers.read().get(hash).cloned()
    }

    pub fn get_or_create<F>(&self, hash: AddressHash, make: F) -> Arc<Peer>
    where
        F: FnOnce() -> Arc<Peer>,
    {
        if let Some(existing) = self.peers.read().get(&hash).cloned() {
            return existing;
        }
        let mut guard = self.peers.write();
        guard.entry(hash).or_insert_with(make).clone()
    }

    pub fn all(&self) -> Vec<Arc<Peer>> {
        self.peers.read().values().cloned().collect()
    }
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
