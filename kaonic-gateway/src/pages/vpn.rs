use leptos::prelude::*;
use kaonic_vpn::{VpnPeerSnapshot, VpnRouteSnapshot, VpnSnapshot};
use serde::{Deserialize, Serialize};

use super::PageTitle;

fn format_timestamp(ts: u64) -> String {
    let seconds = ts % 86_400;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    format!("{hours:02}:{minutes:02}:{secs:02} UTC")
}

fn format_hash(value: &str) -> String {
    let compact = value.split_whitespace().collect::<String>();
    if compact.is_empty() {
        return "—".into();
    }
    compact
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn vpn_badge_class(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "running" | "active" | "ready" | "installed" | "yes" => "badge-ok",
        "discovered" | "configured" | "pending" | "starting" => "badge-warn",
        "error" | "closed" | "failed" | "no" | "drop" => "badge-err",
        _ => "reticulum-badge-soft",
    }
}

fn vpn_backend_badge_class(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "linux" => "reticulum-badge-kind-data",
        "mock" => "reticulum-badge-soft",
        _ => "reticulum-badge-kind-link",
    }
}

fn vpn_error_badge_class(value: &str) -> &'static str {
    if value == "—" {
        "reticulum-badge-soft"
    } else {
        "badge-err"
    }
}

fn render_vpn_badge(value: &str, class_fn: fn(&str) -> &'static str) -> impl IntoView {
    let class_name = format!("badge {}", class_fn(value));
    view! { <span class=class_name>{value.to_string()}</span> }
}

