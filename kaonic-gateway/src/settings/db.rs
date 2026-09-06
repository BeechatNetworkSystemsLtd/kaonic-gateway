use std::str::FromStr;

use rand::RngCore;
use rusqlite::{params, Connection, Result};
use serde_json;

use crate::config::GatewayConfig;
use crate::radio::{HardwareRadioConfig, RadioModuleConfig};

const DEFAULT_NETWORK: &str = "10.20.0.0/16";
const DEFAULT_ANNOUNCE_FREQ_SECS: u32 = 5;
const DEFAULT_ADVERTISED_ROUTES: &str = "[\"192.168.10.0/24\"]";
const DEFAULT_ALLOW_ALL_PEERS: bool = true;

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
            );
            CREATE TABLE IF NOT EXISTS remote_paired (
                identity_hash TEXT PRIMARY KEY,
                identity_hex  TEXT NOT NULL,
                codename      TEXT NOT NULL,
                paired_at     INTEGER NOT NULL,
                permissions   INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS remote_tags (
                identity_hash TEXT PRIMARY KEY,
                tag           TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS wifi_networks (
                ssid       TEXT PRIMARY KEY,
                psk        TEXT NOT NULL,
                priority   INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL DEFAULT 0,
                last_used  INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS remote_pairing_requests (
                identity_hash TEXT NOT NULL,
                direction     TEXT NOT NULL,
                identity_hex  TEXT NOT NULL,
                codename      TEXT NOT NULL,
                state         TEXT NOT NULL,
                detail        TEXT NOT NULL,
                ts            INTEGER NOT NULL,
                retries       INTEGER NOT NULL,
                PRIMARY KEY (identity_hash, direction)
            );",
        )
    }

    // ── Remote control trust store ───────────────────────────────────────────

    pub fn load_remote_paired(&self) -> Result<Vec<kaonic_remote::PairedNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT identity_hash, identity_hex, codename, paired_at, permissions
             FROM remote_paired ORDER BY paired_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(kaonic_remote::PairedNode {
                identity_hash: row.get(0)?,
                identity_hex: row.get(1)?,
                codename: row.get(2)?,
                paired_at: row.get::<_, i64>(3)? as u64,
                permissions: row.get::<_, i64>(4)? as u32,
            })
        })?;
        rows.collect()
    }

    pub fn save_remote_paired(&self, node: &kaonic_remote::PairedNode) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO remote_paired
             (identity_hash, identity_hex, codename, paired_at, permissions)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                node.identity_hash,
                node.identity_hex,
                node.codename,
                node.paired_at as i64,
                node.permissions as i64
            ],
        )?;
        Ok(())
    }

    pub fn remove_remote_paired(&self, identity_hash: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM remote_paired WHERE identity_hash = ?1",
            params![identity_hash],
        )?;
        Ok(())
    }

    pub fn load_remote_requests(&self) -> Result<Vec<kaonic_remote::PairingRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT identity_hash, direction, identity_hex, codename, state, detail, ts, retries
             FROM remote_pairing_requests ORDER BY ts",
        )?;
        let rows = stmt.query_map([], |row| {
            let direction: String = row.get(1)?;
            Ok((
                direction,
                kaonic_remote::PairingRecord {
                    identity_hash: row.get(0)?,
                    identity_hex: row.get(2)?,
                    codename: row.get(3)?,
                    direction: kaonic_remote::PairingDirection::Outgoing,
                    state: row.get(4)?,
                    detail: row.get(5)?,
                    ts: row.get::<_, i64>(6)? as u64,
                    retries: row.get::<_, i64>(7)? as u32,
                },
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (direction, mut record) = row?;
            let Some(direction) = kaonic_remote::PairingDirection::parse(&direction) else {
                continue;
            };
            record.direction = direction;
            records.push(record);
        }
        Ok(records)
    }

    pub fn save_remote_request(&self, record: &kaonic_remote::PairingRecord) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO remote_pairing_requests
             (identity_hash, direction, identity_hex, codename, state, detail, ts, retries)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                record.identity_hash,
                record.direction.as_str(),
                record.identity_hex,
                record.codename,
                record.state,
                record.detail,
                record.ts as i64,
                record.retries as i64
            ],
        )?;
        Ok(())
    }

    pub fn remove_remote_request(
        &self,
        identity_hash: &str,
        direction: kaonic_remote::PairingDirection,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM remote_pairing_requests WHERE identity_hash = ?1 AND direction = ?2",
            params![identity_hash, direction.as_str()],
        )?;
        Ok(())
    }

    // ── Operator labels for remote nodes ─────────────────────────────────────

    pub fn load_remote_tags(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT identity_hash, tag FROM remote_tags")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn set_remote_tag(&self, identity_hash: &str, tag: &str) -> Result<()> {
        if tag.is_empty() {
            self.conn.execute(
                "DELETE FROM remote_tags WHERE identity_hash = ?1",
                params![identity_hash],
            )?;
        } else {
            self.conn.execute(
                "INSERT OR REPLACE INTO remote_tags (identity_hash, tag) VALUES (?1, ?2)",
                params![identity_hash, tag],
            )?;
        }
        Ok(())
    }

    // ── Saved Wi-Fi networks ─────────────────────────────────────────────────

    pub fn load_wifi_networks(&self) -> Result<Vec<crate::network::SavedWifiNetwork>> {
        let mut stmt = self.conn.prepare(
            "SELECT ssid, psk, priority, created_at, last_used FROM wifi_networks
             ORDER BY priority DESC, last_used DESC, ssid",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::network::SavedWifiNetwork {
                ssid: row.get(0)?,
                psk: row.get(1)?,
                priority: row.get::<_, i64>(2)? as i32,
                created_at: row.get::<_, i64>(3)? as u64,
                last_used: row.get::<_, i64>(4)? as u64,
            })
        })?;
        rows.collect()
    }

    pub fn save_wifi_network(&self, network: &crate::network::SavedWifiNetwork) -> Result<()> {
        self.conn.execute(
            "INSERT INTO wifi_networks (ssid, psk, priority, created_at, last_used)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(ssid) DO UPDATE SET psk = ?2, priority = ?3",
            params![
                network.ssid,
                network.psk,
                network.priority as i64,
                network.created_at as i64,
                network.last_used as i64
            ],
        )?;
        Ok(())
    }

    pub fn remove_wifi_network(&self, ssid: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM wifi_networks WHERE ssid = ?1", params![ssid])?;
        Ok(())
    }

    pub fn touch_wifi_network(&self, ssid: &str, when: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE wifi_networks SET last_used = ?2 WHERE ssid = ?1",
            params![ssid, when as i64],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.get(key)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.set(key, value)
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        Ok(if let Some(row) = rows.next()? {
            Some(row.get(0)?)
        } else {
            None
        })
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
        let network_str = self
            .get("network")?
            .unwrap_or_else(|| DEFAULT_NETWORK.to_string());
        let network = cidr::Ipv4Cidr::from_str(&network_str).map_err(|e| {
            rusqlite::Error::InvalidParameterName(format!("invalid network '{network_str}': {e}"))
        })?;

        let announce_freq_secs = self
            .get("announce_freq_secs")?
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(DEFAULT_ANNOUNCE_FREQ_SECS);
        let allow_all_peers = self
            .get("allow_all_peers")?
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(DEFAULT_ALLOW_ALL_PEERS);

        let peers = {
            let mut stmt = self
                .conn
                .prepare("SELECT destination_hash FROM peers ORDER BY destination_hash")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<Result<Vec<String>>>()?
        };

        let advertised_routes = self
            .get("advertised_routes")?
            .unwrap_or_else(|| DEFAULT_ADVERTISED_ROUTES.to_string());
        let advertised_routes = serde_json::from_str(&advertised_routes).unwrap_or_default();

        let defaults = HardwareRadioConfig::default();
        let module_configs = std::array::from_fn(|i| {
            let suffix = format!("_{i}");
            let radio_config = self
                .get(&format!("kaonic_ctrl_radio_config{suffix}"))
                .ok()
                .flatten()
                .or_else(|| {
                    if i == 0 {
                        self.get("kaonic_ctrl_radio_config").ok()?
                    } else {
                        None
                    }
                })
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_else(|| defaults.module_configs[i].radio_config.clone());
            let modulation = self
                .get(&format!("kaonic_ctrl_modulation{suffix}"))
                .ok()
                .flatten()
                .or_else(|| {
                    if i == 0 {
                        self.get("kaonic_ctrl_modulation").ok()?
                    } else {
                        None
                    }
                })
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_else(|| defaults.module_configs[i].modulation.clone());
            let accelerator = self
                .get(&format!("kaonic_ctrl_accelerator{suffix}"))
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_else(|| defaults.module_configs[i].accelerator);
            let antenna = self
                .get(&format!("kaonic_ctrl_antenna{suffix}"))
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_else(|| defaults.module_configs[i].antenna);
            RadioModuleConfig {
                radio_config,
                modulation,
                accelerator,
                antenna,
            }
        });

        Ok(GatewayConfig {
            network,
            allow_all_peers,
            peers,
            advertised_routes,
            announce_freq_secs,
            radio: HardwareRadioConfig { module_configs },
        })
    }

    pub fn save_config(&self, config: &GatewayConfig) -> Result<()> {
        self.set("network", &config.network.to_string())?;
        self.set(
            "advertised_routes",
            &serde_json::to_string(&config.advertised_routes).unwrap(),
        )?;
        self.set("allow_all_peers", &config.allow_all_peers.to_string())?;
        self.set("announce_freq_secs", &config.announce_freq_secs.to_string())?;

        self.conn.execute("DELETE FROM peers", [])?;
        for peer in &config.peers {
            self.conn.execute(
                "INSERT OR IGNORE INTO peers (destination_hash) VALUES (?1)",
                params![peer],
            )?;
        }

        for (i, module_cfg) in config.radio.module_configs.iter().enumerate() {
            self.save_module_config(i, module_cfg)?;
        }

        Ok(())
    }

    /// Save a single module's radio+modulation config by module index.
    pub fn save_module_config(&self, module: usize, cfg: &RadioModuleConfig) -> Result<()> {
        let suffix = format!("_{module}");
        self.set(
            &format!("kaonic_ctrl_radio_config{suffix}"),
            &serde_json::to_string(&cfg.radio_config).unwrap(),
        )?;
        self.set(
            &format!("kaonic_ctrl_modulation{suffix}"),
            &serde_json::to_string(&cfg.modulation).unwrap(),
        )?;
        self.set(
            &format!("kaonic_ctrl_accelerator{suffix}"),
            &serde_json::to_string(&cfg.accelerator).unwrap(),
        )?;
        self.set(
            &format!("kaonic_ctrl_antenna{suffix}"),
            &serde_json::to_string(&cfg.antenna).unwrap(),
        )?;
        Ok(())
    }
}

