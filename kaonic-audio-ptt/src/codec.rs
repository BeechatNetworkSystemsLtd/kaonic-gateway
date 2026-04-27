use crate::config::PluginConfig;

pub struct TxCodec;

pub struct RxCodec;

impl TxCodec {
    pub fn new(cfg: &PluginConfig) -> Result<Self, String> {
        let _ = cfg;
        Ok(Self)
    }

    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(pcm.len() * 2);
        for sample in pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        Ok(out)
    }
}

impl RxCodec {
    pub fn new(cfg: &PluginConfig) -> Result<Self, String> {
        let _ = cfg;
        Ok(Self)
    }

    pub fn decode(&mut self, payload: &[u8], frame_samples: usize) -> Result<Vec<i16>, String> {
        if payload.len() % 2 != 0 {
            return Err("PCM audio payload is not aligned to i16 samples".into());
        }
        let pcm = payload
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        if pcm.len() != frame_samples {
            return Err(format!(
                "PCM audio frame must contain exactly {frame_samples} samples, got {}",
                pcm.len()
            ));
        }
        Ok(pcm)
    }
}
