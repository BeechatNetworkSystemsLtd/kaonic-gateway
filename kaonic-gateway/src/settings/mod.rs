mod db;

use db::Database;
use rusqlite::Result;

use crate::config::GatewayConfig;
use crate::radio::RadioModuleConfig;

pub struct Settings {
    db: Database,
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

    pub fn load_or_create_codename(&self) -> Result<String> {
        self.db.load_or_create_codename()
    }

    pub fn save_codename(&self, codename: &str) -> Result<()> {
        self.db.save_codename(codename)
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
