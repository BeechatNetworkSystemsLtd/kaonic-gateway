use std::collections::HashMap;
use std::sync::Arc;

use radio_common::{Modulation, RadioConfig};
use reticulum::identity::PrivateIdentity;
use reticulum::transport::{Transport, TransportConfig};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::config::{GatewayConfig, KaonicCtrlConfig};
use kaonic_reticulum::KaonicCtrlInterface;

/// Live-update senders for a single radio module.
pub struct ModuleSenders {
    pub radio_config_tx: mpsc::Sender<RadioConfig>,
    pub modulation_tx:   mpsc::Sender<Modulation>,
}

/// Map of module index → senders for all connected interfaces.
pub type RadioSenders = HashMap<usize, ModuleSenders>;

/// Connect to all kaonic-ctrl interfaces from `ctrl_configs`, attach them to
/// the Reticulum transport, and return the transport + per-module senders.
pub async fn setup_transport(
    id: &PrivateIdentity,
    ctrl_configs: Vec<KaonicCtrlConfig>,
) -> Result<(Arc<Mutex<Transport>>, RadioSenders), Box<dyn std::error::Error + Send + Sync>> {
    let mut config = TransportConfig::new("kaonic-gateway", id, true);
    config.set_retransmit(true);
    let transport = Transport::new(config);

    let mut senders: RadioSenders = HashMap::new();

    for ctrl_config in ctrl_configs {
        log::info!(
            "spawning kaonic-ctrl interface: listen={}, server={}, module={}",
            ctrl_config.listen_addr,
            ctrl_config.server_addr,
            ctrl_config.module,
        );
        let cancel = CancellationToken::new();
        let iface = match KaonicCtrlInterface::connect::<1400, 5>(
            ctrl_config.listen_addr,
            ctrl_config.server_addr,
            ctrl_config.module,
            cancel,
        )
        .await
        {
            Ok(iface) => iface,
            Err(e) => {
                log::error!("kaonic-ctrl connect error for module {}: {e:?}", ctrl_config.module);
                continue;
            }
        };

        let module_senders = ModuleSenders {
            radio_config_tx: iface.radio_config_tx.clone(),
            modulation_tx:   iface.modulation_tx.clone(),
        };

        // Apply saved settings on boot.
        if let Some(radio_config) = ctrl_config.radio_config {
            log::info!("applying saved radio config on boot (module {})", ctrl_config.module);
            let _ = iface.radio_config_tx.send(radio_config).await;
        }
        if let Some(modulation) = ctrl_config.modulation {
            log::info!("applying saved modulation on boot (module {})", ctrl_config.module);
            let _ = iface.modulation_tx.send(modulation).await;
        }

        transport
            .iface_manager()
            .lock()
            .await
            .spawn(iface, KaonicCtrlInterface::spawn);

        senders.insert(ctrl_config.module, module_senders);
    }

    Ok((Arc::new(Mutex::new(transport)), senders))
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

