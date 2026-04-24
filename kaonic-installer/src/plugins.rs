use std::ffi::OsStr;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::installer::{apply_update, Target};

const MANIFEST_NAME: &str = "kaonic-plugin.toml";

#[derive(Debug, Clone, Copy)]
pub enum PluginAction {
    Start,
    Stop,
    Restart,
}

impl PluginAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PluginError {
    pub status: StatusCode,
    pub detail: String,
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for PluginError {}

impl PluginError {
    fn bad_request(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            detail: detail.into(),
        }
    }

    fn not_found(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            detail: detail.into(),
        }
    }

    fn internal(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            detail: detail.into(),
        }
    }
}

type PluginResult<T> = Result<T, PluginError>;

#[derive(Debug, Clone)]
pub struct CorePluginSpec {
    pub target: Arc<Target>,
    pub plugin_id: String,
    pub name: String,
    pub description: String,
    pub developer: String,
}

impl CorePluginSpec {
    pub fn new(
        target: Arc<Target>,
        name: impl Into<String>,
        description: impl Into<String>,
        developer: impl Into<String>,
    ) -> Self {
        let plugin_id = target
            .bin_path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(target.name)
            .to_string();
        Self {
            target,
            plugin_id,
            name: name.into(),
            description: description.into(),
            developer: developer.into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PluginManifest {
    name: String,
    description: String,
    version: String,
    service: String,
    developer: String,
    #[serde(default)]
    bin_path: Option<String>,
}

#[derive(Debug, Clone)]
struct PluginPackage {
    manifest: PluginManifest,
    manifest_raw: String,
    id: String,
    binary_name: String,
    service_name: String,
    bin_path: Option<String>,
    sha256: String,
    binary_bytes: Vec<u8>,
    service_bytes: Vec<u8>,
    signature_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct PluginRecord {
    id: String,
    name: String,
    description: String,
    version: String,
    service: String,
    developer: String,
    binary_name: String,
    bin_path: Option<String>,
    sha256: String,
    install_dir: String,
    package_path: String,
    official: bool,
    enabled: bool,
    removable: bool,
    target_name: Option<String>,
    installed_at: u64,
    updated_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub service: String,
    pub developer: String,
    pub binary_name: String,
    pub bin_path: Option<String>,
    pub sha256: String,
    pub install_dir: String,
    pub package_path: String,
    pub official: bool,
    pub enabled: bool,
    pub removable: bool,
    pub target_name: Option<String>,
    pub status: String,
    pub installed_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginMessage {
    pub detail: String,
}

pub fn initialize_store(
    plugins_root: &Path,
    db_path: &Path,
    cert_path: Option<&Path>,
    core_plugins: &[CorePluginSpec],
) -> PluginResult<()> {
    fs::create_dir_all(plugins_root)
        .map_err(|err| PluginError::internal(format!("create plugin root: {err}")))?;
    let conn = open_db(db_path)?;
    init_db(&conn)?;
    discover_plugins(plugins_root, &conn, cert_path)?;
    sync_core_plugins(&conn, core_plugins)?;
    Ok(())
}

pub fn list_plugins(
    plugins_root: &Path,
    db_path: &Path,
    cert_path: Option<&Path>,
    core_plugins: &[CorePluginSpec],
) -> PluginResult<Vec<PluginSummary>> {
    initialize_store(plugins_root, db_path, cert_path, core_plugins)?;
    let conn = open_db(db_path)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, name, description, version, service, developer, binary_name, bin_path, sha256, install_dir, package_path, official, enabled, removable, target_name, installed_at, updated_at
             FROM plugins
             ORDER BY name COLLATE NOCASE, id",
        )
        .map_err(|err| PluginError::internal(format!("prepare plugin list: {err}")))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(PluginRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                version: row.get(3)?,
                service: row.get(4)?,
                developer: row.get(5)?,
                binary_name: row.get(6)?,
                bin_path: row.get(7)?,
                sha256: row.get(8)?,
                install_dir: row.get(9)?,
                package_path: row.get(10)?,
                official: row.get::<_, i64>(11)? != 0,
                enabled: row.get::<_, i64>(12)? != 0,
                removable: row.get::<_, i64>(13)? != 0,
                target_name: row.get(14)?,
                installed_at: row.get(15)?,
                updated_at: row.get(16)?,
            })
        })
        .map_err(|err| PluginError::internal(format!("query plugins: {err}")))?;

    let mut records = Vec::new();
    for row in rows {
        let record = row.map_err(|err| PluginError::internal(format!("read plugin row: {err}")))?;
        records.push(PluginSummary {
            status: read_service_status(&record.service),
            id: record.id,
            name: record.name,
            description: record.description,
            version: record.version,
            service: record.service,
            developer: record.developer,
            binary_name: record.binary_name,
            bin_path: record.bin_path,
            sha256: record.sha256,
            install_dir: record.install_dir,
            package_path: record.package_path,
            official: record.official,
            enabled: record.enabled,
            removable: record.removable,
            target_name: record.target_name,
            installed_at: record.installed_at,
            updated_at: record.updated_at,
        });
    }

    Ok(records)
}

pub fn install_plugin(
    plugins_root: &Path,
    db_path: &Path,
    systemd_dir: &Path,
    cert_path: &Path,
    core_plugins: &[CorePluginSpec],
    zip_bytes: &[u8],
) -> PluginResult<PluginMessage> {
    initialize_store(plugins_root, db_path, Some(cert_path), core_plugins)?;
    let package = parse_plugin_package(zip_bytes)?;
    let conn = open_db(db_path)?;
    let existing = load_plugin(&conn, &package.id)?;
    ensure_service_available(&conn, &package.service_name, Some(&package.id))?;
    ensure_bin_path_available(&conn, package.bin_path.as_deref(), Some(&package.id))?;

    let plugin_dir = plugins_root.join(&package.id);
    let current_dir = plugin_dir.join("current");
    let package_path = plugin_dir.join("package.zip");
    let manifest_path = plugin_dir.join(MANIFEST_NAME);
    let stored_service_path = plugin_dir.join(&package.service_name);
    let stored_hash_path = plugin_dir.join(format!("{}.sha256", package.binary_name));
    let stored_signature_path = plugin_dir.join(format!("{}.sign", package.binary_name));
    let service_path = systemd_dir.join(&package.service_name);
    let binary_path = current_dir.join(&package.binary_name);
    let binary_target = binary_path.clone();
    let now = now_secs();
    let official = evaluate_official(
        cert_path,
        &package.binary_bytes,
        package.signature_bytes.as_deref(),
    )?;
    let should_enable = existing
        .as_ref()
        .map(|record| record.enabled)
        .unwrap_or(true);
    let was_running = existing
        .as_ref()
        .map(|record| is_service_running(&record.service))
        .unwrap_or(false);
    let should_start = existing.is_none() || was_running;
    let rollback_dir = plugin_dir.join(".rollback");
    let had_existing_service = service_path.exists();

    fs::create_dir_all(&plugin_dir)
        .map_err(|err| PluginError::internal(format!("create plugin dir: {err}")))?;
    prepare_rollback(
        &plugin_dir,
        &rollback_dir,
        &service_path,
        &package.service_name,
    )?;
    prepare_bin_path_rollback(
        &rollback_dir,
        existing
            .as_ref()
            .and_then(|record| record.bin_path.as_deref()),
        existing.as_ref().map(|record| {
            plugin_binary_target(Path::new(&record.install_dir), &record.binary_name)
        }),
    )?;

    if existing.is_some() || had_existing_service {
        let _ = systemctl("stop", &package.service_name);
    }

    let install_result = (|| -> PluginResult<()> {
        if current_dir.exists() {
            fs::remove_dir_all(&current_dir).map_err(|err| {
                PluginError::internal(format!("remove previous plugin files: {err}"))
            })?;
        }
        fs::create_dir_all(&current_dir)
            .map_err(|err| PluginError::internal(format!("create current plugin dir: {err}")))?;

        fs::write(&binary_path, &package.binary_bytes)
            .map_err(|err| PluginError::internal(format!("write plugin binary: {err}")))?;
        make_executable(&binary_path)
            .map_err(|err| PluginError::internal(format!("chmod plugin binary: {err}")))?;
        fs::write(&package_path, zip_bytes)
            .map_err(|err| PluginError::internal(format!("persist plugin package: {err}")))?;
        fs::write(&manifest_path, package.manifest_raw.as_bytes())
            .map_err(|err| PluginError::internal(format!("persist plugin manifest: {err}")))?;
        fs::write(&stored_service_path, &package.service_bytes)
            .map_err(|err| PluginError::internal(format!("persist plugin service: {err}")))?;
        fs::write(&stored_hash_path, format!("{}\n", package.sha256))
            .map_err(|err| PluginError::internal(format!("persist plugin checksum: {err}")))?;
        if let Some(signature) = &package.signature_bytes {
            fs::write(&stored_signature_path, signature)
                .map_err(|err| PluginError::internal(format!("persist plugin signature: {err}")))?;
        } else if stored_signature_path.exists() {
            fs::remove_file(&stored_signature_path).map_err(|err| {
                PluginError::internal(format!("remove stale plugin signature: {err}"))
            })?;
        }

        fs::create_dir_all(systemd_dir)
            .map_err(|err| PluginError::internal(format!("create systemd dir: {err}")))?;
        fs::write(&service_path, &package.service_bytes)
            .map_err(|err| PluginError::internal(format!("install systemd unit: {err}")))?;
        daemon_reload()?;
        install_bin_path(
            package.bin_path.as_deref(),
            existing
                .as_ref()
                .and_then(|record| record.bin_path.as_deref()),
            &binary_target,
            &rollback_dir,
        )?;

        if should_enable {
            let _ = systemctl("enable", &package.service_name);
        } else {
            let _ = systemctl("disable", &package.service_name);
        }

        if should_start {
            systemctl("start", &package.service_name)
                .map_err(|err| PluginError::internal(format!("start plugin service: {err}")))?;
            if !poll_running(&package.service_name, 10, Duration::from_secs(1)) {
                return Err(PluginError::internal(
                    "plugin failed health check after install; rolled back",
                ));
            }
        }

        upsert_plugin(
            &conn,
            PluginRecord {
                id: package.id.clone(),
                name: package.manifest.name.clone(),
                description: package.manifest.description.clone(),
                version: package.manifest.version.clone(),
                service: package.service_name.clone(),
                developer: package.manifest.developer.clone(),
                binary_name: package.binary_name.clone(),
                bin_path: package.bin_path.clone(),
                sha256: package.sha256.clone(),
                install_dir: plugin_dir.to_string_lossy().into_owned(),
                package_path: package_path.to_string_lossy().into_owned(),
                official,
                enabled: should_enable,
                removable: true,
                target_name: None,
                installed_at: existing
                    .as_ref()
                    .map(|record| record.installed_at)
                    .unwrap_or(now),
                updated_at: now,
            },
        )?;

        Ok(())
    })();

    if let Err(err) = install_result {
        let _ = systemctl("stop", &package.service_name);
        let _ = rollback_install(
            &plugin_dir,
            &rollback_dir,
            &service_path,
            &package.service_name,
            package.bin_path.as_deref(),
            &binary_target,
            existing.as_ref(),
        );
        return Err(err);
    }

    cleanup_rollback(&rollback_dir);
    Ok(PluginMessage {
        detail: if existing.is_some() {
            format!(
                "Updated plugin {} to {}",
                package.manifest.name, package.manifest.version
            )
        } else {
            format!(
                "Installed plugin {} {}",
                package.manifest.name, package.manifest.version
            )
        },
    })
}

pub fn control_plugin(
    db_path: &Path,
    plugin_id: &str,
    action: PluginAction,
) -> PluginResult<PluginMessage> {
    let conn = open_db(db_path)?;
    init_db(&conn)?;
    let plugin = load_plugin(&conn, plugin_id)?
        .ok_or_else(|| PluginError::not_found(format!("unknown plugin: {plugin_id}")))?;

    systemctl(action.as_str(), &plugin.service)
        .map_err(|err| PluginError::internal(format!("{} service: {err}", action.as_str())))?;

    let enabled = match action {
        PluginAction::Start | PluginAction::Restart => {
            let _ = systemctl("enable", &plugin.service);
            true
        }
        PluginAction::Stop => false,
    };
    if matches!(action, PluginAction::Stop) {
        let _ = systemctl("disable", &plugin.service);
    }

    update_plugin_enabled(&conn, plugin_id, enabled)?;
    Ok(PluginMessage {
        detail: format!("{} {}", plugin.name, action.as_str()),
    })
}

pub fn delete_plugin(
    plugins_root: &Path,
    db_path: &Path,
    systemd_dir: &Path,
    plugin_id: &str,
) -> PluginResult<PluginMessage> {
    let conn = open_db(db_path)?;
    init_db(&conn)?;
    let plugin = load_plugin(&conn, plugin_id)?
        .ok_or_else(|| PluginError::not_found(format!("unknown plugin: {plugin_id}")))?;
    if !plugin.removable {
        return Err(PluginError::bad_request(format!(
            "plugin {} is built-in and cannot be removed",
            plugin.name
        )));
    }
    let plugin_dir = plugins_root.join(plugin_id);
    let service_path = systemd_dir.join(&plugin.service);

    let _ = systemctl("stop", &plugin.service);
    let _ = systemctl("disable", &plugin.service);
    if let Some(bin_path) = plugin.bin_path.as_deref() {
        remove_managed_symlink(
            Path::new(bin_path),
            &plugin_binary_target(Path::new(&plugin.install_dir), &plugin.binary_name),
        )?;
    }
    if service_path.exists() {
        fs::remove_file(&service_path)
            .map_err(|err| PluginError::internal(format!("remove service file: {err}")))?;
    }
    daemon_reload()?;
    if plugin_dir.exists() {
        fs::remove_dir_all(&plugin_dir)
            .map_err(|err| PluginError::internal(format!("remove plugin files: {err}")))?;
    }
    conn.execute("DELETE FROM plugins WHERE id = ?1", params![plugin_id])
        .map_err(|err| PluginError::internal(format!("delete plugin record: {err}")))?;

    Ok(PluginMessage {
        detail: format!("Deleted plugin {}", plugin.name),
    })
}

pub fn upload_plugin_update(
    plugins_root: &Path,
    db_path: &Path,
    systemd_dir: &Path,
    cert_path: &Path,
    core_plugins: &[CorePluginSpec],
    plugin_id: &str,
    zip_bytes: &[u8],
) -> PluginResult<PluginMessage> {
    initialize_store(plugins_root, db_path, Some(cert_path), core_plugins)?;
    let conn = open_db(db_path)?;
    let plugin = load_plugin(&conn, plugin_id)?
        .ok_or_else(|| PluginError::not_found(format!("unknown plugin: {plugin_id}")))?;

    if let Some(target_name) = plugin.target_name.as_deref() {
        let spec = core_plugins
            .iter()
            .find(|spec| spec.target.name == target_name)
            .ok_or_else(|| {
                PluginError::internal(format!(
                    "missing built-in target mapping for plugin {plugin_id}"
                ))
            })?;
        let detail = apply_update(&spec.target, zip_bytes)
            .map_err(PluginError::internal)?;
        sync_core_plugins(&conn, core_plugins)?;
        return Ok(PluginMessage { detail });
    }

    let package = parse_plugin_package(zip_bytes)?;
    if package.id != plugin_id {
        return Err(PluginError::bad_request(format!(
            "plugin package targets {}, expected {plugin_id}",
            package.id
        )));
    }
    install_plugin(
        plugins_root,
        db_path,
        systemd_dir,
        cert_path,
        core_plugins,
        zip_bytes,
    )
}

fn parse_plugin_package(zip_bytes: &[u8]) -> PluginResult<PluginPackage> {
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|err| PluginError::bad_request(format!("bad plugin ZIP: {err}")))?;
    let manifest_raw = read_zip_entry_text(&mut archive, MANIFEST_NAME)?;
    let manifest: PluginManifest = toml::from_str(&manifest_raw)
        .map_err(|err| PluginError::bad_request(format!("bad plugin manifest: {err}")))?;
    validate_manifest(&manifest)?;