fn render_vpn_badge_class(value: &str, class_name: &str) -> impl IntoView {
    let class_name = format!("badge {class_name}");
    view! { <span class=class_name>{value.to_string()}</span> }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnPageSnapshot {
    pub local_hash: String,
    pub vpn: VpnSnapshot,
}

#[server]
pub async fn load_vpn_snapshot() -> Result<VpnPageSnapshot, ServerFnError> {
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    Ok(VpnPageSnapshot {
        local_hash: state.vpn_hash.clone(),
        vpn: match &state.vpn {
            Some(vpn) => vpn.snapshot().await,
            None => VpnSnapshot::default(),
        },
    })
}

const VPN_WS_JS: &str = r#"
(function() {
    var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    var ws = new WebSocket(proto + '//' + location.host + '/api/ws/status');
    var pingState = Object.create(null);

    function shouldPauseLiveUpdates() {
        if (document.body.classList.contains('modal-open')) { return true; }
        var active = document.activeElement;
        if (active && (
            active.tagName === 'INPUT' ||
            active.tagName === 'TEXTAREA' ||
            active.tagName === 'SELECT' ||
            active.isContentEditable
        )) {
            return true;
        }
        var selection = window.getSelection ? window.getSelection() : null;
        return !!(selection && !selection.isCollapsed && String(selection).trim().length > 0);
    }

    function escapeHtml(value) {
        return String(value == null ? '' : value)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    function setText(id, text) {
        var el = document.getElementById(id);
        if (el) { el.textContent = text; }
    }

    function setBadge(id, text, className) {
        var el = document.getElementById(id);
        if (!el) { return; }
        el.textContent = text;
        el.className = 'badge ' + className;
    }

    function formatTime(ts) {
        if (!ts) { return '—'; }
        return new Date(ts * 1000).toLocaleTimeString();
    }

    function formatBytes(bytes) {
        bytes = Number(bytes) || 0;
        if (bytes >= 1024 * 1024) { return (bytes / (1024 * 1024)).toFixed(1) + ' MB'; }
        if (bytes >= 1024) { return (bytes / 1024).toFixed(1) + ' KB'; }
        return String(bytes) + ' B';
    }

    function formatHash(value) {
        var text = String(value == null ? '' : value).replace(/\s+/g, '');
        if (!text) { return '—'; }
        return text;
    }

    function vpnBadgeClass(value) {
        var text = String(value == null ? '' : value).trim().toLowerCase();
        if (text === 'running' || text === 'active' || text === 'ready' || text === 'installed' || text === 'yes') { return 'badge-ok'; }
        if (text === 'discovered' || text === 'configured' || text === 'pending' || text === 'starting') { return 'badge-warn'; }
        if (text === 'error' || text === 'closed' || text === 'failed' || text === 'no' || text === 'drop') { return 'badge-err'; }
        return 'reticulum-badge-soft';
    }

    function vpnBackendBadgeClass(value) {
        var text = String(value == null ? '' : value).trim().toLowerCase();
        if (text === 'linux') { return 'reticulum-badge-kind-data'; }
        if (text === 'mock') { return 'reticulum-badge-soft'; }
        return 'reticulum-badge-kind-link';
    }

    function badgeHtml(value, className) {
        return '<span class="badge ' + className + '">' + escapeHtml(value || '—') + '</span>';
    }

    function pingHtml(peer) {
        var key = String(peer.destination || '');
        var state = pingState[key] || {};
        var ip = String(peer.tunnel_ip || '');
        var busy = !!state.busy;
        var disabled = !ip || ip === '—' || busy;
        var statusClass = 'vpn-ping-status' + (state.kind ? ' ' + state.kind : '');
        return '<td class="vpn-ping-cell">'
            + '<button type="button" class="btn-secondary vpn-ping-btn" data-vpn-ping data-peer-key="' + escapeHtml(key) + '" data-peer-ip="' + escapeHtml(ip) + '"' + (disabled ? ' disabled' : '') + '>'
            + escapeHtml(busy ? 'Pinging…' : 'Ping')
            + '</button>'
            + '<div class="' + statusClass + '" data-vpn-ping-status="' + escapeHtml(key) + '">'
            + escapeHtml(state.text || '')
            + '</div>'
            + '</td>';
    }

    function renderLocalRoutes(routes) {
        var tbody = document.getElementById('vpn-local-routes');
        if (!tbody) { return; }
        if (!routes || routes.length === 0) {
            tbody.innerHTML = '<tr><td class="frames-empty">No local routes advertised yet</td></tr>';
            return;
        }
        tbody.innerHTML = routes.map(function(route) {
            return '<tr><td class="td-hex">' + escapeHtml(route) + '</td></tr>';
        }).join('');
    }

    function setAdvertisedRoutes(routes) {
        var input = document.getElementById('vpn-advertised-routes-input');
        if (!input || document.body.classList.contains('modal-open')) { return; }
        input.value = (routes || []).join('\n');
    }

    function setRouteEditorStatus(text, kind) {
        var status = document.getElementById('vpn-route-editor-status');
        if (!status) { return; }
        status.textContent = text;
        status.className = kind || '';
    }

    function openRouteEditor() {
        var modal = document.getElementById('vpn-routes-modal');
        var input = document.getElementById('vpn-advertised-routes-input');
        if (!modal || !input) { return; }
        setRouteEditorStatus('', '');
        modal.hidden = false;
        document.body.classList.add('modal-open');
        input.focus();
        input.select();
    }

    function closeRouteEditor() {
        var modal = document.getElementById('vpn-routes-modal');
        if (!modal) { return; }
        modal.hidden = true;
        document.body.classList.remove('modal-open');
    }

    function renderVpnPeers(peers) {
        var tbody = document.getElementById('vpn-peers');
        if (!tbody) { return; }
        if (!peers || peers.length === 0) {
            tbody.innerHTML = '<tr><td colspan="7" class="frames-empty">No VPN peers discovered yet</td></tr>';
            return;
        }
        tbody.innerHTML = peers.map(function(peer) {
            var routes = (peer.announced_routes || []).join(', ');
            return '<tr>'
                + '<td class="td-hex td-hash">' + escapeHtml(formatHash(peer.destination || '—')) + '</td>'
                + '<td class="td-hex">' + escapeHtml(peer.tunnel_ip || '—') + '</td>'
                + '<td>' + badgeHtml(peer.link_state || '—', vpnBadgeClass(peer.link_state || '—')) + '</td>'
                + '<td class="td-hex">' + escapeHtml(routes || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(formatTime(peer.last_seen_ts)) + '</td>'
                + '<td>' + badgeHtml(peer.last_error || '—', peer.last_error ? 'badge-err' : 'reticulum-badge-soft') + '</td>'
                + pingHtml(peer)
                + '</tr>';
        }).join('');
    }

    function renderVpnRoutes(routes) {
        var tbody = document.getElementById('vpn-routes');
        if (!tbody) { return; }
        if (!routes || routes.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="frames-empty">No VPN routes announced yet</td></tr>';
            return;
        }
        tbody.innerHTML = routes.map(function(route) {
            return '<tr>'
                + '<td class="td-hex">' + escapeHtml(route.network || '—') + '</td>'
                + '<td class="td-hex td-hash">' + escapeHtml(formatHash(route.owner || '—')) + '</td>'
                + '<td>' + badgeHtml(route.status || '—', vpnBadgeClass(route.status || '—')) + '</td>'
                + '<td class="td-time">' + escapeHtml(formatTime(route.last_seen_ts)) + '</td>'
                + '<td>' + badgeHtml(route.installed ? 'yes' : 'no', vpnBadgeClass(route.installed ? 'yes' : 'no')) + '</td>'
                + '</tr>';
        }).join('');
    }

    ws.onmessage = function(ev) {
        try {
            if (shouldPauseLiveUpdates()) { return; }
            var payload = JSON.parse(ev.data) || {};
            var vpn = payload.vpn || {};
            setBadge('vpn-status', vpn.status || '—', vpnBadgeClass(vpn.status || '—'));
            setBadge('vpn-backend', vpn.backend || '—', vpnBackendBadgeClass(vpn.backend || '—'));
            setText('vpn-interface', vpn.interface_name || '—');
            setText('vpn-network', vpn.network || '—');
            setText('vpn-local-ip', vpn.local_tunnel_ip || '—');
            setBadge('vpn-peer-policy', 'Auto-accept discovered peers', 'reticulum-badge-kind-announce');
            setBadge('vpn-last-error', vpn.last_error || '—', vpn.last_error ? 'badge-err' : 'reticulum-badge-soft');
            setText('vpn-peer-count', String((vpn.peers || []).length));
            setText('vpn-route-count', String((vpn.remote_routes || []).length));
            setText('vpn-tx-packets', String(vpn.tx_packets || 0));
            setText('vpn-tx-bytes', formatBytes(vpn.tx_bytes || 0));
            setText('vpn-rx-packets', String(vpn.rx_packets || 0));
            setText('vpn-rx-bytes', formatBytes(vpn.rx_bytes || 0));
            setText('vpn-drop-packets', String(vpn.drop_packets || 0));
            setText('vpn-last-tx', formatTime(vpn.last_tx_ts || 0));
            setText('vpn-last-rx', formatTime(vpn.last_rx_ts || 0));
            setAdvertisedRoutes(vpn.advertised_routes || []);
            renderLocalRoutes(vpn.local_routes || []);
            renderVpnPeers(vpn.peers || []);
            renderVpnRoutes(vpn.remote_routes || []);
        } catch (e) {}
    };

    document.addEventListener('click', function(ev) {
        var target = ev.target;
        if (!(target instanceof HTMLElement)) { return; }
        if (target.closest('[data-open-vpn-routes]')) {
            openRouteEditor();
            return;
        }
        var pingBtn = target.closest('[data-vpn-ping]');
        if (pingBtn) {
            var key = pingBtn.getAttribute('data-peer-key') || '';
            var ip = pingBtn.getAttribute('data-peer-ip') || '';
            if (!key || !ip || ip === '—') { return; }
            pingState[key] = { text: 'Pinging…', kind: 'pending', busy: true };
            pingBtn.disabled = true;
            pingBtn.textContent = 'Pinging…';
            var statusEl = document.querySelector('[data-vpn-ping-status="' + key + '"]');
            if (statusEl) {
                statusEl.textContent = 'Pinging…';
                statusEl.className = 'vpn-ping-status pending';
            }
            fetch('/api/vpn/ping', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ address: ip })
            }).then(async function(resp) {
                var text = await resp.text();
                var data = {};
                if (text) {
                    try {
                        data = JSON.parse(text);
                    } catch (_) {
                        data = { status: text };
                    }
                }
                if (!resp.ok) {
                    throw new Error((data && data.status) || text || 'Ping failed');
                }
                pingState[key] = {
                    text: (data && data.status) || ('Ping to ' + ip + ' succeeded'),
                    kind: (data && data.ok) ? 'ok' : 'err',
                    busy: false
                };
            }).catch(function(err) {
                pingState[key] = {
                    text: err && err.message ? err.message : 'Ping failed',
                    kind: 'err',
                    busy: false
                };
            }).finally(function() {
                var latestBtn = document.querySelector('[data-vpn-ping][data-peer-key="' + key + '"]');
                var latestStatus = document.querySelector('[data-vpn-ping-status="' + key + '"]');
                if (latestBtn) {
                    latestBtn.disabled = false;
                    latestBtn.textContent = 'Ping';
                }
                if (latestStatus) {
                    latestStatus.textContent = pingState[key].text || '';
                    latestStatus.className = 'vpn-ping-status' + (pingState[key].kind ? ' ' + pingState[key].kind : '');
                }
            });
            return;
        }
        if (target.closest('[data-close-vpn-routes]') || target.id === 'vpn-routes-modal') {
            closeRouteEditor();
        }
    });

    document.addEventListener('keydown', function(ev) {
        if (ev.key === 'Escape') { closeRouteEditor(); }
    });

    document.addEventListener('submit', function(ev) {
        var form = ev.target;
        if (!(form instanceof HTMLFormElement) || form.id !== 'vpn-routes-form') { return; }
        ev.preventDefault();
        var input = document.getElementById('vpn-advertised-routes-input');
        var submit = document.getElementById('vpn-routes-save');
        if (!(input instanceof HTMLTextAreaElement) || !(submit instanceof HTMLButtonElement)) { return; }
        var routes = input.value
            .split(/\r?\n/)
            .map(function(value) { return value.trim(); })
            .filter(Boolean);
        submit.disabled = true;
        setRouteEditorStatus('Saving…', '');
        fetch('/api/vpn/routes', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ routes: routes })
        }).then(async function(resp) {
            var text = await resp.text();
            var data = text ? JSON.parse(text) : {};
            if (!resp.ok) {
                throw new Error((data && data.status) || text || 'Failed to save VPN routes');
            }
            setRouteEditorStatus((data && data.status) || 'Saved', 'flash-ok');
            setTimeout(closeRouteEditor, 250);
        }).catch(function(err) {
            setRouteEditorStatus(err && err.message ? err.message : 'Failed to save VPN routes', 'flash-err');
        }).finally(function() {
            submit.disabled = false;
        });
    });
})();
"#;

