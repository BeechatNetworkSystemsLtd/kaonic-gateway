use leptos::prelude::*;

use kaonic_vpn::VpnSnapshot;

use super::PageTitle;
use crate::app_types::{
    GatewayStatusDto, NetworkPortStatusDto, RadioModuleConfigDto, ServiceStatusDto, SystemStatusDto,
};
use crate::system_metrics::{
    read_cpu_freq_mhz, read_cpu_percent_async, read_fs_mb, read_gateway_services, read_mem_mb,
    read_os_details,
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
    let services = read_gateway_services().await;
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
        cpu_freq_mhz: read_cpu_freq_mhz(),
        ram_used_mb,
        ram_total_mb,
        fs_free_mb,
        fs_total_mb,
        os_details,
        uptime_secs: crate::system_metrics::read_uptime_secs(),
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

  // btop-style mirrored traffic graph per radio module: RX grows up from the
  // baseline, TX grows down. Rates are derived client-side from the byte
  // counters at 1 Hz so idle periods really read as zero.
  var traffic = (function() {
    var HISTORY = 90;
    var mods = [newModule(0), newModule(1)];
    function newModule(i) {
      return { index: i, stats: null, last: null, lastTs: 0, rx: [], tx: [], rssi: [], peakRx: 0, peakTx: 0 };
    }
    function fmtRate(bps) {
      if (bps >= 1048576) { return (bps / 1048576).toFixed(2) + ' MB/s'; }
      if (bps >= 1024) { return (bps / 1024).toFixed(1) + ' KB/s'; }
      return Math.round(bps) + ' B/s';
    }
    function fmtBytes(n) {
      n = n || 0;
      if (n >= 1073741824) { return (n / 1073741824).toFixed(2) + ' GB'; }
      if (n >= 1048576) { return (n / 1048576).toFixed(1) + ' MB'; }
      if (n >= 1024) { return (n / 1024).toFixed(1) + ' KB'; }
      return n + ' B';
    }
    function update(index, stats) {
      var m = mods[index === 1 ? 1 : 0];
      m.stats = stats;
      set('traffic-rx-frames-' + m.index, String(stats.rx_frames || 0));
      set('traffic-tx-frames-' + m.index, String(stats.tx_frames || 0));
      set('traffic-rx-bytes-' + m.index, fmtBytes(stats.rx_bytes));
      set('traffic-tx-bytes-' + m.index, fmtBytes(stats.tx_bytes));
      set('traffic-rssi-' + m.index, stats.last_rssi == null ? '—' : stats.last_rssi + ' dBm');
    }
    function sample() {
      var now = Date.now();
      mods.forEach(function(m) {
        var rx = 0, tx = 0;
        if (m.stats && m.last) {
          var dt = Math.max(0.25, (now - m.lastTs) / 1000);
          rx = Math.max(0, (m.stats.rx_bytes - m.last.rx_bytes)) / dt;
          tx = Math.max(0, (m.stats.tx_bytes - m.last.tx_bytes)) / dt;
        }
        if (m.stats) { m.last = { rx_bytes: m.stats.rx_bytes || 0, tx_bytes: m.stats.tx_bytes || 0 }; m.lastTs = now; }
        m.rx.push(rx); m.tx.push(tx);
        // Sampled on the same tick as the rates, so a column in the strip is
        // the same moment as the column above it.
        m.rssi.push(m.stats && m.stats.last_rssi != null ? m.stats.last_rssi : null);
        if (m.rx.length > HISTORY) { m.rx.shift(); m.tx.shift(); m.rssi.shift(); }
        m.peakRx = Math.max(m.peakRx, rx); m.peakTx = Math.max(m.peakTx, tx);
        draw(m, rx, tx);
      });
    }
    function draw(m, rx, tx) {
      var svg = document.getElementById('traffic-chart-' + m.index);
      if (!svg) { return; }
      var W = 600, H = 160, mid = H / 2, pad = 6;
      var max = 1;
      for (var i = 0; i < m.rx.length; i++) { max = Math.max(max, m.rx[i], m.tx[i]); }
      // Round the scale up to a friendly step so the graph does not jitter.
      var step = Math.pow(2, Math.ceil(Math.log(max) / Math.log(2)));
      var scale = (mid - pad) / step;
      function path(series, dir) {
        var n = HISTORY, d = '', x0 = 0;
        var start = n - series.length;
        d += 'M' + ((start / (n - 1)) * W).toFixed(1) + ',' + mid;
        for (var i = 0; i < series.length; i++) {
          var x = ((start + i) / (n - 1)) * W;
          var y = mid - dir * series[i] * scale;
          d += 'L' + x.toFixed(1) + ',' + y.toFixed(1);
          x0 = x;
        }
        d += 'L' + x0.toFixed(1) + ',' + mid + 'Z';
        return d;
      }
      var grid = '';
      for (var g = 1; g <= 3; g++) {
        var gy = (mid - pad) * g / 4;
        grid += '<line class="tg-grid" x1="0" x2="' + W + '" y1="' + (mid - gy).toFixed(1) + '" y2="' + (mid - gy).toFixed(1) + '"/>';
        grid += '<line class="tg-grid" x1="0" x2="' + W + '" y1="' + (mid + gy).toFixed(1) + '" y2="' + (mid + gy).toFixed(1) + '"/>';
      }
      svg.innerHTML =
        '<defs><linearGradient id="tg-rx-' + m.index + '" x1="0" y1="0" x2="0" y2="1">' +
        '<stop offset="0%" stop-color="rgba(52,208,88,.85)"/><stop offset="100%" stop-color="rgba(52,208,88,.08)"/></linearGradient>' +
        '<linearGradient id="tg-tx-' + m.index + '" x1="0" y1="0" x2="0" y2="1">' +
        '<stop offset="0%" stop-color="rgba(13,203,240,.08)"/><stop offset="100%" stop-color="rgba(13,203,240,.85)"/></linearGradient></defs>' +
        grid +
        '<path class="tg-area tg-rx" fill="url(#tg-rx-' + m.index + ')" d="' + path(m.rx, 1) + '"/>' +
        '<path class="tg-area tg-tx" fill="url(#tg-tx-' + m.index + ')" d="' + path(m.tx, -1) + '"/>' +
        '<line class="tg-mid" x1="0" x2="' + W + '" y1="' + mid + '" y2="' + mid + '"/>' +
        '<text class="tg-scale" x="4" y="12">' + fmtRate(step) + '</text>' +
        '<text class="tg-scale" x="4" y="' + (H - 4) + '">' + fmtRate(step) + '</text>';
      drawRssi(m);
      set('traffic-rx-rate-' + m.index, fmtRate(rx));
      set('traffic-tx-rate-' + m.index, fmtRate(tx));
      set('traffic-rx-peak-' + m.index, 'peak ' + fmtRate(m.peakRx));
      set('traffic-tx-peak-' + m.index, 'peak ' + fmtRate(m.peakTx));
    }
    // Signal quality bands. Chosen for what an operator does about them, not
    // for even spacing: above -70 nothing needs doing, below -95 a link is
    // about to stop working.
    function rssiColor(dbm) {
      if (dbm == null) { return 'rgba(148,163,184,.18)'; }
      if (dbm >= -70) { return '#34d058'; }
      if (dbm >= -80) { return '#a3d94a'; }
      if (dbm >= -88) { return '#f2c744'; }
      if (dbm >= -95) { return '#f08a3c'; }
      return '#f04b4b';
    }

    // One column per sample, aligned with the chart above. No text: the strip
    // is stretched to the card width, which distorts glyphs but not blocks.
    function drawRssi(m) {
      var svg = document.getElementById('rssi-strip-' + m.index);
      if (!svg) { return; }
      var W = 600, H = 20;
      var colW = W / (HISTORY - 1);
      var start = HISTORY - m.rssi.length;
      var out = '', last = null;
      for (var i = 0; i < m.rssi.length; i++) {
        var v = m.rssi[i];
        if (v != null) { last = v; }
        out += '<rect x="' + ((start + i) * colW).toFixed(2) + '" y="0" width="' +
               (colW + 0.6).toFixed(2) + '" height="' + H + '" fill="' + rssiColor(v) + '"/>';
      }
      svg.innerHTML = out;
      set('traffic-rssi-strip-' + m.index, last == null ? 'no signal' : last + ' dBm');
    }

    setInterval(sample, 1000);
    return { update: update };
  })();
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
          set('cpu-freq-val', sys.cpu_freq_mhz ? sys.cpu_freq_mhz + ' MHz' : '—');
          bar('cpu-bar', cpu.toFixed(0));
          set('ram-val', ramUsed + ' / ' + ramTotal + ' MB');
          bar('ram-bar', ramPct);
          set('fs-val', formatStorageMb(fsUsed) + ' / ' + formatStorageMb(fsTotal) + ' used');
          bar('fs-bar', fsPct);
          sysChart.update(sys);
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
          set('dash-vpn-tx-pkts', (vpn.tx_packets || 0) + ' pkts');
          set('dash-vpn-rx-pkts', (vpn.rx_packets || 0) + ' pkts');
          return;
        }
        if (msg.type === 'radio_frames') {
          traffic.update(data.module || 0, data.stats || {});
          return;
        }
        if (msg.type === 'remote') {
          remoteMap.render(data);
          return;
        }
      } catch(err) { console.warn('ws parse error', err); }
    };
    ws.onclose = function() { setTimeout(connect, 3000); };
    ws.onerror = function() { ws.close(); };
  }
  // btop-style CPU/RAM history: CPU grows up from the baseline, RAM down.
  var sysChart = (function() {
    var HISTORY = 90, cpu = [], ram = [], last = null;
    function fmtMb(mb) { return mb >= 1024 ? (mb / 1024).toFixed(1) + ' GB' : mb + ' MB'; }
    function update(sys) { last = sys; }
    function draw() {
      var svg = document.getElementById('sys-chart');
      if (!svg) { return; }
      if (last) {
        cpu.push(Math.max(0, Math.min(100, last.cpu_percent || 0)));
        ram.push(last.ram_total_mb > 0 ? (last.ram_used_mb * 100 / last.ram_total_mb) : 0);
      } else { cpu.push(0); ram.push(0); }
      if (cpu.length > HISTORY) { cpu.shift(); ram.shift(); }
      var W = 600, H = 160, mid = H / 2, pad = 6, scale = (mid - pad) / 100;
      function path(series, dir) {
        var n = HISTORY, start = n - series.length, x0 = 0;
        var d = 'M' + ((start / (n - 1)) * W).toFixed(1) + ',' + mid;
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
        grid += '<line class="tg-grid" x1="0" x2="' + W + '" y1="' + (mid - gy).toFixed(1) + '" y2="' + (mid - gy).toFixed(1) + '"/>'
             +  '<line class="tg-grid" x1="0" x2="' + W + '" y1="' + (mid + gy).toFixed(1) + '" y2="' + (mid + gy).toFixed(1) + '"/>';
      }
      svg.innerHTML =
        '<defs><linearGradient id="tg-cpu" x1="0" y1="0" x2="0" y2="1">' +
        '<stop offset="0%" stop-color="rgba(52,208,88,.85)"/><stop offset="100%" stop-color="rgba(52,208,88,.08)"/></linearGradient>' +
        '<linearGradient id="tg-ram" x1="0" y1="0" x2="0" y2="1">' +
        '<stop offset="0%" stop-color="rgba(13,203,240,.08)"/><stop offset="100%" stop-color="rgba(13,203,240,.85)"/></linearGradient></defs>' +
        grid +
        '<path class="tg-area tg-rx" fill="url(#tg-cpu)" d="' + path(cpu, 1) + '"/>' +
        '<path class="tg-area tg-tx" fill="url(#tg-ram)" d="' + path(ram, -1) + '"/>' +
        '<line class="tg-mid" x1="0" x2="' + W + '" y1="' + mid + '" y2="' + mid + '"/>' +
        '<text class="tg-scale" x="4" y="12">100%</text>' +
        '<text class="tg-scale" x="4" y="' + (H - 4) + '">100%</text>';
      if (last) {
        set('sys-chart-cpu', (last.cpu_percent || 0).toFixed(1) + '%');
        set('sys-chart-ram', Math.round(ram[ram.length - 1]) + '%');
        set('sys-chart-ram-mb', fmtMb(last.ram_used_mb || 0) + ' / ' + fmtMb(last.ram_total_mb || 0));
        set('sys-chart-freq', last.cpu_freq_mhz ? last.cpu_freq_mhz + ' MHz' : '—');
        var used = Math.max(0, (last.fs_total_mb || 0) - (last.fs_free_mb || 0));
        set('sys-chart-fs', fmtMb(used) + ' / ' + fmtMb(last.fs_total_mb || 0));
        if (last.os_details) { set('sys-chart-os', last.os_details); }
        if (last.uptime_secs != null) {
          var u = last.uptime_secs;
          set('sys-chart-uptime', u >= 86400 ? Math.floor(u / 86400) + 'd ' + Math.floor(u % 86400 / 3600) + 'h'
            : u >= 3600 ? Math.floor(u / 3600) + 'h ' + Math.floor(u % 3600 / 60) + 'm'
            : Math.floor(u / 60) + 'm');
        }
      }
    }
    setInterval(draw, 1000);
    return { update: update };
  })();

  // Read-only mirror of the Remote page map.
  var remoteMap = (function() {
    function rssiNorm(r) { return r == null ? 0.5 : Math.max(0, Math.min(1, (r + 100) / 70)); }
    function render(snap) {
      var svg = document.getElementById('dash-remote-map');
      if (!svg || !snap || !snap.local) { return; }
      var W = 640, H = 520, cx = W / 2, cy = H / 2;
      var nodes = snap.nodes || [];
      var maxHops = 1, unknown = false;
      nodes.forEach(function(n) { if (n.hops != null) { maxHops = Math.max(maxHops, n.hops); } else { unknown = true; } });
      var rings = Math.min(5, Math.max(2, maxHops + (unknown ? 1 : 0)));
      var outer = Math.min(W, H) / 2 - 40, step = outer / rings, out = [];
      out.push('<defs><radialGradient id="dm-glow"><stop offset="0%" stop-color="rgba(13,203,240,.30)"/><stop offset="100%" stop-color="rgba(13,203,240,0)"/></radialGradient></defs>');
      out.push('<circle cx="' + cx + '" cy="' + cy + '" r="' + (outer + 20) + '" fill="url(#dm-glow)"/>');
      for (var r = 1; r <= rings; r++) {
        out.push('<circle class="rm-ring" cx="' + cx + '" cy="' + cy + '" r="' + (r * step) + '"/>');
      }
      var groups = {};
      nodes.forEach(function(n) {
        var level = n.hops == null ? rings : Math.min(n.hops, rings);
        (groups[level] = groups[level] || []).push(n);
      });
      Object.keys(groups).forEach(function(level) {
        var list = groups[level], count = list.length;
        list.forEach(function(n, i) {
          var lo = (level - 1) * step + (Number(level) === 1 ? 62 : 18);
          var hi = Math.max(lo + 4, level * step - 18);
          var rad = lo + (1 - rssiNorm(n.rssi)) * (hi - lo);
          var angle = -Math.PI / 2 + (i / count) * Math.PI * 2 + (count > 1 ? Math.PI / count / 2 : 0);
          var x = cx + Math.cos(angle) * rad, y = cy + Math.sin(angle) * rad;
          var cls = 'rm-node' + (n.online ? ' online' : ' offline') + (n.paired ? ' paired' : '') + (n.link === 'active' ? ' linked' : '');
          out.push('<g class="' + cls + '" transform="translate(' + x.toFixed(1) + ',' + y.toFixed(1) + ')">');
          out.push('<line class="rm-spoke" x1="0" y1="0" x2="' + (cx - x).toFixed(1) + '" y2="' + (cy - y).toFixed(1) + '"/>');
          out.push('<circle class="rm-dot" r="11"/>');
          if (n.paired) { out.push('<circle class="rm-paired-ring" r="15"/>'); }
          out.push('<text class="rm-label" y="29">' + escapeHtml(n.codename) + '</text></g>');
        });
      });
      out.push('<g class="rm-self" transform="translate(' + cx + ',' + cy + ')">');
      out.push('<circle class="rm-self-halo" r="28"/><circle class="rm-self-dot" r="15"/>');
      out.push('<text class="rm-label rm-self-label" y="36">' + escapeHtml(snap.local.codename || '') + '</text></g>');
      svg.innerHTML = out.join('');
      var online = nodes.filter(function(n) { return n.online; }).length;
      var paired = nodes.filter(function(n) { return n.paired; }).length;
      set('dash-remote-summary', nodes.length
        ? online + ' of ' + nodes.length + ' online · ' + paired + ' paired'
        : 'No nodes announced yet');
    }
    return { render: render };
  })();

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
  function escapeHtml(value) {
    return String(value == null ? '' : value)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }
  function appIconUrl(plugin) {
    if (!plugin || !plugin.icon) { return ''; }
    return '/api/plugins/' + encodeURIComponent(plugin.id) + '/icon?v='
      + encodeURIComponent(plugin.updated_at || plugin.version || '');
  }
  function appWebviewUrl(plugin) {
    var port = plugin && plugin.webview;
    if (port == null || port === '') { return ''; }
    var numericPort = Number(port);
    if (!Number.isInteger(numericPort) || numericPort < 1 || numericPort > 65535) { return ''; }
    var host = window.location.hostname || window.location.host || '';
    if (!host) { return ''; }
    if (host.includes(':') && host.charAt(0) !== '[') { host = '[' + host + ']'; }
    var scheme = plugin && plugin.tls ? 'https://' : 'http://';
    return scheme + host + ':' + numericPort;
  }
  function renderApps(plugins) {
    var grid = document.getElementById('dashboard-apps');
    if (!grid) { return; }
    var apps = (plugins || []).filter(function(p) { return !!appWebviewUrl(p); });
    var addTile = '<a class="app-tile app-tile--add" href="/plugins" title="Manage plugins">'
      + '<span class="app-tile-icon app-tile-icon--add" aria-hidden="true">+</span>'
      + '<span class="app-tile-label">Add app</span></a>';
    if (apps.length === 0) {
      grid.innerHTML = addTile;
      return;
    }
    grid.innerHTML = apps.map(function(p) {
      var url = appWebviewUrl(p);
      var icon = appIconUrl(p);
      var iconHtml = icon
        ? '<img class="app-tile-icon" src="' + escapeHtml(icon) + '" alt="" loading="lazy">'
        : '<span class="app-tile-icon app-tile-icon--empty" aria-hidden="true">🧩</span>';
      return '<a class="app-tile" href="' + escapeHtml(url) + '" target="_blank" rel="noreferrer">'
        + iconHtml
        + '<span class="app-tile-label">' + escapeHtml(p.name || p.id || 'App') + '</span>'
        + '</a>';
    }).join('') + addTile;
  }
  function loadApps() {
    fetch('/api/plugins').then(function(resp) {
      return resp.ok ? resp.json() : [];
    }).then(renderApps).catch(function(err) { console.warn('failed to load apps', err); });
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
  // The apps grid lives inside a Suspense boundary that streams in after this
  // script tag runs, so wait for the full document (all streamed chunks) to
  // land before querying for it.
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', loadApps);
  } else {
    loadApps();
  }
  fetch('/api/remote/snapshot')
    .then(function(r) { return r.ok ? r.json() : null; })
    .then(function(d) { if (d) { remoteMap.render(d); } })
    .catch(function() {});
})();
"#;

