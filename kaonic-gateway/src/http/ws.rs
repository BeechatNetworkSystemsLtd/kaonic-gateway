use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{interval, Duration};

use kaonic_gateway::app_types::{FrameStatsDto, RxFrameDto, WsRadioFramesDto, WsStatusEvent};

use super::handlers::{
    build_atak_bridges, build_frame_stats, build_network_ports, build_radio_frames, build_services,
    build_system_status, build_vpn_snapshot, build_ws_interfaces, build_ws_reticulum_snapshot,
};
use super::AppState;

/// `GET /api/ws/status` — WebSocket that pushes typed JSON events for partial live updates.
pub async fn ws_status(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.ws_events.subscribe();

    for event in initial_events(&state).await {
        if send_event(&mut socket, &event).await.is_err() {
            return;
        }
    }

    loop {
        match rx.recv().await {
            Ok(event) => {
                if send_event(&mut socket, &event).await.is_err() {
                    break;
                }
            }
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        }
    }
}

pub fn spawn_status_publishers(state: AppState) {
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            publish_periodic_events(&state).await;
        }
    });
}

pub fn publish_radio_frames(
    state: &AppState,
    module: usize,
    frames: Vec<RxFrameDto>,
    stats: FrameStatsDto,
) {
    let _ = state
        .ws_events
        .send(WsStatusEvent::RadioFrames(WsRadioFramesDto {
            module: module.min(1),
            frames,
            stats,
        }));
}

async fn publish_periodic_events(state: &AppState) {
    let services = build_services();
    let network_ports = build_network_ports(state, &services);
    let _ = state
        .ws_events
        .send(WsStatusEvent::Interfaces(build_ws_interfaces()));
    let _ = state
        .ws_events
        .send(WsStatusEvent::AtakBridges(build_atak_bridges(state)));
    let _ = state
        .ws_events
        .send(WsStatusEvent::System(build_system_status().await));
    let _ = state.ws_events.send(WsStatusEvent::Services(services));
    let _ = state
        .ws_events
        .send(WsStatusEvent::NetworkPorts(network_ports));
    let _ = state
        .ws_events
        .send(WsStatusEvent::Vpn(build_vpn_snapshot(state).await));
    let _ = state.ws_events.send(WsStatusEvent::Reticulum(
        build_ws_reticulum_snapshot(state).await,
    ));
}

async fn initial_events(state: &AppState) -> Vec<WsStatusEvent> {
    let services = build_services();
    let network_ports = build_network_ports(state, &services);
    vec![
        WsStatusEvent::Interfaces(build_ws_interfaces()),
        WsStatusEvent::AtakBridges(build_atak_bridges(state)),
        WsStatusEvent::System(build_system_status().await),
        WsStatusEvent::Services(services),
        WsStatusEvent::NetworkPorts(network_ports),
        WsStatusEvent::Vpn(build_vpn_snapshot(state).await),
        WsStatusEvent::Reticulum(build_ws_reticulum_snapshot(state).await),
        WsStatusEvent::RadioFrames(WsRadioFramesDto {
            module: 0,
            frames: build_radio_frames(state, 0).await,
            stats: build_frame_stats(state, 0),
        }),
        WsStatusEvent::RadioFrames(WsRadioFramesDto {
            module: 1,
            frames: build_radio_frames(state, 1).await,
            stats: build_frame_stats(state, 1),
        }),
    ]
}

async fn send_event(socket: &mut WebSocket, event: &WsStatusEvent) -> Result<(), ()> {
    match serde_json::to_string(event) {
        Ok(json) => socket
            .send(Message::Text(json.into()))
            .await
            .map_err(|_| ()),
        Err(err) => {
            log::error!("ws_status: serialize error: {err}");
            Err(())
        }
    }
}
