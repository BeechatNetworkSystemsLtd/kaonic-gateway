//! Lock-free counters for the VPN hot path.
//!
//! `Metrics` is cheap to share (`record_tx` / `record_rx` only touch
//! `AtomicU64`s) and reports a bits-per-second rate that is recomputed
//! at most once per `RATE_WINDOW_SECS` from the delta of the raw counters.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

/// Minimum window between rate recomputations. A snapshot taken sooner
/// just re-reads the last cached value.
const RATE_WINDOW_SECS: u64 = 1;

#[derive(Default)]
pub struct Metrics {
    pub tx_packets: AtomicU64,
    pub tx_bytes: AtomicU64,
    pub rx_packets: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub drop_packets: AtomicU64,
    pub last_tx_ts: AtomicU64,
    pub last_rx_ts: AtomicU64,
    sampler: Mutex<RateSampler>,
}

#[derive(Default)]
struct RateSampler {
    last_ts: u64,
    last_tx_bytes: u64,
    last_rx_bytes: u64,
    tx_bps: u64,
    rx_bps: u64,
}

impl Metrics {
    pub fn record_tx(&self, bytes: usize) {
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
        self.tx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.last_tx_ts.store(now_secs(), Ordering::Relaxed);
    }

    pub fn record_rx(&self, bytes: usize) {
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.rx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.last_rx_ts.store(now_secs(), Ordering::Relaxed);
    }

    pub fn record_drop(&self) {
        self.drop_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let tx_bytes = self.tx_bytes.load(Ordering::Relaxed);
        let rx_bytes = self.rx_bytes.load(Ordering::Relaxed);
        let (tx_bps, rx_bps) = self.refresh_rates(tx_bytes, rx_bytes);
        MetricsSnapshot {
            tx_packets: self.tx_packets.load(Ordering::Relaxed),
            tx_bytes,
            rx_packets: self.rx_packets.load(Ordering::Relaxed),
            rx_bytes,
            drop_packets: self.drop_packets.load(Ordering::Relaxed),
            last_tx_ts: self.last_tx_ts.load(Ordering::Relaxed),
            last_rx_ts: self.last_rx_ts.load(Ordering::Relaxed),
            tx_bps,
            rx_bps,
        }
    }

    /// Lazily refresh the cached bits/sec rates from the raw byte counters.
    /// A call within `RATE_WINDOW_SECS` of the last update is a no-op and
    /// just returns the cached pair.
    fn refresh_rates(&self, tx_bytes: u64, rx_bytes: u64) -> (u64, u64) {
        let now = now_secs();
        let mut s = self.sampler.lock();
        if s.last_ts == 0 {
            s.last_ts = now;
            s.last_tx_bytes = tx_bytes;
            s.last_rx_bytes = rx_bytes;
            return (0, 0);
        }
        let elapsed = now.saturating_sub(s.last_ts);
        if elapsed < RATE_WINDOW_SECS {
            return (s.tx_bps, s.rx_bps);
        }
        let tx_delta = tx_bytes.saturating_sub(s.last_tx_bytes);
        let rx_delta = rx_bytes.saturating_sub(s.last_rx_bytes);
        s.tx_bps = tx_delta.saturating_mul(8) / elapsed;
        s.rx_bps = rx_delta.saturating_mul(8) / elapsed;
        s.last_ts = now;
        s.last_tx_bytes = tx_bytes;
        s.last_rx_bytes = rx_bytes;
        (s.tx_bps, s.rx_bps)
    }
}

#[derive(Clone, Copy, Default)]
pub struct MetricsSnapshot {
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub drop_packets: u64,
    pub last_tx_ts: u64,
    pub last_rx_ts: u64,
    pub tx_bps: u64,
    pub rx_bps: u64,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}


// ── Recent packets ───────────────────────────────────────────────────────────

use std::collections::VecDeque;

use super::types::VpnPacketSnapshot;

/// How many packet summaries are kept. Enough to see a handshake or a ping
/// exchange; small enough that the whole thing rides along in every status
/// frame without being noticed.
pub const RECENT_CAPACITY: usize = 24;

/// A ring of recent packet summaries, for the live view on the VPN page.
#[derive(Default)]
pub struct RecentPackets {
    inner: parking_lot::Mutex<VecDeque<VpnPacketSnapshot>>,
}

