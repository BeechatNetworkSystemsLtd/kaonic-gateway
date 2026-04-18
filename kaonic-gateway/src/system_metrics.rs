use crate::app_types::ServiceStatusDto;

pub const GATEWAY_SERVICE_UNITS: [&str; 4] = [
    "kaonic-commd.service",
    "kaonic-factory.service",
    "kaonic-gateway.service",
    "kaonic-update.service",
];

pub async fn read_cpu_percent_async() -> f32 {
    let Some((idle1, total1)) = parse_stat() else {
        return 0.0;
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let Some((idle2, total2)) = parse_stat() else {
        return 0.0;
    };
    let total_diff = total2.saturating_sub(total1) as f32;
    let idle_diff = idle2.saturating_sub(idle1) as f32;
    if total_diff == 0.0 {
        return 0.0;
    }
    ((total_diff - idle_diff) / total_diff * 100.0 * 10.0).round() / 10.0
}

pub fn read_mem_mb() -> (u64, u64) {
    let Ok(data) = std::fs::read_to_string("/proc/meminfo") else {
        return (0, 0);
    };
    let mut total = 0u64;
    let mut available = 0u64;
    for line in data.lines() {
        if let Some(r) = line.strip_prefix("MemTotal:") {
            total = r
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        } else if let Some(r) = line.strip_prefix("MemAvailable:") {
            available = r
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
    }
    (total.saturating_sub(available) / 1024, total / 1024)
}

pub fn read_fs_mb() -> (u64, u64) {
    #[cfg(unix)]
    {
        use std::ffi::CString;

        let probe_path = user_filesystem_probe_path();
        let Ok(path) = CString::new(probe_path.to_string_lossy().into_owned()) else {
            return (0, 0);
        };
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let rc = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
        if rc != 0 {
            return (0, 0);
        }
        let stats = unsafe { stats.assume_init() };
        let block_size = if stats.f_frsize > 0 {
            stats.f_frsize as u64
        } else {
            stats.f_bsize as u64
        };
        let total = (stats.f_blocks as u64).saturating_mul(block_size);
        let free = (stats.f_bavail as u64).saturating_mul(block_size);
        return (free / 1024 / 1024, total / 1024 / 1024);
    }

    #[cfg(not(unix))]
    {
        (0, 0)
    }
}

pub fn read_os_details() -> String {
    let os_name = read_os_name();
    let kernel = read_kernel_release();
    match (os_name.is_empty(), kernel.is_empty()) {
        (false, false) => format!("{os_name} / {kernel}"),
        (false, true) => os_name,
        (true, false) => kernel,
        (true, true) => "Unknown".into(),
    }
}

pub fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Unknown".into())
}

pub fn read_cpu_model() -> String {
    let Ok(data) = std::fs::read_to_string("/proc/cpuinfo") else {
        return "Unknown".into();
    };

    for key in ["model name", "Hardware", "Processor", "Model"] {
        for line in data.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.trim() == key {
                let value = value.trim();
                if !value.is_empty() {
                    return value.to_string();
                }
            }
        }
    }

    "Unknown".into()
}

pub fn read_architecture() -> String {
    #[cfg(unix)]
    {
        let mut uts = std::mem::MaybeUninit::<libc::utsname>::uninit();
        let rc = unsafe { libc::uname(uts.as_mut_ptr()) };
        if rc == 0 {
            let uts = unsafe { uts.assume_init() };
            let machine = unsafe { std::ffi::CStr::from_ptr(uts.machine.as_ptr()) };
            let value = machine.to_string_lossy().trim().to_string();
            if !value.is_empty() {
                return value;
            }
        }
    }

    std::env::consts::ARCH.into()
}

pub fn read_cpu_cores() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

pub fn read_gateway_services() -> Vec<ServiceStatusDto> {
    GATEWAY_SERVICE_UNITS
        .iter()
        .copied()
        .map(read_service_status)
        .collect()
}

pub fn is_gateway_service_unit(unit: &str) -> bool {
    GATEWAY_SERVICE_UNITS.contains(&unit)
}

#[cfg(target_os = "linux")]
fn read_service_status(unit: &str) -> ServiceStatusDto {
    let output = std::process::Command::new("systemctl")
        .args([
            "show",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=SubState",
            "--value",
            unit,
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let mut lines = stdout.lines();
            let load_state = lines.next().unwrap_or("unknown").trim().to_string();
            let active_state = lines.next().unwrap_or("unknown").trim().to_string();
            let sub_state = lines.next().unwrap_or_default().trim().to_string();
            ServiceStatusDto {
                unit: unit.into(),
                brief_name: service_brief_name(unit).into(),
                status: format_service_status(&load_state, &active_state, &sub_state),
                load_state,
                active_state,
                sub_state,
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let message = if stderr.is_empty() { stdout } else { stderr };
            ServiceStatusDto {
                unit: unit.into(),
                brief_name: service_brief_name(unit).into(),
                load_state: "unknown".into(),
                active_state: "error".into(),
                sub_state: String::new(),
                status: if message.is_empty() {
                    "systemctl error".into()
                } else {
                    message
                },
            }
        }
        Err(err) => ServiceStatusDto {
            unit: unit.into(),
            brief_name: service_brief_name(unit).into(),
            load_state: "unknown".into(),
            active_state: "error".into(),
            sub_state: String::new(),
            status: format!("systemctl unavailable: {err}"),
        },
    }
}

#[cfg(not(target_os = "linux"))]
fn read_service_status(unit: &str) -> ServiceStatusDto {
    ServiceStatusDto {
        unit: unit.into(),
        brief_name: service_brief_name(unit).into(),
        load_state: "mock".into(),
        active_state: "unknown".into(),
        sub_state: String::new(),
        status: "Unavailable on this host".into(),
    }
}

fn service_brief_name(unit: &str) -> &'static str {
    match unit {
        "kaonic-commd.service" => "Radio control",
        "kaonic-factory.service" => "Factory setup",
        "kaonic-gateway.service" => "Web gateway",
        "kaonic-update.service" => "Update agent",
        _ => "Service",
    }
}

#[cfg(target_os = "linux")]
fn format_service_status(load_state: &str, active_state: &str, sub_state: &str) -> String {
    if load_state != "loaded" && !load_state.is_empty() {
        return load_state.to_string();
    }
    if active_state.is_empty() && sub_state.is_empty() {
        return "unknown".into();
    }
    if sub_state.is_empty() || sub_state == active_state {
        return active_state.to_string();
    }
    format!("{active_state} ({sub_state})")
}

fn parse_stat() -> Option<(u64, u64)> {
    let data = std::fs::read_to_string("/proc/stat").ok()?;
    let line = data.lines().next()?;
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if vals.len() < 4 {
        return None;
    }
    Some((vals[3], vals.iter().sum()))
}

#[cfg(unix)]
fn user_filesystem_probe_path() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}

fn read_os_name() -> String {
    if let Ok(data) = std::fs::read_to_string("/etc/os-release") {
        for line in data.lines() {
            if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                return value.trim_matches('"').to_string();
            }
        }
    }
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

fn read_kernel_release() -> String {
    #[cfg(unix)]
    {
        let mut uts = std::mem::MaybeUninit::<libc::utsname>::uninit();
        let rc = unsafe { libc::uname(uts.as_mut_ptr()) };
        if rc == 0 {
            let uts = unsafe { uts.assume_init() };
            let release = unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) };
            return format!("Kernel {}", release.to_string_lossy());
        }
    }

    String::new()
}
