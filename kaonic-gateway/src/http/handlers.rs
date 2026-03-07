use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use kaonic_settings::{GatewayConfig, KaonicCtrlConfig, Settings};

use super::SharedSettings;

/// `GET /api/settings` — return the full gateway config.
pub async fn get_settings(
    State(settings): State<SharedSettings>,
) -> Result<Json<GatewayConfig>, StatusCode> {
    let s = settings.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.load_config()
        .map(Json)
        .map_err(|err| {
            log::error!("failed to load settings: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// `PUT /api/settings` — replace the full gateway config.
pub async fn put_settings(
    State(settings): State<SharedSettings>,
    Json(config): Json<GatewayConfig>,
) -> StatusCode {
    let s = settings.lock().unwrap_or_else(|e: std::sync::PoisonError<std::sync::MutexGuard<Settings>>| e.into_inner());
    match s.save_config(&config) {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(err) => {
            log::error!("failed to save settings: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// `GET /api/settings/radio` — return the kaonic-ctrl radio config (or `null`).
pub async fn get_radio(
    State(settings): State<SharedSettings>,
) -> Result<Json<Option<KaonicCtrlConfig>>, StatusCode> {
    let s = settings.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.load_config()
        .map(|c| Json(c.kaonic_ctrl_config))
        .map_err(|err| {
            log::error!("failed to load radio settings: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// `PUT /api/settings/radio` — update only the kaonic-ctrl radio config.
pub async fn put_radio(
    State(settings): State<SharedSettings>,
    Json(radio_config): Json<KaonicCtrlConfig>,
) -> StatusCode {
    let s = settings.lock().unwrap_or_else(|e: std::sync::PoisonError<std::sync::MutexGuard<Settings>>| e.into_inner());
    let mut config = match s.load_config() {
        Ok(c) => c,
        Err(err) => {
            log::error!("failed to load config: {err}");
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };
    config.kaonic_ctrl_config = Some(radio_config);
    match s.save_config(&config) {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(err) => {
            log::error!("failed to save radio settings: {err}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
