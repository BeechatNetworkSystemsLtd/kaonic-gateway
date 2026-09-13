//! `/api/remote/*` — local operator surface for Kaonic Remote. Everything
//! here runs on the operator's own gateway; authority over *other* nodes is
//! still decided by their trust stores, never by this API.

use axum::extract::{ConnectInfo, Multipart, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use kaonic_remote::protocol::{blob, plugin_action, RadioConfigWire};
use kaonic_remote::{RemoteError, RemoteRuntime, RemoteSnapshot};
use reticulum::hash::AddressHash;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;

use super::AppState;

type ApiError = (StatusCode, Json<serde_json::Value>);

fn api_error(status: StatusCode, detail: impl Into<String>) -> ApiError {
    (status, Json(json!({ "detail": detail.into() })))
}

fn remote_error(err: RemoteError) -> ApiError {
    use kaonic_remote::protocol::status;
    let code = match err.status {
        status::UNAUTHORIZED => StatusCode::FORBIDDEN,
        status::NOT_FOUND => StatusCode::NOT_FOUND,
        status::BAD_REQUEST => StatusCode::BAD_REQUEST,
        status::UNSUPPORTED => StatusCode::NOT_IMPLEMENTED,
        status::BUSY => StatusCode::CONFLICT,
        _ => StatusCode::BAD_GATEWAY,
    };
    api_error(code, err.to_string())
}

fn runtime(state: &AppState) -> Result<Arc<RemoteRuntime>, ApiError> {
    state.remote.clone().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "remote control is not running (webapp-only mode)",
        )
    })
}

/// Endpoints that widen what a peer can do (the root shell, feature
/// switches, media plumbing) are for the device's own operator. A paired
/// node reaching us over the tunnel must not be able to call them.
fn deny_mesh_client(state: &AppState, addr: SocketAddr) -> Result<(), ApiError> {
    if kaonic_gateway::state::is_mesh_client(state, addr) {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "not permitted from the mesh; use the device's own interface",
        ));
    }
    Ok(())
}

fn parse_hash(hash: &str) -> Result<AddressHash, ApiError> {
    AddressHash::new_from_hex_string(hash.trim())
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid node hash"))
}

#[derive(Serialize)]
pub struct DetailResponse {
    pub detail: String,
}

pub async fn get_snapshot(State(state): State<AppState>) -> Result<Json<RemoteSnapshot>, ApiError> {
    let mut snapshot = runtime(&state)?.snapshot();
    kaonic_gateway::remote::enrich_snapshot(&state, &mut snapshot).await;
    Ok(Json(snapshot))
}

#[derive(Deserialize)]
pub struct TagRequest {
    pub tag: String,
}

/// Operator's own label for a node, stored locally (never sent on air).
pub async fn put_node_tag(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Json(request): Json<TagRequest>,
) -> Result<Json<DetailResponse>, ApiError> {
    let node = parse_hash(&hash)?;
    let tag: String = request
        .tag
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(48)
        .collect();
    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    settings
        .set_remote_tag(&node.to_hex_string(), &tag)
        .map_err(|err| api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(DetailResponse {
        detail: if tag.is_empty() {
            "Tag cleared".into()
        } else {
            format!("Tagged as {tag}")
        },
    }))
}

// ── Pairing ───────────────────────────────────────────────────────────────────

/// A scanned pairing code: the peer's public keys, plus what to call it.
///
/// Carrying the keys — not just the hash — is what lets a node be added before
/// it has ever announced. The hash alone would leave nothing to address.
#[derive(Deserialize)]
pub struct AddNodeRequest {
    /// Hex public + verifying key, as printed in this device's own code.
    pub identity_hex: String,
    #[serde(default)]
    pub codename: String,
    /// Send a pairing request straight away. The far operator still has to
    /// approve it, so this only saves a second click here.
    #[serde(default)]
    pub pair: bool,
}

#[derive(Serialize)]
pub struct AddNodeResponse {
    pub identity_hash: String,
    pub codename: String,
    pub pairing: String,
    pub detail: String,
}

