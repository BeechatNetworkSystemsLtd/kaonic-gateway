//! kaonic-gateway: Reticulum VPN gateway using kaonic radio hardware

mod http;

use std::process;
use std::sync::{Arc, Mutex};

use clap::Parser;
use env_logger;
use kaonic_gateway::atak::{AtakBridge, BridgeMetrics};
use kaonic_gateway::settings::Settings;
use kaonic_vpn::{KaonicCtrlConfig, RadioSenders};
use http::{AppState, SharedSettings};
use log;
use reticulum::identity::PrivateIdentity;
use tokio;
use tokio_util::sync::CancellationToken;

const DEFAULT_DB_PATH: &str = "kaonic-gateway.db";
const ATAK_PORTS: &[u16] = &[6969, 17012];

/// kaonic-gateway: VPN over Reticulum using the kaonic radio hardware.
#[derive(Parser)]
#[command(name = "kaonic-gateway", version)]
pub struct Command {
    /// kaonic-ctrl server UDP address (overrides config / default 192.168.10.1:9090)
    #[arg(short = 'a', long)]
    pub kaonic_ctrl_server: Option<std::net::SocketAddr>,
    /// Address to bind the HTTP server — dashboard + API (default: 0.0.0.0:80)
    #[arg(long, default_value = "0.0.0.0:80")]
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
        .parse_filters("warn,kaonic_gateway=trace,kaonic_vpn=warn,kaonic_reticulum=trace,rns_vpn=warn,reticulum=error")
        .parse_default_env()  // RUST_LOG overrides the above
        .init();

    // Read device serial number
    let serial = std::fs::read_to_string("/etc/kaonic/kaonic_serial")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    log::info!("device serial: {serial}");
    kaonic_dashboard::set_serial(serial);

    // Load or generate a persistent Reticulum identity from the DB
    let seed = settings.lock().unwrap().load_or_create_seed().unwrap_or_else(|err| {
        log::error!("failed to load/create identity seed: {err}");
        process::exit(1);
    });
    let id = PrivateIdentity::new_from_name(&seed);
    let vpn_hash = id.address_hash().to_hex_string();
    log::info!("Reticulum identity ready: {vpn_hash}");

    let mut ctrl_configs = config.kaonic_ctrl_configs.clone();
    if ctrl_configs.is_empty() {
        log::info!("no radio module config in database, using defaults");
        ctrl_configs.push(KaonicCtrlConfig::default());
    }
    if let Some(server_addr) = cmd.kaonic_ctrl_server {
        for c in &mut ctrl_configs { c.server_addr = server_addr; }
    }
    // Use module 0 for TX/RX only.
    ctrl_configs.retain(|c| c.module == 0);

    let (transport, radio_senders) = kaonic_vpn::setup_transport(&id, ctrl_configs)
        .await
        .map_err(|err| {
            log::error!("transport setup error: {err:?}");
            process::ExitCode::FAILURE
        })?;
    let radio_senders = Arc::new(radio_senders);

    let mut atak_metrics = Vec::new();
    for &port in ATAK_PORTS {
        let seed = settings.lock().unwrap()
            .load_or_create_named_seed(&format!("atak_identity_seed_{port}"))
            .unwrap_or_else(|err| {
                log::error!("failed to load/create atak identity seed for port {port}: {err}");
                process::exit(1);
            });
        let atak_identity = PrivateIdentity::new_from_name(&seed);
        let metrics = BridgeMetrics::new(port);
        atak_metrics.push(metrics.clone());
        let bridge = AtakBridge::new(transport.clone(), atak_identity, port, metrics);
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            if let Err(e) = bridge.run(cancel).await {
                log::error!("atak-bridge:{port} exited with error: {e}");
            }
        });
    }

    tokio::spawn(http::serve(AppState {
        settings: settings.clone(),
        atak_metrics,
        vpn_hash,
        radio_senders,
    }, cmd.http_addr));

    kaonic_vpn::run_vpn(transport, config, id).await.map_err(|err| {
        log::error!("gateway error: {err:?}");
        process::ExitCode::FAILURE
    })
}
