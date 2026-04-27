use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageTitle;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemAbout {
    serial: String,
    codename: String,
    hostname: String,
    os_details: String,
    architecture: String,
    cpu_model: String,
    cpu_cores: usize,
    ram_total_mb: u64,
    fs_total_mb: u64,
}

#[server]
pub async fn load_system_about() -> Result<SystemAbout, ServerFnError> {
    use crate::state::AppState;
    use crate::system_metrics::{
        read_architecture, read_cpu_cores, read_cpu_model, read_fs_mb, read_hostname, read_mem_mb,
        read_os_details,
    };

    let state = leptos::context::use_context::<AppState>()
        .ok_or_else(|| ServerFnError::new("missing AppState context"))?;
    let codename = state
        .settings
        .lock()
        .map_err(|_| ServerFnError::new("settings lock poisoned"))?
        .load_or_create_codename()
        .map_err(|err| ServerFnError::new(err.to_string()))?;
    let (_, ram_total_mb) = read_mem_mb();
    let (_, fs_total_mb) = read_fs_mb();

    Ok(SystemAbout {
        serial: state.serial.clone(),
        codename,
        hostname: read_hostname(),
        os_details: read_os_details(),
        architecture: read_architecture(),
        cpu_model: read_cpu_model(),
        cpu_cores: read_cpu_cores(),
        ram_total_mb,
        fs_total_mb,
    })
}

const SYSTEM_JS: &str = r#"
(function() {
    function setModalOpen(modal, open) {
        if (!(modal instanceof HTMLElement)) { return; }
        modal.hidden = !open;
        if (!document.body) { return; }
        if (open) {
            document.body.classList.add('modal-open');
            return;
        }
        if (!document.querySelector('.modal-backdrop:not([hidden])')) {
            document.body.classList.remove('modal-open');
        }
    }

    function setRebootStatus(text, kind) {
        var status = document.getElementById('system-reboot-status');
        if (!status) { return; }
        status.textContent = text;
        status.className = kind || '';
    }

    function setCodenameStatus(text, kind) {
        var status = document.getElementById('system-codename-status');
        if (!status) { return; }
        status.textContent = text;
        status.className = kind || '';
    }

    function applyCodename(codename) {
        document.querySelectorAll('[data-system-codename-value]').forEach(function(node) {
            node.textContent = codename;
        });
    }

    function openRebootModal() {
        setRebootStatus('', '');
        setModalOpen(document.getElementById('system-reboot-modal'), true);
    }

    function closeRebootModal() {
        setModalOpen(document.getElementById('system-reboot-modal'), false);
    }

    function openCodenameModal() {
        var input = document.getElementById('system-codename-input');
        var current = document.querySelector('[data-system-codename-value]');
        if (!(input instanceof HTMLInputElement)) { return; }
        input.value = current ? String(current.textContent || '') : '';
        setCodenameStatus('', '');
        setModalOpen(document.getElementById('system-codename-modal'), true);
        window.setTimeout(function() {
            input.focus();
            input.select();
        }, 0);
    }

    function closeCodenameModal() {
        setModalOpen(document.getElementById('system-codename-modal'), false);
    }

    document.addEventListener('click', function(ev) {
        var target = ev.target;
        if (!(target instanceof Element)) { return; }

        if (target.closest('#system-reboot-open')) {
            openRebootModal();
            return;
        }

        if (target.closest('#system-codename-open')) {
            openCodenameModal();
            return;
        }

        if (target.closest('[data-close-system-reboot]')) {
            closeRebootModal();
            return;
        }

        if (target.closest('[data-close-system-codename]')) {
            closeCodenameModal();
            return;
        }

        if (target.id === 'system-reboot-modal') {
            closeRebootModal();
            return;
        }

        if (target.id === 'system-codename-modal') {
            closeCodenameModal();
        }
    });

    var rebootBtn = document.getElementById('system-reboot-confirm');
    if (rebootBtn) {
        rebootBtn.addEventListener('click', function() {
            rebootBtn.disabled = true;
            setRebootStatus('Requesting reboot…', 'flash-ok');
            fetch('/api/system/reboot', { method: 'POST' })
                .then(function(resp) {
                    if (!resp.ok) {
                        return resp.text().then(function(text) {
                            throw new Error(text || ('HTTP ' + resp.status));
                        });
                    }
                    return resp.json();
                })
                .then(function(data) {
                    setRebootStatus((data && data.status) || 'Reboot requested', 'flash-ok');
                })
                .catch(function(err) {
                    setRebootStatus('Error: ' + (err.message || err), 'flash-err');
                    rebootBtn.disabled = false;
                });
        });
    }

    var renameBtn = document.getElementById('system-codename-confirm');
    if (renameBtn) {
        renameBtn.addEventListener('click', function() {
            var input = document.getElementById('system-codename-input');
            if (!(input instanceof HTMLInputElement)) { return; }
            renameBtn.disabled = true;
            setCodenameStatus('Saving…', '');
            fetch('/api/system/codename', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ codename: String(input.value || '').trim().toLowerCase() })
            })
                .then(function(resp) {
                    if (!resp.ok) {
                        return resp.text().then(function(text) {
                            throw new Error(text || ('HTTP ' + resp.status));
                        });
                    }
                    return resp.json();
                })
                .then(function(data) {
                    var codename = String((data && data.codename) || '').trim();
                    if (codename) { applyCodename(codename); }
                    closeCodenameModal();
                })
                .catch(function(err) {
                    setCodenameStatus('Error: ' + (err.message || err), 'flash-err');
                })
                .finally(function() {
                    renameBtn.disabled = false;
                });
        });
    }

    window.addEventListener('keydown', function(ev) {
        var codenameModal = document.getElementById('system-codename-modal');
        if (ev.key === 'Escape') {
            closeRebootModal();
            closeCodenameModal();
            return;
        }
        if (
            ev.key === 'Enter' &&
            codenameModal instanceof HTMLElement &&
            !codenameModal.hidden &&
            document.activeElement &&
            document.activeElement.id === 'system-codename-input' &&
            renameBtn
        ) {
            renameBtn.click();
        }
    });
})();
"#;

