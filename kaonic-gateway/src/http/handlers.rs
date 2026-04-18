use axum::extract::{Form, Path, State};
use axum::http::{header, StatusCode};
use axum::{response::IntoResponse, Json};
use kaonic_gateway::app_types::{
    FrameStatsDto, NetworkPortStatusDto, NetworkSnapshotDto, ReticulumSnapshotDto, RxFrameDto,
    ServiceStatusDto,
};
use kaonic_gateway::audio::{
    AudioCardSnapshot, AudioControlSnapshot, AudioControlState, AudioError, AudioOutput,
};
use kaonic_gateway::config::GatewayConfig;
use kaonic_gateway::network::{read_interface_ipv4, NetworkError, WifiMode};
use kaonic_gateway::radio::{transmit_test_frame, RadioModuleConfig};
use kaonic_gateway::system_metrics::{
    is_gateway_service_unit, read_cpu_percent_async, read_fs_mb, read_gateway_services,
    read_mem_mb, read_os_details,
};
use kaonic_vpn::VpnSnapshot;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::process::Command;

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
    let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    match s.save_config(&config) {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(err) => {
            log::error!("failed to save settings: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
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

    if let Some(client) = state.radio_client.clone() {
        let mut client = client.lock().await;
        match client.set_radio_config(module, cfg.radio_config).await {
            Ok(_) => log::info!("put_radio: radio_config applied to module {module}"),
            Err(e) => {
                log::error!("put_radio: set_radio_config failed for module {module}: {e:?}")
            }
        }
        match client.set_modulation(module, cfg.modulation).await {
            Ok(_) => log::info!("put_radio: modulation applied to module {module}"),
            Err(e) => log::error!("put_radio: set_modulation failed for module {module}: {e:?}"),
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

pub async fn put_vpn_routes(
    State(state): State<AppState>,
    Json(request): Json<PutVpnRoutesRequest>,
) -> Result<Json<SystemActionResponse>, (StatusCode, String)> {
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
pub struct VpnPingRequest {
    pub address: String,
}

#[derive(Deserialize)]
pub struct ServiceActionRequest {
    pub unit: String,
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
pub struct VpnPingResponse {
    pub ok: bool,
    pub latency: Option<String>,
}

struct PingAttempt {
    ok: bool,
    latency: Option<String>,
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

pub async fn post_wifi_connect(
    State(state): State<AppState>,
    Form(form): Form<WifiConnectForm>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .network
        .connect_wifi(&form.ssid, &form.psk)
        .await
        .map(|_| StatusCode::NO_CONTENT)
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
pub struct AtakBridgeStatus {
    port: u16,
    dest_hash: String,
    rx_packets: u64,
    tx_packets: u64,
}

#[derive(Serialize)]
pub struct SystemStatus {
    cpu_percent: f32,
    ram_used_mb: u64,
    ram_total_mb: u64,
    fs_free_mb: u64,
    fs_total_mb: u64,
    os_details: String,
}

#[derive(Serialize)]
pub struct StatusResponse {
    vpn_hash: String,
    wlan0_ip: Option<String>,
    usb0_ip: Option<String>,
    atak_bridges: Vec<AtakBridgeStatus>,
    network_ports: Vec<NetworkPortStatusDto>,
    system: SystemStatus,
    services: Vec<ServiceStatusDto>,
    radio_modules: Vec<RadioModuleConfig>,
    reticulum: ReticulumSnapshotDto,
    vpn: VpnSnapshot,
    rx_frames: [Vec<RxFrameDto>; 2],
    frame_stats: [FrameStatsDto; 2],
}

/// `GET /api/status` — live gateway status: ATAK counters, system resources, VPN hash, radio config.
pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(build_status(&state).await)
}

/// Build a `StatusResponse` from shared application state. Used by both the REST handler
/// and the WebSocket streamer.
pub async fn build_status(state: &AppState) -> StatusResponse {
    use std::sync::atomic::Ordering;

    let atak_bridges = state
        .atak_metrics
        .iter()
        .map(|m| AtakBridgeStatus {
            port: m.port,
            dest_hash: m.dest_hash.get().cloned().unwrap_or_default(),
            rx_packets: m.rx_packets.load(Ordering::Relaxed),
            tx_packets: m.tx_packets.load(Ordering::Relaxed),
        })
        .collect();

    let radio_modules = state
        .settings
        .lock()
        .ok()
        .and_then(|s| s.load_config().ok())
        .map(|c| c.radio.module_configs.to_vec())
        .unwrap_or_default();
    let services = read_gateway_services();
    let network_ports = state.network_ports(&services);

    let rx_frames = [
        state.rx_buffers[0].lock().await.iter().cloned().collect(),
        state.rx_buffers[1].lock().await.iter().cloned().collect(),
    ];
    let frame_stats = [
        FrameStatsDto {
            rx_frames: state.frame_stats[0].rx_frames.load(Ordering::Relaxed),
            rx_bytes: state.frame_stats[0].rx_bytes.load(Ordering::Relaxed),
            tx_frames: state.frame_stats[0].tx_frames.load(Ordering::Relaxed),
            tx_bytes: state.frame_stats[0].tx_bytes.load(Ordering::Relaxed),
            last_rssi: if state.frame_stats[0].rx_frames.load(Ordering::Relaxed) > 0 {
                Some(state.frame_stats[0].last_rssi.load(Ordering::Relaxed) as i8)
            } else {
                None
            },
        },
        FrameStatsDto {
            rx_frames: state.frame_stats[1].rx_frames.load(Ordering::Relaxed),
            rx_bytes: state.frame_stats[1].rx_bytes.load(Ordering::Relaxed),
            tx_frames: state.frame_stats[1].tx_frames.load(Ordering::Relaxed),
            tx_bytes: state.frame_stats[1].tx_bytes.load(Ordering::Relaxed),
            last_rssi: if state.frame_stats[1].rx_frames.load(Ordering::Relaxed) > 0 {
                Some(state.frame_stats[1].last_rssi.load(Ordering::Relaxed) as i8)
            } else {
                None
            },
        },
    ];
    let reticulum = state.reticulum.snapshot().await;
    let vpn = match &state.vpn {
        Some(vpn) => vpn.snapshot().await,
        None => VpnSnapshot::default(),
    };

    StatusResponse {
        vpn_hash: state.vpn_hash.clone(),
        wlan0_ip: read_interface_ipv4("wlan0"),
        usb0_ip: read_interface_ipv4("usb0"),
        atak_bridges,
        network_ports,
        system: read_system_status_async().await,
        services,
        radio_modules,
        reticulum,
        vpn,
        rx_frames,
        frame_stats,
    }
}

async fn read_system_status_async() -> SystemStatus {
    let (ram_used_mb, ram_total_mb) = read_mem_mb();
    let (fs_free_mb, fs_total_mb) = read_fs_mb();
    SystemStatus {
        cpu_percent: read_cpu_percent_async().await,
        ram_used_mb,
        ram_total_mb,
        fs_free_mb,
        fs_total_mb,
        os_details: read_os_details(),
    }
}
