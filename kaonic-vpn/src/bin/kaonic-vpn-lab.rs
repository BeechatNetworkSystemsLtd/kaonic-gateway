//! A Kaonic VPN node with a TCP link where the radio would be.
//!
//! The VPN itself does not care what carries Reticulum, so swapping the radio
//! for TCP exercises the real runtime — tunnel IP derivation, announces, route
//! import, kernel routes, NETMAP aliasing and gateway forwarding — inside a
//! container, with no hardware and no invented test doubles.
//!
//! ```text
//! kaonic-vpn-lab --name site-a --listen 0.0.0.0:4242 \
//!     --advertise 192.168.5.0/24 --gateway-routes 192.168.5.0/24 --gateway-egress eth0
//! kaonic-vpn-lab --name site-b --connect site-a:4242
//! ```
//!
//! `--name` seeds the identity, so a node keeps its tunnel IP across restarts
//! and the lab is reproducible.

use std::sync::Arc;
use std::time::Duration;

use cidr::Ipv4Cidr;
use kaonic_vpn::{VpnConfig, VpnGatewayConfig, VpnRuntime, VpnUplinkConfig};
use reticulum::iface::tcp_client::TcpClient;
use reticulum::iface::tcp_server::TcpServer;
use reticulum::identity::PrivateIdentity;
use reticulum::transport::{Transport, TransportConfig};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct Args {
    name: String,
    listen: Option<String>,
    connect: Vec<String>,
    network: Ipv4Cidr,
    advertise: Vec<Ipv4Cidr>,
    gateway_routes: Vec<Ipv4Cidr>,
    gateway_egress: Option<String>,
    /// Destination hash of the node to route through, for the client side.
    router: Option<String>,
    /// The lab has no pairing store, so membership has to be opted out of
    /// explicitly — the same switch an operator would have to set knowingly.
    allow_all_peers: bool,
    status_secs: u64,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        name: "lab-node".into(),
        listen: None,
        connect: Vec::new(),
        network: "10.20.0.0/16".parse().expect("default transit network"),
        advertise: Vec::new(),
        gateway_routes: Vec::new(),
        gateway_egress: None,
        router: None,
        allow_all_peers: false,
        status_secs: 10,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--name" => args.name = value()?,
            "--listen" => args.listen = Some(value()?),
            "--connect" => args.connect.push(value()?),
            "--network" => args.network = value()?.parse().map_err(|e| format!("--network: {e}"))?,
            "--advertise" => args
                .advertise
                .push(value()?.parse().map_err(|e| format!("--advertise: {e}"))?),
            "--gateway-routes" => args
                .gateway_routes
                .push(value()?.parse().map_err(|e| format!("--gateway-routes: {e}"))?),
            "--gateway-egress" => args.gateway_egress = Some(value()?),
            "--router" => args.router = Some(value()?),
            "--allow-all-peers" => args.allow_all_peers = true,
            "--status-secs" => args.status_secs = value()?.parse().unwrap_or(10),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> Result<(), String> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = parse_args()?;

    let id = PrivateIdentity::new_from_name(&args.name);
    let cancel = CancellationToken::new();
    let transport = Arc::new(Mutex::new(Transport::new(
        // A lab node is the only path between the two sites, so unlike a
        // plugin it *must* retransmit for announces to cross a relay.
        TransportConfig::new(&args.name, &id).set_retransmit(true),
    )));

    {
        let iface_mgr = transport.lock().await.iface_manager();
        if let Some(listen) = args.listen.as_deref() {
            let server = TcpServer::new(listen.to_string(), iface_mgr.clone());
            iface_mgr.lock().await.spawn(server, TcpServer::spawn);
            log::info!("listening for lab peers on {listen}");
        }
        for peer in &args.connect {
            iface_mgr
                .lock()
                .await
                .spawn(TcpClient::new(peer.clone()), TcpClient::spawn);
            log::info!("connecting to lab peer {peer}");
        }
    }

    let gateway = VpnGatewayConfig {
        enabled: !args.gateway_routes.is_empty(),
        egress_interface: args.gateway_egress.clone(),
        routes: args.gateway_routes.clone(),
    };
    let uplink = VpnUplinkConfig {
        enabled: args.router.is_some(),
        peer: args.router.clone(),
    };
    let vpn = VpnRuntime::start(
        VpnConfig {
            network: args.network,
            allow_all_peers: args.allow_all_peers,
            peers: Vec::new(),
            advertised_routes: args.advertise.clone(),
            announce_freq_secs: 5,
            gateway,
            uplink,
        },
        transport.clone(),
        id,
        cancel.clone(),
    )
    .await
    .map_err(|err| format!("vpn start: {err}"))?;

    let snapshot = vpn.snapshot().await;
    log::info!(
        "{}: destination {} tunnel {} on {}",
        args.name,
        snapshot.destination_hash,
        snapshot.local_tunnel_ip.clone().unwrap_or_default(),
        snapshot.interface_name.clone().unwrap_or_else(|| "none".into())
    );

    // A line per interval is what the test harness waits on, and what a person
    // watching `docker compose logs` reads.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_secs(args.status_secs)) => {}
        }
        let s = vpn.snapshot().await;
        let peers: Vec<String> = s
            .peers
            .iter()
            .map(|p| format!("{}@{}", &p.destination[..8.min(p.destination.len())], p.tunnel_ip.clone().unwrap_or_default()))
            .collect();
        log::info!(
            "status: {} peers [{}] routes {:?} gateway enabled={} active={} egress={:?} {}",
            s.peers.len(),
            peers.join(" "),
            s.remote_routes.iter().map(|r| r.network.clone()).collect::<Vec<_>>(),
            s.gateway.enabled,
            s.gateway.active,
            s.gateway.egress_interface,
            s.gateway.detail.clone().unwrap_or_default(),
        );
        log::info!(
            "uplink: enabled={} active={} routes {:?} default={} {}",
            s.uplink.enabled,
            s.uplink.active,
            s.uplink.routes,
            s.uplink.default_route,
            s.uplink.detail.clone().unwrap_or_default(),
        );
    }
    cancel.cancel();
    Ok(())
}