    let service_name = manifest.service.trim().to_string();
    let binary_name = derive_binary_name(&service_name)?;
    let bin_path = normalize_bin_path(manifest.bin_path.as_deref())?;
    let id = binary_name.clone();
    let sha256_name = format!("{binary_name}.sha256");
    let service_bytes = read_zip_entry_bytes(&mut archive, &service_name)?;
    let binary_bytes = read_zip_entry_bytes(&mut archive, &binary_name)?;
    let sha256 = read_zip_entry_text(&mut archive, &sha256_name)?
        .trim()
        .to_string();
    validate_sha256_text(&sha256)?;
    verify_sha256(&binary_bytes, &sha256, "plugin package binary")?;
    let signature_name = format!("{binary_name}.sign");
    let signature_bytes = read_optional_zip_entry_bytes(&mut archive, &signature_name)?;

    Ok(PluginPackage {
        manifest,
        manifest_raw,
        id,
        binary_name,
        service_name,
        bin_path,
        sha256,
        binary_bytes,
        service_bytes,
        signature_bytes,
    })
}

fn discover_plugins(
    plugins_root: &Path,
    conn: &Connection,
    cert_path: Option<&Path>,
) -> PluginResult<()> {
    let entries = fs::read_dir(plugins_root)
        .map_err(|err| PluginError::internal(format!("scan plugin root: {err}")))?;

    for entry in entries {
        let entry = entry
            .map_err(|err| PluginError::internal(format!("read plugin directory entry: {err}")))?;
        let plugin_dir = entry.path();
        if !entry
            .file_type()
            .map_err(|err| PluginError::internal(format!("inspect plugin entry type: {err}")))?
            .is_dir()
        {
            continue;
        }

        let plugin_id = entry.file_name().to_string_lossy().into_owned();
        if plugin_id.starts_with('.') {
            continue;
        }
        if load_plugin(conn, &plugin_id)?.is_some() {
            continue;
        }

        match discover_plugin_record(&plugin_dir, cert_path) {
            Ok(record) => {
                if let Err(err) = ensure_service_available(conn, &record.service, Some(&record.id))
                {
                    log::warn!(
                        "skipping plugin discovery in {}: {}",
                        plugin_dir.display(),
                        err
                    );
                    continue;
                }
                if let Err(err) =
                    ensure_bin_path_available(conn, record.bin_path.as_deref(), Some(&record.id))
                {
                    log::warn!(
                        "skipping plugin discovery in {}: {}",
                        plugin_dir.display(),
                        err
                    );
                    continue;
                }
                upsert_plugin(conn, record.clone())?;
                log::info!("discovered preinstalled plugin {}", record.id);
            }
            Err(err) => {
                log::warn!(
                    "skipping plugin discovery in {}: {}",
                    plugin_dir.display(),
                    err
                );
            }
        }
    }

    Ok(())
}

