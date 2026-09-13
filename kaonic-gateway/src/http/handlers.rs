use axum::extract::{Form, Path, State};
use axum::http::{header, StatusCode};
use axum::{response::IntoResponse, Json};
use kaonic_gateway::app_types::{
    FrameStatsDto, NetworkPortStatusDto, NetworkSnapshotDto, ReticulumSnapshotDto, RxFrameDto,
    ServiceStatusDto, SystemStatusDto, WsInterfacesDto, WsReticulumSnapshotDto,
};
use kaonic_gateway::audio::{
    AudioCardSnapshot, AudioControlSnapshot, AudioControlState, AudioError, AudioOutput,
};
use kaonic_gateway::config::GatewayConfig;
use kaonic_gateway::local_https;
use kaonic_gateway::network::{read_interface_ipv4, NetworkError, WifiAntenna, WifiMode};
use kaonic_gateway::radio::{apply_module_config, transmit_test_frame, RadioModuleConfig};
use kaonic_gateway::settings::normalize_codename;
use kaonic_gateway::system_metrics::{
    is_gateway_service_unit, read_cpu_freq_mhz, read_cpu_percent_async, read_fs_mb,
    read_gateway_services, read_mem_mb, read_os_details,
};
use kaonic_vpn::VpnSnapshot;
use reticulum::hash::AddressHash;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::{Duration, Instant};

use super::AppState;

// ── /api/info ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct InfoResponse {
    pub serial: String,
}

pub async fn get_info(State(state): State<AppState>) -> Json<InfoResponse> {
    Json(InfoResponse {
        serial: state.serial.clone(),
    })
}

pub async fn get_serial(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; charset=utf-8".to_string(),
        )],
        state.serial.clone(),
    )
}

pub async fn get_system_rootca() -> Result<impl IntoResponse, (StatusCode, String)> {
    let path = local_https::root_ca_cert_path();
    let bytes = tokio::fs::read(&path).await.map_err(|err| {
        let status = if err.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (
            status,
            format!(
                "failed to read root CA certificate {}: {err}",
                path.display()
            ),
        )
    })?;

    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/x-x509-ca-cert".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{}\"",
                    local_https::ROOT_CA_DOWNLOAD_NAME
                ),
            ),
        ],
        bytes,
    ))
}

/// `GET /api/settings` — return the full gateway config.
pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<GatewayConfig>, StatusCode> {
    let s = state
        .settings
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.load_config().map(Json).map_err(|err| {
        log::error!("failed to load settings: {err}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// `PUT /api/settings` — replace the full gateway config.
pub async fn put_settings(
    State(state): State<AppState>,
    Json(config): Json<GatewayConfig>,
) -> StatusCode {
    {
        let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = s.save_config(&config) {
            log::error!("failed to save settings: {err}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    if let Some(vpn) = &state.vpn {
        if let Err(err) = vpn
            .replace_peer_policy(config.allow_all_peers, config.peers.clone())
            .await
        {
            log::error!("failed to apply VPN peer policy from settings: {err}");
            return StatusCode::BAD_REQUEST;
        }
        vpn.replace_advertised_routes(config.advertised_routes.clone())
            .await;
    }

    StatusCode::NO_CONTENT
}

/// `GET /api/settings/radio/:module` — return config for one RF module (0 or 1).
pub async fn get_radio(
    State(state): State<AppState>,
    Path(module): Path<usize>,
) -> Result<Json<RadioModuleConfig>, StatusCode> {
    let s = state
        .settings
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.load_config()
        .map_err(|err| {
            log::error!("failed to load radio settings: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
        .and_then(|c| {
            c.radio
                .module_configs
                .get(module)
                .cloned()
                .map(Json)
                .ok_or(StatusCode::NOT_FOUND)
        })
}

/// `PUT /api/settings/radio/:module` — save config for one RF module and apply to hardware.
pub async fn put_radio(
    State(state): State<AppState>,
    Path(module): Path<usize>,
    Json(cfg): Json<RadioModuleConfig>,
) -> StatusCode {
    log::info!(
        "put_radio: module={} radio_config={:?} modulation={:?}",
        module,
        cfg.radio_config,
        cfg.modulation
    );

    let save_result = {
        let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        s.save_module_config(module, &cfg)
    };
    if let Err(err) = save_result {
        log::error!("failed to save radio settings for module {module}: {err}");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    log::info!("put_radio: module={module} saved to DB");

    if let Some(client) = state.radio_client.as_ref() {
        match apply_module_config(client, module, &cfg).await {
            Ok(()) => log::info!("put_radio: config applied to module {module}"),
            Err(err) => log::error!("put_radio: apply failed for module {module}: {err}"),
        }
    } else {
        log::info!("put_radio: running without radio backend, saved config only");
    }

    StatusCode::NO_CONTENT
}

pub async fn post_radio_test(
    State(state): State<AppState>,
    Path(module): Path<usize>,
    Json(request): Json<RadioTestRequest>,
) -> Result<Json<RadioTestResponse>, (StatusCode, String)> {
    if module > 1 {
        return Err((
            StatusCode::NOT_FOUND,
            format!("radio module {module} not found"),
        ));
    }

    let message = request.message.trim().to_string();
    if message.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "message is required".into()));
    }
    if message.chars().count() > 2047 {
        return Err((
            StatusCode::BAD_REQUEST,
            "message exceeds 2047 characters".into(),
        ));
    }

    transmit_test_frame(
        state.radio_client.clone(),
        state.radio_tx_observer.clone(),
        module,
        message.as_bytes(),
    )
    .await
    .map_err(|err| (StatusCode::SERVICE_UNAVAILABLE, err))?;

    Ok(Json(RadioTestResponse {
        status: format!(
            "Sent test frame on {}",
            if module == 0 { "Radio A" } else { "Radio B" }
        ),
    }))
}

pub async fn post_system_reboot() -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    let status = request_system_reboot().map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err))?;
    Ok(Json(SystemActionResponse { status }))
}

pub async fn post_system_codename(
    State(state): State<AppState>,
    Json(request): Json<SetSystemCodenameRequest>,
) -> Result<Json<SystemCodenameResponse>, (StatusCode, String)> {
    let codename = normalize_codename(&request.codename)
        .map_err(|err| (StatusCode::BAD_REQUEST, err.to_string()))?;

    {
        let settings = state.settings.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings lock poisoned".to_string(),
            )
        })?;
        settings.save_codename(&codename).map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save codename: {err}"),
            )
        })?;
    }
    if let Some(remote) = state.remote.as_ref() {
        remote.set_codename(&codename);
    }

    Ok(Json(SystemCodenameResponse {
        status: "Codename updated".into(),
        codename,
    }))
}

