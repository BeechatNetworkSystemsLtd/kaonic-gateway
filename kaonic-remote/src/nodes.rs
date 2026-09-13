//! Registry of every `kaonic.remote` node observed on the network.
//!
//! Nodes are keyed by identity hash. Reachability metrics come from the raw
//! radio path (hop count from the announce header, RSSI from the frame that
//! carried it) so the map can place a node by distance and signal.

use std::collections::{HashMap, HashSet};

use parking_lot::Mutex;
use reticulum::destination::{DestinationDesc, DestinationName, SingleOutputDestination};
use reticulum::hash::AddressHash;
use reticulum::identity::Identity;

use crate::protocol::{AnnounceInfo, APP_NAME, ASPECT};

/// Reachability samples older than this are dropped from the snapshot so a
/// node that went quiet does not keep a stale RSSI forever.
const METRIC_TTL_SECS: u64 = 15 * 60;
/// Upper bound on tracked nodes. Announces are unauthenticated, so an
/// unbounded registry is a memory and CPU sink; the oldest unpaired,
/// offline entry is evicted once the map is full.
const MAX_NODES: usize = 256;

#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub identity: Identity,
    pub identity_hash: AddressHash,
    pub destination: AddressHash,
    pub codename: String,
    pub gateway_version: (u8, u8, u8),
    pub protocol: u8,
    pub flags: u8,
    pub announce_secs: u8,
    /// Last advertised plugin service directory digest (0 = none).
    pub services_digest: u32,
    pub last_announce_ts: u64,
    pub hops: Option<u8>,
    pub rssi: Option<i8>,
    pub metric_ts: u64,
}

impl NodeEntry {
    /// A node is "online" while announces keep arriving within a small
    /// multiple of the period it advertises (2.5 periods + 5 s grace).
    pub fn online(&self, now: u64) -> bool {
        if self.last_announce_ts == 0 {
            return false;
        }
        let period = self.announce_secs.max(5) as u64;
        now.saturating_sub(self.last_announce_ts) <= period * 5 / 2 + 5
    }

    pub fn desc(&self) -> DestinationDesc {
        DestinationDesc {
            identity: self.identity,
            address_hash: self.destination,
            name: remote_destination_name(),
        }
    }
}

/// Announced codenames are unauthenticated text; keep them printable and
/// short before they reach logs or the UI.
fn sanitize_codename(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(16)
        .collect();
    if cleaned.is_empty() {
        "unknown".into()
    } else {
        cleaned
    }
}

pub fn remote_destination_name() -> DestinationName {
    DestinationName::new(APP_NAME, ASPECT)
}

/// Address of another aspect of the same identity (e.g. `kaonic.vpn`), so a
/// host can correlate a remote node with its VPN peer entry.
pub fn sibling_destination(identity: Identity, app: &str, aspect: &str) -> AddressHash {
    SingleOutputDestination::new(identity, DestinationName::new(app, aspect))
        .desc
        .address_hash
}

/// Destination descriptor for a remote identity's `kaonic.remote` destination.
/// Lets a paired node be addressed even before its first announce is heard.
pub fn remote_destination_for(identity: Identity) -> DestinationDesc {
    SingleOutputDestination::new(identity, remote_destination_name()).desc
}

#[derive(Default)]
pub struct NodeRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    nodes: HashMap<AddressHash, NodeEntry>,
    /// destination hash → identity hash (announce/link events carry the former).
    by_destination: HashMap<AddressHash, AddressHash>,
    /// Identities that must never be evicted (paired or seeded by the host).
    pinned: HashSet<AddressHash>,
}

impl Inner {
    /// Make room for a new entry by dropping the least recently heard node
    /// that is neither pinned nor currently online.
    fn evict_if_full(&mut self, now: u64) {
        while self.nodes.len() >= MAX_NODES {
            let victim = self
                .nodes
                .values()
                .filter(|entry| !self.pinned.contains(&entry.identity_hash) && !entry.online(now))
                .min_by_key(|entry| entry.last_announce_ts)
                .map(|entry| (entry.identity_hash, entry.destination));
            let Some((identity, destination)) = victim else {
                return;
            };
            self.nodes.remove(&identity);
            self.by_destination.remove(&destination);
        }
    }
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an announce. Returns true when this is a newly discovered node.
    pub fn observe_announce(&self, desc: &DestinationDesc, info: &AnnounceInfo, now: u64) -> bool {
        let identity_hash = desc.identity.address_hash;
        let mut inner = self.inner.lock();
        inner
            .by_destination
            .insert(desc.address_hash, identity_hash);
        match inner.nodes.get_mut(&identity_hash) {
            Some(entry) => {
                entry.identity = desc.identity;
                entry.destination = desc.address_hash;
                entry.codename = sanitize_codename(&info.codename);
                entry.gateway_version = info.gateway_version;
                entry.protocol = info.protocol;
                entry.flags = info.flags;
                entry.announce_secs = info.announce_secs;
                entry.services_digest = info.services_digest;
                entry.last_announce_ts = now;
                false
            }
            None => {
                inner.evict_if_full(now);
                inner.nodes.insert(
                    identity_hash,
                    NodeEntry {
                        identity: desc.identity,
                        identity_hash,
                        destination: desc.address_hash,
                        codename: sanitize_codename(&info.codename),
                        gateway_version: info.gateway_version,
                        protocol: info.protocol,
                        flags: info.flags,
                        announce_secs: info.announce_secs,
                        services_digest: info.services_digest,
                        last_announce_ts: now,
                        hops: None,
                        rssi: None,
                        metric_ts: 0,
                    },
                );
                true
            }
        }
    }

