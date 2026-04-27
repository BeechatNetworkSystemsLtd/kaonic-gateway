mod audio;
mod codec;
mod config;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::audio::{frame_samples, AudioDevices};
use crate::codec::{RxCodec, TxCodec};
use crate::config::{load_or_create_config, resolve_config_path, save_config, PluginConfig};

const PACKET_MAGIC: [u8; 4] = *b"KPT1";
const PACKET_HEADER_LEN: usize = 8;
const PLAYBACK_BUFFER_FRAMES: usize = 64;
const BROWSER_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Kaonic Audio PTT</title>
  <style>
    :root {
      color-scheme: dark;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    body {
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      background: #0f172a;
      color: #e2e8f0;
    }
    .app {
      width: min(92vw, 30rem);
      padding: 1.5rem;
      text-align: center;
      background: #111827;
      border: 1px solid #1f2937;
      border-radius: 1.25rem;
      box-shadow: 0 24px 48px rgba(0,0,0,0.35);
    }
    h1 {
      margin: 0 0 0.5rem;
      font-size: 1.8rem;
    }
    .lead {
      margin: 0 0 1rem;
      color: #94a3b8;
    }
    .config {
      display: flex;
      gap: 0.5rem;
      margin: 0 0 1rem;
    }
    .config input {
      flex: 1;
      min-width: 0;
      border: 1px solid #334155;
      background: #020617;
      color: #e2e8f0;
      border-radius: 0.8rem;
      padding: 0.9rem 1rem;
      font-size: 1rem;
    }
    .config button {
      border: 0;
      border-radius: 0.8rem;
      padding: 0.9rem 1rem;
      font-size: 1rem;
      font-weight: 700;
      color: white;
      background: #2563eb;
    }
    .mic-btn {
      width: 15rem;
      height: 15rem;
      max-width: 72vw;
      max-height: 72vw;
      border: 0;
      border-radius: 999px;
      background: linear-gradient(180deg, #ef4444, #b91c1c);
      color: white;
      font-size: 1.35rem;
      font-weight: 800;
      box-shadow: 0 20px 40px rgba(239, 68, 68, 0.35);
      touch-action: none;
      user-select: none;
    }
    .mic-btn.is-active {
      transform: scale(0.97);
      background: linear-gradient(180deg, #fb7185, #e11d48);
      box-shadow: 0 0 0 0.6rem rgba(251, 113, 133, 0.18);
    }
    .status {
      margin-top: 1rem;
      min-height: 2.5rem;
      color: #cbd5e1;
      font-size: 0.98rem;
      white-space: pre-line;
    }
    .hint {
      margin-top: 0.75rem;
      color: #94a3b8;
      font-size: 0.9rem;
    }
  </style>
</head>
<body>
  <main class="app">
    <h1>Kaonic Audio PTT</h1>
    <p class="lead">Hold the microphone button to talk from this browser.</p>
    <div class="config">
      <input id="remote-peer" type="text" placeholder="Remote peer, e.g. 10.8.0.42:6790" />
      <button id="save-peer" type="button">Save</button>
    </div>
    <button id="mic-btn" class="mic-btn" type="button">Hold to Talk</button>
    <div id="status" class="status">Loading…</div>
    <div class="hint">Remote Kaonic should run audio-ptt and listen on its media port.</div>
  </main>
  <script>
    (function () {
      const SAMPLE_RATE = 16000;
      const FRAME_SAMPLES = 320;
      const micBtn = document.getElementById('mic-btn');
      const statusEl = document.getElementById('status');
      const remotePeerInput = document.getElementById('remote-peer');
      const savePeerBtn = document.getElementById('save-peer');

      let stream = null;
      let audioContext = null;
      let sourceNode = null;
      let processor = null;
      let muteNode = null;
      let ws = null;
      let queue = [];
      let starting = false;
      let active = false;
      let currentConfig = null;

      function setStatus(text) {
        statusEl.textContent = text;
      }

      async function loadStatus() {
        const resp = await fetch('/api/status');
        const data = await resp.json();
        currentConfig = data.config;
        remotePeerInput.value = data.config.remote_peer || '';
        setStatus(
          'Remote peer: ' + (data.config.remote_peer || 'not configured') +
          '\nPlayback device: ' + data.config.playback_device +
          '\nCapture device: ' + data.config.capture_device
        );
      }

      async function saveRemotePeer() {
        if (!currentConfig) {
          await loadStatus();
        }
        const payload = {
          remote_peer: remotePeerInput.value.trim() || null,
          capture_device: currentConfig.capture_device,
          playback_device: currentConfig.playback_device
        };
        const resp = await fetch('/api/config', {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(payload)
        });
        const data = await resp.json().catch(function () { return {}; });
        if (!resp.ok) {
          throw new Error(data.detail || ('HTTP ' + resp.status));
        }
        await loadStatus();
        setStatus((data.detail || 'Remote peer saved') + '\nRemote peer: ' + (payload.remote_peer || 'not configured'));
      }

      function downsample(input, inputRate, outputRate) {
        if (inputRate === outputRate) {
          return input;
        }
        const ratio = inputRate / outputRate;
        const outputLength = Math.max(1, Math.round(input.length / ratio));
        const output = new Float32Array(outputLength);
        let offsetResult = 0;
        let offsetBuffer = 0;
        while (offsetResult < output.length) {
          const nextOffsetBuffer = Math.min(input.length, Math.round((offsetResult + 1) * ratio));
          let accum = 0;
          let count = 0;
          for (let i = offsetBuffer; i < nextOffsetBuffer; i += 1) {
            accum += input[i];
            count += 1;
          }
          output[offsetResult] = count ? (accum / count) : 0;
          offsetResult += 1;
          offsetBuffer = nextOffsetBuffer;
        }
        return output;
      }

      function appendSamples(floatSamples, sourceRate) {
        const resampled = downsample(floatSamples, sourceRate, SAMPLE_RATE);
        for (let i = 0; i < resampled.length; i += 1) {
          const sample = Math.max(-1, Math.min(1, resampled[i]));
          queue.push(sample < 0 ? sample * 0x8000 : sample * 0x7fff);
        }
        while (queue.length >= FRAME_SAMPLES && ws && ws.readyState === WebSocket.OPEN) {
          const frame = new Int16Array(FRAME_SAMPLES);
          for (let i = 0; i < FRAME_SAMPLES; i += 1) {
            frame[i] = queue[i];
          }
          queue = queue.slice(FRAME_SAMPLES);
          ws.send(frame.buffer);
        }
      }

      async function startTalk() {
        if (active || starting) { return; }
        if (!remotePeerInput.value.trim()) {
          setStatus('Configure remote peer first.');
          return;
        }
        starting = true;
        try {
          await saveRemotePeer();
          stream = await navigator.mediaDevices.getUserMedia({
            audio: {
              channelCount: 1,
              echoCancellation: true,
              noiseSuppression: true,
              autoGainControl: true
            }
          });
          audioContext = new (window.AudioContext || window.webkitAudioContext)();
          sourceNode = audioContext.createMediaStreamSource(stream);
          processor = audioContext.createScriptProcessor(4096, 1, 1);
          muteNode = audioContext.createGain();
          muteNode.gain.value = 0;
          ws = new WebSocket((location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + '/ws/browser-ptt');
          ws.binaryType = 'arraybuffer';

          await new Promise(function (resolve, reject) {
            ws.onopen = resolve;
            ws.onerror = function () { reject(new Error('WebSocket connection failed')); };
          });

          processor.onaudioprocess = function (event) {
            if (!active || !ws || ws.readyState !== WebSocket.OPEN) { return; }
            const input = event.inputBuffer.getChannelData(0);
            appendSamples(input, audioContext.sampleRate);
          };

          sourceNode.connect(processor);
          processor.connect(muteNode);
          muteNode.connect(audioContext.destination);
          active = true;
          micBtn.classList.add('is-active');
          setStatus('Talking… release to stop');
        } catch (err) {
          setStatus('Error: ' + (err && err.message ? err.message : err));
          await stopTalk();
        } finally {
          starting = false;
        }
      }

      async function stopTalk() {
        queue = [];
        active = false;
        micBtn.classList.remove('is-active');
        if (processor) {
          processor.onaudioprocess = null;
          try { processor.disconnect(); } catch (_) {}
          processor = null;
        }
        if (sourceNode) {
          try { sourceNode.disconnect(); } catch (_) {}
          sourceNode = null;
        }
        if (muteNode) {
          try { muteNode.disconnect(); } catch (_) {}
          muteNode = null;
        }
        if (audioContext) {
          try { await audioContext.close(); } catch (_) {}
          audioContext = null;
        }
        if (stream) {
          stream.getTracks().forEach(function (track) { track.stop(); });
          stream = null;
        }
        if (ws) {
          try { ws.close(); } catch (_) {}
          ws = null;
        }
        await loadStatus().catch(function () {});
      }

      micBtn.addEventListener('pointerdown', function (event) {
        event.preventDefault();
        micBtn.setPointerCapture(event.pointerId);
        startTalk();
      });
      micBtn.addEventListener('pointerup', function () { stopTalk(); });
      micBtn.addEventListener('pointercancel', function () { stopTalk(); });
      micBtn.addEventListener('lostpointercapture', function () { stopTalk(); });
      savePeerBtn.addEventListener('click', function () {
        saveRemotePeer().catch(function (err) {
          setStatus('Error: ' + (err && err.message ? err.message : err));
        });
      });

      window.addEventListener('beforeunload', function () {
        stopTalk();
      });

      loadStatus().catch(function (err) {
        setStatus('Error: ' + (err && err.message ? err.message : err));
      });
    })();
  </script>
</body>
</html>
"#;

#[derive(Parser, Debug)]
#[command(name = "kaonic-audio-ptt", version)]
struct Command {
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Clone)]
struct AppState {
    config_path: Arc<PathBuf>,
    config: Arc<RwLock<PluginConfig>>,
    media_socket: Arc<UdpSocket>,
    tx_session: Arc<Mutex<Option<ActiveTx>>>,
    stats: Arc<Stats>,
    playback_tx: mpsc::Sender<Vec<i16>>,
}

enum ActiveTx {
    Alsa(TxControl),
    Browser,
}

struct TxControl {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct Stats {
    transmitting: AtomicBool,
    tx_packets: AtomicU64,
    rx_packets: AtomicU64,
    tx_bytes: AtomicU64,
    rx_bytes: AtomicU64,
    dropped_rx_while_talking: AtomicU64,
    seq: AtomicU64,
    last_remote: Mutex<Option<SocketAddr>>,
}

#[derive(Debug, Serialize)]
struct StatusSnapshot {
    config: PluginConfig,
    transmitting: bool,
    active_source: Option<&'static str>,
    tx_packets: u64,
    rx_packets: u64,
    tx_bytes: u64,
    rx_bytes: u64,
    dropped_rx_while_talking: u64,
    last_remote: Option<SocketAddr>,
}

#[derive(Debug, Deserialize)]
struct ConfigUpdate {
    remote_peer: Option<SocketAddr>,
    capture_device: String,
    playback_device: String,
}

#[derive(Debug, Serialize)]
struct MessageResponse {
    detail: String,
}

#[derive(Debug, Serialize)]
struct ConfigResponse {
    config: PluginConfig,
}

#[derive(Debug, Serialize)]
struct AudioDevicesResponse {
    devices: AudioDevices,
}

#[tokio::main]
async fn main() -> Result<(), std::process::ExitCode> {
    env_logger::Builder::new()
        .parse_filters("info,kaonic_audio_ptt=debug")
        .parse_default_env()
        .init();

    let cmd = Command::parse();
    let config_path = resolve_config_path(cmd.config).unwrap_or_else(|err| {
        log::error!("resolve config path: {err}");
        std::process::exit(1);
    });
    let cfg = load_or_create_config(&config_path).unwrap_or_else(|err| {
        log::error!("load config {}: {err}", config_path.display());
        std::process::exit(1);
    });

    let media_socket = Arc::new(UdpSocket::bind(cfg.media_bind).await.unwrap_or_else(|err| {
        log::error!("bind media socket {}: {err}", cfg.media_bind);
        std::process::exit(1);
    }));
    let (playback_tx, playback_rx) = mpsc::channel(PLAYBACK_BUFFER_FRAMES);
    let playback_cancel = CancellationToken::new();
    let receiver_cancel = CancellationToken::new();

    let state = AppState {
        config_path: Arc::new(config_path),
        config: Arc::new(RwLock::new(cfg.clone())),
        media_socket: media_socket.clone(),
        tx_session: Arc::new(Mutex::new(None)),
        stats: Arc::new(Stats::default()),
        playback_tx,
    };

    let playback_cfg = cfg.clone();
    let playback_task = tokio::spawn(async move {
        if let Err(err) = audio::playback_loop(playback_cfg, playback_rx, playback_cancel).await {
            log::error!("playback loop: {err}");
        }
    });

    let receiver_state = state.clone();
    let receiver_task = tokio::spawn(async move {
        if let Err(err) = receiver_loop(receiver_state, receiver_cancel).await {
            log::error!("receiver loop: {err}");
        }
    });

    let app = Router::new()
        .route("/", get(get_browser_page))
        .route("/ws/browser-ptt", get(get_browser_ptt_ws))
        .route("/api/status", get(get_status))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/audio/devices", get(get_audio_devices))
        .route("/api/ptt/start", post(post_ptt_start))
        .route("/api/ptt/stop", post(post_ptt_stop))
        .with_state(state.clone());

    log::info!("kaonic-audio-ptt listening on http://{}", cfg.http_bind);
    let listener = tokio::net::TcpListener::bind(cfg.http_bind)
        .await
        .unwrap_or_else(|err| {
            log::error!("bind HTTP listener {}: {err}", cfg.http_bind);
            std::process::exit(1);
        });
    let server = axum::serve(listener, app);

    tokio::select! {
        result = server => {
            if let Err(err) = result {
                log::error!("HTTP server error: {err}");
            }
        }
        _ = shutdown_signal() => {
            log::info!("shutdown signal received");
        }
    }

    shutdown_tx_session(&state).await;
    playback_task.abort();
    receiver_task.abort();
    Ok(())
}

async fn get_browser_page() -> Html<&'static str> {
    Html(BROWSER_PAGE)
}

async fn get_browser_ptt_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> axum::response::Response {
    let cfg = state.config.read().await.clone();
    let Some(remote_peer) = cfg.remote_peer else {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                detail: "remote_peer must be configured before browser transmit".into(),
            }),
        )
            .into_response();
    };

    {
        let mut guard = state.tx_session.lock().await;
        if guard.is_some() {
            return (
                StatusCode::CONFLICT,
                Json(MessageResponse {
                    detail: "another transmit source is already active".into(),
                }),
            )
                .into_response();
        }
        *guard = Some(ActiveTx::Browser);
    }
    state.stats.transmitting.store(true, Ordering::Relaxed);

    ws.on_upgrade(move |socket| async move {
        if let Err(err) = browser_transmit_loop(state.clone(), cfg, remote_peer, socket).await {
            log::warn!("browser transmit session ended with error: {err}");
        }
        finish_browser_tx_session(&state).await;
    })
}

