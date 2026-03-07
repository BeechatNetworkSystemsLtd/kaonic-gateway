//! kaonic-dashboard: web configuration portal for kaonic-gateway

mod frames;
mod pages;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{Router, routing::get};
use clap::Parser;
use pages::SharedSettings;

use kaonic_settings::settings::Settings;

/// kaonic-dashboard: web portal for editing Kaonic Gateway settings.
#[derive(Parser)]
#[command(name = "kaonic-dashboard", version)]
struct Command {
    /// Address to bind the dashboard HTTP server
    #[arg(long, default_value = "0.0.0.0:3000")]
    addr: SocketAddr,
    /// Path to the kaonic-gateway SQLite database
    #[arg(long, env = "KAONIC_GATEWAY_DB_PATH", default_value = "kaonic-gateway.db")]
    db_path: String,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let cmd = Command::parse();

    let settings: SharedSettings = Arc::new(Mutex::new(
        Settings::open(&cmd.db_path).unwrap_or_else(|err| {
            eprintln!("failed to open settings database '{}': {err}", cmd.db_path);
            std::process::exit(1);
        }),
    ));

    let app = Router::new()
        .route("/", get(frames::get_dashboard))
        .route("/frames/json", get(frames::get_frames_json))
        .route("/settings", get(pages::get_settings).post(pages::post_settings))
        .with_state(settings);

    let listener = tokio::net::TcpListener::bind(cmd.addr)
        .await
        .expect("failed to bind dashboard listener");

    log::info!("Kaonic Dashboard → http://{}", cmd.addr);
    axum::serve(listener, app).await.expect("dashboard server error");
}
