mod db;

use db::Database;
use rusqlite::Result;

use crate::config::{FrameRecord, GatewayConfig};

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

    /// Return the Reticulum identity seed stored in the DB, creating one if absent.
    /// Pass the returned string to `PrivateIdentity::new_from_name()`.
    pub fn load_or_create_seed(&self) -> Result<String> {
        self.db.load_or_create_seed()
    }

    /// Log a received frame; keeps at most `keep` rows in the DB.
    pub fn log_frame(&self, record: &FrameRecord, keep: usize) -> Result<()> {
        self.db.log_frame(record, keep)
    }

    /// Return the most recent `limit` frames, newest first.
    pub fn recent_frames(&self, limit: usize) -> Result<Vec<FrameRecord>> {
        self.db.recent_frames(limit)
    }
}