async fn get_status(State(state): State<AppState>) -> impl IntoResponse {
    Json(build_status(&state).await)
}

async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.config.read().await.clone();
    Json(ConfigResponse { config: cfg })
}

async fn put_config(
    State(state): State<AppState>,
    Json(update): Json<ConfigUpdate>,
) -> impl IntoResponse {
    let mut cfg = state.config.read().await.clone();
    cfg.remote_peer = update.remote_peer;
    cfg.capture_device = update.capture_device.trim().to_string();
    cfg.playback_device = update.playback_device.trim().to_string();
    if cfg.capture_device.is_empty() || cfg.playback_device.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                detail: "capture_device and playback_device must not be empty".into(),
            }),
        )
            .into_response();
    }
    if let Err(err) = save_config(state.config_path.as_ref(), &cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(MessageResponse { detail: err }),
        )
            .into_response();
    }
    *state.config.write().await = cfg.clone();
    (
        StatusCode::OK,
        Json(MessageResponse {
            detail: "Config saved. remote_peer and capture_device apply on the next transmit start; playback_device and bind changes apply on restart.".into(),
        }),
    )
        .into_response()
}

async fn get_audio_devices() -> impl IntoResponse {
    Json(AudioDevicesResponse {
        devices: audio::list_devices().await,
    })
}

