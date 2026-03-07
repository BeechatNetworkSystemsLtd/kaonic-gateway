mod handlers;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{Router, routing::get};

use kaonic_gateway::settings::Settings;

/// Shared settings handle passed to all HTTP handlers.
pub type SharedSettings = Arc<Mutex<Settings>>;

/// Start the HTTP API server. Runs until the process exits.
pub async fn serve(settings: SharedSettings, addr: SocketAddr) {
    let app = Router::new()
        .route(
            "/api/settings",
            get(handlers::get_settings).put(handlers::put_settings),
        )
        .route(
            "/api/settings/radio",
            get(handlers::get_radio).put(handlers::put_radio),
        )
        .with_state(settings);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind HTTP listener");

    log::info!("HTTP API listening on http://{addr}");
    axum::serve(listener, app).await.expect("HTTP server error");
}