#[component]
pub fn VpnPage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_vpn_snapshot());

    view! {
        <div class="page">
            <PageTitle icon="🔐" title="VPN" />
            <Suspense fallback=|| view! { <p class="loading">"Loading…"</p> }>
                {move || match snapshot.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(e)) => view! {
                        <div class="error-banner">"Error: "{e.to_string()}</div>
                    }.into_any(),
                    Some(Ok(snapshot)) => view! { <VpnContent snapshot=snapshot/> }.into_any(),
                }}
            </Suspense>
            <script>{VPN_WS_JS}</script>
        </div>
    }
}

#[component]
fn VpnContent(snapshot: VpnPageSnapshot) -> impl IntoView {
    let vpn_status = snapshot.vpn.status.clone();
    let vpn_backend = snapshot.vpn.backend.clone();
    let vpn_status_class = format!("badge {}", vpn_badge_class(&vpn_status));
    let vpn_backend_class = format!("badge {}", vpn_backend_badge_class(&vpn_backend));
    let vpn_interface = snapshot
        .vpn
        .interface_name
        .clone()
        .unwrap_or_else(|| "—".into());
    let vpn_network = snapshot.vpn.network.clone();
    let vpn_local_ip = snapshot
        .vpn
        .local_tunnel_ip
        .clone()
        .unwrap_or_else(|| "—".into());
    let vpn_error = snapshot
        .vpn
        .last_error
        .clone()
        .unwrap_or_else(|| "—".into());
    let vpn_error_class = format!("badge {}", vpn_error_badge_class(&vpn_error));
    let tx_packets = snapshot.vpn.tx_packets;
    let tx_bytes = format_bytes(snapshot.vpn.tx_bytes);
    let rx_packets = snapshot.vpn.rx_packets;
    let rx_bytes = format_bytes(snapshot.vpn.rx_bytes);
    let drop_packets = snapshot.vpn.drop_packets;
    let last_tx = if snapshot.vpn.last_tx_ts == 0 {
        "—".into()
    } else {
        format_timestamp(snapshot.vpn.last_tx_ts)
    };
    let last_rx = if snapshot.vpn.last_rx_ts == 0 {
        "—".into()
    } else {
        format_timestamp(snapshot.vpn.last_rx_ts)
    };
    let peer_count = snapshot.vpn.peers.len();
    let route_count = snapshot.vpn.remote_routes.len();

    view! {
        <div class="card reticulum-card vpn-help-card">
            <div class="card-header">
                <span class="card-title">"How to use"</span>
            </div>
            <p class="card-body-text">
                "Each discovered peer gets its own VPN tunnel IP from the "
                <strong>"Tunnel network"</strong>
                " above. Use that IP when you want to reach the peer device itself."
            </p>
            <p class="card-body-text">
                <strong>"Advertise routes"</strong>
                " is only for LANs behind this device, such as "
                <span class="td-hex">"192.168.10.0/24"</span>
                ". VPN peers will see those LANs as automatic translated aliases like "
                <span class="td-hex">"192.168.220.0/24 -> 192.168.10.0/24"</span>
                " so overlapping local subnets can still be reached."
            </p>
            <p class="card-body-text">
                "Typical flow: wait for a peer to appear as "
                <strong>"active"</strong>
                ", use its tunnel IP for direct peer traffic, and use the exported alias subnet shown below when you want to reach devices behind that peer."
            </p>
        </div>

        <div class="reticulum-summary">
            <div class="card stat-card">
                <span class="stat-label">"Local hash"</span>
                <span class="stat-value td-hex td-hash">{format_hash(&snapshot.local_hash)}</span>
                <span class="badge reticulum-badge-kind-announce" id="vpn-peer-policy">"Auto-accept discovered peers"</span>
            </div>
            <div class="card stat-card vpn-stat-card vpn-stat-card--status">
                <span class="stat-label">"VPN status"</span>
                <span class=vpn_status_class id="vpn-status">{vpn_status}</span>
                <span class=vpn_backend_class id="vpn-backend">{vpn_backend}</span>
                <span class="stat-label" id="vpn-interface">{vpn_interface}</span>
            </div>
            <div class="card stat-card vpn-stat-card vpn-stat-card--network">
                <span class="stat-label">"Tunnel network"</span>
                <span class="stat-value td-hex" id="vpn-network">{vpn_network}</span>
                <span class="stat-label" id="vpn-local-ip">{vpn_local_ip}</span>
            </div>
            <div class="card stat-card vpn-stat-card vpn-stat-card--peers">
                <span class="stat-label">"Discovered peers"</span>
                <span class="stat-value" id="vpn-peer-count">{peer_count}</span>
            </div>
            <div class="card stat-card vpn-stat-card vpn-stat-card--routes">
                <span class="stat-label">"Remote routes"</span>
                <span class="stat-value" id="vpn-route-count">{route_count}</span>
            </div>
            <div class="card stat-card vpn-stat-card vpn-stat-card--traffic">
                <span class="stat-label">"VPN traffic"</span>
                <span class="stat-label">
                    "TX "
                    <strong id="vpn-tx-packets">{tx_packets}</strong>
                    " · "
                    <span id="vpn-tx-bytes">{tx_bytes}</span>
                    " · "
                    <span id="vpn-last-tx">{last_tx}</span>
                </span>
                <span class="stat-label">
                    "RX "
                    <strong id="vpn-rx-packets">{rx_packets}</strong>
                    " · "
                    <span id="vpn-rx-bytes">{rx_bytes}</span>
                    " · "
                    <span id="vpn-last-rx">{last_rx}</span>
                </span>
                <span class="stat-label">
                    "Drops "
                    <strong id="vpn-drop-packets">{drop_packets}</strong>
                </span>
            </div>
            <div class="card stat-card vpn-stat-card vpn-stat-card--error">
                <span class="stat-label">"Last VPN error"</span>
                <span class=vpn_error_class id="vpn-last-error">{vpn_error}</span>
            </div>
        </div>

        <div class="reticulum-grid">
            <VpnLocalRoutesCard
                routes=snapshot.vpn.local_routes
                advertised_routes=snapshot.vpn.advertised_routes
            />
            <VpnPeersCard peers=snapshot.vpn.peers />
        </div>

        <VpnRoutesCard routes=snapshot.vpn.remote_routes />
    }
}

