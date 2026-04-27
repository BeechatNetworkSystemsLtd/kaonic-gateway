use std::env;
use std::io;
use std::path::{Path, PathBuf};

use p256::ecdsa::SigningKey;
use p256::elliptic_curve::rand_core::OsRng;
use p256::pkcs8::EncodePrivateKey;
use rcgen::{
    date_time_ymd, BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SerialNumber,
};
use rustls::crypto::{ring, CryptoProvider};
use sha2::{Digest, Sha256};

pub const DEFAULT_CERTS_DIR: &str = "/etc/kaonic/certs";
pub const ROOT_CA_CERT_FILE: &str = "rootca.crt";
pub const ROOT_CA_KEY_FILE: &str = "rootca.key";
pub const GATEWAY_TLS_CERT_PATH: &str = "/etc/kaonic/kaonic-gateway-tls.crt";
pub const GATEWAY_TLS_KEY_PATH: &str = "/etc/kaonic/kaonic-gateway-tls.key";
pub const PLUGIN_TLS_CERT_FILE: &str = "plugin-tls.crt";
pub const PLUGIN_TLS_KEY_FILE: &str = "plugin-tls.key";
pub const ROOT_CA_DOWNLOAD_NAME: &str = "kaonic-rootca.crt";
pub const DEFAULT_CA_SEED_PHRASE: &str = "BEECHAT_KAONIC_GATEWAY";
pub const CA_SEED_PHRASE_ENV: &str = "KAONIC_GATEWAY_CA_SEED_PHRASE";

struct RootCaMaterial {
    cert_pem: String,
    key_der: Vec<u8>,
}

struct RootCaArtifacts {
    cert: Certificate,
    key_pair: KeyPair,
    material: RootCaMaterial,
}

struct GatewayTlsMaterial {
    cert_pem: String,
    key_pem: String,
}

