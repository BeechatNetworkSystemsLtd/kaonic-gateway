//! kaonic-gateway: Reticulum VPN gateway using kaonic radio hardware

mod http;

use std::fs;
use std::process;
use std::sync::{Arc, Mutex};

use clap::Parser;
use ed25519_dalek;
use env_logger;
use kaonic_gateway::settings::Settings;
use kaonic_vpn::{FrameRecord, KaonicCtrlConfig};
use http::SharedSettings;
use log;
use pem;
use rand::RngCore;
use reticulum::identity::PrivateIdentity;
use tokio;
use x25519_dalek;

const DEFAULT_DB_PATH: &str = "kaonic-gateway.db";

/// Loads an X25519 private key from `path`, generating and saving it first if the file
/// does not yet exist.
fn generate_or_load_x25519(path: &str) -> Result<x25519_dalek::StaticSecret, process::ExitCode> {
    if !std::path::Path::new(path).exists() {
        log::info!("privkey not found, generating new X25519 key at {path}");
        let mut key_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key_bytes);
        // Minimal PKCS#8 DER encoding for X25519 (OID 1.3.101.110).
        // The existing loader takes the last 32 bytes of the DER, so this layout must match.
        let header: &[u8] = &[
            0x30, 0x2e,                   // SEQUENCE (46 bytes)
            0x02, 0x01, 0x00,             // INTEGER 0  (version)
            0x30, 0x05, 0x06, 0x03,
            0x2b, 0x65, 0x6e,             // OID 1.3.101.110
            0x04, 0x22, 0x04, 0x20,       // OCTET STRING { OCTET STRING (32 bytes) }
        ];
        let mut der = header.to_vec();
        der.extend_from_slice(&key_bytes);
        let pem_obj = pem::Pem::new("PRIVATE KEY", der);
        fs::write(path, pem::encode(&pem_obj)).map_err(|err| {
            log::error!("failed to write privkey {path}: {err:?}");
            process::ExitCode::FAILURE
        })?;
    }
    let pem_data = fs::read(path).map_err(|err| {
        log::error!("failed to read privkey {path}: {err:?}");
        process::ExitCode::FAILURE
    })?;
    let pem = pem::parse(pem_data).map_err(|err| {
        log::error!("failed to parse privkey {path}: {err:?}");
        process::ExitCode::FAILURE
    })?;
    let pem_bytes: [u8; 32] = pem.contents()[pem.contents().len() - 32..]
        .try_into()
        .map_err(|err| {
            log::error!("invalid privkey bytes: {err:?}");
            process::ExitCode::FAILURE
        })?;
    Ok(x25519_dalek::StaticSecret::from(pem_bytes))
}

/// Loads an Ed25519 signing key from `path`, generating and saving it first if the file
/// does not yet exist.
fn generate_or_load_ed25519(path: &str) -> Result<ed25519_dalek::SigningKey, process::ExitCode> {
    if !std::path::Path::new(path).exists() {
        log::info!("signkey not found, generating new Ed25519 key at {path}");
        use ed25519_dalek::pkcs8::EncodePrivateKey;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&seed);
        signing_key
            .write_pkcs8_pem_file(path, Default::default())
            .map_err(|err| {
                log::error!("failed to write signkey {path}: {err:?}");
                process::ExitCode::FAILURE
            })?;
    }
    use ed25519_dalek::pkcs8::DecodePrivateKey;
    ed25519_dalek::SigningKey::read_pkcs8_pem_file(path).map_err(|err| {
        log::error!("failed to load signkey {path}: {err:?}");
        process::ExitCode::FAILURE
    })
}


/// kaonic-gateway: VPN over Reticulum using the kaonic radio hardware.
#[derive(Parser)]
#[command(name = "kaonic-gateway", version)]
pub struct Command {
    /// kaonic-ctrl server UDP address (overrides config kaonic_ctrl_config.server_addr)
    #[arg(short = 'a', long)]
    pub kaonic_ctrl_server: Option<std::net::SocketAddr>,
    /// kaonic-ctrl local listen UDP address (overrides config kaonic_ctrl_config.listen_addr)
    #[arg(short = 'l', long)]
    pub kaonic_ctrl_listen: Option<std::net::SocketAddr>,
    /// Optional: derive Reticulum identity from this name string
    #[arg(short, long)]
    pub id_string: Option<String>,
    /// Address to bind the HTTP API server (default: 127.0.0.1:8080)
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub http_addr: std::net::SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), process::ExitCode> {
    let cmd = Command::parse();

    let db_path = std::env::var("KAONIC_GATEWAY_DB_PATH")
        .unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());

    let settings: SharedSettings = Arc::new(Mutex::new(
        Settings::open(&db_path).unwrap_or_else(|err| {
            eprintln!("failed to open database {db_path}: {err}");
            process::exit(1);
        }),
    ));

    let config = settings.lock().unwrap().load_config().unwrap_or_else(|err| {
        eprintln!("failed to load config from database: {err}");
        process::exit(1);
    });

    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    tokio::spawn(http::serve(settings.clone(), cmd.http_addr));

    let id = if let Some(name) = cmd.id_string {
        log::info!("using identity string: {name:?}");
        PrivateIdentity::new_from_name(&name)
    } else {
        log::info!("loading Reticulum private identity from key files");
        let private_key = {
            let path = std::env::var("KAONIC_GATEWAY_PRIVKEY_PATH").map_err(|err| {
                log::error!("env KAONIC_GATEWAY_PRIVKEY_PATH not found: {err:?}");
                process::ExitCode::FAILURE
            })?;
            log::info!("loading privkey: {path}");
            generate_or_load_x25519(&path)?
        };
        let sign_key = {
            let path = std::env::var("KAONIC_GATEWAY_SIGNKEY_PATH").map_err(|err| {
                log::error!("env KAONIC_GATEWAY_SIGNKEY_PATH not found: {err:?}");
                process::ExitCode::FAILURE
            })?;
            log::info!("loading signkey: {path}");
            generate_or_load_ed25519(&path)?
        };
        PrivateIdentity::new(private_key, sign_key)
    };

    let mut ctrl_config: KaonicCtrlConfig = config.kaonic_ctrl_config
        .as_ref()
        .cloned()
        .unwrap_or_else(|| {
            log::info!("kaonic_ctrl_config not found in database, using defaults");
            KaonicCtrlConfig::default()
        });
    if let Some(server_addr) = cmd.kaonic_ctrl_server {
        ctrl_config.server_addr = server_addr;
    }
    if let Some(listen_addr) = cmd.kaonic_ctrl_listen {
        ctrl_config.listen_addr = listen_addr;
    }

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<FrameRecord>();

    // Spawn task that persists received frames to the DB
    {
        let settings = settings.clone();
        tokio::spawn(async move {
            while let Some(record) = frame_rx.recv().await {
                if let Err(err) = settings.lock().unwrap().log_frame(&record, 500) {
                    log::warn!("failed to log frame: {err}");
                }
            }
        });
    }

    kaonic_vpn::run(config, id, ctrl_config, Some(frame_tx)).await.map_err(|err: Box<dyn std::error::Error + Send + Sync>| {
        log::error!("gateway error: {err:?}");
        process::ExitCode::FAILURE
    })
}

