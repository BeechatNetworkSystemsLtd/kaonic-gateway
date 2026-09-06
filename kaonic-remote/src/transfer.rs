//! Chunked blob transfer (controller → target) over a Reticulum link.
//!
//! The sender streams chunks in windows and periodically asks the receiver
//! for its progress ([`BlobStatusResponse`]); the receiver keeps a bitmap of
//! chunks written to a spool file and verifies the SHA-256 at the end. There
//! is no per-chunk acknowledgement — on a radio link that would double the
//! airtime — only the window status round-trip.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::erasure::{pad_shard, BlockCoder};

use crate::protocol::{BlobStatusResponse, CHUNK_SIZE, MAX_BLOB_SIZE, MAX_MISSING_REPORT};

/// Chunks the sender pushes between two status requests (streaming mode).
pub const WINDOW_CHUNKS: u32 = 36;
/// Upper bound on chunks sent beyond the receiver's confirmed position.
pub const MAX_IN_FLIGHT: u32 = 64;
/// Parity is kept for at most this many incomplete blocks per transfer.
const MAX_PENDING_BLOCKS: usize = 48;
/// Smallest chunk a sender may negotiate. Without a floor a peer could ask
/// for one-byte chunks and make the receiver allocate a bitmap per byte.
pub const MIN_CHUNK_SIZE: u32 = 256;

/// Parity shards received for a block that is not complete yet.
struct PendingBlock {
    parity: Vec<Option<Vec<u8>>>,
}

/// What a sender negotiated for one transfer.
#[derive(Debug, Clone)]
pub struct BlobSpec {
    pub purpose: u8,
    pub name: String,
    pub size: u32,
    pub sha256: [u8; 32],
    pub chunk_size: u32,
    /// Data chunks per erasure block, and parity shards per block (0 = none).
    pub block_k: u8,
    pub block_m: u8,
}

pub struct BlobReceiver {
    pub purpose: u8,
    pub name: String,
    pub size: u32,
    pub sha256: [u8; 32],
    pub chunk_size: u32,
    pub path: PathBuf,
    received: Vec<u64>,
    received_count: u32,
    highest: u32,
    pub last_activity: u64,
    // Chunk writes are 800 B positional writes into the page cache; doing
    // them synchronously avoids two thread hops per chunk on a single core.
    file: std::fs::File,
    /// Outer erasure code (None when the sender uses no parity).
    coder: Option<BlockCoder>,
    block_k: u32,
    pending: HashMap<u32, PendingBlock>,
    pending_order: VecDeque<u32>,
    /// Data chunks rebuilt from parity instead of retransmitted.
    pub recovered: u32,
}

impl BlobReceiver {
    pub fn total_chunks(size: u32, chunk_size: u32) -> u32 {
        if size == 0 {
            0
        } else {
            size.div_ceil(chunk_size)
        }
    }

    pub async fn create(
        spool_dir: &Path,
        file_name: &str,
        spec: BlobSpec,
        now: u64,
    ) -> Result<Self, String> {
        let BlobSpec {
            purpose,
            name,
            size,
            sha256,
            chunk_size,
            block_k,
            block_m,
        } = spec;
        let coder = if block_m > 0 {
            Some(
                BlockCoder::new(block_k as usize, block_m as usize)
                    .map_err(|err| format!("erasure block: {err}"))?,
            )
        } else {
            None
        };
        if size == 0 || size > MAX_BLOB_SIZE {
            return Err(format!("blob size {size} out of range"));
        }
        if !(MIN_CHUNK_SIZE..=CHUNK_SIZE as u32).contains(&chunk_size) {
            return Err(format!("chunk size {chunk_size} out of range"));
        }
        std::fs::create_dir_all(spool_dir).map_err(|err| format!("spool dir: {err}"))?;
        let path = spool_dir.join(file_name);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(true)
            .open(&path)
            .map_err(|err| format!("spool file: {err}"))?;
        file.set_len(size as u64)
            .map_err(|err| format!("spool alloc: {err}"))?;
        let total = Self::total_chunks(size, chunk_size);
        Ok(Self {
            purpose,
            name,
            size,
            sha256,
            chunk_size,
            path,
            received: vec![0u64; total.div_ceil(64) as usize],
            received_count: 0,
            highest: 0,
            last_activity: now,
            file,
            coder,
            block_k: if block_k == 0 { 1 } else { u32::from(block_k) },
            pending: HashMap::new(),
            pending_order: VecDeque::new(),
            recovered: 0,
        })
    }

