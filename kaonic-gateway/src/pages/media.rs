use leptos::prelude::*;

const MEDIA_JS: &str = r#"
(function() {
    var API_ROOT = '/api/audio/';

    function initAudio(id) {
        var card    = document.getElementById('audio-' + id);
        var slider  = document.getElementById('vol-' + id);
        var display = document.getElementById('voldsp-' + id);
        var bars    = document.querySelectorAll('#volbars-' + id + ' .vol-bar');
        var muteBtn = document.getElementById('mute-' + id);
        var status  = document.getElementById('status-' + id);
        var saveTimer = null;
        var inFlight = 0;
        var state = {
            volume: parseInt(slider.value, 10) || 0,
            muted: false,
            backend: 'Mock'
        };

        function setStatus(text, kind) {
            if (!status) { return; }
            status.textContent = text;
            status.className = 'audio-status' + (kind ? ' ' + kind : '');
        }

        function setBusy(busy) {
            card.classList.toggle('is-loading', busy);
            slider.disabled = busy;
            muteBtn.disabled = busy;
        }

        function updateBars(vol) {
            var total = bars.length;
            bars.forEach(function(b, i) {
                b.classList.toggle('active', !state.muted && i < Math.round(vol / 100 * total));
            });
        }

        function applyVolume() {
            slider.value = String(state.volume);
            display.textContent = state.muted ? 'Muted' : state.volume + '%';
            updateBars(state.muted ? 0 : state.volume);
            card.classList.toggle('is-muted', state.muted);
            muteBtn.querySelector('.mute-label').textContent = state.muted ? 'Unmute' : 'Mute';
        }

        function syncState(next) {
            if (!next) { return; }
            state.volume = Math.max(0, Math.min(100, parseInt(next.volume, 10) || 0));
            state.muted = !!next.muted;
            if (next.backend) {
                state.backend = next.backend;
            }
            applyVolume();
            setStatus(state.backend, 'ok');
        }

        function readState() {
            setBusy(true);
            fetch(API_ROOT + id)
                .then(function(resp) {
                    if (!resp.ok) {
                        return resp.text().then(function(text) {
                            throw new Error(text || ('HTTP ' + resp.status));
                        });
                    }
                    return resp.json();
                })
                .then(syncState)
                .catch(function(err) {
                    setStatus(String(err && err.message ? err.message : err), 'err');
                })
                .finally(function() {
                    setBusy(false);
                });
        }

        function writeState() {
            inFlight += 1;
            setBusy(true);

            fetch(API_ROOT + id, {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    volume: state.volume,
                    muted: state.muted
                })
            })
                .then(function(resp) {
                    if (!resp.ok) {
                        return resp.text().then(function(text) {
                            throw new Error(text || ('HTTP ' + resp.status));
                        });
                    }
                    return resp.json();
                })
                .then(syncState)
                .catch(function(err) {
                    setStatus(String(err && err.message ? err.message : err), 'err');
                })
                .finally(function() {
                    inFlight = Math.max(0, inFlight - 1);
                    setBusy(inFlight > 0);
                });
        }

        function scheduleWrite() {
            clearTimeout(saveTimer);
            saveTimer = setTimeout(writeState, 150);
        }

        slider.addEventListener('input', function() {
            state.volume = Math.max(0, Math.min(100, parseInt(slider.value, 10) || 0));
            if (state.muted) { state.muted = false; }
            applyVolume();
            scheduleWrite();
        });

        slider.addEventListener('change', function() {
            clearTimeout(saveTimer);
            writeState();
        });

        muteBtn.addEventListener('click', function() {
            clearTimeout(saveTimer);
            state.muted = !state.muted;
            applyVolume();
            writeState();
        });

        applyVolume();
        setStatus('Loading...', 'pending');
        readState();
    }

    initAudio('speaker');
    initAudio('headphones');
})();
"#;

#[component]
pub fn MediaPage() -> impl IntoView {
    view! {
        <div class="page">
            <h1 class="page-title">"Media"</h1>
            <div class="media-grid">
                <AudioCard
                    id="speaker"
                    title="Speaker"
                    icon=r#"<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/></svg>"#
                    default_volume=75
                />
                <AudioCard
                    id="headphones"
                    title="Headphones"
                    icon=r#"<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 18v-6a9 9 0 0 1 18 0v6"/><path d="M21 19a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3z"/><path d="M3 19a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-3a2 2 0 0 0-2-2H3z"/></svg>"#
                    default_volume=60
                />
            </div>
            <script>{MEDIA_JS}</script>
        </div>
    }
}

#[component]
fn AudioCard(
    id: &'static str,
    title: &'static str,
    icon: &'static str,
    default_volume: u8,
) -> impl IntoView {
    let bar_count = 16usize;
    let active_bars = (default_volume as usize * bar_count) / 100;

    view! {
        <div class="card audio-card" id=format!("audio-{id}")>
            <div class="card-header">
                <span class="card-title audio-title">
                    <span class="audio-icon" inner_html=icon></span>
                    {title}
                </span>
                <span class="audio-status pending" id=format!("status-{id}")>
                    "Loading..."
                </span>
            </div>

            // Volume visualizer bars
            <div class="vol-bars-wrap">
                <div class="vol-bars" id=format!("volbars-{id}")>
                    {(0..bar_count).map(|i| view! {
                        <div class=if i < active_bars { "vol-bar active" } else { "vol-bar" }></div>
                    }).collect_view()}
                </div>
                <span class="vol-display" id=format!("voldsp-{id}")>
                    {format!("{}%", default_volume)}
                </span>
            </div>

            // Volume slider
            <div class="audio-row">
                <span class="audio-label">"Volume"</span>
                <input
                    type="range"
                    id=format!("vol-{id}")
                    class="audio-slider"
                    min="0" max="100" step="1"
                    value=default_volume.to_string()
                />
            </div>

            // Mute toggle
            <div class="audio-row">
                <span class="audio-label">"Output"</span>
                <button class="mute-btn" id=format!("mute-{id}")>
                    <span class="mute-label">"Mute"</span>
                </button>
            </div>
        </div>
    }
    .into_any()
}
