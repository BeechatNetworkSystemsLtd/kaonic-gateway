use leptos::prelude::*;

use super::PageTitle;
use kaonic_remote::RemoteSnapshot;

#[server]
pub async fn load_remote_snapshot() -> Result<Option<RemoteSnapshot>, ServerFnError> {
    use crate::state::AppState;

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;
    let Some(remote) = state.remote.as_ref() else {
        return Ok(None);
    };
    let mut snapshot = remote.snapshot();
    crate::remote::enrich_snapshot(&state, &mut snapshot).await;
    Ok(Some(snapshot))
}

const REMOTE_JS: &str = r##"
(function() {
    var state = { snapshot: null, selected: null, tab: 'overview', radio: {}, plugins: null, info: null, rtts: [], jobRates: {}, media: null, shell: {}, shellBusy: {}, shellHistory: [], shellHistoryPos: 0, termFull: false, hovered: null, mapPositions: {}, mapCenter: null };
    var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';

    function $(id) { return document.getElementById(id); }
    function esc(v) {
        return String(v == null ? '' : v)
            .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
            .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
    }
    function shortHash(h) { h = String(h || ''); return h ? h.slice(0, 8) + '…' + h.slice(-4) : '—'; }
    function ago(ts) {
        if (!ts) { return 'never'; }
        var d = Math.max(0, Math.floor(Date.now() / 1000) - ts);
        if (d < 60) { return d + 's ago'; }
        if (d < 3600) { return Math.floor(d / 60) + 'm ago'; }
        if (d < 86400) { return Math.floor(d / 3600) + 'h ago'; }
        return Math.floor(d / 86400) + 'd ago';
    }
    function duration(secs) {
        secs = Math.max(0, Math.round(secs || 0));
        if (secs < 60) { return secs + 's'; }
        if (secs < 3600) { return Math.floor(secs / 60) + 'm ' + (secs % 60) + 's'; }
        if (secs < 86400) { return Math.floor(secs / 3600) + 'h ' + Math.floor(secs % 3600 / 60) + 'm'; }
        return Math.floor(secs / 86400) + 'd ' + Math.floor(secs % 86400 / 3600) + 'h';
    }
    function bytes(n) {
        n = n || 0;
        if (n < 1024) { return n + ' B'; }
        if (n < 1048576) { return (n / 1024).toFixed(1) + ' KB'; }
        return (n / 1048576).toFixed(2) + ' MB';
    }
    function rate(bps) {
        if (bps >= 1048576) { return (bps / 1048576).toFixed(2) + ' MB/s'; }
        if (bps >= 1024) { return (bps / 1024).toFixed(1) + ' KB/s'; }
        return Math.round(bps) + ' B/s';
    }
    function signalQuality(rssi) {
        if (rssi == null) { return { label: 'unknown', bars: 0 }; }
        if (rssi >= -60) { return { label: 'excellent', bars: 4 }; }
        if (rssi >= -75) { return { label: 'good', bars: 3 }; }
        if (rssi >= -88) { return { label: 'fair', bars: 2 }; }
        return { label: 'weak', bars: 1 };
    }
    function signalBars(rssi) {
        var q = signalQuality(rssi), out = '<span class="sig" title="' + esc(rssi == null ? 'no RSSI' : rssi + ' dBm') + '">';
        for (var i = 1; i <= 4; i++) { out += '<i class="sig-bar' + (i <= q.bars ? ' on' : '') + '"></i>'; }
        return out + '</span>';
    }
    function flash(msg, kind) {
        var el = $('remote-flash');
        if (!el) { return; }
        el.textContent = msg;
        el.className = 'remote-flash ' + (kind || 'info');
        el.hidden = false;
        clearTimeout(el._t);
        el._t = setTimeout(function() { el.hidden = true; }, 6000);
    }
    function api(method, path, body, isForm) {
        var opts = { method: method, headers: {} };
        if (body !== undefined && body !== null) {
            if (isForm) { opts.body = body; }
            else { opts.headers['Content-Type'] = 'application/json'; opts.body = JSON.stringify(body); }
        }
        return fetch(path, opts).then(function(r) {
            return r.text().then(function(t) {
                var data = null;
                try { data = t ? JSON.parse(t) : null; } catch (e) { data = { detail: t }; }
                if (!r.ok) { throw new Error((data && data.detail) || ('HTTP ' + r.status)); }
                return data;
            });
        });
    }
    function nodeUrl(suffix, hash) { return '/api/remote/nodes/' + encodeURIComponent(hash || state.selected) + suffix; }
    function shouldPauseShell() { return state.tab === 'shell' && document.activeElement === $('term-input'); }
    function findNode(hash) {
        if (!state.snapshot || !hash) { return null; }
        for (var i = 0; i < state.snapshot.nodes.length; i++) {
            if (state.snapshot.nodes[i].identity_hash === hash) { return state.snapshot.nodes[i]; }
        }
        return null;
    }
    function selectedNode() { return findNode(state.selected); }

    // ── Map ──────────────────────────────────────────────────────────────────
    function rssiNorm(rssi) {
        if (rssi == null) { return 0.5; }
        var v = (rssi + 100) / 70; // -100 dBm → 0, -30 dBm → 1
        return Math.max(0, Math.min(1, v));
    }
    function renderMap(snap) {
        var svg = $('remote-map');
        if (!svg) { return; }
        var W = 640, H = 520; // viewBox units; the SVG scales to its card
        var cx = W / 2, cy = H / 2;
        var maxHops = 1;
        snap.nodes.forEach(function(n) { if (n.hops != null && n.hops > maxHops) { maxHops = n.hops; } });
        var unknown = snap.nodes.some(function(n) { return n.hops == null; });
        var rings = Math.min(5, Math.max(2, maxHops + (unknown ? 1 : 0)));
        var outer = Math.min(W, H) / 2 - 40;
        var step = outer / rings;
        var out = [];
        var positions = {};
        state.mapCenter = { x: cx, y: cy };
        out.push('<defs><radialGradient id="rm-glow"><stop offset="0%" stop-color="rgba(13,203,240,.35)"/><stop offset="100%" stop-color="rgba(13,203,240,0)"/></radialGradient></defs>');
        out.push('<circle cx="' + cx + '" cy="' + cy + '" r="' + (outer + 20) + '" fill="url(#rm-glow)"/>');
        for (var r = 1; r <= rings; r++) {
            var rad = r * step;
            var label = (unknown && r === rings) ? 'hops ?' : (r + ' hop' + (r > 1 ? 's' : ''));
            out.push('<circle class="rm-ring" cx="' + cx + '" cy="' + cy + '" r="' + rad + '"/>');
            out.push('<text class="rm-ring-label" x="' + (cx + 6) + '" y="' + (cy - rad + 14) + '">' + esc(label) + '</text>');
        }
        var groups = {};
        snap.nodes.forEach(function(n) {
            var level = n.hops == null ? rings : Math.min(n.hops, rings);
            (groups[level] = groups[level] || []).push(n);
        });
        Object.keys(groups).forEach(function(level) {
            var list = groups[level].slice().sort(function(a, b) { return a.codename.localeCompare(b.codename); });
            var count = list.length;
            list.forEach(function(n, i) {
                // Each level occupies the band between its inner and outer
                // ring; level 1 starts clear of the centre bubble. Stronger
                // RSSI pulls the node toward the inner edge of its band.
                var lo = (level - 1) * step + (Number(level) === 1 ? 62 : 18);
                var hi = Math.max(lo + 4, level * step - 18);
                var rad = lo + (1 - rssiNorm(n.rssi)) * (hi - lo);
                var angle = -Math.PI / 2 + (i / count) * Math.PI * 2 + (count > 1 ? Math.PI / count / 2 : 0) + (level % 2 ? 0 : Math.PI / 6);
                var x = cx + Math.cos(angle) * rad, y = cy + Math.sin(angle) * rad;
                positions[n.identity_hash] = { x: x, y: y };
                var cls = 'rm-node' + (n.online ? ' online' : ' offline') + (n.paired ? ' paired' : '') +
                    (n.pairing === 'incoming' ? ' incoming' : '') + (n.pairing === 'requested' ? ' requested' : '') +
                    (n.link === 'active' ? ' linked' : '') + (state.selected === n.identity_hash ? ' selected' : '');
                var tip = (n.tag ? n.tag + ' — ' : '') + n.codename + ' · ' + (n.online ? 'online' : 'offline') + (n.paired ? ' · paired' : '') +
                    (n.rssi != null ? ' · ' + n.rssi + ' dBm (' + signalQuality(n.rssi).label + ')' : '') +
                    (n.hops != null ? ' · ' + n.hops + ' hop' + (n.hops > 1 ? 's' : '') : '') + ' · v' + n.gateway_version +
                    (n.vpn_tunnel_ip ? ' · vpn ' + n.vpn_tunnel_ip : '') +
                    (n.vpn_routes && n.vpn_routes.length ? ' · ' + n.vpn_routes.join(' ') : '');
                out.push('<g class="' + cls + '" data-hash="' + esc(n.identity_hash) + '" transform="translate(' + x.toFixed(1) + ',' + y.toFixed(1) + ')"><title>' + esc(tip) + '</title>');
                out.push('<line class="rm-spoke" x1="0" y1="0" x2="' + (cx - x).toFixed(1) + '" y2="' + (cy - y).toFixed(1) + '"/>');
                out.push('<circle class="rm-halo" r="24"/>');
                if (n.online) { out.push('<circle class="rm-pulse" r="12"/>'); }
                out.push('<circle class="rm-dot" r="12"/>');
                if (n.paired) { out.push('<circle class="rm-paired-ring" r="16"/>'); }
                if (n.pairing === 'incoming') { out.push('<text class="rm-flag" y="-20">wants to pair</text>'); }
                out.push('<text class="rm-label" y="31">' + esc(n.tag || n.codename) + '</text>');
                var meta = (n.rssi != null ? n.rssi + ' dBm' : '') + (n.hops != null ? (n.rssi != null ? ' · ' : '') + n.hops + 'h' : '');
                if (meta) { out.push('<text class="rm-meta" y="44">' + esc(meta) + '</text>'); }
                out.push('</g>');
            });
        });
        out.push('<g class="rm-self" transform="translate(' + cx + ',' + cy + ')"><title>this node</title>');
        out.push('<circle class="rm-self-halo" r="30"/><circle class="rm-self-dot" r="16"/>');
        out.push('<text class="rm-label rm-self-label" y="38">' + esc(snap.local.codename) + '</text>');
        out.push('<text class="rm-meta" y="51">this node</text></g>');
        // Layer the hover link + callout above the nodes.
        out.push('<line class="rm-hover-link" id="rm-hover-link" x1="0" y1="0" x2="0" y2="0" style="display:none"/>');
        svg.innerHTML = out.join('');
        state.mapPositions = positions;
        Array.prototype.forEach.call(svg.querySelectorAll('.rm-node'), function(g) {
            var hash = g.getAttribute('data-hash');
            g.addEventListener('click', function() { select(hash); });
            g.addEventListener('mouseenter', function() { hoverNode(hash); });
            g.addEventListener('mouseleave', function() { hoverNode(null); });
        });
        if (state.hovered) { hoverNode(state.hovered); }
    }
    /// Draw a link from this node to the hovered one and show its details.
    function hoverNode(hash) {
        state.hovered = hash;
        var link = $('rm-hover-link');
        var info = $('remote-map-info');
        var pos = hash && state.mapPositions ? state.mapPositions[hash] : null;
        var node = hash ? findNode(hash) : null;
        if (link) {
            if (pos && state.mapCenter) {
                link.setAttribute('x1', state.mapCenter.x);
                link.setAttribute('y1', state.mapCenter.y);
                link.setAttribute('x2', pos.x.toFixed(1));
                link.setAttribute('y2', pos.y.toFixed(1));
                link.style.display = '';
            } else {
                link.style.display = 'none';
            }
        }
        if (!info) { return; }
        if (!node) {
            var local = state.snapshot && state.snapshot.local;
            info.innerHTML = '<span class="remote-map-info-hint">Hover a node</span>' +
                (local ? '<span class="remote-map-info-self">this node: <b>' + esc(local.codename) + '</b></span>' : '');
            return;
        }
        var routes = (node.vpn_routes || []).join(', ');
        info.innerHTML =
            '<span class="remote-map-info-name">' + esc(node.tag || node.codename) + (node.tag ? ' <span class="remote-sub">(' + esc(node.codename) + ')</span>' : '') + '</span>' +
            '<span class="remote-map-info-item">' + signalBars(node.rssi) + ' ' + esc(node.rssi != null ? node.rssi + ' dBm' : 'no signal') +
            (node.hops != null ? ' · ' + esc(node.hops) + ' hop' + (node.hops > 1 ? 's' : '') : '') + '</span>' +
            '<span class="remote-map-info-item">VPN: <code>' + esc(node.vpn_tunnel_ip || 'not linked') + '</code></span>' +
            '<span class="remote-map-info-item">Subnets: <code>' + esc(routes || '—') + '</code></span>';
    }

    // ── Lists ────────────────────────────────────────────────────────────────
    function badge(text, cls) { return '<span class="badge ' + cls + '">' + esc(text) + '</span>'; }
    function pairingBadge(n) {
        if (n.paired) { return badge('paired', 'badge-ok'); }
        if (n.pairing === 'incoming') { return badge('approve?', 'badge-warn'); }
        if (n.pairing === 'requested') { return badge('requested', 'badge-warn'); }
        if (n.pairing === 'rejected') { return badge('rejected', 'badge-err'); }
        if (n.pairing === 'failed') { return badge('failed', 'badge-err'); }
        return badge('not paired', 'reticulum-badge-soft');
    }
    function rowAction(n) {
        if (n.paired) { return '<button class="btn-secondary btn-small" data-row-act="control" data-hash="' + esc(n.identity_hash) + '">Control</button>'; }
        if (n.pairing === 'incoming') { return '<button class="btn-primary btn-small" data-row-act="approve" data-hash="' + esc(n.identity_hash) + '">Approve</button>'; }
        if (n.pairing === 'requested') { return '<span class="card-body-text">waiting…</span>'; }
        return '<button class="btn-primary btn-small" data-row-act="pair" data-hash="' + esc(n.identity_hash) + '"' + (n.online ? '' : ' disabled title="node is offline"') + '>Pair</button>';
    }
    function renderNodes(snap) {
        var tbody = $('remote-node-rows');
        if (!tbody) { return; }
        if (!snap.nodes.length) {
            tbody.innerHTML = '<tr><td colspan="7" class="frames-empty">No nodes announced yet</td></tr>';
            return;
        }
        tbody.innerHTML = snap.nodes.map(function(n) {
            return '<tr class="remote-row' + (state.selected === n.identity_hash ? ' selected' : '') + '" data-hash="' + esc(n.identity_hash) + '">' +
                '<td><span class="status-dot ' + (n.online ? 'status-dot--ok' : 'status-dot--idle') + '"></span></td>' +
                '<td class="td-time"><strong>' + esc(n.tag || n.codename) + '</strong><br><span class="remote-sub">' +
                    (n.tag ? esc(n.codename) + ' · ' : '') + 'v' + esc(n.gateway_version) + '</span></td>' +
                '<td class="td-len">' + esc(n.hops != null ? n.hops : '—') + '</td>' +
                '<td class="td-len">' + signalBars(n.rssi) + ' ' + esc(n.rssi != null ? n.rssi + ' dBm' : '—') + '</td>' +
                '<td>' + pairingBadge(n) + (n.link === 'active' ? ' ' + badge('link', 'badge-ok') : (n.link === 'pending' ? ' ' + badge('linking', 'badge-warn') : '')) + '</td>' +
                '<td class="td-time">' + esc(ago(n.last_seen_ts)) + '</td>' +
                '<td>' + rowAction(n) + '</td></tr>';
        }).join('');
        Array.prototype.forEach.call(tbody.querySelectorAll('tr[data-hash]'), function(tr) {
            tr.addEventListener('click', function(ev) {
                if (ev.target && ev.target.getAttribute && ev.target.getAttribute('data-row-act')) { return; }
                select(tr.getAttribute('data-hash'));
            });
        });
        Array.prototype.forEach.call(tbody.querySelectorAll('button[data-row-act]'), function(b) {
            b.addEventListener('click', function() {
                var hash = b.getAttribute('data-hash'), act = b.getAttribute('data-row-act');
                select(hash);
                if (act === 'control') { state.tab = 'radio'; render(); return; }
                b.disabled = true;
                api('POST', nodeUrl('/' + act, hash))
                    .then(function(d) { flash(d.detail || ('Pairing: ' + d.pairing), 'ok'); })
                    .catch(function(e) { flash(e.message, 'err'); b.disabled = false; });
            });
        });
    }
    function renderIncoming(snap) {
        var el = $('remote-incoming');
        if (!el) { return; }
        var list = snap.incoming_requests || [];
        $('remote-incoming-count').textContent = String(list.length);
        if (!list.length) { el.innerHTML = '<p class="card-body-text">No pending requests</p>'; return; }
        el.innerHTML = list.map(function(r) {
            return '<div class="remote-request">' +
                '<div class="remote-request-main"><strong>' + esc(r.codename) + '</strong>' +
                '<code class="td-hash">' + esc(r.identity_hash) + '</code>' +
                '<span class="card-body-text">' + esc(ago(r.received_ts)) + '</span></div>' +
                '<div class="remote-sas"><span class="remote-sas-label">Verify this code on the requesting node</span><span class="remote-sas-code">' + esc(r.sas) + '</span></div>' +
                '<div class="remote-request-actions">' +
                '<button class="btn-primary" data-act="approve" data-hash="' + esc(r.identity_hash) + '">Approve</button>' +
                '<button class="btn-secondary" data-act="reject" data-hash="' + esc(r.identity_hash) + '">Reject</button></div></div>';
        }).join('');
        Array.prototype.forEach.call(el.querySelectorAll('button[data-act]'), function(b) {
            b.addEventListener('click', function() {
                var hash = b.getAttribute('data-hash'), act = b.getAttribute('data-act');
                b.disabled = true;
                api('POST', nodeUrl('/' + act, hash))
                    .then(function(d) { flash(d.detail || act, 'ok'); })
                    .catch(function(e) { flash(e.message, 'err'); b.disabled = false; });
            });
        });
    }
    function renderJobs(snap) {
        var el = $('remote-jobs');
        if (!el) { return; }
        var jobs = (snap.jobs || []).slice().sort(function(a, b) { return b.id - a.id; });
        var now = Date.now();
        if (!jobs.length) { el.innerHTML = '<p class="card-body-text">No transfers</p>'; return; }
        el.innerHTML = jobs.map(function(j) {
            var pct = j.size ? Math.min(100, Math.round(j.sent * 100 / j.size)) : 0;
            var cls = j.state === 'done' ? 'badge-ok' : (j.state === 'failed' ? 'badge-err' : 'badge-warn');
            var track = state.jobRates[j.id] || (state.jobRates[j.id] = { sent: j.sent, ts: now, bps: 0 });
            if (j.state === 'transferring' && now - track.ts >= 2000 && j.sent > track.sent) {
                track.bps = (j.sent - track.sent) * 1000 / (now - track.ts);
                track.sent = j.sent; track.ts = now;
            }
            var eta = (j.state === 'transferring' && track.bps > 0) ? ' · ETA ' + duration((j.size - j.sent) / track.bps) : '';
            var speed = (j.state === 'transferring' && track.bps > 0) ? rate(track.bps) + eta : '';
            var elapsed = j.state === 'done' || j.state === 'failed' ? 'took ' + duration(j.updated_ts - j.started_ts) : 'running ' + duration(Math.floor(now / 1000) - j.started_ts);
            return '<div class="remote-job"><div class="remote-job-head"><strong>' + esc(j.codename) + '</strong> ' +
                '<span class="card-body-text">' + esc(j.name || 'new package') + ' · ' + esc(bytes(j.size)) + '</span> ' + badge(j.state, cls) +
                '<span class="remote-job-speed">' + esc(speed) + '</span></div>' +
                '<div class="remote-progress"><div class="remote-progress-bar' + (j.state === 'failed' ? ' failed' : '') + '" style="width:' + pct + '%"></div></div>' +
                '<div class="card-body-text">' + esc(bytes(j.sent)) + ' / ' + esc(bytes(j.size)) + ' (' + pct + '%) · ' + esc(elapsed) + (j.detail ? ' — ' + esc(j.detail) : '') + '</div></div>';
        }).join('');
    }
    function renderEvents(snap) {
        var el = $('remote-events');
        if (!el) { return; }
        var ev = snap.events || [];
        if (!ev.length) { el.innerHTML = '<tr><td colspan="4" class="frames-empty">No activity yet</td></tr>'; return; }
        el.innerHTML = ev.slice(0, 30).map(function(e) {
            return '<tr><td class="td-time">' + esc(ago(e.ts)) + '</td><td>' + badge(e.kind, eventBadgeClass(e.kind)) + '</td>' +
                '<td class="td-time">' + esc(e.codename || shortHash(e.node)) + '</td><td>' + esc(e.details) + '</td></tr>';
        }).join('');
    }
    function eventBadgeClass(kind) {
        if (kind === 'paired' || kind === 'pair-notify') { return 'badge-ok'; }
        if (kind === 'pair-incoming' || kind === 'pair-request' || kind === 'blob-begin') { return 'badge-warn'; }
        if (kind === 'unpair' || kind === 'pair-reject') { return 'badge-err'; }
        return 'reticulum-badge-soft';
    }
    function renderStats(snap) {
        var online = 0, paired = 0;
        snap.nodes.forEach(function(n) { if (n.online) { online++; } if (n.paired) { paired++; } });
        $('remote-stat-nodes').textContent = String(snap.nodes.length);
        $('remote-stat-online').textContent = String(online);
        $('remote-stat-paired').textContent = String(paired);
        $('remote-stat-pending').textContent = String((snap.incoming_requests || []).length);
        $('remote-local-hash').textContent = snap.local.identity_hash;
        setText('remote-status-text', snap.local.codename);
        var dot = $('remote-status-dot');
        if (dot) { dot.className = 'status-dot ' + (online ? 'status-dot--ok' : 'status-dot--idle'); }
    }
    function setText(id, v) { var el = $(id); if (el) { el.textContent = v; } }

    // ── Detail panel ─────────────────────────────────────────────────────────
    function select(hash) {
        if (state.selected !== hash) { state.plugins = null; state.info = null; state.radio = {}; state.rtts = []; state.tab = 'overview'; }
        state.selected = hash;
        render();
        loadTabData(state.tab);
        var panel = $('remote-detail-card');
        if (panel && window.innerWidth < 1100) { panel.scrollIntoView({ behavior: 'smooth', block: 'start' }); }
    }
    /// Fetch what a tab shows the first time it is opened, so the operator
    /// does not have to press "Load" on every panel.
    function loadTabData(tab) {
        var node = selectedNode();
        if (!node || !node.paired) { return; }
        if (tab === 'radio') {
            [0, 1].forEach(function(m) {
                if (state.radio[m]) { return; }
                api('GET', nodeUrl('/radio/' + m))
                    .then(function(d) { if (state.selected === node.identity_hash) { state.radio[m] = d; renderDetail(); } })
                    .catch(function(e) { flash('Radio ' + (m ? 'B' : 'A') + ': ' + e.message, 'err'); });
            });
        } else if (tab === 'plugins') {
            if (state.plugins) { return; }
            api('GET', nodeUrl('/plugins'))
                .then(function(d) { if (state.selected === node.identity_hash) { state.plugins = d; renderDetail(); } })
                .catch(function(e) { flash('Plugins: ' + e.message, 'err'); });
        } else if (tab === 'media') {
            refreshMedia().then(function() { if (state.tab === 'media') { renderDetail(); } });
        } else if (tab === 'overview') {
            if (state.info) { return; }
            api('GET', nodeUrl('/info'))
                .then(function(d) { if (state.selected === node.identity_hash) { state.info = d; renderDetail(); } })
                .catch(function() {});
        }
    }
    function tabButton(id, label, enabled) {
        return '<button class="remote-tab' + (state.tab === id ? ' active' : '') + '" data-tab="' + id + '"' + (enabled ? '' : ' disabled') + '>' + esc(label) + '</button>';
    }
    function renderDetail() {
        var el = $('remote-detail');
        if (!el) { return; }
        var n = selectedNode();
        if (!n) {
            el.innerHTML = '<div class="remote-empty"><div class="remote-empty-icon">📡</div><p class="card-body-text">Select a node</p></div>';
            return;
        }
        var h = [];
        h.push('<div class="remote-detail-head"><div><div class="remote-detail-name">' + esc(n.tag || n.codename) +
            (n.tag ? ' <span class="remote-sub">' + esc(n.codename) + '</span>' : '') + '</div>' +
            '<code class="td-hash remote-detail-hash">' + esc(n.identity_hash) + '</code></div>' +
            '<div class="remote-detail-badges">' + badge(n.online ? 'online' : 'offline', n.online ? 'badge-ok' : 'reticulum-badge-soft') + ' ' + pairingBadge(n) +
            (n.link === 'active' ? ' ' + badge('link active', 'badge-ok') : '') + '</div></div>');
        h.push('<div class="remote-tabs">' + tabButton('overview', 'Overview', true) + tabButton('radio', 'Radio', n.paired) +
            tabButton('plugins', 'Plugins', n.paired) + tabButton('media', 'Media', n.paired) +
            tabButton('shell', 'Terminal', n.paired) + tabButton('system', 'System', n.paired) + '</div>');
        if (!n.paired && state.tab !== 'overview') { state.tab = 'overview'; }
        h.push('<div class="remote-tab-body">');
        if (state.tab === 'overview') { h.push(renderOverview(n)); }
        else if (state.tab === 'radio') { h.push(renderRadioTab()); }
        else if (state.tab === 'plugins') { h.push(renderPluginsTab()); }
        else if (state.tab === 'media') { h.push(renderMediaTab(n)); }
        else if (state.tab === 'shell') { h.push(renderShellTab(n)); }
        else if (state.tab === 'system') { h.push(renderSystemTab(n)); }
        h.push('</div>');
        el.innerHTML = h.join('');
        Array.prototype.forEach.call(el.querySelectorAll('button[data-act]'), function(b) {
            b.addEventListener('click', function() { action(b); });
        });
        Array.prototype.forEach.call(el.querySelectorAll('button[data-tab]'), function(b) {
            b.addEventListener('click', function() {
                state.tab = b.getAttribute('data-tab');
                renderDetail();
                loadTabData(state.tab);
            });
        });
        var kind = el.querySelector('select[id$="-kind"]');
        if (kind) { kind.addEventListener('change', function() { syncModulationOptions(kind); }); }
        var termWindow = $('term-window');
        if (termWindow) {
            termWindow.addEventListener('mousedown', function(ev) {
                // Clicking the chrome (not a button, not a selection) focuses the prompt.
                if (ev.target.closest('button') || (window.getSelection && String(window.getSelection()))) { return; }
                var input = $('term-input');
                if (input && !input.disabled) { setTimeout(function() { input.focus(); }, 0); }
            });
        }
        var termInput = $('term-input');
        if (termInput) {
            termInput.focus();
            termInput.addEventListener('keydown', function(ev) {
                if (ev.key === 'l' && (ev.ctrlKey || ev.metaKey)) {
                    ev.preventDefault();
                    state.shell[state.selected] = [];
                    renderDetail();
                    return;
                }
                if (ev.key === 'Escape' && state.termFull) {
                    ev.preventDefault();
                    state.termFull = false;
                    document.body.classList.remove('term-fullscreen');
                    renderDetail();
                    return;
                }
                if (ev.key === 'Enter') {
                    ev.preventDefault();
                    var cmd = (termInput.value || '').trim();
                    termInput.value = '';
                    runShell(cmd);
                } else if (ev.key === 'ArrowUp') {
                    ev.preventDefault();
                    if (state.shellHistoryPos > 0) { state.shellHistoryPos--; termInput.value = state.shellHistory[state.shellHistoryPos] || ''; }
                } else if (ev.key === 'ArrowDown') {
                    ev.preventDefault();
                    if (state.shellHistoryPos < state.shellHistory.length - 1) { state.shellHistoryPos++; termInput.value = state.shellHistory[state.shellHistoryPos] || ''; }
                    else { state.shellHistoryPos = state.shellHistory.length; termInput.value = ''; }
                }
            });
            var term = $('remote-term');
            if (term) { term.scrollTop = term.scrollHeight; }
        }
        var cls = $('remote-node-class');
        if (cls) {
            cls.addEventListener('change', function() {
                api('POST', nodeUrl('/class'), { class: cls.value })
                    .then(function(d) { flash(d.detail, 'ok'); })
                    .catch(function(e) { flash(e.message, 'err'); });
            });
        }
    }
    function renderOverview(n) {
        var q = signalQuality(n.rssi);
        var h = [];
        h.push('<div class="remote-overview-grid">' +
            '<div class="remote-tile"><span class="remote-tile-label">Signal</span><span class="remote-tile-value">' + signalBars(n.rssi) + ' ' + esc(n.rssi != null ? n.rssi + ' dBm' : '—') + '</span><span class="remote-tile-sub">' + esc(q.label) + '</span></div>' +
            '<div class="remote-tile"><span class="remote-tile-label">Distance</span><span class="remote-tile-value">' + esc(n.hops != null ? n.hops + ' hop' + (n.hops > 1 ? 's' : '') : '—') + '</span><span class="remote-tile-sub">via Reticulum</span></div>' +
            '<div class="remote-tile"><span class="remote-tile-label">Last announce</span><span class="remote-tile-value">' + esc(ago(n.last_seen_ts)) + '</span><span class="remote-tile-sub">' + esc(n.online ? 'online' : 'offline') + '</span></div>' +
            '<div class="remote-tile"><span class="remote-tile-label">Gateway</span><span class="remote-tile-value">v' + esc(n.gateway_version || '—') + '</span><span class="remote-tile-sub">' + esc(n.link === 'active' ? 'link active' : n.link) + '</span></div>' +
            '</div>');
        h.push('<div class="remote-tag-row">' +
            '<input class="form-input remote-tag-input" id="node-tag" maxlength="48" placeholder="Add a label (e.g. roof mast, van 2)" value="' + esc(n.tag || '') + '"/>' +
            '<button class="btn-secondary btn-small" data-act="tag-save">Save label</button></div>');
        h.push('<dl class="remote-kv">' +
            '<dt>VPN address</dt><dd>' + (n.vpn_tunnel_ip ? '<code>' + esc(n.vpn_tunnel_ip) + '</code>' : '<span class="remote-sub">not linked</span>') + '</dd>' +
            '<dt>VPN subnets</dt><dd>' + ((n.vpn_routes || []).length ? '<code>' + esc(n.vpn_routes.join(', ')) + '</code>' : '<span class="remote-sub">none announced</span>') + '</dd>' +
            '<dt>Destination</dt><dd><code class="td-hash">' + esc(n.destination_hash) + '</code></dd>' +
            '<dt>Link</dt><dd>' + esc(n.link) + '</dd>' +
            '<dt>Coding</dt><dd>' + (n.fec_capable ? badge('adaptive FEC', 'badge-ok') : badge('legacy FEC', 'reticulum-badge-soft')) +
            (n.paired && n.fec_capable ? ' <select class="form-input remote-inline-select" id="remote-node-class"><option value="control">control (robust)</option><option value="bulk">bulk (auto)</option><option value="media">media (fast)</option></select>' : '') + '</dd>' +
            (n.paired ? '<dt>Permissions</dt><dd>' + esc(n.permissions === 4294967295 ? 'full control' : '0x' + n.permissions.toString(16)) + '</dd>' : '') + '</dl>');
        h.push('<div class="remote-section"><div class="remote-section-title">Pairing</div>');
        h.push('<div class="remote-sas"><span class="remote-sas-label">Verification code (must match on both nodes)</span><span class="remote-sas-code">' + esc(n.sas) + '</span></div>');
        if (n.pairing_detail) { h.push('<p class="card-body-text">' + esc(n.pairing_detail) + '</p>'); }
        h.push('<div class="remote-actions">');
        if (n.paired) {
            h.push('<button class="btn-secondary" data-act="ping">Ping</button><button class="btn-secondary" data-act="info">Fetch info</button><button class="btn-secondary btn-danger" data-act="unpair">Unpair</button>');
        } else if (n.pairing === 'incoming') {
            h.push('<button class="btn-primary" data-act="approve">Approve</button><button class="btn-secondary" data-act="reject">Reject</button>');
        } else if (n.pairing === 'requested') {
            h.push('<button class="btn-primary" data-act="pair">Resend request</button><button class="btn-secondary" data-act="cancel">Cancel</button>');
        } else {
            h.push('<button class="btn-primary" data-act="pair"' + (n.online ? '' : ' disabled title="node is offline"') + (n.accepts_pairing || n.protocol === 0 ? '' : ' disabled title="node does not accept pairing"') + '>Request pairing</button>');
        }
        h.push('</div>');
        h.push('</div>');
        if (n.paired && (state.info || state.rtts.length)) {
            h.push('<div class="remote-section"><div class="remote-section-title">Live</div>');
            if (state.rtts.length) {
                h.push('<div class="remote-rtt">' + state.rtts.slice(-12).map(function(ms) { return '<i style="height:' + Math.max(4, Math.min(40, ms / 10)) + 'px" title="' + ms + ' ms"></i>'; }).join('') +
                    '<span class="remote-sub">last ping ' + esc(state.rtts[state.rtts.length - 1]) + ' ms</span></div>');
            }
            if (state.info) {
                var i = state.info;
                h.push('<dl class="remote-kv"><dt>Serial</dt><dd>' + esc(i.serial) + '</dd><dt>Version</dt><dd>' + esc(i.gateway_version) + '</dd>' +
                    '<dt>Uptime</dt><dd>' + esc(duration(i.uptime_secs)) + '</dd><dt>Radios</dt><dd>' + esc(i.radio_modules) + ' modules</dd></dl>');
            }
            h.push('</div>');
        }
        return h.join('');
    }
    var MCS = ['0 · BPSK ½ 4×', '1 · BPSK ½ 2×', '2 · QPSK ½ 2×', '3 · QPSK ½', '4 · QPSK ¾', '5 · 16-QAM ½', '6 · 16-QAM ¾'];
    var BWOPT = ['Option 1', 'Option 2', 'Option 3', 'Option 4'];
    var FCHIP = ['100 kchip/s', '200 kchip/s', '1000 kchip/s', '2000 kchip/s'];
    var QMODE = ['Mode 0', 'Mode 1', 'Mode 2', 'Mode 3', 'Mode 4'];
    function renderRadioTab() {
        var h = [];
        [0, 1].forEach(function(m) {
            var cfg = state.radio[m];
            h.push('<div class="remote-radio-module"><div class="remote-radio-head"><strong>Radio ' + (m ? 'B' : 'A') + '</strong>' +
                '<button class="btn-secondary btn-small" data-act="radio-load" data-module="' + m + '">' + (cfg ? 'Reload' : 'Load') + '</button></div>');
            if (cfg) {
                var isQpsk = cfg.mod_kind === 2;
                h.push('<div class="form-row">' +
                    field('Frequency (Hz)', 'rf-' + m + '-freq', cfg.freq_hz) +
                    field('Spacing (Hz)', 'rf-' + m + '-spacing', cfg.spacing_hz) +
                    field('Channel', 'rf-' + m + '-channel', cfg.channel) +
                    selectField('Filter', 'rf-' + m + '-bw', cfg.bw_filter, ['Narrow', 'Wide']) +
                    selectField('Modulation', 'rf-' + m + '-kind', cfg.mod_kind, ['Off', 'OFDM', 'QPSK', 'FSK']) +
                    selectField(isQpsk ? 'Chip rate' : 'MCS', 'rf-' + m + '-a', cfg.mod_a, isQpsk ? FCHIP : MCS) +
                    selectField(isQpsk ? 'Rate mode' : 'Bandwidth option', 'rf-' + m + '-b', cfg.mod_b, isQpsk ? QMODE : BWOPT) +
                    field('TX power (dBm)', 'rf-' + m + '-tx', cfg.tx_power) +
                    selectField('Accelerator', 'rf-' + m + '-acc', cfg.accelerator, ['Native', 'Hardware']) +
                    selectField('Antenna (2.4G)', 'rf-' + m + '-ant', cfg.antenna, ['Internal', 'External']) +
                    '</div><div class="remote-actions"><button class="btn-apply" data-act="radio-apply" data-module="' + m + '">Apply to remote node</button></div>');
            } else {
                h.push('<p class="card-body-text">Loading…</p>');
            }
            h.push('</div>');
        });
        return h.join('');
    }
    function syncModulationOptions(kindEl) {
        var m = kindEl.id.split('-')[1];
        var isQpsk = kindEl.value === '2';
        var a = $('rf-' + m + '-a'), b = $('rf-' + m + '-b');
        if (!a || !b) { return; }
        a.innerHTML = (isQpsk ? FCHIP : MCS).map(function(o, i) { return '<option value="' + i + '">' + esc(o) + '</option>'; }).join('');
        b.innerHTML = (isQpsk ? QMODE : BWOPT).map(function(o, i) { return '<option value="' + i + '">' + esc(o) + '</option>'; }).join('');
    }
    function renderPluginsTab() {
        var h = ['<div class="remote-actions"><button class="btn-secondary" data-act="plugins">' + (state.plugins ? 'Refresh list' : 'Load plugins') + '</button></div>'];
        if (state.plugins) {
            if (!state.plugins.length) { h.push('<p class="card-body-text">No plugins</p>'); }
            else {
                h.push('<table class="frames-table remote-plugins"><thead><tr><th>Plugin</th><th>Version</th><th>State</th><th></th></tr></thead><tbody>');
                state.plugins.forEach(function(p) {
                    h.push('<tr><td><strong>' + esc(p.name || p.id) + '</strong><br><code class="td-hash">' + esc(p.id) + '</code></td><td>' + esc(p.version) + '</td>' +
                        '<td>' + badge(p.active ? 'active' : 'stopped', p.active ? 'badge-ok' : 'reticulum-badge-soft') + '</td>' +
                        '<td class="remote-plugin-actions">' +
                        '<button class="btn-secondary btn-small" data-act="plugin" data-id="' + esc(p.id) + '" data-plugin-act="restart">Restart</button>' +
                        '<button class="btn-secondary btn-small" data-act="plugin" data-id="' + esc(p.id) + '" data-plugin-act="' + (p.active ? 'stop' : 'start') + '">' + (p.active ? 'Stop' : 'Start') + '</button>' +
                        '<button class="btn-secondary btn-small" data-act="plugin-update" data-id="' + esc(p.id) + '">Update…</button></td></tr>');
                });
                h.push('</tbody></table>');
            }
        }
        h.push('<div class="remote-section"><div class="remote-section-title">Install / update over radio</div>' +
            '<div class="remote-upload"><input type="file" id="remote-upload-file" accept=".zip" class="form-input"/>' +
            '<input type="text" id="remote-upload-id" class="form-input" placeholder="plugin id (empty = install new)"/>' +
            '<button class="btn-primary" data-act="upload">Send over radio</button></div></div>');
        return h.join('');
    }
    function renderMediaTab(n) {
        var h = [];
        var mine = (state.media && state.media.channels || []).filter(function(c) { return c.node === n.identity_hash; });
        var streams = (state.snapshot.media || []).filter(function(m) { return m.node === n.identity_hash; });
        if (!mine.length && !streams.length) { h.push('<p class="card-body-text">No channels</p>'); }
        if (mine.length) {
            h.push('<table class="frames-table remote-plugins"><thead><tr><th>Stream</th><th>Ingress → radio</th><th>Radio → egress</th><th>FEC</th><th>Datagrams</th><th></th></tr></thead><tbody>');
            mine.forEach(function(c) {
                var st = streams.filter(function(m) { return m.stream === c.stream && m.direction === 'out'; })[0];
                h.push('<tr><td>' + esc(c.stream) + ' <span class="remote-sub">' + esc(c.profile) + '</span></td><td><code>' + esc(c.ingress) + '</code></td><td><code>' + esc(c.egress) + '</code></td>' +
                    '<td>' + esc(c.k) + '+' + esc(c.m) + '</td><td class="remote-sub">in ' + esc(c.datagrams_in) + ' · out ' + esc(c.datagrams_out) + (c.dropped ? ' · dropped ' + esc(c.dropped) : '') +
                    (st ? '<br>sent ' + esc(st.packets_sent) + ' + ' + esc(st.parity_sent) + ' parity' : '') + '</td>' +
                    '<td><button class="btn-secondary btn-small" data-act="media-stop" data-stream="' + esc(c.stream) + '">Stop</button></td></tr>');
            });
            h.push('</tbody></table>');
        }
        var incoming = streams.filter(function(m) { return m.direction === 'in'; });
        if (incoming.length) {
            h.push('<div class="remote-section-title">Receiving</div><table class="frames-table remote-plugins"><thead><tr><th>Stream</th><th>Received</th><th>Recovered by FEC</th><th>Lost</th><th>Late</th></tr></thead><tbody>');
            incoming.forEach(function(m) {
                h.push('<tr><td>' + esc(m.stream) + '</td><td>' + esc(m.packets_received) + '</td><td>' + esc(m.packets_recovered) + '</td><td>' + esc(m.packets_lost) + '</td><td>' + esc(m.shards_late) + '</td></tr>');
            });
            h.push('</tbody></table>');
        }
        h.push('<div class="remote-section"><div class="remote-section-title">Open channel</div><div class="form-row">' +
            field('Stream id', 'media-stream', 1) +
            field('Ingress (bind)', 'media-ingress', '127.0.0.1:5004') +
            field('Egress (send to)', 'media-egress', '127.0.0.1:5006') +
            selectField('Profile', 'media-profile', 0, ['voice', 'video']) +
            '</div><div class="remote-actions"><button class="btn-primary" data-act="media-start">Open channel</button></div></div>');
        return h.join('');
    }
    // Minimal ANSI SGR renderer so coloured command output looks right.
    var ANSI_FG = { 30: '#4a5568', 31: '#f85149', 32: '#34d058', 33: '#e3a730', 34: '#58a6ff',
                    35: '#bc8cff', 36: '#0dcbf0', 37: '#ddeaf8',
                    90: '#6a8ba8', 91: '#ff7b72', 92: '#7ee787', 93: '#f2cc60', 94: '#79c0ff',
                    95: '#d2a8ff', 96: '#56d4dd', 97: '#ffffff' };
    function ansi(text) {
        // Honour carriage returns the way a terminal does: keep the last
        // overwrite of each line.
        var lines = String(text == null ? '' : text).split('\n').map(function(line) {
            var parts = line.split('\r');
            return parts[parts.length - 1];
        });
        return lines.map(function(line) {
            var out = '', open = 0, last = 0, re = /\x1b\[([0-9;]*)m/g, match;
            while ((match = re.exec(line)) !== null) {
                out += esc(line.slice(last, match.index));
                last = re.lastIndex;
                var codes = (match[1] || '0').split(';');
                for (var i = 0; i < codes.length; i++) {
                    var code = parseInt(codes[i], 10) || 0;
                    if (code === 0) { while (open > 0) { out += '</span>'; open--; } }
                    else if (code === 1) { out += '<span style="font-weight:700">'; open++; }
                    else if (ANSI_FG[code]) { out += '<span style="color:' + ANSI_FG[code] + '">'; open++; }
                }
            }
            out += esc(line.slice(last));
            while (open > 0) { out += '</span>'; open--; }
            // Strip any non-colour escape sequences (cursor moves, clears).
            return out.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '');
        }).join('\n');
    }
    function renderShellTab(n) {
        var log = (state.shell[state.selected] || []);
        var ps1 = '<span class="term-ps1"><span class="term-user">root</span>@<span class="term-host">' +
            esc(n.codename) + '</span>:<span class="term-cwd">~</span># </span>';
        var body = log.map(function(e) {
            if (e.kind === 'cmd') { return '<div class="term-line">' + ps1 + esc(e.text) + '</div>'; }
            if (e.kind === 'err') { return '<div class="term-line term-err">' + esc(e.text) + '</div>'; }
            if (e.kind === 'meta') { return '<div class="term-line term-meta">' + esc(e.text) + '</div>'; }
            return '<div class="term-line">' + ansi(e.text) + '</div>';
        }).join('');
        var running = !!state.shellBusy[state.selected];
        return '<div class="term-window' + (state.termFull ? ' term-window--full' : '') + '" id="term-window">' +
            '<div class="term-titlebar">' +
            '<span class="term-dots"><i class="term-dot term-dot--r"></i><i class="term-dot term-dot--y"></i><i class="term-dot term-dot--g"></i></span>' +
            '<span class="term-title">root@' + esc(n.codename) + ' — sh — over radio</span>' +
            '<span class="term-tools">' +
            '<button class="term-tool" data-act="shell-clear" title="Clear (Ctrl+L)">clear</button>' +
            '<button class="term-tool" data-act="shell-full" title="Toggle fullscreen">' + (state.termFull ? '&#10530;' : '&#10529;') + '</button>' +
            '</span></div>' +
            '<div class="term-screen" id="remote-term">' +
            (body || '<div class="term-line term-meta">Connected over the radio link. Commands run as root on ' + esc(n.codename) + '.</div>') +
            '<div class="term-line term-inputline">' + ps1 +
            '<input class="term-input" id="term-input" spellcheck="false" autocomplete="off" autocapitalize="off" autocorrect="off"' +
            (running ? ' disabled' : '') + '/>' +
            (running ? '<span class="term-spinner"></span>' : '') +
            '</div></div></div>';
    }
    function shellLog(kind, text) {
        var log = state.shell[state.selected] || (state.shell[state.selected] = []);
        log.push({ kind: kind, text: text });
        while (log.length > 400) { log.shift(); }
    }
    function runShell(command) {
        if (!command) { return; }
        var hist = state.shellHistory;
        if (hist[hist.length - 1] !== command) { hist.push(command); }
        state.shellHistoryPos = hist.length;
        shellLog('cmd', command);
        var node = state.selected;
        state.shellBusy[node] = true;
        renderDetail();
        api('POST', nodeUrl('/shell', node), { command: command, timeout_secs: 30 })
            .then(function(r) {
                if (r.chunk) { shellLog('out', r.chunk.replace(/\n$/, '')); }
                shellLog('meta', 'exit ' + r.code + ' · ' + r.total + ' B · ' + r.duration_ms + ' ms' + (r.truncated ? ' · truncated' : ''));
            })
            .catch(function(e) { shellLog('err', e.message); })
            .then(function() {
                state.shellBusy[node] = false;
                if (state.selected === node && state.tab === 'shell') {
                    renderDetail();
                    var term = $('remote-term');
                    if (term) { term.scrollTop = term.scrollHeight; }
                    var input = $('term-input');
                    if (input) { input.focus(); }
                }
            });
    }
    function renderSystemTab(n) {
        return '<div class="remote-section"><div class="remote-section-title">Services</div>' +
            '<div class="remote-actions"><button class="btn-secondary" data-act="restart-gateway">Restart gateway</button>' +
            '<button class="btn-secondary" data-act="restart-commd">Restart radio daemon</button></div></div>' +
            '<div class="remote-section"><div class="remote-section-title">Power</div>' +
            '<div class="remote-actions"><button class="btn-secondary btn-danger" data-act="reboot">Reboot ' + esc(n.codename) + '</button></div></div>';
    }
    function field(label, id, value) {
        return '<div class="form-group"><label class="form-label" for="' + id + '">' + esc(label) + '</label><input class="form-input" id="' + id + '" value="' + esc(value) + '"/></div>';
    }
    function selectField(label, id, value, options) {
        return '<div class="form-group"><label class="form-label" for="' + id + '">' + esc(label) + '</label><select class="form-input" id="' + id + '">' +
            options.map(function(o, i) { return '<option value="' + i + '"' + (i === value ? ' selected' : '') + '>' + esc(o) + '</option>'; }).join('') + '</select></div>';
    }
    function num(id) { var v = parseInt(($(id) || {}).value, 10); return isNaN(v) ? 0 : v; }
    function action(btn) {
        var act = btn.getAttribute('data-act');
        var run = null;
        var n = selectedNode() || {};
        var confirmMsg = { unpair: 'Remove pairing with ' + n.codename + '?', reboot: 'Reboot ' + n.codename + '?', 'restart-gateway': 'Restart the gateway service on ' + n.codename + '?', 'restart-commd': 'Restart the radio daemon on ' + n.codename + '? The link will drop briefly.' }[act];
        if (confirmMsg && !window.confirm(confirmMsg)) { return; }
        switch (act) {
            case 'pair': run = api('POST', nodeUrl('/pair')).then(function(d) { flash('Pairing: ' + d.pairing, 'ok'); }); break;
            case 'cancel': run = api('POST', nodeUrl('/cancel')).then(function(d) { flash(d.detail, 'ok'); }); break;
            case 'approve': run = api('POST', nodeUrl('/approve')).then(function(d) { flash(d.detail, 'ok'); }); break;
            case 'reject': run = api('POST', nodeUrl('/reject')).then(function(d) { flash(d.detail, 'ok'); }); break;
            case 'unpair': run = api('POST', nodeUrl('/unpair')).then(function(d) { flash(d.detail, 'ok'); }); break;
            case 'ping': run = api('POST', nodeUrl('/ping')).then(function(d) { state.rtts.push(d.rtt_ms); flash('Pong in ' + d.rtt_ms + ' ms', 'ok'); renderDetail(); }); break;
            case 'info': run = api('GET', nodeUrl('/info')).then(function(d) { state.info = d; renderDetail(); }); break;
            case 'radio-load': {
                var m = parseInt(btn.getAttribute('data-module'), 10);
                run = api('GET', nodeUrl('/radio/' + m)).then(function(d) { state.radio[m] = d; renderDetail(); });
                break;
            }
            case 'radio-apply': {
                var mm = parseInt(btn.getAttribute('data-module'), 10);
                var cfg = {
                    module: mm, freq_hz: num('rf-' + mm + '-freq'), spacing_hz: num('rf-' + mm + '-spacing'), channel: num('rf-' + mm + '-channel'),
                    bw_filter: num('rf-' + mm + '-bw'), mod_kind: num('rf-' + mm + '-kind'), mod_a: num('rf-' + mm + '-a'), mod_b: num('rf-' + mm + '-b'),
                    tx_power: num('rf-' + mm + '-tx'), accelerator: num('rf-' + mm + '-acc'), antenna: num('rf-' + mm + '-ant')
                };
                if (mm === 0 && !window.confirm('Radio A carries the Reticulum link to this node. Apply anyway?')) { return; }
                run = api('PUT', nodeUrl('/radio/' + mm), cfg).then(function(d) { flash(d.detail, 'ok'); state.radio[mm] = cfg; });
                break;
            }
            case 'plugins': run = api('GET', nodeUrl('/plugins')).then(function(d) { state.plugins = d; renderDetail(); }); break;
            case 'plugin': {
                var id = btn.getAttribute('data-id'), pa = btn.getAttribute('data-plugin-act');
                run = api('POST', nodeUrl('/plugins/' + encodeURIComponent(id) + '/' + pa)).then(function(d) { flash(d.detail, 'ok'); return api('GET', nodeUrl('/plugins')); })
                    .then(function(d) { state.plugins = d; renderDetail(); });
                break;
            }
            case 'plugin-update': { $('remote-upload-id').value = btn.getAttribute('data-id'); $('remote-upload-file').click(); return; }
            case 'upload': {
                var input = $('remote-upload-file');
                if (!input.files || !input.files[0]) { flash('Choose a .zip package first', 'err'); return; }
                var form = new FormData();
                form.append('file', input.files[0], input.files[0].name);
                var pid = ($('remote-upload-id').value || '').trim();
                run = api('POST', nodeUrl('/plugins/upload' + (pid ? '?plugin_id=' + encodeURIComponent(pid) : '')), form, true)
                    .then(function(d) { flash('Transfer #' + d.job + ' started — ' + bytes(input.files[0].size) + ' over radio', 'ok'); });
                break;
            }
            case 'tag-save': {
                var tag = ($('node-tag').value || '').trim();
                run = api('PUT', nodeUrl('/tag'), { tag: tag }).then(function(d) { flash(d.detail, 'ok'); });
                break;
            }
            case 'shell-clear': { state.shell[state.selected] = []; renderDetail(); return; }
            case 'shell-full': {
                state.termFull = !state.termFull;
                document.body.classList.toggle('term-fullscreen', state.termFull);
                renderDetail();
                return;
            }
            case 'media-start': {
                var profile = ['voice', 'video'][num('media-profile')] || 'voice';
                run = api('POST', nodeUrl('/media'), { stream: num('media-stream'), ingress: ($('media-ingress').value || '').trim(), egress: ($('media-egress').value || '').trim(), profile: profile })
                    .then(function(d) { flash(d.detail, 'ok'); return refreshMedia(); }).then(function() { renderDetail(); });
                break;
            }
            case 'media-stop': {
                run = api('DELETE', nodeUrl('/media/' + btn.getAttribute('data-stream'))).then(function(d) { flash(d.detail, 'ok'); return refreshMedia(); }).then(function() { renderDetail(); });
                break;
            }
            case 'reboot': run = api('POST', nodeUrl('/reboot')).then(function(d) { flash(d.detail, 'ok'); }); break;
            case 'restart-gateway': run = api('POST', nodeUrl('/service/restart'), { unit: 'kaonic-gateway.service' }).then(function(d) { flash(d.detail, 'ok'); }); break;
            case 'restart-commd': run = api('POST', nodeUrl('/service/restart'), { unit: 'kaonic-commd.service' }).then(function(d) { flash(d.detail, 'ok'); }); break;
        }
        if (!run) { return; }
        btn.disabled = true;
        var label = btn.textContent;
        btn.textContent = 'Working…';
        run.catch(function(e) { flash(e.message, 'err'); }).then(function() { btn.disabled = false; btn.textContent = label; });
    }

    // ── Local settings ───────────────────────────────────────────────────────
    function refreshFec() {
        api('GET', '/api/remote/fec').then(function(f) {
            var sel = $('remote-fec-default');
            if (sel && sel.options.length !== f.classes.length) {
                sel.innerHTML = f.classes.map(function(c) { return '<option value="' + c + '">' + c + '</option>'; }).join('');
            }
            if (sel && document.activeElement !== sel) { sel.value = f.default_class; }
        }).catch(function() {});
    }
    var features = {};
    function refreshFeatures() {
        return api('GET', '/api/system/features').then(function(f) {
            features = f;
            ['remote', 'shell'].forEach(function(name) {
                var el = $('feature-' + name);
                if (el && document.activeElement !== el) { el.checked = !!f[name + '_enabled']; }
            });
            setText('feature-remote-label', f.remote_enabled
                ? (f.restart_required ? 'Restart to apply' : 'Running')
                : (f.restart_required ? 'Restart to stop' : 'Disabled'));
            setText('feature-shell-label', f.shell_enabled ? 'Shell enabled' : 'Shell off');
            var banner = $('remote-banner');
            if (banner) { banner.classList.toggle('remote-banner--restart', !!f.restart_required); }
        }).catch(function() {});
    }
    function bindFeatures() {
        ['remote', 'shell'].forEach(function(name) {
            var el = $('feature-' + name);
            if (!el) { return; }
            el.addEventListener('change', function() {
                if (name === 'shell' && el.checked &&
                    !window.confirm('Enable the remote shell?\n\nPaired nodes with shell permission will be able to run commands on THIS device as root.')) {
                    el.checked = false;
                    return;
                }
                api('PUT', '/api/system/features', {
                    vpn_enabled: features.vpn_enabled !== false,
                    remote_enabled: $('feature-remote').checked,
                    shell_enabled: $('feature-shell').checked,
                    restart_required: false
                }).then(function(f) {
                    flash(f.restart_required ? 'Saved — restart the gateway to apply' : 'Saved', 'ok');
                    refreshFeatures();
                }).catch(function(e) { flash(e.message, 'err'); refreshFeatures(); });
            });
        });
    }
    function refreshMedia() {
        return api('GET', '/api/remote/media').then(function(m) { state.media = m; }).catch(function() {});
    }
    function bindSettings() {
        var save = $('remote-settings-save');
        if (!save) { return; }
        refreshFec();
        setInterval(refreshFec, 5000);
        refreshMedia();
        setInterval(function() { refreshMedia().then(function() { if (state.tab === 'media' && !shouldPause()) { renderDetail(); } }); }, 3000);
        refreshFeatures();
        bindFeatures();
        setInterval(refreshFeatures, 10000);
        var sel = $('remote-fec-default');
        if (sel) {
            sel.addEventListener('change', function() {
                api('PUT', '/api/remote/fec', { class: sel.value })
                    .then(function(f) { flash('Default link coding: ' + f.default_class, 'ok'); })
                    .catch(function(e) { flash(e.message, 'err'); refreshFec(); });
            });
        }
        api('GET', '/api/remote/settings').then(function(s) {
            $('remote-set-announce').value = s.announce_secs;
            $('remote-set-gap').value = s.chunk_gap_ms;
            $('remote-set-parity').value = s.bulk_parity;
            $('remote-set-pairing').checked = !!s.accept_pairing;
        }).catch(function() {});
        save.addEventListener('click', function() {
            save.disabled = true;
            api('PUT', '/api/remote/settings', { announce_secs: num('remote-set-announce'), chunk_gap_ms: num('remote-set-gap'), bulk_parity: num('remote-set-parity'), accept_pairing: $('remote-set-pairing').checked })
                .then(function(d) { flash(d.detail, 'ok'); })
                .catch(function(e) { flash(e.message, 'err'); })
                .then(function() { save.disabled = false; });
        });
    }

    function render() {
        var snap = state.snapshot;
        if (!snap) { return; }
        if (state.selected && !selectedNode()) { state.selected = null; }
        renderStats(snap); renderMap(snap); renderNodes(snap); renderIncoming(snap); renderJobs(snap); renderEvents(snap); renderDetail();
    }
    function shouldPause() {
        if (shouldPauseShell()) { return true; }
        var a = document.activeElement;
        return !!(a && (a.tagName === 'INPUT' || a.tagName === 'SELECT' || a.tagName === 'TEXTAREA'));
    }

    var initial = $('remote-initial');
    if (initial) { try { state.snapshot = JSON.parse(initial.textContent); } catch (e) {} }
    render();
    bindSettings();
    window.addEventListener('resize', function() { if (state.snapshot) { renderMap(state.snapshot); } });

    function connect() {
        var ws = new WebSocket(proto + '//' + location.host + '/api/ws/status');
        ws.onmessage = function(ev) {
            try {
                var msg = JSON.parse(ev.data) || {};
                if (msg.type !== 'remote') { return; }
                state.snapshot = msg.data;
                if (shouldPause()) { renderStats(msg.data); renderMap(msg.data); renderNodes(msg.data); renderJobs(msg.data); renderIncoming(msg.data); renderEvents(msg.data); return; }
                render();
            } catch (e) {}
        };
        ws.onclose = function() { setTimeout(connect, 3000); };
    }
    connect();
    setInterval(function() {
        api('GET', '/api/remote/snapshot').then(function(d) { state.snapshot = d; if (!shouldPause()) { render(); } }).catch(function() {});
    }, 30000);
})();
"##;