// ── Status view ───────────────────────────────────────────────────────────────

#[component]
fn StatusView(status: GatewayStatusDto) -> impl IntoView {
    let mut modules = status.radio_modules.clone().into_iter();
    let radio_a = modules.next().unwrap_or_default();
    let radio_b = modules.next().unwrap_or_default();
    view! {
        <div class="traffic-grid traffic-grid--three">
            <RadioTrafficCard index=0 module=radio_a/>
            <RadioTrafficCard index=1 module=radio_b/>
            <SystemChartCard system=status.system/>
        </div>
        <div class="apps-section">
            <h2 class="section-title">"Apps"</h2>
            <div class="apps-grid" id="dashboard-apps"></div>
        </div>
        <div class="dash-grid">
            <VpnCard vpn_hash=status.vpn_hash serial=status.serial vpn=status.vpn/>
            <RemoteMapCard/>
        </div>
        <div class="dash-grid">
            <ServicesCard services=status.services/>
            <NetworkPortsCard ports=status.network_ports/>
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
    }
}

#[component]
fn SystemCard(system: SystemStatusDto) -> impl IntoView {
    let cpu = system.cpu_percent;
    let cpu_freq = system.cpu_freq_mhz;
    let cpu_freq_text = if cpu_freq > 0 {
        format!("{cpu_freq} MHz")
    } else {
        "—".to_string()
    };
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
            <div class="info-row">
                <span class="info-label">"CPU Clock"</span>
                <span class="info-value" id="cpu-freq-val">{cpu_freq_text}</span>
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
    let tx_packets = format!("{} pkts", vpn.tx_packets);
    let rx_packets = format!("{} pkts", vpn.rx_packets);

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
            <div class="metric-row">
                <span class="metric-label">"TX pkts"</span>
                <span class="metric-value" id="dash-vpn-tx-pkts">{tx_packets}</span>
                <span class="metric-label">"RX pkts"</span>
                <span class="metric-value" id="dash-vpn-rx-pkts">{rx_packets}</span>
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
fn RadioTrafficCard(index: usize, module: RadioModuleConfigDto) -> impl IntoView {
    let idx = index.to_string();
    let id = |prefix: &str| format!("{prefix}-{idx}");
    let (mod_name, mod_summary) = modulation_summary(&module);
    let freq = format!("{:.3} MHz", module.radio_config.freq.as_mhz());
    let channel = format!(
        "ch {} · {:.0} kHz",
        module.radio_config.channel,
        module.radio_config.channel_spacing.as_khz()
    );
    view! {
        <div class="card traffic-card">
            <div class="card-header">
                <span class="card-title">{radio_label(index)}</span>
                <span class="badge badge-ok">{mod_name}</span>
            </div>
            <div class="traffic-config">
                <span class="traffic-config-freq">{freq}</span>
                <span class="traffic-config-sep">"·"</span>
                <span>{channel}</span>
                <span class="traffic-config-sep">"·"</span>
                <span>{mod_summary}</span>
                <span class="traffic-rssi" id=id("traffic-rssi")>"—"</span>
            </div>
            <div class="traffic-legend">
                <div class="traffic-legend-item rx">
                    <span class="traffic-arrow">"▲"</span>
                    <span class="traffic-legend-label">"RX"</span>
                    <span class="traffic-rate" id=id("traffic-rx-rate")>"0 B/s"</span>
                    <span class="traffic-peak" id=id("traffic-rx-peak")>"peak 0 B/s"</span>
                </div>
                <div class="traffic-legend-item tx">
                    <span class="traffic-arrow">"▼"</span>
                    <span class="traffic-legend-label">"TX"</span>
                    <span class="traffic-rate" id=id("traffic-tx-rate")>"0 B/s"</span>
                    <span class="traffic-peak" id=id("traffic-tx-peak")>"peak 0 B/s"</span>
                </div>
            </div>
            <svg class="traffic-chart" id=id("traffic-chart") viewBox="0 0 600 160" preserveAspectRatio="none"></svg>
            <div class="rssi-strip-row">
                <span class="rssi-strip-label">"Signal"</span>
                <svg class="rssi-strip" id=id("rssi-strip") viewBox="0 0 600 20" preserveAspectRatio="none"></svg>
                <span class="rssi-strip-value" id=id("traffic-rssi-strip")>"no signal"</span>
            </div>
            <div class="traffic-counters">
                <div class="traffic-counter">
                    <span class="metric-label">"RX frames"</span>
                    <span class="metric-value stat-rx" id=id("traffic-rx-frames")>"0"</span>
                    <span class="traffic-sub" id=id("traffic-rx-bytes")>"0 B"</span>
                </div>
                <div class="traffic-counter">
                    <span class="metric-label">"TX frames"</span>
                    <span class="metric-value stat-tx" id=id("traffic-tx-frames")>"0"</span>
                    <span class="traffic-sub" id=id("traffic-tx-bytes")>"0 B"</span>
                </div>
            </div>
        </div>
    }
}

/// Short human summary of a module's modulation, for the traffic card header.
fn modulation_summary(module: &RadioModuleConfigDto) -> (&'static str, String) {
    use radio_common::modulation::{
        Modulation, OfdmBandwidthOption, OfdmMcs, QpskChipFrequency, QpskRateMode,
    };
    match &module.modulation {
        Modulation::Off => ("OFF", "disabled".into()),
        Modulation::Fsk => ("FSK", "fsk".into()),
        Modulation::Ofdm(o) => {
            let mcs = match o.mcs {
                OfdmMcs::BpskC1_2_4x => "BPSK ½ 4×",
                OfdmMcs::BpskC1_2_2x => "BPSK ½ 2×",
                OfdmMcs::QpskC1_2_2x => "QPSK ½ 2×",
                OfdmMcs::QpskC1_2 => "QPSK ½",
                OfdmMcs::QpskC3_4 => "QPSK ¾",
                OfdmMcs::QamC1_2 => "16-QAM ½",
                OfdmMcs::QamC3_4 => "16-QAM ¾",
            };
            let opt = match o.opt {
                OfdmBandwidthOption::Option1 => 1,
                OfdmBandwidthOption::Option2 => 2,
                OfdmBandwidthOption::Option3 => 3,
                OfdmBandwidthOption::Option4 => 4,
            };
            ("OFDM", format!("{mcs} · BW{opt} · {} dBm", o.tx_power))
        }
        Modulation::Qpsk(q) => {
            let fchip = match q.fchip {
                QpskChipFrequency::Fchip100 => "100 kchip/s",
                QpskChipFrequency::Fchip200 => "200 kchip/s",
                QpskChipFrequency::Fchip1000 => "1 Mchip/s",
                QpskChipFrequency::Fchip2000 => "2 Mchip/s",
            };
            let mode = match q.mode {
                QpskRateMode::RateMode0 => 0,
                QpskRateMode::RateMode1 => 1,
                QpskRateMode::RateMode2 => 2,
                QpskRateMode::RateMode3 => 3,
                QpskRateMode::RateMode4 => 4,
            };
            ("QPSK", format!("{fchip} · mode {mode} · {} dBm", q.tx_power))
        }
    }
}

/// btop-style CPU/RAM history chart, fed by the same WS system events.
#[component]
fn SystemChartCard(system: SystemStatusDto) -> impl IntoView {
    let cpu = system.cpu_percent;
    let ram_pct = if system.ram_total_mb > 0 {
        system.ram_used_mb * 100 / system.ram_total_mb
    } else {
        0
    };
    view! {
        <div class="card traffic-card">
            <div class="card-header">
                <span class="card-title">"System"</span>
                <span class="traffic-rssi" id="sys-chart-freq">
                    {if system.cpu_freq_mhz > 0 { format!("{} MHz", system.cpu_freq_mhz) } else { "—".into() }}
                </span>
            </div>
            <div class="traffic-legend">
                <div class="traffic-legend-item rx">
                    <span class="traffic-arrow">"▲"</span>
                    <span class="traffic-legend-label">"CPU"</span>
                    <span class="traffic-rate" id="sys-chart-cpu">{format!("{cpu:.1}%")}</span>
                </div>
                <div class="traffic-legend-item tx">
                    <span class="traffic-arrow">"▼"</span>
                    <span class="traffic-legend-label">"RAM"</span>
                    <span class="traffic-rate" id="sys-chart-ram">{format!("{ram_pct}%")}</span>
                    <span class="traffic-peak" id="sys-chart-ram-mb">
                        {format!("{} / {} MB", system.ram_used_mb, system.ram_total_mb)}
                    </span>
                </div>
            </div>
            <svg class="traffic-chart" id="sys-chart" viewBox="0 0 600 160" preserveAspectRatio="none"></svg>
            <div class="traffic-counters">
                <div class="traffic-counter">
                    <span class="metric-label">"Storage"</span>
                    <span class="metric-value" id="sys-chart-fs">"—"</span>
                    <span class="traffic-sub" id="sys-chart-os">{system.os_details.clone()}</span>
                </div>
                <div class="traffic-counter">
                    <span class="metric-label">"Uptime"</span>
                    <span class="metric-value" id="sys-chart-uptime">"—"</span>
                    <span class="traffic-sub">"since boot"</span>
                </div>
            </div>
        </div>
    }
}

/// Read-only mirror of the Remote node map.
#[component]
fn RemoteMapCard() -> impl IntoView {
    view! {
        <div class="card remote-map-card">
            <div class="card-header">
                <span class="card-title">"Remote nodes"</span>
                <a class="dash-card-link" href="/remote">"Open"</a>
            </div>
            <svg id="dash-remote-map" class="remote-map remote-map--compact" viewBox="0 0 640 520" preserveAspectRatio="xMidYMid meet"></svg>
            <div class="card-body-text" id="dash-remote-summary">"Loading…"</div>
        </div>
    }
}

#[component]
fn RadioModuleCard(index: usize, module: RadioModuleConfigDto) -> impl IntoView {
    use radio_common::modulation::{Modulation, OfdmBandwidthOption, OfdmMcs};
    let freq_mhz = module.radio_config.freq.as_mhz();
    let spacing_khz = module.radio_config.channel_spacing.as_khz();
    let accel_label = match module.accelerator {
        radio_common::Accelerator::Native => "Native",
        radio_common::Accelerator::Hardware => "Hardware (FPGA)",
    };
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
            <div class="info-row">
                <span class="info-label">"Acceleration"</span>
                <span class="info-value">{accel_label}</span>
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
