use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageTitle;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PluginMock {
    id: &'static str,
    name: &'static str,
    version: &'static str,
    service: &'static str,
    status: &'static str,
    summary: &'static str,
}

fn mock_plugins() -> Vec<PluginMock> {
    vec![
        PluginMock {
            id: "atak-bridge",
            name: "ATAK Bridge",
            version: "0.3.1",
            service: "kaonic-plugin-atak.service",
            status: "running",
            summary: "Bridges Cursor-on-Target traffic between ATAK clients and the local gateway stack.",
        },
        PluginMock {
            id: "mesh-recorder",
            name: "Mesh Recorder",
            version: "0.1.4",
            service: "kaonic-plugin-recorder.service",
            status: "stopped",
            summary: "Records plugin-facing radio and VPN metadata into rolling local archives for later export.",
        },
        PluginMock {
            id: "telemetry-exporter",
            name: "Telemetry Exporter",
            version: "0.2.0",
            service: "kaonic-plugin-telemetry.service",
            status: "error",
            summary: "Exports gateway health and network telemetry to an external collector over the VPN tunnel.",
        },
    ]
}

fn plugin_status_badge_class(status: &str) -> &'static str {
    match status {
        "running" | "active" => "badge badge-ok",
        "stopped" | "inactive" => "badge badge-warn",
        "error" | "failed" => "badge badge-err",
        _ => "badge",
    }
}

const PLUGINS_JS: &str = r#"
(function() {
    var source = document.getElementById('plugins-mock-data');
    if (!source) { return; }

    var plugins;
    try {
        plugins = JSON.parse(source.textContent || '[]');
    } catch (_) {
        plugins = [];
    }

    var state = {
        selectedId: plugins.length ? plugins[0].id : '',
        plugins: plugins.slice(),
    };

    function badgeClass(status) {
        var text = String(status || '').trim().toLowerCase();
        if (text === 'running' || text === 'active') { return 'badge badge-ok'; }
        if (text === 'stopped' || text === 'inactive') { return 'badge badge-warn'; }
        if (text === 'error' || text === 'failed') { return 'badge badge-err'; }
        return 'badge';
    }

    function setStatus(text, kind) {
        var el = document.getElementById('plugins-action-status');
        if (!el) { return; }
        el.textContent = text || '';
        el.className = 'plugins-action-status' + (kind ? ' ' + kind : '');
    }

    function currentPlugin() {
        return state.plugins.find(function(plugin) {
            return plugin.id === state.selectedId;
        }) || null;
    }

    function renderList() {
        var list = document.getElementById('plugins-list');
        if (!list) { return; }
        if (!state.plugins.length) {
            list.innerHTML = '<div class=\"plugins-empty\">No plugins installed.</div>';
            return;
        }
        list.innerHTML = state.plugins.map(function(plugin) {
            var active = plugin.id === state.selectedId ? ' plugins-list-item--active' : '';
            return '<button type=\"button\" class=\"plugins-list-item' + active + '\" data-plugin-select=\"' + plugin.id + '\">'
                + '<span class=\"plugins-list-name\">' + plugin.name + '</span>'
                + '<span class=\"plugins-list-meta\">v' + plugin.version + '</span>'
                + '<span class=\"' + badgeClass(plugin.status) + '\">' + plugin.status + '</span>'
                + '</button>';
        }).join('');
    }

    function renderDetails() {
        var panel = document.getElementById('plugins-detail');
        if (!panel) { return; }
        var plugin = currentPlugin();
        if (!plugin) {
            panel.innerHTML = '<div class=\"plugins-empty plugins-empty--detail\">Select a plugin from the left list.</div>';
            return;
        }
        var running = plugin.status === 'running' || plugin.status === 'active';
        panel.innerHTML = ''
            + '<div class=\"card-header\">'
                + '<div>'
                    + '<span class=\"card-title\">Plugin Details</span>'
                    + '<h2 class=\"plugins-detail-name\">' + plugin.name + '</h2>'
                + '</div>'
                + '<span class=\"' + badgeClass(plugin.status) + '\" id=\"plugins-service-status\">' + plugin.status + '</span>'
            + '</div>'
            + '<p class=\"card-body-text plugins-detail-summary\">' + plugin.summary + '</p>'
            + '<div class=\"plugins-detail-actions\">'
                + '<button type=\"button\" class=\"btn-primary\" data-plugin-toggle>' + (running ? 'Stop' : 'Start') + '</button>'
                + '<button type=\"button\" class=\"btn-secondary\" data-plugin-restart>Restart</button>'
                + '<button type=\"button\" class=\"btn-secondary plugins-delete-btn\" data-plugin-delete>Delete</button>'
            + '</div>'
            + '<div class=\"plugins-detail-grid\">'
                + '<div class=\"info-row\"><span class=\"info-label\">Name</span><span class=\"info-value\">' + plugin.name + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Version</span><span class=\"info-value\">' + plugin.version + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Service</span><code class=\"info-value\">' + plugin.service + '</code></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Status</span><span class=\"info-value\"><span class=\"' + badgeClass(plugin.status) + '\">' + plugin.status + '</span></span></div>'
            + '</div>';
    }

    function render() {
        renderList();
        renderDetails();
    }

    document.addEventListener('click', function(ev) {
        var target = ev.target;
        if (!(target instanceof HTMLElement)) { return; }

        var selectBtn = target.closest('[data-plugin-select]');
        if (selectBtn) {
            state.selectedId = selectBtn.getAttribute('data-plugin-select') || '';
            setStatus('', '');
            render();
            return;
        }

        if (target.closest('[data-plugin-install]')) {
            setStatus('Mock installer opened. Package upload flow is not wired yet.', 'ok');
            return;
        }

        var plugin = currentPlugin();
        if (!plugin) { return; }

        if (target.closest('[data-plugin-toggle]')) {
            plugin.status = (plugin.status === 'running' || plugin.status === 'active') ? 'stopped' : 'running';
            setStatus(plugin.name + ' is now ' + plugin.status + '.', 'ok');
            render();
            return;
        }

        if (target.closest('[data-plugin-restart]')) {
            plugin.status = 'running';
            setStatus(plugin.name + ' restarted.', 'ok');
            render();
            return;
        }

        if (target.closest('[data-plugin-delete]')) {
            state.plugins = state.plugins.filter(function(entry) { return entry.id !== plugin.id; });
            state.selectedId = state.plugins.length ? state.plugins[0].id : '';
            setStatus(plugin.name + ' removed from the mock list.', 'warn');
            render();
        }
    });

    render();
})();
"#;

