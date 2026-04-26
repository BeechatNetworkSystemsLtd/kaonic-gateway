use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use std::{fs, io};

use sha2::{Digest, Sha256};

/// Describes one updatable target on the system.
#[derive(Debug)]
pub struct Target {
    /// Stable built-in target name ("commd" or "gateway").
    pub name: &'static str,
    /// Public executable path exposed in plugin metadata (usually a symlink).
    pub symlink_path: PathBuf,
    /// Actual managed binary path updated by OTA.
    pub binary_path: PathBuf,
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
        self.binary_path.file_name().unwrap().to_str().unwrap()
    }
}

/// Validate the installed binary against the stored hash, restoring backup if corrupt.
/// Called once at startup.
pub fn validate_on_boot(target: &Target) {
    if !target.meta_dir.exists() {
        log::debug!(
            "[{}] creating missing metadata directory {}",
            target.name,
            target.meta_dir.display()
        );
        let _ = fs::create_dir_all(&target.meta_dir);
        return;
    }
    if !target.binary_path.exists() && !target.backup_path().exists() {
        log::debug!(
            "[{}] no installed binary or backup found, skipping boot validation",
            target.name
        );
        return;
    }
    let expected = match read_trimmed(&target.hash_path()) {
        Some(h) => h,
        None => {
            log::debug!(
                "[{}] no stored hash found, skipping boot validation",
                target.name
            );
            return;
        }
    };
    let actual = match sha256_file(&target.binary_path) {
        Ok(h) => h,
        Err(_) => String::new(),
    };
    log::debug!(
        "[{}] boot validation expected_hash_present=true actual_hash_present={}",
        target.name,
        !actual.is_empty()
    );
    if expected != actual {
        log::warn!("[{}] hash mismatch on boot — restoring backup", target.name);
        let _ = systemctl("stop", target.service);
        restore_backup(target);
        let _ = systemctl("start", target.service);
    } else {
        log::debug!("[{}] boot validation passed", target.name);
    }
}

/// Full OTA update flow. Returns a human-readable result message or an error string.
pub fn apply_update(target: &Target, zip_bytes: &[u8]) -> Result<String, String> {
    log::info!(
        "[{}] starting OTA apply payload_bytes={} target_bin={} service={}",
        target.name,
        zip_bytes.len(),
        target.binary_path.display(),
        target.service
    );
    let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let tmp_path = tmp.path();

    // Extract ZIP
    let cursor = io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("bad ZIP: {e}"))?;
    log::debug!("[{}] OTA archive entries={}", target.name, archive.len());
    archive
        .extract(tmp_path)
        .map_err(|e| format!("ZIP extract: {e}"))?;
    log::debug!(
        "[{}] extracted OTA package into {}",
        target.name,
        tmp_path.display()
    );

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
    log::debug!(
        "[{}] OTA package checksum computed for {}",
        target.name,
        new_bin.display()
    );
    if expected_hash != actual_hash {
        return Err(format!(
            "SHA-256 mismatch: expected {expected_hash}, got {actual_hash}"
        ));
    }

    // Signature verification (skipped if cert absent — dev/test mode)
    let cert = target.cert_path();
    if cert.exists() {
        log::debug!(
            "[{}] verifying OTA signature using cert={}",
            target.name,
            cert.display()
        );
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
    log::debug!("[{}] stopping service {}", target.name, target.service);
    let _ = systemctl("stop", target.service);
    log::debug!("[{}] backing up current binary", target.name);
    backup_current(target);

    log::debug!(
        "[{}] replacing binary {}",
        target.name,
        target.binary_path.display()
    );
    if let Some(parent) = target.binary_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create binary dir: {e}"))?;
    }
    fs::copy(&new_bin, &target.binary_path).map_err(|e| format!("copy binary: {e}"))?;
    make_executable(&target.binary_path).map_err(|e| format!("chmod: {e}"))?;

    log::debug!("[{}] starting service {}", target.name, target.service);
    let _ = systemctl("start", target.service);

    // Poll for up to 10 s (skip for gateway — it restarts itself, breaking the poll)
    let is_gateway = target.name == "gateway";
    let running = if is_gateway {
        log::debug!(
            "[{}] skipping health poll for self-restarting gateway",
            target.name
        );
        true // assume success; gateway service restart will proceed independently
    } else {
        log::debug!(
            "[{}] polling service health for {}",
            target.name,
            target.service
        );
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
    let status = Command::new("systemctl").args([action, service]).status()?;
    if status.success() {
        log::debug!("systemctl {} {} succeeded", action, service);
    } else {
        log::warn!(
            "systemctl {} {} exited with status {}",
            action,
            service,
            status
        );
    }
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
    if target.binary_path.exists() {
        log::debug!(
            "[{}] writing backup to {}",
            target.name,
            target.backup_path().display()
        );
        let _ = fs::copy(&target.binary_path, target.backup_path());
    }
}

