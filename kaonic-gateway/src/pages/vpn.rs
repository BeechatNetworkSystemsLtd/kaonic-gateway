use kaonic_vpn::{VpnPeerSnapshot, VpnRouteSnapshot, VpnSnapshot};
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageTitle;

// ── Snapshot ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VpnPageSnapshot {
    pub local_hash: String,
    pub wlan0_ip: Option<String>,
    pub usb0_ip: Option<String>,
    pub vpn: VpnSnapshot,
}

#[server]
pub async fn load_vpn_snapshot() -> Result<VpnPageSnapshot, ServerFnError> {
    use crate::network::read_interface_ipv4;
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    Ok(VpnPageSnapshot {
        local_hash: state.vpn_hash.clone(),
        wlan0_ip: read_interface_ipv4("wlan0"),
        usb0_ip: read_interface_ipv4("usb0"),
        vpn: match &state.vpn {
            Some(vpn) => vpn.snapshot().await,
            None => VpnSnapshot::default(),
        },
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

fn vpn_badge_class(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        "running" | "active" | "ready" | "installed" | "yes" => "badge-ok",
        "discovered" | "configured" | "pending" | "starting" => "badge-warn",
        "error" | "closed" | "failed" | "no" | "drop" => "badge-err",
        _ => "reticulum-badge-soft",
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

fn peer_dot_class(link_state: &str) -> &'static str {
    match link_state.trim().to_ascii_lowercase().as_str() {
        "active" => "status-dot status-dot--ok",
        "pending" | "starting" | "configured" | "discovered" => "status-dot status-dot--warn",
        "closed" | "error" | "failed" => "status-dot status-dot--err",
        _ => "status-dot status-dot--idle",
    }
}

/// Parse "alias/prefix -> local/prefix" route strings produced by the VPN.
/// Returns (displayed_alias, Option<local_net>).
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

fn serial_test_ip(route: &str) -> Option<String> {
    let network = route.split('/').next()?.trim();
    let mut octets = network.split('.');
    let a = octets.next()?;
    let b = octets.next()?;
    let c = octets.next()?;
    let d = octets.next()?;
    if octets.next().is_some() || d != "0" {
        return None;
    }
    Some(format!("{a}.{b}.{c}.1"))
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
        <div class="page">
            <PageTitle icon="🔐" title="VPN" />
            <Suspense fallback=|| view! { <p class="loading">"Loading…"</p> }>
                {move || match snapshot.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(e)) => view! {
                        <div class="error-banner">"Error: "{e.to_string()}</div>
                    }.into_any(),
                    Some(Ok(snap)) => view! { <VpnContent snapshot=snap/> }.into_any(),
                }}
            </Suspense>
        </div>
    }
}

#[component]
fn VpnContent(snapshot: VpnPageSnapshot) -> impl IntoView {
    let vpn = &snapshot.vpn;

    let status = vpn.status.clone();
    let peer_count = vpn.peers.len();
    let tunnel_ip = vpn.local_tunnel_ip.clone().unwrap_or_else(|| "—".into());
    let wlan0_ip = snapshot.wlan0_ip.clone().unwrap_or_else(|| "—".into());
    let usb0_ip = snapshot.usb0_ip.clone().unwrap_or_else(|| "—".into());
    let network = vpn.network.clone();
    let tx_bytes = format_bytes(vpn.tx_bytes);
    let rx_bytes = format_bytes(vpn.rx_bytes);
    let has_error = vpn.last_error.is_some();
    let last_error = vpn.last_error.clone().unwrap_or_default();

    let installed_routes: Vec<&VpnRouteSnapshot> =
        vpn.remote_routes.iter().filter(|r| r.installed).collect();

    view! {
        // ── Connection banner ────────────────────────────────────────────────
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
                <span class="vpn-banner-label">"wlan0 IP"</span>
                <span class="vpn-banner-ip" id="vpn-wlan0-ip">{wlan0_ip}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"usb0 IP"</span>
                <span class="vpn-banner-ip" id="vpn-usb0-ip">{usb0_ip}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Network"</span>
                <span class="vpn-banner-ip vpn-banner-net" id="vpn-network">{network}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"TX bytes"</span>
                <span class="vpn-banner-ip" id="vpn-tx-bytes">{tx_bytes}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"TX packets"</span>
                <span class="vpn-banner-ip" id="vpn-tx-packets">{vpn.tx_packets}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"RX bytes"</span>
                <span class="vpn-banner-ip" id="vpn-rx-bytes">{rx_bytes}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"RX packets"</span>
                <span class="vpn-banner-ip" id="vpn-rx-packets">{vpn.rx_packets}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Drops"</span>
                <span class="vpn-banner-ip" id="vpn-drop-packets">{vpn.drop_packets}</span>
            </div>
            <div class="vpn-banner-spacer"></div>
            <div class="vpn-banner-peers">
                <span id="vpn-peer-count">{peer_count}</span>
                " peer"{if peer_count == 1 { "" } else { "s" }}
            </div>
        </div>

        // ── Error bar (visible only when there's an active error) ────────────
        {if has_error {
            view! {
                <div class="vpn-error-bar" id="vpn-error-bar">
                    "⚠ " <span id="vpn-error-msg">{last_error}</span>
                </div>
            }.into_any()
        } else {
            view! { <div id="vpn-error-bar" style="display:none"></div> }.into_any()
        }}

        // ── Top grid: This Device + Laptop Setup ─────────────────────────────
        <div class="vpn-twin-row">
            <VpnThisDeviceCard
                local_hash=snapshot.local_hash.clone()
                local_routes=vpn.local_routes.clone()
                advertised_routes=vpn.advertised_routes.clone()
                interface_name=vpn.interface_name.clone()
                backend=vpn.backend.clone()
            />
            <VpnLaptopSetupCard
                installed_routes=installed_routes.iter().map(|r| r.network.clone()).collect()
            />
        </div>

        // ── Peers ─────────────────────────────────────────────────────────────
        <VpnPeersCard peers=vpn.peers.clone() />

        // ── Advanced / Debug ──────────────────────────────────────────────────
        <VpnDebugSection
            local_hash=snapshot.local_hash.clone()
            vpn=snapshot.vpn.clone()
        />

        // ── Route editor modal (shared, opened by the device card button) ─────
        <VpnRouteEditorModal
            advertised_routes=vpn.advertised_routes.clone()
        />

        <script>{VPN_WS_JS}</script>
    }
}