    pub fn block_k(&self) -> u32 {
        self.block_k
    }

    fn chunk_len(&self, index: u32) -> usize {
        if index + 1 == self.total() {
            (self.size - index * self.chunk_size) as usize
        } else {
            self.chunk_size as usize
        }
    }

    /// Number of erasure blocks in this transfer.
    fn block_count(&self) -> u32 {
        self.total().div_ceil(self.block_k)
    }

    /// Data chunks that belong to `block` (the last block may be short).
    /// Out-of-range block numbers yield an empty range rather than
    /// overflowing — `block` comes off the wire.
    fn block_range(&self, block: u32) -> std::ops::Range<u32> {
        if block >= self.block_count() {
            return 0..0;
        }
        let start = block * self.block_k;
        start..(start + self.block_k).min(self.total())
    }

    fn block_complete(&self, block: u32) -> bool {
        self.block_range(block).all(|index| self.is_received(index))
    }

    fn mark_received(&mut self, index: u32) {
        self.received[(index / 64) as usize] |= 1u64 << (index % 64);
        self.received_count += 1;
        self.highest = self.highest.max(index + 1);
    }

    /// Store a parity shard; returns the number of chunks recovered.
    pub async fn write_parity(
        &mut self,
        block: u32,
        shard: u8,
        data: &[u8],
        now: u64,
    ) -> Result<u32, String> {
        self.last_activity = now;
        let Some(coder) = self.coder.as_ref() else {
            return Ok(0);
        };
        let range = self.block_range(block);
        if range.is_empty() || usize::from(shard) >= coder.m() || self.block_complete(block) {
            return Ok(0);
        }
        if data.len() != self.chunk_size as usize {
            return Err(format!("parity shard length {}", data.len()));
        }
        let m = coder.m();
        if !self.pending.contains_key(&block) {
            while self.pending.len() >= MAX_PENDING_BLOCKS {
                let Some(old) = self.pending_order.pop_front() else {
                    break;
                };
                self.pending.remove(&old);
            }
            self.pending.insert(
                block,
                PendingBlock {
                    parity: vec![None; m],
                },
            );
            self.pending_order.push_back(block);
        }
        if let Some(pending) = self.pending.get_mut(&block) {
            pending.parity[usize::from(shard)] = Some(data.to_vec());
        }
        self.try_reconstruct(block)
    }

    /// Rebuild missing data chunks of `block` once k of k+m shards are here.
    fn try_reconstruct(&mut self, block: u32) -> Result<u32, String> {
        let Some(coder) = self.coder.as_ref() else {
            return Ok(0);
        };
        let range = self.block_range(block);
        let k = coder.k();
        let present_data = range.clone().filter(|i| self.is_received(*i)).count();
        if present_data == range.len() {
            self.forget_block(block);
            return Ok(0);
        }
        // Slots beyond a short last block are known zero shards.
        let virtual_slots = k - range.len();
        let present_parity = self
            .pending
            .get(&block)
            .map(|p| p.parity.iter().filter(|s| s.is_some()).count())
            .unwrap_or(0);
        if present_data + virtual_slots + present_parity < k {
            return Ok(0);
        }
        // Short last block: the sender coded it as a full block with zero
        // chunks appended, so we do the same.
        let shard_len = self.chunk_size as usize;
        let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(k + coder.m());
        for slot in 0..k as u32 {
            let index = block * self.block_k + slot;
            if index >= range.end {
                shards.push(Some(vec![0u8; shard_len]));
            } else if self.is_received(index) {
                let mut buf = vec![0u8; self.chunk_len(index)];
                self.file
                    .read_exact_at(&mut buf, u64::from(index) * u64::from(self.chunk_size))
                    .map_err(|err| format!("read: {err}"))?;
                shards.push(Some(pad_shard(&buf, shard_len)));
            } else {
                shards.push(None);
            }
        }
        if let Some(pending) = self.pending.get(&block) {
            shards.extend(pending.parity.iter().cloned());
        }
        coder
            .reconstruct(&mut shards)
            .map_err(|err| err.to_string())?;
        let mut recovered = 0u32;
        for slot in 0..k as u32 {
            let index = block * self.block_k + slot;
            if index >= range.end || self.is_received(index) {
                continue;
            }
            let data = shards[slot as usize].as_ref().ok_or("reconstruct gap")?;
            let len = self.chunk_len(index);
            self.file
                .write_all_at(&data[..len], u64::from(index) * u64::from(self.chunk_size))
                .map_err(|err| format!("write: {err}"))?;
            self.mark_received(index);
            recovered += 1;
        }
        self.forget_block(block);
        self.recovered += recovered;
        Ok(recovered)
    }

