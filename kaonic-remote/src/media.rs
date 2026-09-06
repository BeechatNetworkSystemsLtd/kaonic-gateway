//! Real-time media transport (PTT voice, video) over a link.
//!
//! No retransmission ever: each application packet becomes one radio frame
//! (a data shard) and is delivered the moment it arrives; every `k` packets
//! — or when `block_timeout` passes with a partial block — the sender emits
//! `m` Reed–Solomon parity shards so the receiver rebuilds up to `m` lost
//! frames per block without a round trip. Blocks are short so the FEC delay
//! stays inside the codec's latency budget; anything older than
//! `latency_budget` on the receiver is dropped, never repaired late.
//!
//! The block header carries `k` per block, so a block closed by timeout is
//! simply coded with fewer data shards — no padding frames on air.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::erasure::{pad_shard, BlockCoder, MAX_BLOCK_SHARDS};
use crate::protocol::{Frame, MediaShard, MAX_LINK_PAYLOAD, MEDIA_HEADER_LEN};

/// Blocks a receiver keeps before evicting the oldest.
const MAX_RX_BLOCKS: usize = 256;
/// Packets coalesced into one shard; keeps the per-packet index in a u8.
const MAX_PACKED_PER_SHARD: u8 = 64;

/// Largest application packet that fits one radio frame.
pub const MAX_MEDIA_PACKET: usize = MAX_LINK_PAYLOAD - MEDIA_HEADER_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaConfig {
    pub stream: u8,
    /// Data shards per block (packets); the last block of a burst may be shorter.
    pub k: u8,
    /// Parity shards per block; 0 disables the outer code.
    pub m: u8,
    /// Close a partial block after this long so parity is not held back.
    pub block_timeout: Duration,
    /// Receiver drops (and stops repairing) blocks older than this.
    pub latency_budget: Duration,
    /// Coalesce application packets arriving within this window into one
    /// radio frame (the radio is frame-rate bound: ~55 frames/s). Zero
    /// sends every packet in its own frame.
    pub pack_window: Duration,
}

impl MediaConfig {
    /// Voice: 4 + 1 with 60 ms blocks — fits 20–40 ms codec packets.
    pub fn voice(stream: u8) -> Self {
        Self {
            stream,
            k: 4,
            m: 1,
            block_timeout: Duration::from_millis(120),
            latency_budget: Duration::from_millis(300),
            pack_window: Duration::from_millis(40),
        }
    }

