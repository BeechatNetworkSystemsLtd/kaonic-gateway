//! Peer registry.
//!
//! Peers are keyed by `AddressHash` (16-byte, Copy, Hash). Each Peer holds
//! mutable bookkeeping fields behind atomics + short-lived locks; the hot
//! TX/RX path never needs to lock the whole registry while doing I/O.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    /// Wall-clock seconds when the current out-link request attempt started.
    /// Zero when no attempt is in flight (i.e. link is Active or absent).
    /// The watchdog uses this to detect links that never activate — reticulum
    /// keeps re-sending the link request forever on its own, so without a
    /// timeout on our side a lost proof reply would wedge the peer permanently.
    pub link_attempt_ts: AtomicU64,
    /// Guards the actual transport.link() call so announce-driven opens and
    /// watchdog recovery do not overlap.
    pub link_request_in_flight: AtomicBool,
    /// Earliest wall-clock second when a new out-link request may start.
    pub reconnect_cooldown_until_ts: AtomicU64,
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
            link_attempt_ts: AtomicU64::new(0),
            link_request_in_flight: AtomicBool::new(false),
            reconnect_cooldown_until_ts: AtomicU64::new(0),
            last_error: RwLock::new(None),
            metrics: Metrics::default(),
        })
    }

    /// Stamp the start time of a fresh link-request attempt. Unconditional:
    /// a forced teardown + re-request should reset the clock so the watchdog
    /// times the *new* attempt, not the wedged old one.
    pub fn mark_link_attempt(&self) {
        self.link_attempt_ts.store(now_secs(), Ordering::Relaxed);
    }

    pub fn clear_link_attempt(&self) {
        self.link_attempt_ts.store(0, Ordering::Relaxed);
    }

    pub fn try_begin_link_request(&self) -> bool {
        self.link_request_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn finish_link_request(&self) {
        self.link_request_in_flight.store(false, Ordering::Release);
    }

    #[cfg(test)]
    pub fn link_request_in_flight(&self) -> bool {
        self.link_request_in_flight.load(Ordering::Acquire)
    }

    pub fn set_reconnect_cooldown(&self, delay_secs: u64) {
        self.reconnect_cooldown_until_ts
            .store(now_secs().saturating_add(delay_secs), Ordering::Relaxed);
    }

    pub fn clear_reconnect_cooldown(&self) {
        self.reconnect_cooldown_until_ts.store(0, Ordering::Relaxed);
    }

    pub fn reconnect_cooldown_until(&self) -> u64 {
        self.reconnect_cooldown_until_ts.load(Ordering::Relaxed)
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

    pub fn remove(&self, hash: &AddressHash) {
        self.peers.write().remove(hash);
    }
}

impl Default for PeerRegistry {
    fn default() -> Self {
        Self::new()
    }
}
