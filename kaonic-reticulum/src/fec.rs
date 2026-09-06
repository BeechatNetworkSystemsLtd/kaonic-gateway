//! Runtime FEC selection for the radio interface.
//!
//! Nothing here is persisted: each sender picks a [`TrafficClass`] for a
//! destination (or link id) at runtime and the selector turns that plus the
//! last RSSI heard from that destination into a [`FecCode`] per frame. The
//! default is the wire-compatible TM2048, so a node that never sets a class
//! behaves exactly like older firmware.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub use kaonic_net::coder::{CoderStats, FecCode};
use reticulum::hash::AddressHash;
use reticulum::packet::Packet;

/// RSSI samples older than this no longer influence code choice.
const RSSI_TTL: Duration = Duration::from_secs(300);
/// Hysteresis around the RSSI thresholds so a fluttering link does not
/// flip codes every frame.
const HYSTERESIS_DB: i16 = 4;
const STRONG_DBM: i16 = -60;
const GOOD_DBM: i16 = -75;

/// What the sender cares about for a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrafficClass {
    /// Maximum correction, wire-compatible with every node (TM2048).
    #[default]
    Robust,
    /// Pick the cheapest code the measured link supports.
    Auto,
    /// Rate 4/5: minimum airtime and decode cost; only for strong links.
    Fast,
    /// Uncoded payload (CRC only): lab / very short range.
    Fastest,
    /// A specific code, chosen by the caller.
    Fixed(FecCode),
}

#[derive(Default)]
struct Inner {
    default: TrafficClass,
    per_destination: HashMap<AddressHash, TrafficClass>,
    rssi: HashMap<AddressHash, (i8, Instant)>,
    chosen: HashMap<AddressHash, FecCode>,
}

/// Shared between the radio interface (which asks per frame) and the
/// applications (which set classes and are told link quality).
pub struct FecSelector {
    inner: Mutex<Inner>,
    verified_fast: AtomicU64,
    decoded_full: AtomicU64,
    failed: AtomicU64,
    header_failed: AtomicU64,
}

impl Default for FecSelector {
    fn default() -> Self {
        Self::new(TrafficClass::Robust)
    }
}