    /// Video: 8 + 2, larger budget.
    pub fn video(stream: u8) -> Self {
        Self {
            stream,
            k: 8,
            m: 2,
            block_timeout: Duration::from_millis(100),
            latency_budget: Duration::from_millis(400),
            pack_window: Duration::ZERO,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.k == 0 {
            return Err("k must be > 0".into());
        }
        if usize::from(self.k) + usize::from(self.m) > MAX_BLOCK_SHARDS {
            return Err(format!("k + m must be <= {MAX_BLOCK_SHARDS}"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaStats {
    pub packets_sent: u64,
    pub parity_sent: u64,
    pub packets_received: u64,
    pub packets_recovered: u64,
    pub packets_lost: u64,
    pub shards_late: u64,
}

// ── Sender ────────────────────────────────────────────────────────────────────

pub struct MediaSender {
    config: MediaConfig,
    coder: Option<BlockCoder>,
    block: u16,
    shards: Vec<Vec<u8>>,
    block_started: Option<Instant>,
    /// Packets waiting to be coalesced into the next shard.
    packing: Vec<u8>,
    packed_count: u8,
    packing_started: Option<Instant>,
    pub stats: MediaStats,
}

/// Shard payload layout when packing: repeated `[len u16 BE][packet]`.
fn pack_into(buf: &mut Vec<u8>, packet: &[u8]) {
    buf.extend_from_slice(&(packet.len() as u16).to_be_bytes());
    buf.extend_from_slice(packet);
}

/// Split a packed shard back into application packets.
pub fn unpack(shard: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 2 <= shard.len() {
        let len = u16::from_be_bytes([shard[offset], shard[offset + 1]]) as usize;
        offset += 2;
        if len == 0 || offset + len > shard.len() {
            break;
        }
        out.push(shard[offset..offset + len].to_vec());
        offset += len;
    }
    out
}

impl MediaSender {
    pub fn new(config: MediaConfig) -> Result<Self, String> {
        config.validate()?;
        Ok(Self {
            config,
            coder: None,
            block: 0,
            shards: Vec::with_capacity(usize::from(config.k)),
            block_started: None,
            packing: Vec::new(),
            packed_count: 0,
            packing_started: None,
            stats: MediaStats::default(),
        })
    }

    pub fn config(&self) -> &MediaConfig {
        &self.config
    }

    /// Queue one application packet. Returns the frames to transmit now:
    /// a data shard when one is complete (immediately, unless packing), plus
    /// the block's parity when it just filled.
    pub fn push(&mut self, packet: &[u8], now: Instant) -> Result<Vec<Frame>, String> {
        if packet.is_empty() || packet.len() > MAX_MEDIA_PACKET - 2 {
            return Err(format!(
                "media packet must be 1..={} bytes",
                MAX_MEDIA_PACKET - 2
            ));
        }
        self.stats.packets_sent += 1;
        let mut frames = Vec::new();
        // Would this packet overflow the shard being packed? Ship that first.
        if !self.packing.is_empty() && self.packing.len() + 2 + packet.len() > MAX_MEDIA_PACKET {
            frames.extend(self.emit_shard(now));
        }
        if self.packing.is_empty() {
            self.packing_started = Some(now);
        }
        pack_into(&mut self.packing, packet);
        self.packed_count = self.packed_count.saturating_add(1);
        if self.config.pack_window.is_zero()
            || self.packing.len() + 2 >= MAX_MEDIA_PACKET
            || self.packed_count >= MAX_PACKED_PER_SHARD
        {
            frames.extend(self.emit_shard(now));
        }
        Ok(frames)
    }

    /// Time-driven work: ship a packed shard whose window elapsed and
    /// parity for a partial block whose timeout expired (call periodically).
    pub fn poll(&mut self, now: Instant) -> Vec<Frame> {
        let mut frames = Vec::new();
        if let Some(started) = self.packing_started {
            if !self.packing.is_empty() && now.duration_since(started) >= self.config.pack_window {
                frames.extend(self.emit_shard(now));
            }
        }
        if let Some(started) = self.block_started {
            if !self.shards.is_empty() && now.duration_since(started) >= self.config.block_timeout {
                frames.extend(self.close_block());
            }
        }
        frames
    }

    /// Turn the packing buffer into a data shard of the current block.
    fn emit_shard(&mut self, now: Instant) -> Vec<Frame> {
        if self.packing.is_empty() {
            return Vec::new();
        }
        let data = std::mem::take(&mut self.packing);
        self.packed_count = 0;
        self.packing_started = None;
        let mut frames = Vec::with_capacity(1 + usize::from(self.config.m));
        if self.shards.is_empty() {
            self.block_started = Some(now);
        }
        let shard = self.shards.len() as u8;
        frames.push(Frame::Media(MediaShard {
            stream: self.config.stream,
            block: self.block,
            shard,
            k: self.config.k,
            m: self.config.m,
            len: data.len() as u16,
            data: data.clone(),
        }));
        self.shards.push(data);
        if self.shards.len() >= usize::from(self.config.k) {
            frames.extend(self.close_block());
        }
        frames
    }

    /// Force out whatever is pending (end of a PTT burst).
    pub fn flush(&mut self) -> Vec<Frame> {
        let mut frames = self.emit_shard(Instant::now());
        if !self.shards.is_empty() {
            frames.extend(self.close_block());
        }
        frames
    }

    fn close_block(&mut self) -> Vec<Frame> {
        let k = self.shards.len();
        let m = usize::from(self.config.m);
        let block = self.block;
        self.block = self.block.wrapping_add(1);
        self.block_started = None;
        let shards = std::mem::take(&mut self.shards);
        if m == 0 {
            return Vec::new();
        }
        let shard_len = shards.iter().map(|s| s.len()).max().unwrap_or(0);
        let padded: Vec<Vec<u8>> = shards.iter().map(|s| pad_shard(s, shard_len)).collect();
        let coder = match self.coder.as_ref().filter(|c| c.k() == k) {
            Some(coder) => coder,
            None => match BlockCoder::new(k, m) {
                Ok(coder) => {
                    self.coder = Some(coder);
                    self.coder.as_ref().unwrap()
                }
                Err(_) => return Vec::new(),
            },
        };
        let parity = coder.parity(&padded).unwrap_or_default();
        self.stats.parity_sent += parity.len() as u64;
        parity
            .into_iter()
            .enumerate()
            .map(|(i, data)| {
                Frame::Media(MediaShard {
                    stream: self.config.stream,
                    block,
                    shard: (k + i) as u8,
                    k: k as u8,
                    m: m as u8,
                    len: 0,
                    data,
                })
            })
            .collect()
    }
}

// ── Receiver ──────────────────────────────────────────────────────────────────

/// A delivered application packet with its position for reordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPacket {
    pub stream: u8,
    pub block: u16,
    /// Shard within the block.
    pub index: u8,
    /// Position within a packed shard.
    pub sub: u8,
    pub data: Vec<u8>,
    pub recovered: bool,
}

struct RxBlock {
    /// Data shards in the block. Data frames advertise the sender's
    /// configured `k`; a block closed early by the timeout carries the real,
    /// smaller `k` on its parity, so parity is authoritative.
    k: usize,
    k_confirmed: bool,
    m: usize,
    shards: Vec<Option<Vec<u8>>>,
    lens: Vec<u16>,
    delivered: Vec<bool>,
    first_seen: Instant,
    /// Monotonic arrival order, so eviction is not confused by id wrap.
    seq: u64,
    done: bool,
}

pub struct MediaReceiver {
    latency_budget: Duration,
    blocks: BTreeMap<u16, RxBlock>,
    coders: HashMap<(usize, usize), BlockCoder>,
    next_seq: u64,
    last_activity: Instant,
    pub stats: MediaStats,
}

impl MediaReceiver {
    pub fn new(latency_budget: Duration) -> Self {
        Self {
            latency_budget,
            blocks: BTreeMap::new(),
            coders: HashMap::new(),
            next_seq: 0,
            last_activity: Instant::now(),
            stats: MediaStats::default(),
        }
    }

    /// Accept one shard; returns packets ready for the application (the
    /// shard itself if it is data, plus anything parity could rebuild).
    pub fn receive(&mut self, shard: MediaShard, now: Instant) -> Vec<MediaPacket> {
        self.last_activity = now;
        self.expire(now);
        let k = usize::from(shard.k);
        let m = usize::from(shard.m);
        if k == 0 || k + m > MAX_BLOCK_SHARDS || usize::from(shard.shard) >= k + m {
            return Vec::new();
        }
        let stream = shard.stream;
        let block_id = shard.block;
        let seq = self.next_seq;
        self.next_seq += 1;
        let block = self.blocks.entry(block_id).or_insert_with(|| RxBlock {
            k,
            k_confirmed: false,
            m,
            shards: vec![None; k + m],
            lens: vec![0; k],
            delivered: vec![false; k],
            first_seen: now,
            seq,
            done: false,
        });
        let index = usize::from(shard.shard);
        // A parity shard states the block's real width; data shards only
        // carry the sender's configured k, which is larger when the block
        // was closed early by the packing timeout. Adopt the parity's k
        // whichever order the shards arrive in.
        let is_parity = index >= k;
        if !block.done && block.k != k {
            if is_parity && !block.k_confirmed && k < block.k {
                block.shards.truncate(k);
                block.shards.extend(std::iter::repeat_n(None, m));
                block.lens.truncate(k);
                block.delivered.truncate(k);
                block.k = k;
                block.k_confirmed = true;
            } else if !is_parity && block.k_confirmed && k > block.k {
                // Late data shard of an already-shrunk block: its slot is
                // valid, only its advertised width is stale.
                if index >= block.k {
                    self.stats.shards_late += 1;
                    return Vec::new();
                }
            }
        }
        if is_parity {
            block.k_confirmed = true;
        }
        let k = block.k;
        let m = block.m;
        if block.done || index >= k + m {
            if !(block.done && index >= k) {
                self.stats.shards_late += 1;
            }
            return Vec::new();
        }
        if block.shards[index].is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if index < k {
            block.lens[index] = shard.len;
            block.delivered[index] = true;
            for (sub, data) in unpack(&shard.data).into_iter().enumerate() {
                self.stats.packets_received += 1;
                out.push(MediaPacket {
                    stream,
                    block: block_id,
                    index: index as u8,
                    sub: sub as u8,
                    data,
                    recovered: false,
                });
            }
        }
        block.shards[index] = Some(shard.data);

        // Try to rebuild once k of k+m shards are present and data is missing.
        let present = block.shards.iter().filter(|s| s.is_some()).count();
        let missing_data = block.delivered.iter().filter(|d| !**d).count();
        if missing_data == 0 {
            block.done = true;
        } else if present >= k && m > 0 {
            let shard_len = block
                .shards
                .iter()
                .skip(k)
                .flatten()
                .map(|s| s.len())
                .max()
                .or_else(|| block.shards.iter().flatten().map(|s| s.len()).max())
                .unwrap_or(0);
            let mut shards: Vec<Option<Vec<u8>>> = block
                .shards
                .iter()
                .map(|s| s.as_ref().map(|s| pad_shard(s, shard_len)))
                .collect();
            let coder = self
                .coders
                .entry((k, m))
                .or_insert_with(|| BlockCoder::new(k, m).expect("validated params"));
            if coder.reconstruct(&mut shards).is_ok() {
                for (slot, shard) in shards.iter().enumerate().take(k) {
                    if block.delivered[slot] {
                        continue;
                    }
                    let Some(data) = shard.as_ref() else {
                        continue;
                    };
                    // Recovered shards are padded, but every packet inside is
                    // length-prefixed, so unpacking stops exactly at the end.
                    block.delivered[slot] = true;
                    for (sub, data) in unpack(data).into_iter().enumerate() {
                        self.stats.packets_recovered += 1;
                        out.push(MediaPacket {
                            stream,
                            block: block_id,
                            index: slot as u8,
                            sub: sub as u8,
                            data,
                            recovered: true,
                        });
                    }
                }
                block.done = true;
            }
        }
        out
    }

    /// Drop blocks past the latency budget, counting undelivered packets.
    pub fn expire(&mut self, now: Instant) {
        let budget = self.latency_budget;
        let expired: Vec<u16> = self
            .blocks
            .iter()
            .filter(|(_, b)| now.duration_since(b.first_seen) > budget)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            if let Some(block) = self.blocks.remove(&id) {
                if !block.done {
                    self.stats.packets_lost +=
                        block.delivered.iter().filter(|d| !**d).count() as u64;
                }
            }
        }
        // Hard cap regardless of time, so a stalled clock cannot grow state.
        // Evict by arrival order — block ids wrap.
        while self.blocks.len() > MAX_RX_BLOCKS {
            let Some(oldest) = self
                .blocks
                .iter()
                .min_by_key(|(_, block)| block.seq)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.blocks.remove(&oldest);
        }
    }

    /// True when nothing has arrived for `idle`; used to reap dead streams.
    pub fn is_idle(&self, now: Instant, idle: Duration) -> bool {
        now.duration_since(self.last_activity) > idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(i: usize) -> Vec<u8> {
        (0..(80 + i * 7))
            .map(|j| ((i * 31 + j) % 250 + 1) as u8)
            .collect()
    }

    /// Voice profile without packing so each packet is one shard.
    fn unpacked_voice() -> MediaConfig {
        MediaConfig {
            pack_window: Duration::ZERO,
            block_timeout: Duration::from_millis(60),
            ..MediaConfig::voice(1)
        }
    }

    #[test]
    fn full_block_recovers_one_lost_packet() {
        let t0 = Instant::now();
        let mut tx = MediaSender::new(unpacked_voice()).unwrap();
        let mut rx = MediaReceiver::new(Duration::from_millis(250));
        let mut frames = Vec::new();
        for i in 0..4 {
            frames.extend(tx.push(&packet(i), t0).unwrap());
        }
        assert_eq!(frames.len(), 5); // 4 data + 1 parity
        let mut delivered = Vec::new();
        for (n, frame) in frames.into_iter().enumerate() {
            if n == 2 {
                continue; // packet 2 lost on air
            }
            let Frame::Media(shard) = frame else { panic!() };
            delivered.extend(rx.receive(shard, t0));
        }
        assert_eq!(delivered.len(), 4);
        let recovered = delivered.iter().find(|p| p.index == 2).unwrap();
        assert!(recovered.recovered);
        assert_eq!(recovered.data, packet(2));
        assert_eq!(rx.stats.packets_recovered, 1);
        assert_eq!(rx.stats.packets_received, 3);
    }

    #[test]
    fn partial_block_closes_on_timeout_with_smaller_k() {
        let t0 = Instant::now();
        let mut tx = MediaSender::new(unpacked_voice()).unwrap();
        let mut frames = tx.push(&packet(0), t0).unwrap();
        frames.extend(tx.push(&packet(1), t0).unwrap());
        assert!(tx.poll(t0 + Duration::from_millis(10)).is_empty());
        let parity = tx.poll(t0 + Duration::from_millis(70));
        assert_eq!(parity.len(), 1);
        let Frame::Media(p) = &parity[0] else {
            panic!()
        };
        assert_eq!(p.k, 2);
        assert_eq!(p.shard, 2);
        frames.extend(parity);

        let mut rx = MediaReceiver::new(Duration::from_millis(250));
        let mut delivered = Vec::new();
        for (n, frame) in frames.into_iter().enumerate() {
            if n == 0 {
                continue;
            }
            let Frame::Media(shard) = frame else { panic!() };
            delivered.extend(rx.receive(shard, t0));
        }
        assert_eq!(delivered.len(), 2);
        assert_eq!(
            delivered.iter().find(|p| p.index == 0).unwrap().data,
            packet(0)
        );
        // Next block starts at 1.
        let next = tx.push(&packet(5), t0).unwrap();
        let Frame::Media(s) = &next[0] else { panic!() };
        assert_eq!(s.block, 1);
    }

    #[test]
    fn late_blocks_are_dropped_not_repaired() {
        let t0 = Instant::now();
        let mut tx = MediaSender::new(unpacked_voice()).unwrap();
        let mut rx = MediaReceiver::new(Duration::from_millis(100));
        let frames: Vec<Frame> = (0..4)
            .flat_map(|i| tx.push(&packet(i), t0).unwrap())
            .collect();
        let mut iter = frames.into_iter();
        let Frame::Media(first) = iter.next().unwrap() else {
            panic!()
        };
        assert_eq!(rx.receive(first, t0).len(), 1);
        // Everything else arrives after the budget.
        let late = t0 + Duration::from_millis(500);
        let mut delivered = 0;
        for frame in iter {
            let Frame::Media(s) = frame else { panic!() };
            delivered += rx.receive(s, late).len();
        }
        // shards start a fresh entry for the same block id, deliver their
        // data shards and rebuild slot 0 again — harmless for a codec with
        // its own jitter buffer, and it never blocks on stale state.
        assert_eq!(rx.stats.packets_lost, 3);
        assert_eq!(delivered, 4);
    }

    #[test]
    fn rejects_oversized_packets() {
        let mut tx = MediaSender::new(unpacked_voice()).unwrap();
        assert!(tx
            .push(&vec![1u8; MAX_MEDIA_PACKET + 1], Instant::now())
            .is_err());
        assert!(tx.push(&[], Instant::now()).is_err());
    }

    #[test]
    fn packing_coalesces_small_packets_into_one_frame() {
        let t0 = Instant::now();
        let mut tx = MediaSender::new(MediaConfig::voice(3)).unwrap(); // 40 ms window
        assert!(tx.push(&packet(0), t0).unwrap().is_empty());
        assert!(tx
            .push(&packet(1), t0 + Duration::from_millis(20))
            .unwrap()
            .is_empty());
        let frames = tx.poll(t0 + Duration::from_millis(45));
        assert_eq!(frames.len(), 1);
        let Frame::Media(shard) = &frames[0] else {
            panic!()
        };
        assert_eq!(unpack(&shard.data), vec![packet(0), packet(1)]);

        let mut rx = MediaReceiver::new(Duration::from_millis(300));
        let delivered = rx.receive(shard.clone(), t0);
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[1].sub, 1);
        assert_eq!(delivered[1].data, packet(1));
        assert_eq!(rx.stats.packets_received, 2);
    }

    #[test]
    fn shrunk_block_reconstructs_when_parity_arrives_first() {
        let t0 = Instant::now();
        let mut tx = MediaSender::new(unpacked_voice()).unwrap();
        // Two packets, then the block closes early on its timeout: data
        // shards advertise k=4, the parity carries the real k=2.
        let mut frames = tx.push(&packet(0), t0).unwrap();
        frames.extend(tx.push(&packet(1), t0).unwrap());
        frames.extend(tx.poll(t0 + Duration::from_millis(70)));
        assert_eq!(frames.len(), 3);

        // Parity first, then the surviving data shard: packet 0 was lost.
        let mut rx = MediaReceiver::new(Duration::from_millis(250));
        let order = [2usize, 1];
        let mut delivered = Vec::new();
        for i in order {
            let Frame::Media(shard) = frames[i].clone() else {
                panic!()
            };
            delivered.extend(rx.receive(shard, t0));
        }
        assert_eq!(rx.stats.shards_late, 0, "no shard may be discarded");
        let recovered = delivered
            .iter()
            .find(|p| p.index == 0)
            .expect("packet 0 rebuilt");
        assert!(recovered.recovered);
        assert_eq!(recovered.data, packet(0));
    }
}
