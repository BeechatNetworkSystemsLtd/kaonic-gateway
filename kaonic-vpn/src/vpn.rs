use std::sync::Arc;

use reticulum::identity::PrivateIdentity;
use reticulum::transport::{Transport, TransportConfig};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::{FrameRecord, GatewayConfig, KaonicCtrlConfig};
use crate::interface::KaonicCtrlInterface;

/// Set up the Reticulum transport, spawn the kaonic-ctrl interface, and run the VPN client.
pub async fn run(
    config: GatewayConfig,
    id: PrivateIdentity,
    ctrl_config: KaonicCtrlConfig,
    frame_tx: Option<mpsc::UnboundedSender<FrameRecord>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let transport = Transport::new(TransportConfig::new("kaonic-gateway", &id, true));

    log::info!(
        "spawning kaonic-ctrl interface: listen={}, server={}",
        ctrl_config.listen_addr,
        ctrl_config.server_addr
    );
    let cancel = CancellationToken::new();
    let mut iface = KaonicCtrlInterface::connect(&ctrl_config, cancel).await
        .map_err(|e| format!("kaonic-ctrl connect error: {e:?}"))?;
    iface.frame_tx = frame_tx;
    transport
        .iface_manager()
        .lock()
        .await
        .spawn(iface, KaonicCtrlInterface::spawn);

    let network_str = config.network.to_string();
    let network = network_str.parse::<cidr_v3::Ipv4Cidr>()
        .map_err(|e| format!("invalid network cidr: {e}"))?;

    let vpn_config = rns_vpn::Config {
        network,
        peers: config.peers,
        announce_freq_secs: config.announce_freq_secs,
        allow_all: true,
    };

    let transport = Arc::new(tokio::sync::Mutex::new(transport));
    let client = rns_vpn::Client::run(vpn_config, transport, id).await
        .map_err(|e| format!("VPN client error: {e:?}"))?;
    client.await_finished().await;
    Ok(())
}

