use leptos::prelude::*;

use kaonic_vpn::VpnSnapshot;

use super::PageTitle;
use crate::app_types::{
    GatewayStatusDto, NetworkPortStatusDto, RadioModuleConfigDto, ServiceStatusDto, SystemStatusDto,
};
use crate::system_metrics::{
    read_cpu_percent_async, read_fs_mb, read_gateway_services, read_mem_mb, read_os_details,
};

fn radio_label(index: usize) -> &'static str {
    match index {
        0 => "Radio A",
        1 => "Radio B",
        _ => "Radio",
    }
}

// ── Server function ───────────────────────────────────────────────────────────

#[server]
pub async fn get_gateway_status() -> Result<GatewayStatusDto, ServerFnError> {
    use crate::app_types::RadioModuleConfigDto;
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    let radio_modules = state
        .settings
        .lock()
        .ok()
        .and_then(|s| s.load_config().ok())
        .map(|c| c.radio.module_configs.to_vec())
        .unwrap_or_default()
        .into_iter()
        .map(RadioModuleConfigDto::from)
        .collect();

    let system = read_system_status_async().await;
    let services = read_gateway_services();
    let network_ports = state.network_ports(&services);

    Ok(GatewayStatusDto {
        serial: state.serial.clone(),
        vpn_hash: state.vpn_hash.clone(),
        network_ports,
        system,
        services,
        radio_modules,
        reticulum: state.reticulum.snapshot().await,
        vpn: match &state.vpn {
            Some(vpn) => vpn.snapshot().await,
            None => Default::default(),
        },
    })
}

async fn read_system_status_async() -> SystemStatusDto {
    let cpu = read_cpu_percent_async().await;
    let (ram_used_mb, ram_total_mb) = read_mem_mb();
    let (fs_free_mb, fs_total_mb) = read_fs_mb();
    let os_details = read_os_details();
    SystemStatusDto {
        cpu_percent: cpu,
        ram_used_mb,
        ram_total_mb,
        fs_free_mb,
        fs_total_mb,
        os_details,
    }
}

// ── Page component ────────────────────────────────────────────────────────────

#[component]
pub fn DashboardPage() -> impl IntoView {
    let status = Resource::new(|| (), |_| get_gateway_status());

    view! {
        <div class="page">
            <PageTitle icon="📊" title="Dashboard" />
            <Suspense fallback=|| view! { <p class="loading">"Loading…"</p> }>
                {move || match status.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(e)) => view! {
                        <div class="error-banner">"Error: "{e.to_string()}</div>
                    }.into_any(),
                    Some(Ok(s)) => view! { <StatusView status=s/> }.into_any(),
                }}
            </Suspense>
            // Inline WebSocket script for live updates — no WASM needed.
            <script>{WS_SCRIPT}</script>
        </div>
    }
}

