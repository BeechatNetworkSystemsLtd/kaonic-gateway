use leptos::prelude::*;

use super::PageTitle;
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

const NETWORK_JS: &str = r##"
(function() {
    var state = { snapshot: null, scan: [], saved: [], firewall: null, scanning: false, selectedSsid: null };

    function $(id) { return document.getElementById(id); }
    function esc(v) {
        return String(v == null ? '' : v)
            .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }
    function flash(msg, kind) {
        var el = $('network-flash');
        if (!el) { return; }
        el.textContent = msg;
        el.className = 'remote-flash ' + (kind || 'info');
        el.hidden = false;
        clearTimeout(el._t);
        el._t = setTimeout(function() { el.hidden = true; }, 6000);
    }
    function api(method, path, body, form) {
        var opts = { method: method, headers: {} };
        if (body !== undefined && body !== null) {
            if (form) {
                opts.headers['Content-Type'] = 'application/x-www-form-urlencoded';
                opts.body = body;
            } else {
                opts.headers['Content-Type'] = 'application/json';
                opts.body = JSON.stringify(body);
            }
        }
        return fetch(path, opts).then(function(r) {
            return r.text().then(function(t) {
                var data = null;
                try { data = t ? JSON.parse(t) : null; } catch (e) { data = { detail: t }; }
                if (!r.ok) { throw new Error((data && (data.detail || data.status)) || t || ('HTTP ' + r.status)); }
                return data;
            });
        });
    }
    function signalBars(dbm) {
        var bars = dbm >= -60 ? 4 : dbm >= -70 ? 3 : dbm >= -80 ? 2 : 1;
        var out = '<span class="sig" title="' + esc(dbm) + ' dBm">';
        for (var i = 1; i <= 4; i++) { out += '<i class="sig-bar' + (i <= bars ? ' on' : '') + '"></i>'; }
        return out + '</span>';
    }

    // ── Status banner ────────────────────────────────────────────────────────
    function renderStatus(snap) {
        if (!snap) { return; }
        state.snapshot = snap;
        var wifi = snap.wifi || {};
        var sta = wifi.mode === 'sta';
        var connected = !!wifi.connected_ssid;
        var dot = $('net-status-dot');
        if (dot) { dot.className = 'status-dot ' + (sta ? (connected ? 'status-dot--ok' : 'status-dot--warn') : 'status-dot--ok'); }
        setText('net-status-text', sta ? (connected ? wifi.connected_ssid : 'Not connected') : 'Access Point');
        setText('net-mode-value', sta ? 'Station' : 'Access Point');
        setText('net-ip-value', wifi.wlan0_ip || '—');
        setText('net-antenna-value', wifi.antenna_supported ? wifi.antenna : 'fixed');
        setText('net-hostapd-value', sta ? wifi.wpa_supplicant_status : wifi.hostapd_status);
        var banner = $('net-banner');
        if (banner) {
            banner.className = 'vpn-banner remote-banner ' +
                (sta && !connected ? 'vpn-banner--idle' : 'vpn-banner--ok');
        }
        ['ap', 'sta'].forEach(function(mode) {
            var btn = $('wifi-mode-btn-' + mode);
            if (btn) { btn.classList.toggle('active', wifi.mode === mode); }
        });
        ['internal', 'external'].forEach(function(a) {
            var btn = $('wifi-antenna-btn-' + a);
            if (btn) { btn.classList.toggle('active', wifi.antenna === a); }
        });
        var antennaSection = $('wifi-antenna-section');
        if (antennaSection) { antennaSection.hidden = !wifi.antenna_supported; }
        var link = $('net-link-details');
        if (link) { link.textContent = wifi.link_details || (sta ? 'Disconnected' : 'Access Point mode'); }
        setText('net-iface-details', snap.interface_details || '');
        renderScan();
    }
    function setText(id, text) { var el = $(id); if (el) { el.textContent = text; } }

    // ── Wi-Fi scan + saved networks ──────────────────────────────────────────
    function refreshSaved() {
        return api('GET', '/api/network/wifi/networks').then(function(list) {
            state.saved = list || [];
            renderScan();
        }).catch(function() {});
    }
    function scan() {
        if (state.scanning) { return; }
        state.scanning = true;
        renderScan();
        api('GET', '/api/network/wifi/scan')
            .then(function(list) { state.scan = list || []; })
            .catch(function(e) { flash('Scan failed: ' + e.message, 'err'); })
            .then(function() { state.scanning = false; renderScan(); });
    }
    function savedFor(ssid) {
        for (var i = 0; i < state.saved.length; i++) {
            if (state.saved[i].ssid === ssid) { return state.saved[i]; }
        }
        return null;
    }
    function renderScan() {
        var el = $('wifi-list');
        if (!el) { return; }
        var wifi = (state.snapshot && state.snapshot.wifi) || {};
        var sta = wifi.mode === 'sta';
        if (!sta) {
            el.innerHTML = '<p class="card-body-text">Access Point mode — switch to Station to scan</p>';
            return;
        }
        // Saved networks that the scan did not see are still listed, greyed out.
        var seen = {};
        var rows = state.scan.map(function(ap) {
            seen[ap.ssid] = true;
            return { ssid: ap.ssid, signal: ap.signal_dbm, channel: ap.channel, security: ap.security,
                     connected: ap.connected, saved: !!savedFor(ap.ssid), visible: true };
        });
        state.saved.forEach(function(s) {
            if (!seen[s.ssid]) {
                rows.push({ ssid: s.ssid, signal: null, channel: 0, security: '', connected: false, saved: true, visible: false });
            }
        });
        if (!rows.length) {
            el.innerHTML = '<p class="card-body-text">' + (state.scanning ? 'Scanning…' : 'No networks found. Press Scan.') + '</p>';
            return;
        }
        el.innerHTML = rows.map(function(r) {
            var cls = 'wifi-row' + (r.connected ? ' wifi-row--connected' : '') + (r.visible ? '' : ' wifi-row--offline');
            return '<div class="' + cls + '" data-ssid="' + esc(r.ssid) + '">' +
                '<div class="wifi-row-sig">' + (r.signal != null ? signalBars(r.signal) : '<span class="sig"></span>') + '</div>' +
                '<div class="wifi-row-main"><span class="wifi-row-ssid">' + esc(r.ssid) + '</span>' +
                '<span class="wifi-row-meta">' +
                (r.signal != null ? esc(r.signal) + ' dBm' : 'not in range') +
                (r.channel ? ' · ch ' + esc(r.channel) : '') +
                (r.security ? ' · ' + esc(r.security) : '') +
                (r.saved ? ' · saved' : '') + '</span></div>' +
                '<div class="wifi-row-actions">' +
                (r.connected ? '<span class="badge badge-ok">connected</span>' :
                  (r.saved ? '<button class="btn-secondary btn-small" data-net-act="connect-saved" data-ssid="' + esc(r.ssid) + '">Connect</button>' :
                             '<button class="btn-secondary btn-small" data-net-act="join" data-ssid="' + esc(r.ssid) + '">Join…</button>')) +
                (r.saved ? '<button class="btn-secondary btn-small" data-net-act="forget" data-ssid="' + esc(r.ssid) + '">Forget</button>' : '') +
                '</div></div>';
        }).join('');
        el.querySelectorAll('button[data-net-act]').forEach(function(btn) {
            btn.addEventListener('click', function() {
                var ssid = btn.getAttribute('data-ssid');
                var act = btn.getAttribute('data-net-act');
                if (act === 'join') { openJoin(ssid); return; }
                if (act === 'forget') {
                    if (!window.confirm('Forget ' + ssid + '?')) { return; }
                    btn.disabled = true;
                    api('DELETE', '/api/network/wifi/networks/' + encodeURIComponent(ssid))
                        .then(function(r) { flash(r.status, 'ok'); return refreshSaved(); })
                        .catch(function(e) { flash(e.message, 'err'); btn.disabled = false; });
                    return;
                }
                btn.disabled = true;
                btn.textContent = 'Connecting…';
                api('POST', '/api/network/wifi/networks/' + encodeURIComponent(ssid) + '/connect')
                    .then(function(r) { flash(r.status, 'ok'); setTimeout(refreshAll, 4000); })
                    .catch(function(e) { flash(e.message, 'err'); btn.disabled = false; btn.textContent = 'Connect'; });
            });
        });
    }
    function openJoin(ssid) {
        state.selectedSsid = ssid || '';
        var modal = $('wifi-connect-modal');
        if (!modal) { return; }
        $('wifi-ssid').value = ssid || '';
        $('wifi-psk').value = '';
        modal.hidden = false;
        document.body.classList.add('modal-open');
        ($('wifi-psk') && ssid ? $('wifi-psk') : $('wifi-ssid')).focus();
    }
    function closeJoin() {
        var modal = $('wifi-connect-modal');
        if (!modal) { return; }
        modal.hidden = true;
        document.body.classList.remove('modal-open');
    }

    // ── VPN firewall / routes ────────────────────────────────────────────────
    function refreshFirewall() {
        return api('GET', '/api/network/firewall').then(function(f) {
            state.firewall = f;
            var rules = $('fw-rules');
            if (rules) {
                if (!f.available) {
                    rules.innerHTML = '<tr><td colspan="2" class="frames-empty">' + esc(f.detail || 'iptables unavailable') + '</td></tr>';
                } else if (!f.rules.length) {
                    rules.innerHTML = '<tr><td colspan="2" class="frames-empty">No translation rules</td></tr>';
                } else {
                    rules.innerHTML = f.rules.map(function(r) {
                        return '<tr><td class="td-time">' + esc(r.chain.replace('KAONIC_VPN_', '')) +
                            '</td><td class="td-hex">' + esc(r.rule.replace(/^-A \S+ /, '')) + '</td></tr>';
                    }).join('');
                }
            }
            var routes = $('fw-routes');
            if (routes) {
                routes.innerHTML = (f.routes || []).map(function(r) {
                    return '<tr><td class="td-hex">' + esc(r.destination) + '</td><td class="td-time">' +
                        esc(r.via || '—') + '</td><td class="td-time">' + esc(r.device || '—') + '</td>' +
                        '<td class="td-time">' + esc(r.scope || '—') + '</td></tr>';
                }).join('') || '<tr><td colspan="4" class="frames-empty">No routes</td></tr>';
            }
            var fwd = $('fw-forwarding');
            if (fwd) {
                fwd.textContent = f.forwarding ? 'enabled' : 'disabled';
                fwd.className = 'badge ' + (f.forwarding ? 'badge-ok' : 'badge-warn');
            }
        }).catch(function() {});
    }

    function refreshAll() {
        return Promise.all([
            api('GET', '/api/network/snapshot').then(renderStatus).catch(function() {}),
            refreshSaved(),
            refreshFirewall()
        ]);
    }

    // ── Bindings ─────────────────────────────────────────────────────────────
    document.addEventListener('click', function(ev) {
        var t = ev.target;
        if (!t || !t.getAttribute) { return; }
        if (t.hasAttribute('data-close-connect')) { closeJoin(); }
    });
    ['ap', 'sta'].forEach(function(mode) {
        var btn = $('wifi-mode-btn-' + mode);
        if (!btn) { return; }
        btn.addEventListener('click', function() {
            btn.disabled = true;
            api('POST', '/network/wifi/mode', 'mode=' + mode, true)
                .then(function() { flash('Switched to ' + (mode === 'ap' ? 'Access Point' : 'Station'), 'ok'); setTimeout(refreshAll, 3000); })
                .catch(function(e) { flash(e.message, 'err'); })
                .then(function() { btn.disabled = false; });
        });
    });
    ['internal', 'external'].forEach(function(a) {
        var btn = $('wifi-antenna-btn-' + a);
        if (!btn) { return; }
        btn.addEventListener('click', function() {
            btn.disabled = true;
            api('POST', '/network/wifi/antenna', 'antenna=' + a, true)
                .then(function() { flash('Antenna: ' + a, 'ok'); setTimeout(refreshAll, 1500); })
                .catch(function(e) { flash(e.message, 'err'); })
                .then(function() { btn.disabled = false; });
        });
    });
    var scanBtn = $('wifi-scan-btn');
    if (scanBtn) { scanBtn.addEventListener('click', scan); }
    var addBtn = $('wifi-add-btn');
    if (addBtn) { addBtn.addEventListener('click', function() { openJoin(''); }); }
    var refreshBtn = $('network-refresh-btn');
    if (refreshBtn) { refreshBtn.addEventListener('click', function() { refreshAll(); scan(); }); }
    var form = $('wifi-connect-form');
    if (form) {
        form.addEventListener('submit', function(ev) {
            ev.preventDefault();
            var ssid = ($('wifi-ssid').value || '').trim();
            var psk = $('wifi-psk').value || '';
            if (!ssid) { return; }
            var save = $('wifi-save-only').checked;
            var btn = $('wifi-connect-submit');
            btn.disabled = true;
            var req = save
                ? api('POST', '/api/network/wifi/networks', { ssid: ssid, psk: psk })
                : api('POST', '/network/wifi/connect', 'ssid=' + encodeURIComponent(ssid) + '&psk=' + encodeURIComponent(psk), true);
            req.then(function(r) {
                flash((r && r.status) || ('Connecting to ' + ssid), 'ok');
                closeJoin();
                return refreshSaved();
            })
            .then(function() { if (!save) { setTimeout(refreshAll, 4000); } })
            .catch(function(e) { flash(e.message, 'err'); })
            .then(function() { btn.disabled = false; });
        });
    }
    document.addEventListener('keydown', function(ev) { if (ev.key === 'Escape') { closeJoin(); } });

    refreshAll();
    setInterval(function() {
        if (document.body.classList.contains('modal-open')) { return; }
        api('GET', '/api/network/snapshot').then(renderStatus).catch(function() {});
        refreshFirewall();
    }, 10000);
})();
"##;