#[component]
fn VpnLocalRoutesCard(routes: Vec<String>, advertised_routes: Vec<String>) -> impl IntoView {
    let advertised_routes_text = advertised_routes.join("\n");
    view! {
        <div class="card reticulum-card">
            <div class="card-header">
                <span class="card-title">"Exported Routes"</span>
                <button type="button" class="btn-secondary" data-open-vpn-routes>
                    "Advertise routes"
                </button>
            </div>
            <p class="card-body-text">
                "Auto-detected and configured LAN subnets announced to VPN peers as translated aliases."
            </p>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Network"</th>
                        </tr>
                    </thead>
                    <tbody id="vpn-local-routes">
                        {if routes.is_empty() {
                            view! { <tr><td class="frames-empty">"No local routes advertised yet"</td></tr> }.into_any()
                        } else {
                            routes.into_iter().map(|route| {
                                view! {
                                    <tr>
                                        <td class="td-hex">{route}</td>
                                    </tr>
                                }
                            }).collect_view().into_any()
                        }}
                    </tbody>
                </table>
            </div>
            <div class="modal-backdrop" id="vpn-routes-modal" hidden>
                <div class="modal-card">
                    <div class="modal-header">
                        <h2 class="modal-title">"Advertise local subnets"</h2>
                        <button type="button" class="modal-close" data-close-vpn-routes>"×"</button>
                    </div>
                    <form class="modal-form" id="vpn-routes-form">
                        <p class="card-body-text">
                            "Enter one CIDR subnet per line. These routes are announced to peers even if they are not auto-detected."
                        </p>
                        <textarea
                            id="vpn-advertised-routes-input"
                            class="field-input radio-test-textarea"
                            placeholder="192.168.10.0/24"
                        >{advertised_routes_text}</textarea>
                        <div id="vpn-route-editor-status"></div>
                        <div class="modal-actions">
                            <button type="button" class="btn-secondary" data-close-vpn-routes>
                                "Cancel"
                            </button>
                            <button type="submit" id="vpn-routes-save" class="btn-primary">
                                "Save"
                            </button>
                        </div>
                    </form>
                </div>
            </div>
        </div>
    }
}