fn discover_plugin_record(
    plugin_dir: &Path,
    cert_path: Option<&Path>,
) -> PluginResult<PluginRecord> {
    let manifest_path = plugin_dir.join(MANIFEST_NAME);
    let manifest_raw = fs::read_to_string(&manifest_path)
        .map_err(|err| PluginError::internal(format!("read plugin manifest: {err}")))?;
    let manifest: PluginManifest = toml::from_str(&manifest_raw)
        .map_err(|err| PluginError::bad_request(format!("bad plugin manifest: {err}")))?;
    validate_manifest(&manifest)?;

    let service_name = manifest.service.trim().to_string();
    let binary_name = derive_binary_name(&service_name)?;
    let binary_path = plugin_dir.join("current").join(&binary_name);
    let service_path = plugin_dir.join(&service_name);
    let hash_path = plugin_dir.join(format!("{}.sha256", binary_name));
    let package_path = plugin_dir.join("package.zip");
    let signature_path = plugin_dir.join(format!("{}.sign", binary_name));

    if !binary_path.is_file() {
        return Err(PluginError::bad_request(format!(
            "missing plugin binary {}",
            binary_path.display()
        )));
    }
    if !service_path.is_file() {
        return Err(PluginError::bad_request(format!(
            "missing plugin service {}",
            service_path.display()
        )));
    }
    if !hash_path.is_file() {
        return Err(PluginError::bad_request(format!(
            "missing plugin checksum {}",
            hash_path.display()
        )));
    }

    let binary_bytes = fs::read(&binary_path)
        .map_err(|err| PluginError::internal(format!("read plugin binary: {err}")))?;
    let sha256 = read_sha256_file(&hash_path)?;
    verify_sha256(&binary_bytes, &sha256, "installed plugin binary")?;
    let signature_bytes = if signature_path.is_file() {
        Some(
            fs::read(&signature_path)
                .map_err(|err| PluginError::internal(format!("read plugin signature: {err}")))?,
        )
    } else {
        None
    };

    let official = match cert_path {
        Some(cert_path) => evaluate_official(cert_path, &binary_bytes, signature_bytes.as_deref())?,
        None => false,
    };

    let installed_at = first_available_timestamp(&[
        plugin_dir,
        &manifest_path,
        &service_path,
        &binary_path,
        &hash_path,
        &package_path,
    ])
    .unwrap_or_else(now_secs);
    let updated_at = latest_available_timestamp(&[
        plugin_dir,
        &manifest_path,
        &service_path,
        &binary_path,
        &hash_path,
        &package_path,
    ])
    .unwrap_or(installed_at);

    Ok(PluginRecord {
        id: binary_name.clone(),
        name: manifest.name,
        description: manifest.description,
        version: manifest.version,
        service: service_name.clone(),
        developer: manifest.developer,
        binary_name,
        bin_path: normalize_bin_path(manifest.bin_path.as_deref())?,
        sha256,
        install_dir: plugin_dir.to_string_lossy().into_owned(),
        package_path: package_path.to_string_lossy().into_owned(),
        official,
        enabled: read_service_enabled(&service_name),
        removable: true,
        target_name: None,
        installed_at,
        updated_at,
    })
}