pub fn certs_dir() -> PathBuf {
    env::var("KAONIC_GATEWAY_CERTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CERTS_DIR))
}

pub fn ensure_certs_dir() -> io::Result<PathBuf> {
    let dir = certs_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn ensure_root_ca_files() -> io::Result<PathBuf> {
    let dir = ensure_certs_dir()?;
    let root_ca = load_or_create_root_ca_artifacts(&dir)?;

    write_if_changed(
        &root_ca_cert_path_for(&dir),
        root_ca.material.cert_pem.as_bytes(),
    )?;
    write_if_changed(&dir.join(ROOT_CA_KEY_FILE), &root_ca.material.key_der)?;
    set_private_permissions(&dir.join(ROOT_CA_KEY_FILE))?;

    Ok(dir)
}

pub fn ensure_gateway_tls_files() -> io::Result<PathBuf> {
    let dir = ensure_root_ca_files()?;
    let root_ca = load_or_create_root_ca_artifacts(&dir)?;
    let device_identity = device_identity();
    let gateway_tls = generate_service_tls_material(
        &root_ca,
        "gateway-tls-v2",
        &service_common_name("kaonic-gateway", &device_identity),
        deterministic_gateway_tls_key_pair()
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?,
    )
    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    let cert_path = gateway_tls_cert_path();
    let key_path = gateway_tls_key_path();
    ensure_parent_dir(&cert_path)?;
    ensure_parent_dir(&key_path)?;
    write_if_changed(&cert_path, gateway_tls.cert_pem.as_bytes())?;
    write_if_changed(&key_path, gateway_tls.key_pem.as_bytes())?;
    set_private_permissions(&key_path)?;

    log::debug!(
        "prepared local HTTPS files root_ca_dir={} tls_cert={} tls_key={}",
        dir.display(),
        cert_path.display(),
        key_path.display()
    );
    Ok(dir)
}

pub fn ensure_plugin_tls_files(current_dir: &Path, plugin_id: &str) -> io::Result<()> {
    let dir = ensure_root_ca_files()?;
    let root_ca = load_or_create_root_ca_artifacts(&dir)?;
    let device_identity = device_identity();
    let plugin_tls = generate_service_tls_material(
        &root_ca,
        &format!("plugin-tls-v1:{plugin_id}"),
        &service_common_name(plugin_id, &device_identity),
        deterministic_plugin_tls_key_pair(plugin_id)?,
    )
    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    let cert_path = current_dir.join(PLUGIN_TLS_CERT_FILE);
    let key_path = current_dir.join(PLUGIN_TLS_KEY_FILE);
    ensure_parent_dir(&cert_path)?;
    ensure_parent_dir(&key_path)?;
    write_if_changed(&cert_path, plugin_tls.cert_pem.as_bytes())?;
    write_if_changed(&key_path, plugin_tls.key_pem.as_bytes())?;
    set_private_permissions(&key_path)?;
    Ok(())
}

pub fn install_rustls_crypto_provider() {
    if CryptoProvider::get_default().is_some() {
        return;
    }
    let _ = ring::default_provider().install_default();
}

pub fn root_ca_cert_path() -> PathBuf {
    certs_dir().join(ROOT_CA_CERT_FILE)
}

pub fn root_ca_cert_path_for(dir: &Path) -> PathBuf {
    dir.join(ROOT_CA_CERT_FILE)
}

pub fn gateway_tls_cert_path() -> PathBuf {
    PathBuf::from(GATEWAY_TLS_CERT_PATH)
}

pub fn gateway_tls_key_path() -> PathBuf {
    PathBuf::from(GATEWAY_TLS_KEY_PATH)
}

fn ca_seed_phrase() -> String {
    env::var(CA_SEED_PHRASE_ENV).unwrap_or_else(|_| DEFAULT_CA_SEED_PHRASE.to_string())
}

fn load_or_create_root_ca_artifacts(dir: &Path) -> io::Result<RootCaArtifacts> {
    let key_path = dir.join(ROOT_CA_KEY_FILE);
    let key_der = match std::fs::read(&key_path) {
        Ok(existing) if !existing.is_empty() && !is_legacy_root_ca_key_der(&existing) => existing,
        Ok(_) => generate_secure_root_ca_key_der()?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => generate_secure_root_ca_key_der()?,
        Err(err) => return Err(err),
    };
    generate_root_ca_artifacts(&key_der, read_device_serial().as_deref())
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
}

fn generate_root_ca_artifacts(
    key_der: &[u8],
    device_serial: Option<&str>,
) -> Result<RootCaArtifacts, String> {
    let key_pair =
        KeyPair::try_from(key_der).map_err(|err| format!("load local root CA key: {err}"))?;

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    let common_name = root_ca_common_name(device_serial);
    dn.push(DnType::CommonName, common_name.clone());
    dn.push(DnType::OrganizationName, "Beechat Network Systems Ltd");
    params.distinguished_name = dn;
    params.not_before = date_time_ymd(2024, 1, 1);
    params.not_after = date_time_ymd(2044, 1, 1);
    params.serial_number = Some(derived_serial_number(
        "root-ca-v4",
        key_der,
        &[common_name.as_bytes()],
    ));
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.use_authority_key_identifier_extension = true;
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let cert = params
        .self_signed(&key_pair)
        .map_err(|err| format!("generate local root CA certificate: {err}"))?;

    Ok(RootCaArtifacts {
        material: RootCaMaterial {
            cert_pem: cert.pem(),
            key_der: key_der.to_vec(),
        },
        cert,
        key_pair,
    })
}

fn root_ca_common_name(device_serial: Option<&str>) -> String {
    match device_serial
        .map(str::trim)
        .filter(|serial| !serial.is_empty())
    {
        Some(serial) => format!("Kaonic Local Root CA {serial}"),
        None => "Kaonic Local Root CA".to_string(),
    }
}

fn generate_secure_root_ca_key_der() -> io::Result<Vec<u8>> {
    SigningKey::random(&mut OsRng)
        .to_pkcs8_der()
        .map_err(|err| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("encode local root CA key: {err}"),
            )
        })
        .map(|document| document.as_bytes().to_vec())
}

