//! kaonic-dashboard: standalone web portal for kaonic-gateway.
//!
//! When run standalone it serves only the dashboard UI pages.
//! In production, use `kaonic-gateway` which embeds the dashboard
//! alongside the JSON API on the same port.

use std::net::SocketAddr;

use clap::Parser;

/// kaonic-dashboard: web portal for editing Kaonic Gateway settings.
#[derive(Parser)]
#[command(name = "kaonic-dashboard", version)]
struct Command {
    /// Address to bind the dashboard HTTP server
    #[arg(long, default_value = "0.0.0.0:3000")]
    addr: SocketAddr,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .parse_default_env()
        .init();

    let cmd = Command::parse();
    let app = kaonic_dashboard::router();

    let listener = tokio::net::TcpListener::bind(cmd.addr)
        .await
        .expect("failed to bind dashboard listener");

    log::info!("Kaonic Dashboard → http://{}", cmd.addr);
    axum::serve(listener, app).await.expect("dashboard server error");
}