fn validate_manifest(manifest: &PluginManifest) -> PluginResult<()> {
    if manifest.name.trim().is_empty() {
        return Err(PluginError::bad_request("plugin name must not be empty"));
    }
    if manifest.description.trim().is_empty() {
        return Err(PluginError::bad_request(
            "plugin description must not be empty",
        ));
    }
    if manifest.version.trim().is_empty() {
        return Err(PluginError::bad_request("plugin version must not be empty"));
    }
    if manifest.developer.trim().is_empty() {
        return Err(PluginError::bad_request(
            "plugin developer must not be empty",
        ));
    }
    normalize_bin_path(manifest.bin_path.as_deref())?;
    derive_binary_name(manifest.service.trim()).map(|_| ())
}

fn derive_binary_name(service_name: &str) -> PluginResult<String> {
    if !service_name.ends_with(".service") {
        return Err(PluginError::bad_request(
            "plugin service must end with .service",
        ));
    }
    let path = Path::new(service_name);
    if path.components().count() != 1 {
        return Err(PluginError::bad_request(
            "plugin service file must be a plain filename",
        ));
    }
    let stem = path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| PluginError::bad_request("plugin service name is invalid"))?;
    validate_file_name(stem)?;
    Ok(stem.to_string())
}

fn validate_file_name(value: &str) -> PluginResult<()> {
    if value.is_empty() {
        return Err(PluginError::bad_request(
            "plugin filename must not be empty",
        ));
    }
    if value.contains('/') || value.contains('\\') {
        return Err(PluginError::bad_request(
            "plugin filename must not contain path separators",
        ));
    }
    if value.starts_with('.') {
        return Err(PluginError::bad_request(
            "plugin filename must not start with a dot",
        ));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(PluginError::bad_request(
            "plugin filename must use only ASCII letters, numbers, ., _ or -",
        ));
    }
    Ok(())
}

fn read_zip_entry_text<R: io::Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> PluginResult<String> {
    let bytes = read_zip_entry_bytes(archive, name)?;
    String::from_utf8(bytes)
        .map_err(|err| PluginError::bad_request(format!("plugin file {name} is not utf-8: {err}")))
}

fn read_zip_entry_bytes<R: io::Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> PluginResult<Vec<u8>> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| PluginError::bad_request(format!("missing required plugin file {name}")))?;
    validate_zip_name(file.name())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|err| PluginError::bad_request(format!("read plugin file {name}: {err}")))?;
    Ok(buf)
}

fn read_optional_zip_entry_bytes<R: io::Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> PluginResult<Option<Vec<u8>>> {
    match archive.by_name(name) {
        Ok(mut file) => {
            validate_zip_name(file.name())?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map_err(|err| {
                PluginError::bad_request(format!("read optional plugin file {name}: {err}"))
            })?;
            Ok(Some(buf))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(err) => Err(PluginError::bad_request(format!(
            "read optional plugin file {name}: {err}"
        ))),
    }
}

fn validate_zip_name(name: &str) -> PluginResult<()> {
    let path = Path::new(name);
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(PluginError::bad_request(format!(
                "plugin package contains invalid path {name}"
            )));
        }
    }
    Ok(())
}

