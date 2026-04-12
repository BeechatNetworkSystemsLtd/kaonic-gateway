use leptos::prelude::*;

use crate::audio::{AudioCardSnapshot, AudioControlSnapshot};

#[server]
pub async fn load_audio_cards() -> Result<Vec<AudioCardSnapshot>, ServerFnError> {
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    state
        .audio
        .list_cards()
        .await
        .map_err(|err| ServerFnError::new(err.to_string()))
}

const MEDIA_JS: &str = r#"
(function() {
    function initAudioDeviceCard(device) {
        var saveEndpoint = device.dataset.saveEndpoint || '';
        var saveBtn = device.querySelector('[data-audio-card-save]');
        var status = device.querySelector('[data-audio-card-status]');
        var inFlight = 0;

        function setStatus(text, kind) {
            if (!status) { return; }
            status.textContent = text;
            status.className = 'audio-status media-card-status' + (kind ? ' ' + kind : '');
        }

        function setBusy(busy) {
            device.classList.toggle('is-loading', busy);
            if (saveBtn) { saveBtn.disabled = busy; }
        }

        function saveCard() {
            if (!saveEndpoint) { return; }
            inFlight += 1;
            setBusy(true);
            setStatus('Saving...', 'pending');

            fetch(saveEndpoint, {
                method: 'POST'
            }).then(function(resp) {
                if (!resp.ok) {
                    return resp.text().then(function(text) {
                        throw new Error(text || ('HTTP ' + resp.status));
                    });
                }
                return resp.json();
            }).then(function(data) {
                setStatus((data && data.status) || 'Saved', 'ok');
            }).catch(function(err) {
                setStatus(String(err && err.message ? err.message : err), 'err');
            }).finally(function() {
                inFlight = Math.max(0, inFlight - 1);
                setBusy(inFlight > 0);
            });
        }

        if (saveBtn) {
            saveBtn.addEventListener('click', saveCard);
        }

        setStatus('Ready', 'ok');
    }

    function initAudio(card) {
        var endpoint = card.dataset.endpoint || '';
        var testEndpoint = card.dataset.testEndpoint || '';
        var slider = card.querySelector('[data-audio-slider]');
        var display = card.querySelector('[data-audio-display]');
        var bars = card.querySelectorAll('.vol-bar');
        var muteBtn = card.querySelector('[data-audio-mute]');
        var testBtn = card.querySelector('[data-audio-test]');
        var status = card.querySelector('[data-audio-status]');
        var saveTimer = null;
        var inFlight = 0;
        var state = {
            volume: parseInt(card.dataset.volume || '0', 10) || 0,
            muted: card.dataset.muted === 'true',
            backend: card.dataset.backend || 'Mock'
        };

        function setStatus(text, kind) {
            if (!status) { return; }
            status.textContent = text;
            status.className = 'audio-status' + (kind ? ' ' + kind : '');
        }

        function setBusy(busy) {
            card.classList.toggle('is-loading', busy);
            if (slider) { slider.disabled = busy; }
            if (muteBtn) { muteBtn.disabled = busy; }
            if (testBtn) { testBtn.disabled = busy; }
        }

        function updateBars(vol) {
            var total = bars.length;
            bars.forEach(function(bar, i) {
                bar.classList.toggle('active', !state.muted && i < Math.round(vol / 100 * total));
            });
        }

        function applyVolume() {
            if (slider) { slider.value = String(state.volume); }
            if (display) {
                display.textContent = state.muted ? 'Muted' : state.volume + '%';
            }
            updateBars(state.muted ? 0 : state.volume);
            card.classList.toggle('is-muted', state.muted);
            if (muteBtn) {
                var label = muteBtn.querySelector('.mute-label');
                if (label) {
                    label.textContent = state.muted ? 'Unmute' : 'Mute';
                }
            }
        }

        function syncState(next) {
            if (!next) { return; }
            state.volume = Math.max(0, Math.min(100, parseInt(next.volume, 10) || 0));
            state.muted = !!next.muted;
            if (next.backend) {
                state.backend = next.backend;
            }
            card.dataset.volume = String(state.volume);
            card.dataset.muted = state.muted ? 'true' : 'false';
            card.dataset.backend = state.backend;
            applyVolume();
            setStatus(state.backend, 'ok');
        }

        function writeState() {
            inFlight += 1;
            setBusy(true);

            fetch(endpoint, {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    volume: state.volume,
                    muted: state.muted
                })
            }).then(function(resp) {
                if (!resp.ok) {
                    return resp.text().then(function(text) {
                        throw new Error(text || ('HTTP ' + resp.status));
                    });
                }
                return resp.json();
            }).then(syncState).catch(function(err) {
                setStatus(String(err && err.message ? err.message : err), 'err');
            }).finally(function() {
                inFlight = Math.max(0, inFlight - 1);
                setBusy(inFlight > 0);
            });
        }

        function playSample() {
            if (!testEndpoint) { return; }
            inFlight += 1;
            setBusy(true);
            setStatus('Playing sample...', 'pending');

            fetch(testEndpoint, {
                method: 'POST'
            }).then(function(resp) {
                if (!resp.ok) {
                    return resp.text().then(function(text) {
                        throw new Error(text || ('HTTP ' + resp.status));
                    });
                }
                return resp.json();
            }).then(syncState).catch(function(err) {
                setStatus(String(err && err.message ? err.message : err), 'err');
            }).finally(function() {
                inFlight = Math.max(0, inFlight - 1);
                setBusy(inFlight > 0);
            });
        }

        function scheduleWrite() {
            clearTimeout(saveTimer);
            saveTimer = setTimeout(writeState, 150);
        }

        if (slider) {
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
        }

        if (muteBtn) {
            muteBtn.addEventListener('click', function() {
                clearTimeout(saveTimer);
                state.muted = !state.muted;
                applyVolume();
                writeState();
            });
        }

        if (testBtn) {
            testBtn.addEventListener('click', function() {
                clearTimeout(saveTimer);
                playSample();
            });
        }

        applyVolume();
        setStatus(state.backend, 'ok');
    }

    document.querySelectorAll('[data-audio-device-card]').forEach(initAudioDeviceCard);
    document.querySelectorAll('[data-audio-control]').forEach(initAudio);
})();
"#;

