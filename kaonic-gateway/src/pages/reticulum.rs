use leptos::prelude::*;
use kaonic_vpn::{VpnPeerSnapshot, VpnRouteSnapshot, VpnSnapshot};
use serde::{Deserialize, Serialize};

use crate::app_types::{ReticulumEventDto, ReticulumLinkDto, ReticulumSnapshotDto};

fn format_timestamp(ts: u64) -> String {
    let seconds = ts % 86_400;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    format!("{hours:02}:{minutes:02}:{secs:02} UTC")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReticulumPageSnapshot {
    pub local_hash: String,
    pub configured_peers: Vec<String>,
    pub reticulum: ReticulumSnapshotDto,
    pub vpn: VpnSnapshot,
}

#[server]
pub async fn load_reticulum_snapshot() -> Result<ReticulumPageSnapshot, ServerFnError> {
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    let configured_peers = state
        .settings
        .lock()
        .ok()
        .and_then(|settings| settings.load_config().ok())
        .map(|config| config.peers)
        .unwrap_or_default();

    Ok(ReticulumPageSnapshot {
        local_hash: state.vpn_hash.clone(),
        configured_peers,
        reticulum: state.reticulum.snapshot().await,
        vpn: match &state.vpn {
            Some(vpn) => vpn.snapshot().await,
            None => VpnSnapshot::default(),
        },
    })
}

const RETICULUM_WS_JS: &str = r#"
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

    function renderLinks(id, links, emptyText) {
        var tbody = document.getElementById(id);
        if (!tbody) { return; }
        if (!links || links.length === 0) {
            tbody.innerHTML = '<tr><td colspan="7" class="frames-empty">' + escapeHtml(emptyText) + '</td></tr>';
            return;
        }
        tbody.innerHTML = links.map(function(link) {
            return '<tr>'
                + '<td class="td-hex">' + escapeHtml(link.id || '—') + '</td>'
                + '<td class="td-hex">' + escapeHtml(link.destination || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(link.status || '—') + '</td>'
                + '<td class="td-len">' + escapeHtml(link.rtt_ms != null ? String(link.rtt_ms) + " ms" : '—') + '</td>'
                + '<td class="td-len">' + escapeHtml(String(link.packets || 0)) + '</td>'
                + '<td class="td-len">' + escapeHtml(String(link.bytes || 0)) + ' B</td>'
                + '<td class="td-time">' + escapeHtml(link.last_event || '—') + '</td>'
                + '</tr>';
        }).join('');
    }

    function renderEvents(events) {
        var tbody = document.getElementById('reticulum-events');
        if (!tbody) { return; }
        if (!events || events.length === 0) {
            tbody.innerHTML = '<tr><td colspan="6" class="frames-empty">No Reticulum events yet</td></tr>';
            return;
        }
        tbody.innerHTML = events.map(function(event) {
            return '<tr>'
                + '<td class="td-time">' + escapeHtml(formatTime(event.ts)) + '</td>'
                + '<td class="td-time">' + escapeHtml(event.direction || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(event.kind || '—') + '</td>'
                + '<td class="td-hex">' + escapeHtml(event.link_id || '—') + '</td>'
                + '<td class="td-hex">' + escapeHtml(event.destination || '—') + '</td>'
                + '<td class="td-hex">' + escapeHtml(event.details || '—') + '</td>'
                + '</tr>';
        }).join('');
    }

    function renderVpnPeers(peers) {
        var tbody = document.getElementById('vpn-peers');
        if (!tbody) { return; }
        if (!peers || peers.length === 0) {
            tbody.innerHTML = '<tr><td colspan="4" class="frames-empty">No VPN peers configured</td></tr>';
            return;
        }
        tbody.innerHTML = peers.map(function(peer) {
            var routes = (peer.announced_routes || []).join(', ');
            return '<tr>'
                + '<td class="td-hex">' + escapeHtml(peer.destination || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(peer.link_state || '—') + '</td>'
                + '<td class="td-hex">' + escapeHtml(routes || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(formatTime(peer.last_seen_ts)) + '</td>'
                + '</tr>';
        }).join('');
    }

    function renderVpnRoutes(routes) {
        var tbody = document.getElementById('vpn-routes');
        if (!tbody) { return; }
        if (!routes || routes.length === 0) {
            tbody.innerHTML = '<tr><td colspan="4" class="frames-empty">No VPN routes announced yet</td></tr>';
            return;
        }
        tbody.innerHTML = routes.map(function(route) {
            return '<tr>'
                + '<td class="td-hex">' + escapeHtml(route.network || '—') + '</td>'
                + '<td class="td-hex">' + escapeHtml(route.owner || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(route.status || '—') + '</td>'
                + '<td class="td-time">' + escapeHtml(route.installed ? 'yes' : 'no') + '</td>'
                + '</tr>';
        }).join('');
    }

    ws.onmessage = function(ev) {
        try {
            var payload = JSON.parse(ev.data) || {};
            var snapshot = payload.reticulum || {};
            var vpn = payload.vpn || {};
            var incoming = snapshot.incoming_links || [];
            var outgoing = snapshot.outgoing_links || [];
            var events = snapshot.events || [];
            setText('reticulum-incoming-count', String(incoming.length));
            setText('reticulum-outgoing-count', String(outgoing.length));
            setText('reticulum-events-count', String(events.length));
            setText('vpn-status', vpn.status || '—');
            setText('vpn-backend', vpn.backend || '—');
            setText('vpn-interface', vpn.interface_name || '—');
            renderLinks('reticulum-incoming-links', incoming, 'No incoming links seen');
            renderLinks('reticulum-outgoing-links', outgoing, 'No outgoing links seen');
            renderEvents(events);
            renderVpnPeers(vpn.peers || []);
            renderVpnRoutes(vpn.remote_routes || []);
        } catch (e) {}
    };
})();
"#;

#[component]
pub fn ReticulumPage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_reticulum_snapshot());

    view! {
        <div class="page page--fill">
            <h1 class="page-title">"Reticulum"</h1>
            <Suspense fallback=|| view! { <p class="loading">"Loading…"</p> }>
                {move || match snapshot.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(e)) => view! {
                        <div class="error-banner">"Error: "{e.to_string()}</div>
                    }.into_any(),
                    Some(Ok(snapshot)) => view! { <ReticulumContent snapshot=snapshot/> }.into_any(),
                }}
            </Suspense>
            <script>{RETICULUM_WS_JS}</script>
        </div>
    }
}