/// Adds a node from a scanned code, with no announce required.
pub async fn post_add_node(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<AddNodeRequest>,
) -> Result<Json<AddNodeResponse>, ApiError> {
    deny_mesh_client(&state, peer)?;
    let runtime = runtime(&state)?;

    let identity_hex = request.identity_hex.trim().to_lowercase();
    let identity = reticulum::identity::Identity::new_from_hex_string(&identity_hex)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "that code is not a valid node key"))?;

    let codename = kaonic_gateway::settings::normalize_codename(request.codename.trim())
        .unwrap_or_else(|_| request.codename.trim().to_string());

    let hash = runtime
        .add_node_from_identity(identity, &codename)
        .await
        .map_err(remote_error)?;

    let mut pairing = "none".to_string();
    let mut detail = format!("{} added", if codename.is_empty() { hash.to_hex_string() } else { codename.clone() });
    if request.pair {
        match runtime.request_pairing(hash).await {
            Ok(state) => {
                pairing = format!("{state:?}").to_lowercase();
                detail = "pairing requested; the other operator has to approve it".into();
            }
            Err(err) => {
                // The node is on the list either way, which is the point: the
                // request can be retried when it comes within reach.
                detail = format!("added, but the request did not go out yet: {}", err.detail);
            }
        }
    }

    Ok(Json(AddNodeResponse {
        identity_hash: hash.to_hex_string(),
        codename,
        pairing,
        detail,
    }))
}

pub async fn post_pair(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let runtime = runtime(&state)?;
    let node = parse_hash(&hash)?;
    let pairing = runtime.request_pairing(node).await.map_err(remote_error)?;
    Ok(Json(json!({ "pairing": pairing })))
}