pub async fn post_system_service_restart(
    Json(request): Json<ServiceActionRequest>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    if !is_gateway_service_unit(&request.unit) {
        return Err((StatusCode::BAD_REQUEST, "unsupported service".into()));
    }

    let status = request_service_restart(&request.unit)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err))?;
    Ok(Json(SystemActionResponse { status }))
}

/// The node this device routes through, and what that currently reaches.
#[derive(serde::Serialize)]
pub struct VpnUplinkResponse {
    pub enabled: bool,
    pub active: bool,
    pub peer: Option<String>,
    pub peer_codename: Option<String>,
    pub routes: Vec<String>,
    /// True when the router shares its whole uplink, so general traffic from
    /// this device and its clients goes over the radio.
    pub default_route: bool,
    pub detail: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct PutVpnUplinkRequest {
    pub enabled: bool,
    /// Destination hash of the node to route through.
    #[serde(default)]
    pub peer: Option<String>,
}

pub async fn get_vpn_uplink(
    State(state): State<AppState>,
) -> Result<Json<VpnUplinkResponse>, (StatusCode, String)> {
    let uplink = match &state.vpn {
        Some(vpn) => vpn.snapshot().await.uplink,
        None => Default::default(),
    };
    // The stored selection is the operator-facing identity hash; the VPN's own
    // `peer` is the derived tunnel hash, which would mean nothing on the page.
    let peer = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_vpn_uplink()
        .ok()
        .and_then(|saved| saved.peer);
    let peer_codename = peer.as_deref().and_then(|hash| codename_for(&state, hash));
    Ok(Json(VpnUplinkResponse {
        enabled: uplink.enabled,
        active: uplink.active,
        peer,
        peer_codename,
        routes: uplink.routes,
        default_route: uplink.default_route,
        detail: uplink.detail,
    }))
}

fn codename_for(state: &AppState, identity_hash: &str) -> Option<String> {
    let paired = state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_remote_paired()
        .ok()?;
    paired
        .into_iter()
        .find(|node| node.identity_hash == identity_hash)
        .map(|node| node.codename)
        .filter(|codename| !codename.is_empty())
}

pub async fn put_vpn_uplink(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<PutVpnUplinkRequest>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    deny_mesh(&state, peer)?;

    let selected = request
        .peer
        .map(|hash| hash.trim().to_lowercase())
        .filter(|hash| !hash.is_empty());
    if request.enabled {
        let hash = selected.as_deref().ok_or((
            StatusCode::BAD_REQUEST,
            "choose a node to route through".to_string(),
        ))?;
        if hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err((StatusCode::BAD_REQUEST, "invalid node hash".into()));
        }
        // Routing through a node means trusting it with everything this device
        // and its clients send. That has to be a node the operator paired, not
        // any hash someone can put in a request.
        if codename_for(&state, hash).is_none() {
            return Err((
                StatusCode::BAD_REQUEST,
                "that node is not paired; pair it first on the Remote page".into(),
            ));
        }
    }

    // Stored as the identity hash the operator chose, applied as the VPN
    // destination hash the tunnel actually sees announces from.
    let saved = kaonic_vpn::VpnUplinkConfig {
        enabled: request.enabled,
        peer: selected.clone(),
    };
    let applied = kaonic_vpn::VpnUplinkConfig {
        enabled: request.enabled,
        peer: selected
            .as_deref()
            .and_then(|hash| kaonic_gateway::remote::vpn_hash_for_identity(&state.settings, hash)),
    };
    if request.enabled && applied.peer.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "that node's keys are not on file; re-pair it on the Remote page".into(),
        ));
    }

    {
        let settings = state.settings.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings lock poisoned".into(),
            )
        })?;
        settings.save_vpn_uplink(&saved).map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save uplink settings: {err}"),
            )
        })?;
    }

    if let Some(vpn) = &state.vpn {
        vpn.set_uplink(applied);
    }
    let uplink = saved;

    Ok(Json(SystemActionResponse {
        status: if uplink.enabled {
            "Router selected".into()
        } else {
            "Router cleared".into()
        },
    }))
}

/// Refuses writes that arrive over the tunnel.
///
/// What this node advertises, who may join, and where it routes are decisions
/// made at the device. A peer already on the mesh must not be able to reach
/// back through it and rearrange the network around itself.
fn deny_mesh(state: &AppState, peer: std::net::SocketAddr) -> Result<(), (StatusCode, String)> {
    if kaonic_gateway::state::is_mesh_client(state, peer) {
        return Err((
            StatusCode::FORBIDDEN,
            "not permitted from the mesh; use the device's own interface".into(),
        ));
    }
    Ok(())
}

