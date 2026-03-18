use std::net::SocketAddr;
use std::str::FromStr;

use rand::RngCore;
use rusqlite::{Connection, Result, params};
use serde_json;

use kaonic_vpn::{GatewayConfig, KaonicCtrlConfig};

const DEFAULT_NETWORK: &str = "10.20.0.0/16";
const DEFAULT_ANNOUNCE_FREQ_SECS: u32 = 1;

pub struct Database {
    conn: Connection,
}

impl Database {
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
            );",
        )
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        Ok(if let Some(row) = rows.next()? { Some(row.get(0)?) } else { None })
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn load_or_create_seed(&self) -> Result<String> {
        self.load_or_create_named_seed("identity_seed")
    }

    pub fn load_or_create_named_seed(&self, key: &str) -> Result<String> {
        if let Some(seed) = self.get(key)? {
            return Ok(seed);
        }
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let seed = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        self.set(key, &seed)?;
        log::info!("generated new seed for '{key}'");
        Ok(seed)
    }

    pub fn load_config(&self) -> Result<GatewayConfig> {
        let network_str = self.get("network")?.unwrap_or_else(|| DEFAULT_NETWORK.to_string());
        let network = cidr::Ipv4Cidr::from_str(&network_str).map_err(|e| {
            rusqlite::Error::InvalidParameterName(format!("invalid network '{network_str}': {e}"))
        })?;

        let announce_freq_secs = self.get("announce_freq_secs")?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(DEFAULT_ANNOUNCE_FREQ_SECS);

        let peers = {
            let mut stmt = self.conn.prepare("SELECT destination_hash FROM peers ORDER BY destination_hash")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<Result<Vec<String>>>()?
        };

        let default_ctrl = KaonicCtrlConfig::default();
        let mut kaonic_ctrl_configs = Vec::new();
        for module_idx in 0usize..2 {
            let suffix = format!("_{module_idx}");
            // Load listen/server — fall back to legacy unsuffixed keys for module 0, then to defaults.
            let listen_str = self.get(&format!("kaonic_ctrl_listen_addr{suffix}"))?.or_else(|| {
                if module_idx == 0 { self.get("kaonic_ctrl_listen_addr").ok()? } else { None }
            });
            let server_str = self.get(&format!("kaonic_ctrl_server_addr{suffix}"))?.or_else(|| {
                if module_idx == 0 { self.get("kaonic_ctrl_server_addr").ok()? } else { None }
            });
            let listen_addr = listen_str.and_then(|s| SocketAddr::from_str(&s).ok())
                .unwrap_or(default_ctrl.listen_addr);
            let server_addr = server_str.and_then(|s| SocketAddr::from_str(&s).ok())
                .unwrap_or(default_ctrl.server_addr);

            let radio_config_key = format!("kaonic_ctrl_radio_config{suffix}");
            let modulation_key   = format!("kaonic_ctrl_modulation{suffix}");
            let radio_config = self.get(&radio_config_key)?
                .or_else(|| if module_idx == 0 { self.get("kaonic_ctrl_radio_config").ok()? } else { None })
                .and_then(|v| serde_json::from_str(&v).ok());
            let modulation = self.get(&modulation_key)?
                .or_else(|| if module_idx == 0 { self.get("kaonic_ctrl_modulation").ok()? } else { None })
                .and_then(|v| serde_json::from_str(&v).ok());

            // Always include module 0; include module 1 only if it has radio settings saved.
            if module_idx == 0 || radio_config.is_some() || modulation.is_some() {
                kaonic_ctrl_configs.push(KaonicCtrlConfig {
                    listen_addr,
                    server_addr,
                    module: module_idx,
                    radio_config,
                    modulation,
                });
            }
        }

        Ok(GatewayConfig { network, peers, announce_freq_secs, kaonic_ctrl_configs })
    }

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

        for ctrl in &config.kaonic_ctrl_configs {
            self.save_module_config(ctrl)?;
        }

        Ok(())
    }

    /// Save a single module's radio config (keyed by `ctrl.module` index).
    pub fn save_module_config(&self, ctrl: &KaonicCtrlConfig) -> Result<()> {
        let suffix = format!("_{}", ctrl.module);
        self.set(&format!("kaonic_ctrl_listen_addr{suffix}"), &ctrl.listen_addr.to_string())?;
        self.set(&format!("kaonic_ctrl_server_addr{suffix}"), &ctrl.server_addr.to_string())?;
        if let Some(rc) = &ctrl.radio_config {
            self.set(&format!("kaonic_ctrl_radio_config{suffix}"), &serde_json::to_string(rc).unwrap())?;
        }
        if let Some(m) = &ctrl.modulation {
            self.set(&format!("kaonic_ctrl_modulation{suffix}"), &serde_json::to_string(m).unwrap())?;
        }
        Ok(())
    }
}
