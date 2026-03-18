pub mod frames;
pub mod pages;

use axum::{Router, routing::get};

/// Returns the dashboard Axum router (stateless — all data ops go through the API).
///
/// Routes:
/// - `GET /`            — main dashboard HTML (JS polls `/api/frames`)
/// - `GET /settings`    — settings page HTML (JS fetches/saves via `/api/settings`)
pub fn router() -> Router {
    Router::new()
        .route("/", get(frames::get_dashboard))
        .route("/settings", get(pages::get_settings))
        .route("/update", get(pages::get_update))
}