#[component]
pub fn MediaPage() -> impl IntoView {
    let cards = Resource::new(|| (), |_| load_audio_cards());

    view! {
        <div class="page">
            <h1 class="page-title">"Media"</h1>
            <Suspense fallback=|| view! { <p class="loading">"Loading audio controls…"</p> }>
                {move || match cards.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(err)) => view! { <div class="error-banner">{err.to_string()}</div> }.into_any(),
                    Some(Ok(cards)) => view! { <MediaCards cards=cards /> }.into_any(),
                }}
            </Suspense>
        </div>
    }
}

#[component]
fn MediaCards(cards: Vec<AudioCardSnapshot>) -> impl IntoView {
    view! {
        <>
            <h2 class="section-title media-section-title">"Audio"</h2>
            <div class="media-device-grid">
                {cards
                    .into_iter()
                    .map(|card| view! { <AudioDeviceCard card=card /> })
                    .collect_view()}
            </div>
            <h2 class="section-title media-section-title">"Camera"</h2>
            <CameraSection/>
            <script>{MEDIA_JS}</script>
        </>
    }
}

#[component]
fn AudioDeviceCard(card: AudioCardSnapshot) -> impl IntoView {
    let control_count = card.controls.len();
    let save_endpoint = format!("/api/audio/{}/save", card.card_id);

    view! {
        <div class="card media-device-card" data-audio-device-card data-save-endpoint=save_endpoint>
            <div class="card-header">
                <span class="card-title">{card.card_name}</span>
                <div class="media-card-actions">
                    <span class="audio-status media-card-status ok" data-audio-card-status>"Ready"</span>
                    <span class="badge">{format!("{control_count} controls")}</span>
                    <button class="btn-secondary media-save-btn" type="button" data-audio-card-save>
                        "Save"
                    </button>
                </div>
            </div>
            {if control_count == 0 {
                view! { <p class="card-body-text">"No supported controls detected for this sound card."</p> }
                    .into_any()
            } else {
                view! {
                    <div class="media-controls-grid">
                        {card
                            .controls
                            .into_iter()
                            .map(|control| view! { <AudioControlCard card_id=card.card_id control=control /> })
                            .collect_view()}
                    </div>
                }
                .into_any()
            }}
        </div>
    }
}

