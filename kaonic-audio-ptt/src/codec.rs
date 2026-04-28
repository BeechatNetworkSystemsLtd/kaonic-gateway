use codec2::{Codec2, Codec2Mode};

use crate::config::PluginConfig;

pub const CODEC2_MODE: Codec2Mode = Codec2Mode::MODE_3200;

pub struct TxCodec {
    inner: Codec2,
    samples_per_frame: usize,
    bytes_per_frame: usize,
    scratch: Vec<u8>,
}

impl TxCodec {
    pub fn new(_cfg: &PluginConfig) -> Result<Self, String> {
        let inner = Codec2::new(CODEC2_MODE);
        let samples_per_frame = inner.samples_per_frame();
        let bytes_per_frame = (inner.bits_per_frame() + 7) / 8;
        Ok(Self {
            inner,
            samples_per_frame,
            bytes_per_frame,
            scratch: vec![0u8; bytes_per_frame],
        })
    }

    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.bytes_per_frame
    }

    /// Encode one Codec2 frame from `pcm` (must be `samples_per_frame` long) and
    /// append the compressed bytes to `out`. Re-uses an internal scratch buffer
    /// so the only allocation is the `extend_from_slice` into `out`.
    pub fn encode_into(&mut self, pcm: &[i16], out: &mut Vec<u8>) -> Result<(), String> {
        if pcm.len() != self.samples_per_frame {
            return Err(format!(
                "codec2 expects {} pcm samples per frame, got {}",
                self.samples_per_frame,
                pcm.len()
            ));
        }
        self.inner.encode(&mut self.scratch, pcm);
        out.extend_from_slice(&self.scratch);
        Ok(())
    }
}

pub struct RxCodec {
    inner: Codec2,
    samples_per_frame: usize,
    bytes_per_frame: usize,
}

impl RxCodec {
    pub fn new(_cfg: &PluginConfig) -> Result<Self, String> {
        let inner = Codec2::new(CODEC2_MODE);
        let samples_per_frame = inner.samples_per_frame();
        let bytes_per_frame = (inner.bits_per_frame() + 7) / 8;
        Ok(Self {
            inner,
            samples_per_frame,
            bytes_per_frame,
        })
    }

    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.bytes_per_frame
    }

    /// Decode one Codec2 frame in place into `out` (must be `samples_per_frame` long).
    pub fn decode_into(&mut self, encoded: &[u8], out: &mut [i16]) -> Result<(), String> {
        if encoded.len() != self.bytes_per_frame {
            return Err(format!(
                "codec2 expects {} encoded bytes per frame, got {}",
                self.bytes_per_frame,
                encoded.len()
            ));
        }
        if out.len() != self.samples_per_frame {
            return Err(format!(
                "codec2 decode_into expects {} sample slots, got {}",
                self.samples_per_frame,
                out.len()
            ));
        }
        self.inner.decode(out, encoded);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_silence_through_codec2() {
        let cfg = PluginConfig::default();
        let mut tx = TxCodec::new(&cfg).expect("tx codec");
        let mut rx = RxCodec::new(&cfg).expect("rx codec");
        assert_eq!(tx.samples_per_frame(), rx.samples_per_frame());
        assert_eq!(tx.bytes_per_frame(), rx.bytes_per_frame());

        let pcm = vec![0i16; tx.samples_per_frame()];
        let mut encoded = Vec::with_capacity(tx.bytes_per_frame());
        tx.encode_into(&pcm, &mut encoded).expect("encode");
        assert_eq!(encoded.len(), tx.bytes_per_frame());

        let mut out = vec![0i16; rx.samples_per_frame()];
        rx.decode_into(&encoded, &mut out).expect("decode");
    }

    #[test]
    fn rejects_wrong_pcm_size() {
        let cfg = PluginConfig::default();
        let mut tx = TxCodec::new(&cfg).expect("tx codec");
        let mut buf = Vec::new();
        assert!(tx.encode_into(&[0i16; 4], &mut buf).is_err());
    }

    #[test]
    fn rejects_wrong_encoded_size() {
        let cfg = PluginConfig::default();
        let mut rx = RxCodec::new(&cfg).expect("rx codec");
        let mut out = vec![0i16; rx.samples_per_frame()];
        assert!(rx.decode_into(&[0u8; 4], &mut out).is_err());
    }
}
