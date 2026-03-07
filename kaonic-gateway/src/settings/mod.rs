mod db;

use db::Database;
use rusqlite::Result;

use kaonic_vpn::GatewayConfig;

/// Persistent gateway settings backed by a SQLite database.
pub struct Settings {
    db: Database,
}

impl Settings {
    /// Open (or create) the settings database at `path`.
    pub fn open(path: &str) -> Result<Self> {
        Ok(Self {
            db: Database::open(path)?,
        })
    }

    /// Load the full gateway config from the database.
    /// Missing settings fall back to sensible defaults.
    pub fn load_config(&self) -> Result<GatewayConfig> {
        self.db.load_config()
    }

    /// Persist the full gateway config to the database.
    pub fn save_config(&self, config: &GatewayConfig) -> Result<()> {
        self.db.save_config(config)
    }
}