#[component]
pub fn NetworkPage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_network_snapshot());

    view! {
        <div class="page">
            <div class="page-header">
                <PageTitle icon="🌐" title="Network" />
                <button type="button" id="network-refresh-btn" class="btn-secondary">
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
    let status_text = if is_station {
        wifi.connected_ssid.clone().unwrap_or_else(|| "Not connected".into())
    } else {
        "Access Point".to_string()
    };
    let dot_class = if is_station && wifi.connected_ssid.is_none() {
        "status-dot status-dot--warn"
    } else {
        "status-dot status-dot--ok"
    };
    let banner_class = if is_station && wifi.connected_ssid.is_none() {
        "vpn-banner remote-banner vpn-banner--idle"
    } else {
        "vpn-banner remote-banner vpn-banner--ok"
    };
    let service_status = if is_station {
        wifi.wpa_supplicant_status.clone()
    } else {
        wifi.hostapd_status.clone()
    };

    view! {
        <div class="remote-flash" id="network-flash" hidden=true></div>

        <div class=banner_class id="net-banner">
            <div class="vpn-banner-lead">
                <span class=dot_class id="net-status-dot"></span>
                <span class="vpn-banner-status-text" id="net-status-text">{status_text}</span>
            </div>
            <div class="vpn-banner-divider"></div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Mode"</span>
                <span class="vpn-banner-ip" id="net-mode-value">
                    {if is_station { "Station" } else { "Access Point" }}
                </span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"wlan0 IP"</span>
                <span class="vpn-banner-ip" id="net-ip-value">
                    {wifi.wlan0_ip.clone().unwrap_or_else(|| "—".into())}
                </span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Antenna"</span>
                <span class="vpn-banner-ip" id="net-antenna-value">
                    {if wifi.antenna_supported { wifi.antenna.clone() } else { "fixed".into() }}
                </span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Service"</span>
                <span class="vpn-banner-ip" id="net-hostapd-value">{service_status}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Backend"</span>
                <span class="vpn-banner-ip">{snapshot.backend}</span>
            </div>
        </div>

        <div class="network-grid">
            <div class="card network-card">
                <div class="card-header">
                    <span class="card-title">"Wi-Fi"</span>
                    <div class="network-mode-toggle network-mode-toggle--inline">
                        <button type="button" class="wifi-mode-btn" id="wifi-mode-btn-ap">"AP"</button>
                        <button type="button" class="wifi-mode-btn" id="wifi-mode-btn-sta">"Station"</button>
                    </div>
                </div>
                <div class="network-actions">
                    <button type="button" class="btn-secondary" id="wifi-scan-btn">"Scan"</button>
                    <button type="button" class="btn-secondary" id="wifi-add-btn">"Add network…"</button>
                </div>
                <div id="wifi-list" class="wifi-list">
                    <p class="card-body-text">"Loading…"</p>
                </div>
                <div class="network-detail-block" id="wifi-antenna-section" hidden=!wifi.antenna_supported>
                    <div class="network-subtitle">"Antenna"</div>
                    <div class="network-mode-toggle">
                        <button type="button" class="wifi-mode-btn" id="wifi-antenna-btn-internal">"Internal"</button>
                        <button type="button" class="wifi-mode-btn" id="wifi-antenna-btn-external">"External"</button>
                    </div>
                </div>
            </div>

            <div class="card network-card">
                <div class="card-header"><span class="card-title">"Link"</span></div>
                <pre class="network-pre" id="net-link-details">{wifi.link_details}</pre>
                <div class="card-header"><span class="card-title">"Interfaces"</span></div>
                <pre class="network-pre network-dump" id="net-iface-details">{snapshot.interface_details}</pre>
            </div>
        </div>

        <div class="card network-card">
            <div class="card-header">
                <span class="card-title">"VPN firewall rules"</span>
                <span class="badge" id="fw-forwarding">"—"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead><tr><th>"Chain"</th><th>"Rule"</th></tr></thead>
                    <tbody id="fw-rules"><tr><td colspan="2" class="frames-empty">"Loading…"</td></tr></tbody>
                </table>
            </div>
        </div>

        <div class="card network-card">
            <div class="card-header"><span class="card-title">"Routing table"</span></div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead><tr><th>"Destination"</th><th>"Via"</th><th>"Device"</th><th>"Scope"</th></tr></thead>
                    <tbody id="fw-routes"><tr><td colspan="4" class="frames-empty">"Loading…"</td></tr></tbody>
                </table>
            </div>
        </div>

        <div class="modal-backdrop" id="wifi-connect-modal" hidden>
            <div class="modal-card">
                <div class="modal-header">
                    <h2 class="modal-title">"Wi-Fi network"</h2>
                    <button type="button" class="modal-close" data-close-connect>"×"</button>
                </div>
                <form id="wifi-connect-form" class="modal-form">
                    <label class="form-label" for="wifi-ssid">"SSID"</label>
                    <input id="wifi-ssid" name="ssid" class="form-input" autocomplete="off" required />

                    <label class="form-label" for="wifi-psk">"Password"</label>
                    <input id="wifi-psk" name="psk" type="password" class="form-input" minlength="8" maxlength="63" />

                    <label class="remote-toggle">
                        <input type="checkbox" id="wifi-save-only"/>
                        <span>"Save only (do not connect now)"</span>
                    </label>

                    <div class="modal-actions">
                        <button type="button" class="btn-secondary" data-close-connect>"Cancel"</button>
                        <button type="submit" class="btn-primary" id="wifi-connect-submit">"Connect"</button>
                    </div>
                </form>
            </div>
        </div>
        <script>{NETWORK_JS}</script>
    }
    .into_any()
}
