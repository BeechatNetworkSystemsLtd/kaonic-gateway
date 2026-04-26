pub(crate) mod handlers;
mod installer;
pub(crate) mod ws;

use std::net::SocketAddr;

use axum::extract::Path;
use axum::http::header;
use axum::response::IntoResponse;
use axum::{
    routing::{delete, get, post},
    Router,
};
use leptos::config::LeptosOptions;
use leptos::prelude::*;
use leptos_axum::{generate_route_list, LeptosRoutes};
use rust_embed::Embed;

pub use kaonic_gateway::state::{AppState, SharedSettings};

#[derive(Embed)]
#[folder = "assets/"]
struct Assets;

/// Start the HTTP server: Leptos SSR pages + REST/WebSocket API. Runs forever.
pub async fn serve(state: AppState, addr: SocketAddr) {
    let leptos_options = LeptosOptions::builder()
        .output_name("kaonic-gateway")
        .site_root(".")
        .site_pkg_dir("pkg")
        .site_addr(addr)
        .build();

    ws::spawn_status_publishers(state.clone());

    let routes = generate_route_list(kaonic_gateway::app::App);

    // REST/WebSocket API + embedded static assets
    let api = Router::new()
        .route("/api/audio/cards", get(handlers::get_audio_cards))
        .route(
            "/api/audio/{card}/save",
            post(handlers::post_audio_card_save),
        )
        .route(
            "/api/audio/{card}/{output}/test",
            post(handlers::post_audio_control_test),
        )
        .route(
            "/api/audio/{card}/{output}",
            get(handlers::get_audio_control).put(handlers::put_audio_control),
        )
        .route(
            "/api/audio/{output}",
            get(handlers::get_audio).put(handlers::put_audio),
        )
        .route(
            "/api/settings",
            get(handlers::get_settings).put(handlers::put_settings),
        )
        .route(
            "/api/settings/radio/{module}",
            get(handlers::get_radio).put(handlers::put_radio),
        )
        .route("/api/radio/{module}/test", post(handlers::post_radio_test))
        .route("/api/system/reboot", post(handlers::post_system_reboot))
        .route(
            "/api/system/service/restart",
            post(handlers::post_system_service_restart),
        )
        .route("/api/status", get(handlers::get_status))
        .route("/api/info", get(handlers::get_info))
        .route("/api/serial", get(handlers::get_serial))
        .route("/api/network/snapshot", get(handlers::get_network_snapshot))
        .route(
            "/api/vpn/routes",
            get(handlers::get_vpn_routes).put(handlers::put_vpn_routes),
        )
        .route("/api/vpn/access", post(handlers::put_vpn_access))
        .route("/api/vpn/ping", post(handlers::post_vpn_ping))
        .route("/api/vpn/speed-test", post(handlers::post_vpn_speed_test))
        .route("/api/plugins", get(installer::list_plugins))
        .route(
            "/api/plugins/installer-version",
            get(installer::installer_version),
        )
        .route("/api/plugins/install", post(installer::install_plugin))
        .route(
            "/api/plugins/{plugin_id}/upload",
            post(installer::upload_plugin),
        )
        .route(
            "/api/plugins/{plugin_id}/start",
            post(installer::start_plugin),
        )
        .route(
            "/api/plugins/{plugin_id}/stop",
            post(installer::stop_plugin),
        )
        .route(
            "/api/plugins/{plugin_id}/restart",
            post(installer::restart_plugin),
        )
        .route("/api/plugins/{plugin_id}", delete(installer::delete_plugin))
        .route("/network/wifi/mode", post(handlers::post_wifi_mode))
        .route("/network/wifi/antenna", post(handlers::post_wifi_antenna))
        .route("/network/wifi/connect", post(handlers::post_wifi_connect))
        .route("/api/ws/status", get(ws::ws_status))
        .route("/assets/{*path}", get(serve_asset))
        // Convenience short-paths kept for compatibility
        .route("/style.css", get(serve_style_css))
        .route("/kaonic-logo.svg", get(serve_logo_svg))
        .with_state(state.clone());

    // Leptos SSR routes — inject AppState as leptos context for server functions
    let leptos_app = {
        let leptos_options = leptos_options.clone();
        let state = state.clone();
        Router::new()
            .leptos_routes_with_context(
                &leptos_options,
                routes,
                move || provide_context(state.clone()),
                {
                    let leptos_options = leptos_options.clone();
                    move || kaonic_gateway::app::shell(leptos_options.clone())
                },
            )
            .fallback(file_and_error_handler)
            .with_state(leptos_options)
    };

    let app = api.merge(leptos_app);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind HTTP listener");

    log::info!("HTTP server listening on http://{addr}");
    axum::serve(listener, app).await.expect("HTTP server error");
}

// ── Asset handlers ────────────────────────────────────────────────────────────

async fn serve_asset(Path(path): Path<String>) -> impl IntoResponse {
    serve_embedded(&path)
}

async fn serve_style_css() -> impl IntoResponse {
    serve_embedded("style.css")
}

async fn serve_logo_svg() -> impl IntoResponse {
    serve_embedded("kaonic-logo.svg")
}

fn serve_embedded(path: &str) -> axum::response::Response {
    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

async fn file_and_error_handler(
    uri: axum::http::Uri,
    axum::extract::State(options): axum::extract::State<LeptosOptions>,
    req: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    leptos_axum::file_and_error_handler(|opts: LeptosOptions| kaonic_gateway::app::shell(opts))(
        uri,
        axum::extract::State(options),
        req,
    )
    .await
}
