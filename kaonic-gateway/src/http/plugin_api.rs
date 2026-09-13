//! `/api/plugin/*` — the local surface plugins talk to.
//!
//! A plugin runs as its own process on the device with its own Reticulum
//! identity, so it cannot see the gateway's trust store, its contacts or its
//! database. Rather than teach every plugin to open those files, the gateway
//! lends it three things over loopback:
//!
//!   * **identity** — who this node is, so a plugin can label itself;
//!   * **contacts** — the nodes the operator has already paired with, with the
//!     service destinations each of them advertises;
//!   * **a key/value store** — per-plugin, persisted in the gateway database,
//!     so plugin state survives a reinstall of the plugin.
//!
//! Pairing is *not* re-done per plugin. The operator pairs gateways once, in
//! the remote feature; a plugin inherits that decision by asking here. What a
//! plugin registers with [`post_services`] is published to those already-paired
//! peers over the authenticated remote link, so a contact's PTT destination is
//! bound to the pairing rather than matched on a codename anyone could claim.
//!
//! # Reach
//!
//! This surface is local and nothing else. It is served by its own listener
//! bound to `127.0.0.1` — it is not mounted on the gateway's HTTPS router,
//! which answers on every interface including the VPN tunnel — and each
//! handler independently re-checks that the caller is on loopback and holds
//! the bearer token from a 0600 file only processes on the device can read.
//! Two mechanisms, so a future routing change cannot quietly expose it.
//!
//! # Trust between plugins
//!
//! All plugins on a device share one token, so this API isolates the device
//! from the network — not plugins from each other. A plugin can read another
//! plugin's stored values and register a service in another's name. That is
//! the same trust level they already have: they run as the same user on the
//! same box and can read each other's files directly. Plugins must therefore
//! not store secrets here that other plugins may not have.
//!
//! What plugins *cannot* do is forge the peer directory: it is cached under a
//! namespace whose name no plugin can address (see [`valid_name`]), so what a
//! plugin learns about a remote node always came over the paired link.
//!
//! # Compatibility
//!
//! Paths are versioned (`/api/plugin/v1/...`) and [`API_VERSION`] is reported
//! by [`get_identity`], so a plugin can tell what it is talking to. Within a
//! version the contract only ever grows: fields are added, never removed or
//! retyped, and every response field a client reads should be optional on its
//! side. A change that cannot be made additively takes a new `/v2` prefix,
//! served alongside `/v1` for as long as plugins in the field use it.

use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::AppState;

/// The version this gateway serves at `/api/plugin/v1`. Bumped only when a
/// new prefix is added, never for an additive change within one.
pub const API_VERSION: u32 = 1;

/// Loopback port the plugin API listens on. Fixed, so a plugin needs no
/// discovery beyond the token file.
pub const DEFAULT_PORT: u16 = 8781;

/// Starts the loopback listener. Failure is logged, not fatal: the gateway
/// serves its own UI whether or not plugins can reach it.
pub fn serve_local(state: AppState) {
    // Created before anything can ask for it, so an installed plugin finds the
    // token waiting rather than racing the first request.
    if ensure_token().is_none() {
        log::warn!("plugin api: no token; the local plugin API will refuse every request");
    }
    let port: u16 = std::env::var("KAONIC_PLUGIN_API_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    tokio::spawn(async move {
        let addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(err) => {
                log::error!("plugin api: cannot bind {addr}: {err}");
                return;
            }
        };
        log::info!("plugin api v{API_VERSION} on http://{addr}");
        let app = router(state);
        if let Err(err) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            log::error!("plugin api: {err}");
        }
    });
}

/// The `/api/plugin/v1` surface. Kept in one place so the route table is the
/// documentation of what a plugin may ask for.
fn router(state: AppState) -> axum::Router {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/plugin/v1/identity", get(get_identity))
        .route("/api/plugin/v1/contacts", get(get_contacts))
        .route(
            "/api/plugin/v1/services",
            get(get_services).post(post_services),
        )
        .route("/api/plugin/v1/store/{plugin}", get(get_store_keys))
        .route(
            "/api/plugin/v1/store/{plugin}/{key}",
            get(get_store).put(put_store).delete(delete_store),
        )
        .with_state(state)
}

type ApiError = (StatusCode, Json<serde_json::Value>);

