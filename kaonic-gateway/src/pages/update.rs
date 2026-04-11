use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use reqwest;

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
"#;

#[component]
pub fn UpdatePage() -> impl IntoView {
    let versions = Resource::new(|| (), |_| fetch_versions());

    view! {
        <div class="page">
            <h1 class="page-title">"System Update"</h1>
            <Suspense fallback=|| view! { <p class="loading">"Loading version info…"</p> }>
                {move || match versions.get() {
                    None => view! { <p class="loading">"Loading…"</p> }.into_any(),
                    Some(Err(_)) => view! {
                        <UpdateCards
                            commd=VersionInfo { version: None, hash: None }
                            gateway=VersionInfo { version: None, hash: None }
                        />
                    }.into_any(),
                    Some(Ok(v)) => view! {
                        <UpdateCards commd=v.commd gateway=v.gateway />
                    }.into_any(),
                }}
            </Suspense>
            <script>{UPDATE_JS}</script>
        </div>
    }
}

#[component]
fn UpdateCards(commd: VersionInfo, gateway: VersionInfo) -> impl IntoView {
    view! {
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
    }
    .into_any()
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
