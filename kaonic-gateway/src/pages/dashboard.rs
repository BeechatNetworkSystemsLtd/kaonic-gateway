use leptos::prelude::*;

use crate::app_types::{
    AtakBridgeStatusDto, GatewayStatusDto, RadioModuleConfigDto, SystemStatusDto,
};
use crate::system_metrics::{read_cpu_percent_async, read_fs_mb, read_mem_mb, read_os_details};

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
    use crate::app_types::{AtakBridgeStatusDto, RadioModuleConfigDto};
    use crate::state::AppState;
    use std::sync::atomic::Ordering;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;

    let atak_bridges = state
        .atak_metrics
        .iter()
        .map(|m| AtakBridgeStatusDto {
            port: m.port,
            dest_hash: m.dest_hash.get().cloned().unwrap_or_default(),
            rx_packets: m.rx_packets.load(Ordering::Relaxed),
            tx_packets: m.tx_packets.load(Ordering::Relaxed),
        })
        .collect();

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

    Ok(GatewayStatusDto {
        serial: state.serial.clone(),
        vpn_hash: state.vpn_hash.clone(),
        atak_bridges,
        system,
        radio_modules,
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
            <h1 class="page-title">"Dashboard"</h1>
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
  function connect() {
    var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    var ws = new WebSocket(proto + '//' + location.host + '/api/ws/status');
    ws.onmessage = function(e) {
      try {
        var s = JSON.parse(e.data);
        var sys = s.system || {};
        var cpu = (sys.cpu_percent || 0);
        var ramUsed = sys.ram_used_mb || 0;
        var ramTotal = sys.ram_total_mb || 0;
        var ramPct = ramTotal > 0 ? Math.round(ramUsed * 100 / ramTotal) : 0;
        var fsFree = sys.fs_free_mb || 0;
        var fsTotal = sys.fs_total_mb || 0;
        var fsPct = fsTotal > 0 ? Math.round((fsTotal - fsFree) * 100 / fsTotal) : 0;
        set('os-val', sys.os_details || 'Unknown');
        set('cpu-pct', cpu.toFixed(1) + '%');
        bar('cpu-bar', cpu.toFixed(0));
        set('ram-val', ramUsed + ' / ' + ramTotal + ' MB');
        bar('ram-bar', ramPct);
        set('fs-val', formatStorageMb(fsFree) + ' / ' + formatStorageMb(fsTotal) + ' free');
        bar('fs-bar', fsPct);
        (s.atak_bridges || []).forEach(function(b, i) {
          set('bridge-rx-' + i, '\u2193 ' + b.rx_packets);
          set('bridge-tx-' + i, '\u2191 ' + b.tx_packets);
          var badge = document.getElementById('bridge-badge-' + i);
          if (badge) {
            badge.textContent = b.dest_hash ? 'linked' : 'waiting';
            badge.className = b.dest_hash ? 'badge badge-ok' : 'badge badge-warn';
          }
        });
      } catch(err) { console.warn('ws parse error', err); }
    };
    ws.onclose = function() { setTimeout(connect, 3000); };
    ws.onerror = function() { ws.close(); };
  }
  function set(id, val) { var el = document.getElementById(id); if (el) el.textContent = val; }
  function bar(id, pct) { var el = document.getElementById(id); if (el) el.style.width = pct + '%'; }
  function formatStorageMb(mb) {
    return mb >= 1024 ? (mb / 1024).toFixed(1) + ' GB' : mb + ' MB';
  }
  connect();
})();
"#;

// ── Status view ───────────────────────────────────────────────────────────────

#[component]
fn StatusView(status: GatewayStatusDto) -> impl IntoView {
    view! {
        <div class="status-grid">
            <SystemCard system=status.system/>
            <VpnCard vpn_hash=status.vpn_hash serial=status.serial/>
            <AtakCard bridges=status.atak_bridges/>
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
    let fs_value = format_storage_mb(fs_free) + " / " + &format_storage_mb(fs_total) + " free";

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

fn format_storage_mb(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.0)
    } else {
        format!("{mb} MB")
    }
}

#[component]
fn VpnCard(vpn_hash: String, serial: String) -> impl IntoView {
    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"Identity"</span>
            </div>
            <div class="info-row">
                <span class="info-label">"Serial"</span>
                <code class="info-value">{serial}</code>
            </div>
            <div class="info-row">
                <span class="info-label">"VPN Hash"</span>
                <code class="info-value hash">{vpn_hash}</code>
            </div>
        </div>
    }
}

#[component]
fn AtakCard(bridges: Vec<AtakBridgeStatusDto>) -> impl IntoView {
    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">"ATAK Bridges"</span>
                <span class="badge">{bridges.len()}" active"</span>
            </div>
            {bridges.into_iter().enumerate().map(|(i, b)| {
                let active = !b.dest_hash.is_empty();
                view! {
                    <div class="bridge-row">
                        <div class="bridge-info">
                            <span class="bridge-port">"Port "{b.port}</span>
                            <code class="bridge-hash">{b.dest_hash}</code>
                        </div>
                        <div class="bridge-stats">
                            <span class="stat-rx" id=format!("bridge-rx-{i}")>"↓ "{b.rx_packets}</span>
                            <span class="stat-tx" id=format!("bridge-tx-{i}")>"↑ "{b.tx_packets}</span>
                            <span id=format!("bridge-badge-{i}")
                                class=if active { "badge badge-ok" } else { "badge badge-warn" }>
                                {if active { "linked" } else { "waiting" }}
                            </span>
                        </div>
                    </div>
                }
            }).collect_view()}
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