fn api_error(status: StatusCode, detail: impl Into<String>) -> ApiError {
    (status, Json(json!({ "detail": detail.into() })))
}

/// Where the shared secret lives. Root-owned and 0600, like the TLS key next
/// to it; `KAONIC_PLUGIN_TOKEN_PATH` moves it for development.
pub fn token_path() -> PathBuf {
    std::env::var("KAONIC_PLUGIN_TOKEN_PATH")
        .unwrap_or_else(|_| "/etc/kaonic/plugin-api.token".to_string())
        .into()
}

/// Bytes of randomness in the token. 36 characters from a 36-symbol alphabet
/// is ~186 bits, far past anything guessable, and it costs nothing.
const TOKEN_LEN: usize = 48;

/// Read once per process. Caching matters for correctness as much as speed: if
/// the file were consulted per request, deleting it at runtime would silently
/// mint a new token and lock out every already-running plugin.
static TOKEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Reads the token, creating it on first call. Returns `None` when the file
/// cannot be created — the API then refuses every request rather than falling
/// back to an unauthenticated one.
pub fn ensure_token() -> Option<String> {
    TOKEN.get_or_init(load_or_create_token).clone()
}

fn load_or_create_token() -> Option<String> {
    let path = token_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        // A short file is a truncated write or a placeholder, not a secret.
        if existing.len() >= TOKEN_LEN {
            // The token is a bearer credential: if the file became readable to
            // others, the secret is already suspect. Tighten it and say so,
            // rather than trusting a permission we did not verify.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&path) {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode & 0o077 != 0 {
                        log::warn!(
                            "plugin api: {} was mode {mode:o}; restoring 0600",
                            path.display()
                        );
                        let _ = std::fs::set_permissions(
                            &path,
                            std::fs::Permissions::from_mode(0o600),
                        );
                    }
                }
            }
            return Some(existing);
        }
        if !existing.is_empty() {
            log::warn!("plugin api: {} is too short; replacing it", path.display());
        }
    }
    use rand::Rng;
    let token: String = {
        // `thread_rng` is a CSPRNG seeded from the OS.
        let mut rng = rand::thread_rng();
        (0..TOKEN_LEN)
            .map(|_| {
                const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
                ALPHABET[rng.gen_range(0..ALPHABET.len())] as char
            })
            .collect()
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            if file.write_all(token.as_bytes()).is_err() {
                log::warn!("plugin api: could not write {}", path.display());
                return None;
            }
            log::info!("plugin api: token created at {}", path.display());
            Some(token)
        }
        Err(err) => {
            log::warn!("plugin api: no token at {} ({err})", path.display());
            None
        }
    }
}

/// Loopback plus a matching bearer token. Both are required: loopback alone
/// would admit anything port-forwarded onto the device, and the token alone
/// would be replayable from the mesh.
fn authorize(peer: SocketAddr, headers: &HeaderMap) -> Result<(), ApiError> {
    if !peer.ip().is_loopback() {
        return Err(api_error(
            StatusCode::FORBIDDEN,
            "the plugin API answers on loopback only",
        ));
    }
    let expected = ensure_token().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "plugin API token is unavailable",
        )
    })?;
    let offered = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .trim();
    // Compared without an early exit so a wrong token leaks no length or prefix.
    let matches = offered.len() == expected.len()
        && offered
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !matches {
        return Err(api_error(StatusCode::UNAUTHORIZED, "invalid plugin token"));
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A plugin name is a path segment and a database key; keep it to what a
/// package id can be. Rejecting everything else is what keeps a plugin out of
/// reserved namespaces such as the peer directory's `@peers`, and out of the
/// URL and SQL shapes that path traversal needs.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

// ── Identity ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct IdentityResponse {
    /// The `/api/plugin/vN` prefix this gateway serves.
    pub api_version: u32,
    pub codename: String,
    pub serial: String,
    pub gateway_version: String,
    /// The gateway's own Reticulum identity, as the mesh sees it.
    pub identity_hash: String,
    pub destination_hash: String,
    pub vpn_hash: String,
    /// True when the remote feature is running; contacts are empty without it.
    pub remote_enabled: bool,
}

