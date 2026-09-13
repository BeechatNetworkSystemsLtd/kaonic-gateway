//! The VPN page.
//!
//! Two questions an operator actually has: *am I reaching another site through
//! someone?* and *is anyone reaching a site through me?* The page answers those
//! two and shows the traffic, and does not try to be a routing table viewer —
//! pairing lives on the Remote page, and the kernel's own tables are a better
//! debugger than a rendering of them.

use kaonic_vpn::VpnSnapshot;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageTitle;

// ── Snapshot ──────────────────────────────────────────────────────────────────

/// A node the operator paired with, as the router picker needs it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PairedPeerDto {
    pub identity_hash: String,
    pub codename: String,
    /// The operator's own label, when they set one.
    pub tag: String,
    pub online: bool,
    /// Networks this peer is currently offering.
    pub routes: Vec<String>,
}

impl PairedPeerDto {
    fn display_name(&self) -> String {
        if !self.tag.is_empty() {
            return self.tag.clone();
        }
        if !self.codename.is_empty() {
            return self.codename.clone();
        }
        self.identity_hash.chars().take(8).collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnGatewayDto {
    pub enabled: bool,
    pub active: bool,
    pub routes: Vec<String>,
    pub egress_interface: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnUplinkDto {
    pub enabled: bool,
    pub active: bool,
    pub peer: Option<String>,
    pub routes: Vec<String>,
    pub default_route: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnPageSnapshot {
    pub local_hash: String,
    pub codename: String,
    pub wlan0_ip: Option<String>,
    pub usb0_ip: Option<String>,
    pub paired: Vec<PairedPeerDto>,
    pub gateway: VpnGatewayDto,
    pub uplink: VpnUplinkDto,
    pub vpn: VpnSnapshot,
}

#[server]
pub async fn load_vpn_snapshot() -> Result<VpnPageSnapshot, ServerFnError> {
    use crate::network::read_interface_ipv4;
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;
    let codename = {
        let settings = state
            .settings
            .lock()
            .map_err(|_| ServerFnError::new("settings lock poisoned"))?;
        settings
            .load_or_create_codename()
            .map_err(|err| ServerFnError::new(err.to_string()))?
    };
    let paired_nodes = {
        let settings = state
            .settings
            .lock()
            .map_err(|_| ServerFnError::new("settings lock poisoned"))?;
        settings.load_remote_paired().unwrap_or_default()
    };
    let tags = {
        let settings = state
            .settings
            .lock()
            .map_err(|_| ServerFnError::new("settings lock poisoned"))?;
        settings.load_remote_tags().unwrap_or_default()
    };

    let mut vpn = match &state.vpn {
        Some(vpn) => vpn.snapshot().await,
        None => VpnSnapshot::default(),
    };
    // Same translation the websocket does: the page speaks identity hashes.
    crate::state::present_uplink_peer(&state, &mut vpn);
    crate::state::present_peer_names(&state, &mut vpn);

    // Only paired nodes can join the VPN, so the picker is exactly the pairing
    // list — joined to what each peer is offering right now.
    let paired = paired_nodes
        .into_iter()
        .map(|node| {
            // A node's VPN destination is derived from its identity, not equal
            // to it, so the two lists join on the derived hash.
            let vpn_hash = crate::remote::vpn_destination_hash(&node)
                .map(|hash| hash.to_hex_string())
                .unwrap_or_default();
            let peer = vpn
                .peers
                .iter()
                .find(|peer| peer.destination == vpn_hash);
            PairedPeerDto {
                tag: tags
                    .iter()
                    .find(|(hash, _)| *hash == node.identity_hash)
                    .map(|(_, tag)| tag.clone())
                    .unwrap_or_default(),
                online: peer.is_some_and(|peer| peer.link_state == "active"),
                routes: vpn
                    .remote_routes
                    .iter()
                    .filter(|route| route.owner == vpn_hash)
                    .map(|route| route.network.clone())
                    .collect(),
                identity_hash: node.identity_hash,
                codename: node.codename,
            }
        })
        .collect();

    Ok(VpnPageSnapshot {
        local_hash: state.vpn_hash.clone(),
        codename,
        wlan0_ip: read_interface_ipv4("wlan0"),
        usb0_ip: read_interface_ipv4("usb0"),
        paired,
        gateway: VpnGatewayDto {
            enabled: vpn.gateway.enabled,
            active: vpn.gateway.active,
            routes: vpn.gateway.routes.clone(),
            egress_interface: vpn.gateway.egress_interface.clone(),
            detail: vpn.gateway.detail.clone(),
        },
        uplink: VpnUplinkDto {
            enabled: vpn.uplink.enabled,
            active: vpn.uplink.active,
            peer: vpn.uplink.peer.clone(),
            routes: vpn.uplink.routes.clone(),
            default_route: vpn.uplink.default_route,
            detail: vpn.uplink.detail.clone(),
        },
        vpn,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn truncate_hash(hash: &str) -> String {
    let compact: String = hash.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() > 16 {
        format!("{}…", &compact[..16])
    } else {
        compact
    }
}

fn format_relative_time(ts: u64) -> String {
    if ts == 0 {
        return "never".into();
    }
    // SSR: we don't know current server time in a useful SSR context;
    // JS will overwrite with live relative times on each WS tick.
    let seconds = ts % 86_400;
    let h = seconds / 3_600;
    let m = (seconds % 3_600) / 60;
    let s = seconds % 60;
    format!("{h:02}:{m:02}:{s:02} UTC")
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

fn format_bps(bps: u64) -> String {
    if bps >= 1_000_000 {
        format!("{:.1} Mbps", bps as f64 / 1_000_000.0)
    } else if bps >= 1_000 {
        format!("{:.1} Kbps", bps as f64 / 1_000.0)
    } else {
        format!("{bps} bps")
    }
}

fn status_dot_class(status: &str) -> &'static str {
    match status.trim().to_ascii_lowercase().as_str() {
        "running" => "status-dot status-dot--ok",
        "error" => "status-dot status-dot--err",
        "mock" => "status-dot status-dot--idle",
        _ => "status-dot status-dot--warn",
    }
}

fn banner_modifier(status: &str) -> &'static str {
    match status.trim().to_ascii_lowercase().as_str() {
        "running" => "vpn-banner vpn-banner--ok",
        "error" => "vpn-banner vpn-banner--err",
        _ => "vpn-banner vpn-banner--idle",
    }
}

fn parse_route_display(route: &str) -> (String, Option<String>) {
    if let Some(idx) = route.find(" -> ") {
        (
            route[..idx].trim().to_string(),
            Some(route[idx + 4..].trim().to_string()),
        )
    } else {
        (route.trim().to_string(), None)
    }
}

fn default_advertised_route_strings(routes: Vec<String>) -> Vec<String> {
    if routes.is_empty() {
        vec!["192.168.10.0/24".into()]
    } else {
        routes
    }
}


// ── Page ──────────────────────────────────────────────────────────────────────

#[component]
pub fn VpnPage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_vpn_snapshot());
    view! {
        // Every page in the app sits in this container; it is what supplies the
        // outer padding, so content must not be emitted outside it.
        <div class="page">
            <PageTitle title="VPN" icon="🔒"/>
            <Suspense fallback=|| view! { <div class="card">"Loading…"</div> }>
                {move || snapshot.get().map(|result| match result {
                    Ok(snapshot) => view! { <VpnContent snapshot=snapshot/> }.into_any(),
                    Err(err) => view! {
                        <div class="card">"Failed to load VPN status: " {err.to_string()}</div>
                    }.into_any(),
                })}
            </Suspense>
        </div>
    }
}

#[component]
fn VpnContent(snapshot: VpnPageSnapshot) -> impl IntoView {
    let vpn = &snapshot.vpn;
    let status = vpn.status.clone();
    let tunnel_ip = vpn.local_tunnel_ip.clone().unwrap_or_else(|| "—".into());
    let network = vpn.network.clone();
    let peer_count = vpn.peers.len();
    let has_error = vpn.last_error.is_some();
    let last_error = vpn.last_error.clone().unwrap_or_default();
    let paired_json = serde_json::to_string(&snapshot.paired).unwrap_or_else(|_| "[]".into());
    let peers_json = serde_json::to_string(&snapshot.vpn.peers).unwrap_or_else(|_| "[]".into());
    let gateway_json = serde_json::to_string(&snapshot.gateway).unwrap_or_else(|_| "{}".into());
    let uplink_json = serde_json::to_string(&snapshot.uplink).unwrap_or_else(|_| "{}".into());

    view! {
        // ── Status banner ────────────────────────────────────────────────────
        <div class=banner_modifier(&status) id="vpn-banner">
            <div class="vpn-banner-lead">
                <span class=status_dot_class(&status) id="vpn-status-dot"></span>
                <span class="vpn-banner-status-text" id="vpn-status-text">{status.clone()}</span>
            </div>
            <div class="vpn-banner-divider"></div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Tunnel IP"</span>
                <span class="vpn-banner-ip" id="vpn-local-ip">{tunnel_ip}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Network"</span>
                <span class="vpn-banner-ip vpn-banner-net" id="vpn-network">{network}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Peers"</span>
                <span class="vpn-banner-ip" id="vpn-peer-count">{peer_count}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Router"</span>
                <span class="vpn-banner-ip" id="vpn-banner-router">"—"</span>
            </div>
            <div class="vpn-banner-spacer"></div>
            <div class="vpn-banner-field feature-switches">
                <span class="vpn-banner-label">"VPN"</span>
                <div class="feature-switch-row">
                    <label class="feature-switch">
                        <input type="checkbox" id="feature-vpn"/>
                        <span class="feature-switch-track"><span class="feature-switch-thumb"></span></span>
                        <span class="feature-switch-label" id="feature-vpn-label">"Running"</span>
                    </label>
                </div>
            </div>
        </div>

        {if has_error {
            view! {
                <div class="vpn-error-bar" id="vpn-error-bar">
                    "⚠ " <span id="vpn-error-msg">{last_error}</span>
                </div>
            }.into_any()
        } else {
            view! { <div id="vpn-error-bar" style="display:none"></div> }.into_any()
        }}

        <div class="vpn-stack">
            <div class="vpn-top-row">
                <VpnTrafficCard vpn=snapshot.vpn.clone() />
                <VpnDeviceCard
                    local_hash=snapshot.local_hash.clone()
                    local_routes=vpn.local_routes.clone()
                    interface_name=vpn.interface_name.clone()
                    backend=vpn.backend.clone()
                    tunnel_ip=vpn.local_tunnel_ip.clone()
                    network=vpn.network.clone()
                    codename=snapshot.codename.clone()
                    wlan0_ip=snapshot.wlan0_ip.clone()
                    usb0_ip=snapshot.usb0_ip.clone()
                />
            </div>

            <VpnPeersCard peers=snapshot.vpn.peers.clone() />
        </div>

        <VpnRouteEditorModal
            advertised_routes=vpn.advertised_routes.clone()
        />

        <script inner_html=format!(
            "window.__vpn = {{ paired: {paired_json}, peers: {peers_json}, gateway: {gateway_json}, uplink: {uplink_json} }};"
        )></script>
        <script>{VPN_JS}</script>
    }
}

// ── Traffic ───────────────────────────────────────────────────────────────────

/// In and out over the tunnel, on the same chart the dashboard uses for the
/// radios — the one place on this page where a graph beats a number, because
/// what matters is whether traffic is flowing at all and in which direction.
#[component]
fn VpnTrafficCard(vpn: VpnSnapshot) -> impl IntoView {
    view! {
        <div class="card traffic-card">
            <div class="card-header">
                <span class="card-title">"Tunnel Traffic"</span>
                <span class="traffic-rssi" id="vpn-chart-scale">"scale 1 B/s"</span>
                <span class="traffic-rssi" id="vpn-traffic-total">
                    {format!("{} in / {} out", format_bytes(vpn.rx_bytes), format_bytes(vpn.tx_bytes))}
                </span>
            </div>
            <div class="traffic-legend">
                <div class="traffic-legend-item rx">
                    <span class="traffic-arrow">"▲"</span>
                    <span class="traffic-legend-label">"IN"</span>
                    <span class="traffic-rate" id="vpn-rx-rate">"0 B/s"</span>
                    <span class="traffic-peak" id="vpn-rx-peak">"peak 0 B/s"</span>
                </div>
                <div class="traffic-legend-item tx">
                    <span class="traffic-arrow">"▼"</span>
                    <span class="traffic-legend-label">"OUT"</span>
                    <span class="traffic-rate" id="vpn-tx-rate">"0 B/s"</span>
                    <span class="traffic-peak" id="vpn-tx-peak">"peak 0 B/s"</span>
                </div>
            </div>
            <svg class="traffic-chart" id="vpn-chart" viewBox="0 0 600 160" preserveAspectRatio="none"></svg>
            <div class="vpn-wire">
                <div class="vpn-wire-head">
                    <span class="vpn-section-title">"Live packets"</span>
                    <label class="feature-switch vpn-wire-pause">
                        <input type="checkbox" id="vpn-wire-pause"/>
                        <span class="feature-switch-track"><span class="feature-switch-thumb"></span></span>
                        <span class="feature-switch-label">"Hold"</span>
                    </label>
                </div>
                <div class="vpn-wire-list" id="vpn-wire-list">
                    <div class="vpn-wire-empty">"Waiting for traffic…"</div>
                </div>
            </div>
            <div class="traffic-counters">
                <div class="traffic-counter">
                    <span class="metric-label">"Packets in"</span>
                    <span class="metric-value stat-rx" id="vpn-rx-packets">{vpn.rx_packets}</span>
                    <span class="traffic-sub" id="vpn-rx-bytes">{format_bytes(vpn.rx_bytes)}</span>
                </div>
                <div class="traffic-counter">
                    <span class="metric-label">"Packets out"</span>
                    <span class="metric-value stat-tx" id="vpn-tx-packets">{vpn.tx_packets}</span>
                    <span class="traffic-sub" id="vpn-tx-bytes">{format_bytes(vpn.tx_bytes)}</span>
                </div>
            </div>
        </div>
    }
}

// ── Peers ─────────────────────────────────────────────────────────────────────

/// Who is on the tunnel, at which address, and what you can do about it.
///
/// One row per peer with the route-through switch and a ping on it, because
/// every question an operator has here is about a specific node: can I reach
/// it, will it route for me, am I routing through it. Splitting those across
/// separate cards made you hold a node in your head while you moved between
/// them.
#[component]
fn VpnPeersCard(peers: Vec<kaonic_vpn::VpnPeerSnapshot>) -> impl IntoView {
    let count = peers.len();
    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"Peers"</span>
                <span class="card-body-text">
                    <span id="vpn-peers-count">{count}</span>
                    {if count == 1 { " node on the tunnel" } else { " nodes on the tunnel" }}
                </span>
            </div>
            <div class="vpn-peers-wrap">
                <table class="vpn-peers-table">
                    <thead>
                        <tr>
                            <th>"Node"</th>
                            <th>"Tunnel IP"</th>
                            <th>"Link"</th>
                            <th>"Offers"</th>
                            <th>"Last seen"</th>
                            <th>"Route through"</th>
                            <th>"Reachable"</th>
                        </tr>
                    </thead>
                    <tbody id="vpn-peers-body">
                        <tr><td colspan="7" class="vpn-peers-empty">"Loading…"</td></tr>
                    </tbody>
                </table>
            </div>
            <p class="vpn-note" id="vpn-uplink-detail"></p>
        </div>
    }
}

// ── This device ───────────────────────────────────────────────────────────────

/// Everything about *this* node, in one panel: what it is on the tunnel, what
/// it offers, and whether it forwards for anyone.
///
/// These were two cards. They answered the same question from two sides — what
/// this device is and what it gives out — and separating them meant reading
/// half the story, scrolling, and reading the other half.
#[component]
fn VpnDeviceCard(
    local_hash: String,
    local_routes: Vec<String>,
    interface_name: Option<String>,
    backend: String,
    tunnel_ip: Option<String>,
    network: String,
    codename: String,
    wlan0_ip: Option<String>,
    usb0_ip: Option<String>,
) -> impl IntoView {
    let iface = interface_name.unwrap_or_else(|| "—".into());
    let tunnel_ip = tunnel_ip.unwrap_or_else(|| "—".into());
    let wlan0 = wlan0_ip.unwrap_or_else(|| "not connected".into());
    let usb0 = usb0_ip.unwrap_or_else(|| "not connected".into());
    let backend_badge = format!(
        "badge {}",
        if backend == "linux" {
            "reticulum-badge-kind-data"
        } else {
            "reticulum-badge-soft"
        }
    );
    let route_count = local_routes.len();

    view! {
        <div class="card vpn-device-card">
            <div class="card-header">
                <span class="card-title">"This Device"</span>
                <span class=backend_badge id="vpn-backend">{backend}</span>
            </div>

            // ── Identity ────────────────────────────────────────────────────
            <div class="vpn-section">
                <div class="vpn-section-title">"Identity"</div>
                <dl class="vpn-facts">
                    <dt>"Codename"</dt><dd>{codename}</dd>
                    <dt>"Node hash"</dt>
                    <dd><code class="vpn-facts-mono">{local_hash.clone()}</code></dd>
                </dl>
            </div>

            // ── Tunnel ──────────────────────────────────────────────────────
            <div class="vpn-section">
                <div class="vpn-section-title">"Tunnel"</div>
                <dl class="vpn-facts">
                    <dt>"Address"</dt>
                    <dd><code class="vpn-facts-mono vpn-facts-accent">{tunnel_ip}</code></dd>
                    <dt>"Network"</dt><dd><code class="vpn-facts-mono">{network}</code></dd>
                    <dt>"Interface"</dt><dd><code class="vpn-facts-mono">{iface}</code></dd>
                </dl>
            </div>

            // ── Local networks ──────────────────────────────────────────────
            <div class="vpn-section">
                <div class="vpn-section-title">"Local networks"</div>
                <dl class="vpn-facts">
                    <dt>"Wi-Fi"</dt><dd><code class="vpn-facts-mono">{wlan0}</code></dd>
                    <dt>"USB"</dt><dd><code class="vpn-facts-mono">{usb0}</code></dd>
                </dl>
            </div>

            // ── What peers can see ──────────────────────────────────────────
            <div class="vpn-section">
                <div class="vpn-section-title vpn-section-title--action">
                    <span>"Offered to peers"</span>
                    <button type="button" class="btn-secondary btn-small" data-open-vpn-routes>
                        "Advertise routes"
                    </button>
                </div>
                <div class="vpn-route-list" id="vpn-local-routes-list">
                    {if route_count == 0 {
                        view! {
                            <p class="vpn-setup-empty">
                                "Nothing offered yet. " <em>"Advertise routes"</em>
                                " shares a local subnet with paired nodes."
                            </p>
                        }.into_any()
                    } else {
                        local_routes.into_iter().map(|route| {
                            let (alias, local) = parse_route_display(&route);
                            view! {
                                <div class="vpn-route-item">
                                    <span class="vpn-route-alias">{alias}</span>
                                    {local.map(|l| view! {
                                        <span class="vpn-route-local">"→ local " {l}</span>
                                    })}
                                </div>
                            }
                        }).collect_view().into_any()
                    }}
                </div>
            </div>

            // ── Acting as a router ──────────────────────────────────────────
            <div class="vpn-section vpn-section--last">
                <div class="vpn-section-title vpn-section-title--action">
                    <span>"Share with peers"</span>
                    <span class="badge" id="vpn-gateway-badge">"Off"</span>
                </div>
                <p class="card-body-text">
                    "Forward paired nodes' traffic onto a local network, or this device's whole internet connection."
                </p>
                <div class="vpn-share-controls">
                    <label class="feature-switch">
                        <input type="checkbox" id="vpn-gateway-toggle"/>
                        <span class="feature-switch-track"><span class="feature-switch-thumb"></span></span>
                        <span class="feature-switch-label">"Act as a router"</span>
                    </label>
                </div>
                <div class="vpn-share-body">
                    <label class="vpn-field-label" for="vpn-gateway-routes">"Networks to share"</label>
                    <input
                        type="text"
                        id="vpn-gateway-routes"
                        class="form-input"
                        placeholder="192.168.5.0/24, or 0.0.0.0/0 for the whole connection"
                    />
                    <div class="vpn-share-presets">
                        <button type="button" class="btn-secondary btn-small" data-vpn-preset="lan">"Local LAN"</button>
                        <button type="button" class="btn-secondary btn-small" data-vpn-preset="internet">"Whole connection"</button>
                    </div>
                    <button type="button" class="btn-primary btn-small" id="vpn-gateway-apply">
                        "Apply"
                    </button>
                </div>
                <p class="vpn-note" id="vpn-gateway-detail"></p>
            </div>
        </div>
    }
}


#[component]
fn VpnRouteEditorModal(advertised_routes: Vec<String>) -> impl IntoView {
    let advertised_text = default_advertised_route_strings(advertised_routes).join("\n");
    view! {
        <div class="modal-backdrop" id="vpn-routes-modal" hidden>
            <div class="modal-card">
                <div class="modal-header">
                    <h2 class="modal-title">"Advertise local subnets"</h2>
                    <button type="button" class="modal-close" data-close-vpn-routes>"×"</button>
                </div>
                <form class="modal-form" id="vpn-routes-form">
                    <p class="card-body-text" style="margin-bottom:12px;">
                        "Enter one CIDR subnet per line — e.g. "
                        <code style="font-family:var(--font-mono);font-size:12px;">"192.168.10.0/24"</code>
                        ". Each subnet stays local on this Kaonic and is shared with peers over the VPN link as a VPN alias to avoid conflicts."
                    </p>
                    <textarea
                        id="vpn-routes-editor-input"
                        class="field-input radio-test-textarea"
                        placeholder="192.168.10.0/24"
                        style="min-height:120px;"
                    >{advertised_text}</textarea>
                    <div id="vpn-route-editor-status" style="min-height:18px;font-size:13px;margin-top:6px;"></div>
                    <div class="modal-actions">
                        <button type="button" class="btn-secondary" data-close-vpn-routes>"Cancel"</button>
                        <button type="submit" id="vpn-routes-save" class="btn-primary">"Save"</button>
                    </div>
                </form>
            </div>
        </div>
    }
}

// ── WebSocket live-update script ──────────────────────────────────────────────

const VPN_JS: &str = r#"
(function () {
    var state = window.__vpn || { paired: [], gateway: {}, uplink: {} };
    var peers = state.peers || [];
    var renderedKeys = null;
    // True while a share change is in flight, so a status frame cannot yank
    // the control out from under the request that is still being answered.
    var shareBusy = false;
    var shareError = false;
    var HISTORY = 60;
    var rx = [], tx = [], peakRx = 0, peakTx = 0;
    var lastBytes = null, lastTs = 0;

    function el(id) { return document.getElementById(id); }
    function setText(id, text) { var e = el(id); if (e) { e.textContent = text; } }
    function esc(v) {
        return String(v == null ? '' : v)
            .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }
    function fmtRate(bps) {
        if (bps >= 1048576) { return (bps / 1048576).toFixed(1) + ' MB/s'; }
        if (bps >= 1024) { return (bps / 1024).toFixed(1) + ' KB/s'; }
        return Math.round(bps) + ' B/s';
    }
    function fmtBytes(n) {
        n = Number(n) || 0;
        if (n >= 1073741824) { return (n / 1073741824).toFixed(2) + ' GB'; }
        if (n >= 1048576) { return (n / 1048576).toFixed(1) + ' MB'; }
        if (n >= 1024) { return (n / 1024).toFixed(1) + ' KB'; }
        return n + ' B';
    }

    // ── Traffic chart ──────────────────────────────────────────────────────
    // Same shape as the dashboard's radio charts: in above the line, out
    // below, so direction reads without a legend.
    function draw(inRate, outRate) {
        var svg = el('vpn-chart');
        if (!svg) { return; }
        var W = 600, H = 160, mid = H / 2, pad = 6;
        var max = 1;
        for (var i = 0; i < rx.length; i++) { max = Math.max(max, rx[i], tx[i]); }
        var step = Math.pow(2, Math.ceil(Math.log(max) / Math.log(2)));
        var scale = (mid - pad) / step;
        function path(series, dir) {
            var n = HISTORY, d = '', x0 = 0;
            var start = n - series.length;
            d += 'M' + ((start / (n - 1)) * W).toFixed(1) + ',' + mid;
            for (var i = 0; i < series.length; i++) {
                var x = ((start + i) / (n - 1)) * W;
                d += 'L' + x.toFixed(1) + ',' + (mid - dir * series[i] * scale).toFixed(1);
                x0 = x;
            }
            return d + 'L' + x0.toFixed(1) + ',' + mid + 'Z';
        }
        var grid = '';
        for (var g = 1; g <= 3; g++) {
            var gy = (mid - pad) * g / 4;
            grid += '<line class="tg-grid" x1="0" x2="' + W + '" y1="' + (mid - gy).toFixed(1) + '" y2="' + (mid - gy).toFixed(1) + '"/>';
            grid += '<line class="tg-grid" x1="0" x2="' + W + '" y1="' + (mid + gy).toFixed(1) + '" y2="' + (mid + gy).toFixed(1) + '"/>';
        }
        svg.innerHTML =
            '<defs><linearGradient id="vpn-g-rx" x1="0" y1="0" x2="0" y2="1">' +
            '<stop offset="0%" stop-color="rgba(52,208,88,.85)"/><stop offset="100%" stop-color="rgba(52,208,88,.08)"/></linearGradient>' +
            '<linearGradient id="vpn-g-tx" x1="0" y1="0" x2="0" y2="1">' +
            '<stop offset="0%" stop-color="rgba(13,203,240,.08)"/><stop offset="100%" stop-color="rgba(13,203,240,.85)"/></linearGradient></defs>' +
            grid +
            '<path class="tg-area tg-rx" fill="url(#vpn-g-rx)" d="' + path(rx, 1) + '"/>' +
            '<path class="tg-area tg-tx" fill="url(#vpn-g-tx)" d="' + path(tx, -1) + '"/>' +
            '<line class="tg-mid" x1="0" x2="' + W + '" y1="' + mid + '" y2="' + mid + '"/>';
        // The viewBox is stretched to the card width, so anything drawn inside
        // it is stretched too. Areas survive that; letterforms do not, so the
        // scale is reported in HTML beside the title instead.
        setText('vpn-chart-scale', '\u00b1' + fmtRate(step) + ' full scale');
        setText('vpn-rx-rate', fmtRate(inRate));
        setText('vpn-tx-rate', fmtRate(outRate));
        setText('vpn-rx-peak', 'peak ' + fmtRate(peakRx));
        setText('vpn-tx-peak', 'peak ' + fmtRate(peakTx));
    }

    // A short scroll of what is actually crossing the tunnel. Summaries only —
    // no payloads are captured — but enough to answer "is my traffic going
    // over the radio, and what is it" without reaching for tcpdump.
    function renderWire(recent) {
        var list = el('vpn-wire-list');
        var pause = el('vpn-wire-pause');
        if (!list || (pause && pause.checked)) { return; }
        if (!recent || !recent.length) {
            if (!list.querySelector('.vpn-wire-empty')) {
                list.innerHTML = '<div class="vpn-wire-empty">Waiting for traffic…</div>';
            }
            return;
        }
        list.innerHTML = recent.map(function (p) {
            var t = new Date((p.ts || 0) * 1000);
            var stamp = String(t.getHours()).padStart(2, '0') + ':' +
                String(t.getMinutes()).padStart(2, '0') + ':' +
                String(t.getSeconds()).padStart(2, '0') + '.' +
                String(p.ms || 0).padStart(3, '0');
            var out = p.dir === 'tx';
            // The port is what separates one flow from another on the same
            // pair of hosts, so it belongs with the address, not in a column
            // of its own that would be empty for ICMP.
            function endpoint(addr, port) {
                return esc(addr) + (port == null ? '' : '<span class="vpn-wire-port">:' + port + '</span>');
            }
            return '<div class="vpn-wire-row ' + (out ? 'is-tx' : 'is-rx') + '">' +
                '<span class="vpn-wire-time">' + stamp + '</span>' +
                '<span class="vpn-wire-dir">' + (out ? '\u25b2' : '\u25bc') + '</span>' +
                '<span class="vpn-wire-addr">' + endpoint(p.src, p.sport) + '</span>' +
                '<span class="vpn-wire-arrow">\u2192</span>' +
                '<span class="vpn-wire-addr">' + endpoint(p.dst, p.dport) + '</span>' +
                '<span class="vpn-wire-proto">' + esc(p.proto) + '</span>' +
                '<span class="vpn-wire-len">' + (p.len || 0) + ' B</span>' +
                '</div>';
        }).join('');
    }

    function sampleTraffic(snap) {
        var now = Date.now();
        var inRate = 0, outRate = 0;
        if (lastBytes) {
            var dt = Math.max(0.25, (now - lastTs) / 1000);
            inRate = Math.max(0, snap.rx_bytes - lastBytes.rx) / dt;
            outRate = Math.max(0, snap.tx_bytes - lastBytes.tx) / dt;
        }
        lastBytes = { rx: snap.rx_bytes || 0, tx: snap.tx_bytes || 0 };
        lastTs = now;
        rx.push(inRate); tx.push(outRate);
        if (rx.length > HISTORY) { rx.shift(); tx.shift(); }
        peakRx = Math.max(peakRx, inRate); peakTx = Math.max(peakTx, outRate);
        draw(inRate, outRate);
        setText('vpn-rx-packets', String(snap.rx_packets || 0));
        setText('vpn-tx-packets', String(snap.tx_packets || 0));
        setText('vpn-rx-bytes', fmtBytes(snap.rx_bytes));
        setText('vpn-tx-bytes', fmtBytes(snap.tx_bytes));
        setText('vpn-traffic-total', fmtBytes(snap.rx_bytes) + ' in / ' + fmtBytes(snap.tx_bytes) + ' out');
        setText('vpn-peer-count', String((snap.peers || []).length));
        if (snap.local_tunnel_ip) { setText('vpn-local-ip', snap.local_tunnel_ip); }
    }

    // ── Route through ──────────────────────────────────────────────────────

    // ── Peer table ─────────────────────────────────────────────────────────

    function peerName(peer) {
        return peer.tag || peer.codename || (peer.destination || '').slice(0, 8);
    }

    function ago(ts) {
        if (!ts) { return 'never'; }
        var secs = Math.max(0, Math.floor(Date.now() / 1000) - ts);
        if (secs < 5) { return 'just now'; }
        if (secs < 60) { return secs + ' sec ago'; }
        if (secs < 3600) { return Math.floor(secs / 60) + ' min ago'; }
        if (secs < 86400) { return Math.floor(secs / 3600) + ' hr ago'; }
        return Math.floor(secs / 86400) + ' d ago';
    }

    function linkDot(linkState) {
        if (linkState === 'active') { return 'status-dot status-dot--ok'; }
        if (linkState === 'pending') { return 'status-dot status-dot--warn'; }
        return 'status-dot status-dot--idle';
    }

    // Rows are built once and then patched in place. Replacing the tbody on
    // every socket frame reflowed the table under the pointer and threw away
    // whatever a ping had just written into it — which is what made clicking
    // Ping feel like the table jumped away from you.
    function rowKeys() {
        return peers.map(function (p) { return p.destination; }).join('|');
    }

    function buildRows() {
        var body = el('vpn-peers-body');
        if (!body) { return; }
        if (!peers.length) {
            body.innerHTML = '<tr><td colspan="7" class="vpn-peers-empty">' +
                'No peers yet. Only paired nodes join the VPN \u2014 pair one on the Remote page.' +
                '</td></tr>';
            return;
        }
        body.innerHTML = peers.map(function (peer) {
            var key = esc(peer.destination);
            return '<tr data-peer="' + key + '">' +
                '<td><span class="status-dot" data-cell="dot"></span>' +
                    '<span class="vpn-peers-name" data-cell="name"></span>' +
                    '<span class="vpn-peers-sub" data-cell="sub"></span>' +
                '</td>' +
                '<td class="vpn-peers-ip" data-cell="ip"></td>' +
                '<td data-cell="link"></td>' +
                '<td class="vpn-peers-routes" data-cell="offers"></td>' +
                '<td class="vpn-peers-seen" data-cell="seen"></td>' +
                '<td data-cell="route"></td>' +
                '<td class="vpn-peers-ping" data-cell="ping"></td>' +
                '</tr>';
        }).join('');
        renderedKeys = rowKeys();
    }

    function renderPeers() {
        var body = el('vpn-peers-body');
        if (!body) { return; }
        setText('vpn-peers-count', String(peers.length));
        if (rowKeys() !== renderedKeys) { buildRows(); }

        var selected = state.uplink.enabled ? (state.uplink.peer || '') : '';
        var banner = el('vpn-banner-router');
        if (banner) {
            var chosen = peers.find(function (p) { return p.identity_hash === selected; });
            banner.textContent = selected
                ? (chosen ? peerName(chosen) : selected.slice(0, 8))
                : '\u2014';
        }

        peers.forEach(function (peer) {
            var row = body.querySelector('[data-peer="' + peer.destination + '"]');
            if (!row) { return; }
            function cell(name) { return row.querySelector('[data-cell="' + name + '"]'); }
            function put(name, text) {
                var c = cell(name);
                if (c && c.textContent !== text) { c.textContent = text; }
            }

            cell('dot').className = linkDot(peer.link_state);
            put('name', peerName(peer));
            put('sub', peer.codename && peer.tag ? peer.codename : '');
            put('ip', peer.tunnel_ip || '\u2014');
            put('link', peer.link_state || 'unknown');
            put('offers', (peer.announced_routes || []).join(', ') || '\u2014');
            put('seen', ago(peer.last_seen_ts));

            // The route control: only rebuilt when its shape changes, so the
            // switch never re-renders under a finger mid-toggle.
            var id = peer.identity_hash || '';
            var isSelected = !!id && id === selected;
            var canRoute = !!peer.is_gateway;
            var shape = !id ? 'none' : (!canRoute && !isSelected) ? 'blocked' : 'switch';
            var routeCell = cell('route');
            if (routeCell.getAttribute('data-shape') !== shape) {
                routeCell.setAttribute('data-shape', shape);
                if (shape === 'none') {
                    routeCell.innerHTML = '<span class="vpn-peers-muted">not paired here</span>';
                } else if (shape === 'blocked') {
                    routeCell.innerHTML = '<span class="vpn-peers-muted" ' +
                        'title="This node is not sharing a network. Turn on Share with peers there.">' +
                        'not sharing</span>';
                } else {
                    routeCell.innerHTML =
                        '<label class="feature-switch vpn-route-switch">' +
                        '<input type="checkbox" data-router="' + esc(id) + '"/>' +
                        '<span class="feature-switch-track"><span class="feature-switch-thumb"></span></span>' +
                        '</label>';
                }
            }
            var box = routeCell.querySelector('[data-router]');
            if (box && box.checked !== isSelected && document.activeElement !== box) {
                box.checked = isSelected;
            }

            // The ping cell is left alone once built: it holds a result the
            // operator asked for, and no status frame should erase it.
            var pingCell = cell('ping');
            var ip = peer.tunnel_ip || '';
            if (pingCell.getAttribute('data-ip') !== ip) {
                pingCell.setAttribute('data-ip', ip);
                pingCell.innerHTML = ip
                    ? '<button type="button" class="btn-secondary btn-small" data-ping="' + esc(ip) + '">Ping</button>' +
                      '<span class="vpn-ping-result"></span>'
                    : '<span class="vpn-peers-muted">\u2014</span>';
            }
        });

        var detail = el('vpn-uplink-detail');
        if (detail) {
            if (state.uplink.detail) {
                detail.textContent = state.uplink.detail;
            } else if (state.uplink.active) {
                detail.textContent = 'Reaching ' + state.uplink.routes.join(', ') +
                    (state.uplink.default_route ? ' \u2014 all traffic goes over the radio.' : '.');
            } else {
                detail.textContent = '';
            }
        }
    }

    async function selectRouter(hash, on) {
        var body = on ? { enabled: true, peer: hash } : { enabled: false };
        try {
            var resp = await fetch('/api/vpn/uplink', {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify(body)
            });
            if (!resp.ok) { throw new Error(await resp.text()); }
            await refresh();
        } catch (err) {
            var detail = el('vpn-uplink-detail');
            if (detail) { detail.textContent = String(err.message || err); }
        }
    }

    // ── Share with peers ───────────────────────────────────────────────────

    // The server owns this switch. An optimistic local flag here was the bug:
    // flipping the toggle changed nothing on the device, and the next status
    // frame — one second later — put the checkbox back, so it could never be
    // turned on at all.
    function renderShare() {
        var toggle = el('vpn-gateway-toggle');
        var input = el('vpn-gateway-routes');
        // Never move a control the operator is currently using.
        if (toggle && document.activeElement !== toggle && !shareBusy) {
            toggle.checked = !!state.gateway.enabled;
        }
        if (input && document.activeElement !== input && !shareBusy) {
            input.value = (state.gateway.routes || []).join(', ');
        }
        var badge = el('vpn-gateway-badge');
        if (badge) {
            var on = state.gateway.enabled;
            badge.textContent = on ? (state.gateway.active ? 'Active' : 'Waiting') : 'Off';
            badge.className = 'badge ' + (on ? (state.gateway.active ? 'badge-ok' : 'badge-warn') : '');
        }
        var detail = el('vpn-gateway-detail');
        if (detail && !shareError) {
            if (state.gateway.detail) {
                detail.textContent = state.gateway.detail;
                detail.className = 'vpn-note';
            } else if (state.gateway.active) {
                detail.textContent = 'Forwarding out ' + (state.gateway.egress_interface || 'the default route') + '.';
                detail.className = 'vpn-note is-ok';
            } else {
                detail.textContent = '';
                detail.className = 'vpn-note';
            }
        }
    }

    async function applyShare(enabled, routes) {
        var detail = el('vpn-gateway-detail');
        shareBusy = true;
        shareError = false;
        try {
            var resp = await fetch('/api/vpn/gateway', {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ enabled: enabled, routes: routes })
            });
            if (!resp.ok) {
                var text = await resp.text();
                throw new Error(text || ('HTTP ' + resp.status));
            }
            shareBusy = false;
            await refresh();
        } catch (err) {
            shareBusy = false;
            shareError = true;
            if (detail) {
                detail.textContent = String(err.message || err);
                detail.className = 'vpn-note is-err';
            }
            // Put the switch back to whatever the device actually did.
            await refresh();
        }
    }

    function routesFromInput() {
        var input = el('vpn-gateway-routes');
        if (!input) { return []; }
        return input.value.split(/[\s,]+/).filter(function (v) { return v.length; });
    }

    /// Pings a peer's tunnel address. This is the question "does the VPN
    /// actually carry traffic to that node", which no amount of status text
    /// answers as well as trying it.
    async function runPing(address, button) {
        var out = button.parentElement.querySelector('.vpn-ping-result');
        if (out) { out.textContent = '…'; out.className = 'vpn-ping-result'; }
        if (button) { button.disabled = true; }
        try {
            var resp = await fetch('/api/vpn/ping', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ address: address })
            });
            var body = await resp.json();
            if (out) {
                if (body.ok) {
                    out.textContent = body.latency ? body.latency : 'ok';
                    out.className = 'vpn-ping-result is-ok';
                } else {
                    out.textContent = 'no reply';
                    out.className = 'vpn-ping-result is-err';
                }
            }
        } catch (err) {
            if (out) { out.textContent = 'failed'; out.className = 'vpn-ping-result is-err'; }
        } finally {
            if (button) { button.disabled = false; }
        }
    }

    // ── Refresh ────────────────────────────────────────────────────────────

    async function refresh() {
        try {
            var [gw, up] = await Promise.all([
                fetch('/api/vpn/gateway').then(function (r) { return r.json(); }),
                fetch('/api/vpn/uplink').then(function (r) { return r.json(); })
            ]);
            state.gateway = gw;
            state.uplink = up;
            renderPeers();
            renderShare();
        } catch (err) { /* transient; the next tick tries again */ }
    }

    // The gateway already publishes a VPN snapshot on the status socket, once
    // a second. Polling a second endpoint for the same numbers would only add
    // load to a single-core board.
    function connect() {
        var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
        var ws = new WebSocket(proto + '//' + location.host + '/api/ws/status');
        ws.onmessage = function (event) {
            try {
                var msg = JSON.parse(event.data) || {};
                if (msg.type !== 'vpn') { return; }
                var vpn = msg.data || {};
                sampleTraffic(vpn);
                renderWire(vpn.recent);
                var dot = el('vpn-status-dot');
                var text = el('vpn-status-text');
                if (text && vpn.status) { text.textContent = vpn.status; }
                if (dot && vpn.status) {
                    var kind = vpn.status === 'running' ? 'ok'
                        : vpn.status === 'error' ? 'err'
                        : vpn.status === 'mock' ? 'idle' : 'warn';
                    dot.className = 'status-dot status-dot--' + kind;
                }
                if (vpn.network) { setText('vpn-network', vpn.network); }
                if (vpn.interface_name) { setText('vpn-interface', vpn.interface_name); }
                var bar = el('vpn-error-bar');
                if (bar) {
                    if (vpn.last_error) {
                        bar.style.display = '';
                        bar.className = 'vpn-error-bar';
                        bar.textContent = '\u26a0 ' + vpn.last_error;
                    } else {
                        bar.style.display = 'none';
                    }
                }
                // The peer list and both cards come from the same snapshot, so
                // the router picker follows link state without another request.
                if (Array.isArray(vpn.peers)) { peers = vpn.peers; }
                if (vpn.gateway) { state.gateway = vpn.gateway; renderShare(); }
                if (vpn.uplink) { state.uplink = vpn.uplink; }
                renderPeers();
            } catch (err) { /* a malformed frame is not worth tearing down for */ }
        };
        ws.onclose = function () { setTimeout(connect, 3000); };
        ws.onerror = function () { ws.close(); };
    }


    // ── Wiring ─────────────────────────────────────────────────────────────

    document.addEventListener('change', function (event) {
        var box = event.target.closest('[data-router]');
        if (box) { selectRouter(box.getAttribute('data-router'), box.checked); }
    });

    document.addEventListener('click', function (event) {
        var ping = event.target.closest('[data-ping]');
        if (ping) { runPing(ping.getAttribute('data-ping'), ping); return; }
        var preset = event.target.closest('[data-vpn-preset]');
        if (preset) {
            var input = el('vpn-gateway-routes');
            if (input) {
                input.value = preset.getAttribute('data-vpn-preset') === 'internet'
                    ? '0.0.0.0/0'
                    : (window.__vpnLan || '192.168.0.0/24');
            }
            return;
        }
        if (event.target.closest('#vpn-gateway-apply')) {
            var routes = routesFromInput();
            if (!routes.length) {
                var d = el('vpn-gateway-detail');
                if (d) {
                    d.textContent = 'List at least one network, or 0.0.0.0/0 to share the whole connection.';
                    d.className = 'vpn-note is-err';
                }
                shareError = true;
                return;
            }
            applyShare(true, routes);
        }
    });

    var toggle = el('vpn-gateway-toggle');
    if (toggle) {
        toggle.addEventListener('change', function () {
            if (!toggle.checked) {
                applyShare(false, []);
                return;
            }
            var routes = routesFromInput();
            if (!routes.length) {
                // Nothing to share yet: say so rather than sending a request
                // the device will refuse, and leave the switch off.
                toggle.checked = false;
                var detail = el('vpn-gateway-detail');
                if (detail) {
                    detail.textContent = 'List a network to share first — or use one of the buttons below.';
                    detail.className = 'vpn-note is-err';
                }
                shareError = true;
                return;
            }
            applyShare(true, routes);
        });
    }

    // ── VPN feature switch ─────────────────────────────────────────────────
    // Enabling or disabling the VPN is a service-level change, like the shell
    // switch: it is saved immediately and takes effect on the next start, so
    // the label says which of those two states you are looking at.
    (function () {
        var box = el('feature-vpn');
        if (!box) { return; }
        var label = el('feature-vpn-label');
        var current = {};

        function paint(features) {
            current = features || {};
            if (document.activeElement !== box) {
                box.checked = current.vpn_enabled !== false;
            }
            if (label) {
                label.textContent = current.vpn_enabled
                    ? (current.restart_required ? 'Restart to apply' : 'Running')
                    : (current.restart_required ? 'Restart to stop' : 'Disabled');
            }
            var banner = el('vpn-banner');
            if (banner) {
                banner.classList.toggle('remote-banner--restart', !!current.restart_required);
            }
        }

        function refresh() {
            fetch('/api/system/features')
                .then(function (r) { return r.json(); })
                .then(paint)
                .catch(function () { /* transient */ });
        }

        box.addEventListener('change', function () {
            fetch('/api/system/features', {
                method: 'PUT',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({
                    vpn_enabled: box.checked,
                    // The other switches are sent back unchanged; this endpoint
                    // takes the whole set, and omitting one would clear it.
                    remote_enabled: current.remote_enabled !== false,
                    shell_enabled: !!current.shell_enabled,
                    restart_required: false
                })
            })
                .then(function (r) { return r.json(); })
                .then(paint)
                .catch(refresh);
        });

        refresh();
        setInterval(refresh, 10000);
    })();

    renderPeers();
    renderShare();
    connect();
    // Relative times go stale on their own even when nothing arrives.
    setInterval(renderPeers, 5000);
})();
"#;
