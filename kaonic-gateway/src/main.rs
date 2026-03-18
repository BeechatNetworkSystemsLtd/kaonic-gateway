//! kaonic-gateway: Reticulum VPN gateway using kaonic radio hardware

mod http;

use std::process;
use std::sync::Arc;

use clap::Parser;
use env_logger;
use http::{AppState, SharedSettings};
use kaonic_gateway::atak::{AtakBridge, BridgeMetrics};
use kaonic_gateway::radio::{attach_radio_interface, connect_radio_client, SharedRadioClient};
use kaonic_gateway::settings::Settings;
use log;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::{Transport, TransportConfig};
use std::sync::Mutex;
use tokio;
use tokio_util::sync::CancellationToken;

const DEFAULT_DB_PATH: &str = "kaonic-gateway.db";
const ATAK_PORTS: &[u16] = &[6969];

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

    let db_path =
        std::env::var("KAONIC_GATEWAY_DB_PATH").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());

    let settings: SharedSettings =
        Arc::new(Mutex::new(Settings::open(&db_path).unwrap_or_else(|err| {
            eprintln!("failed to open database {db_path}: {err}");
            process::exit(1);
        })));

    let config = settings
        .lock()
        .unwrap()
        .load_config()
        .unwrap_or_else(|err| {
            eprintln!("failed to load config from database: {err}");
            process::exit(1);
        });

    env_logger::Builder::new()
        .parse_filters("warn,kaonic_gateway=trace,kaonic_vpn=warn,kaonic_reticulum=trace,rns_vpn=warn,reticulum=debug")
        .parse_default_env()
        .init();

    let serial = std::fs::read_to_string("/etc/kaonic/kaonic_serial")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    log::info!("device serial: {serial}");
    kaonic_dashboard::set_serial(serial);

    let seed = settings
        .lock()
        .unwrap()
        .load_or_create_seed()
        .unwrap_or_else(|err| {
            log::error!("failed to load/create identity seed: {err}");
            process::exit(1);
        });
    let id = PrivateIdentity::new_from_name(&seed);
    let vpn_hash = id.address_hash().to_hex_string();
    log::info!("Reticulum identity ready: {vpn_hash}");

    let default_server: std::net::SocketAddr = "192.168.10.1:9090".parse().unwrap();
    let default_listen: std::net::SocketAddr = "0.0.0.0:0".parse().unwrap();
    let server_addr = cmd.kaonic_ctrl_server.unwrap_or(default_server);
    log::info!("connecting to kaonic-ctrl at {server_addr}");
    let radio_client: SharedRadioClient = connect_radio_client(default_listen, server_addr)
        .await
        .map_err(|e| {
            log::error!("kaonic-ctrl connect error: {e:?}");
            process::ExitCode::FAILURE
        })?;

    let mut transport_cfg = TransportConfig::new("kaonic-gateway", &id, true);
    transport_cfg.set_retransmit(true);
    let transport = Arc::new(tokio::sync::Mutex::new(Transport::new(transport_cfg)));

    attach_radio_interface(&transport, radio_client.clone(), &config.radio, 0)
        .await
        .map_err(|err| {
            log::error!("radio interface attach error: {err:?}");
            process::ExitCode::FAILURE
        })?;

    // Shared cancellation token — cancelled on Ctrl-C / SIGTERM.
    let cancel = CancellationToken::new();

    let mut atak_metrics = Vec::new();
    for &port in ATAK_PORTS {
        let seed = settings
            .lock()
            .unwrap()
            .load_or_create_named_seed(&format!("atak_identity_seed_{port}"))
            .unwrap_or_else(|err| {
                log::error!("failed to load/create atak identity seed for port {port}: {err}");
                process::exit(1);
            });
        let atak_identity = PrivateIdentity::new_from_name(&seed);
        let metrics = BridgeMetrics::new(port);
        atak_metrics.push(metrics.clone());
        let bridge = AtakBridge::new(transport.clone(), atak_identity, port, metrics);
        let c = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = bridge.run(c).await {
                log::error!("atak-bridge:{port} exited with error: {e}");
            }
        });
    }

    tokio::spawn(http::serve(
        AppState {
            settings: settings.clone(),
            atak_metrics,
            vpn_hash,
            radio_client,
        },
        cmd.http_addr,
    ));

    shutdown_signal(cancel.clone()).await;

    // kaonic_vpn::run_vpn(
    //     transport,
    //     &config.network.to_string(),
    //     config.peers,
    //     config.announce_freq_secs,
    //     id,
    //     cancel.clone(),
    // ).await.map_err(|err| {
    //     log::error!("gateway error: {err:?}");
    //     process::ExitCode::FAILURE
    // })
    //
    Ok(())
}

/// Wait for Ctrl-C or SIGTERM, then cancel the token.
async fn shutdown_signal(cancel: CancellationToken) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => log::info!("received Ctrl-C"),
            _ = sigterm.recv()          => log::info!("received SIGTERM"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        log::info!("received Ctrl-C");
    }
    log::info!("shutting down…");
    cancel.cancel();
}