fn is_legacy_root_ca_key_der(key_der: &[u8]) -> bool {
    let Ok(legacy_signing_key) = deterministic_p256_signing_key(&ca_seed_phrase()) else {
        return false;
    };
    let Ok(legacy_key_der) = legacy_signing_key.to_pkcs8_der() else {
        return false;
    };
    key_der == legacy_key_der.as_bytes()
}

fn deterministic_p256_signing_key(seed_phrase: &str) -> Result<SigningKey, String> {
    let mut material = Sha256::digest(seed_phrase.as_bytes()).to_vec();
    for _ in 0..8 {
        if let Ok(signing_key) = SigningKey::from_slice(&material) {
            return Ok(signing_key);
        }
        material = Sha256::digest(&material).to_vec();
    }
    Err("derive deterministic P-256 root CA key".into())
}

fn generate_service_tls_material(
    root_ca: &RootCaArtifacts,
    serial_label: &str,
    common_name: &str,
    key_pair: KeyPair,
) -> Result<GatewayTlsMaterial, String> {
    let subject_alt_names = service_subject_alt_names();
    let mut params = CertificateParams::new(subject_alt_names)
        .map_err(|err| format!("build gateway TLS SANs: {err}"))?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    dn.push(DnType::OrganizationName, "Beechat Network Systems Ltd");
    params.distinguished_name = dn;
    params.not_before = date_time_ymd(2024, 1, 1);
    params.not_after = date_time_ymd(2034, 1, 1);
    let san_context = params
        .subject_alt_names
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join("|");
    params.serial_number = Some(derived_serial_number(
        serial_label,
        key_pair.serialized_der(),
        &[common_name.as_bytes(), san_context.as_bytes()],
    ));
    params.use_authority_key_identifier_extension = true;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params
        .signed_by(&key_pair, &root_ca.cert, &root_ca.key_pair)
        .map_err(|err| format!("generate gateway TLS certificate: {err}"))?;

    Ok(GatewayTlsMaterial {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
    })
}

fn deterministic_gateway_tls_key_pair() -> Result<KeyPair, String> {
    let device_identity = device_identity();
    let signing_key = deterministic_p256_signing_key(&format!(
        "{}|gateway-tls|{device_identity}",
        ca_seed_phrase()
    ))?;
    let key_der = signing_key
        .to_pkcs8_der()
        .map_err(|err| format!("encode deterministic gateway TLS key: {err}"))?
        .as_bytes()
        .to_vec();
    KeyPair::try_from(key_der.as_slice())
        .map_err(|err| format!("load deterministic gateway TLS key: {err}"))
}

fn deterministic_plugin_tls_key_pair(plugin_id: &str) -> io::Result<KeyPair> {
    let device_identity = device_identity();
    let signing_key = deterministic_p256_signing_key(&format!(
        "{}|plugin-tls|{}|{}",
        ca_seed_phrase(),
        device_identity,
        plugin_id
    ))
    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    let key_der = signing_key
        .to_pkcs8_der()
        .map_err(|err| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("encode deterministic plugin TLS key: {err}"),
            )
        })?
        .as_bytes()
        .to_vec();
    KeyPair::try_from(key_der.as_slice()).map_err(|err| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("load deterministic plugin TLS key: {err}"),
        )
    })
}

fn service_subject_alt_names() -> Vec<String> {
    let mut subject_alt_names = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "192.168.10.1".to_string(),
    ];
    if let Some(hostname) = read_hostname() {
        if !subject_alt_names.contains(&hostname) {
            subject_alt_names.push(hostname.clone());
        }
        if !hostname.ends_with(".local") {
            let local_name = format!("{hostname}.local");
            if !subject_alt_names.contains(&local_name) {
                subject_alt_names.push(local_name);
            }
        }
    }
    subject_alt_names
}

