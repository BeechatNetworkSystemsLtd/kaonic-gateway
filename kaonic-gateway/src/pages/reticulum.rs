use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageTitle;
use crate::app_types::{ReticulumLinkDto, ReticulumSnapshotDto};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReticulumPageSnapshot {
    pub local_hash: String,
    pub local_destinations: Vec<LocalDestinationRow>,
    pub open_destinations: Vec<DestinationRow>,
    pub reticulum: ReticulumSnapshotDto,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalDestinationRow {
    pub name: String,
    pub destination: String,
    pub kind: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DestinationRow {
    pub destination: String,
    pub source: String,
    pub status: String,
}

#[server]
pub async fn load_reticulum_snapshot() -> Result<ReticulumPageSnapshot, ServerFnError> {
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    let reticulum = state.reticulum.snapshot().await;
    let vpn = match &state.vpn {
        Some(vpn) => vpn.snapshot().await,
        None => Default::default(),
    };

    Ok(ReticulumPageSnapshot {
        local_hash: state.vpn_hash.clone(),
        local_destinations: collect_local_destinations(&state, &vpn),
        open_destinations: collect_open_destinations(&reticulum),
        reticulum,
    })
}

const RETICULUM_WS_JS: &str = r#"
(function() {
    var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    var ws = new WebSocket(proto + '//' + location.host + '/api/ws/status');
    var liveState = { vpn: {}, reticulum: {} };

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

    function formatHash(value) {
        var text = String(value == null ? '' : value).replace(/\s+/g, '');
        if (!text) { return '—'; }
        return text;
    }

    function splitCommaValues(value) {
        return String(value == null ? '' : value)
            .split(',')
            .map(function(item) { return item.trim(); })
            .filter(Boolean);
    }

    function statusBadgeClass(value) {
        var text = String(value == null ? '' : value).toLowerCase();
        if (text === 'active' || text === 'ready' || text === 'running' || text === 'announce' || text === 'open') {
            return 'badge-ok';
        }
        if (text === 'starting' || text === 'discovered' || text === 'configured' || text === 'pending') {
            return 'badge-warn';
        }
        if (text === 'error' || text === 'failed' || text === 'closed' || text === 'down') {
            return 'badge-err';
        }
        return 'reticulum-badge-soft';
    }

    function sourceBadgeClass(value) {
        var text = String(value == null ? '' : value).toLowerCase();
        if (text === 'incoming link') { return 'reticulum-badge-source-in'; }
        if (text === 'outgoing link') { return 'reticulum-badge-source-out'; }
        if (text === 'announce') { return 'reticulum-badge-source-announce'; }
        return 'reticulum-badge-soft';
    }

    function destinationKindBadgeClass(value) {
        var text = String(value == null ? '' : value).toLowerCase();
        if (text === 'vpn') { return 'reticulum-badge-kind-vpn'; }
        return 'reticulum-badge-soft';
    }

    function renderBadgeList(items, classResolver) {
        if (!items || items.length === 0) {
            return '<span class="badge reticulum-badge-soft">—</span>';
        }
        return '<div class="reticulum-badge-list">' + items.map(function(item) {
            return '<span class="badge ' + classResolver(item) + '">' + escapeHtml(item) + '</span>';
        }).join('') + '</div>';
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
                + '<td class="td-hex td-hash">' + escapeHtml(formatHash(link.id || '—')) + '</td>'
                + '<td class="td-hex td-hash">' + escapeHtml(formatHash(link.destination || '—')) + '</td>'
                + '<td>' + renderBadgeList([link.status || '—'], statusBadgeClass) + '</td>'
                + '<td class="td-len">' + escapeHtml(link.rtt_ms != null ? String(link.rtt_ms) + " ms" : '—') + '</td>'
                + '<td class="td-len">' + escapeHtml(String(link.packets || 0)) + '</td>'
                + '<td class="td-len">' + escapeHtml(String(link.bytes || 0)) + ' B</td>'
                + '<td class="td-time">' + escapeHtml(link.last_event || '—') + '</td>'
                + '</tr>';
        }).join('');
    }

    function collectOpenDestinations(reticulum) {
        var map = {};

        function upsert(destination, source, status) {
            if (!destination) { return; }
            if (!map[destination]) {
                map[destination] = { destination: destination, sources: [], statuses: [] };
            }
            if (source && map[destination].sources.indexOf(source) === -1) {
                map[destination].sources.push(source);
            }
            if (status && map[destination].statuses.indexOf(status) === -1) {
                map[destination].statuses.push(status);
            }
        }

        (reticulum.incoming_links || []).forEach(function(link) {
            upsert(link.destination, 'incoming link', link.status);
        });
        (reticulum.outgoing_links || []).forEach(function(link) {
            upsert(link.destination, 'outgoing link', link.status);
        });
        return Object.values(map)
            .sort(function(a, b) { return a.destination.localeCompare(b.destination); })
            .map(function(entry) {
                return {
                    destination: entry.destination,
                    source: entry.sources.join(', ') || '—',
                    status: entry.statuses.join(', ') || '—'
                };
            });
    }

    function renderOpenDestinations(reticulum) {
        var tbody = document.getElementById('reticulum-open-destinations');
        if (!tbody) { return; }
        var rows = collectOpenDestinations(reticulum);
        if (!rows.length) {
            tbody.innerHTML = '<tr><td colspan="3" class="frames-empty">No open destinations observed yet</td></tr>';
            return;
        }
        tbody.innerHTML = rows.map(function(row) {
            return '<tr>'
                + '<td class="td-hex td-hash">' + escapeHtml(formatHash(row.destination)) + '</td>'
                + '<td>' + renderBadgeList(splitCommaValues(row.source), sourceBadgeClass) + '</td>'
                + '<td>' + renderBadgeList(splitCommaValues(row.status), statusBadgeClass) + '</td>'
                + '</tr>';
        }).join('');
    }

    function renderInterfaceStats(stats) {
        stats = stats || {};
        setText('reticulum-rx-errors', String(stats.rx_errors || 0));
        setText('reticulum-tx-errors', String(stats.tx_errors || 0));
    }

    function collectLocalDestinations(payload) {
        var rows = [];
        var vpn = payload.vpn || {};
        if (vpn.destination_hash) {
            rows.push({
                name: 'kaonic.vpn',
                destination: vpn.destination_hash,
                kind: 'VPN',
                status: vpn.status || 'ready'
            });
        }
        return rows.sort(function(a, b) { return a.name.localeCompare(b.name); });
    }

    function renderLocalDestinations(payload) {
        var tbody = document.getElementById('reticulum-local-destinations');
        if (!tbody) { return; }
        var rows = collectLocalDestinations(payload);
        if (!rows.length) {
            tbody.innerHTML = '<tr><td colspan="4" class="frames-empty">No local destinations registered yet</td></tr>';
            return;
        }
        tbody.innerHTML = rows.map(function(row) {
            return '<tr>'
                + '<td class="td-time">' + escapeHtml(row.name) + '</td>'
                + '<td class="td-hex td-hash">' + escapeHtml(formatHash(row.destination || '—')) + '</td>'
                + '<td>' + renderBadgeList([row.kind || '—'], destinationKindBadgeClass) + '</td>'
                + '<td>' + renderBadgeList([row.status || '—'], statusBadgeClass) + '</td>'
                + '</tr>';
        }).join('');
    }

    ws.onmessage = function(ev) {
        try {
            if (shouldPauseLiveUpdates()) { return; }
            var msg = JSON.parse(ev.data) || {};
            if (msg.type === 'vpn') {
                liveState.vpn = msg.data || {};
                setText('reticulum-local-destinations-count', String(collectLocalDestinations(liveState).length));
                renderLocalDestinations(liveState);
                return;
            }
            if (msg.type !== 'reticulum') { return; }
            var snapshot = msg.data || {};
            liveState.reticulum = snapshot;
            var incoming = snapshot.incoming_links || [];
            var outgoing = snapshot.outgoing_links || [];
            setText('reticulum-incoming-count', String(incoming.length));
            setText('reticulum-outgoing-count', String(outgoing.length));
            renderInterfaceStats(snapshot.interface_stats || {});
            renderLinks('reticulum-incoming-links', incoming, 'No incoming links seen');
            renderLinks('reticulum-outgoing-links', outgoing, 'No outgoing links seen');
            renderOpenDestinations(snapshot);
        } catch (e) {}
    };
})();
"#;