/// Gateway mode: what this node forwards for its peers, and out of which
/// interface. `GET` reports what is running, which is not always what was
/// asked for — see `active` and `detail`.
#[derive(serde::Serialize)]
pub struct VpnGatewayResponse {
    pub enabled: bool,
    pub active: bool,
    pub egress_interface: Option<String>,
    pub routes: Vec<String>,
    pub detail: Option<String>,
    /// Interfaces the operator can pick from, so the UI need not guess.
    pub available_interfaces: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct PutVpnGatewayRequest {
    pub enabled: bool,
    /// `None` (or empty) uses whichever interface carries the default route.
    #[serde(default)]
    pub egress_interface: Option<String>,
    /// Destinations peers may reach through this node, as CIDRs.
    #[serde(default)]
    pub routes: Vec<String>,
}

pub async fn get_vpn_gateway(
    State(state): State<AppState>,
) -> Result<Json<VpnGatewayResponse>, (StatusCode, String)> {
    let snapshot = match &state.vpn {
        Some(vpn) => vpn.snapshot().await.gateway,
        None => Default::default(),
    };
    Ok(Json(VpnGatewayResponse {
        enabled: snapshot.enabled,
        active: snapshot.active,
        egress_interface: snapshot.egress_interface,
        routes: snapshot.routes,
        detail: snapshot.detail,
        available_interfaces: forwardable_interfaces(),
    }))
}

/// Interfaces that could plausibly be an egress: up, not loopback, and not the
/// tunnel itself (forwarding the tunnel back into the tunnel is never right).
fn forwardable_interfaces() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name != "lo" && !name.starts_with("kaonic") && !name.starts_with("tun"))
        .collect();
    names.sort();
    names
}

pub async fn put_vpn_gateway(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<PutVpnGatewayRequest>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    deny_mesh(&state, peer)?;
    let routes = request
        .routes
        .iter()
        .map(|route| {
            route.trim().parse::<cidr::Ipv4Cidr>().map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("invalid route '{route}': {err}"),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Turning it on with nothing to forward is almost always a mistake, and
    // silently doing nothing would be hard to debug from the UI.
    if request.enabled && routes.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "list at least one destination to forward (0.0.0.0/0 shares the full uplink)".into(),
        ));
    }

    let egress = request
        .egress_interface
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    if let Some(name) = egress.as_deref() {
        if !forwardable_interfaces().iter().any(|iface| iface == name) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("'{name}' is not an interface this node can forward out of"),
            ));
        }
    }

    let gateway = kaonic_vpn::VpnGatewayConfig {
        enabled: request.enabled,
        egress_interface: egress,
        routes,
    };

    {
        let settings = state.settings.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings lock poisoned".into(),
            )
        })?;
        settings.save_vpn_gateway(&gateway).map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save gateway settings: {err}"),
            )
        })?;
    }

    if let Some(vpn) = &state.vpn {
        vpn.set_gateway(gateway.clone());
    }

    Ok(Json(SystemActionResponse {
        status: if gateway.enabled {
            "VPN gateway mode enabled".into()
        } else {
            "VPN gateway mode disabled".into()
        },
    }))
}

pub async fn put_vpn_routes(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<PutVpnRoutesRequest>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    deny_mesh(&state, peer)?;
    let routes = request
        .routes
        .iter()
        .map(|route| {
            route.trim().parse::<cidr::Ipv4Cidr>().map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("invalid route '{route}': {err}"),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    {
        let settings = state.settings.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings lock poisoned".into(),
            )
        })?;
        let mut config = settings.load_config().map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load config: {err}"),
            )
        })?;
        config.advertised_routes = routes.clone();
        settings.save_config(&config).map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save config: {err}"),
            )
        })?;
    }

    if let Some(vpn) = &state.vpn {
        vpn.replace_advertised_routes(routes).await;
    }

    Ok(Json(SystemActionResponse {
        status: "VPN advertised routes updated".into(),
    }))
}

pub async fn put_vpn_access(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(request): Json<PutVpnAccessRequest>,
) -> Result<Json<VpnAccessResponse>, (StatusCode, String)> {
    deny_mesh(&state, peer)?;
    let peers = normalize_vpn_peer_hashes(&request.peers)?;

    {
        let settings = state.settings.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings lock poisoned".into(),
            )
        })?;
        let mut config = settings.load_config().map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to load config: {err}"),
            )
        })?;
        config.allow_all_peers = request.allow_all_peers;
        config.peers = peers.clone();
        settings.save_config(&config).map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save config: {err}"),
            )
        })?;
    }

    if let Some(vpn) = &state.vpn {
        vpn.replace_peer_policy(request.allow_all_peers, peers.clone())
            .await
            .map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("failed to apply VPN peer policy: {err}"),
                )
            })?;
    }

    Ok(Json(VpnAccessResponse {
        status: if request.allow_all_peers {
            "VPN access updated (allow all peers)".into()
        } else {
            "VPN access updated (allowlist only)".into()
        },
        allow_all_peers: request.allow_all_peers,
        peers,
    }))
}

pub async fn post_vpn_ping(
    Json(request): Json<VpnPingRequest>,
) -> Result<Json<VpnPingResponse>, (StatusCode, String)> {
    let address = request.address.trim().parse::<Ipv4Addr>().map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid IPv4 address '{}': {err}", request.address.trim()),
        )
    })?;

    let result = request_vpn_ping(address)
        .await
        .map_err(|err| (StatusCode::BAD_GATEWAY, err))?;
    Ok(Json(VpnPingResponse {
        ok: result.ok,
        latency: result.latency,
    }))
}