#[component]
fn ReticulumContent(snapshot: ReticulumPageSnapshot) -> impl IntoView {
    let incoming_count = snapshot.reticulum.incoming_links.len();
    let outgoing_count = snapshot.reticulum.outgoing_links.len();
    let events_count = snapshot.reticulum.events.len();
    let vpn_status = snapshot.vpn.status.clone();
    let vpn_backend = snapshot.vpn.backend.clone();
    let configured_peer_count = snapshot.configured_peers.len();
    let vpn_interface = snapshot
        .vpn
        .interface_name
        .clone()
        .unwrap_or_else(|| "—".into());

    view! {
        <div class="reticulum-summary">
            <div class="card stat-card">
                <span class="stat-label">"Incoming links"</span>
                <span class="stat-value" id="reticulum-incoming-count">{incoming_count}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"Outgoing links"</span>
                <span class="stat-value" id="reticulum-outgoing-count">{outgoing_count}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"Recent events"</span>
                <span class="stat-value" id="reticulum-events-count">{events_count}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"Local hash"</span>
                <span class="stat-value td-hex">{snapshot.local_hash}</span>
                <span class="stat-label">{format!("{configured_peer_count} configured peers")}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"VPN status"</span>
                <span class="stat-value" id="vpn-status">{vpn_status}</span>
                <span class="stat-label" id="vpn-backend">{vpn_backend}</span>
                <span class="stat-label" id="vpn-interface">{vpn_interface}</span>
            </div>
        </div>

        <div class="reticulum-grid">
            <ReticulumLinksCard
                title="Incoming Links"
                table_id="reticulum-incoming-links"
                empty_text="No incoming links seen"
                links=snapshot.reticulum.incoming_links
            />
            <ReticulumLinksCard
                title="Outgoing Links"
                table_id="reticulum-outgoing-links"
                empty_text="No outgoing links seen"
                links=snapshot.reticulum.outgoing_links
            />
        </div>

        <div class="reticulum-grid">
            <ConfiguredPeersCard peers=snapshot.configured_peers />
            <VpnPeersCard peers=snapshot.vpn.peers />
            <VpnRoutesCard routes=snapshot.vpn.remote_routes />
        </div>

        <ReticulumEventsCard events=snapshot.reticulum.events />
    }
}

