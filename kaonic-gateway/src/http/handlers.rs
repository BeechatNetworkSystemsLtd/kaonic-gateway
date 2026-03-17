use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use kaonic_vpn::{GatewayConfig, KaonicCtrlConfig};
use serde::Serialize;

use super::AppState;

/// `GET /api/settings` — return the full gateway config.
pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<GatewayConfig>, StatusCode> {
    let s = state.settings.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.load_config()
        .map(Json)
        .map_err(|err| {
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
) -> Result<Json<Option<KaonicCtrlConfig>>, StatusCode> {
    let s = state.settings.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.load_config()
        .map(|c| Json(c.kaonic_ctrl_configs.into_iter().find(|m| m.module == module)))
        .map_err(|err| {
            log::error!("failed to load radio settings: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// `PUT /api/settings/radio/:module` — save config for one RF module.
pub async fn put_radio(
    State(state): State<AppState>,
    Path(module): Path<usize>,
    Json(mut radio_config): Json<KaonicCtrlConfig>,
) -> StatusCode {
    radio_config.module = module;
    let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
    match s.save_module_config(&radio_config) {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(err) => {
            log::error!("failed to save radio settings for module {module}: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
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
}

#[derive(Serialize)]
pub struct StatusResponse {
    vpn_hash: String,
    atak_bridges: Vec<AtakBridgeStatus>,
    system: SystemStatus,
    radio_modules: Vec<KaonicCtrlConfig>,
}

/// `GET /api/status` — live gateway status: ATAK counters, system resources, VPN hash, radio config.
pub async fn get_status(State(state): State<AppState>) -> Json<StatusResponse> {
    use std::sync::atomic::Ordering;

    let atak_bridges = state.atak_metrics.iter().map(|m| AtakBridgeStatus {
        port: m.port,
        dest_hash: m.dest_hash.get().cloned().unwrap_or_default(),
        rx_packets: m.rx_packets.load(Ordering::Relaxed),
        tx_packets: m.tx_packets.load(Ordering::Relaxed),
    }).collect();

    let radio_modules = state.settings.lock().ok()
        .and_then(|s| s.load_config().ok())
        .map(|c| c.kaonic_ctrl_configs)
        .unwrap_or_default();

    Json(StatusResponse {
        vpn_hash: state.vpn_hash.clone(),
        atak_bridges,
        system: read_system_status(),
        radio_modules,
    })
}

fn read_system_status() -> SystemStatus {
    let mut s = read_mem_status();
    s.cpu_percent = read_cpu_percent();
    s
}

fn read_mem_status() -> SystemStatus {
    let Ok(data) = std::fs::read_to_string("/proc/meminfo") else {
        return SystemStatus { cpu_percent: 0.0, ram_used_mb: 0, ram_total_mb: 0 };
    };
    let mut total = 0u64;
    let mut available = 0u64;
    for line in data.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.split_whitespace().next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    SystemStatus {
        cpu_percent: 0.0,
        ram_total_mb: total / 1024,
        ram_used_mb: total.saturating_sub(available) / 1024,
    }
}

fn read_cpu_percent() -> f32 {
    // Read /proc/stat twice with a short sleep to calculate CPU usage delta.
    // We do a single-sample approximation using idle vs total from one read,
    // which is sufficient for a dashboard display.
    fn parse_stat() -> Option<(u64, u64)> {
        let data = std::fs::read_to_string("/proc/stat").ok()?;
        let line = data.lines().next()?; // "cpu  ..."
        let vals: Vec<u64> = line.split_whitespace()
            .skip(1)
            .filter_map(|v| v.parse().ok())
            .collect();
        if vals.len() < 4 { return None; }
        let total: u64 = vals.iter().sum();
        let idle = vals[3];
        Some((idle, total))
    }

    let Some((idle1, total1)) = parse_stat() else { return 0.0 };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let Some((idle2, total2)) = parse_stat() else { return 0.0 };

    let total_diff = total2.saturating_sub(total1) as f32;
    let idle_diff  = idle2.saturating_sub(idle1) as f32;
    if total_diff == 0.0 { return 0.0; }
    ((total_diff - idle_diff) / total_diff * 100.0 * 10.0).round() / 10.0
}

