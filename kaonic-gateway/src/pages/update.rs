use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use super::PageTitle;
use reqwest;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SystemAbout {
    serial: String,
    hostname: String,
    os_details: String,
    architecture: String,
    cpu_model: String,
    cpu_cores: usize,
    ram_total_mb: u64,
    fs_total_mb: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: Option<String>,
    pub hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Versions {
    commd: VersionInfo,
    gateway: VersionInfo,
}

async fn fetch_versions() -> Result<Versions, ServerFnError> {
    const BASE: &str = "http://127.0.0.1:8682";
    let client = reqwest::Client::new();

    let commd = match client
        .get(format!("{BASE}/api/update/commd/version"))
        .send()
        .await
    {
        Ok(r) => r.json::<VersionInfo>().await.unwrap_or(VersionInfo {
            version: None,
            hash: None,
        }),
        Err(_) => VersionInfo {
            version: None,
            hash: None,
        },
    };

    let gateway = match client
        .get(format!("{BASE}/api/update/gateway/version"))
        .send()
        .await
    {
        Ok(r) => r.json::<VersionInfo>().await.unwrap_or(VersionInfo {
            version: None,
            hash: None,
        }),
        Err(_) => VersionInfo {
            version: None,
            hash: None,
        },
    };

    Ok(Versions { commd, gateway })
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
    let (_, ram_total_mb) = read_mem_mb();
    let (_, fs_total_mb) = read_fs_mb();

    Ok(SystemAbout {
        serial: state.serial.clone(),
        hostname: read_hostname(),
        os_details: read_os_details(),
        architecture: read_architecture(),
        cpu_model: read_cpu_model(),
        cpu_cores: read_cpu_cores(),
        ram_total_mb,
        fs_total_mb,
    })
}

const UPDATE_JS: &str = r#"
function kaonic_upload(target, inputId, statusId) {
    var input = document.getElementById(inputId);
    var status = document.getElementById(statusId);
    if (!input.files || !input.files[0]) {
        status.textContent = 'No file selected.';
        status.className = 'update-status err';
        return;
    }
    var form = new FormData();
    form.append('file', input.files[0]);
    status.textContent = 'Uploading…';
    status.className = 'update-status pending';
    fetch('/api/update/' + target + '/upload', { method: 'POST', body: form })
        .then(function(r) { return r.json(); })
        .then(function(d) {
            status.textContent = d.detail || 'Done';
            status.className = 'update-status ' + (d.detail && d.detail.toLowerCase().includes('fail') ? 'err' : 'ok');
        })
        .catch(function(e) {
            status.textContent = 'Error: ' + e;
            status.className = 'update-status err';
        });
}

(function() {
    function setRebootStatus(text, kind) {
        var status = document.getElementById('system-reboot-status');
        if (!status) { return; }
        status.textContent = text;
        status.className = kind || '';
    }

    function openRebootModal() {
        var modal = document.getElementById('system-reboot-modal');
        if (!modal) { return; }
        modal.hidden = false;
        document.body.classList.add('modal-open');
        setRebootStatus('', '');
    }

    function closeRebootModal() {
        var modal = document.getElementById('system-reboot-modal');
        if (!modal) { return; }
        modal.hidden = true;
        document.body.classList.remove('modal-open');
    }

    document.addEventListener('click', function(ev) {
        var target = ev.target;
        if (!(target instanceof Element)) { return; }

        if (target.closest('#system-reboot-open')) {
            openRebootModal();
            return;
        }

        if (target.closest('[data-close-system-reboot]')) {
            closeRebootModal();
            return;
        }

        if (target.id === 'system-reboot-modal') {
            closeRebootModal();
        }
    });

    var confirmBtn = document.getElementById('system-reboot-confirm');
    if (confirmBtn) {
        confirmBtn.addEventListener('click', function() {
            confirmBtn.disabled = true;
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
                    confirmBtn.disabled = false;
                });
        });
    }

    window.addEventListener('keydown', function(ev) {
        if (ev.key === 'Escape') {
            closeRebootModal();
        }
    });
})();
"#;

#[component]
pub fn SystemPage() -> impl IntoView {
    let versions = Resource::new(|| (), |_| fetch_versions());
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
                {move || match (about.get(), versions.get()) {
                    (None, _) | (_, None) => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    (Some(Err(err)), _) => view! { <div class="error-banner">{err.to_string()}</div> }.into_any(),
                    (Some(Ok(about)), Some(version_state)) => match version_state {
                        Err(_) => view! {
                            <SystemSections
                                about=about
                                commd=VersionInfo { version: None, hash: None }
                                gateway=VersionInfo { version: None, hash: None }
                            />
                        }.into_any(),
                        Ok(v) => view! {
                            <SystemSections about=about commd=v.commd gateway=v.gateway />
                        }.into_any(),
                    },
                }}
            </Suspense>
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
            <script>{UPDATE_JS}</script>
        </div>
    }
}

#[component]
fn SystemSections(about: SystemAbout, commd: VersionInfo, gateway: VersionInfo) -> impl IntoView {
    view! {
        <>
            <h2 class="section-title">"About"</h2>
            <SystemAboutSection about=about/>
            <h2 class="section-title">"Update"</h2>
            <div class="update-grid">
                <UpdateCard
                    title="Radio Driver (commd)"
                    target="commd"
                    file_id="file-commd"
                    status_id="status-commd"
                    version=commd.version
                    hash=commd.hash
                />
                <UpdateCard
                    title="Gateway"
                    target="gateway"
                    file_id="file-gateway"
                    status_id="status-gateway"
                    version=gateway.version
                    hash=gateway.hash
                />
            </div>
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

#[component]
fn UpdateCard(
    title: &'static str,
    target: &'static str,
    file_id: &'static str,
    status_id: &'static str,
    version: Option<String>,
    hash: Option<String>,
) -> impl IntoView {
    let ver_text = version.clone().unwrap_or_else(|| "Unknown".to_string());
    let hash_text = hash
        .clone()
        .map(|h| format!("{}…", &h[..h.len().min(16)]))
        .unwrap_or_else(|| "—".to_string());

    let onclick = format!("kaonic_upload('{target}', '{file_id}', '{status_id}')");

    view! {
        <div class="card">
            <div class="card-header">
                <span class="card-title">{title}</span>
                <span class="badge">{ver_text}</span>
            </div>
            <div class="info-row">
                <span class="info-label">"SHA-256"</span>
                <code class="info-value">{hash_text}</code>
            </div>
            <div class="update-upload">
                <div class="upload-row">
                    <input type="file" id=file_id accept=".zip" class="file-input"/>
                    <button class="btn-primary" onclick=onclick>"Apply Update"</button>
                </div>
                <div id=status_id class="update-status"></div>
            </div>
        </div>
    }
    .into_any()
}
