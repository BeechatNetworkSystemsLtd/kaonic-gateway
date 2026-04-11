use leptos::prelude::*;

use crate::app_types::NetworkSnapshotDto;

#[server]
pub async fn load_network_snapshot() -> Result<NetworkSnapshotDto, ServerFnError> {
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    state
        .network
        .snapshot()
        .await
        .map_err(|err| ServerFnError::new(err.to_string()))
}

const NETWORK_JS: &str = r#"
(function() {
    function setText(id, text, kind) {
        var el = document.getElementById(id);
        if (!el) { return; }
        el.textContent = text;
        el.className = 'network-status' + (kind ? ' ' + kind : '');
    }

    function toggleBusy(disabled) {
        document.querySelectorAll('[data-network-action]').forEach(function(el) {
            el.disabled = disabled;
        });
    }

    function openModal() {
        var modal = document.getElementById('wifi-connect-modal');
        if (!modal) { return; }
        modal.hidden = false;
        document.body.classList.add('modal-open');
        var ssid = document.getElementById('wifi-ssid');
        if (ssid) { ssid.focus(); }
    }

    function closeModal() {
        var modal = document.getElementById('wifi-connect-modal');
        if (!modal) { return; }
        modal.hidden = true;
        document.body.classList.remove('modal-open');
        setText('wifi-connect-status', '', '');
    }

    function postForm(url, payload) {
        return fetch(url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8' },
            body: new URLSearchParams(payload)
        }).then(function(resp) {
            if (!resp.ok) {
                return resp.text().then(function(text) {
                    throw new Error(text || ('HTTP ' + resp.status));
                });
            }
        });
    }

    document.querySelectorAll('[data-wifi-mode]').forEach(function(btn) {
        btn.addEventListener('click', function() {
            setText('wifi-action-status', 'Applying WiFi mode...', 'pending');
            toggleBusy(true);
            postForm('/network/wifi/mode', { mode: btn.dataset.wifiMode || '' })
                .then(function() { window.location.reload(); })
                .catch(function(err) {
                    setText('wifi-action-status', String(err.message || err), 'err');
                })
                .finally(function() {
                    toggleBusy(false);
                });
        });
    });

    var openBtn = document.getElementById('open-wifi-connect');
    if (openBtn) {
        openBtn.addEventListener('click', openModal);
    }

    document.querySelectorAll('[data-close-connect]').forEach(function(btn) {
        btn.addEventListener('click', closeModal);
    });

    var modal = document.getElementById('wifi-connect-modal');
    if (modal) {
        modal.addEventListener('click', function(ev) {
            if (ev.target === modal) { closeModal(); }
        });
    }

    var connectForm = document.getElementById('wifi-connect-form');
    if (connectForm) {
        connectForm.addEventListener('submit', function(ev) {
            ev.preventDefault();
            var ssid = document.getElementById('wifi-ssid');
            var psk = document.getElementById('wifi-psk');
            setText('wifi-connect-status', 'Connecting...', 'pending');
            toggleBusy(true);
            postForm('/network/wifi/connect', {
                ssid: ssid ? ssid.value : '',
                psk: psk ? psk.value : ''
            }).then(function() {
                closeModal();
                window.location.reload();
            }).catch(function(err) {
                setText('wifi-connect-status', String(err.message || err), 'err');
            }).finally(function() {
                toggleBusy(false);
            });
        });
    }

    window.addEventListener('keydown', function(ev) {
        if (ev.key === 'Escape') {
            closeModal();
        }
    });
})();
"#;

#[component]
pub fn NetworkPage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_network_snapshot());

    view! {
        <div class="page">
            <h1 class="page-title">"Network"</h1>
            <Suspense fallback=|| view! { <p class="loading">"Loading network details…"</p> }>
                {move || match snapshot.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(err)) => view! {
                        <div class="error-banner">{err.to_string()}</div>
                    }.into_any(),
                    Some(Ok(snapshot)) => view! {
                        <NetworkContent snapshot=snapshot />
                    }.into_any(),
                }}
            </Suspense>
            <script>{NETWORK_JS}</script>
        </div>
    }
}

