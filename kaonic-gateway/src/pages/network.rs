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
    function setNodeText(id, text) {
        var el = document.getElementById(id);
        if (!el) { return; }
        el.textContent = text;
    }

    function setHidden(id, hidden) {
        var el = document.getElementById(id);
        if (!el) { return; }
        el.hidden = !!hidden;
    }

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

    function renderSnapshot(snapshot) {
        var wifi = snapshot.wifi || {};
        var isStation = wifi.mode === 'sta';
        var stationConnected = !!wifi.connected_ssid;

        setNodeText('wifi-mode-badge', isStation ? 'Station' : 'Access Point');
        setNodeText('network-backend', snapshot.backend || '—');
        setNodeText('wifi-configured-ssid', wifi.configured_ssid || '—');
        setNodeText('wifi-status-value', isStation ? (stationConnected ? 'Connected' : 'Disconnected') : 'Active');
        setNodeText('wifi-ip-hero-value', wifi.wlan0_ip || '—');
        setNodeText('wifi-connected-ssid', wifi.connected_ssid || '—');
        setNodeText('wifi-station-link-text', wifi.link_details || 'Disconnected');
        setNodeText('wifi-station-empty-text', 'Disconnected');
        setNodeText('interfaces-source', snapshot.interface_source || '—');
        setNodeText('interfaces-details', snapshot.interface_details || '');

        var apBtn = document.getElementById('wifi-mode-btn-ap');
        var staBtn = document.getElementById('wifi-mode-btn-sta');
        if (apBtn) { apBtn.classList.toggle('active', !isStation); }
        if (staBtn) { staBtn.classList.toggle('active', isStation); }

        setHidden('open-wifi-connect', !isStation);
        setHidden('wifi-station-fields', !isStation);
        setHidden('wifi-station-link', !isStation);
        setHidden('wifi-ap-fields', isStation);
        setHidden('wifi-ap-mode', isStation);
        setHidden('wifi-station-link-pre', !isStation || !stationConnected);
        setHidden('wifi-station-link-empty', !isStation || stationConnected);
    }

    function loadSnapshot() {
        return fetch('/api/network/snapshot', {
            headers: { 'Accept': 'application/json' }
        }).then(function(resp) {
            if (!resp.ok) {
                return resp.text().then(function(text) {
                    throw new Error(text || ('HTTP ' + resp.status));
                });
            }
            return resp.json();
        }).then(function(snapshot) {
            renderSnapshot(snapshot);
        });
    }

    document.addEventListener('click', function(ev) {
        var target = ev.target;
        if (!(target instanceof Element)) { return; }

        if (target.closest('#network-refresh-btn')) {
            setText('wifi-action-status', 'Refreshing...', 'pending');
            toggleBusy(true);
            loadSnapshot()
                .then(function() {
                    setText('wifi-action-status', '', '');
                })
                .catch(function(err) {
                    setText('wifi-action-status', String(err.message || err), 'err');
                })
                .finally(function() {
                    toggleBusy(false);
                });
            return;
        }

        var modeBtn = target.closest('[data-wifi-mode]');
        if (modeBtn) {
            if (modeBtn.classList.contains('active')) {
                return;
            }
            var nextMode = modeBtn.dataset.wifiMode === 'sta' ? 'Station' : 'Access Point';
            if (!window.confirm('Switch WiFi mode to ' + nextMode + '?')) {
                return;
            }
            setText('wifi-action-status', 'Applying WiFi mode...', 'pending');
            toggleBusy(true);
            postForm('/network/wifi/mode', { mode: modeBtn.dataset.wifiMode || '' })
                .then(function() { return loadSnapshot(); })
                .then(function() {
                    setText('wifi-action-status', '', '');
                })
                .catch(function(err) {
                    setText('wifi-action-status', String(err.message || err), 'err');
                })
                .finally(function() {
                    toggleBusy(false);
                });
            return;
        }

        if (target.closest('#open-wifi-connect')) {
            openModal();
            return;
        }

        if (target.closest('[data-close-connect]')) {
            closeModal();
        }
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
                return loadSnapshot();
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
            <div class="page-header">
                <h1 class="page-title">"Network"</h1>
                <button type="button" id="network-refresh-btn" class="btn-secondary" data-network-action>
                    "Refresh"
                </button>
            </div>
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
        </div>
    }
}