#[component]
pub fn PluginsPage() -> impl IntoView {
    let plugins = mock_plugins();
    let initial = plugins.first().cloned();
    let plugins_json = serde_json::to_string(&plugins).unwrap_or_else(|_| "[]".into());

    view! {
        <div class="page">
            <div class="page-header">
                <div style="display:flex;align-items:center;gap:12px;flex-wrap:wrap;">
                    <PageTitle icon="🧩" title="Plugins" />
                    <span class="badge badge-warn">"Coming soon"</span>
                </div>
                <button type="button" class="btn-primary" data-plugin-install>"Install"</button>
            </div>

            <div class="card plugins-intro-card">
                <div class="card-header">
                    <span class="card-title">"Plugin Manager"</span>
                </div>
                <p class="card-body-text">
                    "Mock UI for managing gateway plugins. Installation and service actions are placeholders for the future backend flow."
                </p>
            </div>

            <div class="card plugins-shell">
                <div class="plugins-layout">
                    <PluginsList plugins=plugins.clone() selected=initial.as_ref().map(|plugin| plugin.id) />
                    <PluginsDetail plugin=initial />
                </div>
                <div id="plugins-action-status" class="plugins-action-status"></div>
            </div>

            <script id="plugins-mock-data" type="application/json">{plugins_json}</script>
            <script>{PLUGINS_JS}</script>
        </div>
    }
}

#[component]
fn PluginsList(plugins: Vec<PluginMock>, selected: Option<&'static str>) -> impl IntoView {
    view! {
        <aside class="plugins-sidebar">
            <div class="card-header">
                <span class="card-title">"Installed Plugins"</span>
                <span class="badge">{plugins.len().to_string()}</span>
            </div>
            <div class="plugins-list" id="plugins-list">
                {plugins
                    .into_iter()
                    .map(|plugin| {
                        let is_selected = selected == Some(plugin.id);
                        view! { <PluginListItem plugin=plugin selected=is_selected /> }
                    })
                    .collect_view()}
            </div>
        </aside>
    }
}

#[component]
fn PluginListItem(plugin: PluginMock, selected: bool) -> impl IntoView {
    let item_class = if selected {
        "plugins-list-item plugins-list-item--active"
    } else {
        "plugins-list-item"
    };
    let badge_class = plugin_status_badge_class(plugin.status);

    view! {
        <button type="button" class=item_class data-plugin-select=plugin.id>
            <span class="plugins-list-name">{plugin.name}</span>
            <span class="plugins-list-meta">{format!("v{}", plugin.version)}</span>
            <span class=badge_class>{plugin.status}</span>
        </button>
    }
}

#[component]
fn PluginsDetail(plugin: Option<PluginMock>) -> impl IntoView {
    view! {
        <section class="plugins-detail" id="plugins-detail">
            {plugin.map(|plugin| view! { <PluginDetailContent plugin=plugin /> })}
        </section>
    }
}

#[component]
fn PluginDetailContent(plugin: PluginMock) -> impl IntoView {
    let status_class = plugin_status_badge_class(plugin.status);
    let action_label = if plugin.status == "running" || plugin.status == "active" {
        "Stop"
    } else {
        "Start"
    };

    view! {
        <div class="card-header">
            <div>
                <span class="card-title">"Plugin Details"</span>
                <h2 class="plugins-detail-name">{plugin.name}</h2>
            </div>
            <span class=status_class id="plugins-service-status">{plugin.status}</span>
        </div>
        <p class="card-body-text plugins-detail-summary">{plugin.summary}</p>
        <div class="plugins-detail-actions">
            <button type="button" class="btn-primary" data-plugin-toggle>{action_label}</button>
            <button type="button" class="btn-secondary" data-plugin-restart>"Restart"</button>
            <button type="button" class="btn-secondary plugins-delete-btn" data-plugin-delete>"Delete"</button>
        </div>
        <div class="plugins-detail-grid">
            <PluginInfoRow label="Name" value=plugin.name code=false />
            <PluginInfoRow label="Version" value=plugin.version code=false />
            <PluginInfoRow label="Service" value=plugin.service code=true />
            <div class="info-row">
                <span class="info-label">"Status"</span>
                <span class="info-value"><span class=status_class>{plugin.status}</span></span>
            </div>
        </div>
    }
}

#[component]
fn PluginInfoRow(label: &'static str, value: &'static str, code: bool) -> impl IntoView {
    view! {
        <div class="info-row">
            <span class="info-label">{label}</span>
            {if code {
                view! { <code class="info-value">{value}</code> }.into_any()
            } else {
                view! { <span class="info-value">{value}</span> }.into_any()
            }}
        </div>
    }
}