#[derive(serde::Deserialize)]
pub struct SpeedPayloadQuery {
    #[serde(default)]
    pub bytes: Option<usize>,
}

/// Fixed-size payload so a speed test measures the link, not whatever the
/// index page happens to weigh today. Served over the VPN tunnel.
pub async fn get_speed_payload(
    axum::extract::Query(query): axum::extract::Query<SpeedPayloadQuery>,
) -> impl IntoResponse {
    const DEFAULT: usize = 32 * 1024;
    const MAX: usize = 256 * 1024;
    let len = query.bytes.unwrap_or(DEFAULT).clamp(1024, MAX);
    // Incompressible bytes so a proxy or gzip cannot flatter the result.
    let mut body = vec![0u8; len];
    for (i, byte) in body.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(31) % 251) as u8;
    }
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
}

pub async fn post_vpn_speed_test(
    State(state): State<AppState>,
    Json(request): Json<VpnSpeedTestRequest>,
) -> Result<Json<VpnSpeedTestResponse>, (StatusCode, String)> {
    let address = request.address.trim().parse::<Ipv4Addr>().map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid IPv4 address '{}': {err}", request.address.trim()),
        )
    })?;

    let vpn = state
        .vpn
        .as_ref()
        .ok_or((StatusCode::SERVICE_UNAVAILABLE, "vpn unavailable".into()))?;
    let peer_ip = address.to_string();
    let snapshot = vpn.snapshot().await;
    if !snapshot
        .peers
        .iter()
        .any(|peer| peer.tunnel_ip.as_deref() == Some(peer_ip.as_str()))
    {
        return Err((StatusCode::NOT_FOUND, "peer tunnel IP not found".into()));
    }

    // Ask for a fixed-size payload; older peers without the endpoint fall
    // back to their index page below.
    let payload_bytes = request.bytes.unwrap_or(32 * 1024).clamp(1024, 256 * 1024);
    let url = format!("https://{peer_ip}/api/speed-payload?bytes={payload_bytes}");
    let root_ca_pem = std::fs::read(local_https::root_ca_cert_path()).map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read local Root CA certificate: {err}"),
        )
    })?;
    let root_ca = reqwest::Certificate::from_pem(&root_ca_pem).map_err(|err| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to parse local Root CA certificate: {err}"),
        )
    })?;
    // The TLS handshake alone moves a few KB over the radio, so both
    // timeouts are generous; short ones expire mid-handshake.
    // Every device signs its certificate with its own Root CA, so a peer can
    // never chain to ours. This request only times a fixed payload over a
    // tunnel Reticulum already authenticates and encrypts, sends no
    // credentials and keeps only the byte count, so verification is skipped
    // here and nowhere else.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(90))
        .timeout(Duration::from_secs(180))
        .danger_accept_invalid_certs(true)
        .add_root_certificate(root_ca)
        .build()
        .map_err(|err| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to build speed-test client: {err}"),
            )
        })?;

    let started = Instant::now();
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|err| {
            let mut detail = err.to_string();
            let mut source = std::error::Error::source(&err);
            while let Some(inner) = source {
                detail.push_str(&format!(": {inner}"));
                source = inner.source();
            }
            let reason = if err.is_timeout() {
                format!("timed out — the link is too slow or the peer is unreachable ({detail})")
            } else if err.is_connect() {
                format!("could not connect to the peer ({detail})")
            } else {
                detail
            };
            (StatusCode::BAD_GATEWAY, format!("speed-test failed: {reason}"))
        })?;

    let response = if response.status() == reqwest::StatusCode::NOT_FOUND {
        // Peer predates the payload endpoint: measure its index page instead.
        client
            .get(format!("https://{peer_ip}/"))
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|err| {
                (
                    StatusCode::BAD_GATEWAY,
                    format!("speed-test fallback failed: {err}"),
                )
            })?
    } else {
        response
    };

    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("speed-test returned {}", response.status()),
        ));
    }

    // Count the body as it streams and stop at a sane ceiling: the peer is
    // paired, not trusted, and an unbounded read would exhaust memory.
    const MAX_DOWNLOAD: u64 = 8 * 1024 * 1024;
    let mut bytes_len = 0u64;
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|err| {
        (
            StatusCode::BAD_GATEWAY,
            if err.is_timeout() {
                "speed-test timed out while downloading".to_string()
            } else {
                format!("failed to read speed-test body: {err}")
            },
        )
    })? {
        bytes_len += chunk.len() as u64;
        if bytes_len > MAX_DOWNLOAD {
            return Err((
                StatusCode::BAD_GATEWAY,
                "speed-test response exceeded the size limit".into(),
            ));
        }
    }
    let duration_ms = started.elapsed().as_millis().max(1) as u64;
    let bps = ((bytes_len as u128) * 8 * 1000 / duration_ms as u128) as u64;

    log::info!(
        "vpn speed-test peer={} bytes={} duration_ms={} bps={}",
        peer_ip,
        bytes_len,
        duration_ms,
        bps
    );

    Ok(Json(VpnSpeedTestResponse {
        ok: true,
        bytes: bytes_len,
        duration_ms,
        bps,
    }))
}

#[derive(Serialize)]
pub struct VpnRoutesResponse {
    pub tunnel_ip: Option<String>,
    /// Routes exported to peers. Entries use "exported/prefix -> local/prefix" when
    /// NAT aliasing is active, or just "net/prefix" when no aliasing is needed.
    pub exported_routes: Vec<String>,
    /// Alias subnets announced by remote peers that are currently installed as kernel routes.
    /// Add these on laptops/hosts behind this device:
    ///   ip route add <network> via <kaonic-lan-ip>
    pub remote_installed: Vec<String>,
}