pub async fn get_identity(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<IdentityResponse>, ApiError> {
    authorize(peer, &headers)?;
    let local = state
        .remote
        .as_ref()
        .map(|remote| remote.snapshot().local)
        .unwrap_or_default();
    Ok(Json(IdentityResponse {
        api_version: API_VERSION,
        codename: state
            .settings
            .lock().unwrap_or_else(|e| e.into_inner())
            .load_or_create_codename()
            .unwrap_or_else(|_| local.codename.clone()),
        serial: state.serial.clone(),
        gateway_version: env!("CARGO_PKG_VERSION").to_string(),
        identity_hash: local.identity_hash,
        destination_hash: local.destination_hash,
        vpn_hash: state.vpn_hash.clone(),
        remote_enabled: state.remote.is_some(),
    }))
}

// ── Contacts ─────────────────────────────────────────────────────────────────

/// One node the operator has a relationship with, flattened for plugins: the
/// pairing decision, how reachable it is right now, and what it serves.
#[derive(Serialize)]
pub struct ContactDto {
    pub identity_hash: String,
    pub destination_hash: String,
    pub codename: String,
    /// The operator's own label, when they set one.
    pub tag: String,
    /// The operator paired with this node; a plugin may treat it as trusted.
    pub paired: bool,
    pub online: bool,
    pub hops: Option<u8>,
    pub rssi: Option<i8>,
    pub last_seen_ts: u64,
    pub vpn_tunnel_ip: String,
    /// Service name → Reticulum destination hash the peer serves it on.
    pub services: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct ContactsResponse {
    pub contacts: Vec<ContactDto>,
}

pub async fn get_contacts(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<ContactsResponse>, ApiError> {
    authorize(peer, &headers)?;
    let Some(remote) = state.remote.as_ref() else {
        return Ok(Json(ContactsResponse {
            contacts: Vec::new(),
        }));
    };
    let mut snapshot = remote.snapshot();
    kaonic_gateway::remote::enrich_snapshot(&state, &mut snapshot).await;
    let contacts = snapshot
        .nodes
        .into_iter()
        // Only nodes the operator actually decided on. An announce alone says
        // nothing about trust, and a plugin should not have to know that.
        .filter(|node| node.paired)
        .map(|node| ContactDto {
            services: peer_services(&state, &node.identity_hash),
            identity_hash: node.identity_hash,
            destination_hash: node.destination_hash,
            codename: node.codename,
            tag: node.tag,
            paired: node.paired,
            online: node.online,
            hops: node.hops,
            rssi: node.rssi,
            last_seen_ts: node.last_seen_ts,
            vpn_tunnel_ip: node.vpn_tunnel_ip,
        })
        .collect();
    Ok(Json(ContactsResponse { contacts }))
}

/// Services a paired peer has told us about, cached under its identity in the
/// plugin store. Empty until the peer's gateway publishes them.
fn peer_services(
    state: &AppState,
    identity_hash: &str,
) -> std::collections::BTreeMap<String, String> {
    let raw = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .plugin_get(PEER_SERVICE_PLUGIN, identity_hash)
        .ok()
        .flatten();
    raw.and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Reserved namespace the peer service cache lives under. Defined by the
/// remote module that writes it; named here so the two cannot drift.
use kaonic_gateway::remote::PEER_SERVICE_PLUGIN;

// ── Service directory ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ServiceRequest {
    /// Protocol name, e.g. `kaonic-audio-ptt`. Both ends derive a channel id
    /// from the same string.
    pub service: String,
    /// The Reticulum destination hash the plugin listens on.
    pub destination: String,
}

#[derive(Serialize)]
pub struct ServicesResponse {
    pub services: std::collections::BTreeMap<String, String>,
}

/// A plugin announces where it can be reached. The gateway keeps it so paired
/// peers can be told, and so the destination survives a plugin restart.
pub async fn post_services(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<ServiceRequest>,
) -> Result<Json<ServicesResponse>, ApiError> {
    authorize(peer, &headers)?;
    if !valid_name(&request.service) {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid service name"));
    }
    let destination = request.destination.trim().to_lowercase();
    if destination.len() != 32 || !destination.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "destination must be a 16-byte hex hash",
        ));
    }
    let services = {
        let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings
            .set_local_service(&request.service, &destination, now_secs())
            .map_err(store_error)?;
        settings.load_local_services().map_err(store_error)?
    };
    // Peers fetch the directory only when the announced digest changes, so
    // advertise the new one right away.
    kaonic_gateway::remote::refresh_local_services_digest(&state);
    Ok(Json(ServicesResponse {
        services: services.into_iter().collect(),
    }))
}

