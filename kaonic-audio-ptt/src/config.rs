use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8780";
pub const DEFAULT_MEDIA_BIND: &str = "0.0.0.0:6790";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    pub http_bind: SocketAddr,
    pub media_bind: SocketAddr,
    pub remote_peer: Option<SocketAddr>,
    pub capture_device: String,
    pub playback_device: String,
    pub frame_ms: u32,
    pub sample_rate_hz: u32,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            http_bind: DEFAULT_HTTP_BIND.parse().expect("default HTTP bind"),
            media_bind: DEFAULT_MEDIA_BIND.parse().expect("default media bind"),
            remote_peer: None,
            capture_device: "default".into(),
            playback_device: "default".into(),
            frame_ms: 20,
            sample_rate_hz: 16_000,
        }
    }
}

impl PluginConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.frame_ms != 20 {
            return Err("only 20ms audio framing is currently supported".into());
        }
        if self.sample_rate_hz != 16_000 {
            return Err("only 16kHz audio is currently supported".into());
        }
        Ok(())
    }
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
        let cfg: PluginConfig = toml::from_str(&raw)
            .map_err(|err| format!("parse config {}: {err}", path.display()))?;
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
}
