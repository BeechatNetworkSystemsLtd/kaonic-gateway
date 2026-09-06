//! Trust store: which remote identities may control this node.
//!
//! Zero-trust rule: a link may execute commands only if the initiator has
//! identified itself (Reticulum `LinkIdentify`, signature bound to the link
//! id) *and* that identity is in the paired set persisted through
//! [`TrustStore`]. Pairing itself always requires an operator decision on the
//! target node, verified out-of-band with a short authentication string.

use std::collections::HashMap;

use parking_lot::Mutex;
use reticulum::hash::AddressHash;
use reticulum::identity::Identity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::protocol::perm;

/// A remote node this node trusts (and that trusts this node back).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairedNode {
    /// Hex identity hash (16 bytes) — the stable node key.
    pub identity_hash: String,
    /// Hex public + verifying key, so the node can be addressed without an announce.
    pub identity_hex: String,
    pub codename: String,
    pub paired_at: u64,
    /// Bitmask of [`crate::protocol::perm`] this node grants the remote.
    pub permissions: u32,
}

impl PairedNode {
    pub fn identity(&self) -> Option<Identity> {
        Identity::new_from_hex_string(&self.identity_hex).ok()
    }

    pub fn address_hash(&self) -> Option<AddressHash> {
        AddressHash::new_from_hex_string(&self.identity_hash).ok()
    }

    pub fn allows(&self, required: u32) -> bool {
        self.permissions & required == required
    }
}

#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PairingDirection {
    /// The remote asked us; awaiting local approval.
    Incoming,
    /// We asked the remote; awaiting its operator.
    Outgoing,
}

impl PairingDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            PairingDirection::Incoming => "incoming",
            PairingDirection::Outgoing => "outgoing",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "incoming" => Some(PairingDirection::Incoming),
            "outgoing" => Some(PairingDirection::Outgoing),
            _ => None,
        }
    }
}

/// A pairing in flight, persisted so a restart neither forgets a request
/// the operator still has to approve nor one we are waiting on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingRecord {
    pub identity_hash: String,
    pub identity_hex: String,
    pub codename: String,
    pub direction: PairingDirection,
    /// [`crate::types::PairingState`] name (outgoing only; incoming is always pending).
    pub state: String,
    pub detail: String,
    pub ts: u64,
    pub retries: u32,
}

impl PairingRecord {
    pub fn identity(&self) -> Option<Identity> {
        Identity::new_from_hex_string(&self.identity_hex).ok()
    }
}

/// Persistence for trust state. Implemented by the host (SQLite in the gateway).
pub trait TrustStore: Send + Sync {
    fn load(&self) -> Vec<PairedNode>;
    fn save(&self, node: &PairedNode) -> Result<(), String>;
    fn remove(&self, identity_hash: &str) -> Result<(), String>;

    fn load_requests(&self) -> Vec<PairingRecord>;
    fn save_request(&self, record: &PairingRecord) -> Result<(), String>;
    fn remove_request(
        &self,
        identity_hash: &str,
        direction: PairingDirection,
    ) -> Result<(), String>;
}

/// In-memory store for tests and hosts without persistence.
#[derive(Default)]
pub struct MemoryTrustStore {
    nodes: Mutex<HashMap<String, PairedNode>>,
    requests: Mutex<HashMap<(String, PairingDirection), PairingRecord>>,
}

impl TrustStore for MemoryTrustStore {
    fn load(&self) -> Vec<PairedNode> {
        self.nodes.lock().values().cloned().collect()
    }

    fn save(&self, node: &PairedNode) -> Result<(), String> {
        self.nodes
            .lock()
            .insert(node.identity_hash.clone(), node.clone());
        Ok(())
    }

    fn remove(&self, identity_hash: &str) -> Result<(), String> {
        self.nodes.lock().remove(identity_hash);
        Ok(())
    }

    fn load_requests(&self) -> Vec<PairingRecord> {
        self.requests.lock().values().cloned().collect()
    }

    fn save_request(&self, record: &PairingRecord) -> Result<(), String> {
        self.requests.lock().insert(
            (record.identity_hash.clone(), record.direction),
            record.clone(),
        );
        Ok(())
    }

    fn remove_request(
        &self,
        identity_hash: &str,
        direction: PairingDirection,
    ) -> Result<(), String> {
        self.requests
            .lock()
            .remove(&(identity_hash.to_string(), direction));
        Ok(())
    }
}

/// Short authentication string shown on both nodes during pairing. It is a
/// function of both identity hashes (order independent) so an attacker who
/// merely copies a codename cannot produce a matching code.
pub fn sas_code(a: &AddressHash, b: &AddressHash) -> String {
    let (lo, hi) = if a.as_slice() <= b.as_slice() {
        (a, b)
    } else {
        (b, a)
    };
    let digest = Sha256::new()
        .chain_update(b"kaonic-remote-sas")
        .chain_update(lo.as_slice())
        .chain_update(hi.as_slice())
        .finalize();
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000;
    format!("{:03} {:03}", value / 1000, value % 1000)
}

pub fn default_permissions() -> u32 {
    perm::ALL
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn sas_is_symmetric_and_six_digits() {
        let a = AddressHash::new_from_rand(OsRng);
        let b = AddressHash::new_from_rand(OsRng);
        let code = sas_code(&a, &b);
        assert_eq!(code, sas_code(&b, &a));
        assert_eq!(code.len(), 7);
        assert_ne!(code, sas_code(&a, &AddressHash::new_from_rand(OsRng)));
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryTrustStore::default();
        let node = PairedNode {
            identity_hash: "aa".into(),
            identity_hex: String::new(),
            codename: "x".into(),
            paired_at: 1,
            permissions: perm::ALL,
        };
        store.save(&node).unwrap();
        assert_eq!(store.load(), vec![node.clone()]);
        store.remove("aa").unwrap();
        assert!(store.load().is_empty());
        assert!(node.allows(perm::RADIO));
    }
}
