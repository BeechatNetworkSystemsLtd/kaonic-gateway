use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{interval, Duration};

use kaonic_gateway::app_types::{FrameStatsDto, RxFrameDto, WsRadioFramesDto, WsStatusEvent};

use super::handlers::{
    build_frame_stats, build_network_ports, build_radio_frames, build_services,
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
    // Remote snapshots are pushed on change (debounced) rather than polled,
    // so pairing/link state reaches the map within a fraction of a second.
    if let Some(remote) = state.remote.clone() {
        let state = state.clone();
        tokio::spawn(async move {
            let mut changes = remote.subscribe_changes();
            loop {
                match changes.recv().await {
                    Ok(()) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
                // Drain anything that arrived during the debounce window.
                while changes.try_recv().is_ok() {}
                if state.ws_events.receiver_count() == 0 {
                    continue;
                }
                let mut snapshot = remote.snapshot();
                kaonic_gateway::remote::enrich_snapshot(&state, &mut snapshot).await;
                let _ = state.ws_events.send(WsStatusEvent::Remote(snapshot));
            }
        });
    }

    // CPU/RAM come from /proc and are cheap, so they get their own fast tick
    // for a live-looking chart; the heavy snapshot (systemctl, VPN, reticulum)
    // stays on the slow one.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                if state.ws_events.receiver_count() == 0 {
                    continue;
                }
                let _ = state
                    .ws_events
                    .send(WsStatusEvent::System(build_system_status().await));
            }
        });
    }

    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            // Nobody is watching: skip the whole snapshot (systemctl, /proc, VPN,
            // reticulum) so an idle gateway stays idle.
            if state.ws_events.receiver_count() == 0 {
                continue;
            }
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
    let services = build_services().await;
    let network_ports = build_network_ports(state, &services);
    let _ = state
        .ws_events
        .send(WsStatusEvent::Interfaces(build_ws_interfaces()));
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
    if let Some(remote) = state.remote.as_ref() {
        let mut snapshot = remote.snapshot();
        kaonic_gateway::remote::enrich_snapshot(state, &mut snapshot).await;
        let _ = state.ws_events.send(WsStatusEvent::Remote(snapshot));
    }
}

async fn initial_events(state: &AppState) -> Vec<WsStatusEvent> {
    let services = build_services().await;
    let network_ports = build_network_ports(state, &services);
    let mut events = vec![
        WsStatusEvent::Interfaces(build_ws_interfaces()),
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
    ];
    if let Some(remote) = state.remote.as_ref() {
        let mut snapshot = remote.snapshot();
        kaonic_gateway::remote::enrich_snapshot(state, &mut snapshot).await;
        events.push(WsStatusEvent::Remote(snapshot));
    }
    events
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
