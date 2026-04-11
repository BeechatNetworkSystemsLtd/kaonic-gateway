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

        let Ok(path) = CString::new("/") else {
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