#[component]
fn NetworkContent(snapshot: NetworkSnapshotDto) -> impl IntoView {
    let wifi = snapshot.wifi.clone();
    let is_station = wifi.mode == "sta";
    let configured_ssid = wifi.configured_ssid.clone().unwrap_or_else(|| "—".into());
    let connected_ssid = wifi.connected_ssid.clone().unwrap_or_else(|| "—".into());
    let wlan0_ip = wifi.wlan0_ip.clone().unwrap_or_else(|| "—".into());
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
                    <span class="badge badge-ok" id="wifi-mode-badge">{mode_label}</span>
                </div>

                <div class="wifi-ip-hero">
                    <div class="wifi-ip-hero-label">"IP address"</div>
                    <div class="wifi-ip-hero-value" id="wifi-ip-hero-value">{wlan0_ip}</div>
                </div>

                <div class="network-mode-toggle">
                    <button
                        type="button"
                        class=if wifi.mode == "ap" { "wifi-mode-btn active" } else { "wifi-mode-btn" }
                        id="wifi-mode-btn-ap"
                        data-network-action
                        data-wifi-mode="ap"
                    >
                        "Access Point"
                    </button>
                    <button
                        type="button"
                        class=if wifi.mode == "sta" { "wifi-mode-btn active" } else { "wifi-mode-btn" }
                        id="wifi-mode-btn-sta"
                        data-network-action
                        data-wifi-mode="sta"
                    >
                        "Station"
                    </button>
                </div>

                <div class="network-actions">
                    <button
                        type="button"
                        class="btn-primary"
                        id="open-wifi-connect"
                        data-network-action
                        hidden=!is_station
                    >
                        "Connect WiFi"
                    </button>
                </div>

                <div id="wifi-action-status" class="network-status"></div>

                <div class="info-row">
                    <span class="info-label">"Backend"</span>
                    <span class="info-value" id="network-backend">{snapshot.backend}</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"Status"</span>
                    <span class="info-value" id="wifi-status-value">
                        {if is_station { station_status } else { "Active" }}
                    </span>
                </div>
                <div id="wifi-station-fields" hidden=!is_station>
                    <div class="info-row">
                        <span class="info-label">"Saved SSID"</span>
                        <span class="info-value" id="wifi-configured-ssid">{configured_ssid}</span>
                    </div>
                    <div class="info-row">
                        <span class="info-label">"Connected"</span>
                        <span class="info-value" id="wifi-connected-ssid">{connected_ssid}</span>
                    </div>
                </div>
                <div id="wifi-ap-fields" hidden=is_station></div>
                <div class="network-detail-block" id="wifi-station-link" hidden=!is_station>
                    <div class="network-subtitle">"Station link"</div>
                    <pre class="network-pre" id="wifi-station-link-pre" hidden=!station_connected>
                        <span id="wifi-station-link-text">{wifi.link_details}</span>
                    </pre>
                    <div class="network-empty-state" id="wifi-station-link-empty" hidden=station_connected>
                        <span id="wifi-station-empty-text">"Disconnected"</span>
                    </div>
                </div>
                <div class="network-detail-block" id="wifi-ap-mode" hidden=is_station>
                    <div class="network-subtitle">"Mode"</div>
                    <div class="network-empty-state">"Access Point mode enabled"</div>
                </div>
            </div>

            <div class="card network-card">
                <div class="card-header network-card-header">
                    <span class="card-title">"Interfaces"</span>
                    <span class="badge" id="interfaces-source">{snapshot.interface_source}</span>
                </div>
                <pre class="network-pre network-dump" id="interfaces-details">{snapshot.interface_details}</pre>
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
        <script>{NETWORK_JS}</script>
    }
    .into_any()
}