#[component]
fn VpnPeersCard(peers: Vec<VpnPeerSnapshot>) -> impl IntoView {
    view! {
        <div class="card reticulum-card">
            <div class="card-header">
                <span class="card-title">"VPN Peers"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Destination"</th>
                            <th>"Tunnel IP"</th>
                            <th>"Link"</th>
                            <th>"Routes"</th>
                            <th>"Last seen"</th>
                            <th>"Last error"</th>
                            <th>"Ping"</th>
                        </tr>
                    </thead>
                    <tbody id="vpn-peers">
                        {if peers.is_empty() {
                            view! { <tr><td colspan="7" class="frames-empty">"No VPN peers discovered yet"</td></tr> }.into_any()
                        } else {
                            peers.into_iter().map(|peer| {
                                let routes = if peer.announced_routes.is_empty() {
                                    "—".to_string()
                                } else {
                                    peer.announced_routes.join(", ")
                                };
                                let tunnel_ip = peer.tunnel_ip.clone().unwrap_or_else(|| "—".into());
                                let ping_ip = peer.tunnel_ip.clone().unwrap_or_default();
                                let ping_disabled = peer.tunnel_ip.is_none();
                                let last_error = peer.last_error.unwrap_or_else(|| "—".into());
                                let last_error_class = vpn_error_badge_class(&last_error);
                                view! {
                                    <tr>
                                        <td class="td-hex td-hash">{format_hash(&peer.destination)}</td>
                                        <td class="td-hex">{tunnel_ip}</td>
                                        <td>{render_vpn_badge(&peer.link_state, vpn_badge_class)}</td>
                                        <td class="td-hex">{routes}</td>
                                        <td class="td-time">{format_timestamp(peer.last_seen_ts)}</td>
                                        <td>{render_vpn_badge_class(&last_error, last_error_class)}</td>
                                        <td class="vpn-ping-cell">
                                            <button
                                                type="button"
                                                class="btn-secondary vpn-ping-btn"
                                                data-vpn-ping
                                                data-peer-key=peer.destination.clone()
                                                data-peer-ip=ping_ip
                                                disabled=ping_disabled
                                            >
                                                "Ping"
                                            </button>
                                            <div class="vpn-ping-status" data-vpn-ping-status=peer.destination.clone()></div>
                                        </td>
                                    </tr>
                                }
                            }).collect_view().into_any()
                        }}
                    </tbody>
                </table>
            </div>
        </div>
    }
}