fn normalize_bin_path(bin_path: Option<&str>) -> PluginResult<Option<String>> {
    let Some(bin_path) = bin_path.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = Path::new(bin_path);
    if !path.is_absolute() {
        return Err(PluginError::bad_request(
            "plugin bin_path must be an absolute path",
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PluginError::bad_request(
            "plugin bin_path must not contain parent-directory segments",
        ));
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

fn plugin_binary_target(install_dir: &Path, binary_name: &str) -> PathBuf {
    install_dir.join("current").join(binary_name)
}

fn rollback_symlink_info_path(rollback_dir: &Path) -> PathBuf {
    rollback_dir.join("bin_path.rollback")
}

fn prepare_bin_path_rollback(
    rollback_dir: &Path,
    bin_path: Option<&str>,
    target: Option<PathBuf>,
) -> PluginResult<()> {
    let info_path = rollback_symlink_info_path(rollback_dir);
    let _ = fs::remove_file(&info_path);
    let Some(bin_path) = bin_path else {
        return Ok(());
    };
    let Some(target) = target else {
        return Ok(());
    };
    let link_path = Path::new(bin_path);
    if !link_path.exists() && fs::symlink_metadata(link_path).is_err() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(link_path)
        .map_err(|err| PluginError::internal(format!("inspect plugin symlink: {err}")))?;
    if !metadata.file_type().is_symlink() {
        return Err(PluginError::bad_request(format!(
            "plugin bin_path {} conflicts with an existing non-symlink entry",
            link_path.display()
        )));
    }
    let current_target = fs::read_link(link_path)
        .map_err(|err| PluginError::internal(format!("read plugin symlink: {err}")))?;
    if current_target != target {
        return Err(PluginError::bad_request(format!(
            "plugin bin_path {} is already used by another target",
            link_path.display()
        )));
    }
    fs::write(
        &info_path,
        format!("{}\n{}\n", link_path.display(), current_target.display()),
    )
    .map_err(|err| PluginError::internal(format!("persist symlink rollback metadata: {err}")))?;
    Ok(())
}

fn install_bin_path(
    new_bin_path: Option<&str>,
    previous_bin_path: Option<&str>,
    target: &Path,
    rollback_dir: &Path,
) -> PluginResult<()> {
    if let Some(previous) = previous_bin_path {
        remove_managed_symlink(Path::new(previous), target)?;
    }

    let Some(new_bin_path) = new_bin_path else {
        return Ok(());
    };
    let link_path = Path::new(new_bin_path);
    if let Some(parent) = link_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| PluginError::internal(format!("create bin_path parent: {err}")))?;
    }
    match fs::symlink_metadata(link_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                let current_target = fs::read_link(link_path)
                    .map_err(|err| PluginError::internal(format!("read plugin symlink: {err}")))?;
                if current_target != target {
                    return Err(PluginError::bad_request(format!(
                        "plugin bin_path {} conflicts with an existing symlink",
                        link_path.display()
                    )));
                }
                fs::remove_file(link_path).map_err(|err| {
                    PluginError::internal(format!("replace plugin symlink: {err}"))
                })?;
            } else {
                return Err(PluginError::bad_request(format!(
                    "plugin bin_path {} conflicts with an existing file",
                    link_path.display()
                )));
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(PluginError::internal(format!(
                "inspect plugin bin_path {}: {err}",
                link_path.display()
            )))
        }
    }

    std::os::unix::fs::symlink(target, link_path)
        .map_err(|err| PluginError::internal(format!("create plugin symlink: {err}")))?;
    let _ = rollback_dir;
    Ok(())
}

fn remove_managed_symlink(link_path: &Path, expected_target: &Path) -> PluginResult<()> {
    match fs::symlink_metadata(link_path) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return Err(PluginError::bad_request(format!(
                    "plugin bin_path {} conflicts with an existing non-symlink entry",
                    link_path.display()
                )));
            }
            let current_target = fs::read_link(link_path)
                .map_err(|err| PluginError::internal(format!("read plugin symlink: {err}")))?;
            if current_target != expected_target {
                return Err(PluginError::bad_request(format!(
                    "plugin bin_path {} is owned by another target",
                    link_path.display()
                )));
            }
            fs::remove_file(link_path)
                .map_err(|err| PluginError::internal(format!("remove plugin symlink: {err}")))?;
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(PluginError::internal(format!(
            "inspect plugin symlink {}: {err}",
            link_path.display()
        ))),
    }
}

fn restore_bin_path(rollback_dir: &Path) -> PluginResult<()> {
    let info_path = rollback_symlink_info_path(rollback_dir);
    if !info_path.exists() {
        return Ok(());
    }
    let data = fs::read_to_string(&info_path)
        .map_err(|err| PluginError::internal(format!("read symlink rollback metadata: {err}")))?;
    let mut lines = data.lines();
    let Some(link_path) = lines.next() else {
        return Ok(());
    };
    let Some(target_path) = lines.next() else {
        return Ok(());
    };
    let link_path = PathBuf::from(link_path);
    if let Some(parent) = link_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| PluginError::internal(format!("create symlink parent: {err}")))?;
    }
    if fs::symlink_metadata(&link_path).is_ok() {
        let _ = fs::remove_file(&link_path);
    }
    std::os::unix::fs::symlink(PathBuf::from(target_path), &link_path)
        .map_err(|err| PluginError::internal(format!("restore plugin symlink: {err}")))?;
    cleanup_bin_path_rollback(rollback_dir);
    Ok(())
}

fn cleanup_bin_path_rollback(rollback_dir: &Path) {
    let _ = fs::remove_file(rollback_symlink_info_path(rollback_dir));
}

fn evaluate_official(
    cert_path: &Path,
    binary_bytes: &[u8],
    signature_bytes: Option<&[u8]>,
) -> PluginResult<bool> {
    let Some(signature) = signature_bytes else {
        return Ok(false);
    };
    if !cert_path.exists() {
        return Ok(false);
    }

    let tmp = tempfile::tempdir()
        .map_err(|err| PluginError::internal(format!("create signature tempdir: {err}")))?;
    let bin_path = tmp.path().join("plugin.bin");
    let sig_path = tmp.path().join("plugin.sign");
    fs::write(&bin_path, binary_bytes)
        .map_err(|err| PluginError::internal(format!("write temp plugin binary: {err}")))?;
    fs::write(&sig_path, signature)
        .map_err(|err| PluginError::internal(format!("write temp plugin signature: {err}")))?;
    verify_signature(&bin_path, &sig_path, cert_path)?;
    Ok(true)
}

fn prepare_rollback(
    plugin_dir: &Path,
    rollback_dir: &Path,
    service_path: &Path,
    service_name: &str,
) -> PluginResult<()> {
    cleanup_rollback(rollback_dir);
    fs::create_dir_all(rollback_dir)
        .map_err(|err| PluginError::internal(format!("create rollback dir: {err}")))?;

    move_if_exists(&plugin_dir.join("current"), &rollback_dir.join("current"))?;
    move_if_exists(
        &plugin_dir.join("package.zip"),
        &rollback_dir.join("package.zip"),
    )?;
    move_if_exists(
        &plugin_dir.join(MANIFEST_NAME),
        &rollback_dir.join(MANIFEST_NAME),
    )?;
    move_if_exists(
        &plugin_dir.join(service_name),
        &rollback_dir.join(service_name),
    )?;
    move_if_exists(
        &plugin_dir.join(format!(
            "{}.sha256",
            Path::new(service_name)
                .file_stem()
                .and_then(OsStr::to_str)
                .unwrap_or(service_name)
        )),
        &rollback_dir.join(format!(
            "{}.sha256",
            Path::new(service_name)
                .file_stem()
                .and_then(OsStr::to_str)
                .unwrap_or(service_name)
        )),
    )?;

    let signature_path = plugin_dir.join(format!(
        "{}.sign",
        Path::new(service_name)
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or(service_name)
    ));
    move_if_exists(
        &signature_path,
        &rollback_dir.join(signature_path.file_name().unwrap()),
    )?;

    if service_path.exists() {
        fs::copy(service_path, rollback_dir.join(service_name))
            .map_err(|err| PluginError::internal(format!("backup service file: {err}")))?;
    }

    Ok(())
}