#[component]
pub fn SystemPage() -> impl IntoView {
    let about = Resource::new(|| (), |_| load_system_about());

    view! {
        <div class="page">
            <div class="page-header">
                <PageTitle icon="🖥️" title="System" />
                <button type="button" id="system-reboot-open" class="btn-secondary system-reboot-btn">
                    "Reboot"
                </button>
            </div>
            <Suspense fallback=|| view! { <p class="loading">"Loading system details…"</p> }>
                {move || match about.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(err)) => view! { <div class="error-banner">{err.to_string()}</div> }.into_any(),
                    Some(Ok(about)) => view! { <SystemSections about=about /> }.into_any(),
                }}
            </Suspense>
            <div class="modal-backdrop" id="system-codename-modal" hidden>
                <div class="modal-card">
                    <div class="modal-header">
                        <h2 class="modal-title">"Rename codename"</h2>
                        <button type="button" class="modal-close" data-close-system-codename>"×"</button>
                    </div>
                    <div class="modal-form">
                        <p class="card-body-text">
                            "Set a new 8-character codename using letters and digits."
                        </p>
                        <input
                            type="text"
                            id="system-codename-input"
                            class="form-input system-codename-input"
                            maxlength="8"
                            spellcheck="false"
                            autocapitalize="none"
                            autocomplete="off"
                        />
                        <p class="system-codename-hint">"Exactly 8 letters or digits."</p>
                        <div id="system-codename-status"></div>
                        <div class="modal-actions">
                            <button type="button" class="btn-secondary" data-close-system-codename>
                                "Cancel"
                            </button>
                            <button type="button" id="system-codename-confirm" class="btn-primary">
                                "Rename"
                            </button>
                        </div>
                    </div>
                </div>
            </div>
            <div class="modal-backdrop" id="system-reboot-modal" hidden>
                <div class="modal-card">
                    <div class="modal-header">
                        <h2 class="modal-title">"Confirm reboot"</h2>
                        <button type="button" class="modal-close" data-close-system-reboot>"×"</button>
                    </div>
                    <div class="modal-form">
                        <p class="card-body-text">
                            "Are you sure you want to reboot the device now?"
                        </p>
                        <div id="system-reboot-status"></div>
                        <div class="modal-actions">
                            <button type="button" class="btn-secondary" data-close-system-reboot>
                                "Cancel"
                            </button>
                            <button type="button" id="system-reboot-confirm" class="btn-primary">
                                "Reboot"
                            </button>
                        </div>
                    </div>
                </div>
            </div>
            <script>{SYSTEM_JS}</script>
        </div>
    }
}

#[component]
fn SystemSections(about: SystemAbout) -> impl IntoView {
    view! {
        <>
            <h2 class="section-title">"About"</h2>
            <SystemAboutSection about=about/>
        </>
    }
    .into_any()
}

#[component]
fn SystemAboutSection(about: SystemAbout) -> impl IntoView {
    view! {
        <div class="system-about-grid">
            <div class="card">
                <div class="card-header">
                    <span class="card-title">"Device"</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"Serial"</span>
                    <code class="info-value">{about.serial}</code>
                </div>
                <div class="info-row">
                    <span class="info-label">"Codename"</span>
                    <div class="system-codename-row">
                        <code class="info-value" data-system-codename-value>{about.codename}</code>
                        <button type="button" class="btn-secondary system-codename-btn" id="system-codename-open">
                            "Rename"
                        </button>
                    </div>
                </div>
                <div class="info-row">
                    <span class="info-label">"Hostname"</span>
                    <span class="info-value">{about.hostname}</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"OS"</span>
                    <span class="info-value">{about.os_details}</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"Arch"</span>
                    <span class="info-value">{about.architecture}</span>
                </div>
            </div>

            <div class="card">
                <div class="card-header">
                    <span class="card-title">"Specs"</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"CPU"</span>
                    <span class="info-value">{about.cpu_model}</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"Cores"</span>
                    <span class="info-value">{about.cpu_cores.to_string()}</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"Memory"</span>
                    <span class="info-value">{format_storage_mb(about.ram_total_mb)}</span>
                </div>
                <div class="info-row">
                    <span class="info-label">"Storage"</span>
                    <span class="info-value">{format_storage_mb(about.fs_total_mb)}</span>
                </div>
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
