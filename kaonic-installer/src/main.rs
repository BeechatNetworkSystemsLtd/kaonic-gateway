#![recursion_limit = "512"]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use clap::Parser;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;

mod installer;
mod plugins;

use installer::{apply_update, get_version, validate_on_boot, Target};

const META_DIR: &str = "/etc/kaonic";
const BIN_DIR: &str = "/usr/bin";
const PLUGINS_DIR: &str = "/etc/kaonic/plugins";
const PLUGINS_DB: &str = "/etc/kaonic/plugins/kaonic-plugins.db";
const SYSTEMD_DIR: &str = "/etc/systemd/system";
const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024; // 256 MiB

/// kaonic-installer: standalone installer server for kaonic packages and plugins.
#[derive(Parser)]
#[command(name = "kaonic-installer", version)]
struct Cmd {
    /// Address to listen on
    #[arg(long, default_value = "0.0.0.0:8682")]
    pub listen: SocketAddr,
}

#[derive(Clone)]
struct AppState {
    commd: Arc<Target>,
    gateway: Arc<Target>,
    core_plugins: Vec<plugins::CorePluginSpec>,
    plugins_root: PathBuf,
    plugins_db: PathBuf,
    systemd_dir: PathBuf,
    cert_path: PathBuf,
}

fn make_targets(meta_dir: &str, bin_dir: &str) -> (Arc<Target>, Arc<Target>) {
    let meta = PathBuf::from(meta_dir);
    let bin = PathBuf::from(bin_dir);
    (
        Arc::new(Target {
            name: "commd",
            bin_path: bin.join("kaonic-commd"),
            service: "kaonic-commd.service",
            meta_dir: meta.clone(),
        }),
        Arc::new(Target {
            name: "gateway",
            bin_path: bin.join("kaonic-gateway"),
            service: "kaonic-gateway.service",
            meta_dir: meta,
        }),
    )
}

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .parse_filters("info,kaonic_installer=debug")
        .parse_default_env()
        .init();

    let cmd = Cmd::parse();

    let (commd, gateway) = make_targets(META_DIR, BIN_DIR);

    // Validate installed binaries against stored hashes on boot
    validate_on_boot(&commd);
    validate_on_boot(&gateway);

    let state = AppState {
        core_plugins: vec![
            plugins::CorePluginSpec::new(
                commd.clone(),
                "Kaonic Radio Driver",
                "Built-in radio control service managed by the platform installer.",
                "Beechat Network Systems Ltd",
            ),
            plugins::CorePluginSpec::new(
                gateway.clone(),
                "Kaonic Gateway",
                "Built-in Kaonic web gateway service managed by the platform installer.",
                "Beechat Network Systems Ltd",
            ),
        ],
        commd,
        gateway,
        plugins_root: PathBuf::from(PLUGINS_DIR),
        plugins_db: PathBuf::from(PLUGINS_DB),
        systemd_dir: PathBuf::from(SYSTEMD_DIR),
        cert_path: PathBuf::from(META_DIR).join("beechat-ota.pub.pem"),
    };

    if let Err(err) = plugins::initialize_store(
        &state.plugins_root,
        &state.plugins_db,
        Some(&state.cert_path),
        &state.core_plugins,
    ) {
        log::error!("failed to initialize plugin store: {err}");
    }

    let app = Router::new()
        .route("/api/installer/:target/version", get(handle_version))
        .route("/api/installer/:target/upload", post(handle_upload))
        .route("/api/plugins", get(handle_plugins_list))
        .route("/api/plugins/install", post(handle_plugin_install))
        .route("/api/plugins/:plugin_id/upload", post(handle_plugin_upload))
        .route("/api/plugins/:plugin_id/start", post(handle_plugin_start))
        .route("/api/plugins/:plugin_id/stop", post(handle_plugin_stop))
        .route(
            "/api/plugins/:plugin_id/restart",
            post(handle_plugin_restart),
        )
        .route("/api/plugins/:plugin_id", delete(handle_plugin_delete))
        .layer(CorsLayer::permissive())
        .layer(RequestBodyLimitLayer::new(MAX_UPLOAD_BYTES))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cmd.listen)
        .await
        .expect("failed to bind");
    log::info!("kaonic-installer listening on http://{}", cmd.listen);
    axum::serve(listener, app).await.expect("server error");
}

// ── handlers ─────────────────────────────────────────────────────────────────

async fn handle_version(
    Path(target): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    match resolve_target(&state, &target) {
        Some(t) => Json(get_version(&t)).into_response(),
        None => (StatusCode::NOT_FOUND, "unknown target").into_response(),
    }
}

async fn handle_upload(
    Path(target): Path<String>,
    State(state): State<AppState>,
    multipart: Multipart,
) -> impl IntoResponse {
    let t = match resolve_target(&state, &target) {
        Some(t) => t,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"detail":"unknown target"})),
            )
                .into_response()
        }
    };

    let zip_bytes = match read_upload_bytes(multipart).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };

    // Run the blocking OTA logic on a threadpool thread so we don't block Tokio
    let result = tokio::task::spawn_blocking(move || apply_update(&t, &zip_bytes))
        .await
        .unwrap_or_else(|e| Err(format!("task panic: {e}")));

    match result {
        Ok(msg) => Json(serde_json::json!({"detail": msg})).into_response(),
        Err(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"detail": msg})),
        )
            .into_response(),
    }
}