fn restore_backup(target: &Target) {
    let bak = target.backup_path();
    if bak.exists() {
        log::debug!("[{}] restoring backup from {}", target.name, bak.display());
        if let Some(parent) = target.binary_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::copy(&bak, &target.binary_path);
        let _ = make_executable(&target.binary_path);
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
    log::debug!(
        "verifying signature bin={} sig={} cert={}",
        bin.display(),
        sig.display(),
        cert.display()
    );
    let status = Command::new("openssl")
        .args(["dgst", "-sha256", "-verify"])
        .arg(cert)
        .arg("-signature")
        .arg(sig)
        .arg(bin)
        .status()
        .map_err(|e| format!("openssl exec error: {e}"))?;
    if status.success() {
        log::debug!("signature verification succeeded for {}", bin.display());
        Ok(())
    } else {
        Err("signature verification failed".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn ota_zip_bytes(bin_name: &str, binary_bytes: &[u8], version: &str) -> Vec<u8> {
        let mut hasher = Sha256::new();
        hasher.update(binary_bytes);
        let hash = hex::encode(hasher.finalize());

        let cursor = io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(cursor);
        let options = SimpleFileOptions::default();
        zip.start_file(bin_name, options).unwrap();
        zip.write_all(binary_bytes).unwrap();
        zip.start_file(format!("{bin_name}.sha256"), options)
            .unwrap();
        zip.write_all(format!("{hash}\n").as_bytes()).unwrap();
        zip.start_file(format!("{bin_name}.version"), options)
            .unwrap();
        zip.write_all(format!("{version}\n").as_bytes()).unwrap();
        zip.start_file(format!("{bin_name}.sig"), options).unwrap();
        zip.write_all(b"dev-signature").unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn apply_update_replaces_real_binary_not_public_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let public_dir = tmp.path().join("usr/bin");
        let plugin_dir = tmp.path().join("plugins/kaonic-gateway/current");
        let meta_dir = tmp.path().join("meta");
        fs::create_dir_all(&public_dir).unwrap();
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::create_dir_all(&meta_dir).unwrap();

        let binary_path = plugin_dir.join("kaonic-gateway");
        let public_path = public_dir.join("kaonic-gateway");
        fs::write(&binary_path, b"old-binary").unwrap();
        std::os::unix::fs::symlink(&binary_path, &public_path).unwrap();

        let target = Target {
            name: "gateway",
            symlink_path: public_path.clone(),
            binary_path: binary_path.clone(),
            service: "kaonic-gateway.service",
            meta_dir: meta_dir.clone(),
        };

        let result = apply_update(
            &target,
            &ota_zip_bytes("kaonic-gateway", b"new-binary", "2.0.0"),
        );

        assert!(result.is_ok());
        assert_eq!(fs::read_link(&target.symlink_path).unwrap(), binary_path);
        assert_eq!(fs::read(&binary_path).unwrap(), b"new-binary");
        assert_eq!(
            fs::read_to_string(meta_dir.join("kaonic-gateway.version")).unwrap(),
            "2.0.0"
        );
    }
}