pub async fn get_vpn_routes(State(state): State<AppState>) -> Json<VpnRoutesResponse> {
    let vpn = match &state.vpn {
        Some(vpn) => vpn.snapshot().await,
        None => kaonic_vpn::VpnSnapshot::default(),
    };
    let remote_installed = vpn
        .remote_routes
        .iter()
        .filter(|r| r.installed)
        .map(|r| r.network.clone())
        .collect();
    Json(VpnRoutesResponse {
        tunnel_ip: vpn.local_tunnel_ip,
        exported_routes: vpn.local_routes,
        remote_installed,
    })
}

// ── /api/audio ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PutAudioRequest {
    pub volume: u8,
    pub muted: bool,
}

#[derive(Deserialize)]
pub struct RadioTestRequest {
    pub message: String,
}

#[derive(Deserialize)]
pub struct PutVpnRoutesRequest {
    pub routes: Vec<String>,
}

#[derive(Deserialize)]
pub struct PutVpnAccessRequest {
    pub allow_all_peers: bool,
    pub peers: Vec<String>,
}

#[derive(Deserialize)]
pub struct VpnPingRequest {
    pub address: String,
}

#[derive(Deserialize)]
pub struct VpnSpeedTestRequest {
    pub address: String,
    /// Payload size to request; defaults to 32 KiB.
    #[serde(default)]
    pub bytes: Option<usize>,
}

#[derive(Deserialize)]
pub struct ServiceActionRequest {
    pub unit: String,
}

#[derive(Deserialize)]
pub struct SetSystemCodenameRequest {
    pub codename: String,
}

#[derive(Serialize)]
pub struct AudioSaveResponse {
    pub status: String,
}

#[derive(Serialize)]
pub struct RadioTestResponse {
    pub status: String,
}

#[derive(Serialize)]
pub struct SystemActionResponse {
    pub status: String,
}

#[derive(Serialize)]
pub struct SystemCodenameResponse {
    pub status: String,
    pub codename: String,
}

#[derive(Serialize)]
pub struct VpnAccessResponse {
    pub status: String,
    pub allow_all_peers: bool,
    pub peers: Vec<String>,
}

#[derive(Serialize)]
pub struct VpnPingResponse {
    pub ok: bool,
    pub latency: Option<String>,
}

#[derive(Serialize)]
pub struct VpnSpeedTestResponse {
    pub ok: bool,
    pub bytes: u64,
    pub duration_ms: u64,
    pub bps: u64,
}

struct PingAttempt {
    ok: bool,
    latency: Option<String>,
}

