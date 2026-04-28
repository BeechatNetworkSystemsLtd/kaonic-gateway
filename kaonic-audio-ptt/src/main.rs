mod audio;
mod codec;
mod config;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use futures_util::StreamExt;
use kaonic_gateway::local_https;
use kaonic_reticulum::KaonicCtrlInterface;
use reticulum::destination::link::{LinkEvent, LinkStatus};
use reticulum::destination::{DestinationDesc, DestinationName, SingleInputDestination};
use reticulum::hash::AddressHash;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::{TimerConfig, Transport, TransportConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::audio::{frame_samples, AudioDevices};
use crate::codec::{RxCodec, TxCodec};
use crate::config::{
    load_or_create_config, normalize_selected_peer, resolve_config_path, save_config, PluginConfig,
};

const PACKET_MAGIC: [u8; 4] = *b"KPT2";
const PACKET_HEADER_LEN: usize = 8;
const PLAYBACK_BUFFER_FRAMES: usize = 64;
const RECEIVE_PLAYBACK_BUFFER_FRAMES: usize = 128;
const AUDIO_PTT_ANNOUNCE_MAGIC: &[u8] = b"KAP1";
const PLUGIN_TLS_CERT_FILE: &str = "plugin-tls.crt";
const PLUGIN_TLS_KEY_FILE: &str = "plugin-tls.key";
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
      background: #0f172a;
      color: #e2e8f0;
      user-select: none;
      -webkit-user-select: none;
      -webkit-touch-callout: none;
      -webkit-tap-highlight-color: transparent;
    }
    .app {
      min-height: 100vh;
      display: flex;
      align-items: stretch;
      padding-bottom: 15rem;
      box-sizing: border-box;
    }
    .sidebar {
      width: min(25rem, 100%);
      padding: 1.4rem 1.15rem;
      background: rgba(2, 6, 23, 0.82);
      border-right: 1px solid #1e293b;
      box-sizing: border-box;
      display: flex;
      flex-direction: column;
      gap: 1rem;
    }
    .content {
      flex: 1;
      min-width: 0;
      padding: 1.5rem;
      display: flex;
      align-items: flex-start;
      justify-content: center;
      box-sizing: border-box;
    }
    .sidebar-header h1 {
      margin: 0 0 0.35rem;
      font-size: 1.6rem;
    }
    .sidebar-header p {
      margin: 0;
      color: #94a3b8;
      line-height: 1.45;
      font-size: 0.96rem;
    }
    .contacts-title {
      display: flex;
      justify-content: space-between;
      align-items: center;
      color: #cbd5e1;
      font-weight: 700;
      letter-spacing: 0.01em;
    }
    .contacts-count {
      font-size: 0.85rem;
      color: #94a3b8;
      font-weight: 600;
    }
    .peer-list {
      display: grid;
      gap: 0.7rem;
      overflow-y: auto;
      padding-right: 0.2rem;
    }
    .peer-card {
      width: 100%;
      text-align: left;
      border: 1px solid #1f2937;
      background: #111827;
      color: inherit;
      border-radius: 1rem;
      padding: 0.95rem 1rem;
      cursor: pointer;
      transition: border-color 0.18s ease, transform 0.18s ease, background 0.18s ease;
      user-select: none;
      -webkit-user-select: none;
    }
    .peer-card:hover {
      border-color: #334155;
      transform: translateY(-1px);
    }
    .peer-card.is-selected {
      border-color: #2563eb;
      background: #0f1f47;
      box-shadow: inset 0 0 0 1px rgba(37, 99, 235, 0.35);
    }
    .peer-card-header {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 0.75rem;
      margin-bottom: 0.45rem;
    }
    .peer-name {
      font-size: 0.98rem;
      font-weight: 700;
      color: #f8fafc;
      word-break: break-all;
    }
    .peer-status {
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
      white-space: nowrap;
      font-size: 0.82rem;
      color: #cbd5e1;
    }
    .status-dot {
      width: 0.65rem;
      height: 0.65rem;
      border-radius: 999px;
      background: #64748b;
      box-shadow: 0 0 0 0.2rem rgba(100, 116, 139, 0.12);
    }
    .status-dot.is-online {
      background: #22c55e;
      box-shadow: 0 0 0 0.2rem rgba(34, 197, 94, 0.14);
    }
    .status-dot.is-waiting {
      background: #f59e0b;
      box-shadow: 0 0 0 0.2rem rgba(245, 158, 11, 0.14);
    }
    .peer-subtitle {
      color: #94a3b8;
      font-size: 0.84rem;
      line-height: 1.35;
      word-break: break-word;
    }
    .empty-peers {
      padding: 1rem;
      border: 1px dashed #334155;
      border-radius: 1rem;
      color: #94a3b8;
      text-align: center;
      line-height: 1.5;
    }
    .ptt-shell {
      display: none;
    }
    .ptt-overlay {
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      display: flex;
      justify-content: center;
      padding: 0 1rem calc(env(safe-area-inset-bottom, 0px) + 1rem);
      pointer-events: none;
      z-index: 20;
    }
    .mic-wrap {
      display: flex;
      justify-content: center;
      align-items: center;
      width: min(100%, 22rem);
      padding: 1.25rem;
      border-radius: 1.4rem;
      border: 1px solid #1f2937;
      background: #111827;
      box-shadow: 0 24px 48px rgba(0,0,0,0.35);
      box-sizing: border-box;
      pointer-events: auto;
    }
    .mic-btn {
      width: min(20rem, 62vw);
      height: min(20rem, 62vw);
      border: 0;
      border-radius: 999px;
      background: linear-gradient(180deg, #fcd34d, #f59e0b);
      color: #451a03;
      font-size: 1.35rem;
      font-weight: 800;
      box-shadow: 0 20px 40px rgba(245, 158, 11, 0.35);
      touch-action: none;
      user-select: none;
      -webkit-user-select: none;
      -webkit-touch-callout: none;
      transition: transform 0.16s ease, box-shadow 0.16s ease, opacity 0.16s ease;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
      gap: 0.8rem;
      text-align: center;
    }
    .mic-btn:focus {
      outline: none;
    }
    .mic-btn:disabled {
      opacity: 0.6;
    }
    .mic-btn.is-active {
      transform: scale(0.97);
      background: linear-gradient(180deg, #fde68a, #fbbf24);
      box-shadow: 0 0 0 0.85rem rgba(251, 191, 36, 0.2);
    }
    .mic-icon {
      width: 3.9rem;
      height: 3.9rem;
      display: block;
      color: currentColor;
    }
    .mic-label {
      max-width: 10rem;
      line-height: 1.15;
    }
    .status {
      height: 7.4rem;
      padding: 0.95rem 1rem;
      border-radius: 1rem;
      border: 1px solid #1f2937;
      background: #0b1220;
      color: #cbd5e1;
      font-size: 0.98rem;
      white-space: pre-line;
      line-height: 1.45;
      text-align: left;
      box-sizing: border-box;
      overflow-y: auto;
    }
    .playback-status {
      height: 9rem;
      padding: 0.95rem 1rem;
      border-radius: 1rem;
      border: 1px solid #1f2937;
      background: #0b1220;
      text-align: left;
      box-sizing: border-box;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr);
    }
    .playback-status-title {
      color: #cbd5e1;
      font-size: 0.82rem;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      margin-bottom: 0.55rem;
    }
    .playback-status-body {
      color: #cbd5e1;
      font-size: 0.92rem;
      white-space: pre-line;
      line-height: 1.45;
      overflow-y: auto;
      min-height: 0;
    }
    .hint {
      margin-top: 0.75rem;
      color: #94a3b8;
      font-size: 0.9rem;
      line-height: 1.5;
    }
    @media (max-width: 880px) {
      .app {
        flex-direction: column;
      }
      .sidebar {
        width: 100%;
        border-right: 0;
        border-bottom: 1px solid #1e293b;
      }
      .content {
        padding-top: 1rem;
      }
      .mic-btn {
        width: min(18rem, 72vw);
        height: min(18rem, 72vw);
      }
    }
  </style>
</head>
<body>
  <main class="app">
    <aside class="sidebar">
      <div class="sidebar-header">
        <h1>Kaonic Audio PTT</h1>
        <p>Select a contact on the left. Online contacts can be called immediately; offline ones stay saved until their Reticulum link appears.</p>
      </div>
      <div class="contacts-title">
        <span>Contacts</span>
        <span id="contacts-count" class="contacts-count">0 peers</span>
      </div>
      <div id="peer-list" class="peer-list">
        <div class="empty-peers">Loading contacts…</div>
      </div>
    </aside>
    <section class="content">
      <div class="ptt-shell">
        <div class="playback-status">
          <div class="playback-status-title">Last received audio</div>
          <div id="playback-status" class="playback-status-body">Waiting for audio…</div>
        </div>
        <div id="status" class="status">Loading…</div>
        <div class="hint">Audio is sent over Reticulum links and played by the remote Kaonic audio-ptt plugin on ALSA output.</div>
      </div>
    </section>
  </main>
  <div class="ptt-overlay">
    <div class="mic-wrap">
      <button id="mic-btn" class="mic-btn" type="button" aria-label="Push to Talk">
        <svg class="mic-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
          <path fill="currentColor" d="M12 15.5a4 4 0 0 0 4-4v-5a4 4 0 1 0-8 0v5a4 4 0 0 0 4 4Zm-6-4a1 1 0 1 1 2 0a4 4 0 1 0 8 0a1 1 0 1 1 2 0a6 6 0 0 1-5 5.91V20h2a1 1 0 1 1 0 2H9a1 1 0 1 1 0-2h2v-2.59A6 6 0 0 1 6 11.5Z"/>
        </svg>
        <span class="mic-label">Push to Talk</span>
      </button>
    </div>
  </div>
  <script>
    (function () {
      const SAMPLE_RATE = 16000;
      const FRAME_SAMPLES = 320;
      const STATUS_POLL_MS = 3000;
      const micBtn = document.getElementById('mic-btn');
      const statusEl = document.getElementById('status');
      const playbackStatusEl = document.getElementById('playback-status');
      const peerListEl = document.getElementById('peer-list');
      const contactsCountEl = document.getElementById('contacts-count');

      let stream = null;
      let audioContext = null;
      let sourceNode = null;
      let processor = null;
      let muteNode = null;
      let ws = null;
      let queue = [];
      let starting = false;
      let active = false;
      let currentStatus = null;
      let pollHandle = null;
      let streamPromise = null;

      function setStatus(text) {
        statusEl.textContent = text;
      }

      function setPlaybackStatus(text) {
        playbackStatusEl.textContent = text;
      }

      async function primeMicrophone() {
        if (stream && stream.active && stream.getTracks().some(function (track) { return track.readyState === 'live'; })) {
          return stream;
        }
        if (!streamPromise) {
          streamPromise = navigator.mediaDevices.getUserMedia({
            audio: {
              channelCount: 1,
              echoCancellation: true,
              noiseSuppression: true,
              autoGainControl: true
            }
          }).then(function (mediaStream) {
            stream = mediaStream;
            return mediaStream;
          }).finally(function () {
            streamPromise = null;
          });
        }
        return streamPromise;
      }

      function selectedPeer() {
        return currentStatus && currentStatus.config ? currentStatus.config.selected_peer : null;
      }

      function selectedPeerSnapshot() {
        const selected = selectedPeer();
        const peers = (currentStatus && currentStatus.peers) || [];
        return peers.find(function (peer) { return peer.hash === selected; }) || null;
      }

      function peersWithSavedSelection() {
        const peers = ((currentStatus && currentStatus.peers) || []).slice();
        const selected = selectedPeer();
        if (selected && !peers.some(function (peer) { return peer.hash === selected; })) {
          peers.unshift({
            hash: selected,
            status: 'waiting',
            last_seen_ts: 0
          });
        }
        return peers;
      }

      function statusClass(status) {
        if (status === 'active') { return 'is-online'; }
        if (status === 'connecting' || status === 'pending' || status === 'waiting') { return 'is-waiting'; }
        return '';
      }

      function statusLabel(status) {
        if (status === 'active') { return 'Online'; }
        if (status === 'connecting') { return 'Connecting'; }
        if (status === 'pending') { return 'Pending'; }
        if (status === 'waiting') { return 'Waiting for announce'; }
        return 'Offline';
      }

      function peerSubtitle(peer) {
        if (peer.last_seen_ts) {
          return 'Last seen ' + new Date(peer.last_seen_ts * 1000).toLocaleTimeString();
        }
        if (peer.status === 'active') {
          return 'Ready to talk';
        }
        return 'Saved contact';
      }

      function renderPeerList() {
        const peers = peersWithSavedSelection();
        contactsCountEl.textContent = peers.length === 1 ? '1 peer' : (peers.length + ' peers');
        if (!peers.length) {
          peerListEl.innerHTML = '<div class="empty-peers">No discovered contacts yet.\nWait for a Reticulum announce from another Kaonic audio-ptt node.</div>';
          return;
        }
        const selected = selectedPeer();
        peerListEl.innerHTML = peers.map(function (peer) {
          const chosen = peer.hash === selected ? ' is-selected' : '';
          const label = statusLabel(peer.status);
          return ''
            + '<button type="button" class="peer-card' + chosen + '" data-peer-hash="' + peer.hash + '">'
            +   '<div class="peer-card-header">'
            +     '<div class="peer-name">' + peer.hash + '</div>'
            +     '<div class="peer-status"><span class="status-dot ' + statusClass(peer.status) + '"></span><span>' + label + '</span></div>'
            +   '</div>'
            +   '<div class="peer-subtitle">' + peerSubtitle(peer) + '</div>'
            + '</button>';
        }).join('');
        peerListEl.querySelectorAll('[data-peer-hash]').forEach(function (button) {
          button.addEventListener('click', function () {
            choosePeer(button.getAttribute('data-peer-hash')).catch(function (err) {
              setStatus('Error: ' + (err && err.message ? err.message : err));
            });
          });
        });
      }

      async function loadStatus() {
        const resp = await fetch('/api/status');
        const data = await resp.json();
        currentStatus = data;
        renderPeerList();
        const peer = selectedPeerSnapshot();
        const lines = [
          'My destination: ' + data.local_destination,
          'Selected contact: ' + (selectedPeer() || 'not configured'),
          'Link state: ' + (peer ? statusLabel(peer.status) : 'Waiting for announce'),
          'Playback device: ' + data.config.playback_device,
          'Capture device: ' + data.config.capture_device
        ];
        if (data.transmitting) {
          lines.push('Transmit source: ' + (data.active_source || 'active'));
        }
        setStatus(lines.join('\n'));

        const playbackLines = [];
        playbackLines.push('Last source: ' + (data.last_remote || 'No audio received yet'));
        playbackLines.push('Played frames: ' + data.played_frames);
        playbackLines.push('Dropped frames: ' + data.playback_drops);
        if (data.last_played_ts) {
          playbackLines.push('Last played: ' + new Date(data.last_played_ts * 1000).toLocaleTimeString());
        } else {
          playbackLines.push('Last played: Waiting for audio');
        }
        if (data.last_played_samples) {
          playbackLines.push('Last frame: ' + data.last_played_samples + ' samples');
        }
        if (data.last_playback_error) {
          const when = data.last_playback_error_ts
            ? ' at ' + new Date(data.last_playback_error_ts * 1000).toLocaleTimeString()
            : '';
          playbackLines.push('Last error: ' + data.last_playback_error + when);
        } else {
          playbackLines.push('Last error: None');
        }
        setPlaybackStatus(playbackLines.join('\n'));
      }

      async function saveSelectedPeer(peerHash) {
        if (!currentStatus) {
          await loadStatus();
        }
        const payload = {
          selected_peer: peerHash || null,
          capture_device: currentStatus.config.capture_device,
          playback_device: currentStatus.config.playback_device
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
        setStatus((data.detail || 'Contact selected') + '\nSelected contact: ' + (payload.selected_peer || 'not configured'));
      }

      async function choosePeer(peerHash) {
        if (!peerHash) {
          return;
        }
        await saveSelectedPeer(peerHash);
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
        const chosenPeer = selectedPeer();
        if (!chosenPeer) {
          setStatus('Select a contact first.');
          return;
        }
        starting = true;
        try {
          await saveSelectedPeer(chosenPeer);
          await primeMicrophone();
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
          setStatus('Talking to ' + chosenPeer + '… release to stop');
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
        if (ws) {
          try { ws.close(); } catch (_) {}
          ws = null;
        }
        await loadStatus().catch(function () {});
      }

      micBtn.addEventListener('pointerdown', function (event) {
        event.preventDefault();
        if (document.activeElement instanceof HTMLElement) {
          document.activeElement.blur();
        }
        micBtn.setPointerCapture(event.pointerId);
        startTalk();
      });
      micBtn.addEventListener('pointerup', function () { stopTalk(); });
      micBtn.addEventListener('pointercancel', function () { stopTalk(); });
      micBtn.addEventListener('lostpointercapture', function () { stopTalk(); });
      micBtn.addEventListener('contextmenu', function (event) { event.preventDefault(); });

      window.addEventListener('beforeunload', function () {
        stopTalk();
        if (stream) {
          stream.getTracks().forEach(function (track) { track.stop(); });
          stream = null;
        }
        if (pollHandle) { clearInterval(pollHandle); }
      });

      loadStatus().catch(function (err) {
        setStatus('Error: ' + (err && err.message ? err.message : err));
      });
      primeMicrophone().then(function () {
        loadStatus().catch(function () {});
      }).catch(function (err) {
        setStatus('Microphone permission required: ' + (err && err.message ? err.message : err));
      });
      pollHandle = window.setInterval(function () {
        loadStatus().catch(function (err) {
          setStatus('Error: ' + (err && err.message ? err.message : err));
        });
      }, STATUS_POLL_MS);
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
    transport: Arc<Mutex<Transport>>,
    destination: Arc<Mutex<SingleInputDestination>>,
    local_destination: String,
    peers: Arc<RwLock<HashMap<String, PeerState>>>,
    tx_session: Arc<Mutex<Option<ActiveTx>>>,
    stats: Arc<Stats>,
    playback_tx: mpsc::Sender<Vec<i16>>,
}

enum ActiveTx {
    Alsa(TxControl),
    Browser(u64),
}

struct TxControl {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

#[derive(Clone)]
struct PeerState {
    desc: DestinationDesc,
    status: String,
    last_seen_ts: u64,
}

#[derive(Default)]
struct Stats {
    transmitting: AtomicBool,
    tx_packets: AtomicU64,
    rx_packets: AtomicU64,
    tx_bytes: AtomicU64,
    rx_bytes: AtomicU64,
    dropped_rx_while_talking: AtomicU64,
    playback_drops: AtomicU64,
    played_frames: AtomicU64,
    last_played_ts: AtomicU64,
    last_played_samples: AtomicU64,
    last_playback_error_ts: AtomicU64,
    browser_session_seq: AtomicU64,
    seq: AtomicU64,
    last_remote: Mutex<Option<String>>,
    last_playback_error: StdMutex<Option<String>>,
}

impl Stats {
    fn record_played_frame(&self, sample_count: usize) {
        self.played_frames.fetch_add(1, Ordering::Relaxed);
        self.last_played_samples
            .store(sample_count as u64, Ordering::Relaxed);
        self.last_played_ts.store(now_secs(), Ordering::Relaxed);
    }

    async fn record_playback_error(&self, detail: impl Into<String>) {
        if let Ok(mut guard) = self.last_playback_error.lock() {
            *guard = Some(detail.into());
        }
        self.last_playback_error_ts
            .store(now_secs(), Ordering::Relaxed);
    }

    fn last_playback_error(&self) -> Option<String> {
        self.last_playback_error
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn next_browser_session_id(&self) -> u64 {
        self.browser_session_seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}

#[derive(Debug, Serialize)]
struct StatusSnapshot {
    config: PluginConfig,
    local_destination: String,
    peers: Vec<PeerSnapshot>,
    transmitting: bool,
    active_source: Option<&'static str>,
    tx_packets: u64,
    rx_packets: u64,
    tx_bytes: u64,
    rx_bytes: u64,
    dropped_rx_while_talking: u64,
    playback_drops: u64,
    played_frames: u64,
    last_played_ts: u64,
    last_played_samples: u64,
    last_playback_error_ts: u64,
    last_remote: Option<String>,
    last_playback_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PeerSnapshot {
    hash: String,
    status: String,
    last_seen_ts: u64,
}

#[derive(Debug, Deserialize)]
struct ConfigUpdate {
    selected_peer: Option<String>,
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
        .parse_filters("info,kaonic_audio_ptt=debug,reticulum=warn")
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
    local_https::install_rustls_crypto_provider();

    let id = PrivateIdentity::new_from_name(&cfg.identity_seed);
    let cancel = CancellationToken::new();
    let radio_client = KaonicCtrlInterface::connect_client::<1400, 5>(
        "0.0.0.0:0".parse().expect("listen addr"),
        cfg.kaonic_ctrl_server,
        cancel.clone(),
    )
    .await
    .unwrap_or_else(|err| {
        log::error!("connect kaonic-ctrl {}: {err:?}", cfg.kaonic_ctrl_server);
        std::process::exit(1);
    });
    spawn_keepalive(radio_client.clone(), cancel.clone());

    let mut transport_cfg = TransportConfig::new("kaonic-audio-ptt", &id, true);
    transport_cfg.set_retransmit(true);
    transport_cfg.set_timer_config(TimerConfig {
        in_link_stale: Duration::from_secs(30),
        in_link_close: Duration::from_secs(15),
        out_link_restart: Duration::from_secs(45),
        out_link_stale: Duration::from_secs(30),
        out_link_close: Duration::from_secs(15),
        out_link_repeat: Duration::from_secs(10),
        out_link_keep: Duration::from_secs(5),
        ..TimerConfig::default()
    });
    transport_cfg.set_restart_outlinks(true);
    let transport = Arc::new(Mutex::new(Transport::new(transport_cfg)));

    let iface = KaonicCtrlInterface::new(radio_client, cfg.rns_module.min(1), None, None);
    let iface_mgr = transport.lock().await.iface_manager();
    iface_mgr
        .lock()
        .await
        .spawn(iface, KaonicCtrlInterface::spawn);

    let destination = transport
        .lock()
        .await
        .add_destination(id, DestinationName::new("kaonic", "audio-ptt"))
        .await;
    let local_destination = destination.lock().await.desc.address_hash.to_hex_string();
    log::info!(
        "kaonic-audio-ptt Reticulum destination {} via module {}",
        local_destination,
        cfg.rns_module.min(1)
    );
    log::info!(
        "kaonic-audio-ptt local audio path capture_device={} playback_device={} (browser UI can provide transmit microphone audio, but received audio is rendered on local ALSA playback hardware)",
        cfg.capture_device,
        cfg.playback_device
    );

    let (playback_tx, playback_rx) = mpsc::channel(RECEIVE_PLAYBACK_BUFFER_FRAMES);
    let playback_cancel = CancellationToken::new();

    let state = AppState {
        config_path: Arc::new(config_path),
        config: Arc::new(RwLock::new(cfg.clone())),
        transport: transport.clone(),
        destination: destination.clone(),
        local_destination,
        peers: Arc::new(RwLock::new(HashMap::new())),
        tx_session: Arc::new(Mutex::new(None)),
        stats: Arc::new(Stats::default()),
        playback_tx,
    };

    let playback_cfg = cfg.clone();
    let playback_stats = state.stats.clone();
    let playback_task = tokio::spawn(async move {
        if let Err(err) = audio::playback_loop(
            playback_cfg,
            playback_rx,
            playback_cancel,
            playback_stats.clone(),
        )
        .await
        {
            playback_stats
                .record_playback_error(format!("playback loop: {err}"))
                .await;
            log::error!("playback loop: {err}");
        }
    });

    spawn_announce_tx(state.clone(), cancel.clone());
    spawn_announce_rx(state.clone(), cancel.clone());
    spawn_out_link_events(state.clone(), cancel.clone());
    spawn_in_link_events(state.clone(), cancel.clone());

    let app = Router::new()
        .route("/", get(get_browser_page))
        .route("/ws/browser-ptt", get(get_browser_ptt_ws))
        .route("/api/status", get(get_status))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/audio/devices", get(get_audio_devices))
        .route("/api/ptt/start", post(post_ptt_start))
        .route("/api/ptt/stop", post(post_ptt_stop))
        .with_state(state.clone());

    let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        PLUGIN_TLS_CERT_FILE,
        PLUGIN_TLS_KEY_FILE,
    )
    .await
    .unwrap_or_else(|err| {
        log::error!(
            "load plugin HTTPS certificate files {} and {}: {err}",
            PLUGIN_TLS_CERT_FILE,
            PLUGIN_TLS_KEY_FILE
        );
        std::process::exit(1);
    });
    log::info!("kaonic-audio-ptt listening on https://{}", cfg.http_bind);
    let server = axum_server::bind_rustls(cfg.http_bind, tls_config).serve(app.into_make_service());

    tokio::select! {
        result = server => {
            if let Err(err) = result {
                log::error!("HTTPS server error: {err}");
            }
        }
        _ = shutdown_signal() => {
            log::info!("shutdown signal received");
        }
    }

    cancel.cancel();
    shutdown_tx_session(&state).await;
    playback_task.abort();
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
    let Some(remote_peer) = parse_selected_peer(&cfg) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                detail: "selected_peer must be configured before browser transmit".into(),
            }),
        )
            .into_response();
    };
    if let Err(err) = ensure_out_link(&state, remote_peer).await {
        return (StatusCode::CONFLICT, Json(MessageResponse { detail: err })).into_response();
    }

    let browser_session_id = state.stats.next_browser_session_id();
    {
        let mut guard = state.tx_session.lock().await;
        match guard.as_ref() {
            Some(ActiveTx::Alsa(_)) => {
                return (
                    StatusCode::CONFLICT,
                    Json(MessageResponse {
                        detail: "another transmit source is already active".into(),
                    }),
                )
                    .into_response();
            }
            Some(ActiveTx::Browser(_)) => {
                log::debug!("replacing previous browser PTT session");
            }
            None => {}
        }
        *guard = Some(ActiveTx::Browser(browser_session_id));
    }
    state.stats.transmitting.store(true, Ordering::Relaxed);

    ws.on_upgrade(move |socket| async move {
        if let Err(err) = browser_transmit_loop(state.clone(), cfg, remote_peer, socket).await {
            log::warn!("browser transmit session ended with error: {err}");
        }
        finish_browser_tx_session(&state, browser_session_id).await;
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
    let selected_peer = match normalize_selected_peer(update.selected_peer) {
        Ok(value) => value,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(MessageResponse { detail: err }),
            )
                .into_response();
        }
    };
    cfg.selected_peer = selected_peer.clone();
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
    if let Some(hash) = parse_selected_peer(&cfg) {
        if let Err(err) = ensure_out_link(&state, hash).await {
            log::debug!("selected peer link not ready yet: {err}");
        }
    }
    (
        StatusCode::OK,
        Json(MessageResponse {
            detail: if let Some(peer) = selected_peer {
                format!("Config saved. selected_peer={peer}; capture_device applies on the next transmit start and playback_device applies on restart.")
            } else {
                "Config saved. No peer selected; capture_device applies on the next transmit start and playback_device applies on restart.".into()
            },
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
    let Some(remote_peer) = parse_selected_peer(&cfg) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(MessageResponse {
                detail: "selected_peer must be configured before transmit".into(),
            }),
        )
            .into_response();
    };
    if let Err(err) = ensure_out_link(&state, remote_peer).await {
        return (StatusCode::CONFLICT, Json(MessageResponse { detail: err })).into_response();
    }

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
            detail: format!("PTT transmit started to {}", remote_peer.to_hex_string()),
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
            Some(ActiveTx::Browser(session_id)) => {
                *guard = Some(ActiveTx::Browser(session_id));
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

async fn finish_browser_tx_session(state: &AppState, browser_session_id: u64) {
    let mut guard = state.tx_session.lock().await;
    if matches!(guard.as_ref(), Some(ActiveTx::Browser(id)) if *id == browser_session_id) {
        *guard = None;
        state.stats.transmitting.store(false, Ordering::Relaxed);
    }
}

async fn build_status(state: &AppState) -> StatusSnapshot {
    let active_source = {
        let guard = state.tx_session.lock().await;
        match guard.as_ref() {
            Some(ActiveTx::Alsa(_)) => Some("alsa"),
            Some(ActiveTx::Browser(_)) => Some("browser"),
            None => None,
        }
    };
    let cfg = state.config.read().await.clone();
    let selected = cfg.selected_peer.clone();
    let mut peers = state
        .peers
        .read()
        .await
        .iter()
        .map(|(hash, peer)| PeerSnapshot {
            hash: hash.clone(),
            status: peer.status.clone(),
            last_seen_ts: peer.last_seen_ts,
        })
        .collect::<Vec<_>>();
    peers.sort_by(|a, b| {
        let a_selected = selected.as_deref() == Some(a.hash.as_str());
        let b_selected = selected.as_deref() == Some(b.hash.as_str());
        b_selected
            .cmp(&a_selected)
            .then_with(|| a.hash.cmp(&b.hash))
    });

    StatusSnapshot {
        config: cfg,
        local_destination: state.local_destination.clone(),
        peers,
        transmitting: state.stats.transmitting.load(Ordering::Relaxed),
        active_source,
        tx_packets: state.stats.tx_packets.load(Ordering::Relaxed),
        rx_packets: state.stats.rx_packets.load(Ordering::Relaxed),
        tx_bytes: state.stats.tx_bytes.load(Ordering::Relaxed),
        rx_bytes: state.stats.rx_bytes.load(Ordering::Relaxed),
        dropped_rx_while_talking: state.stats.dropped_rx_while_talking.load(Ordering::Relaxed),
        playback_drops: state.stats.playback_drops.load(Ordering::Relaxed),
        played_frames: state.stats.played_frames.load(Ordering::Relaxed),
        last_played_ts: state.stats.last_played_ts.load(Ordering::Relaxed),
        last_played_samples: state.stats.last_played_samples.load(Ordering::Relaxed),
        last_playback_error_ts: state.stats.last_playback_error_ts.load(Ordering::Relaxed),
        last_remote: state.stats.last_remote.lock().await.clone(),
        last_playback_error: state.stats.last_playback_error(),
    }
}

async fn transmit_loop(
    state: AppState,
    cfg: PluginConfig,
    remote_peer: AddressHash,
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
    remote_peer: AddressHash,
    mut socket: WebSocket,
) -> Result<(), String> {
    let expected_samples = frame_samples(&cfg);
    let mut codec = TxCodec::new(&cfg)?;

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
    remote_peer: AddressHash,
    frame: &[i16],
) -> Result<(), String> {
    let encoded = codec.encode(frame)?;
    let seq = state.stats.seq.fetch_add(1, Ordering::Relaxed) as u32;
    let packet = encode_packet(seq, &encoded);
    let sent = state
        .transport
        .lock()
        .await
        .send_to_out_links(&remote_peer, &packet)
        .await;
    if sent.is_empty() {
        return Err(format!(
            "selected peer {} does not have an active Reticulum link",
            remote_peer.to_hex_string()
        ));
    }
    state.stats.tx_packets.fetch_add(1, Ordering::Relaxed);
    state
        .stats
        .tx_bytes
        .fetch_add(packet.len() as u64, Ordering::Relaxed);
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

fn parse_selected_peer(cfg: &PluginConfig) -> Option<AddressHash> {
    cfg.selected_peer
        .as_deref()
        .and_then(|value| AddressHash::new_from_hex_string(value).ok())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn should_reuse_existing_out_link(existing_status: Option<LinkStatus>) -> bool {
    matches!(
        existing_status,
        Some(LinkStatus::Pending | LinkStatus::Handshake | LinkStatus::Active | LinkStatus::Stale)
    )
}

async fn ensure_out_link(state: &AppState, hash: AddressHash) -> Result<(), String> {
    let desc = {
        let peers = state.peers.read().await;
        peers
            .get(&hash.to_hex_string())
            .map(|peer| peer.desc.clone())
            .ok_or_else(|| format!("selected peer {} has not announced audio-ptt yet", hash))?
    };
    let existing = state.transport.lock().await.find_out_link(&hash).await;
    let existing_status = if let Some(link) = existing.as_ref() {
        Some(link.lock().await.status())
    } else {
        None
    };
    if should_reuse_existing_out_link(existing_status) {
        return Ok(());
    }
    {
        let mut peers = state.peers.write().await;
        if let Some(peer) = peers.get_mut(&hash.to_hex_string()) {
            peer.status = "pending".into();
        }
    }
    state.transport.lock().await.link(desc).await;
    Ok(())
}

async fn register_announced_peer(state: &AppState, desc: DestinationDesc) -> Result<(), String> {
    if desc.address_hash.to_hex_string() == state.local_destination {
        return Ok(());
    }
    let hash = desc.address_hash.to_hex_string();
    {
        let mut peers = state.peers.write().await;
        let entry = peers.entry(hash.clone()).or_insert(PeerState {
            desc: desc.clone(),
            status: "discovered".into(),
            last_seen_ts: now_secs(),
        });
        entry.desc = desc.clone();
        entry.last_seen_ts = now_secs();
        if entry.status != "active" {
            entry.status = "discovered".into();
        }
    }
    ensure_out_link(state, desc.address_hash).await
}

async fn update_peer_status(state: &AppState, hash: AddressHash, status: &str) {
    let key = hash.to_hex_string();
    let mut peers = state.peers.write().await;
    if let Some(peer) = peers.get_mut(&key) {
        peer.status = status.into();
        peer.last_seen_ts = now_secs();
    }
}

async fn handle_received_audio(
    state: &AppState,
    cfg: &PluginConfig,
    payload: &[u8],
    remote: String,
) -> Result<(), String> {
    if state.stats.transmitting.load(Ordering::Relaxed) {
        state
            .stats
            .dropped_rx_while_talking
            .fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let body = decode_packet(payload).ok_or_else(|| "invalid audio packet header".to_string())?;
    let frame_len = frame_samples(cfg);
    let mut codec = RxCodec::new(cfg)?;
    let pcm = codec.decode(body, frame_len)?;
    let playback_wait = Duration::from_millis(std::cmp::max((cfg.frame_ms as u64) * 6, 120));
    match tokio::time::timeout(playback_wait, state.playback_tx.send(pcm)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            return Err("playback loop is unavailable".into());
        }
        Err(_) => {
            log::warn!(
                "playback buffer stayed full for {} ms, dropping received frame",
                playback_wait.as_millis()
            );
            state.stats.playback_drops.fetch_add(1, Ordering::Relaxed);
            state
                .stats
                .record_playback_error(format!(
                    "playback buffer stayed full for {} ms, dropping received frame",
                    playback_wait.as_millis()
                ))
                .await;
            return Ok(());
        }
    }
    *state.stats.last_remote.lock().await = Some(remote);
    state.stats.rx_packets.fetch_add(1, Ordering::Relaxed);
    state
        .stats
        .rx_bytes
        .fetch_add(payload.len() as u64, Ordering::Relaxed);
    Ok(())
}

fn spawn_announce_tx(state: AppState, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    state.transport.lock().await.send_announce(&state.destination, Some(AUDIO_PTT_ANNOUNCE_MAGIC)).await;
                }
            }
        }
    });
}

fn spawn_announce_rx(state: AppState, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut announces = state.transport.lock().await.recv_announces().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = announces.recv() => match recv {
                    Ok(ev) => {
                        if ev.app_data.as_slice() != AUDIO_PTT_ANNOUNCE_MAGIC {
                            continue;
                        }
                        let desc = ev.destination.lock().await.desc.clone();
                        if let Err(err) = register_announced_peer(&state, desc).await {
                            log::debug!("audio-ptt announce ignored: {err}");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    });
}

fn spawn_out_link_events(state: AppState, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut events = state.transport.lock().await.out_link_events();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = events.recv() => match recv {
                    Ok(ev) => match ev.event {
                        LinkEvent::Activated => {
                            update_peer_status(&state, ev.address_hash, "active").await;
                        }
                        LinkEvent::Closed => {
                            update_peer_status(&state, ev.address_hash, "closed").await;
                        }
                        LinkEvent::Data(payload) => {
                            let cfg = state.config.read().await.clone();
                            if let Err(err) = handle_received_audio(&state, &cfg, payload.as_slice(), ev.address_hash.to_hex_string()).await {
                                state.stats
                                    .record_playback_error(format!("receive audio over out-link: {err}"))
                                    .await;
                                log::warn!("receive audio over out-link: {err}");
                            }
                        }
                        LinkEvent::Proof(_) => {
                            update_peer_status(&state, ev.address_hash, "handshake").await;
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    });
}

fn spawn_in_link_events(state: AppState, cancel: CancellationToken) {
    let local = state.local_destination.clone();
    tokio::spawn(async move {
        let mut events = state.transport.lock().await.in_link_events();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = events.recv() => match recv {
                    Ok(ev) => {
                        if ev.address_hash.to_hex_string() != local {
                            continue;
                        }
                        if let LinkEvent::Data(payload) = ev.event {
                            let cfg = state.config.read().await.clone();
                            if let Err(err) = handle_received_audio(&state, &cfg, payload.as_slice(), "incoming-link".into()).await {
                                state.stats
                                    .record_playback_error(format!("receive audio over in-link: {err}"))
                                    .await;
                                log::warn!("receive audio over in-link: {err}");
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    });
}

fn spawn_keepalive(
    radio_client: Arc<Mutex<kaonic_reticulum::RadioClient>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(err) = radio_client.lock().await.ping().await {
                        log::warn!("keepalive ping failed: {err:?}");
                    }
                }
                _ = cancel.cancelled() => break,
            }
        }
    });
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
