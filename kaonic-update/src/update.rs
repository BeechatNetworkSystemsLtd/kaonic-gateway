use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{fs, io};

use sha2::{Digest, Sha256};

/// Describes one updatable target on the system.
pub struct Target {
    /// Short name used in API paths ("commd" or "gateway").
    pub name: &'static str,
    /// Installed binary path.
    pub bin_path: PathBuf,
    /// systemd service unit name.
    pub service: &'static str,
    /// Directory that holds version/hash/backup metadata.
    pub meta_dir: PathBuf,
}

impl Target {
    fn version_path(&self) -> PathBuf {
        self.meta_dir.join(format!("{}.version", self.bin_name()))
    }
    fn hash_path(&self) -> PathBuf {
        self.meta_dir.join(format!("{}.sha256", self.bin_name()))
    }
    fn backup_path(&self) -> PathBuf {
        self.meta_dir.join(format!("{}.bak", self.bin_name()))
    }
    fn cert_path(&self) -> PathBuf {
        self.meta_dir.join("beechat-ota.pub.pem")
    }

    fn bin_name(&self) -> &str {
        self.bin_path.file_name().unwrap().to_str().unwrap()
    }
}

/// Current version info for a target.
#[derive(serde::Serialize)]
pub struct VersionInfo {
    pub version: Option<String>,
    pub hash: Option<String>,
}

pub fn get_version(target: &Target) -> VersionInfo {
    VersionInfo {
        version: read_trimmed(&target.version_path()),
        hash: read_trimmed(&target.hash_path()),
    }
}

/// Validate the installed binary against the stored hash, restoring backup if corrupt.
/// Called once at startup.
pub fn validate_on_boot(target: &Target) {
    if !target.meta_dir.exists() {
        let _ = fs::create_dir_all(&target.meta_dir);
        return;
    }
    if !target.bin_path.exists() && !target.backup_path().exists() {
        return;
    }
    let expected = match read_trimmed(&target.hash_path()) {
        Some(h) => h,
        None => return,
    };
    let actual = match sha256_file(&target.bin_path) {
        Ok(h) => h,
        Err(_) => String::new(),
    };
    if expected != actual {
        log::warn!("[{}] hash mismatch on boot — restoring backup", target.name);
        let _ = systemctl("stop", target.service);
        restore_backup(target);
        let _ = systemctl("start", target.service);
    }
}

/// Full OTA update flow. Returns a human-readable result message or an error string.
pub fn apply_update(target: &Target, zip_bytes: &[u8]) -> Result<String, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let tmp_path = tmp.path();

    // Extract ZIP
    let cursor = io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("bad ZIP: {e}"))?;
    archive
        .extract(tmp_path)
        .map_err(|e| format!("ZIP extract: {e}"))?;

    let bin_name = target.bin_name().to_string();
    let new_bin = tmp_path.join(&bin_name);
    let hash_file = tmp_path.join(format!("{bin_name}.sha256"));
    let ver_file = tmp_path.join(format!("{bin_name}.version"));
    let sig_file = tmp_path.join(format!("{bin_name}.sig"));

    // Required files
    for f in [&new_bin, &hash_file, &ver_file, &sig_file] {
        if !f.exists() {
            return Err(format!(
                "missing {} in OTA package",
                f.file_name().unwrap().to_string_lossy()
            ));
        }
    }

    // SHA-256 check
    let expected_hash = fs::read_to_string(&hash_file)
        .map_err(|e| format!("read hash file: {e}"))?
        .trim()
        .to_string();
    let actual_hash = sha256_file(&new_bin).map_err(|e| format!("hash new binary: {e}"))?;
    if expected_hash != actual_hash {
        return Err(format!(
            "SHA-256 mismatch: expected {expected_hash}, got {actual_hash}"
        ));
    }

    // Signature verification (skipped if cert absent — dev/test mode)
    let cert = target.cert_path();
    if cert.exists() {
        verify_signature(&new_bin, &sig_file, &cert)?;
    } else {
        log::warn!(
            "[{}] OTA cert not found at {:?} — skipping signature check",
            target.name,
            cert
        );
    }

    // Stop service, backup, replace
    let _ = fs::create_dir_all(&target.meta_dir);
    let _ = systemctl("stop", target.service);
    backup_current(target);

    fs::copy(&new_bin, &target.bin_path).map_err(|e| format!("copy binary: {e}"))?;
    make_executable(&target.bin_path).map_err(|e| format!("chmod: {e}"))?;

    let _ = systemctl("start", target.service);

    // Poll for up to 10 s (skip for gateway — it restarts itself, breaking the poll)
    let is_gateway = target.name == "gateway";
    let running = if is_gateway {
        true // assume success; gateway service restart will proceed independently
    } else {
        poll_running(target.service, 10, Duration::from_secs(1))
    };

    if running {
        fs::write(&target.hash_path(), &expected_hash).ok();
        if let Ok(ver) = fs::read_to_string(&ver_file) {
            fs::write(&target.version_path(), ver.trim()).ok();
        }
        let msg = if is_gateway {
            "Update applied — gateway service is restarting".to_string()
        } else {
            "Update successful".to_string()
        };
        log::info!("[{}] {}", target.name, msg);
        Ok(msg)
    } else {
        log::error!(
            "[{}] new binary failed to start — rolling back",
            target.name
        );
        let _ = systemctl("stop", target.service);
        restore_backup(target);
        let _ = systemctl("start", target.service);
        Err("new binary failed health check — rollback complete".to_string())
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn sha256_file(path: &Path) -> io::Result<String> {
    let data = fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&data);
    Ok(hex::encode(h.finalize()))
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn systemctl(action: &str, service: &str) -> io::Result<()> {
    Command::new("systemctl").args([action, service]).status()?;
    Ok(())
}

fn is_service_running(service: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", service])
        .status()
        .map(|s| s.success())
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

fn backup_current(target: &Target) {
    if target.bin_path.exists() {
        let _ = fs::copy(&target.bin_path, target.backup_path());
    }
}

fn restore_backup(target: &Target) {
    let bak = target.backup_path();
    if bak.exists() {
        let _ = fs::copy(&bak, &target.bin_path);
        let _ = make_executable(&target.bin_path);
    }
}

fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    let mode = perms.mode();
    perms.set_mode(mode | 0o111);
    fs::set_permissions(path, perms)
}

fn verify_signature(bin: &Path, sig: &Path, cert: &Path) -> Result<(), String> {
    // Use openssl CLI: openssl dgst -sha256 -verify <cert> -signature <sig> <bin>
    let status = Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(cert)
        .arg("-signature")
        .arg(sig)
        .arg(bin)
        .status()
        .map_err(|e| format!("openssl exec error: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("signature verification failed".to_string())
    }
}
