mod db;

use db::Database;
use rusqlite::Result;

use crate::config::GatewayConfig;
use crate::radio::RadioModuleConfig;

pub struct Settings {
    db: Database,
}

impl Settings {
    pub fn open(path: &str) -> Result<Self> {
        Ok(Self { db: Database::open(path)? })
    }

    pub fn load_or_create_seed(&self) -> Result<String> {
        self.db.load_or_create_seed()
    }

    pub fn load_or_create_named_seed(&self, key: &str) -> Result<String> {
        self.db.load_or_create_named_seed(key)
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
