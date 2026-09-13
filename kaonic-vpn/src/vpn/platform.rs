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

/// What this node will act as an internet/LAN gateway for.
///
/// Off by default. When on, traffic that arrives from the tunnel for one of
/// `routes` is forwarded out `egress` with this node's own address as the
/// source, so the machine being reached — a TAK server, say — answers over its
/// normal default route and needs no knowledge of the tunnel at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayRules {
    pub enabled: bool,
    /// The tunnel network; the only source range allowed to be forwarded.
    pub tunnel_network: Option<Ipv4Cidr>,
    /// Destinations peers may reach through this node, each paired with the
    /// interface to forward it out of. Empty forwards nothing: the operator
    /// names what is shared, rather than sharing everything by omission.
    /// `0.0.0.0/0` is how full internet access is expressed.
    pub routes: Vec<(Ipv4Cidr, String)>,
}

/// Using another node as this node's router.
///
/// The mirror of [`GatewayRules`]: that lets a node *serve* a network, this
/// lets a node *use* one. Clients on this node's own LAN send with their own
/// addresses, which the serving node's rules reject — so their traffic is
/// masqueraded into this node's tunnel address on the way in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UplinkRules {
    pub enabled: bool,
    /// Destinations reached through the chosen router.
    pub routes: Vec<Ipv4Cidr>,
    /// This node's tunnel network, to tell locally-originated traffic (already
    /// correctly addressed) from forwarded client traffic (which is not).
    pub tunnel_network: Option<Ipv4Cidr>,
}