    fn forget_block(&mut self, block: u32) {
        if self.pending.remove(&block).is_some() {
            self.pending_order.retain(|b| *b != block);
        }
    }

    pub fn total(&self) -> u32 {
        Self::total_chunks(self.size, self.chunk_size)
    }

    pub fn is_received(&self, index: u32) -> bool {
        self.received
            .get((index / 64) as usize)
            .map(|word| word & (1u64 << (index % 64)) != 0)
            .unwrap_or(false)
    }

    pub fn complete(&self) -> bool {
        self.received_count == self.total()
    }

    pub fn next_index(&self) -> u32 {
        (0..self.total())
            .find(|index| !self.is_received(*index))
            .unwrap_or(self.total())
    }

    pub fn status(&self) -> BlobStatusResponse {
        let next_index = self.next_index();
        let mut received_mask = 0u32;
        for bit in 0..32u32 {
            let index = next_index + 1 + bit;
            if index < self.total() && self.is_received(index) {
                received_mask |= 1 << bit;
            }
        }
        let mut missing = Vec::new();
        let mut index = next_index;
        while index < self.highest && missing.len() < MAX_MISSING_REPORT {
            if !self.is_received(index) {
                missing.push(index);
            }
            index += 1;
        }
        BlobStatusResponse {
            next_index,
            received_mask,
            complete: self.complete(),
            highest: self.highest,
            missing,
        }
    }

    /// Write one chunk. Duplicate or out-of-range chunks are ignored.
    pub async fn write_chunk(&mut self, index: u32, data: &[u8], now: u64) -> Result<(), String> {
        self.last_activity = now;
        let total = self.total();
        if index >= total || self.is_received(index) {
            return Ok(());
        }
        let expected_len = if index + 1 == total {
            self.size - index * self.chunk_size
        } else {
            self.chunk_size
        } as usize;
        if data.len() != expected_len {
            return Err(format!(
                "chunk {index} length {} != expected {expected_len}",
                data.len()
            ));
        }
        let offset = index as u64 * self.chunk_size as u64;
        self.file
            .write_all_at(data, offset)
            .map_err(|err| format!("write: {err}"))?;
        self.mark_received(index);
        let block = index / self.block_k;
        if self.pending.contains_key(&block) {
            let _ = self.try_reconstruct(block)?;
        }
        Ok(())
    }

    /// Flush and verify the SHA-256 of the assembled blob.
    pub async fn finish(&mut self) -> Result<PathBuf, String> {
        if !self.complete() {
            return Err(format!(
                "transfer incomplete: {}/{} chunks",
                self.received_count,
                self.total()
            ));
        }
        self.file
            .sync_data()
            .map_err(|err| format!("flush: {err}"))?;
        // Hashing a multi-MB spool is the one genuinely blocking step.
        let path = self.path.clone();
        let digest: [u8; 32] = tokio::task::spawn_blocking(move || -> Result<[u8; 32], String> {
            let mut file = std::fs::File::open(&path).map_err(|err| format!("open: {err}"))?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = file.read(&mut buf).map_err(|err| format!("read: {err}"))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hasher.finalize().into())
        })
        .await
        .map_err(|err| format!("hash task: {err}"))??;
        if digest != self.sha256 {
            return Err("sha256 mismatch".into());
        }
        Ok(self.path.clone())
    }

    pub async fn discard(&self) {
        let _ = tokio::fs::remove_file(&self.path).await;
    }
}