async fn handle_plugins_list(State(state): State<AppState>) -> impl IntoResponse {
    let plugins_root = state.plugins_root.clone();
    let plugins_db = state.plugins_db.clone();
    let cert_path = state.cert_path.clone();
    let core_plugins = state.core_plugins.clone();
    match tokio::task::spawn_blocking(move || {
        plugins::list_plugins(&plugins_root, &plugins_db, Some(&cert_path), &core_plugins)
    })
    .await
    {
        Ok(Ok(records)) => Json(records).into_response(),
        Ok(Err(err)) => plugin_error_response(err),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"detail": format!("plugin task panic: {err}")})),
        )
            .into_response(),
    }
}

async fn handle_plugin_install(
    State(state): State<AppState>,
    multipart: Multipart,
) -> impl IntoResponse {
    let zip_bytes = match read_upload_bytes(multipart).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let plugins_root = state.plugins_root.clone();
    let plugins_db = state.plugins_db.clone();
    let systemd_dir = state.systemd_dir.clone();
    let cert_path = state.cert_path.clone();
    let core_plugins = state.core_plugins.clone();

    match tokio::task::spawn_blocking(move || {
        plugins::install_plugin(
            &plugins_root,
            &plugins_db,
            &systemd_dir,
            &cert_path,
            &core_plugins,
            &zip_bytes,
        )
    })
    .await
    {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => plugin_error_response(err),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"detail": format!("plugin task panic: {err}")})),
        )
            .into_response(),
    }
}

async fn handle_plugin_upload(
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
    multipart: Multipart,
) -> impl IntoResponse {
    let zip_bytes = match read_upload_bytes(multipart).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    let plugins_root = state.plugins_root.clone();
    let plugins_db = state.plugins_db.clone();
    let systemd_dir = state.systemd_dir.clone();
    let cert_path = state.cert_path.clone();
    let core_plugins = state.core_plugins.clone();

    match tokio::task::spawn_blocking(move || {
        plugins::upload_plugin_update(
            &plugins_root,
            &plugins_db,
            &systemd_dir,
            &cert_path,
            &core_plugins,
            &plugin_id,
            &zip_bytes,
        )
    })
    .await
    {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => plugin_error_response(err),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"detail": format!("plugin task panic: {err}")})),
        )
            .into_response(),
    }
}

async fn handle_plugin_start(
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    handle_plugin_action(plugin_id, state, plugins::PluginAction::Start).await
}

async fn handle_plugin_stop(
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    handle_plugin_action(plugin_id, state, plugins::PluginAction::Stop).await
}

async fn handle_plugin_restart(
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    handle_plugin_action(plugin_id, state, plugins::PluginAction::Restart).await
}

async fn handle_plugin_delete(
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let plugins_root = state.plugins_root.clone();
    let plugins_db = state.plugins_db.clone();
    let systemd_dir = state.systemd_dir.clone();
    match tokio::task::spawn_blocking(move || {
        plugins::delete_plugin(&plugins_root, &plugins_db, &systemd_dir, &plugin_id)
    })
    .await
    {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => plugin_error_response(err),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"detail": format!("plugin task panic: {err}")})),
        )
            .into_response(),
    }
}

async fn handle_plugin_action(
    plugin_id: String,
    state: AppState,
    action: plugins::PluginAction,
) -> axum::response::Response {
    let plugins_db = state.plugins_db.clone();
    match tokio::task::spawn_blocking(move || {
        plugins::control_plugin(&plugins_db, &plugin_id, action)
    })
    .await
    {
        Ok(Ok(response)) => Json(response).into_response(),
        Ok(Err(err)) => plugin_error_response(err),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"detail": format!("plugin task panic: {err}")})),
        )
            .into_response(),
    }
}

async fn read_upload_bytes(mut multipart: Multipart) -> Result<Bytes, axum::response::Response> {
    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => match field.bytes().await {
                Ok(bytes) => return Ok(bytes),
                Err(err) => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"detail": format!("read error: {err}")})),
                    )
                        .into_response())
                }
            },
            Ok(None) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"detail": "no file uploaded"})),
                )
                    .into_response())
            }
            Err(err) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"detail": format!("multipart error: {err}")})),
                )
                    .into_response())
            }
        }
    }
}

fn plugin_error_response(err: plugins::PluginError) -> axum::response::Response {
    (
        err.status,
        Json(serde_json::json!({
            "detail": err.detail
        })),
    )
        .into_response()
}

fn resolve_target(state: &AppState, name: &str) -> Option<Arc<Target>> {
    match name {
        "commd" => Some(state.commd.clone()),
        "gateway" => Some(state.gateway.clone()),
        _ => None,
    }
}