pub async fn get_services(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<ServicesResponse>, ApiError> {
    authorize(peer, &headers)?;
    let services = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_local_services()
        .map_err(store_error)?;
    Ok(Json(ServicesResponse {
        services: services.into_iter().collect(),
    }))
}

// ── Key/value store ──────────────────────────────────────────────────────────

fn store_error(err: impl std::fmt::Display) -> ApiError {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("plugin store: {err}"),
    )
}

#[derive(Serialize)]
pub struct KeysResponse {
    pub keys: Vec<String>,
}

pub async fn get_store_keys(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(plugin): Path<String>,
) -> Result<Json<KeysResponse>, ApiError> {
    authorize(peer, &headers)?;
    if !valid_name(&plugin) {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid plugin name"));
    }
    let keys = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .plugin_keys(&plugin)
        .map_err(store_error)?;
    Ok(Json(KeysResponse { keys }))
}

/// Values are opaque JSON: the gateway stores what the plugin hands it and
/// gives it back unchanged.
pub async fn get_store(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((plugin, key)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(peer, &headers)?;
    if !valid_name(&plugin) || !valid_name(&key) {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid plugin or key"));
    }
    let raw = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .plugin_get(&plugin, &key)
        .map_err(store_error)?
        .ok_or_else(|| api_error(StatusCode::NOT_FOUND, "no such key"))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or(serde_json::Value::String(raw));
    Ok(Json(value))
}

/// Caps what one plugin can park in the gateway database per key.
const MAX_VALUE_BYTES: usize = 64 * 1024;

pub async fn put_store(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((plugin, key)): Path<(String, String)>,
    Json(value): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(peer, &headers)?;
    if !valid_name(&plugin) || !valid_name(&key) {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid plugin or key"));
    }
    let raw = serde_json::to_string(&value).map_err(store_error)?;
    if raw.len() > MAX_VALUE_BYTES {
        return Err(api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("values are limited to {MAX_VALUE_BYTES} bytes"),
        ));
    }
    state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .plugin_set(&plugin, &key, &raw, now_secs())
        .map_err(store_error)?;
    Ok(Json(json!({ "detail": "stored" })))
}

pub async fn delete_store(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((plugin, key)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(peer, &headers)?;
    if !valid_name(&plugin) || !valid_name(&key) {
        return Err(api_error(StatusCode::BAD_REQUEST, "invalid plugin or key"));
    }
    state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .plugin_delete(&plugin, &key)
        .map_err(store_error)?;
    Ok(Json(json!({ "detail": "deleted" })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_and_key_names_stay_inside_one_path_segment() {
        assert!(valid_name("kaonic-audio-ptt"));
        assert!(valid_name("allowed_contacts"));
        assert!(!valid_name(""));
        assert!(!valid_name("../etc/passwd"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(&"x".repeat(65)));
    }

    #[test]
    fn reserved_namespaces_are_unreachable_from_a_plugin() {
        // The gateway caches each paired peer's service directory under this
        // name. If a plugin could address it, it could invent contacts and the
        // pairing would stop being the thing that decides who is trusted.
        assert!(!valid_name(PEER_SERVICE_PLUGIN));
        assert!(!valid_name("@anything"));
    }

    #[test]
    fn a_wrong_token_is_rejected_even_from_loopback() {
        let dir = std::env::temp_dir().join(format!("kaonic-plugin-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token");
        std::env::set_var("KAONIC_PLUGIN_TOKEN_PATH", &path);
        let token = ensure_token().expect("token");
        assert!(token.len() >= TOKEN_LEN);
        assert_eq!(ensure_token().as_deref(), Some(token.as_str()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the token must not be readable by others");
        }

        let loopback: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert!(authorize(loopback, &headers).is_ok());

        let mut wrong = HeaderMap::new();
        wrong.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer nope".parse().unwrap(),
        );
        assert!(authorize(loopback, &wrong).is_err());
        assert!(authorize(loopback, &HeaderMap::new()).is_err());

        // Even with the right token, off-device callers never get in.
        let mesh: SocketAddr = "10.55.0.2:5000".parse().unwrap();
        assert!(authorize(mesh, &headers).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