    /// Seed a node from persisted trust data (no announce heard yet). Seeded
    /// nodes are pinned: they are the ones the operator actually cares about.
    pub fn seed(&self, identity: Identity, codename: &str) {
        let identity_hash = identity.address_hash;
        let desc = remote_destination_for(identity);
        let mut inner = self.inner.lock();
        inner.pinned.insert(identity_hash);
        inner
            .by_destination
            .insert(desc.address_hash, identity_hash);
        inner.nodes.entry(identity_hash).or_insert(NodeEntry {
            identity,
            identity_hash,
            destination: desc.address_hash,
            codename: codename.to_string(),
            gateway_version: (0, 0, 0),
            protocol: 0,
            flags: 0,
            announce_secs: 0,
            services_digest: 0,
            last_announce_ts: 0,
            hops: None,
            rssi: None,
            metric_ts: 0,
        });
    }

    /// Reachability sample for a packet addressed from `destination`.
    pub fn observe_metrics(&self, destination: &AddressHash, hops: u8, rssi: i8, now: u64) {
        let mut inner = self.inner.lock();
        let Some(identity_hash) = inner.by_destination.get(destination).copied() else {
            return;
        };
        if let Some(entry) = inner.nodes.get_mut(&identity_hash) {
            entry.hops = Some(hops);
            entry.rssi = Some(rssi);
            entry.metric_ts = now;
        }
    }

    /// RSSI-only sample (link traffic — hop count unknown from the packet).
    pub fn observe_rssi(&self, identity_hash: &AddressHash, rssi: i8, now: u64) {
        let mut inner = self.inner.lock();
        if let Some(entry) = inner.nodes.get_mut(identity_hash) {
            entry.rssi = Some(rssi);
            entry.metric_ts = now;
        }
    }

    pub fn identity_for_destination(&self, destination: &AddressHash) -> Option<AddressHash> {
        self.inner.lock().by_destination.get(destination).copied()
    }

    pub fn get(&self, identity_hash: &AddressHash) -> Option<NodeEntry> {
        self.inner.lock().nodes.get(identity_hash).cloned()
    }

    pub fn update_codename(&self, identity_hash: &AddressHash, codename: &str) {
        if let Some(entry) = self.inner.lock().nodes.get_mut(identity_hash) {
            entry.codename = codename.to_string();
        }
    }

    pub fn all(&self, now: u64) -> Vec<NodeEntry> {
        self.inner
            .lock()
            .nodes
            .values()
            .map(|entry| {
                let mut entry = entry.clone();
                if now.saturating_sub(entry.metric_ts) > METRIC_TTL_SECS {
                    entry.hops = None;
                    entry.rssi = None;
                }
                entry
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PROTOCOL_VERSION;
    use rand::rngs::OsRng;
    use reticulum::identity::PrivateIdentity;

    fn announce(codename: &str, secs: u8) -> AnnounceInfo {
        AnnounceInfo {
            protocol: PROTOCOL_VERSION,
            flags: 0,
            codename: codename.into(),
            gateway_version: (0, 2, 5),
            announce_secs: secs,
            services_digest: 0,
        }
    }

    #[test]
    fn announce_registers_and_metrics_attach_by_destination() {
        let registry = NodeRegistry::new();
        let id = PrivateIdentity::new_from_rand(OsRng);
        let desc = remote_destination_for(*id.as_identity());

        assert!(registry.observe_announce(&desc, &announce("node0001", 20), 100));
        assert!(!registry.observe_announce(&desc, &announce("node0001", 20), 101));

        registry.observe_metrics(&desc.address_hash, 1, -70, 101);
        let entry = registry.get(&desc.identity.address_hash).unwrap();
        assert_eq!(entry.hops, Some(1));
        assert_eq!(entry.rssi, Some(-70));
        assert!(entry.online(101 + 55));
        assert!(!entry.online(101 + 56));

        // Stale metrics are hidden in the snapshot view but the node remains.
        let all = registry.all(101 + METRIC_TTL_SECS + 1);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].rssi, None);
    }

    #[test]
    fn seeded_node_has_computed_destination() {
        let registry = NodeRegistry::new();
        let id = PrivateIdentity::new_from_rand(OsRng);
        registry.seed(*id.as_identity(), "seednode");
        let entry = registry.get(id.address_hash()).unwrap();
        assert_eq!(
            entry.destination,
            remote_destination_for(*id.as_identity()).address_hash
        );
        assert!(!entry.online(1000));
    }
}