fn rollback_install(
    plugin_dir: &Path,
    rollback_dir: &Path,
    service_path: &Path,
    service_name: &str,
    attempted_bin_path: Option<&str>,
    attempted_target: &Path,
    existing: Option<&PluginRecord>,
) -> PluginResult<()> {
    if let Some(bin_path) = attempted_bin_path {
        let _ = remove_managed_symlink(Path::new(bin_path), attempted_target);
    }
    let current_dir = plugin_dir.join("current");
    if current_dir.exists() {
        let _ = fs::remove_dir_all(&current_dir);
    }
    let _ = fs::remove_file(plugin_dir.join("package.zip"));
    let _ = fs::remove_file(plugin_dir.join(MANIFEST_NAME));
    let _ = fs::remove_file(plugin_dir.join(service_name));
    let _ = fs::remove_file(plugin_dir.join(format!(
        "{}.sha256",
        Path::new(service_name)
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or(service_name)
    )));
    let _ = fs::remove_file(plugin_dir.join(format!(
        "{}.sign",
        Path::new(service_name)
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or(service_name)
    )));

    restore_if_exists(&rollback_dir.join("current"), &current_dir)?;
    restore_if_exists(
        &rollback_dir.join("package.zip"),
        &plugin_dir.join("package.zip"),
    )?;
    restore_if_exists(
        &rollback_dir.join(MANIFEST_NAME),
        &plugin_dir.join(MANIFEST_NAME),
    )?;
    restore_if_exists(
        &rollback_dir.join(service_name),
        &plugin_dir.join(service_name),
    )?;
    restore_if_exists(
        &rollback_dir.join(format!(
            "{}.sha256",
            Path::new(service_name)
                .file_stem()
                .and_then(OsStr::to_str)
                .unwrap_or(service_name)
        )),
        &plugin_dir.join(format!(
            "{}.sha256",
            Path::new(service_name)
                .file_stem()
                .and_then(OsStr::to_str)
                .unwrap_or(service_name)
        )),
    )?;

    let restored_service = rollback_dir.join(service_name);
    if restored_service.exists() {
        fs::copy(&restored_service, service_path)
            .map_err(|err| PluginError::internal(format!("restore service file: {err}")))?;
    } else if service_path.exists() {
        fs::remove_file(service_path)
            .map_err(|err| PluginError::internal(format!("remove failed service file: {err}")))?;
    }
    daemon_reload()?;
    restore_bin_path(rollback_dir)?;

    if let Some(existing) = existing {
        if existing.enabled {
            let _ = systemctl("enable", &existing.service);
        } else {
            let _ = systemctl("disable", &existing.service);
        }
        if is_service_running(&existing.service) || existing.enabled {
            let _ = systemctl("start", &existing.service);
        }
    } else {
        let _ = systemctl("disable", service_name);
        let _ = fs::remove_file(service_path);
        daemon_reload()?;
    }

    cleanup_rollback(rollback_dir);
    Ok(())
}

fn cleanup_rollback(path: &Path) {
    if path.exists() {
        let _ = fs::remove_dir_all(path);
    }
}

fn move_if_exists(src: &Path, dst: &Path) -> PluginResult<()> {
    if !src.exists() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| PluginError::internal(format!("create rollback parent: {err}")))?;
    }
    fs::rename(src, dst)
        .map_err(|err| PluginError::internal(format!("move plugin state into rollback: {err}")))
}

fn restore_if_exists(src: &Path, dst: &Path) -> PluginResult<()> {
    if !src.exists() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| PluginError::internal(format!("create restore parent: {err}")))?;
    }
    fs::rename(src, dst)
        .map_err(|err| PluginError::internal(format!("restore plugin state: {err}")))
}

fn open_db(db_path: &Path) -> PluginResult<Connection> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| PluginError::internal(format!("create plugin db dir: {err}")))?;
    }
    let conn = Connection::open(db_path)
        .map_err(|err| PluginError::internal(format!("open plugin db: {err}")))?;
    init_db(&conn)?;
    Ok(conn)
}

fn init_db(conn: &Connection) -> PluginResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS plugins (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT NOT NULL,
            version TEXT NOT NULL,
            service TEXT NOT NULL UNIQUE,
            developer TEXT NOT NULL,
            binary_name TEXT NOT NULL,
            bin_path TEXT,
            sha256 TEXT NOT NULL DEFAULT '',
            install_dir TEXT NOT NULL,
            package_path TEXT NOT NULL,
            official INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            removable INTEGER NOT NULL DEFAULT 1,
            target_name TEXT,
            installed_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );",
    )
    .map_err(|err| PluginError::internal(format!("init plugin db: {err}")))?;
    ensure_column_exists(
        conn,
        "plugins",
        "bin_path",
        "ALTER TABLE plugins ADD COLUMN bin_path TEXT",
    )?;
    ensure_column_exists(
        conn,
        "plugins",
        "removable",
        "ALTER TABLE plugins ADD COLUMN removable INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column_exists(
        conn,
        "plugins",
        "target_name",
        "ALTER TABLE plugins ADD COLUMN target_name TEXT",
    )?;
    ensure_column_exists(
        conn,
        "plugins",
        "sha256",
        "ALTER TABLE plugins ADD COLUMN sha256 TEXT NOT NULL DEFAULT ''",
    )?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_plugins_service ON plugins(service);
         CREATE UNIQUE INDEX IF NOT EXISTS idx_plugins_bin_path
         ON plugins(bin_path)
         WHERE bin_path IS NOT NULL AND bin_path != '';
         CREATE UNIQUE INDEX IF NOT EXISTS idx_plugins_target_name
         ON plugins(target_name)
         WHERE target_name IS NOT NULL AND target_name != '';",
    )
    .map_err(|err| PluginError::internal(format!("init plugin indexes: {err}")))?;
    Ok(())
}

