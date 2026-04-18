use thiserror::Error;

use crate::app_types::{NetworkSnapshotDto, WifiStatusDto};
use if_addrs::IfAddr;
#[cfg(not(target_os = "linux"))]
use std::sync::Mutex;

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
const DEFAULT_WIFI_SCRIPT: &str = "/usr/bin/kaonic-wifi-mode";
#[cfg(target_os = "linux")]
const DEFAULT_WIFI_MODE_FILE: &str = "/etc/kaonic/wifi-mode";
#[cfg(target_os = "linux")]
const DEFAULT_WPA_CONF: &str = "/etc/wpa_supplicant-wlan0.conf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiMode {
    Ap,
    Sta,
}

impl WifiMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ap" | "AP" => Some(Self::Ap),
            "sta" | "STA" => Some(Self::Sta),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ap => "ap",
            Self::Sta => "sta",
        }
    }
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("invalid WiFi mode '{0}'")]
    InvalidMode(String),
    #[error("SSID is required")]
    InvalidSsid,
    #[error("PSK must be between 8 and 63 characters")]
    InvalidPsk,
    #[error("no saved station WiFi configuration is available")]
    MissingStaConfig,
    #[error("network mock state lock poisoned")]
    StatePoisoned,
    #[error("blocking network task failed: {0}")]
    TaskJoin(String),
    #[error("failed to update WiFi mode file: {0}")]
    ModeFileWrite(String),
    #[error("failed to execute `{command}`: {source}")]
    CommandIo {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` failed: {message}")]
    CommandFailed { command: String, message: String },
}

#[derive(Debug)]
pub struct NetworkService {
    #[cfg(not(target_os = "linux"))]
    mock: Mutex<MockNetworkState>,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, Clone)]
struct MockNetworkState {
    mode: WifiMode,
    configured_ssid: Option<String>,
    connected_ssid: Option<String>,
}

#[cfg(not(target_os = "linux"))]
impl Default for MockNetworkState {
    fn default() -> Self {
        Self {
            mode: WifiMode::Ap,
            configured_ssid: None,
            connected_ssid: None,
        }
    }
}

impl Default for NetworkService {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "linux"))]
            mock: Mutex::new(MockNetworkState::default()),
        }
    }
}

impl NetworkService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> Result<NetworkSnapshotDto, NetworkError> {
        #[cfg(target_os = "linux")]
        {
            tokio::task::spawn_blocking(read_network_snapshot_linux)
                .await
                .map_err(|err| NetworkError::TaskJoin(err.to_string()))?
        }

        #[cfg(not(target_os = "linux"))]
        {
            self.snapshot_mock()
        }
    }

    pub async fn set_wifi_mode(&self, mode: WifiMode) -> Result<(), NetworkError> {
        #[cfg(target_os = "linux")]
        {
            tokio::task::spawn_blocking(move || set_wifi_mode_linux(mode))
                .await
                .map_err(|err| NetworkError::TaskJoin(err.to_string()))?
        }

        #[cfg(not(target_os = "linux"))]
        {
            self.set_wifi_mode_mock(mode)
        }
    }

    pub async fn connect_wifi(&self, ssid: &str, psk: &str) -> Result<(), NetworkError> {
        validate_wifi_credentials(ssid, psk)?;

        #[cfg(target_os = "linux")]
        {
            let ssid = ssid.trim().to_string();
            let psk = psk.to_string();
            tokio::task::spawn_blocking(move || connect_wifi_linux(&ssid, &psk))
                .await
                .map_err(|err| NetworkError::TaskJoin(err.to_string()))?
        }

        #[cfg(not(target_os = "linux"))]
        {
            self.connect_wifi_mock(ssid)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn snapshot_mock(&self) -> Result<NetworkSnapshotDto, NetworkError> {
        let state = self.mock.lock().map_err(|_| NetworkError::StatePoisoned)?;
        Ok(NetworkSnapshotDto {
            backend: "Mock".into(),
            interface_source: "mock ifconfig".into(),
            interface_details: mock_interface_details(),
            wifi: WifiStatusDto {
                mode: state.mode.as_str().into(),
                configured_ssid: state.configured_ssid.clone(),
                connected_ssid: state.connected_ssid.clone(),
                wlan0_ip: match state.mode {
                    WifiMode::Ap => Some("192.168.10.1".into()),
                    WifiMode::Sta if state.connected_ssid.is_some() => Some("192.168.1.42".into()),
                    WifiMode::Sta => None,
                },
                hostapd_status: if state.mode == WifiMode::Ap {
                    "active".into()
                } else {
                    "inactive".into()
                },
                wpa_supplicant_status: if state.mode == WifiMode::Sta {
                    "running".into()
                } else {
                    "stopped".into()
                },
                link_details: if let Some(ssid) = &state.connected_ssid {
                    format!("Connected to mock network\nSSID: {ssid}\nSignal: -43 dBm")
                } else if state.mode == WifiMode::Ap {
                    "AP mode enabled for mock device".into()
                } else {
                    "Not connected.".into()
                },
            },
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn set_wifi_mode_mock(&self, mode: WifiMode) -> Result<(), NetworkError> {
        let mut state = self.mock.lock().map_err(|_| NetworkError::StatePoisoned)?;
        match mode {
            WifiMode::Ap => {
                state.mode = WifiMode::Ap;
                state.connected_ssid = None;
            }
            WifiMode::Sta => {
                state.mode = WifiMode::Sta;
                state.connected_ssid = state.configured_ssid.clone();
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn connect_wifi_mock(&self, ssid: &str) -> Result<(), NetworkError> {
        let mut state = self.mock.lock().map_err(|_| NetworkError::StatePoisoned)?;
        state.mode = WifiMode::Sta;
        state.configured_ssid = Some(ssid.trim().to_string());
        state.connected_ssid = state.configured_ssid.clone();
        Ok(())
    }
}

pub fn read_interface_ipv4(interface_name: &str) -> Option<String> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find_map(|interface| {
            if interface.name != interface_name {
                return None;
            }
            match interface.addr {
                IfAddr::V4(addr) => Some(addr.ip.to_string()),
                IfAddr::V6(_) => None,
            }
        })
}

fn validate_wifi_credentials(ssid: &str, psk: &str) -> Result<(), NetworkError> {
    if ssid.trim().is_empty() {
        return Err(NetworkError::InvalidSsid);
    }
    let psk_len = psk.chars().count();
    if !(8..=63).contains(&psk_len) {
        return Err(NetworkError::InvalidPsk);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_network_snapshot_linux() -> Result<NetworkSnapshotDto, NetworkError> {
    let (interface_source, interface_details) = read_interface_details_linux()?;
    let wifi = read_wifi_status_linux()?;

    Ok(NetworkSnapshotDto {
        backend: "Linux".into(),
        interface_source,
        interface_details,
        wifi,
    })
}

#[cfg(target_os = "linux")]
fn read_interface_details_linux() -> Result<(String, String), NetworkError> {
    match run_command("ifconfig -a", "ifconfig", &["-a"]) {
        Ok(output) => Ok(("ifconfig -a".into(), output)),
        Err(_) => run_command("ip addr show", "ip", &["addr", "show"])
            .map(|output| ("ip addr show".into(), output)),
    }
}

#[cfg(target_os = "linux")]
fn read_wifi_status_linux() -> Result<WifiStatusDto, NetworkError> {
    let mode = read_current_mode_linux();
    let configured_ssid = read_configured_ssid(&wpa_conf_path());
    let wlan0_ip = match mode {
        WifiMode::Ap => Some("192.168.10.1".into()),
        WifiMode::Sta => read_wifi_ip_linux()?,
    };
    let hostapd_status =
        read_service_status_linux("hostapd.service").unwrap_or_else(|_| "unknown".into());
    let wpa_supplicant_status = if wpa_pid_file_path().exists() {
        "running".into()
    } else {
        "stopped".into()
    };
    let (connected_ssid, link_details) = match mode {
        WifiMode::Ap => (None, "Access Point mode enabled.".into()),
        WifiMode::Sta => read_station_link_linux()?,
    };

    Ok(WifiStatusDto {
        mode: mode.as_str().into(),
        configured_ssid,
        connected_ssid,
        wlan0_ip,
        hostapd_status,
        wpa_supplicant_status,
        link_details,
    })
}

#[cfg(target_os = "linux")]
fn set_wifi_mode_linux(mode: WifiMode) -> Result<(), NetworkError> {
    let script = wifi_script_path();

    match mode {
        WifiMode::Ap => {
            run_program(format!("{} ap", script.display()), &script, &["ap"]).map(|_| ())
        }
        WifiMode::Sta => {
            let wpa_conf = wpa_conf_path();
            write_wifi_mode_file("sta")?;
            if wpa_conf.exists() {
                run_program(format!("{} apply", script.display()), &script, &["apply"]).map(|_| ())
            } else {
                prepare_station_mode_without_config_linux()
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn connect_wifi_linux(ssid: &str, psk: &str) -> Result<(), NetworkError> {
    let script = wifi_script_path();
    run_program(
        format!("{} sta <ssid> <redacted>", script.display()),
        &script,
        &["sta", ssid, psk],
    )
    .map(|_| ())
}

#[cfg(target_os = "linux")]
fn prepare_station_mode_without_config_linux() -> Result<(), NetworkError> {
    write_sta_network_override_linux()?;
    let _ = run_command(
        "systemctl stop hostapd.service",
        "systemctl",
        &["stop", "hostapd.service"],
    );
    run_command(
        "systemctl restart systemd-networkd.service",
        "systemctl",
        &["restart", "systemd-networkd.service"],
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_wifi_mode_file(mode: &str) -> Result<(), NetworkError> {
    let mode_file = wifi_mode_file_path();
    if let Some(parent) = mode_file.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| NetworkError::ModeFileWrite(format!("{parent:?}: {err}")))?;
    }
    fs::write(&mode_file, format!("{mode}\n"))
        .map_err(|err| NetworkError::ModeFileWrite(format!("{mode_file:?}: {err}")))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_sta_network_override_linux() -> Result<(), NetworkError> {
    let path = Path::new("/etc/systemd/network/62-wlan0.network");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| NetworkError::ModeFileWrite(format!("{parent:?}: {err}")))?;
    }
    fs::write(
        path,
        "[Match]\nName=wlan0\n\n[Network]\nDHCP=yes\nIPv6AcceptRA=yes\n",
    )
    .map_err(|err| NetworkError::ModeFileWrite(format!("{path:?}: {err}")))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_current_mode_linux() -> WifiMode {
    fs::read_to_string(wifi_mode_file_path())
        .ok()
        .and_then(|value| WifiMode::parse(value.lines().next().unwrap_or_default().trim()))
        .unwrap_or(WifiMode::Ap)
}

#[cfg(target_os = "linux")]
fn read_station_link_linux() -> Result<(Option<String>, String), NetworkError> {
    let output = run_command(
        "iw dev wlan0 link",
        "iw",
        &["dev", wifi_iface_name(), "link"],
    )?;
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Not connected.") {
        return Ok((None, "Disconnected".into()));
    }

    let connected_ssid = output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("SSID: ")
            .map(|value| value.trim().to_string())
    });

    Ok((connected_ssid, output))
}

#[cfg(target_os = "linux")]
fn read_wifi_ip_linux() -> Result<Option<String>, NetworkError> {
    let iface = wifi_iface_name();
    match run_command(
        &format!("ip -4 addr show dev {iface}"),
        "ip",
        &["-4", "addr", "show", "dev", iface],
    ) {
        Ok(output) => Ok(parse_ipv4_from_ip_addr(&output)),
        Err(_) => run_command(&format!("ifconfig {iface}"), "ifconfig", &[iface])
            .map(|output| parse_ipv4_from_ifconfig(&output))
            .or(Ok(None)),
    }
}

#[cfg(target_os = "linux")]
fn parse_ipv4_from_ip_addr(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let value = line.trim().strip_prefix("inet ")?;
        Some(value.split('/').next()?.trim().to_string())
    })
}

#[cfg(target_os = "linux")]
fn parse_ipv4_from_ifconfig(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("inet ") {
            return Some(value.split_whitespace().next()?.trim().to_string());
        }
        let start = trimmed.find("inet ")? + 5;
        Some(
            trimmed[start..]
                .split_whitespace()
                .next()?
                .trim()
                .to_string(),
        )
    })
}

#[cfg(target_os = "linux")]
fn read_service_status_linux(service: &str) -> Result<String, NetworkError> {
    let display = format!("systemctl is-active {service}");
    run_command(&display, "systemctl", &["is-active", service])
        .map(|output| output.trim().to_string())
}

#[cfg(target_os = "linux")]
fn run_command(display: &str, program: &str, args: &[&str]) -> Result<String, NetworkError> {
    run_program(display.to_string(), Path::new(program), args)
}

#[cfg(target_os = "linux")]
fn run_program(display: String, program: &Path, args: &[&str]) -> Result<String, NetworkError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| NetworkError::CommandIo {
            command: display.clone(),
            source,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if stderr.is_empty() { stdout } else { stderr };
        return Err(NetworkError::CommandFailed {
            command: display,
            message,
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "linux")]
fn read_configured_ssid(path: &Path) -> Option<String> {
    let data = fs::read_to_string(path).ok()?;
    data.lines().find_map(|line| {
        let value = line.trim();
        if value.starts_with('#') {
            return None;
        }
        let ssid = value.strip_prefix("ssid=")?;
        Some(ssid.trim_matches('"').to_string())
    })
}

#[cfg(target_os = "linux")]
fn wifi_script_path() -> PathBuf {
    if let Some(path) = std::env::var_os("KAONIC_WIFI_MODE_SCRIPT") {
        return PathBuf::from(path);
    }

    for candidate in [
        DEFAULT_WIFI_SCRIPT,
        "/home/root/wifi_mode.sh",
        "wifi_mode.sh",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_absolute() {
            if path.exists() {
                return path;
            }
        } else {
            return path;
        }
    }

    PathBuf::from(DEFAULT_WIFI_SCRIPT)
}

#[cfg(target_os = "linux")]
fn wifi_mode_file_path() -> PathBuf {
    std::env::var_os("KAONIC_WIFI_MODE_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WIFI_MODE_FILE))
}

#[cfg(target_os = "linux")]
fn wpa_conf_path() -> PathBuf {
    std::env::var_os("KAONIC_WIFI_WPA_CONF")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WPA_CONF))
}

#[cfg(target_os = "linux")]
fn wpa_pid_file_path() -> PathBuf {
    PathBuf::from("/run/wpa_supplicant-wlan0.pid")
}

#[cfg(target_os = "linux")]
fn wifi_iface_name() -> &'static str {
    "wlan0"
}

#[cfg(not(target_os = "linux"))]
fn mock_interface_details() -> String {
    [
        "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384",
        "    inet 127.0.0.1 netmask 0xff000000",
        "en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500",
        "    inet 192.168.1.42 netmask 0xffffff00 broadcast 192.168.1.255",
        "wlan0: flags=8843<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> mtu 1500",
        "    status: mock",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    use super::NetworkService;
    use super::{validate_wifi_credentials, NetworkError, WifiMode};

    #[test]
    fn parse_wifi_modes() {
        assert_eq!(WifiMode::parse("ap"), Some(WifiMode::Ap));
        assert_eq!(WifiMode::parse("sta"), Some(WifiMode::Sta));
        assert_eq!(WifiMode::parse("nope"), None);
    }

    #[test]
    fn reject_invalid_wifi_credentials() {
        let err = validate_wifi_credentials("", "12345678").expect_err("ssid should fail");
        assert!(matches!(err, NetworkError::InvalidSsid));

        let err = validate_wifi_credentials("test", "short").expect_err("psk should fail");
        assert!(matches!(err, NetworkError::InvalidPsk));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn mock_station_mode_does_not_require_saved_config() {
        let service = NetworkService::new();
        service
            .set_wifi_mode_mock(WifiMode::Sta)
            .expect("station mode should be allowed without saved config");
    }
}