async fn post_ptt_start(State(state): State<AppState>) -> impl IntoResponse {
    let cfg = state.config.read().await.clone();
    let Some(remote_peer) = cfg.remote_peer else {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                detail: "remote_peer must be configured before transmit".into(),
            }),
        )
            .into_response();
    };

    let mut guard = state.tx_session.lock().await;
    if guard.is_some() {
        return (
            StatusCode::CONFLICT,
            Json(MessageResponse {
                detail: "another transmit source is already active".into(),
            }),
        )
            .into_response();
    }

    let cancel = CancellationToken::new();
    let task_state = state.clone();
    let task_cfg = cfg.clone();
    let task_cancel = cancel.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = transmit_loop(task_state, task_cfg, remote_peer, task_cancel).await {
            log::error!("transmit loop: {err}");
        }
    });
    state.stats.transmitting.store(true, Ordering::Relaxed);
    *guard = Some(ActiveTx::Alsa(TxControl { cancel, handle }));
    (
        StatusCode::OK,
        Json(MessageResponse {
            detail: format!("PTT transmit started to {remote_peer}"),
        }),
    )
        .into_response()
}

async fn post_ptt_stop(State(state): State<AppState>) -> impl IntoResponse {
    match stop_alsa_tx_task(&state).await {
        Ok(detail) => (StatusCode::OK, Json(MessageResponse { detail })).into_response(),
        Err(detail) => (StatusCode::CONFLICT, Json(MessageResponse { detail })).into_response(),
    }
}

