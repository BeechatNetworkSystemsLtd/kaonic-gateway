use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::Json;
use kaonic_gateway::app_types::RxFrameDto;
use kaonic_gateway::audio::{AudioControlSnapshot, AudioControlState, AudioError, AudioOutput};
use kaonic_gateway::config::GatewayConfig;
use kaonic_gateway::network::{NetworkError, WifiMode};
use kaonic_gateway::radio::RadioModuleConfig;
use kaonic_gateway::system_metrics::{
    read_cpu_percent_async, read_fs_mb, read_mem_mb, read_os_details,
};
use serde::{Deserialize, Serialize};

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

// ── /api/audio ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PutAudioRequest {
    pub volume: u8,
    pub muted: bool,
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

fn map_audio_error(err: &AudioError) -> StatusCode {
    match err {
        AudioError::InvalidVolume(_) => StatusCode::BAD_REQUEST,
        AudioError::StatePoisoned(_)
        | AudioError::TaskJoin(_)
        | AudioError::CommandIo { .. }
        | AudioError::CommandFailed { .. }
        | AudioError::UnexpectedOutput(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
    radio_modules: Vec<RadioModuleConfig>,
    rx_frames: [Vec<RxFrameDto>; 2],
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

    StatusResponse {
        vpn_hash: state.vpn_hash.clone(),
        atak_bridges,
        system: read_system_status_async().await,
        radio_modules,
        rx_frames,
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
