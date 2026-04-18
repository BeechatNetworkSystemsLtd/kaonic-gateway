//! Linux-specific `ip` + `iptables` bindings.
//!
//! Kept behind `cfg(target_os = "linux")` so the crate still builds for dev
//! machines (macOS) without spawning real subprocesses.

use std::net::Ipv4Addr;

use cidr::Ipv4Cidr;

use super::tun::{TCP_MSS, TUN_MTU};

#[derive(Clone, Copy)]
pub struct LocalRouteTranslation {
    pub local: Ipv4Cidr,
    pub exported: Ipv4Cidr,
}

#[cfg(target_os = "linux")]
pub fn configure_tun_address(interface: &str, addr: Ipv4Addr, prefix: u8) -> std::io::Result<()> {
    let cidr = format!("{addr}/{prefix}");
    let mtu = TUN_MTU.to_string();
    run_ip(&["link", "set", "dev", interface, "up"])?;
    run_ip(&["link", "set", "dev", interface, "mtu", &mtu])?;
    run_ip(&["addr", "replace", &cidr, "dev", interface])
}

#[cfg(not(target_os = "linux"))]
pub fn configure_tun_address(_: &str, _: Ipv4Addr, _: u8) -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn enable_forwarding() -> std::io::Result<()> {
    run("sysctl", &["-w", "net.ipv4.ip_forward=1"])
}

#[cfg(not(target_os = "linux"))]
pub fn enable_forwarding() -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn replace_route(interface: &str, route: &str) -> std::io::Result<()> {
    run_ip(&["route", "replace", route, "dev", interface])
}

#[cfg(not(target_os = "linux"))]
pub fn replace_route(_: &str, _: &str) -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn delete_route(interface: &str, route: &str) {
    let _ = run_ip(&["route", "del", route, "dev", interface]);
}

#[cfg(not(target_os = "linux"))]
pub fn delete_route(_: &str, _: &str) {}

#[cfg(target_os = "linux")]
pub fn supports_route_aliasing() -> bool {
    resolve_iptables().is_some()
}

#[cfg(not(target_os = "linux"))]
pub fn supports_route_aliasing() -> bool {
    false
}

pub fn backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else {
        "mock"
    }
}

#[cfg(target_os = "linux")]
pub fn sync_route_translations(
    interface: &str,
    translations: &[LocalRouteTranslation],
) -> std::io::Result<()> {
    const PRE: &str = "KAONIC_VPN_PREROUTING";
    const POST: &str = "KAONIC_VPN_POSTROUTING";
    const MSS: &str = "KAONIC_VPN_MSS";

    let Some(ipt) = resolve_iptables() else {
        return Ok(());
    };

    ensure_chain(ipt, "nat", PRE)?;
    ensure_chain(ipt, "nat", POST)?;
    ensure_chain(ipt, "mangle", MSS)?;
    ensure_jump(ipt, "nat", "PREROUTING", "-i", interface, PRE)?;
    ensure_jump(ipt, "nat", "POSTROUTING", "-o", interface, POST)?;
    ensure_jump(ipt, "mangle", "FORWARD", "-i", interface, MSS)?;
    ensure_jump(ipt, "mangle", "FORWARD", "-o", interface, MSS)?;
    run(ipt, &["-t", "nat", "-F", PRE])?;
    run(ipt, &["-t", "nat", "-F", POST])?;
    run(ipt, &["-t", "mangle", "-F", MSS])?;

    let mss = TCP_MSS.to_string();
    run(
        ipt,
        &[
            "-t", "mangle", "-A", MSS, "-p", "tcp", "--tcp-flags", "SYN,RST", "SYN", "-j", "TCPMSS",
            "--set-mss", &mss,
        ],
    )?;

    for translation in translations {
        if translation.local == translation.exported {
            continue;
        }
        let exported = translation.exported.to_string();
        let local = translation.local.to_string();
        run(
            ipt,
            &[
                "-t", "nat", "-A", PRE, "-d", &exported, "-j", "NETMAP", "--to", &local,
            ],
        )?;
        run(
            ipt,
            &[
                "-t", "nat", "-A", POST, "-s", &local, "-j", "NETMAP", "--to", &exported,
            ],
        )?;
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn sync_route_translations(_: &str, _: &[LocalRouteTranslation]) -> std::io::Result<()> {
    Ok(())
}

// ── Internals ────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn run_ip(args: &[&str]) -> std::io::Result<()> {
    run("ip", args)
}

#[cfg(target_os = "linux")]
fn run(cmd: &str, args: &[&str]) -> std::io::Result<()> {
    let output = std::process::Command::new(cmd).args(args).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "{cmd} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ))
    }
}

#[cfg(target_os = "linux")]
fn ensure_chain(cmd: &str, table: &str, chain: &str) -> std::io::Result<()> {
    match run(cmd, &["-t", table, "-N", chain]) {
        Ok(()) => Ok(()),
        Err(err) if err.to_string().contains("Chain already exists") => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(target_os = "linux")]
fn ensure_jump(
    cmd: &str,
    table: &str,
    parent: &str,
    iface_flag: &str,
    interface: &str,
    target: &str,
) -> std::io::Result<()> {
    if run(
        cmd,
        &[
            "-t", table, "-C", parent, iface_flag, interface, "-j", target,
        ],
    )
    .is_ok()
    {
        return Ok(());
    }
    run(
        cmd,
        &[
            "-t", table, "-A", parent, iface_flag, interface, "-j", target,
        ],
    )
}

#[cfg(target_os = "linux")]
fn resolve_iptables() -> Option<&'static str> {
    for cmd in ["iptables", "iptables-nft", "iptables-legacy"] {
        if supports_netmap(cmd) {
            return Some(cmd);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn supports_netmap(cmd: &str) -> bool {
    let Ok(output) = std::process::Command::new(cmd)
        .args(["-j", "NETMAP", "-h"])
        .output()
    else {
        return false;
    };
    if output.status.success() {
        return true;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout.contains("NETMAP") || stderr.contains("NETMAP")
}