impl FecSelector {
    pub fn new(default: TrafficClass) -> Self {
        Self {
            inner: Mutex::new(Inner {
                default,
                ..Inner::default()
            }),
            verified_fast: AtomicU64::new(0),
            decoded_full: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            header_failed: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_default(&self, class: TrafficClass) {
        self.lock().default = class;
    }

    pub fn default_class(&self) -> TrafficClass {
        self.lock().default
    }

    /// Class for frames addressed to `destination` (a destination hash or a
    /// link id — whatever ends up in the packet's destination field).
    pub fn set_class(&self, destination: AddressHash, class: TrafficClass) {
        self.lock().per_destination.insert(destination, class);
    }

    pub fn clear_class(&self, destination: &AddressHash) {
        let mut inner = self.lock();
        inner.per_destination.remove(destination);
        inner.chosen.remove(destination);
    }

    /// Record the RSSI of a frame that carried a packet for `destination`.
    /// Link ids are the same in both directions, so this also measures the
    /// path we transmit on.
    pub fn observe_rssi(&self, destination: AddressHash, rssi: i8) {
        self.lock().rssi.insert(destination, (rssi, Instant::now()));
    }

    pub fn last_rssi(&self, destination: &AddressHash) -> Option<i8> {
        self.lock()
            .rssi
            .get(destination)
            .filter(|(_, at)| at.elapsed() < RSSI_TTL)
            .map(|(rssi, _)| *rssi)
    }

    /// Code for the next frame carrying `packet`.
    pub fn select(&self, packet: &Packet) -> FecCode {
        let mut inner = self.lock();
        let dest = packet.destination;
        let class = inner
            .per_destination
            .get(&dest)
            .copied()
            .unwrap_or(inner.default);
        match class {
            TrafficClass::Robust => FecCode::Tm2048,
            TrafficClass::Fast => FecCode::Tm1280,
            TrafficClass::Fastest => FecCode::None,
            TrafficClass::Fixed(code) => code,
            TrafficClass::Auto => {
                let rssi = inner
                    .rssi
                    .get(&dest)
                    .filter(|(_, at)| at.elapsed() < RSSI_TTL)
                    .map(|(rssi, _)| i16::from(*rssi));
                let previous = inner.chosen.get(&dest).copied();
                let code = auto_code(rssi, previous);
                inner.chosen.insert(dest, code);
                code
            }
        }
    }

    pub(crate) fn record_stats(&self, stats: CoderStats) {
        self.verified_fast
            .store(stats.verified_fast, Ordering::Relaxed);
        self.decoded_full
            .store(stats.decoded_full, Ordering::Relaxed);
        self.failed.store(stats.failed, Ordering::Relaxed);
        self.header_failed
            .store(stats.header_failed, Ordering::Relaxed);
    }

    /// Receive-side decoder counters for this interface.
    pub fn stats(&self) -> CoderStats {
        CoderStats {
            verified_fast: self.verified_fast.load(Ordering::Relaxed),
            decoded_full: self.decoded_full.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            header_failed: self.header_failed.load(Ordering::Relaxed),
        }
    }
}

/// Threshold decision with hysteresis: a stronger code is only left once
/// the signal is clearly above the threshold, and only re-entered once it
/// is clearly below.
fn auto_code(rssi: Option<i16>, previous: Option<FecCode>) -> FecCode {
    let Some(rssi) = rssi else {
        return FecCode::Tm2048;
    };
    let up = |threshold: i16| rssi >= threshold + HYSTERESIS_DB;
    let down = |threshold: i16| rssi < threshold - HYSTERESIS_DB;
    match previous {
        Some(FecCode::Tm1280) => {
            if down(STRONG_DBM) {
                if down(GOOD_DBM) {
                    FecCode::Tm2048
                } else {
                    FecCode::Tm1536
                }
            } else {
                FecCode::Tm1280
            }
        }
        Some(FecCode::Tm1536) => {
            if up(STRONG_DBM) {
                FecCode::Tm1280
            } else if down(GOOD_DBM) {
                FecCode::Tm2048
            } else {
                FecCode::Tm1536
            }
        }
        _ => {
            if up(STRONG_DBM) {
                FecCode::Tm1280
            } else if up(GOOD_DBM) {
                FecCode::Tm1536
            } else {
                FecCode::Tm2048
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_uses_hysteresis() {
        assert_eq!(auto_code(None, None), FecCode::Tm2048);
        assert_eq!(auto_code(Some(-50), None), FecCode::Tm1280);
        assert_eq!(auto_code(Some(-70), None), FecCode::Tm1536);
        assert_eq!(auto_code(Some(-90), None), FecCode::Tm2048);
        // Slightly below the strong threshold keeps the fast code...
        assert_eq!(auto_code(Some(-62), Some(FecCode::Tm1280)), FecCode::Tm1280);
        // ...until it is clearly below.
        assert_eq!(auto_code(Some(-65), Some(FecCode::Tm1280)), FecCode::Tm1536);
        assert_eq!(auto_code(Some(-90), Some(FecCode::Tm1280)), FecCode::Tm2048);
        // Slightly above the strong threshold does not promote from Tm1536.
        assert_eq!(auto_code(Some(-58), Some(FecCode::Tm1536)), FecCode::Tm1536);
        assert_eq!(auto_code(Some(-55), Some(FecCode::Tm1536)), FecCode::Tm1280);
    }

    #[test]
    fn classes_map_to_codes_and_default_is_compatible() {
        let selector = FecSelector::default();
        let packet = Packet::default();
        assert_eq!(selector.select(&packet), FecCode::Tm2048);
        selector.set_class(packet.destination, TrafficClass::Fast);
        assert_eq!(selector.select(&packet), FecCode::Tm1280);
        selector.set_class(packet.destination, TrafficClass::Auto);
        assert_eq!(selector.select(&packet), FecCode::Tm2048);
        selector.observe_rssi(packet.destination, -45);
        assert_eq!(selector.select(&packet), FecCode::Tm1280);
        selector.clear_class(&packet.destination);
        assert_eq!(selector.select(&packet), FecCode::Tm2048);
    }
}