fn load_plugin(conn: &Connection, plugin_id: &str) -> PluginResult<Option<PluginRecord>> {
    conn.query_row(
        "SELECT id, name, description, version, service, developer, binary_name, bin_path, sha256, install_dir, package_path, official, enabled, removable, target_name, installed_at, updated_at
         FROM plugins
         WHERE id = ?1",
        params![plugin_id],
        |row| {
            Ok(PluginRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                version: row.get(3)?,
                service: row.get(4)?,
                developer: row.get(5)?,
                binary_name: row.get(6)?,
                bin_path: row.get(7)?,
                sha256: row.get(8)?,
                install_dir: row.get(9)?,
                package_path: row.get(10)?,
                official: row.get::<_, i64>(11)? != 0,
                enabled: row.get::<_, i64>(12)? != 0,
                removable: row.get::<_, i64>(13)? != 0,
                target_name: row.get(14)?,
                installed_at: row.get(15)?,
                updated_at: row.get(16)?,
            })
        },
    )
    .optional()
    .map_err(|err| PluginError::internal(format!("load plugin {plugin_id}: {err}")))
}

fn ensure_service_available(
    conn: &Connection,
    service_name: &str,
    current_plugin_id: Option<&str>,
) -> PluginResult<()> {
    let owner = conn
        .query_row(
            "SELECT id FROM plugins WHERE service = ?1",
            params![service_name],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|err| PluginError::internal(format!("query plugin service owner: {err}")))?;

    if let Some(owner) = owner {
        if current_plugin_id != Some(owner.as_str()) {
            return Err(PluginError::bad_request(format!(
                "service {service_name} is already owned by plugin {owner}"
            )));
        }
    }
    Ok(())
}

fn ensure_bin_path_available(
    conn: &Connection,
    bin_path: Option<&str>,
    current_plugin_id: Option<&str>,
) -> PluginResult<()> {
    let Some(bin_path) = bin_path else {
        return Ok(());
    };
    let owner = conn
        .query_row(
            "SELECT id FROM plugins WHERE bin_path = ?1",
            params![bin_path],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|err| PluginError::internal(format!("query plugin bin_path owner: {err}")))?;

    if let Some(owner) = owner {
        if current_plugin_id != Some(owner.as_str()) {
            return Err(PluginError::bad_request(format!(
                "bin_path {bin_path} is already owned by plugin {owner}"
            )));
        }
    }
    Ok(())
}

fn sync_core_plugins(conn: &Connection, core_plugins: &[CorePluginSpec]) -> PluginResult<()> {
    for spec in core_plugins {
        let binary_name = spec
            .target
            .bin_path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(spec.target.name)
            .to_string();
        let version_path = spec
            .target
            .meta_dir
            .join(format!("{binary_name}.version"));
        let hash_path = spec.target.meta_dir.join(format!("{binary_name}.sha256"));
        let version = read_trimmed(&version_path).unwrap_or_else(|| "unknown".into());
        let sha256 = read_trimmed(&hash_path).unwrap_or_else(|| {
            sha256_file(&spec.target.bin_path).unwrap_or_default()
        });
        let installed_at = first_available_timestamp(&[
            spec.target.bin_path.as_path(),
            version_path.as_path(),
            hash_path.as_path(),
        ])
        .unwrap_or_else(now_secs);
        let updated_at = latest_available_timestamp(&[
            spec.target.bin_path.as_path(),
            version_path.as_path(),
            hash_path.as_path(),
        ])
        .unwrap_or(installed_at);

        upsert_plugin(
            conn,
            PluginRecord {
                id: spec.plugin_id.clone(),
                name: spec.name.clone(),
                description: spec.description.clone(),
                version,
                service: spec.target.service.to_string(),
                developer: spec.developer.clone(),
                binary_name,
                bin_path: None,
                sha256,
                install_dir: spec
                    .target
                    .bin_path
                    .parent()
                    .unwrap_or_else(|| Path::new("/"))
                    .to_string_lossy()
                    .into_owned(),
                package_path: String::new(),
                official: true,
                enabled: read_service_enabled(spec.target.service),
                removable: false,
                target_name: Some(spec.target.name.to_string()),
                installed_at,
                updated_at,
            },
        )?;
    }
    Ok(())
}

fn upsert_plugin(conn: &Connection, record: PluginRecord) -> PluginResult<()> {
    conn.execute(
        "INSERT INTO plugins (
            id, name, description, version, service, developer, binary_name, bin_path, sha256, install_dir, package_path, official, enabled, removable, target_name, installed_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            version = excluded.version,
            service = excluded.service,
            developer = excluded.developer,
            binary_name = excluded.binary_name,
            bin_path = excluded.bin_path,
            sha256 = excluded.sha256,
            install_dir = excluded.install_dir,
            package_path = excluded.package_path,
            official = excluded.official,
            enabled = excluded.enabled,
            removable = excluded.removable,
            target_name = excluded.target_name,
            installed_at = excluded.installed_at,
            updated_at = excluded.updated_at",
        params![
            record.id,
            record.name,
            record.description,
            record.version,
            record.service,
            record.developer,
            record.binary_name,
            record.bin_path,
            record.sha256,
            record.install_dir,
            record.package_path,
            if record.official { 1 } else { 0 },
            if record.enabled { 1 } else { 0 },
            if record.removable { 1 } else { 0 },
            record.target_name,
            record.installed_at,
            record.updated_at,
        ],
    )
    .map_err(|err| PluginError::internal(format!("upsert plugin record: {err}")))?;
    Ok(())
}

fn ensure_column_exists(
    conn: &Connection,
    table: &str,
    column: &str,
    alter_sql: &str,
) -> PluginResult<()> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&pragma)
        .map_err(|err| PluginError::internal(format!("prepare table info: {err}")))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| PluginError::internal(format!("query table info: {err}")))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    if !exists {
        conn.execute(alter_sql, [])
            .map_err(|err| PluginError::internal(format!("migrate plugin db: {err}")))?;
    }
    Ok(())
}

fn update_plugin_enabled(conn: &Connection, plugin_id: &str, enabled: bool) -> PluginResult<()> {
    conn.execute(
        "UPDATE plugins SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
        params![plugin_id, if enabled { 1 } else { 0 }, now_secs()],
    )
    .map_err(|err| PluginError::internal(format!("update plugin state: {err}")))?;
    Ok(())
}

fn daemon_reload() -> PluginResult<()> {
    Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .map_err(|err| PluginError::internal(format!("systemctl daemon-reload: {err}")))?;
    Ok(())
}

fn systemctl(action: &str, service: &str) -> io::Result<()> {
    Command::new("systemctl").args([action, service]).status()?;
    Ok(())
}

fn is_service_running(service: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", service])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn poll_running(service: &str, tries: u32, interval: Duration) -> bool {
    for _ in 0..tries {
        std::thread::sleep(interval);
        if is_service_running(service) {
            return true;
        }
    }
    false
}

