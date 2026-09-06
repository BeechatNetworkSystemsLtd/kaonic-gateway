//! Outer erasure code shared by bulk transfers and media streams.
//!
//! The radio loses *whole frames* (RX-buffer overrun, half-duplex
//! collisions) far more often than it delivers corrupted bits, so on top of
//! the per-frame LDPC we run a systematic Reed–Solomon code across a block
//! of `k` shards with `m` parity shards: any `k` of the `k + m` frames rebuild
//! the block, with no round trip. Shards in a block share one length; the
//! last data shard is zero-padded and its true length carried out of band.

use reed_solomon_erasure::galois_8::ReedSolomon;

/// Largest block a receiver will accept (bounds memory per transfer/stream).
pub const MAX_BLOCK_SHARDS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErasureError {
    InvalidParams,
    NotEnoughShards,
    ShardLength,
    Codec(String),
}

impl std::fmt::Display for ErasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErasureError::InvalidParams => f.write_str("invalid erasure parameters"),
            ErasureError::NotEnoughShards => f.write_str("not enough shards to reconstruct"),
            ErasureError::ShardLength => f.write_str("shard length mismatch"),
            ErasureError::Codec(err) => write!(f, "erasure codec: {err}"),
        }
    }
}

impl std::error::Error for ErasureError {}

/// Systematic RS(k + m, k) over GF(2^8).
pub struct BlockCoder {
    rs: ReedSolomon,
    k: usize,
    m: usize,
}

impl BlockCoder {
    pub fn new(k: usize, m: usize) -> Result<Self, ErasureError> {
        if k == 0 || m == 0 || k + m > MAX_BLOCK_SHARDS {
            return Err(ErasureError::InvalidParams);
        }
        let rs = ReedSolomon::new(k, m).map_err(|e| ErasureError::Codec(e.to_string()))?;
        Ok(Self { rs, k, m })
    }

    pub fn k(&self) -> usize {
        self.k
    }

    pub fn m(&self) -> usize {
        self.m
    }

    /// Parity shards for `data` (`k` equal-length shards).
    pub fn parity(&self, data: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, ErasureError> {
        if data.len() != self.k {
            return Err(ErasureError::InvalidParams);
        }
        let len = data[0].len();
        if data.iter().any(|s| s.len() != len) {
            return Err(ErasureError::ShardLength);
        }
        let mut shards: Vec<Vec<u8>> = data.to_vec();
        shards.extend((0..self.m).map(|_| vec![0u8; len]));
        self.rs
            .encode(&mut shards)
            .map_err(|e| ErasureError::Codec(e.to_string()))?;
        Ok(shards.split_off(self.k))
    }

    /// Fill in the missing entries of `shards` (length `k + m`, `None` for
    /// lost shards) in place. Requires at least `k` present.
    pub fn reconstruct(&self, shards: &mut [Option<Vec<u8>>]) -> Result<(), ErasureError> {
        if shards.len() != self.k + self.m {
            return Err(ErasureError::InvalidParams);
        }
        let present = shards.iter().filter(|s| s.is_some()).count();
        if present < self.k {
            return Err(ErasureError::NotEnoughShards);
        }
        let len = shards.iter().flatten().map(|s| s.len()).next().unwrap_or(0);
        if shards.iter().flatten().any(|s| s.len() != len) {
            return Err(ErasureError::ShardLength);
        }
        self.rs
            .reconstruct(shards)
            .map_err(|e| ErasureError::Codec(e.to_string()))
    }
}

/// Zero-pad `shard` to `len` (shards in a block must match).
pub fn pad_shard(shard: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(shard);
    out.resize(len, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(k: usize, len: usize) -> Vec<Vec<u8>> {
        (0..k)
            .map(|i| (0..len).map(|j| ((i * 131 + j * 7) % 251) as u8).collect())
            .collect()
    }

    #[test]
    fn recovers_up_to_m_losses() {
        let coder = BlockCoder::new(16, 2).unwrap();
        let data = block(16, 800);
        let parity = coder.parity(&data).unwrap();
        assert_eq!(parity.len(), 2);

        let mut shards: Vec<Option<Vec<u8>>> = data
            .iter()
            .cloned()
            .map(Some)
            .chain(parity.iter().cloned().map(Some))
            .collect();
        shards[3] = None;
        shards[17] = None; // one data + one parity lost
        coder.reconstruct(&mut shards).unwrap();
        assert_eq!(shards[3].as_deref(), Some(data[3].as_slice()));

        let mut shards: Vec<Option<Vec<u8>>> = data
            .iter()
            .cloned()
            .map(Some)
            .chain(parity.iter().cloned().map(Some))
            .collect();
        shards[0] = None;
        shards[9] = None;
        shards[15] = None; // three losses > m
        assert_eq!(
            coder.reconstruct(&mut shards),
            Err(ErasureError::NotEnoughShards)
        );
    }

    #[test]
    fn small_media_block() {
        let coder = BlockCoder::new(4, 1).unwrap();
        let data = block(4, 120);
        let parity = coder.parity(&data).unwrap();
        let mut shards: Vec<Option<Vec<u8>>> = vec![
            Some(data[0].clone()),
            None,
            Some(data[2].clone()),
            Some(data[3].clone()),
            Some(parity[0].clone()),
        ];
        coder.reconstruct(&mut shards).unwrap();
        assert_eq!(shards[1].as_deref(), Some(data[1].as_slice()));
    }

    #[test]
    fn rejects_bad_params() {
        assert!(BlockCoder::new(0, 1).is_err());
        assert!(BlockCoder::new(60, 8).is_err());
        let coder = BlockCoder::new(2, 1).unwrap();
        assert_eq!(
            coder.parity(&[vec![1], vec![1, 2]]),
            Err(ErasureError::ShardLength)
        );
    }
}
