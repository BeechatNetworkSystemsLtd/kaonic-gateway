use std::sync::Arc;

use reticulum::identity::PrivateIdentity;
use reticulum::transport::Transport;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Run the VPN client on an already-configured transport.
/// `network` must be a valid IPv4 CIDR string (e.g. `"10.20.0.0/16"`).
/// Exits cleanly when `cancel` is triggered.
pub async fn run_vpn(
    transport: Arc<Mutex<Transport>>,
    network: &str,
    peers: Vec<String>,
    announce_freq_secs: u32,
    id: PrivateIdentity,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let network = network
        .parse::<cidr_v3::Ipv4Cidr>()
        .map_err(|e| format!("invalid network cidr '{network}': {e}"))?;

    let vpn_config = rns_vpn::Config {
        network,
        peers,
        announce_freq_secs,
        allow_all: true,
    };

    let client = rns_vpn::Client::run(vpn_config, transport, id)
        .await
        .map_err(|e| format!("VPN client error: {e:?}"))?;

    tokio::select! {
        _ = client.await_finished() => {},
        _ = cancel.cancelled()      => log::info!("run_vpn: cancellation requested"),
    }
    Ok(())
}
