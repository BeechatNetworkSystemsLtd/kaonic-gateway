//! Lock-free IP -> peer routing.
//!
//! The hot path reads a snapshot `Arc<RouteTable>` under one very short
//! `RwLock` read (we clone the Arc and drop the lock immediately), then does
//! a lookup against the snapshot without holding any lock.
//!
//! Updates build a fresh `RouteTable` and swap it in under a write lock. Route
//! changes happen on announce receipt — rare compared to TX packet rate.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use cidr::Ipv4Cidr;
use parking_lot::RwLock;
use reticulum::hash::AddressHash;

use super::peer::Peer;

#[derive(Default)]
pub struct RouteTable {
    /// Direct peer tunnel IPs — highest priority match.
    tunnel_ips: HashMap<Ipv4Addr, AddressHash>,
    /// Advertised peer subnets, sorted by prefix length DESC (longest match first).
    subnets: Vec<(Ipv4Cidr, AddressHash)>,
}

impl RouteTable {
    pub fn build<I>(peers: I, now: u64, local_hash: AddressHash, local_tunnel_ip: Ipv4Addr) -> Self
    where
        I: IntoIterator<Item = Arc<Peer>>,
    {
        let mut tunnel_ips = HashMap::new();
        let mut claimed: HashMap<Ipv4Cidr, AddressHash> = HashMap::new();
        let mut conflicts: std::collections::HashSet<Ipv4Cidr> = Default::default();

        for peer in peers {
            if peer.hash == local_hash {
                continue;
            }
            tunnel_ips.insert(peer.tunnel_ip, peer.hash);

            let expires = peer
                .route_expires_ts
                .load(std::sync::atomic::Ordering::Relaxed);
            if expires != 0 && expires <= now {
                continue;
            }

            for route in peer.routes_clone() {
                // Never let peers own a subnet that contains our own tunnel IP.
                if route.contains(&local_tunnel_ip) {
                    conflicts.insert(route);
                    continue;
                }
                match claimed.get(&route) {
                    Some(existing) if *existing != peer.hash => {
                        conflicts.insert(route);
                    }
                    Some(_) => {}
                    None => {
                        claimed.insert(route, peer.hash);
                    }
                }
            }
        }

        let mut subnets: Vec<_> = claimed
            .into_iter()
            .filter(|(route, _)| !conflicts.contains(route))
            .collect();
        subnets.sort_by_key(|(r, _)| std::cmp::Reverse(r.network_length()));

        Self {
            tunnel_ips,
            subnets,
        }
    }

    pub fn resolve(&self, ip: Ipv4Addr) -> Option<AddressHash> {
        if let Some(hash) = self.tunnel_ips.get(&ip) {
            return Some(*hash);
        }
        for (subnet, owner) in &self.subnets {
            if subnet.contains(&ip) {
                return Some(*owner);
            }
        }
        None
    }

    pub fn subnets(&self) -> &[(Ipv4Cidr, AddressHash)] {
        &self.subnets
    }
}

pub struct Router {
    table: RwLock<Arc<RouteTable>>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            table: RwLock::new(Arc::new(RouteTable::default())),
        }
    }

    pub fn snapshot(&self) -> Arc<RouteTable> {
        self.table.read().clone()
    }

    pub fn swap(&self, next: RouteTable) {
        *self.table.write() = Arc::new(next);
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vpn::peer::{LinkState, Peer};

    fn hash(hex: &str) -> AddressHash {
        AddressHash::new_from_hex_string(hex).unwrap()
    }

    #[test]
    fn longest_prefix_wins() {
        let a = hash("11111111111111111111111111111111");
        let b = hash("22222222222222222222222222222222");
        let peer_a = Peer::new(a, "10.20.0.10".parse().unwrap(), LinkState::Active);
        peer_a.set_routes(vec!["172.16.0.0/16".parse().unwrap()]);
        let peer_b = Peer::new(b, "10.20.0.11".parse().unwrap(), LinkState::Active);
        peer_b.set_routes(vec!["172.16.5.0/24".parse().unwrap()]);

        let table = RouteTable::build(
            [peer_a.clone(), peer_b.clone()],
            0,
            hash("00000000000000000000000000000000"),
            "192.168.200.1".parse().unwrap(),
        );
        assert_eq!(table.resolve("172.16.5.42".parse().unwrap()), Some(b));
        assert_eq!(table.resolve("172.16.9.42".parse().unwrap()), Some(a));
    }

    #[test]
    fn conflicting_routes_are_dropped() {
        let a = hash("11111111111111111111111111111111");
        let b = hash("22222222222222222222222222222222");
        let peer_a = Peer::new(a, "10.20.0.10".parse().unwrap(), LinkState::Active);
        peer_a.set_routes(vec!["192.168.5.0/24".parse().unwrap()]);
        let peer_b = Peer::new(b, "10.20.0.11".parse().unwrap(), LinkState::Active);
        peer_b.set_routes(vec!["192.168.5.0/24".parse().unwrap()]);

        let table = RouteTable::build(
            [peer_a, peer_b],
            0,
            hash("00000000000000000000000000000000"),
            "10.20.0.1".parse().unwrap(),
        );
        assert_eq!(table.resolve("192.168.5.1".parse().unwrap()), None);
    }

    #[test]
    fn tunnel_ip_takes_precedence() {
        let a = hash("11111111111111111111111111111111");
        let peer = Peer::new(a, "10.20.0.10".parse().unwrap(), LinkState::Active);
        peer.set_routes(vec!["10.20.0.0/16".parse().unwrap()]);
        let table = RouteTable::build(
            [peer],
            0,
            hash("00000000000000000000000000000000"),
            "10.20.0.1".parse().unwrap(),
        );
        // Tunnel IP lookup hits directly; subnet that would swallow the whole
        // /16 is dropped because it overlaps our own tunnel IP.
        assert_eq!(table.resolve("10.20.0.10".parse().unwrap()), Some(a));
        assert_eq!(table.resolve("10.20.99.99".parse().unwrap()), None);
    }
}