async fn stop_alsa_tx_task(state: &AppState) -> Result<String, String> {
    let control = {
        let mut guard = state.tx_session.lock().await;
        match guard.take() {
            Some(ActiveTx::Alsa(control)) => Some(control),
            Some(ActiveTx::Browser) => {
                *guard = Some(ActiveTx::Browser);
                return Err("browser PTT session is active; release the browser microphone button to stop transmit".into());
            }
            None => None,
        }
    };

    if let Some(control) = control {
        control.cancel.cancel();
        let _ = control.handle.await;
        state.stats.transmitting.store(false, Ordering::Relaxed);
        Ok("PTT transmit stopped".into())
    } else {
        Ok("No ALSA transmit session was active".into())
    }
}

async fn shutdown_tx_session(state: &AppState) {
    let active = state.tx_session.lock().await.take();
    if let Some(ActiveTx::Alsa(control)) = active {
        control.cancel.cancel();
        let _ = control.handle.await;
    }
    state.stats.transmitting.store(false, Ordering::Relaxed);
}

async fn finish_browser_tx_session(state: &AppState) {
    let mut guard = state.tx_session.lock().await;
    if matches!(guard.as_ref(), Some(ActiveTx::Browser)) {
        *guard = None;
    }
    state.stats.transmitting.store(false, Ordering::Relaxed);
}