#[component]
fn ReticulumLinksCard(
    title: &'static str,
    table_id: &'static str,
    empty_text: &'static str,
    links: Vec<ReticulumLinkDto>,
) -> impl IntoView {
    view! {
        <div class="card reticulum-card">
            <div class="card-header">
                <span class="card-title">{title}</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Link ID"</th>
                            <th>"Destination"</th>
                            <th>"Status"</th>
                            <th>"RTT"</th>
                            <th>"Packets"</th>
                            <th>"Bytes"</th>
                            <th>"Last event"</th>
                        </tr>
                    </thead>
                    <tbody id=table_id>
                        {if links.is_empty() {
                            view! {
                                <tr>
                                    <td colspan="7" class="frames-empty">{empty_text}</td>
                                </tr>
                            }.into_any()
                        } else {
                            links
                                .into_iter()
                                .map(|link| {
                                    let rtt = link
                                        .rtt_ms
                                        .map(|value| format!("{value} ms"))
                                        .unwrap_or_else(|| "—".into());
                                    view! {
                                        <tr>
                                            <td class="td-hex">{link.id}</td>
                                            <td class="td-hex">{link.destination}</td>
                                            <td class="td-time">{link.status}</td>
                                            <td class="td-len">{rtt}</td>
                                            <td class="td-len">{link.packets}</td>
                                            <td class="td-len">{format!("{} B", link.bytes)}</td>
                                            <td class="td-time">{link.last_event}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                                .into_any()
                        }}
                    </tbody>
                </table>
            </div>
        </div>
    }
}

#[component]
fn ReticulumEventsCard(events: Vec<ReticulumEventDto>) -> impl IntoView {
    view! {
        <div class="card reticulum-card reticulum-card--events">
            <div class="card-header">
                <span class="card-title">"Recent Events"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Time"</th>
                            <th>"Source"</th>
                            <th>"Kind"</th>
                            <th>"Link"</th>
                            <th>"Destination"</th>
                            <th>"Details"</th>
                        </tr>
                    </thead>
                    <tbody id="reticulum-events">
                        {if events.is_empty() {
                            view! {
                                <tr>
                                    <td colspan="6" class="frames-empty">"No Reticulum events yet"</td>
                                </tr>
                            }.into_any()
                        } else {
                            events
                                .into_iter()
                                .map(|event| {
                                    let ts = format_timestamp(event.ts);
                                    view! {
                                        <tr>
                                            <td class="td-time">{ts}</td>
                                            <td class="td-time">{event.direction}</td>
                                            <td class="td-time">{event.kind}</td>
                                            <td class="td-hex">
                                                {if event.link_id.is_empty() { "—".into() } else { event.link_id }}
                                            </td>
                                            <td class="td-hex">{event.destination}</td>
                                            <td class="td-hex">{event.details}</td>
                                        </tr>
                                    }
                                })
                                .collect_view()
                                .into_any()
                        }}
                    </tbody>
                </table>
            </div>
        </div>
    }
}

#[component]
fn ConfiguredPeersCard(peers: Vec<String>) -> impl IntoView {
    view! {
        <div class="card reticulum-card">
            <div class="card-header">
                <span class="card-title">"Configured Peers"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Destination"</th>
                        </tr>
                    </thead>
                    <tbody>
                        {if peers.is_empty() {
                            view! { <tr><td class="frames-empty">"No peers configured yet"</td></tr> }.into_any()
                        } else {
                            peers.into_iter().map(|peer| {
                                view! {
                                    <tr>
                                        <td class="td-hex">{peer}</td>
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
                            <th>"Link"</th>
                            <th>"Routes"</th>
                            <th>"Last seen"</th>
                        </tr>
                    </thead>
                    <tbody id="vpn-peers">
                        {if peers.is_empty() {
                            view! { <tr><td colspan="4" class="frames-empty">"No VPN peers configured"</td></tr> }.into_any()
                        } else {
                            peers.into_iter().map(|peer| {
                                let routes = if peer.announced_routes.is_empty() {
                                    "—".to_string()
                                } else {
                                    peer.announced_routes.join(", ")
                                };
                                view! {
                                    <tr>
                                        <td class="td-hex">{peer.destination}</td>
                                        <td class="td-time">{peer.link_state}</td>
                                        <td class="td-hex">{routes}</td>
                                        <td class="td-time">{format_timestamp(peer.last_seen_ts)}</td>
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
        <div class="card reticulum-card">
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
                            <th>"Installed"</th>
                        </tr>
                    </thead>
                    <tbody id="vpn-routes">
                        {if routes.is_empty() {
                            view! { <tr><td colspan="4" class="frames-empty">"No VPN routes announced yet"</td></tr> }.into_any()
                        } else {
                            routes.into_iter().map(|route| {
                                view! {
                                    <tr>
                                        <td class="td-hex">{route.network}</td>
                                        <td class="td-hex">{route.owner}</td>
                                        <td class="td-time">{route.status}</td>
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
