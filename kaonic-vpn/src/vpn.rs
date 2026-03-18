use std::sync::Arc;

use reticulum::identity::PrivateIdentity;
use reticulum::transport::{Transport, TransportConfig};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::{GatewayConfig, KaonicCtrlConfig};
use kaonic_reticulum::KaonicCtrlInterface;

/// Connect to the kaonic-ctrl daemon, attach the Reticulum interface, and
/// return the ready transport wrapped in an `Arc<Mutex<>>` for sharing.
pub async fn setup_transport(
    id: &PrivateIdentity,
    ctrl_config: KaonicCtrlConfig,
) -> Result<Arc<Mutex<Transport>>, Box<dyn std::error::Error + Send + Sync>> {
    let transport = Transport::new(TransportConfig::new("kaonic-gateway", id, true));

    log::info!(
        "spawning kaonic-ctrl interface: listen={}, server={}",
        ctrl_config.listen_addr,
        ctrl_config.server_addr
    );
    let cancel = CancellationToken::new();
    let iface = KaonicCtrlInterface::connect::<1400, 5>(
        ctrl_config.listen_addr,
        ctrl_config.server_addr,
        ctrl_config.module,
        cancel,
    )
    .await
    .map_err(|e| format!("kaonic-ctrl connect error: {e:?}"))?;

    // Queue initial radio settings into the channel before spawning so the
    // interface picks them up on startup and programs the hardware immediately.
    if let Some(radio_config) = ctrl_config.radio_config {
        log::info!("applying saved radio config on boot (module {})", ctrl_config.module);
        let _ = iface.radio_config_tx.try_send(radio_config);
    }
    if let Some(modulation) = ctrl_config.modulation {
        log::info!("applying saved modulation on boot (module {})", ctrl_config.module);
        let _ = iface.modulation_tx.try_send(modulation);
    }

    transport
        .iface_manager()
        .lock()
        .await
        .spawn(iface, KaonicCtrlInterface::spawn);

    Ok(Arc::new(Mutex::new(transport)))
}

/// Start the VPN client on an already-configured transport.
pub async fn run_vpn(
    transport: Arc<Mutex<Transport>>,
    config: GatewayConfig,
    id: PrivateIdentity,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let network_str = config.network.to_string();
    let network = network_str
        .parse::<cidr_v3::Ipv4Cidr>()
        .map_err(|e| format!("invalid network cidr: {e}"))?;

    let vpn_config = rns_vpn::Config {
        network,
        peers: config.peers,
        announce_freq_secs: config.announce_freq_secs,
        allow_all: true,
    };

    let client = rns_vpn::Client::run(vpn_config, transport, id)
        .await
        .map_err(|e| format!("VPN client error: {e:?}"))?;
    client.await_finished().await;
    Ok(())
}