async fn build_status(state: &AppState) -> StatusSnapshot {
    let active_source = {
        let guard = state.tx_session.lock().await;
        match guard.as_ref() {
            Some(ActiveTx::Alsa(_)) => Some("alsa"),
            Some(ActiveTx::Browser) => Some("browser"),
            None => None,
        }
    };

    StatusSnapshot {
        config: state.config.read().await.clone(),
        transmitting: state.stats.transmitting.load(Ordering::Relaxed),
        active_source,
        tx_packets: state.stats.tx_packets.load(Ordering::Relaxed),
        rx_packets: state.stats.rx_packets.load(Ordering::Relaxed),
        tx_bytes: state.stats.tx_bytes.load(Ordering::Relaxed),
        rx_bytes: state.stats.rx_bytes.load(Ordering::Relaxed),
        dropped_rx_while_talking: state.stats.dropped_rx_while_talking.load(Ordering::Relaxed),
        last_remote: *state.stats.last_remote.lock().await,
    }
}

async fn transmit_loop(
    state: AppState,
    cfg: PluginConfig,
    remote_peer: SocketAddr,
    cancel: CancellationToken,
) -> Result<(), String> {
    let mut codec = TxCodec::new(&cfg)?;
    let (frame_tx, mut frame_rx) = mpsc::channel::<Vec<i16>>(PLAYBACK_BUFFER_FRAMES);
    let capture_cancel = cancel.child_token();
    let capture_cfg = cfg.clone();
    let capture_task =
        tokio::spawn(
            async move { audio::capture_loop(capture_cfg, frame_tx, capture_cancel).await },
        );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            next = frame_rx.recv() => {
                let Some(frame) = next else { break; };
                send_pcm_frame(&state, &mut codec, remote_peer, &frame).await?;
            }
        }
    }

    let _ = capture_task.await;
    Ok(())
}