fn service_common_name(service_name: &str, device_identity: &str) -> String {
    format!("{service_name} {device_identity}")
}

fn device_identity() -> String {
    read_device_serial()
        .or_else(read_hostname)
        .unwrap_or_else(|| "kaonic-gateway".to_string())
}

fn derived_serial_number(label: &str, material: &[u8], context_parts: &[&[u8]]) -> SerialNumber {
    let extra_len = context_parts
        .iter()
        .map(|part| part.len() + 1)
        .sum::<usize>();
    let mut input = Vec::with_capacity(label.len() + 1 + material.len() + extra_len);
    input.extend_from_slice(label.as_bytes());
    input.push(0);
    input.extend_from_slice(material);
    for part in context_parts {
        input.push(0);
        input.extend_from_slice(part);
    }
    let digest = Sha256::digest(&input);
    let mut serial = digest[..16].to_vec();
    serial[0] &= 0x7f;
    if serial.iter().all(|byte| *byte == 0) {
        serial[15] = 1;
    }
    SerialNumber::from_slice(&serial)
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, content: &[u8]) -> io::Result<()> {
    let current = std::fs::read(path).ok();
    if current.as_deref() == Some(content) {
        return Ok(());
    }
    std::fs::write(path, content)
}

fn read_hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| {
            value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
        })
}

fn read_device_serial() -> Option<String> {
    std::fs::read_to_string("/etc/kaonic/kaonic_serial")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn set_private_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use p256::pkcs8::EncodePrivateKey;

    use super::{
        derived_serial_number, deterministic_gateway_tls_key_pair, deterministic_p256_signing_key,
        generate_root_ca_artifacts, generate_service_tls_material, service_common_name,
    };

    #[test]
    fn root_ca_material_matches_supplied_key() {
        let key_der = deterministic_p256_signing_key("root-ca-test")
            .expect("root-ca signing key")
            .to_pkcs8_der()
            .expect("root-ca key der")
            .as_bytes()
            .to_vec();
        let first = generate_root_ca_artifacts(&key_der, Some("20254718-WPQCDQB6SAXOS6ZQ"))
            .expect("first root ca");
        let second = generate_root_ca_artifacts(&key_der, Some("20254718-WPQCDQB6SAXOS6ZQ"))
            .expect("second root ca");
        let other =
            generate_root_ca_artifacts(&key_der, Some("OTHER-SERIAL")).expect("other root ca");

        assert_eq!(first.material.key_der, second.material.key_der);
        assert_eq!(first.material.key_der, other.material.key_der);
        assert!(first.material.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(second.material.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(other.material.cert_pem.contains("BEGIN CERTIFICATE"));
        assert_ne!(first.material.cert_pem, other.material.cert_pem);
    }

    #[test]
    fn gateway_tls_material_is_generated() {
        let key_der = deterministic_p256_signing_key("root-ca-test")
            .expect("root-ca signing key")
            .to_pkcs8_der()
            .expect("root-ca key der")
            .as_bytes()
            .to_vec();
        let root_ca =
            generate_root_ca_artifacts(&key_der, Some("kaonic-test-device")).expect("root ca");
        let gateway_tls = generate_service_tls_material(
            &root_ca,
            "gateway-tls-test",
            &service_common_name("kaonic-gateway", "kaonic-test-device"),
            deterministic_gateway_tls_key_pair().expect("gateway tls key"),
        )
        .expect("gateway tls");

        assert!(gateway_tls.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(gateway_tls.key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn serial_changes_when_certificate_identity_changes() {
        let material = b"same-key-material";
        let first = derived_serial_number("service-v1", material, &[b"kaonic-gateway device-a"]);
        let second = derived_serial_number("service-v1", material, &[b"kaonic-gateway device-b"]);

        assert_ne!(first, second);
    }
}