#[component]
fn AudioControlCard(card_id: usize, control: AudioControlSnapshot) -> impl IntoView {
    let bar_count = 16usize;
    let active_bars = (control.volume as usize * bar_count) / 100;
    let control_key = format!("{card_id}-{}", control.control_id);
    let endpoint = format!("/api/audio/{card_id}/{}", control.control_id);
    let test_supported = matches!(control.control_id.as_str(), "speaker" | "headphones");
    let test_endpoint = test_supported
        .then(|| format!("/api/audio/{card_id}/{}/test", control.control_id))
        .unwrap_or_default();

    view! {
        <div
            class="audio-card"
            id=format!("audio-{control_key}")
            data-audio-control
            data-endpoint=endpoint
            data-test-endpoint=test_endpoint
            data-volume=control.volume.to_string()
            data-muted=if control.muted { "true" } else { "false" }
            data-backend=control.backend.clone()
        >
            <div class="card-header">
                <span class="card-title audio-title">
                    <span class="audio-icon" inner_html=audio_icon(&control.control_id)></span>
                    {control.label.clone()}
                </span>
                <span class="audio-status ok" data-audio-status>
                    {control.backend.clone()}
                </span>
            </div>

            <div class="vol-bars-wrap">
                <div class="vol-bars">
                    {(0..bar_count)
                        .map(|i| view! {
                            <div class=if i < active_bars { "vol-bar active" } else { "vol-bar" }></div>
                        })
                        .collect_view()}
                </div>
                <span class="vol-display" data-audio-display>
                    {if control.muted {
                        "Muted".to_string()
                    } else {
                        format!("{}%", control.volume)
                    }}
                </span>
            </div>

            <div class="audio-row">
                <span class="audio-label">"Volume"</span>
                <input
                    type="range"
                    class="audio-slider"
                    min="0"
                    max="100"
                    step="1"
                    value=control.volume.to_string()
                    data-audio-slider
                />
            </div>

            <div class="audio-row">
                <span class="audio-label">"Output"</span>
                <div class="audio-actions">
                    <button class="mute-btn" data-audio-mute>
                        <span class="mute-label">{if control.muted { "Unmute" } else { "Mute" }}</span>
                    </button>
                    {if test_supported {
                        view! { <button class="mute-btn" data-audio-test>"Test"</button> }.into_any()
                    } else {
                        view! {}.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}

fn audio_icon(control_id: &str) -> &'static str {
    match control_id {
        "speaker" => {
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/></svg>"#
        }
        "headphones" => {
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 18v-6a9 9 0 0 1 18 0v6"/><path d="M21 19a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3z"/><path d="M3 19a2 2 0 0 0 2 2h1a2 2 0 0 0 2-2v-3a2 2 0 0 0-2-2H3z"/></svg>"#
        }
        "microphone" => {
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V6a3 3 0 0 1 3-3z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><path d="M12 19v3"/><path d="M8 22h8"/></svg>"#
        }
        _ => {
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M8 12h8"/></svg>"#
        }
    }
}

#[component]
fn CameraSection() -> impl IntoView {
    view! {
        <div class="card media-camera-card">
            <div class="card-header">
                <span class="card-title">"Camera"</span>
                <div class="media-card-actions">
                    <span class="badge badge-warn">"Coming soon"</span>
                </div>
            </div>

            <div class="camera-grid">
                <div class="camera-preview">
                    <div class="camera-preview-frame">
                        <div class="camera-preview-label">"Preview"</div>
                        <div class="camera-preview-placeholder">
                            <span class="camera-preview-icon" inner_html=camera_icon()></span>
                            <span>"Live preview coming soon"</span>
                        </div>
                    </div>
                </div>

                <div class="camera-controls">
                    <div class="camera-control-row">
                        <span class="audio-label">"Mode"</span>
                        <button class="mute-btn" type="button" disabled>"Auto"</button>
                    </div>
                    <div class="camera-control-row">
                        <span class="audio-label">"Zoom"</span>
                        <input type="range" class="audio-slider" min="1" max="100" value="35" disabled/>
                    </div>
                    <div class="camera-control-row">
                        <span class="audio-label">"Exposure"</span>
                        <input type="range" class="audio-slider" min="1" max="100" value="55" disabled/>
                    </div>
                    <div class="camera-control-row">
                        <span class="audio-label">"Capture"</span>
                        <div class="audio-actions">
                            <button class="mute-btn" type="button" disabled>"Snapshot"</button>
                            <button class="mute-btn" type="button" disabled>"Record"</button>
                        </div>
                    </div>
                    <p class="camera-note">
                        "Mock camera controls only. Device camera integration and streaming preview are coming soon."
                    </p>
                </div>
            </div>
        </div>
    }
}

fn camera_icon() -> &'static str {
    r#"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M23 7l-7 5 7 5V7z"/><rect x="1" y="5" width="15" height="14" rx="2" ry="2"/></svg>"#
}
