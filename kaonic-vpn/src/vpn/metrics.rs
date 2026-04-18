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