#[component]
fn VpnRoutesCard(routes: Vec<VpnRouteSnapshot>) -> impl IntoView {
    view! {
        <div class="card reticulum-card reticulum-card--events">
            <div class="card-header">
                <span class="card-title">"VPN Routes"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Network"</th>
                            <th>"Owner"</th>
                            <th>"Status"</th>
                            <th>"Last seen"</th>
                            <th>"Installed"</th>
                        </tr>
                    </thead>
                    <tbody id="vpn-routes">
                        {if routes.is_empty() {
                            view! { <tr><td colspan="5" class="frames-empty">"No VPN routes announced yet"</td></tr> }.into_any()
                        } else {
                            routes.into_iter().map(|route| {
                                view! {
                                    <tr>
                                        <td class="td-hex">{route.network}</td>
                                        <td class="td-hex td-hash">{format_hash(&route.owner)}</td>
                                        <td>{render_vpn_badge(&route.status, vpn_badge_class)}</td>
                                        <td class="td-time">{format_timestamp(route.last_seen_ts)}</td>
                                        <td>{render_vpn_badge(if route.installed { "yes" } else { "no" }, vpn_badge_class)}</td>
                                    </tr>
                                }
                            }).collect_view().into_any()
                        }}
                    </tbody>
                </table>
            </div>
        </div>
    }
}