pub fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(size: u32, sha256: [u8; 32], block_k: u8, block_m: u8) -> BlobSpec {
        BlobSpec {
            purpose: 1,
            name: "x".into(),
            size,
            sha256,
            chunk_size: 800,
            block_k,
            block_m,
        }
    }

    #[tokio::test]
    async fn receiver_assembles_out_of_order_and_verifies() {
        let dir = std::env::temp_dir().join(format!("kaonic-remote-test-{}", std::process::id()));
        let payload: Vec<u8> = (0..2_000u32).map(|i| (i % 251) as u8).collect();
        let sha = sha256_of(&payload);
        let mut rx = BlobReceiver::create(&dir, "t.part", spec(payload.len() as u32, sha, 0, 0), 0)
            .await
            .unwrap();
        assert_eq!(rx.total(), 3);
        assert_eq!(rx.status().next_index, 0);

        rx.write_chunk(2, &payload[1600..], 1).await.unwrap();
        let status = rx.status();
        assert_eq!(status.next_index, 0);
        assert_eq!(status.received_mask, 0b10);
        assert_eq!(status.highest, 3);
        assert_eq!(status.missing, vec![0, 1]);
        assert!(!status.complete);

        rx.write_chunk(0, &payload[..800], 2).await.unwrap();
        rx.write_chunk(0, &payload[..800], 2).await.unwrap(); // duplicate ignored
        assert!(rx.write_chunk(1, &payload[800..1000], 2).await.is_err()); // wrong length
        rx.write_chunk(1, &payload[800..1600], 3).await.unwrap();
        assert!(rx.complete());
        let path = rx.finish().await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        rx.discard().await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn receiver_rejects_bad_hash() {
        let dir = std::env::temp_dir().join(format!("kaonic-remote-test-h-{}", std::process::id()));
        let payload = vec![7u8; 100];
        let mut rx = BlobReceiver::create(&dir, "h.part", spec(100, [0u8; 32], 0, 0), 0)
            .await
            .unwrap();
        rx.write_chunk(0, &payload, 1).await.unwrap();
        assert!(rx.finish().await.is_err());
        rx.discard().await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn parity_recovers_lost_chunks_without_retransmit() {
        let dir = std::env::temp_dir().join(format!("kaonic-remote-test-p-{}", std::process::id()));
        // 3 full blocks of 4 + a short last block (2 chunks, last one partial)
        let payload: Vec<u8> = (0..(14 * 800 - 300) as u32)
            .map(|i| (i * 11 % 251) as u8)
            .collect();
        let sha = sha256_of(&payload);
        let chunk = 800usize;
        let mut rx = BlobReceiver::create(&dir, "p.part", spec(payload.len() as u32, sha, 4, 1), 0)
            .await
            .unwrap();
        let total = rx.total() as usize;
        let coder = BlockCoder::new(4, 1).unwrap();
        let mut data_chunks: Vec<Vec<u8>> = (0..total)
            .map(|i| payload[i * chunk..((i + 1) * chunk).min(payload.len())].to_vec())
            .collect();
        // Lose one chunk in every block (including the short last block).
        for (block, chunks) in data_chunks.chunks(4).enumerate() {
            let mut shards: Vec<Vec<u8>> = chunks.iter().map(|c| pad_shard(c, chunk)).collect();
            while shards.len() < 4 {
                shards.push(vec![0u8; chunk]);
            }
            let parity = coder.parity(&shards).unwrap();
            for (slot, c) in chunks.iter().enumerate() {
                let index = (block * 4 + slot) as u32;
                if slot == 1 {
                    continue; // lost on air
                }
                rx.write_chunk(index, c, 1).await.unwrap();
            }
            let recovered = rx
                .write_parity(block as u32, 0, &parity[0], 2)
                .await
                .unwrap();
            assert_eq!(recovered, 1, "block {block}");
        }
        assert!(rx.complete());
        assert_eq!(rx.recovered, 4);
        assert!(rx.status().missing.is_empty());
        let path = rx.finish().await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), payload);
        rx.discard().await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        data_chunks.clear();
    }
}