fn normalize_vpn_peer_hashes(peers: &[String]) -> Result<Vec<String>, (StatusCode, String)> {
    let mut peers = peers
        .iter()
        .map(|peer| {
            let raw = peer.trim();
            if raw.is_empty() {
                return Err((StatusCode::BAD_REQUEST, "peer hash is required".into()));
            }
            AddressHash::new_from_hex_string(raw)
                .map(|hash| hash.to_hex_string())
                .map_err(|err| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("invalid peer hash '{raw}': {err:?}"),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    peers.sort();
    peers.dedup();
    Ok(peers)
}

pub async fn get_audio(
    State(state): State<AppState>,
    Path(output): Path<String>,
) -> Result<Json<AudioControlSnapshot>, StatusCode> {
    let output = AudioOutput::parse(&output).ok_or(StatusCode::NOT_FOUND)?;

    state.audio.read(output).await.map(Json).map_err(|err| {
        log::error!("failed to read {output:?} audio state: {err}");
        map_audio_error(&err)
    })
}

pub async fn get_audio_cards(
    State(state): State<AppState>,
) -> Result<Json<Vec<AudioCardSnapshot>>, StatusCode> {
    state.audio.list_cards().await.map(Json).map_err(|err| {
        log::error!("failed to list audio cards: {err}");
        map_audio_error(&err)
    })
}

pub async fn get_audio_control(
    State(state): State<AppState>,
    Path((card_id, output)): Path<(usize, String)>,
) -> Result<Json<AudioControlSnapshot>, StatusCode> {
    let output = AudioOutput::parse(&output).ok_or(StatusCode::NOT_FOUND)?;

    state
        .audio
        .read_control(card_id, output)
        .await
        .map(Json)
        .map_err(|err| {
            log::error!("failed to read card {card_id} {output:?} audio state: {err}");
            map_audio_error(&err)
        })
}

pub async fn put_audio(
    State(state): State<AppState>,
    Path(output): Path<String>,
    Json(request): Json<PutAudioRequest>,
) -> Result<Json<AudioControlSnapshot>, StatusCode> {
    let output = AudioOutput::parse(&output).ok_or(StatusCode::NOT_FOUND)?;
    let next = AudioControlState {
        volume: request.volume,
        muted: request.muted,
    };

    state
        .audio
        .write(output, next)
        .await
        .map(Json)
        .map_err(|err| {
            log::error!("failed to update {output:?} audio state: {err}");
            map_audio_error(&err)
        })
}

pub async fn put_audio_control(
    State(state): State<AppState>,
    Path((card_id, output)): Path<(usize, String)>,
    Json(request): Json<PutAudioRequest>,
) -> Result<Json<AudioControlSnapshot>, StatusCode> {
    let output = AudioOutput::parse(&output).ok_or(StatusCode::NOT_FOUND)?;
    let next = AudioControlState {
        volume: request.volume,
        muted: request.muted,
    };

    state
        .audio
        .write_control(card_id, output, next)
        .await
        .map(Json)
        .map_err(|err| {
            log::error!("failed to update card {card_id} {output:?} audio state: {err}");
            map_audio_error(&err)
        })
}

pub async fn post_audio_control_test(
    State(state): State<AppState>,
    Path((card_id, output)): Path<(usize, String)>,
) -> Result<Json<AudioControlSnapshot>, StatusCode> {
    let output = AudioOutput::parse(&output).ok_or(StatusCode::NOT_FOUND)?;

    state
        .audio
        .test_control(card_id, output)
        .await
        .map(Json)
        .map_err(|err| {
            log::error!("failed to play test sample on card {card_id} {output:?}: {err}");
            map_audio_error(&err)
        })
}

pub async fn post_audio_card_save(
    State(state): State<AppState>,
    Path(card_id): Path<usize>,
) -> Result<Json<AudioSaveResponse>, StatusCode> {
    state
        .audio
        .save_card(card_id)
        .await
        .map(|status| Json(AudioSaveResponse { status }))
        .map_err(|err| {
            log::error!("failed to persist audio settings for card {card_id}: {err}");
            map_audio_error(&err)
        })
}

fn map_audio_error(err: &AudioError) -> StatusCode {
    match err {
        AudioError::InvalidVolume(_) => StatusCode::BAD_REQUEST,
        AudioError::NotFound(_) => StatusCode::NOT_FOUND,
        AudioError::StatePoisoned(_)
        | AudioError::TaskJoin(_)
        | AudioError::CommandIo { .. }
        | AudioError::CommandFailed { .. }
        | AudioError::UnexpectedOutput(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(target_os = "linux")]
fn request_system_reboot() -> Result<String, String> {
    let output = Command::new("systemctl")
        .args(["--no-block", "reboot"])
        .output()
        .map_err(|err| format!("failed to execute systemctl reboot: {err}"))?;

    if output.status.success() {
        return Ok("Reboot requested".into());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(if message.is_empty() {
        "systemctl reboot failed".into()
    } else {
        message
    })
}

#[cfg(not(target_os = "linux"))]
fn request_system_reboot() -> Result<String, String> {
    Ok("Mock reboot requested".into())
}

#[cfg(target_os = "linux")]
fn request_service_restart(unit: &str) -> Result<String, String> {
    let output = Command::new("systemctl")
        .args(["--no-block", "restart", unit])
        .output()
        .map_err(|err| format!("failed to execute systemctl restart {unit}: {err}"))?;

    if output.status.success() {
        return Ok(format!("Restart requested for {unit}"));
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(if message.is_empty() {
        format!("systemctl restart {unit} failed")
    } else {
        message
    })
}

#[cfg(not(target_os = "linux"))]
fn request_service_restart(unit: &str) -> Result<String, String> {
    Ok(format!("Mock restart requested for {unit}"))
}

#[cfg(target_os = "linux")]
async fn request_vpn_ping(address: Ipv4Addr) -> Result<PingAttempt, String> {
    let output = tokio::process::Command::new("ping")
        .args(["-n", "-c", "1", "-W", "3", &address.to_string()])
        .output()
        .await
        .map_err(|err| format!("failed to execute ping {address}: {err}"))?;

    if output.status.success() {
        return Ok(PingAttempt {
            ok: true,
            latency: parse_ping_latency(&output.stdout),
        });
    }

    Ok(PingAttempt {
        ok: false,
        latency: None,
    })
}

#[cfg(not(target_os = "linux"))]
async fn request_vpn_ping(_address: Ipv4Addr) -> Result<PingAttempt, String> {
    Ok(PingAttempt {
        ok: true,
        latency: Some("1.0 ms".into()),
    })
}

fn parse_ping_latency(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .find(|line| line.contains("time=") || line.contains("time<"))
        .and_then(|line| {
            let start = line
                .find("time=")
                .map(|idx| idx + "time=".len())
                .or_else(|| line.find("time").map(|idx| idx + "time".len()))?;
            let tail = &line[start..];
            let end = tail.find(" ms").or_else(|| tail.find("ms"))?;
            let value = tail[..end].trim();
            if value.is_empty() {
                return None;
            }
            Some(format!("{value} ms"))
        })
}

#[cfg(test)]
mod tests {
    use super::parse_ping_latency;

    #[test]
    fn parses_ping_latency_from_output() {
        let output = b"64 bytes from 10.20.78.77: icmp_seq=1 ttl=64 time=12.34 ms\n";
        assert_eq!(parse_ping_latency(output), Some("12.34 ms".into()));
    }

    #[test]
    fn missing_ping_latency_returns_none() {
        let output = b"1 packets transmitted, 0 packets received, 100% packet loss\n";
        assert_eq!(parse_ping_latency(output), None);
    }

    #[test]
    fn parses_busybox_sub_millisecond_latency() {
        let output = b"64 bytes from 10.20.78.77: seq=0 ttl=64 time<1 ms\n";
        assert_eq!(parse_ping_latency(output), Some("<1 ms".into()));
    }
}

// ── /network/wifi actions ─────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct WifiModeForm {
    pub mode: String,
}

#[derive(Deserialize)]
pub struct WifiAntennaForm {
    pub antenna: String,
}

#[derive(Deserialize)]
pub struct WifiConnectForm {
    pub ssid: String,
    pub psk: String,
}

pub async fn post_wifi_mode(
    State(state): State<AppState>,
    Form(form): Form<WifiModeForm>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mode = WifiMode::parse(&form.mode).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            NetworkError::InvalidMode(form.mode).to_string(),
        )
    })?;

    state
        .network
        .set_wifi_mode(mode)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(map_network_error)
}

pub async fn post_wifi_antenna(
    State(state): State<AppState>,
    Form(form): Form<WifiAntennaForm>,
) -> Result<StatusCode, (StatusCode, String)> {
    let antenna = WifiAntenna::parse(&form.antenna).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            NetworkError::InvalidAntenna(form.antenna).to_string(),
        )
    })?;

    state
        .network
        .set_wifi_antenna(antenna)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(map_network_error)
}