/// Installs (or removes) the rules that let this node's LAN clients reach a
/// network through the chosen router.
#[cfg(target_os = "linux")]
pub fn sync_uplink(interface: &str, rules: &UplinkRules) -> std::io::Result<()> {
    const FWD: &str = "KAONIC_VPN_UPLINK";
    const NAT: &str = "KAONIC_VPN_UPLINK_NAT";

    let Some(ipt) = resolve_iptables() else {
        return Ok(());
    };

    ensure_chain(ipt, "filter", FWD)?;
    ensure_chain(ipt, "nat", NAT)?;
    ensure_jump(ipt, "filter", "FORWARD", "-o", interface, FWD)?;
    ensure_jump(ipt, "filter", "FORWARD", "-i", interface, FWD)?;
    // Ahead of the alias NETMAP rules, not after them. A client heading for the
    // router would otherwise match this node's own exported-LAN translation and
    // leave wearing an alias address, which the router's rules reject.
    ensure_jump_bare_first(ipt, "nat", "POSTROUTING", NAT)?;
    run(ipt, &["-t", "filter", "-F", FWD])?;
    run(ipt, &["-t", "nat", "-F", NAT])?;

    let Some(tunnel) = rules.tunnel_network else {
        return Ok(());
    };
    if !rules.enabled || rules.routes.is_empty() {
        return Ok(());
    }
    let tunnel = tunnel.to_string();

    for route in &rules.routes {
        let route = route.to_string();
        run(
            ipt,
            &[
                "-t", "filter", "-A", FWD, "-o", interface, "-d", &route, "-j", "ACCEPT",
            ],
        )?;
        run(
            ipt,
            &[
                "-t", "filter", "-A", FWD, "-i", interface, "-m", "conntrack", "--ctstate",
                "ESTABLISHED,RELATED", "-j", "ACCEPT",
            ],
        )?;
        // Anything forwarded from a client keeps its own source address, which
        // the far side will not accept. Traffic this node originated is already
        // inside the tunnel range and is left alone.
        run(
            ipt,
            &[
                "-t", "nat", "-A", NAT, "-o", interface, "!", "-s", &tunnel, "-d", &route, "-j",
                "MASQUERADE",
            ],
        )?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn sync_uplink(_: &str, _: &UplinkRules) -> std::io::Result<()> {
    Ok(())
}

/// Replaces the default route with two halves that cover the same space.
///
/// `0.0.0.0/1` + `128.0.0.0/1` beat the real default on longest-prefix without
/// deleting it, so the node keeps a working path to its own gateway — which is
/// how it stays reachable for management, and how the tunnel's own packets get
/// out. The standard VPN approach, and the only safe way to hand a node's
/// whole uplink to a peer.
#[cfg(target_os = "linux")]
pub fn split_default_halves() -> [&'static str; 2] {
    ["0.0.0.0/1", "128.0.0.0/1"]
}

#[cfg(not(target_os = "linux"))]
pub fn split_default_halves() -> [&'static str; 2] {
    ["0.0.0.0/1", "128.0.0.0/1"]
}

/// Installs (or removes) the gateway chains.
///
/// Everything lives in three chains of our own, flushed and rebuilt each time,
/// so the rules are always exactly what the current settings say and disabling
/// the feature leaves no residue. The FORWARD rules are default-deny: only the
/// tunnel network, only the named destinations, and only replies that belong
/// to a flow the tunnel side started.
#[cfg(target_os = "linux")]
pub fn sync_gateway(interface: &str, rules: &GatewayRules) -> std::io::Result<()> {
    const FWD: &str = "KAONIC_VPN_FORWARD";
    const NAT: &str = "KAONIC_VPN_GW_NAT";

    let Some(ipt) = resolve_iptables() else {
        return Ok(());
    };

    ensure_chain(ipt, "filter", FWD)?;
    ensure_chain(ipt, "nat", NAT)?;
    ensure_jump(ipt, "filter", "FORWARD", "-i", interface, FWD)?;
    ensure_jump(ipt, "filter", "FORWARD", "-o", interface, FWD)?;
    ensure_jump_bare(ipt, "nat", "POSTROUTING", NAT)?;
    run(ipt, &["-t", "filter", "-F", FWD])?;
    run(ipt, &["-t", "nat", "-F", NAT])?;

    let Some(tunnel) = rules.tunnel_network else {
        return Ok(());
    };
    if !rules.enabled || rules.routes.is_empty() {
        return Ok(());
    }

    let tunnel = tunnel.to_string();

    // Replies to a flow the tunnel opened, on every interface a route uses.
    // Stateful, so nothing out there can start a conversation inward.
    let mut seen: Vec<&str> = Vec::new();
    for (_, egress) in &rules.routes {
        if seen.contains(&egress.as_str()) {
            continue;
        }
        seen.push(egress);
        run(
            ipt,
            &[
                "-t", "filter", "-A", FWD, "-i", egress, "-o", interface, "-m", "conntrack",
                "--ctstate", "ESTABLISHED,RELATED", "-j", "ACCEPT",
            ],
        )?;
    }

    for (route, egress) in &rules.routes {
        let route = route.to_string();
        run(
            ipt,
            &[
                "-t", "filter", "-A", FWD, "-i", interface, "-o", egress, "-s", &tunnel, "-d",
                &route, "-j", "ACCEPT",
            ],
        )?;
        // Source NAT to this node's own address on that interface is what makes
        // the far end reachable without touching its routing table.
        run(
            ipt,
            &[
                "-t", "nat", "-A", NAT, "-s", &tunnel, "-d", &route, "-o", egress, "-j",
                "MASQUERADE",
            ],
        )?;
    }

    // Anything else off the tunnel is refused here rather than falling through
    // to whatever the host's FORWARD policy happens to be.
    run(
        ipt,
        &[
            "-t", "filter", "-A", FWD, "-i", interface, "-j", "REJECT", "--reject-with",
            "icmp-net-unreachable",
        ],
    )?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn sync_gateway(_: &str, _: &GatewayRules) -> std::io::Result<()> {
    Ok(())
}

/// The interface the kernel would use to reach `route`.
///
/// Asking the routing table beats naming an interface in config: on a node
/// with several networks the operator would otherwise have to know which one
/// their TAK LAN is on, and interface names are not stable across reboots or
/// hardware changes. `0.0.0.0/0` resolves to whatever carries the default
/// route, which is what "share my uplink" should mean.
#[cfg(target_os = "linux")]
pub fn egress_for(route: &Ipv4Cidr, exclude: &str) -> Option<String> {
    // A representative address inside the route; for a default route that is
    // just some public address the kernel will resolve via the uplink.
    let probe = if route.network_length() == 0 {
        Ipv4Addr::new(1, 1, 1, 1)
    } else {
        let base = u32::from(route.first_address());
        Ipv4Addr::from(if route.network_length() < 32 { base + 1 } else { base })
    };
    let output = std::process::Command::new("ip")
        .args(["-4", "route", "get", &probe.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut words = text.split_whitespace();
    while let Some(word) = words.next() {
        if word == "dev" {
            let dev = words.next()?;
            // Forwarding the tunnel back into the tunnel is never right; it
            // means the destination is only reachable via another peer.
            return (dev != exclude).then(|| dev.to_string());
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn egress_for(_: &Ipv4Cidr, _: &str) -> Option<String> {
    None
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
            "-t",
            "mangle",
            "-A",
            MSS,
            "-p",
            "tcp",
            "--tcp-flags",
            "SYN,RST",
            "SYN",
            "-j",
            "TCPMSS",
            "--set-mss",
            &mss,
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

/// Like [`ensure_jump_bare`] but places the jump at the head of the chain, for
/// rules that must be considered before any that already exist.
#[cfg(target_os = "linux")]
fn ensure_jump_bare_first(
    cmd: &str,
    table: &str,
    parent: &str,
    target: &str,
) -> std::io::Result<()> {
    if run(cmd, &["-t", table, "-C", parent, "-j", target]).is_ok() {
        return Ok(());
    }
    run(cmd, &["-t", table, "-I", parent, "1", "-j", target])
}

#[cfg(target_os = "linux")]
fn ensure_jump_bare(cmd: &str, table: &str, parent: &str, target: &str) -> std::io::Result<()> {
    if run(cmd, &["-t", table, "-C", parent, "-j", target]).is_ok() {
        return Ok(());
    }
    run(cmd, &["-t", table, "-A", parent, "-j", target])
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