fn read_service_status(service: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        match Command::new("systemctl")
            .args(["show", "--property=ActiveState", "--value", service])
            .output()
        {
            Ok(output) if output.status.success() => {
                let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if value.is_empty() {
                    "unknown".into()
                } else {
                    value
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                if stderr.is_empty() {
                    "error".into()
                } else {
                    stderr
                }
            }
            Err(err) => format!("systemctl unavailable: {err}"),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        format!("Unavailable on this host ({service})")
    }
}

fn read_service_enabled(service: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        Command::new("systemctl")
            .args(["is-enabled", "--quiet", service])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = service;
        false
    }
}

fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms)
}

fn read_sha256_file(path: &Path) -> PluginResult<String> {
    let sha256 = fs::read_to_string(path)
        .map_err(|err| PluginError::internal(format!("read plugin checksum: {err}")))?;
    let sha256 = sha256.trim().to_string();
    validate_sha256_text(&sha256)?;
    Ok(sha256)
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|value| value.trim().to_string())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let data = fs::read(path)?;
    Ok(hex::encode(Sha256::digest(data)))
}

fn validate_sha256_text(value: &str) -> PluginResult<()> {
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(PluginError::bad_request(
            "plugin sha256 file must contain a 64-character hex digest",
        ));
    }
    Ok(())
}

fn verify_sha256(bytes: &[u8], expected: &str, context: &str) -> PluginResult<()> {
    let normalized = expected.to_ascii_lowercase();
    let actual = hex::encode(Sha256::digest(bytes));
    if actual != normalized {
        return Err(PluginError::bad_request(format!(
            "{context} SHA-256 mismatch: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}

fn verify_signature(bin: &Path, sig: &Path, cert: &Path) -> PluginResult<()> {
    let status = Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(cert)
        .arg("-signature")
        .arg(sig)
        .arg(bin)
        .status()
        .map_err(|err| PluginError::internal(format!("openssl exec error: {err}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(PluginError::bad_request(
            "plugin signature did not match the official public key",
        ))
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

fn latest_available_timestamp(paths: &[&Path]) -> Option<u64> {
    paths.iter().filter_map(|path| file_timestamp(path)).max()
}

fn first_available_timestamp(paths: &[&Path]) -> Option<u64> {
    paths.iter().filter_map(|path| file_timestamp(path)).min()
}

fn file_timestamp(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::{write::SimpleFileOptions, ZipWriter};

    #[test]
    fn derives_binary_name_from_service() {
        assert_eq!(
            derive_binary_name("kaonic-plugin-sample.service").unwrap(),
            "kaonic-plugin-sample"
        );
    }

    #[test]
    fn rejects_nested_service_path() {
        let err = derive_binary_name("plugins/sample.service").unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn rejects_relative_bin_path() {
        let err = normalize_bin_path(Some("usr/bin/sample")).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parses_plugin_package() {
        let zip_bytes = build_test_plugin_zip();
        let package = parse_plugin_package(&zip_bytes).unwrap();
        assert_eq!(package.id, "kaonic-plugin-sample");
        assert_eq!(package.manifest.name, "Sample");
        assert_eq!(package.sha256.len(), 64);
        assert!(package.signature_bytes.is_none());
    }

    #[test]
    fn rejects_plugin_package_with_bad_sha256() {
        let zip_bytes = build_test_plugin_zip_with_hash("deadbeef");
        let err = parse_plugin_package(&zip_bytes).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn discovers_untracked_plugin_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins_root = tmp.path().join("plugins");
        let plugin_dir = plugins_root.join("kaonic-plugin-sample");
        let current_dir = plugin_dir.join("current");
        fs::create_dir_all(&current_dir).unwrap();
        fs::write(
            plugin_dir.join(MANIFEST_NAME),
            r#"name = "Sample"
description = "Sample plugin"
version = "0.1.0"
service = "kaonic-plugin-sample.service"
developer = "Beechat"
"#,
        )
        .unwrap();
        fs::write(
            plugin_dir.join("kaonic-plugin-sample.service"),
            "[Service]\nExecStart=/etc/kaonic/plugins/kaonic-plugin-sample/current/kaonic-plugin-sample\n",
        )
        .unwrap();
        fs::write(
            current_dir.join("kaonic-plugin-sample"),
            b"#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        let binary_hash = hex::encode(Sha256::digest(b"#!/bin/sh\nexit 0\n"));
        fs::write(
            plugin_dir.join("kaonic-plugin-sample.sha256"),
            format!("{binary_hash}\n"),
        )
        .unwrap();

        let db_path = plugins_root.join("kaonic-plugins.db");
        initialize_store(&plugins_root, &db_path, None, &[]).unwrap();

        let plugins = list_plugins(&plugins_root, &db_path, None, &[]).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].id, "kaonic-plugin-sample");
        assert_eq!(plugins[0].service, "kaonic-plugin-sample.service");
        assert_eq!(plugins[0].sha256, binary_hash);
    }

    #[test]
    fn manages_plugin_symlink_safely() {
        let tmp = tempfile::tempdir().unwrap();
        let rollback_dir = tmp.path().join("rollback");
        let install_dir = tmp.path().join("plugin");
        let current_dir = install_dir.join("current");
        let link_path = tmp.path().join("bin").join("sample");
        fs::create_dir_all(&rollback_dir).unwrap();
        fs::create_dir_all(&current_dir).unwrap();
        let binary_path = current_dir.join("sample");
        fs::write(&binary_path, b"sample").unwrap();

        install_bin_path(
            Some(link_path.to_str().unwrap()),
            None,
            &binary_path,
            &rollback_dir,
        )
        .unwrap();
        assert_eq!(fs::read_link(&link_path).unwrap(), binary_path);

        remove_managed_symlink(&link_path, &binary_path).unwrap();
        assert!(fs::symlink_metadata(&link_path).is_err());
    }

    fn build_test_plugin_zip() -> Vec<u8> {
        let binary = b"#!/bin/sh\nexit 0\n";
        let sha256 = hex::encode(Sha256::digest(binary));
        build_test_plugin_zip_with_hash(&sha256)
    }

    fn build_test_plugin_zip_with_hash(sha256: &str) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let options = SimpleFileOptions::default();
            writer.start_file(MANIFEST_NAME, options).unwrap();
            writer
                .write_all(
                    br#"name = "Sample"
description = "Sample plugin"
version = "0.1.0"
service = "kaonic-plugin-sample.service"
developer = "Beechat"
"#,
                )
                .unwrap();
            writer
                .start_file("kaonic-plugin-sample.service", options)
                .unwrap();
            writer
                .write_all(b"[Service]\nExecStart=/bin/true\n")
                .unwrap();
            writer
                .start_file("kaonic-plugin-sample.sha256", options)
                .unwrap();
            writer.write_all(sha256.as_bytes()).unwrap();
            writer.start_file("kaonic-plugin-sample", options).unwrap();
            writer.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }
}