#[component]
fn NetworkContent(snapshot: NetworkSnapshotDto) -> impl IntoView {
    let wifi = snapshot.wifi.clone();
    let is_station = wifi.mode == "sta";
    let configured_ssid = wifi.configured_ssid.clone().unwrap_or_else(|| "—".into());
    let connected_ssid = wifi.connected_ssid.clone().unwrap_or_else(|| "—".into());
    let station_connected = wifi.connected_ssid.is_some();
    let mode_label = if is_station {
        "Station"
    } else {
        "Access Point"
    };
    let station_status = if station_connected {
        "Connected"
    } else {
        "Disconnected"
    };

    view! {
        <div class="network-grid">
            <div class="card network-card">
                <div class="card-header network-card-header">
                    <span class="card-title">"WiFi"</span>
                    <span class="badge badge-ok">{mode_label}</span>
                </div>

                <div class="network-mode-toggle">
                    <button
                        type="button"
                        class=if wifi.mode == "ap" { "wifi-mode-btn active" } else { "wifi-mode-btn" }
                        data-network-action
                        data-wifi-mode="ap"
                    >
                        "Access Point"
                    </button>
                    <button
                        type="button"
                        class=if wifi.mode == "sta" { "wifi-mode-btn active" } else { "wifi-mode-btn" }
                        data-network-action
                        data-wifi-mode="sta"
                    >
                        "Station"
                    </button>
                </div>

                <div class="network-actions">
                    {is_station.then(|| view! {
                        <button
                            type="button"
                            class="btn-primary"
                            id="open-wifi-connect"
                            data-network-action
                        >
                            "Connect WiFi"
                        </button>
                    })}
                    <button
                        type="button"
                        class="btn-secondary"
                        onclick="window.location.reload()"
                        data-network-action
                    >
                        "Refresh"
                    </button>
                </div>

                <div id="wifi-action-status" class="network-status"></div>

                <div class="info-row">
                    <span class="info-label">"Backend"</span>
                    <span class="info-value">{snapshot.backend}</span>
                </div>
                {if is_station {
                    view! {
                        <>
                            <div class="info-row">
                                <span class="info-label">"Saved SSID"</span>
                                <span class="info-value">{configured_ssid}</span>
                            </div>
                            <div class="info-row">
                                <span class="info-label">"Status"</span>
                                <span class="info-value">{station_status}</span>
                            </div>
                            <div class="info-row">
                                <span class="info-label">"Connected"</span>
                                <span class="info-value">{connected_ssid}</span>
                            </div>
                            {if station_connected {
                                view! {
                                    <div class="network-detail-block">
                                        <div class="network-subtitle">"Station link"</div>
                                        <pre class="network-pre">{wifi.link_details}</pre>
                                    </div>
                                }.into_any()
                            } else {
                                view! {
                                    <div class="network-detail-block">
                                        <div class="network-subtitle">"Station link"</div>
                                        <div class="network-empty-state">"Disconnected"</div>
                                    </div>
                                }.into_any()
                            }}
                        </>
                    }.into_any()
                } else {
                    view! {
                        <>
                            <div class="info-row">
                                <span class="info-label">"Status"</span>
                                <span class="info-value">"Active"</span>
                            </div>
                            <div class="network-detail-block">
                                <div class="network-subtitle">"Mode"</div>
                                <div class="network-empty-state">"Access Point mode enabled"</div>
                            </div>
                        </>
                    }.into_any()
                }}
            </div>

            <div class="card network-card">
                <div class="card-header network-card-header">
                    <span class="card-title">"Interfaces"</span>
                    <span class="badge">{snapshot.interface_source}</span>
                </div>
                <pre class="network-pre network-dump">{snapshot.interface_details}</pre>
            </div>
        </div>

        <div class="modal-backdrop" id="wifi-connect-modal" hidden>
            <div class="modal-card">
                <div class="modal-header">
                    <h2 class="modal-title">"Connect to WiFi"</h2>
                    <button type="button" class="modal-close" data-close-connect>"×"</button>
                </div>
                <form id="wifi-connect-form" class="modal-form">
                    <label class="form-label" for="wifi-ssid">"SSID"</label>
                    <input id="wifi-ssid" name="ssid" class="form-input" autocomplete="off" required />

                    <label class="form-label" for="wifi-psk">"PSK"</label>
                    <input
                        id="wifi-psk"
                        name="psk"
                        type="password"
                        class="form-input"
                        minlength="8"
                        maxlength="63"
                        required
                    />

                    <div id="wifi-connect-status" class="network-status"></div>

                    <div class="modal-actions">
                        <button type="button" class="btn-secondary" data-close-connect data-network-action>
                            "Cancel"
                        </button>
                        <button type="submit" class="btn-primary" data-network-action>
                            "Connect"
                        </button>
                    </div>
                </form>
            </div>
        </div>
    }
    .into_any()
}