impl RecentPackets {
    /// Summarises one packet. Parsing is a couple of header reads on a buffer
    /// already in cache, so this sits in the data path without measurable cost.
    pub fn record(&self, dir: &'static str, packet: &[u8]) {
        let Some(entry) = summarize(dir, packet) else {
            return;
        };
        let mut inner = self.inner.lock();
        if inner.len() >= RECENT_CAPACITY {
            inner.pop_back();
        }
        inner.push_front(entry);
    }

    pub fn snapshot(&self) -> Vec<VpnPacketSnapshot> {
        self.inner.lock().iter().cloned().collect()
    }
}

fn summarize(dir: &'static str, packet: &[u8]) -> Option<VpnPacketSnapshot> {
    use etherparse::IpSlice;
    let slice = IpSlice::from_slice(packet).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let number = u8::from(slice.payload_ip_number());
    let proto = match number {
        1 => "ICMP".to_string(),
        6 => "TCP".to_string(),
        17 => "UDP".to_string(),
        47 => "GRE".to_string(),
        other => other.to_string(),
    };
    // TCP and UDP both open with source then destination port, so the first
    // four bytes of the payload are all that is needed — no transport parser,
    // and nothing beyond the header is ever looked at.
    let (sport, dport) = match number {
        6 | 17 => {
            let payload = slice.payload().payload;
            if payload.len() >= 4 {
                (
                    Some(u16::from_be_bytes([payload[0], payload[1]])),
                    Some(u16::from_be_bytes([payload[2], payload[3]])),
                )
            } else {
                (None, None)
            }
        }
        _ => (None, None),
    };
    Some(VpnPacketSnapshot {
        ts: now.as_secs(),
        ms: now.subsec_millis(),
        dir: dir.to_string(),
        src: slice.source_addr().to_string(),
        dst: slice.destination_addr().to_string(),
        proto,
        sport,
        dport,
        len: packet.len() as u32,
    })
}


#[cfg(test)]
mod recent_tests {
    use super::*;

    /// A minimal IPv4 ICMP packet, exactly as it comes off a tun device: no
    /// link-layer header, no packet-info prefix, just the IP header onwards.
    fn icmp_packet() -> Vec<u8> {
        let mut p = vec![
            0x45, 0x00, 0x00, 0x1c, // version/IHL, DSCP, total length 28
            0x00, 0x01, 0x00, 0x00, // id, flags/fragment
            0x40, 0x01, 0x00, 0x00, // ttl 64, protocol 1 (ICMP), checksum
            10, 20, 34, 166, // src
            10, 20, 124, 1,  // dst
        ];
        p.extend_from_slice(&[0x08, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01]);
        p
    }

    #[test]
    fn a_tun_packet_is_summarized() {
        let ring = RecentPackets::default();
        ring.record("tx", &icmp_packet());
        let seen = ring.snapshot();
        assert_eq!(seen.len(), 1, "the packet should have been summarised");
        assert_eq!(seen[0].src, "10.20.34.166");
        assert_eq!(seen[0].dst, "10.20.124.1");
        assert_eq!(seen[0].proto, "ICMP");
        assert_eq!(seen[0].dir, "tx");
        assert_eq!(seen[0].sport, None, "ICMP has no ports");
    }

    /// A TCP packet, to check the ports come off the payload correctly.
    fn tcp_packet() -> Vec<u8> {
        let mut p = vec![
            0x45, 0x00, 0x00, 0x28,
            0x00, 0x01, 0x00, 0x00,
            0x40, 0x06, 0x00, 0x00, // protocol 6 = TCP
            10, 20, 34, 166,
            10, 20, 124, 1,
        ];
        // source port 51000, destination port 8087 (a TAK CoT port)
        p.extend_from_slice(&[0xc7, 0x38, 0x1f, 0x97]);
        p.extend_from_slice(&[0u8; 16]);
        p
    }

    #[test]
    fn tcp_ports_are_read_from_the_payload() {
        let ring = RecentPackets::default();
        ring.record("tx", &tcp_packet());
        let seen = ring.snapshot();
        assert_eq!(seen[0].proto, "TCP");
        assert_eq!(seen[0].sport, Some(51000));
        assert_eq!(seen[0].dport, Some(8087));
    }

    #[test]
    fn the_ring_keeps_only_the_newest() {
        let ring = RecentPackets::default();
        for _ in 0..(RECENT_CAPACITY + 10) {
            ring.record("rx", &icmp_packet());
        }
        assert_eq!(ring.snapshot().len(), RECENT_CAPACITY);
    }
}
