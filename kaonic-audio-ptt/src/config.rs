use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use rand::RngCore;
use reticulum::hash::AddressHash;
use serde::{Deserialize, Serialize};

pub const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8780";
pub const DEFAULT_CTRL_SERVER: &str = "192.168.10.1:9090";
pub const DEFAULT_RNS_MODULE: usize = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub http_bind: SocketAddr,
    #[serde(default = "default_ctrl_server")]
    pub kaonic_ctrl_server: SocketAddr,
    #[serde(default = "default_rns_module")]
    pub rns_module: usize,
    #[serde(default, skip_serializing)]
    pub identity_seed: String,
    #[serde(default)]
    pub selected_peer: Option<String>,
    pub capture_device: String,
    pub playback_device: String,
    pub frame_ms: u32,
    pub sample_rate_hz: u32,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            http_bind: DEFAULT_HTTP_BIND.parse().expect("default HTTP bind"),
            kaonic_ctrl_server: default_ctrl_server(),
            rns_module: default_rns_module(),
            identity_seed: generate_identity_seed(),
            selected_peer: None,
            capture_device: "default".into(),
            playback_device: "default".into(),
            frame_ms: 20,
            sample_rate_hz: 16_000,
        }
    }
}

impl PluginConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.rns_module > 1 {
            return Err("rns_module must be 0 or 1".into());
        }
        if self.frame_ms != 20 {
            return Err("only 20ms audio framing is currently supported".into());
        }
        if self.sample_rate_hz != 16_000 {
            return Err("only 16kHz audio is currently supported".into());
        }
        if self.identity_seed.len() != 64
            || !self.identity_seed.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err("identity_seed must be a 64-character hex string".into());
        }
        if let Some(selected_peer) = &self.selected_peer {
            normalize_selected_peer(Some(selected_peer.clone()))?;
        }
        Ok(())
    }
}

fn default_ctrl_server() -> SocketAddr {
    DEFAULT_CTRL_SERVER
        .parse()
        .expect("default kaonic-ctrl server")
}

fn default_rns_module() -> usize {
    DEFAULT_RNS_MODULE
}

pub fn resolve_config_path(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Ok(path) = std::env::var("KAONIC_AUDIO_PTT_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().map_err(|err| format!("resolve current exe: {err}"))?;
    let current_dir = exe
        .parent()
        .ok_or_else(|| "resolve current exe parent".to_string())?;
    let plugin_dir = current_dir
        .parent()
        .ok_or_else(|| "resolve plugin dir from current exe".to_string())?;
    Ok(plugin_dir.join("config.toml"))
}

pub fn load_or_create_config(path: &Path) -> Result<PluginConfig, String> {
    if path.is_file() {
        let raw = fs::read_to_string(path).map_err(|err| format!("read config: {err}"))?;
        let mut cfg: PluginConfig = toml::from_str(&raw)
            .map_err(|err| format!("parse config {}: {err}", path.display()))?;
        if cfg.identity_seed.is_empty() {
            cfg.identity_seed = generate_identity_seed();
            save_config(path, &cfg)?;
        }
        cfg.validate()?;
        return Ok(cfg);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create config dir: {err}"))?;
    }
    let cfg = PluginConfig::default();
    save_config(path, &cfg)?;
    Ok(cfg)
}

pub fn save_config(path: &Path, cfg: &PluginConfig) -> Result<(), String> {
    cfg.validate()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create config dir: {err}"))?;
    }
    let raw = toml::to_string_pretty(cfg).map_err(|err| format!("serialize config: {err}"))?;
    fs::write(path, raw).map_err(|err| format!("write config {}: {err}", path.display()))
}

pub fn normalize_selected_peer(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let raw = value.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    AddressHash::new_from_hex_string(raw)
        .map(|hash| Some(hash.to_hex_string()))
        .map_err(|err| format!("invalid selected_peer '{raw}': {err:?}"))
}

fn generate_identity_seed() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_supported_audio_format() {
        let cfg = PluginConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_unsupported_frame_size() {
        let mut cfg = PluginConfig::default();
        cfg.frame_ms = 40;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn normalizes_selected_peer_hash() {
        assert_eq!(
            normalize_selected_peer(Some("0123456789ABCDEF0123456789ABCDEF".into())).unwrap(),
            Some("0123456789abcdef0123456789abcdef".into())
        );
    }
}
