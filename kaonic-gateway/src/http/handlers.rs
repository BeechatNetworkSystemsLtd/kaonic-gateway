use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::Json;
use kaonic_vpn::VpnSnapshot;
use kaonic_gateway::app_types::{
    FrameStatsDto, NetworkSnapshotDto, ReticulumSnapshotDto, RxFrameDto, ServiceStatusDto,
};
use kaonic_gateway::audio::{
    AudioCardSnapshot, AudioControlSnapshot, AudioControlState, AudioError, AudioOutput,
};
use kaonic_gateway::config::GatewayConfig;
use kaonic_gateway::network::{NetworkError, WifiMode};
use kaonic_gateway::radio::{transmit_test_frame, RadioModuleConfig};
use kaonic_gateway::system_metrics::{
    is_gateway_service_unit, read_cpu_percent_async, read_fs_mb, read_gateway_services,
    read_mem_mb, read_os_details,
};
use serde::{Deserialize, Serialize};
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

    transmit_test_frame(state.radio_client.clone(), module, message.as_bytes())
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
    atak_bridges: Vec<AtakBridgeStatus>,
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
        atak_bridges,
        system: read_system_status_async().await,
        services: read_gateway_services(),
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
