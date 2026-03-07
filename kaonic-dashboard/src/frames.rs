use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use maud::{html, Markup, PreEscaped, DOCTYPE};

use kaonic_settings::{FrameRecord, settings::Settings};

pub type SharedSettings = Arc<Mutex<Settings>>;

// ── CSS shared with pages.rs (duplicated intentionally to keep modules standalone) ──

const CSS: &str = r#"
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:system-ui,sans-serif;background:#0f1117;color:#e2e8f0;min-height:100vh}
a{color:#7c85f5;text-decoration:none}a:hover{text-decoration:underline}
header{background:#1a1d27;border-bottom:1px solid #2d3147;padding:.85rem 2rem;display:flex;align-items:center;gap:2rem}
header h1{font-size:1.1rem;font-weight:700;color:#7c85f5;letter-spacing:.02em}
header nav a{color:#94a3b8;font-size:.88rem;margin-right:1.2rem}
header nav a.active,header nav a:hover{color:#e2e8f0}
main{max-width:960px;margin:2rem auto;padding:0 1.25rem}
h2{font-size:1.25rem;font-weight:700;margin-bottom:1.5rem}
.card{background:#1a1d27;border:1px solid #2d3147;border-radius:8px;padding:1.5rem;margin-bottom:1.5rem}
.card-title{font-size:.78rem;font-weight:700;color:#7c85f5;text-transform:uppercase;letter-spacing:.08em;margin-bottom:1.1rem}
.toolbar{display:flex;align-items:center;justify-content:space-between;margin-bottom:1rem}
.toolbar-right{display:flex;align-items:center;gap:.75rem}
.badge{display:inline-block;background:#7c85f520;color:#7c85f5;border:1px solid #7c85f540;border-radius:4px;padding:.15rem .55rem;font-size:.75rem;font-weight:600}
.badge-live{background:#14532d40;color:#86efac;border-color:#16a34a50}
table{width:100%;border-collapse:collapse;font-size:.85rem}
thead th{color:#64748b;font-weight:600;font-size:.75rem;text-transform:uppercase;letter-spacing:.05em;padding:.5rem .75rem;border-bottom:1px solid #2d3147;text-align:left}
tbody tr{border-bottom:1px solid #1e2235}
tbody tr:last-child{border-bottom:none}
tbody tr:hover{background:#ffffff05}
td{padding:.5rem .75rem;vertical-align:middle}
td.mono{font-family:monospace;color:#94a3b8;font-size:.8rem}
td.size{color:#e2e8f0;font-variant-numeric:tabular-nums}
td.ts{color:#64748b;font-variant-numeric:tabular-nums}
td.module{color:#7c85f5;font-weight:600}
.empty{text-align:center;color:#4b5563;padding:3rem;font-size:.9rem}
.btn{padding:.35rem .9rem;border-radius:4px;border:none;cursor:pointer;font-size:.82rem;font-weight:600;background:#2d3147;color:#94a3b8}
.btn:hover{background:#3a3f5c;color:#e2e8f0}
"#;

const JS: &str = r#"
var autoRefresh = true;
var refreshInterval = null;

function fmtTime(ts) {
  var d = new Date(ts * 1000);
  return d.toLocaleTimeString([], {hour12: false}) + '.' + String(d.getMilliseconds()).padStart(3,'0');
}

function renderTable(frames) {
  var tbody = document.getElementById('frames-body');
  if (!frames || frames.length === 0) {
    tbody.innerHTML = '<tr><td class="empty" colspan="5">No frames received yet.</td></tr>';
    return;
  }
  tbody.innerHTML = frames.map(function(f) {
    return '<tr>'
      + '<td class="ts">' + fmtTime(f.timestamp) + '</td>'
      + '<td class="module">M' + f.module + '</td>'
      + '<td class="size">' + f.size + ' B</td>'
      + '<td class="mono">' + f.data_hex + '</td>'
      + '</tr>';
  }).join('');
  document.getElementById('frame-count').textContent = frames.length + ' frames';
}

function fetchFrames() {
  fetch('/frames/json')
    .then(function(r) { return r.json(); })
    .then(function(data) { renderTable(data); })
    .catch(function() {});
}

function toggleAuto() {
  autoRefresh = !autoRefresh;
  var btn = document.getElementById('toggle-btn');
  btn.textContent = autoRefresh ? 'Pause' : 'Resume';
  if (autoRefresh) {
    refreshInterval = setInterval(fetchFrames, 2000);
  } else {
    clearInterval(refreshInterval);
  }
}

document.addEventListener('DOMContentLoaded', function() {
  fetchFrames();
  refreshInterval = setInterval(fetchFrames, 2000);
});
"#;

fn layout(body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { "Dashboard — Kaonic Gateway" }
                style { (PreEscaped(CSS)) }
            }
            body {
                header {
                    h1 { "⚡ Kaonic Gateway" }
                    nav {
                        a href="/" .active { "Dashboard" }
                        a href="/settings" { "Settings" }
                    }
                }
                main { (body) }
                script { (PreEscaped(JS)) }
            }
        }
    }
}

// ── GET / (dashboard) ────────────────────────────────────────────────────────

pub async fn get_dashboard(State(_settings): State<SharedSettings>) -> impl IntoResponse {
    let content = html! {
        h2 { "Received Frames" }

        div .card {
            div .toolbar {
                div {
                    span .badge.badge-live { "● LIVE" }
                    " "
                    span id="frame-count" .badge { "loading…" }
                }
                div .toolbar-right {
                    button .btn id="toggle-btn" onclick="toggleAuto()" { "Pause" }
                    button .btn onclick="fetchFrames()" { "↻ Refresh" }
                }
            }

            table {
                thead {
                    tr {
                        th { "Time" }
                        th { "Module" }
                        th { "Size" }
                        th { "Data (hex preview)" }
                    }
                }
                tbody id="frames-body" {
                    tr { td colspan="4" .empty { "Loading…" } }
                }
            }
        }
    };

    layout(content)
}

// ── GET /frames/json ─────────────────────────────────────────────────────────

pub async fn get_frames_json(
    State(settings): State<SharedSettings>,
) -> Json<Vec<FrameRecord>> {
    let frames = settings
        .lock()
        .unwrap()
        .recent_frames(200)
        .unwrap_or_default();
    Json(frames)
}
