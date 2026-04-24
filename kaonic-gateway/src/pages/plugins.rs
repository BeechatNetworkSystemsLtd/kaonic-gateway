use leptos::prelude::*;

use super::PageTitle;

const PLUGINS_JS: &str = r#"
(function() {
    var state = {
        selectedId: '',
        plugins: [],
        loading: false,
    };

    function badgeClass(status) {
        var text = String(status || '').trim().toLowerCase();
        if (text.includes('running') || text === 'active') { return 'badge badge-ok'; }
        if (text.includes('stopped') || text.includes('inactive')) { return 'badge badge-warn'; }
        if (text.includes('error') || text.includes('failed')) { return 'badge badge-err'; }
        return 'badge';
    }

    function setStatus(text, kind) {
        var el = document.getElementById('plugins-action-status');
        if (!el) { return; }
        el.textContent = text || '';
        el.className = 'plugins-action-status' + (kind ? ' ' + kind : '');
    }

    function setLoading(loading) {
        state.loading = !!loading;
        var installBtn = document.querySelector('[data-plugin-install]');
        if (installBtn) { installBtn.disabled = state.loading; }
        var updateBtn = document.querySelector('[data-plugin-upload]');
        if (updateBtn) { updateBtn.disabled = state.loading; }
    }

    function currentPlugin() {
        return state.plugins.find(function(plugin) {
            return plugin.id === state.selectedId;
        }) || null;
    }

    function detailValue(value) {
        return value == null || value === '' ? '—' : String(value);
    }

    function formatTimestamp(value) {
        if (!value) { return '—'; }
        var date = new Date(Number(value) * 1000);
        if (Number.isNaN(date.getTime())) { return '—'; }
        return date.toLocaleString();
    }

    function escaped(value) {
        return String(value == null ? '' : value)
            .replace(/&/g, '&amp;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;')
            .replace(/\"/g, '&quot;')
            .replace(/'/g, '&#39;');
    }

    function ensureSelection() {
        if (!state.plugins.length) {
            state.selectedId = '';
            return;
        }
        var exists = state.plugins.some(function(plugin) { return plugin.id === state.selectedId; });
        if (!exists) {
            state.selectedId = state.plugins[0].id;
        }
    }

    function loadPlugins(message, kind) {
        setLoading(true);
        return fetch('/api/plugins')
            .then(function(resp) {
                if (!resp.ok) {
                    return resp.json().catch(function() { return {}; }).then(function(data) {
                        throw new Error(data.detail || ('HTTP ' + resp.status));
                    });
                }
                return resp.json();
            })
            .then(function(plugins) {
                state.plugins = Array.isArray(plugins) ? plugins : [];
                ensureSelection();
                if (message) { setStatus(message, kind || 'ok'); }
                else if (!state.plugins.length) { setStatus('No plugins installed yet.', ''); }
                render();
            })
            .catch(function(err) {
                setStatus('Error: ' + (err.message || err), 'warn');
            })
            .finally(function() {
                setLoading(false);
            });
    }

    function renderList() {
        var list = document.getElementById('plugins-list');
        var count = document.getElementById('plugins-count');
        if (count) { count.textContent = String(state.plugins.length); }
        if (!list) { return; }
        if (!state.plugins.length) {
            list.innerHTML = '<div class=\"plugins-empty\">No plugins installed.</div>';
            return;
        }
        list.innerHTML = state.plugins.map(function(plugin) {
            var active = plugin.id === state.selectedId ? ' plugins-list-item--active' : '';
            var kind = plugin.removable ? 'Plugin' : 'System';
            return '<button type=\"button\" class=\"plugins-list-item' + active + '\" data-plugin-select=\"' + plugin.id + '\">'
                + '<span class=\"plugins-list-name\">' + plugin.name + '</span>'
                + '<span class=\"plugins-list-meta\">' + kind + ' • v' + plugin.version + '</span>'
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
        var running = String(plugin.status || '').toLowerCase().includes('running') || String(plugin.status || '').toLowerCase() === 'active';
        var deleteAction = plugin.removable
            ? '<button type=\"button\" class=\"btn-secondary plugins-delete-btn\" data-plugin-delete>Delete</button>'
            : '';
        panel.innerHTML = ''
            + '<div class=\"card-header\">'
                + '<div>'
                    + '<span class=\"card-title\">Plugin Details</span>'
                    + '<h2 class=\"plugins-detail-name\">' + escaped(plugin.name) + '</h2>'
                + '</div>'
                + '<div class=\"plugins-detail-badges\">'
                    + (plugin.removable ? '<span class=\"badge\">Plugin</span>' : '<span class=\"badge badge-ok\">System</span>')
                    + (plugin.official ? '<span class=\"badge badge-ok\">Official</span>' : '<span class=\"badge\">Community</span>')
                    + '<span class=\"' + badgeClass(plugin.status) + '\" id=\"plugins-service-status\">' + escaped(plugin.status) + '</span>'
                + '</div>'
            + '</div>'
            + '<p class=\"card-body-text plugins-detail-summary\">' + escaped(plugin.description) + '</p>'
            + '<div class=\"plugins-detail-actions\">'
                + '<button type=\"button\" class=\"btn-primary\" data-plugin-toggle>' + (running ? 'Stop' : 'Start') + '</button>'
                + '<button type=\"button\" class=\"btn-secondary\" data-plugin-restart>Restart</button>'
                + '<button type=\"button\" class=\"btn-secondary\" data-plugin-upload>Upload update</button>'
                + deleteAction
            + '</div>'
            + '<div class=\"plugins-detail-grid\">'
                + '<div class=\"info-row\"><span class=\"info-label\">Name</span><span class=\"info-value\">' + escaped(detailValue(plugin.name)) + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Version</span><span class=\"info-value\">' + escaped(detailValue(plugin.version)) + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Type</span><span class=\"info-value\">' + (plugin.removable ? 'plugin' : 'system') + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Target</span><code class=\"info-value\">' + escaped(detailValue(plugin.target_name)) + '</code></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Service</span><code class=\"info-value\">' + escaped(detailValue(plugin.service)) + '</code></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Binary</span><code class=\"info-value\">' + escaped(detailValue(plugin.binary_name)) + '</code></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">bin_path</span><code class=\"info-value\">' + escaped(detailValue(plugin.bin_path)) + '</code></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">SHA-256</span><code class=\"info-value\">' + escaped(detailValue(plugin.sha256)) + '</code></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Developer</span><span class=\"info-value\">' + escaped(detailValue(plugin.developer)) + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Enabled</span><span class=\"info-value\">' + (plugin.enabled ? 'yes' : 'no') + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Installed</span><span class=\"info-value\">' + escaped(formatTimestamp(plugin.installed_at)) + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Updated</span><span class=\"info-value\">' + escaped(formatTimestamp(plugin.updated_at)) + '</span></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Install dir</span><code class=\"info-value\">' + escaped(detailValue(plugin.install_dir)) + '</code></div>'
                + '<div class=\"info-row\"><span class=\"info-label\">Package</span><code class=\"info-value\">' + escaped(detailValue(plugin.package_path)) + '</code></div>'
            + '</div>';
    }

    function render() {
        renderList();
        renderDetails();
    }

    function actionRequest(url, successKind, successMessage) {
        fetch(url, { method: 'POST' })
            .then(function(resp) {
                return resp.json().catch(function() { return {}; }).then(function(data) {
                    if (!resp.ok) {
                        throw new Error(data.detail || ('HTTP ' + resp.status));
                    }
                    return data;
                });
            })
            .then(function(data) {
                loadPlugins((data && data.detail) || successMessage, successKind || 'ok');
            })
            .catch(function(err) {
                setStatus('Error: ' + (err.message || err), 'warn');
            });
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
            var input = document.getElementById('plugin-package-input');
            if (input) {
                input.removeAttribute('data-plugin-upload-id');
                input.click();
            }
            return;
        }

        var plugin = currentPlugin();
        if (!plugin) { return; }

        if (target.closest('[data-plugin-toggle]')) {
            actionRequest(
                '/api/plugins/' + encodeURIComponent(plugin.id) + '/' + ((String(plugin.status || '').toLowerCase().includes('running') || String(plugin.status || '').toLowerCase() === 'active') ? 'stop' : 'start'),
                'ok',
                plugin.name + ' updated.'
            );
            return;
        }

        if (target.closest('[data-plugin-restart]')) {
            actionRequest('/api/plugins/' + encodeURIComponent(plugin.id) + '/restart', 'ok', plugin.name + ' restarted.');
            return;
        }

        if (target.closest('[data-plugin-upload]')) {
            var uploadInput = document.getElementById('plugin-package-input');
            if (uploadInput) {
                uploadInput.setAttribute('data-plugin-upload-id', plugin.id);
                uploadInput.click();
            }
            return;
        }

        if (target.closest('[data-plugin-delete]')) {
            fetch('/api/plugins/' + encodeURIComponent(plugin.id), { method: 'DELETE' })
                .then(function(resp) {
                    return resp.json().catch(function() { return {}; }).then(function(data) {
                        if (!resp.ok) {
                            throw new Error(data.detail || ('HTTP ' + resp.status));
                        }
                        return data;
                    });
                })
                .then(function(data) {
                    loadPlugins((data && data.detail) || (plugin.name + ' removed.'), 'warn');
                })
                .catch(function(err) {
                    setStatus('Error: ' + (err.message || err), 'warn');
                });
        }
    });

    var uploadInput = document.getElementById('plugin-package-input');
    if (uploadInput) {
        uploadInput.addEventListener('change', function() {
            var file = uploadInput.files && uploadInput.files[0];
            if (!file) { return; }
            var pluginId = uploadInput.getAttribute('data-plugin-upload-id');
            var form = new FormData();
            form.append('file', file);
            setLoading(true);
            setStatus((pluginId ? 'Uploading update ' : 'Installing ') + file.name + '…', 'ok');
            fetch(pluginId ? ('/api/plugins/' + encodeURIComponent(pluginId) + '/upload') : '/api/plugins/install', { method: 'POST', body: form })
                .then(function(resp) {
                    return resp.json().catch(function() { return {}; }).then(function(data) {
                        if (!resp.ok) {
                            throw new Error(data.detail || ('HTTP ' + resp.status));
                        }
                        return data;
                    });
                })
                .then(function(data) {
                    uploadInput.removeAttribute('data-plugin-upload-id');
                    uploadInput.value = '';
                    return loadPlugins((data && data.detail) || (pluginId ? 'Plugin updated.' : 'Plugin installed.'), 'ok');
                })
                .catch(function(err) {
                    uploadInput.removeAttribute('data-plugin-upload-id');
                    setStatus('Error: ' + (err.message || err), 'warn');
                })
                .finally(function() {
                    setLoading(false);
                });
        });
    }

    loadPlugins();
})();
"#;

#[component]
pub fn PluginsPage() -> impl IntoView {
    view! {
        <div class="page">
            <div class="page-header">
                <div style="display:flex;align-items:center;gap:12px;flex-wrap:wrap;">
                    <PageTitle icon="🧩" title="Plugins" />
                    <span class="badge">"Prototype"</span>
                </div>
                <button type="button" class="btn-primary" data-plugin-install>"Install"</button>
            </div>

            <div class="card plugins-intro-card">
                <div class="card-header">
                    <span class="card-title">"Plugin Manager"</span>
                </div>
                <p class="card-body-text">
                    "Install plugin ZIPs that include kaonic-plugin.toml, a service unit, the plugin binary, and an optional signature. Installed plugins are managed through the installer backend."
                </p>
                <input id="plugin-package-input" class="plugins-file-input" type="file" accept=".zip,application/zip" />
            </div>

            <div class="card plugins-shell">
                <div class="plugins-layout">
                    <PluginsList />
                    <PluginsDetail />
                </div>
                <div id="plugins-action-status" class="plugins-action-status"></div>
            </div>

            <script>{PLUGINS_JS}</script>
        </div>
    }
}

#[component]
fn PluginsList() -> impl IntoView {
    view! {
        <aside class="plugins-sidebar">
            <div class="card-header">
                <span class="card-title">"Installed Plugins"</span>
                <span class="badge" id="plugins-count">"0"</span>
            </div>
            <div class="plugins-list" id="plugins-list"><div class="plugins-empty">"Loading plugins…"</div></div>
        </aside>
    }
}

#[component]
fn PluginsDetail() -> impl IntoView {
    view! {
        <section class="plugins-detail" id="plugins-detail">
            <div class="plugins-empty plugins-empty--detail">
                "Select a plugin from the left list or install a new ZIP package."
            </div>
        </section>
    }
}
