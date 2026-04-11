use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tokio::time::{interval, Duration};

use super::handlers::build_status;
use super::AppState;

/// `GET /api/ws/status` — WebSocket that pushes a `StatusResponse` JSON frame every second.
pub async fn ws_status(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut tick = interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let status = build_status(&state).await;
        match serde_json::to_string(&status) {
            Ok(json) => {
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                log::error!("ws_status: serialize error: {e}");
                break;
            }
        }
    }
}