/// Plain-JS WebSocket client that patches DOM element values live.
const WS_SCRIPT: &str = r#"
(function() {
  var selectedService = null;
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
  function connect() {
    var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    var ws = new WebSocket(proto + '//' + location.host + '/api/ws/status');
    ws.onmessage = function(e) {
      try {
        if (shouldPauseLiveUpdates()) { return; }
        var msg = JSON.parse(e.data) || {};
        var data = msg.data || {};
        if (msg.type === 'system') {
          var sys = data;
          var cpu = (sys.cpu_percent || 0);
          var ramUsed = sys.ram_used_mb || 0;
          var ramTotal = sys.ram_total_mb || 0;
          var ramPct = ramTotal > 0 ? Math.round(ramUsed * 100 / ramTotal) : 0;
          var fsFree = sys.fs_free_mb || 0;
          var fsTotal = sys.fs_total_mb || 0;
          var fsUsed = Math.max(0, fsTotal - fsFree);
          var fsPct = fsTotal > 0 ? Math.round((fsTotal - fsFree) * 100 / fsTotal) : 0;
          set('os-val', sys.os_details || 'Unknown');
          set('cpu-pct', cpu.toFixed(1) + '%');
          bar('cpu-bar', cpu.toFixed(0));
          set('ram-val', ramUsed + ' / ' + ramTotal + ' MB');
          bar('ram-bar', ramPct);
          set('fs-val', formatStorageMb(fsUsed) + ' / ' + formatStorageMb(fsTotal) + ' used');
          bar('fs-bar', fsPct);
          return;
        }
        if (msg.type === 'services') {
          data.forEach(function(svc, i) {
            set('service-status-' + i, svc.status || 'unknown');
            var badge = document.getElementById('service-badge-' + i);
            if (badge) {
              badge.textContent = serviceBadgeLabel(svc);
              badge.className = 'badge ' + serviceBadgeClass(svc);
            }
          });
          var activeServices = data.filter(function(svc) {
            return (svc.load_state || '') === 'loaded' && (svc.active_state || '') === 'active';
          }).length;
          set('services-count', activeServices + '/' + data.length + ' active');
          return;
        }
        if (msg.type === 'network_ports') {
          renderNetworkPorts(data || []);
          return;
        }
        if (msg.type === 'vpn') {
          var vpn = data || {};
          var vpnStatus = (vpn.status || 'unknown');
          var vpnBadge = document.getElementById('dash-vpn-status');
          if (vpnBadge) {
            vpnBadge.textContent = vpnStatus;
            vpnBadge.className = vpnStatusBadgeClass(vpnStatus);
          }
          set('dash-vpn-tunnel', vpn.local_tunnel_ip || '—');
          var peers = vpn.peers || [];
          var active = peers.filter(function(p) { return (p.link_state || '') === 'active'; }).length;
          set('dash-vpn-peers', active + '/' + peers.length + ' linked');
          set('dash-vpn-tx', formatBytes(vpn.tx_bytes || 0));
          set('dash-vpn-rx', formatBytes(vpn.rx_bytes || 0));
          return;
        }
      } catch(err) { console.warn('ws parse error', err); }
    };
    ws.onclose = function() { setTimeout(connect, 3000); };
    ws.onerror = function() { ws.close(); };
  }
  function set(id, val) { var el = document.getElementById(id); if (el) el.textContent = val; }
  function bar(id, pct) { var el = document.getElementById(id); if (el) el.style.width = pct + '%'; }
  function serviceBadgeClass(svc) {
    if ((svc.load_state || '') !== 'loaded') { return 'badge-err'; }
    if ((svc.active_state || '') === 'active') { return 'badge-ok'; }
    if ((svc.active_state || '') === 'activating' || (svc.active_state || '') === 'reloading') { return 'badge-warn'; }
    return 'badge-err';
  }
  function serviceBadgeLabel(svc) {
    if ((svc.load_state || '') !== 'loaded') { return svc.load_state || 'missing'; }
    return svc.active_state || 'unknown';
  }
  function setRestartStatus(text, kind) {
    var status = document.getElementById('service-restart-status');
    if (!status) { return; }
    status.textContent = text;
    status.className = kind || '';
  }
  function openRestartModal(unit) {
    selectedService = unit || null;
    var modal = document.getElementById('service-restart-modal');
    if (!modal) { return; }
    set('service-restart-unit', selectedService || 'service');
    setRestartStatus('', '');
    modal.hidden = false;
    document.body.classList.add('modal-open');
  }
  function closeRestartModal() {
    var modal = document.getElementById('service-restart-modal');
    if (!modal) { return; }
    modal.hidden = true;
    document.body.classList.remove('modal-open');
  }
  function formatStorageMb(mb) {
    return mb >= 1024 ? (mb / 1024).toFixed(1) + ' GB' : mb + ' MB';
  }
  function formatBytes(bytes) {
    var KB = 1024, MB = KB * 1024, GB = MB * 1024;
    if (bytes >= GB) return (bytes / GB).toFixed(1) + ' GB';
    if (bytes >= MB) return (bytes / MB).toFixed(1) + ' MB';
    if (bytes >= KB) return (bytes / KB).toFixed(1) + ' KB';
    return bytes + ' B';
  }
  function vpnStatusBadgeClass(status) {
    if (status === 'running') return 'badge badge-ok';
    if (status === 'mock') return 'badge badge-warn';
    if (status === 'error') return 'badge badge-err';
    return 'badge';
  }
  function portBadgeClass(port) {
    var status = String((port && port.status) || '').toLowerCase();
    if (status === 'linked' || status === 'listening' || status === 'reachable' || status === 'active') { return 'badge-ok'; }
    if (status === 'waiting' || status === 'activating' || status === 'reloading') { return 'badge-warn'; }
    return 'badge-err';
  }
  function renderNetworkPorts(ports) {
    var tbody = document.getElementById('network-ports');
    if (!tbody) { return; }
    set('network-ports-count', String((ports || []).length) + ' tracked');
    if (!ports || ports.length === 0) {
      tbody.innerHTML = '<tr><td colspan="5" class="frames-empty">No network ports tracked</td></tr>';
      return;
    }
    tbody.innerHTML = ports.map(function(port) {
      return '<tr>'
        + '<td class="td-time">' + (port.name || '—') + '</td>'
        + '<td class="td-len">' + (port.protocol || '—') + '</td>'
        + '<td class="td-len">' + String(port.port || 0) + '</td>'
        + '<td class="td-time">' + (port.details || '—') + '</td>'
        + '<td><span class="badge ' + portBadgeClass(port) + '">' + (port.status || 'unknown') + '</span></td>'
        + '</tr>';
    }).join('');
  }
  document.addEventListener('click', function(ev) {
    var target = ev.target;
    if (!(target instanceof Element)) { return; }
    var restartBtn = target.closest('[data-service-restart]');
    if (restartBtn) {
      openRestartModal(restartBtn.getAttribute('data-service-restart'));
      return;
    }
    if (target.closest('[data-close-service-restart]')) {
      closeRestartModal();
      return;
    }
    if (target.id === 'service-restart-modal') {
      closeRestartModal();
    }
  });
  var confirmBtn = document.getElementById('service-restart-confirm');
  if (confirmBtn) {
    confirmBtn.addEventListener('click', function() {
      if (!selectedService) { return; }
      confirmBtn.disabled = true;
      setRestartStatus('Requesting restart…', 'flash-ok');
      fetch('/api/system/service/restart', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ unit: selectedService })
      }).then(function(resp) {
        return resp.text().then(function(text) {
          var payload = null;
          if (text) {
            try { payload = JSON.parse(text); } catch (_) {}
          }
          if (!resp.ok) {
            throw new Error((payload && payload.status) || text || ('HTTP ' + resp.status));
          }
          return payload;
        });
      }).then(function() {
        closeRestartModal();
      }).catch(function(err) {
        setRestartStatus('Error: ' + (err.message || err), 'flash-err');
      }).finally(function() {
        confirmBtn.disabled = false;
      });
    });
  }
  window.addEventListener('keydown', function(ev) {
    if (ev.key === 'Escape') {
      closeRestartModal();
    }
  });
  connect();
})();
"#;