pub async fn post_wifi_connect(
    State(state): State<AppState>,
    Form(form): Form<WifiConnectForm>,
) -> Result<StatusCode, (StatusCode, String)> {
    connect_and_remember(&state, form.ssid, form.psk).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Connect and keep the credentials so the operator can switch back later.
async fn connect_and_remember(
    state: &AppState,
    ssid: String,
    psk: String,
) -> Result<(), (StatusCode, String)> {
    let ssid = ssid.trim().to_string();
    if ssid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "ssid is required".into()));
    }
    state
        .network
        .connect_wifi(&ssid, &psk)
        .await
        .map_err(map_network_error)?;
    let now = unix_timestamp_secs();
    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    let existing = settings
        .load_wifi_networks()
        .unwrap_or_default()
        .into_iter()
        .find(|n| n.ssid == ssid);
    let network = kaonic_gateway::network::SavedWifiNetwork {
        ssid: ssid.clone(),
        psk,
        priority: existing.as_ref().map(|n| n.priority).unwrap_or(0),
        created_at: existing.as_ref().map(|n| n.created_at).unwrap_or(now),
        last_used: now,
    };
    if let Err(err) = settings.save_wifi_network(&network) {
        log::warn!("failed to save wifi network {ssid}: {err}");
    }
    let _ = settings.touch_wifi_network(&ssid, now);
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct WifiNetworkRequest {
    pub ssid: String,
    #[serde(default)]
    pub psk: String,
}

/// Saved networks (PSKs are never sent back to the browser).
pub async fn get_wifi_networks(
    State(state): State<AppState>,
) -> Result<Json<Vec<kaonic_gateway::network::SavedWifiNetwork>>, (StatusCode, String)> {
    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    settings
        .load_wifi_networks()
        .map(Json)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

pub async fn post_wifi_network(
    State(state): State<AppState>,
    Json(request): Json<WifiNetworkRequest>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    let ssid = request.ssid.trim().to_string();
    if ssid.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "ssid is required".into()));
    }
    let now = unix_timestamp_secs();
    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    let existing = settings
        .load_wifi_networks()
        .unwrap_or_default()
        .into_iter()
        .find(|n| n.ssid == ssid);
    // An empty PSK means "keep what we have" — the client never sees the
    // stored value, so overwriting it would lose the credential for good.
    let psk = match (request.psk.is_empty(), existing.as_ref()) {
        (true, Some(previous)) => previous.psk.clone(),
        (true, None) => {
            return Err((StatusCode::BAD_REQUEST, "psk is required".into()));
        }
        (false, _) => request.psk,
    };
    settings
        .save_wifi_network(&kaonic_gateway::network::SavedWifiNetwork {
            ssid: ssid.clone(),
            psk,
            priority: existing.as_ref().map(|n| n.priority).unwrap_or(0),
            created_at: existing.as_ref().map(|n| n.created_at).unwrap_or(now),
            last_used: existing.as_ref().map(|n| n.last_used).unwrap_or(0),
        })
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(SystemActionResponse {
        status: format!("Saved {ssid}"),
    }))
}

pub async fn delete_wifi_network(
    State(state): State<AppState>,
    Path(ssid): Path<String>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    settings
        .remove_wifi_network(&ssid)
        .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(Json(SystemActionResponse {
        status: format!("Removed {ssid}"),
    }))
}

/// Connect to an already-saved network by SSID.
pub async fn post_wifi_network_connect(
    State(state): State<AppState>,
    Path(ssid): Path<String>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
    let saved = {
        let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings
            .load_wifi_networks()
            .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
            .into_iter()
            .find(|n| n.ssid == ssid)
            .ok_or((StatusCode::NOT_FOUND, "unknown network".to_string()))?
    };
    connect_and_remember(&state, saved.ssid.clone(), saved.psk).await?;
    Ok(Json(SystemActionResponse {
        status: format!("Connecting to {ssid}"),
    }))
}

pub async fn get_wifi_scan(
    State(state): State<AppState>,
) -> Result<Json<Vec<kaonic_gateway::network::WifiScanEntry>>, (StatusCode, String)> {
    state
        .network
        .scan_wifi()
        .await
        .map(Json)
        .map_err(map_network_error)
}

pub async fn get_network_firewall(
    State(state): State<AppState>,
) -> Result<Json<kaonic_gateway::network::FirewallSnapshotDto>, (StatusCode, String)> {
    state
        .network
        .firewall()
        .await
        .map(Json)
        .map_err(map_network_error)
}

pub async fn get_network_snapshot(
    State(state): State<AppState>,
) -> Result<Json<NetworkSnapshotDto>, (StatusCode, String)> {
    state
        .network
        .snapshot()
        .await
        .map(Json)
        .map_err(map_network_error)
}