// ── This Device card ──────────────────────────────────────────────────────────

#[component]
fn VpnThisDeviceCard(
    local_hash: String,
    local_routes: Vec<String>,
    advertised_routes: Vec<String>,
    interface_name: Option<String>,
    backend: String,
) -> impl IntoView {
    let iface = interface_name.unwrap_or_else(|| "—".into());
    let _ = advertised_routes;
    let backend_badge = format!(
        "badge {}",
        if backend == "linux" {
            "reticulum-badge-kind-data"
        } else {
            "reticulum-badge-soft"
        }
    );
    let hash_short = truncate_hash(&local_hash);

    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"This Device"</span>
                <div style="display:flex;gap:8px;align-items:center;">
                    <span class=backend_badge id="vpn-backend">{backend}</span>
                    <button type="button" class="btn-secondary" style="padding:4px 12px;font-size:13px;" data-open-vpn-routes>
                        "Advertise routes"
                    </button>
                </div>
            </div>

            // Interface row
            <div class="info-row">
                <span class="info-label">"Interface"</span>
                <span class="info-value" id="vpn-interface">{iface}</span>
            </div>

            // Identity — short hash + tooltip with full value
            <div class="info-row">
                <span class="info-label">"Identity"</span>
                <code class="info-value vpn-hash-display" title=local_hash id="vpn-identity-short">
                    {hash_short}
                </code>
            </div>

            // Advertised routes (alias → local)
            <div style="margin-top:12px;">
                <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.06em;color:var(--text-muted);margin-bottom:8px;">
                    "Exported VPN aliases"
                </div>
                <div class="vpn-route-list" id="vpn-local-routes-list">
                    {if local_routes.is_empty() {
                        view! {
                            <p class="vpn-setup-empty">
                                "Nothing exported yet. Use "
                                <em>"Advertise routes"</em>
                                " to share a local subnet."
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
        </div>
    }
}

// ── Laptop Setup card ─────────────────────────────────────────────────────────

#[component]
fn VpnLaptopSetupCard(installed_routes: Vec<String>) -> impl IntoView {
    let has_routes = !installed_routes.is_empty();
    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"Client Access"</span>
            </div>
            <p class="card-body-text" style="margin-bottom:12px;">
                "Clients connected directly to this device's AP or USB network should reach remote VPN aliases automatically. Each remote Kaonic keeps its own local subnet unchanged."
            </p>
            <div class="vpn-setup-list" id="vpn-setup-commands">
                {if !has_routes {
                    view! {
                        <p class="vpn-setup-empty">"Waiting for remote peer routes…"</p>
                        <p class="vpn-setup-note">
                            "Remote VPN aliases will appear here once a peer is connected and has advertised its local subnet."
                        </p>
                    }.into_any()
                } else {
                    installed_routes.into_iter().map(|net| {
                        let wget_cmd = serial_test_ip(&net)
                            .map(|ip| format!("wget -qO- http://{ip}/api/serial"));
                        view! {
                            {wget_cmd.map(|cmd| {
                                let cmd_copy = cmd.clone();
                                view! {
                                    <div class="vpn-setup-cmd">
                                        <span class="vpn-setup-cmd-text">{cmd}</span>
                                        <button
                                            type="button"
                                            class="vpn-copy-btn"
                                            data-vpn-copy=cmd_copy
                                        >"Copy"</button>
                                    </div>
                                }.into_any()
                            }).unwrap_or_else(|| view! { <span></span> }.into_any())}
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </div>
    }
}

// ── Peers card ────────────────────────────────────────────────────────────────

#[component]
fn VpnPeersCard(peers: Vec<VpnPeerSnapshot>) -> impl IntoView {
    let count = peers.len();
    view! {
        <div class="card" style="margin-bottom:18px;">
            <div class="card-header">
                <span class="card-title">"Connected Peers"</span>
                <span class="badge reticulum-badge-soft" id="vpn-peers-badge">
                    {count}" peer"{if count == 1 { "" } else { "s" }}
                </span>
            </div>
            <div class="vpn-peers-list" id="vpn-peers">
                {if peers.is_empty() {
                    view! {
                        <div class="vpn-empty-peers">
                            <div style="font-size:28px;opacity:.4;margin-bottom:8px;">"📡"</div>
                            <div style="font-weight:600;margin-bottom:6px;">"No peers discovered yet"</div>
                            <div style="font-size:13px;max-width:340px;line-height:1.6;">
                                "Peers appear automatically once a remote kaonic device is within radio range and running the same VPN network configuration."
                            </div>
                        </div>
                    }.into_any()
                } else {
                    peers.into_iter().map(|peer| {
                        let tunnel_ip = peer.tunnel_ip.clone().unwrap_or_else(|| "—".into());
                        let has_ip = peer.tunnel_ip.is_some();
                        let ip_class = if has_ip {
                            "vpn-peer-ip"
                        } else {
                            "vpn-peer-ip vpn-peer-ip--none"
                        };
                        let hash_full = peer.destination.clone();
                        let hash_short = truncate_hash(&peer.destination);
                        let dot_class = peer_dot_class(&peer.link_state);
                        let state_badge = format!("badge {}", vpn_badge_class(&peer.link_state));
                        let last_seen = format_relative_time(peer.last_seen_ts);
                        let ping_ip = peer.tunnel_ip.clone().unwrap_or_default();
                        let ping_disabled = !has_ip;
                        let speed_disabled = !has_ip;
                        let tx_bps_str = format_bps(peer.tx_bps);
                        let rx_bps_str = format_bps(peer.rx_bps);
                        let tx_bytes_str = format_bytes(peer.tx_bytes);
                        let rx_bytes_str = format_bytes(peer.rx_bytes);
                        let tx_packets = peer.tx_packets;
                        let rx_packets = peer.rx_packets;

                        view! {
                            <div class="vpn-peer-row">
                                // Status dot + identity
                                <div class="vpn-peer-left">
                                    <span class=dot_class></span>
                                    <div class="vpn-peer-ident">
                                        <div class="vpn-peer-field">
                                            <span class="vpn-peer-field-label">"Tunnel"</span>
                                            <span class=ip_class>{tunnel_ip}</span>
                                        </div>
                                        <div class="vpn-peer-field">
                                            <span class="vpn-peer-field-label">"Peer"</span>
                                            <span
                                                class="vpn-peer-hash"
                                                title=hash_full.clone()
                                            >{hash_short}</span>
                                        </div>
                                    </div>
                                </div>

                                // Announced route tags
                                <div class="vpn-peer-routes">
                                    <span class="vpn-peer-section-label">"Routes"</span>
                                    <div class="vpn-peer-routes-list">
                                        {if peer.announced_routes.is_empty() {
                                            view! {
                                                <span class="badge reticulum-badge-soft" style="opacity:.6;">"no routes"</span>
                                            }.into_any()
                                        } else {
                                            peer.announced_routes.iter().map(|r| view! {
                                                <span class="vpn-route-tag">{r.clone()}</span>
                                            }).collect_view().into_any()
                                        }}
                                    </div>
                                </div>

                                // Traffic: tx/rx speed + totals
                                <div class="vpn-peer-traffic">
                                    <div class="vpn-peer-traffic-row">
                                        <span class="vpn-peer-traffic-dir">"TX"</span>
                                        <span class="vpn-peer-traffic-rate">{tx_bps_str}</span>
                                        <span class="vpn-peer-traffic-total" title=format!("{tx_packets} packets")>
                                            {tx_bytes_str}
                                        </span>
                                    </div>
                                    <div class="vpn-peer-traffic-row">
                                        <span class="vpn-peer-traffic-dir">"RX"</span>
                                        <span class="vpn-peer-traffic-rate">{rx_bps_str}</span>
                                        <span class="vpn-peer-traffic-total" title=format!("{rx_packets} packets")>
                                            {rx_bytes_str}
                                        </span>
                                    </div>
                                </div>

                                // Meta: last seen + link state
                                <div class="vpn-peer-meta">
                                    <div class="vpn-peer-field vpn-peer-field--meta">
                                        <span class="vpn-peer-field-label">"Seen"</span>
                                        <span class="vpn-peer-lastseen">{last_seen}</span>
                                    </div>
                                    <div class="vpn-peer-field vpn-peer-field--meta">
                                        <span class="vpn-peer-field-label">"State"</span>
                                        <span class=state_badge>{peer.link_state.clone()}</span>
                                    </div>
                                </div>

                                // Ping
                                <div class="vpn-peer-actions">
                                    <div class="vpn-peer-action-buttons">
                                        <button
                                            type="button"
                                            class="btn-secondary vpn-ping-btn"
                                            data-vpn-ping
                                            data-peer-key=hash_full.clone()
                                            data-peer-ip=ping_ip.clone()
                                            disabled=ping_disabled
                                        >"Ping"</button>
                                        <button
                                            type="button"
                                            class="btn-secondary vpn-speed-btn"
                                            data-vpn-speed-test
                                            data-peer-key=hash_full.clone()
                                            data-peer-ip=ping_ip
                                            disabled=speed_disabled
                                        >"Test speed"</button>
                                    </div>
                                    <div
                                        class="vpn-ping-status"
                                        data-vpn-ping-status=hash_full
                                    ></div>
                                    <div
                                        class="vpn-ping-status"
                                        data-vpn-speed-status=peer.destination.clone()
                                    ></div>
                                </div>
                            </div>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </div>
    }
}

// ── Advanced / Debug section ──────────────────────────────────────────────────

#[component]
fn VpnDebugSection(local_hash: String, vpn: VpnSnapshot) -> impl IntoView {
    view! {
        <details class="vpn-advanced">
            <summary>"Advanced / Debug"</summary>
            <div class="vpn-advanced-body">
                // Identity
                <div>
                    <div class="vpn-adv-stat-label" style="margin-bottom:6px;">"Full Identity Hash"</div>
                    <code class="td-hex td-hash" style="font-size:12px;word-break:break-all;">
                        {local_hash}
                    </code>
                </div>

                // Route mapping table
                <div>
                    <div class="vpn-adv-stat-label" style="margin-bottom:8px;">"Local → VPN Alias Mapping"</div>
                    <div class="reticulum-table-wrap">
                        <table class="frames-table">
                            <thead>
                                <tr>
                                    <th>"Local subnet"</th>
                                    <th>"Tunnel IP"</th>
                                    <th>"Exported VPN alias"</th>
                                </tr>
                            </thead>
                            <tbody id="vpn-route-mappings">
                                {if vpn.route_mappings.is_empty() {
                                    view! {
                                        <tr><td colspan="3" class="frames-empty">"No local route mappings yet"</td></tr>
                                    }.into_any()
                                } else {
                                    vpn.route_mappings.into_iter().map(|mapping| {
                                        view! {
                                            <tr>
                                                <td class="td-hex">{mapping.subnet}</td>
                                                <td class="td-hex">{mapping.tunnel}</td>
                                                <td class="td-hex">{mapping.mapped_subnet}</td>
                                            </tr>
                                        }
                                    }).collect_view().into_any()
                                }}
                            </tbody>
                        </table>
                    </div>
                </div>

                // Remote routes table
                <div>
                    <div class="vpn-adv-stat-label" style="margin-bottom:8px;">"Remote VPN Aliases"</div>
                    <div class="reticulum-table-wrap">
                        <table class="frames-table">
                            <thead>
                                <tr>
                                    <th>"VPN alias"</th>
                                    <th>"Owner"</th>
                                    <th>"Status"</th>
                                    <th>"Last seen"</th>
                                    <th>"Installed"</th>
                                </tr>
                            </thead>
                            <tbody id="vpn-routes">
                                {if vpn.remote_routes.is_empty() {
                                    view! {
                                        <tr><td colspan="5" class="frames-empty">"No remote routes yet"</td></tr>
                                    }.into_any()
                                } else {
                                    vpn.remote_routes.into_iter().map(|route| {
                                        let state_class = format!("badge {}", vpn_badge_class(&route.status));
                                        let installed_class = format!("badge {}", vpn_badge_class(if route.installed { "yes" } else { "no" }));
                                        view! {
                                            <tr>
                                                <td class="td-hex">{route.network}</td>
                                                <td class="td-hex td-hash" style="font-size:11px;">
                                                    {truncate_hash(&route.owner)}
                                                </td>
                                                <td><span class=state_class>{route.status}</span></td>
                                                <td class="td-time">{format_relative_time(route.last_seen_ts)}</td>
                                                <td>
                                                    <span class=installed_class>
                                                        {if route.installed { "yes" } else { "no" }}
                                                    </span>
                                                </td>
                                            </tr>
                                        }
                                    }).collect_view().into_any()
                                }}
                            </tbody>
                        </table>
                    </div>
                </div>
            </div>
        </details>
    }
}

// ── Route editor modal ────────────────────────────────────────────────────────

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

const VPN_WS_JS: &str = r#"
(function() {
    var pingState = Object.create(null);
    var speedState = Object.create(null);

    // ── Utilities ──────────────────────────────────────────────────────────

    function shouldPause() {
        if (document.body.classList.contains('modal-open')) { return true; }
        var a = document.activeElement;
        if (a && (a.tagName === 'INPUT' || a.tagName === 'TEXTAREA' || a.tagName === 'SELECT' || a.isContentEditable)) { return true; }
        var sel = window.getSelection ? window.getSelection() : null;
        return !!(sel && !sel.isCollapsed && String(sel).trim().length > 0);
    }

    function esc(v) {
        return String(v == null ? '' : v)
            .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }

    function setText(id, text) {
        var el = document.getElementById(id);
        if (el) el.textContent = text;
    }

    function fmtBytes(b) {
        b = Number(b) || 0;
        if (b >= 1048576) { return (b / 1048576).toFixed(1) + ' MB'; }
        if (b >= 1024) { return (b / 1024).toFixed(1) + ' KB'; }
        return b + ' B';
    }

    function fmtBps(b) {
        b = Number(b) || 0;
        if (b >= 1000000) { return (b / 1000000).toFixed(1) + ' Mbps'; }
        if (b >= 1000)    { return (b / 1000).toFixed(1) + ' Kbps'; }
        return b + ' bps';
    }

    function serialTestIp(route) {
        var network = String(route || '').split('/')[0] || '';
        var octets = network.trim().split('.');
        if (octets.length !== 4 || octets[3] !== '0') { return ''; }
        return octets[0] + '.' + octets[1] + '.' + octets[2] + '.1';
    }

    function fmtRelative(ts) {
        if (!ts) { return 'never'; }
        var diff = Math.floor(Date.now() / 1000) - Number(ts);
        if (diff < 3)    { return 'just now'; }
        if (diff < 60)   { return diff + 's ago'; }
        if (diff < 3600) { return Math.floor(diff / 60) + 'm ago'; }
        return Math.floor(diff / 3600) + 'h ago';
    }

    function vpnBadgeClass(v) {
        v = String(v || '').trim().toLowerCase();
        if (v === 'running' || v === 'active' || v === 'ready' || v === 'installed' || v === 'yes') { return 'badge-ok'; }
        if (v === 'discovered' || v === 'configured' || v === 'pending' || v === 'starting') { return 'badge-warn'; }
        if (v === 'error' || v === 'closed' || v === 'failed' || v === 'no' || v === 'drop') { return 'badge-err'; }
        return 'reticulum-badge-soft';
    }

    function statusDotClass(s) {
        s = String(s || '').toLowerCase().trim();
        if (s === 'running') { return 'status-dot status-dot--ok'; }
        if (s === 'error')   { return 'status-dot status-dot--err'; }
        if (s === 'mock')    { return 'status-dot status-dot--idle'; }
        return 'status-dot status-dot--warn';
    }

    function bannerClass(s) {
        s = String(s || '').toLowerCase().trim();
        if (s === 'running') { return 'vpn-banner vpn-banner--ok'; }
        if (s === 'error')   { return 'vpn-banner vpn-banner--err'; }
        return 'vpn-banner vpn-banner--idle';
    }

    function peerDotClass(s) {
        s = String(s || '').trim().toLowerCase();
        if (s === 'active') { return 'status-dot status-dot--ok'; }
        if (s === 'pending' || s === 'starting' || s === 'configured' || s === 'discovered') { return 'status-dot status-dot--warn'; }
        if (s === 'closed' || s === 'error' || s === 'failed') { return 'status-dot status-dot--err'; }
        return 'status-dot status-dot--idle';
    }

    function flashCopyButton(btn, label) {
        if (!btn) { return; }
        btn.textContent = label;
        btn.classList.add('copied');
        setTimeout(function() {
            btn.textContent = 'Copy';
            btn.classList.remove('copied');
        }, 1800);
    }

    function fallbackCopyText(text) {
        var input = document.createElement('textarea');
        input.value = text;
        input.setAttribute('readonly', '');
        input.style.position = 'fixed';
        input.style.top = '-1000px';
        input.style.left = '-1000px';
        document.body.appendChild(input);
        input.focus();
        input.select();
        var ok = false;
        try { ok = document.execCommand('copy'); } catch (_) {}
        document.body.removeChild(input);
        return ok;
    }

    function copyText(text) {
        if (!text) { return Promise.resolve(false); }
        if (navigator.clipboard && window.isSecureContext) {
            return navigator.clipboard.writeText(text).then(function() { return true; }).catch(function() {
                return fallbackCopyText(text);
            });
        }
        return Promise.resolve(fallbackCopyText(text));
    }

    function shortHash(h) {
        var c = String(h || '').replace(/\s+/g, '');
        return c.length > 16 ? c.substring(0, 16) + '\u2026' : c;
    }

    // ── Banner + error bar ──────────────────────────────────────────────────

    function updateBanner(vpn) {
        var banner = document.getElementById('vpn-banner');
        if (banner) { banner.className = bannerClass(vpn.status || ''); }

        var dot = document.getElementById('vpn-status-dot');
        if (dot) { dot.className = statusDotClass(vpn.status || ''); }

        setText('vpn-status-text', vpn.status || '—');
        setText('vpn-local-ip',    vpn.local_tunnel_ip || '—');
        setText('vpn-network',     vpn.network || '—');

        var peerCount = (vpn.peers || []).length;
        setText('vpn-peer-count', String(peerCount));
        var badge = document.getElementById('vpn-peers-badge');
        if (badge) { badge.textContent = peerCount + ' peer' + (peerCount === 1 ? '' : 's'); }

        var errBar = document.getElementById('vpn-error-bar');
        if (errBar) { errBar.hidden = !vpn.last_error; }
        setText('vpn-error-msg', vpn.last_error || '');
    }

    // ── This Device card ────────────────────────────────────────────────────

    function renderLocalRoutesList(routes) {
        var el = document.getElementById('vpn-local-routes-list');
        if (!el) { return; }
        if (!routes || routes.length === 0) {
            el.innerHTML = '<p class="vpn-setup-empty">Nothing advertised yet. Use <em>Advertise routes</em> to share a local subnet.</p>';
            return;
        }
        el.innerHTML = routes.map(function(route) {
            var parts = route.split(' -> ');
            var alias = (parts[0] || '').trim();
            var local = parts.length > 1 ? (parts[1] || '').trim() : null;
            return '<div class="vpn-route-item">'
                + '<span class="vpn-route-alias">' + esc(alias) + '</span>'
                + (local ? '<span class="vpn-route-local">\u2192 your ' + esc(local) + '</span>' : '')
                + '</div>';
        }).join('');
    }

    function setAdvertisedRoutes(routes) {
        var input = document.getElementById('vpn-routes-editor-input');
        if (!input || document.body.classList.contains('modal-open')) { return; }
        routes = Array.isArray(routes) ? routes.filter(function(route) {
            return String(route || '').trim().length > 0;
        }) : [];
        if (routes.length === 0) { routes = ['192.168.10.0/24']; }
        input.value = routes.join('\n');
    }

    // ── Laptop setup card ───────────────────────────────────────────────────

    function renderSetupCommands(remoteRoutes) {
        var el = document.getElementById('vpn-setup-commands');
        if (!el) { return; }
        var installed = (remoteRoutes || []).filter(function(r) { return r.installed; });
        if (installed.length === 0) {
            el.innerHTML = '<p class="vpn-setup-empty">Waiting for remote peer routes\u2026</p>'
                + '<p class="vpn-setup-note">Remote VPN aliases will appear here once a peer is connected and has advertised its local subnet.</p>';
            return;
        }
        el.innerHTML = installed.map(function(r) {
            var network = r.network || '';
            var wgetIp = serialTestIp(network);
            var html = '';
            if (wgetIp) {
                var wgetCmd = 'wget -qO- http://' + wgetIp + '/api/serial';
                html += '<div class="vpn-setup-cmd">'
                    + '<span class="vpn-setup-cmd-text">' + esc(wgetCmd) + '</span>'
                    + '<button type="button" class="vpn-copy-btn" data-vpn-copy="' + esc(wgetCmd) + '">Copy</button>'
                    + '</div>';
            }
            return html;
        }).join('')
        + '<p class="vpn-setup-note" style="margin-top:10px;">The wget example reads the remote Kaonic serial from <code style="font-family:monospace;font-size:11px;">/api/serial</code>.</p>';
    }

    // ── Peers card ──────────────────────────────────────────────────────────

    function renderPeers(peers) {
        var el = document.getElementById('vpn-peers');
        if (!el) { return; }

        if (!peers || peers.length === 0) {
            el.innerHTML = '<div class="vpn-empty-peers">'
                + '<div style="font-size:28px;opacity:.4;margin-bottom:8px;">\uD83D\uDCE1</div>'
                + '<div style="font-weight:600;margin-bottom:6px;">No peers discovered yet</div>'
                + '<div style="font-size:13px;max-width:340px;line-height:1.6;">Peers appear automatically once a remote kaonic device is within radio range and running the same VPN network configuration.</div>'
                + '</div>';
            return;
        }

        el.innerHTML = peers.map(function(peer) {
            var ip   = peer.tunnel_ip || '\u2014';
            var hasIp = !!peer.tunnel_ip;
            var ipCls = hasIp ? 'vpn-peer-ip' : 'vpn-peer-ip vpn-peer-ip--none';
            var hash = String(peer.destination || '');
            var sh   = shortHash(hash);
            var routes = peer.announced_routes || [];
            var routeHtml = routes.length > 0
                ? routes.map(function(r) { return '<span class="vpn-route-tag">' + esc(r) + '</span>'; }).join('')
                : '<span class="badge reticulum-badge-soft" style="opacity:.6">no routes</span>';
            var pingKey = hash;
            var ps  = pingState[pingKey] || {};
            var ss  = speedState[pingKey] || {};
            var pingBusy = !!ps.busy;
            var speedBusy = !!ss.busy;
            var pingDisabled = !hasIp || pingBusy;
            var speedDisabled = !hasIp || speedBusy;
            var pingStatusCls = 'vpn-ping-status' + (ps.kind ? ' ' + ps.kind : '');
            var speedStatusCls = 'vpn-ping-status' + (ss.kind ? ' ' + ss.kind : '');
            var stateBadgeCls = 'badge ' + vpnBadgeClass(peer.link_state || '\u2014');

            var txBps   = fmtBps(peer.tx_bps);
            var rxBps   = fmtBps(peer.rx_bps);
            var txBytes = fmtBytes(peer.tx_bytes);
            var rxBytes = fmtBytes(peer.rx_bytes);
            var txPkts  = Number(peer.tx_packets) || 0;
            var rxPkts  = Number(peer.rx_packets) || 0;

            return '<div class="vpn-peer-row">'
                + '<div class="vpn-peer-left">'
                    + '<span class="' + peerDotClass(peer.link_state) + '"></span>'
                    + '<div class="vpn-peer-ident">'
                        + '<div class="vpn-peer-field">'
                            + '<span class="vpn-peer-field-label">Tunnel</span>'
                            + '<span class="' + ipCls + '">' + esc(ip) + '</span>'
                        + '</div>'
                        + '<div class="vpn-peer-field">'
                            + '<span class="vpn-peer-field-label">Peer</span>'
                            + '<span class="vpn-peer-hash" title="' + esc(hash) + '">' + esc(sh) + '</span>'
                        + '</div>'
                    + '</div>'
                + '</div>'
                + '<div class="vpn-peer-routes">'
                    + '<span class="vpn-peer-section-label">Routes</span>'
                    + '<div class="vpn-peer-routes-list">' + routeHtml + '</div>'
                + '</div>'
                + '<div class="vpn-peer-traffic">'
                    + '<div class="vpn-peer-traffic-row">'
                        + '<span class="vpn-peer-traffic-dir">TX</span>'
                        + '<span class="vpn-peer-traffic-rate">' + esc(txBps) + '</span>'
                        + '<span class="vpn-peer-traffic-total" title="' + txPkts + ' packets">' + esc(txBytes) + '</span>'
                    + '</div>'
                    + '<div class="vpn-peer-traffic-row">'
                        + '<span class="vpn-peer-traffic-dir">RX</span>'
                        + '<span class="vpn-peer-traffic-rate">' + esc(rxBps) + '</span>'
                        + '<span class="vpn-peer-traffic-total" title="' + rxPkts + ' packets">' + esc(rxBytes) + '</span>'
                    + '</div>'
                + '</div>'
                + '<div class="vpn-peer-meta">'
                    + '<div class="vpn-peer-field vpn-peer-field--meta">'
                        + '<span class="vpn-peer-field-label">Seen</span>'
                        + '<span class="vpn-peer-lastseen">' + esc(fmtRelative(peer.last_seen_ts)) + '</span>'
                    + '</div>'
                    + '<div class="vpn-peer-field vpn-peer-field--meta">'
                        + '<span class="vpn-peer-field-label">State</span>'
                        + '<span class="' + stateBadgeCls + '">' + esc(peer.link_state || '\u2014') + '</span>'
                    + '</div>'
                + '</div>'
                + '<div class="vpn-peer-actions">'
                    + '<div class="vpn-peer-action-buttons">'
                        + '<button type="button" class="btn-secondary vpn-ping-btn" data-vpn-ping'
                            + ' data-peer-key="' + esc(pingKey) + '"'
                            + ' data-peer-ip="' + esc(peer.tunnel_ip || '') + '"'
                            + (pingDisabled ? ' disabled' : '') + '>'
                        + esc(pingBusy ? 'Pinging\u2026' : 'Ping')
                        + '</button>'
                        + '<button type="button" class="btn-secondary vpn-speed-btn" data-vpn-speed-test'
                            + ' data-peer-key="' + esc(pingKey) + '"'
                            + ' data-peer-ip="' + esc(peer.tunnel_ip || '') + '"'
                            + (speedDisabled ? ' disabled' : '') + '>'
                        + esc(speedBusy ? 'Testing\u2026' : 'Test speed')
                        + '</button>'
                    + '</div>'
                    + '<div class="' + pingStatusCls + '" data-vpn-ping-status="' + esc(pingKey) + '">'
                    + esc(ps.text || '')
                    + '</div>'
                    + '<div class="' + speedStatusCls + '" data-vpn-speed-status="' + esc(pingKey) + '">'
                    + esc(ss.text || '')
                    + '</div>'
                + '</div>'
                + '</div>';
        }).join('');
    }

    // ── Debug section ───────────────────────────────────────────────────────

    function renderDebugRoutes(routes) {
        var tbody = document.getElementById('vpn-routes');
        if (!tbody) { return; }
        if (!routes || routes.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5" class="frames-empty">No remote routes yet</td></tr>';
            return;
        }
        tbody.innerHTML = routes.map(function(r) {
            var sc = 'badge ' + vpnBadgeClass(r.status || '');
            var ic = 'badge ' + vpnBadgeClass(r.installed ? 'yes' : 'no');
            var ownerShort = (r.owner||'').replace(/\s+/g,'').substring(0,16)
                + ((r.owner||'').replace(/\s+/g,'').length > 16 ? '\u2026' : '');
            return '<tr>'
                + '<td class="td-hex">' + esc(r.network||'\u2014') + '</td>'
                + '<td class="td-hex td-hash" style="font-size:11px;" title="' + esc(r.owner||'') + '">' + esc(ownerShort) + '</td>'
                + '<td><span class="' + sc + '">' + esc(r.status||'\u2014') + '</span></td>'
                + '<td class="td-time">' + esc(fmtRelative(r.last_seen_ts)) + '</td>'
                + '<td><span class="' + ic + '">' + (r.installed ? 'yes' : 'no') + '</span></td>'
                + '</tr>';
        }).join('');
    }

    function renderRouteMappings(mappings) {
        var tbody = document.getElementById('vpn-route-mappings');
        if (!tbody) { return; }
        if (!mappings || mappings.length === 0) {
            tbody.innerHTML = '<tr><td colspan="3" class="frames-empty">No local route mappings yet</td></tr>';
            return;
        }
        tbody.innerHTML = mappings.map(function(m) {
            return '<tr>'
                + '<td class="td-hex">' + esc(m.subnet || '\u2014') + '</td>'
                + '<td class="td-hex">' + esc(m.tunnel || '\u2014') + '</td>'
                + '<td class="td-hex">' + esc(m.mapped_subnet || '\u2014') + '</td>'
                + '</tr>';
        }).join('');
    }

    // ── WebSocket ───────────────────────────────────────────────────────────

    function connect() {
        var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
        var ws = new WebSocket(proto + '//' + location.host + '/api/ws/status');

        ws.onmessage = function(ev) {
            try {
                if (shouldPause()) { return; }
                var msg = JSON.parse(ev.data) || {};
                if (msg.type === 'interfaces') {
                    var interfaces = msg.data || {};
                    setText('vpn-wlan0-ip', interfaces.wlan0_ip || '\u2014');
                    setText('vpn-usb0-ip', interfaces.usb0_ip || '\u2014');
                    return;
                }
                if (msg.type !== 'vpn') { return; }
                var vpn = msg.data || {};
                updateBanner(vpn);
                renderLocalRoutesList(vpn.local_routes || []);
                setAdvertisedRoutes(vpn.advertised_routes || []);
                renderSetupCommands(vpn.remote_routes || []);
                renderPeers(vpn.peers || []);
                setText('vpn-interface', vpn.interface_name || '\u2014');
                var backendEl = document.getElementById('vpn-backend');
                if (backendEl) {
                    backendEl.textContent = vpn.backend || '\u2014';
                    backendEl.className = 'badge ' + (vpn.backend === 'linux' ? 'reticulum-badge-kind-data' : 'reticulum-badge-soft');
                }
                setText('vpn-tx-packets', String(vpn.tx_packets || 0));
                setText('vpn-tx-bytes',   fmtBytes(vpn.tx_bytes || 0));
                setText('vpn-rx-packets', String(vpn.rx_packets || 0));
                setText('vpn-rx-bytes',   fmtBytes(vpn.rx_bytes || 0));
                setText('vpn-drop-packets', String(vpn.drop_packets || 0));
                renderRouteMappings(vpn.route_mappings || []);
                renderDebugRoutes(vpn.remote_routes || []);
            } catch (e) {}
        };

        ws.onclose = function() { setTimeout(connect, 3000); };
        ws.onerror = function() { ws.close(); };
    }

    // ── Ping handler ────────────────────────────────────────────────────────

    document.addEventListener('click', function(ev) {
        var target = ev.target;
        if (!(target instanceof HTMLElement)) { return; }

        // Copy to clipboard
        var copyBtn = target.closest('[data-vpn-copy]');
        if (copyBtn) {
            var text = copyBtn.getAttribute('data-vpn-copy') || '';
            copyText(text).then(function(ok) {
                flashCopyButton(copyBtn, ok ? 'Copied!' : 'Copy failed');
            });
            return;
        }

        // Open route editor
        if (target.closest('[data-open-vpn-routes]')) {
            openRouteEditor();
            return;
        }

        // Ping
        var pingBtn = target.closest('[data-vpn-ping]');
        if (pingBtn) {
            var key = pingBtn.getAttribute('data-peer-key') || '';
            var ip  = pingBtn.getAttribute('data-peer-ip') || '';
            if (!key || !ip || ip === '\u2014') { return; }

            pingState[key] = { text: 'Pinging\u2026', kind: 'pending', busy: true };
            pingBtn.disabled = true;
            pingBtn.textContent = 'Pinging\u2026';
            var statusEl = document.querySelector('[data-vpn-ping-status="' + key + '"]');
            if (statusEl) {
                statusEl.textContent = 'Pinging\u2026';
                statusEl.className = 'vpn-ping-status pending';
            }

            fetch('/api/vpn/ping', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ address: ip })
            }).then(function(resp) {
                return resp.text().then(function(t) {
                    var d = {};
                    try { d = JSON.parse(t); } catch (_) { d = {}; }
                    if (!resp.ok) { throw new Error('failed'); }
                    return d;
                });
            }).then(function(d) {
                var ok = !!(d && d.ok);
                var latency = d && d.latency;
                pingState[key] = {
                    text: ok ? (latency || 'passed') : 'failed',
                    kind: ok ? 'ok' : 'err',
                    busy: false
                };
            }).catch(function(err) {
                pingState[key] = {
                    text: 'failed',
                    kind: 'err',
                    busy: false
                };
            }).finally(function() {
                var btn = document.querySelector('[data-vpn-ping][data-peer-key="' + key + '"]');
                var st  = document.querySelector('[data-vpn-ping-status="' + key + '"]');
                if (btn) { btn.disabled = false; btn.textContent = 'Ping'; }
                if (st) {
                    st.textContent = pingState[key].text || '';
                    st.className = 'vpn-ping-status' + (pingState[key].kind ? ' ' + pingState[key].kind : '');
                }
            });
            return;
        }

        // Speed test
        var speedBtn = target.closest('[data-vpn-speed-test]');
        if (speedBtn) {
            var speedKey = speedBtn.getAttribute('data-peer-key') || '';
            var speedIp  = speedBtn.getAttribute('data-peer-ip') || '';
            if (!speedKey || !speedIp || speedIp === '\u2014') { return; }

            speedState[speedKey] = { text: 'Testing\u2026', kind: 'pending', busy: true };
            speedBtn.disabled = true;
            speedBtn.textContent = 'Testing\u2026';
            var speedStatusEl = document.querySelector('[data-vpn-speed-status="' + speedKey + '"]');
            if (speedStatusEl) {
                speedStatusEl.textContent = 'Testing\u2026';
                speedStatusEl.className = 'vpn-ping-status pending';
            }

            fetch('/api/vpn/speed-test', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ address: speedIp })
            }).then(function(resp) {
                return resp.text().then(function(t) {
                    var d = {};
                    try { d = JSON.parse(t); } catch (_) { d = {}; }
                    if (!resp.ok) {
                        throw new Error((d && d.error) || 'failed');
                    }
                    return d;
                });
            }).then(function(d) {
                var bytes = Number(d && d.bytes) || 0;
                var durationMs = Number(d && d.duration_ms) || 0;
                var bps = Number(d && d.bps) || 0;
                speedState[speedKey] = {
                    text: fmtBytes(bytes) + ' in ' + durationMs + ' ms · ' + fmtBps(bps),
                    kind: d && d.ok ? 'ok' : 'err',
                    busy: false
                };
            }).catch(function() {
                speedState[speedKey] = {
                    text: 'failed',
                    kind: 'err',
                    busy: false
                };
            }).finally(function() {
                var btn = document.querySelector('[data-vpn-speed-test][data-peer-key="' + speedKey + '"]');
                var st  = document.querySelector('[data-vpn-speed-status="' + speedKey + '"]');
                if (btn) { btn.disabled = false; btn.textContent = 'Test speed'; }
                if (st) {
                    st.textContent = speedState[speedKey].text || '';
                    st.className = 'vpn-ping-status' + (speedState[speedKey].kind ? ' ' + speedState[speedKey].kind : '');
                }
            });
            return;
        }

        // Close route editor
        if (target.closest('[data-close-vpn-routes]') || target.id === 'vpn-routes-modal') {
            closeRouteEditor();
        }
    });

    document.addEventListener('keydown', function(ev) {
        if (ev.key === 'Escape') { closeRouteEditor(); }
    });

    // ── Route editor ────────────────────────────────────────────────────────

    function setRouteEditorStatus(text, kind) {
        var el = document.getElementById('vpn-route-editor-status');
        if (el) { el.textContent = text; el.className = kind || ''; }
    }

    function openRouteEditor() {
        var modal = document.getElementById('vpn-routes-modal');
        if (!modal) { return; }
        setRouteEditorStatus('', '');
        modal.hidden = false;
        document.body.classList.add('modal-open');
        var input = document.getElementById('vpn-routes-editor-input');
        if (input) { input.focus(); input.select(); }
    }

    function closeRouteEditor() {
        var modal = document.getElementById('vpn-routes-modal');
        if (!modal) { return; }
        modal.hidden = true;
        document.body.classList.remove('modal-open');
    }

    document.addEventListener('submit', function(ev) {
        var form = ev.target;
        if (!(form instanceof HTMLFormElement) || form.id !== 'vpn-routes-form') { return; }
        ev.preventDefault();
        var input  = document.getElementById('vpn-routes-editor-input');
        var submit = document.getElementById('vpn-routes-save');
        if (!(input instanceof HTMLTextAreaElement) || !(submit instanceof HTMLButtonElement)) { return; }
        var routes = input.value
            .split(/\r?\n/)
            .map(function(v) { return v.trim(); })
            .filter(Boolean);
        submit.disabled = true;
        setRouteEditorStatus('Saving\u2026', '');
        fetch('/api/vpn/routes', {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ routes: routes })
        }).then(function(resp) {
            return resp.text().then(function(t) {
                var d = t ? JSON.parse(t) : {};
                if (!resp.ok) { throw new Error((d && d.status) || t || 'Failed to save'); }
                return d;
            });
        }).then(function(d) {
            setRouteEditorStatus((d && d.status) || 'Saved', 'flash-ok');
            setTimeout(closeRouteEditor, 250);
        }).catch(function(err) {
            setRouteEditorStatus(err && err.message ? err.message : 'Failed to save routes', 'flash-err');
        }).finally(function() {
            submit.disabled = false;
        });
    });

    connect();
})();
"#;