// ── Status view ───────────────────────────────────────────────────────────────

#[component]
fn StatusView(status: GatewayStatusDto) -> impl IntoView {
    view! {
        <div class="status-grid">
            <SystemCard system=status.system/>
            <ServicesCard services=status.services/>
            <NetworkPortsCard ports=status.network_ports/>
            <VpnCard vpn_hash=status.vpn_hash serial=status.serial vpn=status.vpn/>
        </div>
        <div class="modal-backdrop" id="service-restart-modal" hidden>
            <div class="modal-card">
                <div class="modal-header">
                    <h2 class="modal-title">"Confirm service restart"</h2>
                    <button type="button" class="modal-close" data-close-service-restart>"×"</button>
                </div>
                <div class="modal-form">
                    <p class="card-body-text">
                        "Are you sure you want to restart "
                        <strong id="service-restart-unit">"service"</strong>
                        "?"
                    </p>
                    <div id="service-restart-status"></div>
                    <div class="modal-actions">
                        <button type="button" class="btn-secondary" data-close-service-restart>
                            "Cancel"
                        </button>
                        <button type="button" id="service-restart-confirm" class="btn-primary">
                            "Restart"
                        </button>
                    </div>
                </div>
            </div>
        </div>
        <h2 class="section-title">"Radio Modules"</h2>
        <div class="module-grid">
            {status.radio_modules.into_iter().enumerate().map(|(i, m)| {
                view! { <RadioModuleCard index=i module=m/> }
            }).collect_view()}
        </div>
    }
}

#[component]
fn SystemCard(system: SystemStatusDto) -> impl IntoView {
    let cpu = system.cpu_percent;
    let ram_used = system.ram_used_mb;
    let ram_total = system.ram_total_mb;
    let fs_free = system.fs_free_mb;
    let fs_total = system.fs_total_mb;
    let fs_used = fs_total.saturating_sub(fs_free);
    let os_details = system.os_details;
    let ram_pct = if ram_total > 0 {
        ram_used * 100 / ram_total
    } else {
        0
    };
    let fs_pct = if fs_total > 0 {
        (fs_total.saturating_sub(fs_free)) * 100 / fs_total
    } else {
        0
    };
    let fs_value = format_storage_mb(fs_used) + " / " + &format_storage_mb(fs_total) + " used";

    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"System"</span>
                <span class="badge badge-ok">"Live"</span>
            </div>
            <div class="metric-row">
                <span class="metric-label">"CPU"</span>
                <div class="progress-bar">
                    <div class="progress-fill" id="cpu-bar" style=format!("width:{cpu:.0}%")></div>
                </div>
                <span class="metric-value" id="cpu-pct">{format!("{cpu:.1}%")}</span>
            </div>
            <div class="metric-row">
                <span class="metric-label">"RAM"</span>
                <div class="progress-bar">
                    <div class="progress-fill" id="ram-bar" style=format!("width:{ram_pct}%")></div>
                </div>
                <span class="metric-value" id="ram-val">{format!("{ram_used} / {ram_total} MB")}</span>
            </div>
            <div class="metric-row">
                <span class="metric-label">"Disk"</span>
                <div class="progress-bar">
                    <div class="progress-fill" id="fs-bar" style=format!("width:{fs_pct}%")></div>
                </div>
                <span class="metric-value" id="fs-val">{fs_value}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"OS"</span>
                <span class="info-value" id="os-val">{os_details}</span>
            </div>
        </div>
    }
}

