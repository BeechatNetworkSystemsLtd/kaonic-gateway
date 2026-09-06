mod db;

use std::io::{self, Write};
use std::path::Path;

use db::Database;
use rand::RngCore;
use rusqlite::Result;

use crate::config::GatewayConfig;
use crate::radio::RadioModuleConfig;

const CODENAME_PATH: &str = "/etc/kaonic/codename";
const CODENAME_LEN: usize = 8;
const CODENAME_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

pub struct Settings {
    db: Database,
}

fn generate_codename() -> String {
    let mut bytes = [0u8; CODENAME_LEN];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
        .into_iter()
        .map(|byte| CODENAME_ALPHABET[(byte as usize) % CODENAME_ALPHABET.len()] as char)
        .collect()
}

pub fn normalize_codename(value: &str) -> std::result::Result<String, &'static str> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != 8 {
        return Err("Codename must be exactly 8 characters.");
    }
    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    {
        return Err("Codename must use only letters and digits.");
    }
    Ok(normalized)
}

impl Settings {
    pub fn open(path: &str) -> Result<Self> {
        Ok(Self {
            db: Database::open(path)?,
        })
    }

    pub fn load_or_create_seed(&self) -> Result<String> {
        self.db.load_or_create_seed()
    }

    pub fn load_or_create_named_seed(&self, key: &str) -> Result<String> {
        self.db.load_or_create_named_seed(key)
    }

    pub fn load_or_create_codename(&self) -> io::Result<String> {
        match std::fs::read_to_string(CODENAME_PATH) {
            Ok(contents) => {
                let codename = contents.trim().to_string();
                if !codename.is_empty() {
                    return Ok(codename);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let codename = generate_codename();
        self.save_codename(&codename)?;
        log::info!("generated system codename '{codename}'");
        Ok(codename)
    }

    pub fn save_codename(&self, codename: &str) -> io::Result<()> {
        if let Some(parent) = Path::new(CODENAME_PATH).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(CODENAME_PATH)?;
        file.write_all(codename.as_bytes())?;
        Ok(())
    }

    pub fn load_config(&self) -> Result<GatewayConfig> {
        self.db.load_config()
    }

    pub fn save_config(&self, config: &GatewayConfig) -> Result<()> {
        self.db.save_config(config)
    }

    pub fn save_module_config(&self, module: usize, cfg: &RadioModuleConfig) -> Result<()> {
        self.db.save_module_config(module, cfg)
    }

    pub fn load_remote_paired(&self) -> Result<Vec<kaonic_remote::PairedNode>> {
        self.db.load_remote_paired()
    }

    pub fn save_remote_paired(&self, node: &kaonic_remote::PairedNode) -> Result<()> {
        self.db.save_remote_paired(node)
    }

    pub fn remove_remote_paired(&self, identity_hash: &str) -> Result<()> {
        self.db.remove_remote_paired(identity_hash)
    }

    pub fn load_remote_requests(&self) -> Result<Vec<kaonic_remote::PairingRecord>> {
        self.db.load_remote_requests()
    }

    pub fn save_remote_request(&self, record: &kaonic_remote::PairingRecord) -> Result<()> {
        self.db.save_remote_request(record)
    }

    pub fn remove_remote_request(
        &self,
        identity_hash: &str,
        direction: kaonic_remote::PairingDirection,
    ) -> Result<()> {
        self.db.remove_remote_request(identity_hash, direction)
    }

    pub fn load_remote_tags(&self) -> Result<Vec<(String, String)>> {
        self.db.load_remote_tags()
    }

    pub fn set_remote_tag(&self, identity_hash: &str, tag: &str) -> Result<()> {
        self.db.set_remote_tag(identity_hash, tag)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.db.get_setting(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.db.set_setting(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_codename;

    #[test]
    fn codename_normalizes_to_lowercase() {
        assert_eq!(normalize_codename(" AbCD1234 ").unwrap(), "abcd1234");
    }

    #[test]
    fn codename_rejects_invalid_values() {
        assert!(normalize_codename("short").is_err());
        assert!(normalize_codename("abcd_123").is_err());
    }
}