pub async fn post_cancel(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<DetailResponse>, ApiError> {
    runtime(&state)?.cancel_pairing(parse_hash(&hash)?);
    Ok(Json(DetailResponse {
        detail: "request cancelled".into(),
    }))
}

pub async fn post_approve(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<DetailResponse>, ApiError> {
    runtime(&state)?
        .approve_pairing(parse_hash(&hash)?)
        .map_err(remote_error)?;
    Ok(Json(DetailResponse {
        detail: "pairing approved".into(),
    }))
}

pub async fn post_reject(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<DetailResponse>, ApiError> {
    runtime(&state)?
        .reject_pairing(parse_hash(&hash)?)
        .map_err(remote_error)?;
    Ok(Json(DetailResponse {
        detail: "pairing rejected".into(),
    }))
}

pub async fn post_unpair(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<DetailResponse>, ApiError> {
    runtime(&state)?
        .unpair(parse_hash(&hash)?)
        .await
        .map_err(remote_error)?;
    Ok(Json(DetailResponse {
        detail: "unpaired".into(),
    }))
}

// ── Control ───────────────────────────────────────────────────────────────────

pub async fn post_ping(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rtt = runtime(&state)?
        .ping(parse_hash(&hash)?)
        .await
        .map_err(remote_error)?;
    Ok(Json(json!({ "rtt_ms": rtt.as_millis() as u64 })))
}

pub async fn get_info(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<kaonic_remote::protocol::InfoBody>, ApiError> {
    let info = runtime(&state)?
        .info(parse_hash(&hash)?)
        .await
        .map_err(remote_error)?;
    Ok(Json(info))
}

pub async fn get_radio(
    State(state): State<AppState>,
    Path((hash, module)): Path<(String, u8)>,
) -> Result<Json<RadioConfigWire>, ApiError> {
    let config = runtime(&state)?
        .radio_get(parse_hash(&hash)?, module)
        .await
        .map_err(remote_error)?;
    Ok(Json(config))
}

pub async fn put_radio(
    State(state): State<AppState>,
    Path((hash, module)): Path<(String, u8)>,
    Json(mut config): Json<RadioConfigWire>,
) -> Result<Json<DetailResponse>, ApiError> {
    config.module = module;
    let detail = runtime(&state)?
        .radio_set(parse_hash(&hash)?, config)
        .await
        .map_err(remote_error)?;
    Ok(Json(DetailResponse { detail }))
}

pub async fn get_plugins(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<Vec<kaonic_remote::protocol::PluginInfoWire>>, ApiError> {
    let plugins = runtime(&state)?
        .plugin_list(parse_hash(&hash)?)
        .await
        .map_err(remote_error)?;
    Ok(Json(plugins))
}

pub async fn post_plugin_action(
    State(state): State<AppState>,
    Path((hash, plugin_id, action)): Path<(String, String, String)>,
) -> Result<Json<DetailResponse>, ApiError> {
    let action = match action.as_str() {
        "start" => plugin_action::START,
        "stop" => plugin_action::STOP,
        "restart" => plugin_action::RESTART,
        "delete" => plugin_action::DELETE,
        other => return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown action {other}"))),
    };
    let detail = runtime(&state)?
        .plugin_action(parse_hash(&hash)?, plugin_id, action)
        .await
        .map_err(remote_error)?;
    Ok(Json(DetailResponse { detail }))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    #[serde(default)]
    pub plugin_id: String,
}

/// Multipart plugin package → chunked transfer to the node. Returns the job id.
pub async fn post_plugin_upload(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    let runtime = runtime(&state)?;
    let node = parse_hash(&hash)?;
    let mut bytes = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| api_error(StatusCode::BAD_REQUEST, format!("multipart: {err}")))?
    {
        let data = field
            .bytes()
            .await
            .map_err(|err| api_error(StatusCode::BAD_REQUEST, format!("multipart: {err}")))?;
        if !data.is_empty() {
            bytes = Some(data.to_vec());
            break;
        }
    }
    let bytes = bytes.ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "package is empty"))?;
    let plugin_id = query.plugin_id.trim().to_string();
    if !plugin_id.is_empty()
        && (plugin_id.starts_with('.')
            || !plugin_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid plugin id"));
    }
    let job = runtime
        .push_blob(node, blob::PLUGIN_PACKAGE, plugin_id, bytes)
        .map_err(remote_error)?;
    Ok(Json(json!({ "job": job })))
}

pub async fn post_reboot(
    State(state): State<AppState>,
    Path(hash): Path<String>,
) -> Result<Json<DetailResponse>, ApiError> {
    let detail = runtime(&state)?
        .system_reboot(parse_hash(&hash)?)
        .await
        .map_err(remote_error)?;
    Ok(Json(DetailResponse { detail }))
}

#[derive(Deserialize)]
pub struct ServiceRestartRequest {
    pub unit: String,
}

pub async fn post_service_restart(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Json(request): Json<ServiceRestartRequest>,
) -> Result<Json<DetailResponse>, ApiError> {
    let detail = runtime(&state)?
        .service_restart(parse_hash(&hash)?, request.unit)
        .await
        .map_err(remote_error)?;
    Ok(Json(DetailResponse { detail }))
}

// ── Runtime link coding (not persisted) ───────────────────────────────────────

#[derive(Serialize)]
pub struct FecStatus {
    pub default_class: &'static str,
    pub classes: Vec<&'static str>,
    pub verified_fast: u64,
    pub decoded_full: u64,
    pub failed: u64,
    pub header_failed: u64,
}

fn fec_status(state: &AppState) -> Result<FecStatus, ApiError> {
    let fec = state
        .fec
        .as_ref()
        .ok_or_else(|| api_error(StatusCode::SERVICE_UNAVAILABLE, "radio interface not running"))?;
    let stats = fec.stats();
    Ok(FecStatus {
        default_class: kaonic_gateway::remote::traffic_class_name(fec.default_class()),
        classes: vec!["robust", "auto", "fast", "fastest", "tm2048", "tm1536", "tm1280", "tc512", "none"],
        verified_fast: stats.verified_fast,
        decoded_full: stats.decoded_full,
        failed: stats.failed,
        header_failed: stats.header_failed,
    })
}

pub async fn get_fec(State(state): State<AppState>) -> Result<Json<FecStatus>, ApiError> {
    Ok(Json(fec_status(&state)?))
}

#[derive(Deserialize)]
pub struct ClassRequest {
    pub class: String,
}

/// Default class for destinations no session has claimed. Runtime only;
/// `robust` keeps every frame decodable by older firmware.
pub async fn put_fec(
    State(state): State<AppState>,
    Json(request): Json<ClassRequest>,
) -> Result<Json<FecStatus>, ApiError> {
    let class = kaonic_gateway::remote::parse_traffic_class(&request.class)
        .ok_or_else(|| api_error(StatusCode::BAD_REQUEST, "unknown class"))?;
    state
        .fec
        .as_ref()
        .ok_or_else(|| api_error(StatusCode::SERVICE_UNAVAILABLE, "radio interface not running"))?
        .set_default(class);
    Ok(Json(fec_status(&state)?))
}

/// Per-node traffic class for the current session (control/bulk/media).
pub async fn post_node_class(
    State(state): State<AppState>,
    Path(hash): Path<String>,
    Json(request): Json<ClassRequest>,
) -> Result<Json<DetailResponse>, ApiError> {
    use kaonic_remote::LinkClass;
    let class = match request.class.as_str() {
        "control" => LinkClass::Control,
        "bulk" => LinkClass::Bulk,
        "media" => LinkClass::Media,
        other => return Err(api_error(StatusCode::BAD_REQUEST, format!("unknown class {other}"))),
    };
    runtime(&state)?
        .set_node_class(parse_hash(&hash)?, class)
        .map_err(remote_error)?;
    Ok(Json(DetailResponse {
        detail: format!("link class set to {}", request.class),
    }))
}

// ── Remote shell ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ShellRequest {
    pub command: String,
    #[serde(default = "default_shell_timeout")]
    pub timeout_secs: u32,
}

fn default_shell_timeout() -> u32 {
    20
}

/// Run a command on a paired node. The node must have its own remote-shell
/// switch on; this gateway only relays.
pub async fn post_shell(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(hash): Path<String>,
    Json(request): Json<ShellRequest>,
) -> Result<Json<kaonic_remote::protocol::ShellResultBody>, ApiError> {
    deny_mesh_client(&state, peer)?;
    let result = runtime(&state)?
        .shell_exec(parse_hash(&hash)?, request.command, request.timeout_secs)
        .await
        .map_err(remote_error)?;
    Ok(Json(result))
}

// ── Feature switches ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct FeaturesResponse {
    pub vpn_enabled: bool,
    pub remote_enabled: bool,
    pub shell_enabled: bool,
    /// True when the saved switches differ from what is running.
    pub restart_required: bool,
}

fn features_response(state: &AppState) -> FeaturesResponse {
    let saved = kaonic_gateway::remote::load_feature_flags(&state.settings);
    let remote_running = state.remote.is_some();
    let vpn_running = state.vpn.is_some();
    FeaturesResponse {
        vpn_enabled: saved.vpn_enabled,
        remote_enabled: saved.remote_enabled,
        shell_enabled: kaonic_gateway::remote::shell_enabled().unwrap_or(saved.shell_enabled),
        restart_required: saved.remote_enabled != remote_running || saved.vpn_enabled != vpn_running,
    }
}

pub async fn get_features(State(state): State<AppState>) -> Json<FeaturesResponse> {
    Json(features_response(&state))
}

pub async fn put_features(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(request): Json<FeaturesResponse>,
) -> Result<Json<FeaturesResponse>, ApiError> {
    deny_mesh_client(&state, peer)?;
    {
        let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings
            .set_setting(
                kaonic_gateway::remote::SETTING_VPN_ENABLED,
                &request.vpn_enabled.to_string(),
            )
            .and_then(|_| {
                settings.set_setting(
                    kaonic_gateway::remote::SETTING_REMOTE_ENABLED,
                    &request.remote_enabled.to_string(),
                )
            })
            .and_then(|_| {
                settings.set_setting(
                    kaonic_gateway::remote::SETTING_SHELL_ENABLED,
                    &request.shell_enabled.to_string(),
                )
            })
            .map_err(|err| api_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    }
    // The shell switch applies immediately; VPN/remote need a restart.
    kaonic_gateway::remote::set_shell_enabled(request.shell_enabled);
    Ok(Json(features_response(&state)))
}

// ── Media channels (UDP bridge) ───────────────────────────────────────────────

fn bridge(state: &AppState) -> Result<Arc<kaonic_gateway::media_bridge::MediaBridge>, ApiError> {
    state
        .media
        .clone()
        .ok_or_else(|| api_error(StatusCode::SERVICE_UNAVAILABLE, "media bridge not running"))
}

pub async fn get_media(State(state): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let channels = bridge(&state)?.list();
    let streams = runtime(&state)?.snapshot().media;
    Ok(Json(json!({ "channels": channels, "streams": streams })))
}

pub async fn post_media(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(hash): Path<String>,
    Json(spec): Json<kaonic_gateway::media_bridge::MediaChannelSpec>,
) -> Result<Json<DetailResponse>, ApiError> {
    deny_mesh_client(&state, peer)?;
    let node = parse_hash(&hash)?;
    bridge(&state)?
        .start(node, spec.clone())
        .await
        .map_err(|err| api_error(StatusCode::BAD_REQUEST, err))?;
    Ok(Json(DetailResponse {
        detail: format!(
            "media channel {} open: app → {} → radio, radio → {}",
            spec.stream, spec.ingress, spec.egress
        ),
    }))
}

pub async fn delete_media(
    State(state): State<AppState>,
    Path((hash, stream)): Path<(String, u8)>,
) -> Result<Json<DetailResponse>, ApiError> {
    bridge(&state)?
        .stop(parse_hash(&hash)?, stream)
        .await
        .map_err(|err| api_error(StatusCode::NOT_FOUND, err))?;
    Ok(Json(DetailResponse {
        detail: format!("media channel {stream} closed"),
    }))
}

// ── Local settings ────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct RemoteSettings {
    pub announce_secs: u32,
    pub accept_pairing: bool,
    /// Keep links to paired nodes up (live setting).
    #[serde(default = "default_true")]
    pub auto_link: bool,
    #[serde(default = "default_chunk_gap")]
    pub chunk_gap_ms: u32,
    #[serde(default = "default_bulk_parity")]
    pub bulk_parity: u32,
}

fn default_true() -> bool {
    true
}

fn default_bulk_parity() -> u32 {
    u32::from(kaonic_remote::protocol::BULK_BLOCK_M)
}

fn default_chunk_gap() -> u32 {
    kaonic_gateway::remote::DEFAULT_CHUNK_GAP_MS
}

pub async fn get_settings(State(state): State<AppState>) -> Result<Json<RemoteSettings>, ApiError> {
    let config = kaonic_gateway::remote::load_remote_config(&state.settings);
    Ok(Json(RemoteSettings {
        announce_secs: config.announce_secs,
        accept_pairing: config.accept_pairing,
        auto_link: state
            .remote
            .as_ref()
            .map(|r| r.auto_link())
            .unwrap_or(config.auto_link),
        chunk_gap_ms: state
            .remote
            .as_ref()
            .map(|r| r.chunk_gap_ms())
            .unwrap_or(config.chunk_gap.as_millis() as u32),
        bulk_parity: state
            .remote
            .as_ref()
            .map(|r| r.bulk_parity())
            .unwrap_or_else(default_bulk_parity),
    }))
}

pub async fn put_settings(
    State(state): State<AppState>,
    Json(request): Json<RemoteSettings>,
) -> impl IntoResponse {
    let announce_secs = request.announce_secs.clamp(5, 255);
    let chunk_gap_ms = request.chunk_gap_ms.clamp(0, 500);
    // Chunk pacing and auto-link apply immediately; announce/pairing need a
    // restart.
    let bulk_parity = request.bulk_parity.min(8);
    if let Some(remote) = state.remote.as_ref() {
        remote.set_chunk_gap_ms(chunk_gap_ms);
        remote.set_bulk_parity(bulk_parity);
        remote.set_auto_link(request.auto_link);
    }
    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    let result = settings
        .set_setting("remote_announce_secs", &announce_secs.to_string())
        .and_then(|_| settings.set_setting("remote_accept_pairing", &request.accept_pairing.to_string()))
        .and_then(|_| settings.set_setting(kaonic_gateway::remote::SETTING_AUTO_LINK, &request.auto_link.to_string()))
        .and_then(|_| settings.set_setting(kaonic_gateway::remote::SETTING_CHUNK_GAP_MS, &chunk_gap_ms.to_string()))
        .and_then(|_| settings.set_setting(kaonic_gateway::remote::SETTING_BULK_PARITY, &bulk_parity.to_string()));
    match result {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "detail": "Saved. Chunk pacing and paired-node links apply now; announce settings apply after the gateway restarts." })),
        ),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "detail": err.to_string() })),
        ),
    }
}
