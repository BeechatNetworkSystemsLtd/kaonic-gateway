use leptos::prelude::*;
use kaonic_vpn::{VpnPeerSnapshot, VpnRouteSnapshot, VpnSnapshot};
use serde::{Deserialize, Serialize};

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
    if !compact.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return compact;
    }
    compact
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
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

    function formatTime(ts) {
        if (!ts) { return '—'; }
        return new Date(ts * 1000).toLocaleTimeString();
    }

    function formatHash(value) {
        var text = String(value == null ? '' : value).replace(/\s+/g, '');
        if (!text) { return '—'; }
        if (!/^[0-9a-fA-F]+$/.test(text)) { return text; }
        return text.match(/.{1,4}/g).join(' ');
    }

    function renderLocalRoutes(routes) {
        var tbody = document.getElementById('vpn-local-routes');
        if (!tbody) { return; }
        if (!routes || routes.length === 0) {
            tbody.innerHTML = '<tr><td class="frames-empty">No local routes detected</td></tr>';
            return;
        }
        tbody.innerHTML = routes.map(function(route) {
            return '<tr><td class="td-hex">' + escapeHtml(route) + '</td></tr>';
        }).join('');
    }

    function renderVpnPeers(peers) {
        var tbody = document.getElementById('vpn-peers');
        if (!tbody) { return; }
        if (!peers || peers.length === 0) {
            tbody.innerHTML = '<tr><td colspan="6" class="frames-empty">No VPN peers discovered yet</td></tr>';
            return;
        }
        tbody.innerHTML = peers.map(function(peer) {
            var routes = (peer.announced_routes || []).join(', ');
            return '<tr>'
                + '<td class="td-hex td-hash">' + escapeHtml(formatHash(peer.destination || '—')) + '</td>'
                + '<td class="td-hex">' + escapeHtml(peer.tunnel_ip || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(peer.link_state || '—') + '</td>'
                + '<td class="td-hex">' + escapeHtml(routes || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(formatTime(peer.last_seen_ts)) + '</td>'
                + '<td class="td-hex">' + escapeHtml(peer.last_error || '—') + '</td>'
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
                + '<td class="td-time">' + escapeHtml(route.status || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(formatTime(route.last_seen_ts)) + '</td>'
                + '<td class="td-time">' + escapeHtml(route.installed ? 'yes' : 'no') + '</td>'
                + '</tr>';
        }).join('');
    }

    ws.onmessage = function(ev) {
        try {
            var payload = JSON.parse(ev.data) || {};
            var vpn = payload.vpn || {};
            setText('vpn-status', vpn.status || '—');
            setText('vpn-backend', vpn.backend || '—');
            setText('vpn-interface', vpn.interface_name || '—');
            setText('vpn-network', vpn.network || '—');
            setText('vpn-local-ip', vpn.local_tunnel_ip || '—');
            setText('vpn-peer-policy', 'Auto-accept discovered peers');
            setText('vpn-last-error', vpn.last_error || '—');
            setText('vpn-peer-count', String((vpn.peers || []).length));
            setText('vpn-route-count', String((vpn.remote_routes || []).length));
            renderLocalRoutes(vpn.local_routes || []);
            renderVpnPeers(vpn.peers || []);
            renderVpnRoutes(vpn.remote_routes || []);
        } catch (e) {}
    };
})();
"#;

#[component]
pub fn VpnPage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_vpn_snapshot());

    view! {
        <div class="page page--fill">
            <h1 class="page-title">"VPN"</h1>
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
    let peer_count = snapshot.vpn.peers.len();
    let route_count = snapshot.vpn.remote_routes.len();

    view! {
        <div class="reticulum-summary">
            <div class="card stat-card">
                <span class="stat-label">"Local hash"</span>
                <span class="stat-value td-hex td-hash">{format_hash(&snapshot.local_hash)}</span>
                <span class="stat-label" id="vpn-peer-policy">"Auto-accept discovered peers"</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"VPN status"</span>
                <span class="stat-value" id="vpn-status">{vpn_status}</span>
                <span class="stat-label" id="vpn-backend">{vpn_backend}</span>
                <span class="stat-label" id="vpn-interface">{vpn_interface}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"Tunnel network"</span>
                <span class="stat-value td-hex" id="vpn-network">{vpn_network}</span>
                <span class="stat-label" id="vpn-local-ip">{vpn_local_ip}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"Discovered peers"</span>
                <span class="stat-value" id="vpn-peer-count">{peer_count}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"Remote routes"</span>
                <span class="stat-value" id="vpn-route-count">{route_count}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"Last VPN error"</span>
                <span class="stat-label td-hex" id="vpn-last-error">{vpn_error}</span>
            </div>
        </div>

        <div class="reticulum-grid">
            <VpnLocalRoutesCard routes=snapshot.vpn.local_routes />
            <VpnPeersCard peers=snapshot.vpn.peers />
        </div>

        <VpnRoutesCard routes=snapshot.vpn.remote_routes />
    }
}

#[component]
fn VpnLocalRoutesCard(routes: Vec<String>) -> impl IntoView {
    view! {
        <div class="card reticulum-card">
            <div class="card-header">
                <span class="card-title">"Local Routes"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Network"</th>
                        </tr>
                    </thead>
                    <tbody id="vpn-local-routes">
                        {if routes.is_empty() {
                            view! { <tr><td class="frames-empty">"No local routes detected"</td></tr> }.into_any()
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
                        </tr>
                    </thead>
                    <tbody id="vpn-peers">
                        {if peers.is_empty() {
                            view! { <tr><td colspan="6" class="frames-empty">"No VPN peers discovered yet"</td></tr> }.into_any()
                        } else {
                            peers.into_iter().map(|peer| {
                                let routes = if peer.announced_routes.is_empty() {
                                    "—".to_string()
                                } else {
                                    peer.announced_routes.join(", ")
                                };
                                let last_error = peer.last_error.unwrap_or_else(|| "—".into());
                                view! {
                                    <tr>
                                        <td class="td-hex td-hash">{format_hash(&peer.destination)}</td>
                                        <td class="td-hex">{peer.tunnel_ip.unwrap_or_else(|| "—".into())}</td>
                                        <td class="td-time">{peer.link_state}</td>
                                        <td class="td-hex">{routes}</td>
                                        <td class="td-time">{format_timestamp(peer.last_seen_ts)}</td>
                                        <td class="td-hex">{last_error}</td>
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
                                        <td class="td-time">{route.status}</td>
                                        <td class="td-time">{format_timestamp(route.last_seen_ts)}</td>
                                        <td class="td-time">{if route.installed { "yes" } else { "no" }}</td>
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