#[component]
pub fn RemotePage() -> impl IntoView {
    let snapshot = Resource::new(|| (), |_| load_remote_snapshot());

    view! {
        <div class="page">
            <PageTitle icon="📡" title="Remote" />
            <Suspense fallback=|| view! { <p class="loading">"Loading…"</p> }>
                {move || match snapshot.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(e)) => view! {
                        <div class="error-banner">"Error: "{e.to_string()}</div>
                    }.into_any(),
                    Some(Ok(None)) => view! {
                        <div class="error-banner">"Remote control is unavailable: the gateway is running in webapp-only mode without a Reticulum transport."</div>
                    }.into_any(),
                    Some(Ok(Some(snapshot))) => view! { <RemoteContent snapshot=snapshot/> }.into_any(),
                }}
            </Suspense>
        </div>
    }
}

#[component]
fn RemoteContent(snapshot: RemoteSnapshot) -> impl IntoView {
    let initial = serde_json::to_string(&snapshot).unwrap_or_else(|_| "null".into());
    let online = snapshot.nodes.iter().filter(|n| n.online).count();
    let paired = snapshot.nodes.iter().filter(|n| n.paired).count();
    let pending = snapshot.incoming_requests.len();
    let node_count = snapshot.nodes.len();

    view! {
        <script type="application/json" id="remote-initial">{initial}</script>
        <div class="remote-flash" id="remote-flash" hidden=true></div>

        <div class="vpn-banner remote-banner" id="remote-banner">
            <div class="vpn-banner-lead">
                <span class="status-dot status-dot--ok" id="remote-status-dot"></span>
                <span class="vpn-banner-status-text" id="remote-status-text">{snapshot.local.codename.clone()}</span>
            </div>
            <div class="vpn-banner-divider"></div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Nodes"</span>
                <span class="vpn-banner-ip"><span id="remote-stat-online">{online}</span>" / "<span id="remote-stat-nodes">{node_count}</span>" online"</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Paired"</span>
                <span class="vpn-banner-ip" id="remote-stat-paired">{paired}</span>
            </div>
            <div class="vpn-banner-field">
                <span class="vpn-banner-label">"Pending"</span>
                <span class="vpn-banner-ip" id="remote-stat-pending">{pending}</span>
            </div>
            <div class="vpn-banner-field vpn-banner-field--hash">
                <span class="vpn-banner-label">"Identity"</span>
                <code class="vpn-banner-ip td-hash" id="remote-local-hash">{snapshot.local.identity_hash.clone()}</code>
            </div>
            <div class="vpn-banner-field feature-switches">
                <span class="vpn-banner-label">"Remote"</span>
                <div class="feature-switch-row">
                    <label class="feature-switch">
                        <input type="checkbox" id="feature-remote"/>
                        <span class="feature-switch-track"><span class="feature-switch-thumb"></span></span>
                        <span class="feature-switch-label" id="feature-remote-label">"Running"</span>
                    </label>
                    <label class="feature-switch">
                        <input type="checkbox" id="feature-shell"/>
                        <span class="feature-switch-track"><span class="feature-switch-thumb"></span></span>
                        <span class="feature-switch-label" id="feature-shell-label">"Shell off"</span>
                    </label>
                </div>
            </div>
        </div>

        <div class="remote-grid">
            <div class="card remote-map-card">
                <div class="card-header">
                    <span class="card-title">"Node map"</span>
                    <span class="card-body-text">"hops · signal"</span>
                </div>
                <svg id="remote-map" class="remote-map" viewBox="0 0 640 520" preserveAspectRatio="xMidYMid meet"></svg>
                <div class="remote-map-info" id="remote-map-info">
                    <span class="remote-map-info-hint">"Hover a node"</span>
                </div>
                <div class="remote-legend">
                    <span><i class="rm-legend-dot online"></i>"online"</span>
                    <span><i class="rm-legend-dot offline"></i>"offline"</span>
                    <span><i class="rm-legend-dot paired"></i>"paired"</span>
                    <span><i class="rm-legend-dot incoming"></i>"wants to pair"</span>
                    <span><i class="rm-legend-dot linked"></i>"link active"</span>
                </div>
            </div>
            <div class="card remote-detail-card" id="remote-detail-card">
                <div class="card-header"><span class="card-title">"Node"</span></div>
                <div id="remote-detail"></div>
            </div>
        </div>

        <div class="remote-grid remote-grid--secondary remote-grid--three">
            <div class="card">
                <div class="card-header">
                    <span class="card-title">"Pairing requests"</span>
                    <span class="badge badge-warn" id="remote-incoming-count">{pending}</span>
                </div>
                <div id="remote-incoming"></div>
            </div>
            <div class="card">
                <div class="card-header"><span class="card-title">"Transfers"</span></div>
                <div id="remote-jobs"></div>
            </div>
            <div class="card">
                <div class="card-header"><span class="card-title">"Local settings"</span></div>
                <div class="form-row remote-settings">
                    <div class="form-group">
                        <label class="form-label" for="remote-set-announce">"Announce interval (s)"</label>
                        <input class="form-input" id="remote-set-announce" type="number" min="5" max="255" value=snapshot.local.announce_secs.to_string()/>
                    </div>
                    <div class="form-group">
                        <label class="form-label" for="remote-set-gap">"Transfer pacing (ms/chunk)"</label>
                        <input class="form-input" id="remote-set-gap" type="number" min="0" max="500" value="12"/>
                    </div>
                    <div class="form-group">
                        <label class="form-label" for="remote-set-parity">"Transfer parity (per 16 chunks)"</label>
                        <input class="form-input" id="remote-set-parity" type="number" min="0" max="8" value="2"/>
                    </div>
                    <div class="form-group remote-check">
                        <label class="form-label" for="remote-set-pairing">"Accept pairing requests"</label>
                        <input id="remote-set-pairing" type="checkbox" checked=snapshot.local.accepts_pairing/>
                    </div>
                </div>
                <div class="remote-actions"><button class="btn-apply" id="remote-settings-save">"Save"</button></div>
                <div class="remote-section">
                    <div class="remote-section-title">"Link coding (runtime)"</div>
                    <div class="form-row remote-settings">
                        <div class="form-group">
                            <label class="form-label" for="remote-fec-default">"Default class"</label>
                            <select class="form-input" id="remote-fec-default"></select>
                        </div>
                    </div>
                </div>
            </div>
        </div>

        <div class="card remote-table-card">
            <div class="card-header"><span class="card-title">"All nodes"</span></div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead><tr><th></th><th>"Node"</th><th>"Hops"</th><th>"Signal"</th><th>"Trust"</th><th>"Seen"</th><th></th></tr></thead>
                    <tbody id="remote-node-rows"></tbody>
                </table>
            </div>
        </div>

        <div class="card remote-table-card">
            <div class="card-header"><span class="card-title">"Activity"</span></div>
            <div class="reticulum-table-wrap">
                <table class="frames-table">
                    <thead><tr><th>"When"</th><th>"Event"</th><th>"Node"</th><th>"Details"</th></tr></thead>
                    <tbody id="remote-events"></tbody>
                </table>
            </div>
        </div>

        <script>{REMOTE_JS}</script>
    }
}