#[component]
fn ServicesCard(services: Vec<ServiceStatusDto>) -> impl IntoView {
    let active_count = services
        .iter()
        .filter(|service| service.load_state == "loaded" && service.active_state == "active")
        .count();

    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"Services"</span>
                <span class="badge" id="services-count">
                    {format!("{active_count}/{} active", services.len())}
                </span>
            </div>
            {services.into_iter().enumerate().map(|(i, service)| {
                let badge_class = format!("badge {}", service_badge_class(&service));
                let badge_label = service_badge_label(&service).to_string();
                view! {
                    <div class="service-row">
                        <div class="service-info">
                            <span class="service-name">{service.unit.clone()}</span>
                            <span class="service-brief-name">{service.brief_name.clone()}</span>
                            <span class="service-status-text" id=format!("service-status-{i}")>
                                {service.status.clone()}
                            </span>
                        </div>
                        <div class="service-actions">
                            <span
                                id=format!("service-badge-{i}")
                                class=badge_class
                            >
                                {badge_label}
                            </span>
                            <button
                                type="button"
                                class="btn-secondary service-restart-btn"
                                data-service-restart=service.unit.clone()
                                title="Restart service"
                                aria-label=format!("Restart {}", service.unit)
                            >
                                "↻"
                            </button>
                        </div>
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

fn service_badge_class(service: &ServiceStatusDto) -> &'static str {
    if service.load_state != "loaded" {
        "badge-err"
    } else if service.active_state == "active" {
        "badge-ok"
    } else if matches!(service.active_state.as_str(), "activating" | "reloading") {
        "badge-warn"
    } else {
        "badge-err"
    }
}

fn service_badge_label(service: &ServiceStatusDto) -> &str {
    if service.load_state != "loaded" {
        &service.load_state
    } else {
        &service.active_state
    }
}

fn format_storage_mb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

#[component]
fn VpnCard(vpn_hash: String, serial: String, vpn: VpnSnapshot) -> impl IntoView {
    let status = vpn.status.clone();
    let status_badge_class = vpn_status_badge_class(&status);
    let tunnel_ip = vpn.local_tunnel_ip.clone().unwrap_or_else(|| "—".into());
    let peer_total = vpn.peers.len();
    let peer_active = vpn
        .peers
        .iter()
        .filter(|p| p.link_state == "active")
        .count();
    let peer_summary = format!("{peer_active}/{peer_total} linked");
    let tx = format_bytes(vpn.tx_bytes);
    let rx = format_bytes(vpn.rx_bytes);

    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"VPN"</span>
                <span id="dash-vpn-status" class=status_badge_class>{status}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"Tunnel IP"</span>
                <code class="info-value" id="dash-vpn-tunnel">{tunnel_ip}</code>
            </div>
            <div class="info-row">
                <span class="info-label">"Peers"</span>
                <span class="info-value" id="dash-vpn-peers">{peer_summary}</span>
            </div>
            <div class="metric-row">
                <span class="metric-label">"TX"</span>
                <span class="metric-value" id="dash-vpn-tx">{tx}</span>
                <span class="metric-label">"RX"</span>
                <span class="metric-value" id="dash-vpn-rx">{rx}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"Serial"</span>
                <code class="info-value">{serial}</code>
            </div>
            <div class="info-row">
                <span class="info-label">"Hash"</span>
                <code class="info-value hash">{vpn_hash}</code>
            </div>
        </div>
    }
}

fn vpn_status_badge_class(status: &str) -> &'static str {
    match status {
        "running" => "badge badge-ok",
        "mock" => "badge badge-warn",
        "error" => "badge badge-err",
        _ => "badge",
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[component]
fn NetworkPortsCard(ports: Vec<NetworkPortStatusDto>) -> impl IntoView {
    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"Network Ports"</span>
                <span class="badge" id="network-ports-count">{format!("{} tracked", ports.len())}</span>
            </div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead>
                        <tr>
                            <th>"Name"</th>
                            <th>"Proto"</th>
                            <th>"Port"</th>
                            <th>"Details"</th>
                            <th>"State"</th>
                        </tr>
                    </thead>
                    <tbody id="network-ports">
                        {if ports.is_empty() {
                            view! { <tr><td colspan="5" class="frames-empty">"No network ports tracked"</td></tr> }.into_any()
                        } else {
                            ports.into_iter().map(|port| {
                                let badge_class = match port.status.as_str() {
                                    "linked" | "listening" | "reachable" | "active" => "badge badge-ok",
                                    "waiting" | "activating" | "reloading" => "badge badge-warn",
                                    _ => "badge badge-err",
                                };
                                view! {
                                    <tr>
                                        <td class="td-time">{port.name}</td>
                                        <td class="td-len">{port.protocol}</td>
                                        <td class="td-len">{port.port}</td>
                                        <td class="td-time">{port.details}</td>
                                        <td><span class=badge_class>{port.status}</span></td>
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
fn RadioModuleCard(index: usize, module: RadioModuleConfigDto) -> impl IntoView {
    use radio_common::modulation::{Modulation, OfdmBandwidthOption, OfdmMcs};
    let freq_mhz = module.radio_config.freq.as_mhz();
    let spacing_khz = module.radio_config.channel_spacing.as_khz();
    let channel = module.radio_config.channel;
    let bw = format!("{:?}", module.radio_config.bandwidth_filter);

    let (mod_name, mod_details) = match &module.modulation {
        Modulation::Off => ("Off", vec![]),
        Modulation::Fsk => ("FSK", vec![]),
        Modulation::Ofdm(o) => {
            let mcs = match o.mcs {
                OfdmMcs::BpskC1_2_4x => "BPSK 1/2 4×",
                OfdmMcs::BpskC1_2_2x => "BPSK 1/2 2×",
                OfdmMcs::QpskC1_2_2x => "QPSK 1/2 2×",
                OfdmMcs::QpskC1_2 => "QPSK 1/2",
                OfdmMcs::QpskC3_4 => "QPSK 3/4",
                OfdmMcs::QamC1_2 => "16-QAM 1/2",
                OfdmMcs::QamC3_4 => "16-QAM 3/4",
            };
            let opt = match o.opt {
                OfdmBandwidthOption::Option1 => "BW Opt 1",
                OfdmBandwidthOption::Option2 => "BW Opt 2",
                OfdmBandwidthOption::Option3 => "BW Opt 3",
                OfdmBandwidthOption::Option4 => "BW Opt 4",
            };
            (
                "OFDM",
                vec![
                    ("MCS", mcs.to_string()),
                    ("Bandwidth", opt.to_string()),
                    ("PDT", o.pdt.to_string()),
                    ("TX Power", format!("{} dBm", o.tx_power)),
                ],
            )
        }
        Modulation::Qpsk(q) => {
            use radio_common::modulation::{QpskChipFrequency, QpskRateMode};
            let fchip = match q.fchip {
                QpskChipFrequency::Fchip100 => "100 kchip/s",
                QpskChipFrequency::Fchip200 => "200 kchip/s",
                QpskChipFrequency::Fchip1000 => "1000 kchip/s",
                QpskChipFrequency::Fchip2000 => "2000 kchip/s",
            };
            let mode = match q.mode {
                QpskRateMode::RateMode0 => "Mode 0",
                QpskRateMode::RateMode1 => "Mode 1",
                QpskRateMode::RateMode2 => "Mode 2",
                QpskRateMode::RateMode3 => "Mode 3",
                QpskRateMode::RateMode4 => "Mode 4",
            };
            (
                "QPSK",
                vec![
                    ("Chip Rate", fchip.to_string()),
                    ("Rate Mode", mode.to_string()),
                    ("TX Power", format!("{} dBm", q.tx_power)),
                ],
            )
        }
    };

    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">{radio_label(index)}</span>
                <span class="badge badge-ok">{mod_name}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"Frequency"</span>
                <span class="info-value">{format!("{freq_mhz} MHz")}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"Channel"</span>
                <span class="info-value">{channel.to_string()}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"Spacing"</span>
                <span class="info-value">{format!("{spacing_khz} kHz")}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"Bandwidth"</span>
                <span class="info-value">{bw}</span>
            </div>
            {mod_details.into_iter().map(|(label, val)| view! {
                <div class="info-row mod-detail">
                    <span class="info-label">{label}</span>
                    <span class="info-value">{val}</span>
                </div>
            }).collect_view()}
        </div>
    }
}
