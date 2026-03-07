use std::net::SocketAddr;
use std::str::FromStr;

use rusqlite::{Connection, Result, params};

use crate::config::{FrameRecord, GatewayConfig, KaonicCtrlConfig};

const DEFAULT_NETWORK: &str = "10.20.0.0/16";
const DEFAULT_ANNOUNCE_FREQ_SECS: u32 = 1;

/// SQLite-backed persistent settings store.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (or create) the database at `path` and run schema migrations.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS peers (
                destination_hash TEXT PRIMARY KEY
            );
            CREATE TABLE IF NOT EXISTS received_frames (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                module    INTEGER NOT NULL,
                size      INTEGER NOT NULL,
                data_hex  TEXT    NOT NULL
            );",
        )
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    fn del(&self, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![key],
        )?;
        Ok(())
    }

    /// Load the full gateway config from the database.
    /// Missing settings fall back to sensible defaults.
    pub fn load_config(&self) -> Result<GatewayConfig> {
        let network_str = self
            .get("network")?
            .unwrap_or_else(|| DEFAULT_NETWORK.to_string());
        let network = cidr::Ipv4Cidr::from_str(&network_str).map_err(|e| {
            rusqlite::Error::InvalidParameterName(format!(
                "invalid network '{network_str}': {e}"
            ))
        })?;

        let announce_freq_secs = self
            .get("announce_freq_secs")?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(DEFAULT_ANNOUNCE_FREQ_SECS);

        let peers = {
            let mut stmt = self
                .conn
                .prepare("SELECT destination_hash FROM peers ORDER BY destination_hash")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<Result<Vec<String>>>()?
        };

        let kaonic_ctrl_config = {
            let listen = self.get("kaonic_ctrl_listen_addr")?;
            let server = self.get("kaonic_ctrl_server_addr")?;
            let module = self.get("kaonic_ctrl_module")?;
            match (listen, server) {
                (Some(l), Some(s)) => {
                    let listen_addr = SocketAddr::from_str(&l).map_err(|e| {
                        rusqlite::Error::InvalidParameterName(format!(
                            "invalid kaonic_ctrl_listen_addr '{l}': {e}"
                        ))
                    })?;
                    let server_addr = SocketAddr::from_str(&s).map_err(|e| {
                        rusqlite::Error::InvalidParameterName(format!(
                            "invalid kaonic_ctrl_server_addr '{s}': {e}"
                        ))
                    })?;
                    let module_idx = module
                        .and_then(|m| m.parse::<usize>().ok())
                        .unwrap_or(0);
                    let radio_config = self
                        .get("radio_config")?
                        .and_then(|v| serde_json::from_str(&v).ok());
                    let modulation = self
                        .get("modulation")?
                        .and_then(|v| serde_json::from_str(&v).ok());
                    Some(KaonicCtrlConfig {
                        listen_addr,
                        server_addr,
                        module: module_idx,
                        radio_config,
                        modulation,
                    })
                }
                _ => None,
            }
        };

        Ok(GatewayConfig {
            network,
            peers,
            announce_freq_secs,
            kaonic_ctrl_config,
        })
    }

    /// Persist the full gateway config to the database.
    pub fn save_config(&self, config: &GatewayConfig) -> Result<()> {
        self.set("network", &config.network.to_string())?;
        self.set("announce_freq_secs", &config.announce_freq_secs.to_string())?;

        self.conn.execute("DELETE FROM peers", [])?;
        for peer in &config.peers {
            self.conn.execute(
                "INSERT OR IGNORE INTO peers (destination_hash) VALUES (?1)",
                params![peer],
            )?;
        }

        if let Some(ctrl) = &config.kaonic_ctrl_config {
            self.set("kaonic_ctrl_listen_addr", &ctrl.listen_addr.to_string())?;
            self.set("kaonic_ctrl_server_addr", &ctrl.server_addr.to_string())?;
            self.set("kaonic_ctrl_module", &ctrl.module.to_string())?;
            match &ctrl.radio_config {
                Some(rc) => self.set("radio_config", &serde_json::to_string(rc).unwrap())?,
                None => self.del("radio_config")?,
            }
            match &ctrl.modulation {
                Some(m) => self.set("modulation", &serde_json::to_string(m).unwrap())?,
                None => self.del("modulation")?,
            }
        }

        Ok(())
    }

    /// Append a received frame to the log, keeping at most `keep` rows.
    pub fn log_frame(&self, record: &FrameRecord, keep: usize) -> Result<()> {
        self.conn.execute(
            "INSERT INTO received_frames (timestamp, module, size, data_hex)
             VALUES (?1, ?2, ?3, ?4)",
            params![record.timestamp, record.module as i64, record.size as i64, record.data_hex],
        )?;
        self.conn.execute(
            "DELETE FROM received_frames WHERE id NOT IN (
                SELECT id FROM received_frames ORDER BY id DESC LIMIT ?1
             )",
            params![keep as i64],
        )?;
        Ok(())
    }

    /// Return the most recent `limit` frames, newest first.
    pub fn recent_frames(&self, limit: usize) -> Result<Vec<FrameRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, module, size, data_hex
             FROM received_frames ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(FrameRecord {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                module: row.get::<_, i64>(2)? as usize,
                size: row.get::<_, i64>(3)? as usize,
                data_hex: row.get(4)?,
            })
        })?;
        rows.collect()
    }
}
