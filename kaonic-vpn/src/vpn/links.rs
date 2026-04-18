//! Per-peer link cache.
//!
//! Caches `Arc<Mutex<Link>>` keyed by peer `AddressHash`. Avoids the O(N)
//! linear scan inside `Transport::send_to_out_links` for every packet.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;
use reticulum::destination::link::Link;
use reticulum::hash::AddressHash;
use tokio::sync::Mutex;

pub type LinkHandle = Arc<Mutex<Link>>;

pub struct LinkRegistry {
    links: RwLock<HashMap<AddressHash, LinkHandle>>,
}

impl LinkRegistry {
    pub fn new() -> Self {
        Self {
            links: RwLock::new(HashMap::new()),
        }
    }

    pub fn get(&self, hash: &AddressHash) -> Option<LinkHandle> {
        self.links.read().get(hash).cloned()
    }

    pub fn insert(&self, hash: AddressHash, link: LinkHandle) {
        self.links.write().insert(hash, link);
    }

    pub fn remove(&self, hash: &AddressHash) {
        self.links.write().remove(hash);
    }
}

impl Default for LinkRegistry {
    fn default() -> Self {
        Self::new()
    }
}