fn map_network_error(err: NetworkError) -> (StatusCode, String) {
    let status = match err {
        NetworkError::InvalidMode(_)
        | NetworkError::InvalidAntenna(_)
        | NetworkError::InvalidSsid
        | NetworkError::InvalidPsk
        | NetworkError::MissingStaConfig => StatusCode::BAD_REQUEST,
        NetworkError::StatePoisoned
        | NetworkError::TaskJoin(_)
        | NetworkError::ModeFileWrite(_)
        | NetworkError::CommandIo { .. }
        | NetworkError::CommandFailed { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, err.to_string())
}

// ── /api/status ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StatusResponse {
    vpn_hash: String,
    wlan0_ip: Option<String>,
    usb0_ip: Option<String>,
    network_ports: Vec<NetworkPortStatusDto>,
    system: SystemStatusDto,
    services: Vec<ServiceStatusDto>,
    radio_modules: Vec<RadioModuleConfig>,
    reticulum: ReticulumSnapshotDto,
    vpn: VpnSnapshot,
    rx_frames: [Vec<RxFrameDto>; 2],
    frame_stats: [FrameStatsDto; 2],
}

/// `GET /api/status` — live gateway status: system resources, VPN hash, radio config.
pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(build_status(&state).await)
}

/// Build a `StatusResponse` from shared application state. Used by both the REST handler
/// and the WebSocket streamer.
pub async fn build_status(state: &AppState) -> StatusResponse {
    let radio_modules = state
        .settings
        .lock()
        .ok()
        .and_then(|s| s.load_config().ok())
        .map(|c| c.radio.module_configs.to_vec())
        .unwrap_or_default();
    let services = build_services().await;
    let network_ports = build_network_ports(state, &services);
    let interfaces = build_ws_interfaces();
    let system = build_system_status().await;
    let rx_frames = build_all_radio_frames(state).await;
    let frame_stats = build_all_frame_stats(state);
    let reticulum = build_reticulum_snapshot(state).await;
    let vpn = build_vpn_snapshot(state).await;

    StatusResponse {
        vpn_hash: state.vpn_hash.clone(),
        wlan0_ip: interfaces.wlan0_ip,
        usb0_ip: interfaces.usb0_ip,
        network_ports,
        system,
        services,
        radio_modules,
        reticulum,
        vpn,
        rx_frames,
        frame_stats,
    }
}

pub fn build_ws_interfaces() -> WsInterfacesDto {
    WsInterfacesDto {
        wlan0_ip: read_interface_ipv4("wlan0"),
        usb0_ip: read_interface_ipv4("usb0"),
    }
}

pub async fn build_services() -> Vec<ServiceStatusDto> {
    read_gateway_services().await
}

pub fn build_network_ports(
    state: &AppState,
    services: &[ServiceStatusDto],
) -> Vec<NetworkPortStatusDto> {
    state.network_ports(services)
}

pub async fn build_system_status() -> SystemStatusDto {
    let (ram_used_mb, ram_total_mb) = read_mem_mb();
    let (fs_free_mb, fs_total_mb) = read_fs_mb();
    SystemStatusDto {
        cpu_percent: read_cpu_percent_async().await,
        cpu_freq_mhz: read_cpu_freq_mhz(),
        ram_used_mb,
        ram_total_mb,
        fs_free_mb,
        fs_total_mb,
        os_details: read_os_details(),
        uptime_secs: kaonic_gateway::system_metrics::read_uptime_secs(),
    }
}

pub fn build_frame_stats(state: &AppState, module: usize) -> FrameStatsDto {
    use std::sync::atomic::Ordering;

    let module = module.min(1);
    let rx_bytes = state.frame_stats[module].rx_bytes.load(Ordering::Relaxed);
    let tx_bytes = state.frame_stats[module].tx_bytes.load(Ordering::Relaxed);
    let (rx_bps, tx_bps) = state.frame_stats[module].rates(rx_bytes, tx_bytes);
    FrameStatsDto {
        rx_frames: state.frame_stats[module].rx_frames.load(Ordering::Relaxed),
        rx_bytes,
        rx_bps,
        tx_frames: state.frame_stats[module].tx_frames.load(Ordering::Relaxed),
        tx_bytes,
        tx_bps,
        last_rssi: if state.frame_stats[module].rx_frames.load(Ordering::Relaxed) > 0 {
            Some(state.frame_stats[module].last_rssi.load(Ordering::Relaxed) as i8)
        } else {
            None
        },
    }
}

pub fn build_all_frame_stats(state: &AppState) -> [FrameStatsDto; 2] {
    [build_frame_stats(state, 0), build_frame_stats(state, 1)]
}

pub async fn build_radio_frames(state: &AppState, module: usize) -> Vec<RxFrameDto> {
    let module = module.min(1);
    state.rx_buffers[module]
        .lock()
        .await
        .iter()
        .cloned()
        .collect()
}

pub async fn build_all_radio_frames(state: &AppState) -> [Vec<RxFrameDto>; 2] {
    [
        build_radio_frames(state, 0).await,
        build_radio_frames(state, 1).await,
    ]
}

pub async fn build_reticulum_snapshot(state: &AppState) -> ReticulumSnapshotDto {
    state.reticulum.snapshot().await
}

pub async fn build_ws_reticulum_snapshot(state: &AppState) -> WsReticulumSnapshotDto {
    let snapshot = state.reticulum.snapshot().await;
    WsReticulumSnapshotDto {
        interface_stats: snapshot.interface_stats,
        incoming_links: snapshot.incoming_links,
        outgoing_links: snapshot.outgoing_links,
    }
}

pub async fn build_vpn_snapshot(state: &AppState) -> VpnSnapshot {
    let mut snapshot = match &state.vpn {
        Some(vpn) => vpn.snapshot().await,
        None => VpnSnapshot::default(),
    };
    kaonic_gateway::state::present_uplink_peer(state, &mut snapshot);
    kaonic_gateway::state::present_peer_names(state, &mut snapshot);
    snapshot
}