#[component]
pub fn ReticulumPage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_reticulum_snapshot());

    view! {
        <div class="page">
            <PageTitle icon="🛰️" title="Reticulum" />
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
    let local_destinations_count = snapshot.local_destinations.len();
    let incoming_count = snapshot.reticulum.incoming_links.len();
    let outgoing_count = snapshot.reticulum.outgoing_links.len();
    let rx_errors = snapshot.reticulum.interface_stats.rx_errors;
    let tx_errors = snapshot.reticulum.interface_stats.tx_errors;

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
                <span class="stat-label">"Local hash"</span>
                <span class="stat-value td-hex td-hash">{format_hash(&snapshot.local_hash)}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"My destinations"</span>
                <span class="stat-value" id="reticulum-local-destinations-count">{local_destinations_count}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"RX errors"</span>
                <span class="stat-value" id="reticulum-rx-errors">{rx_errors}</span>
            </div>
            <div class="card stat-card">
                <span class="stat-label">"TX errors"</span>
                <span class="stat-value" id="reticulum-tx-errors">{tx_errors}</span>
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
            <LocalDestinationsCard destinations=snapshot.local_destinations />
            <OpenDestinationsCard destinations=snapshot.open_destinations />
        </div>
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
                                            <td class="td-hex td-hash">{format_hash(&link.id)}</td>
                                            <td class="td-hex td-hash">{format_hash(&link.destination)}</td>
                                            <td>{render_badge_list(&[link.status], status_badge_class)}</td>
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
fn LocalDestinationsCard(destinations: Vec<LocalDestinationRow>) -> impl IntoView {
    view! {
        <div class="card reticulum-card">
            <div class="card-header">
                <span class="card-title">"My Destinations"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Name"</th>
                            <th>"Destination"</th>
                            <th>"Type"</th>
                            <th>"State"</th>
                        </tr>
                    </thead>
                    <tbody id="reticulum-local-destinations">
                        {if destinations.is_empty() {
                            view! { <tr><td colspan="4" class="frames-empty">"No local destinations registered yet"</td></tr> }.into_any()
                        } else {
                            destinations.into_iter().map(|destination| {
                                view! {
                                    <tr>
                                        <td class="td-time">{destination.name}</td>
                                        <td class="td-hex td-hash">{format_hash(&destination.destination)}</td>
                                        <td>{render_badge_list(&[destination.kind], destination_kind_badge_class)}</td>
                                        <td>{render_badge_list(&[destination.status], status_badge_class)}</td>
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
fn OpenDestinationsCard(destinations: Vec<DestinationRow>) -> impl IntoView {
    view! {
        <div class="card reticulum-card">
            <div class="card-header">
                <span class="card-title">"Open Destinations"</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Destination"</th>
                            <th>"Seen via"</th>
                            <th>"State"</th>
                        </tr>
                    </thead>
                    <tbody id="reticulum-open-destinations">
                        {if destinations.is_empty() {
                            view! { <tr><td colspan="3" class="frames-empty">"No open destinations observed yet"</td></tr> }.into_any()
                        } else {
                            destinations.into_iter().map(|destination| {
                                view! {
                                    <tr>
                                        <td class="td-hex td-hash">{format_hash(&destination.destination)}</td>
                                        <td>{render_badge_list(&split_badge_values(&destination.source), source_badge_class)}</td>
                                        <td>{render_badge_list(&split_badge_values(&destination.status), status_badge_class)}</td>
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

fn format_hash(value: &str) -> String {
    let compact = value.split_whitespace().collect::<String>();
    if compact.is_empty() {
        return "—".into();
    }
    compact
}

fn split_badge_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn status_badge_class(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "active" | "ready" | "running" | "announce" | "open" => "badge-ok",
        "starting" | "discovered" | "configured" | "pending" => "badge-warn",
        "error" | "failed" | "closed" | "down" => "badge-err",
        _ => "reticulum-badge-soft",
    }
}

fn source_badge_class(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "incoming link" => "reticulum-badge-source-in",
        "outgoing link" => "reticulum-badge-source-out",
        "announce" => "reticulum-badge-source-announce",
        _ => "reticulum-badge-soft",
    }
}

fn destination_kind_badge_class(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "vpn" => "reticulum-badge-kind-vpn",
        _ => "reticulum-badge-soft",
    }
}

fn render_badge_list(values: &[String], class_fn: fn(&str) -> &'static str) -> impl IntoView {
    if values.is_empty() {
        return view! { <span class="badge reticulum-badge-soft">"—"</span> }.into_any();
    }

    view! {
        <div class="reticulum-badge-list">
            {values.iter().map(|value| {
                let class_name = format!("badge {}", class_fn(value));
                let value = value.clone();
                view! {
                    <span class=class_name>{value}</span>
                }
            }).collect_view()}
        </div>
    }
    .into_any()
}

fn collect_local_destinations(
    _state: &crate::state::AppState,
    vpn: &kaonic_vpn::VpnSnapshot,
) -> Vec<LocalDestinationRow> {
    let mut rows = Vec::new();

    if !vpn.destination_hash.is_empty() {
        rows.push(LocalDestinationRow {
            name: "kaonic.vpn".into(),
            destination: vpn.destination_hash.clone(),
            kind: "VPN".into(),
            status: vpn.status.clone(),
        });
    }

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

fn collect_open_destinations(reticulum: &ReticulumSnapshotDto) -> Vec<DestinationRow> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Entry {
        sources: Vec<String>,
        statuses: Vec<String>,
    }

    fn push_unique(items: &mut Vec<String>, value: &str) {
        if !value.is_empty() && !items.iter().any(|item| item == value) {
            items.push(value.to_string());
        }
    }

    let mut entries = BTreeMap::<String, Entry>::new();

    for link in &reticulum.incoming_links {
        let entry = entries.entry(link.destination.clone()).or_default();
        push_unique(&mut entry.sources, "incoming link");
        push_unique(&mut entry.statuses, &link.status);
    }
    for link in &reticulum.outgoing_links {
        let entry = entries.entry(link.destination.clone()).or_default();
        push_unique(&mut entry.sources, "outgoing link");
        push_unique(&mut entry.statuses, &link.status);
    }
    entries
        .into_iter()
        .map(|(destination, entry)| DestinationRow {
            destination,
            source: if entry.sources.is_empty() {
                "—".into()
            } else {
                entry.sources.join(", ")
            },
            status: if entry.statuses.is_empty() {
                "—".into()
            } else {
                entry.statuses.join(", ")
            },
        })
        .collect()
}