async fn browser_transmit_loop(
    state: AppState,
    cfg: PluginConfig,
    remote_peer: SocketAddr,
    mut socket: WebSocket,
) -> Result<(), String> {
    let mut codec = TxCodec::new(&cfg)?;
    let expected_samples = frame_samples(&cfg);

    while let Some(message) = socket.next().await {
        let message = message.map_err(|err| format!("read browser websocket frame: {err}"))?;
        match message {
            Message::Binary(payload) => {
                let frame = decode_browser_pcm(&payload, expected_samples)?;
                send_pcm_frame(&state, &mut codec, remote_peer, &frame).await?;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) | Message::Text(_) => {}
        }
    }

    Ok(())
}

async fn send_pcm_frame(
    state: &AppState,
    codec: &mut TxCodec,
    remote_peer: SocketAddr,
    frame: &[i16],
) -> Result<(), String> {
    let encoded = codec.encode(frame)?;
    let seq = state.stats.seq.fetch_add(1, Ordering::Relaxed) as u32;
    let packet = encode_packet(seq, &encoded);
    state
        .media_socket
        .send_to(&packet, remote_peer)
        .await
        .map_err(|err| format!("send UDP packet: {err}"))?;
    state.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
    state
        .stats
        .tx_bytes
        .fetch_add(packet.len() as u64, Ordering::Relaxed);
    Ok(())
}

async fn receiver_loop(state: AppState, cancel: CancellationToken) -> Result<(), String> {
    let cfg = state.config.read().await.clone();
    let mut codec = RxCodec::new(&cfg)?;
    let frame_len = frame_samples(&cfg);
    let mut buf = vec![0u8; 2048];

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            received = state.media_socket.recv_from(&mut buf) => {
                let (len, remote) = received.map_err(|err| format!("receive UDP packet: {err}"))?;
                if state.stats.transmitting.load(Ordering::Relaxed) {
                    state.stats.dropped_rx_while_talking.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let payload = decode_packet(&buf[..len]).ok_or_else(|| "invalid packet header".to_string())?;
                let pcm = codec.decode(payload, frame_len)?;
                if state.playback_tx.try_send(pcm).is_err() {
                    log::warn!("playback buffer full, dropping received frame");
                    continue;
                }
                *state.stats.last_remote.lock().await = Some(remote);
                state.stats.rx_packets.fetch_add(1, Ordering::Relaxed);
                state.stats.rx_bytes.fetch_add(len as u64, Ordering::Relaxed);
            }
        }
    }

    Ok(())
}

fn encode_packet(seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(PACKET_HEADER_LEN + payload.len());
    packet.extend_from_slice(&PACKET_MAGIC);
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(payload);
    packet
}

fn decode_packet(packet: &[u8]) -> Option<&[u8]> {
    if packet.len() < PACKET_HEADER_LEN {
        return None;
    }
    if packet[..4] != PACKET_MAGIC {
        return None;
    }
    Some(&packet[PACKET_HEADER_LEN..])
}

fn decode_browser_pcm(payload: &[u8], expected_samples: usize) -> Result<Vec<i16>, String> {
    if payload.len() != expected_samples * 2 {
        return Err(format!(
            "browser audio frame must be exactly {} bytes, got {}",
            expected_samples * 2,
            payload.len()
        ));
    }
    Ok(payload
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip_preserves_payload() {
        let packet = encode_packet(42, b"hello");
        assert_eq!(decode_packet(&packet), Some(b"hello".as_slice()));
    }

    #[test]
    fn packet_decoder_rejects_invalid_magic() {
        assert!(decode_packet(b"nopepayload").is_none());
    }

    #[test]
    fn browser_pcm_decoder_accepts_expected_frame() {
        let frame = vec![0u8; 640];
        let pcm = decode_browser_pcm(&frame, 320).expect("valid browser PCM frame");
        assert_eq!(pcm.len(), 320);
    }

    #[test]
    fn browser_pcm_decoder_rejects_wrong_size() {
        let err = decode_browser_pcm(&[0u8; 12], 320).expect_err("wrong browser PCM frame size");
        assert!(err.contains("exactly 640 bytes"));
    }
}
