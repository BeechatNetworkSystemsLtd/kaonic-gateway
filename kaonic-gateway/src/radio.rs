use std::sync::Arc;
use std::time::Duration;

use kaonic_ctrl::protocol::RADIO_FRAME_SIZE;
use kaonic_ctrl::radio::RadioClient;
use kaonic_frame::frame::Frame;
use kaonic_reticulum::channel::{profiles, ChannelId, ChannelTx, Radio, ReticulumChannel};
use kaonic_reticulum::{ErrorObserver, FecSelector, KaonicCtrlInterface, RxObserver, TxObserver};
use radio_common::{
    Accelerator, Antenna, Hertz, Modulation, RadioBand, RadioConfig, RadioConfigBuilder,
};
use reticulum::transport::{TimerConfig, Transport};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub type SharedRadioClient = Arc<Mutex<RadioClient>>;
pub type SharedTxObserver = TxObserver;
pub type SharedErrorObserver = ErrorObserver;
pub type SharedRxObserver = RxObserver;

/// Default baseband acceleration mode for a radio module.
pub fn default_accelerator() -> Accelerator {
    Accelerator::Native
}

/// Default antenna for the 2.4 GHz band (the only band with an antenna switch).
pub fn default_antenna() -> Antenna {
    Antenna::Internal
}

/// Lowest frequency the 2.4 GHz transceiver (RF24) covers.
const BAND_24_MIN_HZ: u64 = 2_400_000_000;

/// Band a module operates in, derived from its tuned frequency.
pub fn band_for_config(radio_config: &RadioConfig) -> RadioBand {
    if radio_config.freq.as_hz() >= BAND_24_MIN_HZ {
        RadioBand::Band24
    } else {
        RadioBand::Band09
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RadioModuleConfig {
    pub radio_config: RadioConfig,
    pub modulation: Modulation,
    /// Baseband acceleration: on-chip (`Native`) or external FPGA (`Hardware`).
    #[serde(default = "default_accelerator")]
    pub accelerator: Accelerator,
    /// Antenna selection for the 2.4 GHz band. The sub-GHz path has no antenna
    /// switch on this board — it is always wired to the external connector — so
    /// this is only applied when the module is tuned to 2.4 GHz.
    #[serde(default = "default_antenna")]
    pub antenna: Antenna,
}

impl RadioModuleConfig {
    /// Band this module currently operates in.
    pub fn band(&self) -> RadioBand {
        band_for_config(&self.radio_config)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HardwareRadioConfig {
    pub module_configs: [RadioModuleConfig; 2],
}

impl Default for HardwareRadioConfig {
    fn default() -> Self {
        Self {
            module_configs: [
                RadioModuleConfig {
                    radio_config: RadioConfigBuilder::new()
                        .freq(Hertz::new(869_535_000))
                        .channel_spacing(Hertz::new(200_000))
                        .channel(3)
                        .build(),
                    modulation: Modulation::Ofdm(
                        radio_common::modulation::OfdmModulation::default(),
                    ),
                    accelerator: default_accelerator(),
                    antenna: default_antenna(),
                },
                RadioModuleConfig {
                    radio_config: RadioConfigBuilder::new()
                        .freq(Hertz::new(869_535_000))
                        .channel_spacing(Hertz::new(200_000))
                        .channel(11)
                        .build(),
                    modulation: Modulation::Ofdm(
                        radio_common::modulation::OfdmModulation::default(),
                    ),
                    accelerator: default_accelerator(),
                    antenna: default_antenna(),
                },
            ],
        }
    }
}

/// Connect to the kaonic-ctrl daemon. One connection serves all hardware modules.
pub async fn connect_radio_client(
    listen_addr: std::net::SocketAddr,
    server_addr: std::net::SocketAddr,
) -> Result<SharedRadioClient, kaonic_ctrl::error::ControllerError> {
    KaonicCtrlInterface::connect_client::<1400, 5>(
        listen_addr,
        server_addr,
        CancellationToken::new(),
    )
    .await
}

/// Push one module's full configuration to the hardware. Each step is applied
/// independently; the first failure is returned after the rest were attempted.
pub async fn apply_module_config(
    radio_client: &SharedRadioClient,
    module: usize,
    cfg: &RadioModuleConfig,
) -> Result<(), String> {
    let mut first_error = None;
    let mut client = radio_client.lock().await;
    if let Err(e) = client
        .set_radio_config(module, cfg.radio_config.clone())
        .await
    {
        log::error!("radio config error for module {module}: {e:?}");
        first_error.get_or_insert(format!("radio config: {e:?}"));
    }
    if let Err(e) = client.set_modulation(module, cfg.modulation.clone()).await {
        log::error!("modulation error for module {module}: {e:?}");
        first_error.get_or_insert(format!("modulation: {e:?}"));
    }
    if let Err(e) = client.set_accelerator(module, cfg.accelerator).await {
        log::error!("accelerator error for module {module}: {e:?}");
        first_error.get_or_insert(format!("accelerator: {e:?}"));
    }
    // Only the 2.4 GHz front-end has an antenna switch; sub-GHz is hard-wired
    // to the external connector, so there is nothing to select for Band09.
    if cfg.band() == RadioBand::Band24 {
        if let Err(e) = client
            .set_antenna(module, RadioBand::Band24, cfg.antenna)
            .await
        {
            log::error!("antenna error for module {module}: {e:?}");
            first_error.get_or_insert(format!("antenna: {e:?}"));
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Pushes every module's saved configuration at start-up, and keeps trying
/// until it sticks.
///
/// The daemon answers its first requests slowly while it brings the radios
/// up, and a single timeout here used to leave a module on the daemon's
/// built-in defaults — a different frequency and power than the one the
/// operator saved — until the next gateway restart, with nothing but a
/// warning to show for it. So each module is retried a few times with a
/// short pause, and a module that still fails gets one more full attempt
/// later, from a task, so start-up itself is not held back.
pub async fn apply_boot_configs(radio_client: &SharedRadioClient, radio: &HardwareRadioConfig) {
    const ATTEMPTS: usize = 3;
    const PAUSE: Duration = Duration::from_millis(500);
    const LATER: Duration = Duration::from_secs(5);

    // A cheap round trip first, so the real requests are not the ones that
    // eat the daemon's start-up delay.
    for _ in 0..ATTEMPTS {
        if radio_client.lock().await.ping().await.is_ok() {
            break;
        }
        tokio::time::sleep(PAUSE).await;
    }

    let mut failed = Vec::new();
    for (module, cfg) in radio.module_configs.iter().enumerate() {
        log::info!("applying saved radio config on boot (module {module})");
        let mut last_err = None;
        for attempt in 1..=ATTEMPTS {
            match apply_module_config(radio_client, module, cfg).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(err) => {
                    log::warn!("boot radio apply error for module {module} (attempt {attempt}/{ATTEMPTS}): {err}");
                    last_err = Some(err);
                    tokio::time::sleep(PAUSE).await;
                }
            }
        }
        if last_err.is_some() {
            failed.push((module, cfg.clone()));
        }
    }

    if failed.is_empty() {
        return;
    }

    let radio_client = radio_client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(LATER).await;
        for (module, cfg) in failed {
            match apply_module_config(&radio_client, module, &cfg).await {
                Ok(()) => log::info!("saved radio config applied late for module {module}"),
                Err(err) => log::error!(
                    "module {module} is running on the daemon's default radio config, not the saved one: {err}"
                ),
            }
        }
    });
}

/// Apply saved per-module hardware settings and wire `rns_module` into the transport.
pub async fn attach_radio_interface(
    transport: &Arc<Mutex<Transport>>,
    radio_client: SharedRadioClient,
    radio: &HardwareRadioConfig,
    rns_module: usize,
    tx_observer: Option<SharedTxObserver>,
    error_observer: Option<SharedErrorObserver>,
    rx_observer: Option<SharedRxObserver>,
    fec: Arc<FecSelector>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    apply_boot_configs(&radio_client, radio).await;

    let mut iface = KaonicCtrlInterface::new(radio_client, rns_module, tx_observer, error_observer)
        .with_fec_selector(fec);
    if let Some(rx_observer) = rx_observer {
        iface = iface.with_rx_observer(rx_observer);
    }
    let iface_mgr = transport.lock().await.iface_manager();
    iface_mgr
        .lock()
        .await
        .spawn(iface, KaonicCtrlInterface::spawn);

    Ok(())
}

/// Attaches Reticulum to the radio daemon through a channel: framing, coding
/// and scheduling run in the daemon, and this process only exchanges
/// packets. Returns the channel's transmit half so the remote runtime can
/// steer per-peer coding through it.
/// Which channel the gateway's Reticulum traffic rides.
///
/// Channel 0 is the compatibility channel: its payloads must stay
/// byte-identical to what nodes predating channels expect, so the daemon
/// deliberately does not bundle on it. That costs a whole radio frame for every
/// small packet — a 60-byte TCP ACK occupies the same slot as a 1400-byte data
/// packet, which is most of why bulk transfer over the tunnel is slow.
///
/// Setting `KAONIC_RADIO_CHANNEL` to a name moves the gateway onto a named
/// channel, where the Bulk profile's bundler packs small packets together.
/// Every node in the mesh must use the same value: this is a wire change, and
/// a node left on channel 0 will not hear one that moved.
/// Reticulum link timers scaled to what the radio can actually carry.
///
/// The defaults were chosen for OFDM, where a full frame is about 7 ms of air.
/// At MR-O-QPSK 100 kchip/s rate mode 0 the same frame is 2.6 seconds, so a
/// link handshake — several frames each way — takes tens of seconds. Fixed
/// timers expire mid-handshake and the link never forms, even though the radio
/// is delivering frames perfectly well.
///
/// Each timer is the larger of its existing value and a multiple of the frame
/// airtime, so **fast modulations keep exactly the values they have today** and
/// only slow ones stretch. The multipliers are frame counts: roughly how many
/// frame times that stage of the link needs to make progress.
pub fn link_timers(modulation: &Modulation) -> TimerConfig {
    let frame = Duration::from_micros(modulation.airtime_micros(RADIO_FRAME_SIZE));
    let at_least = |floor: Duration, frames: u32| floor.max(frame * frames);

    log::info!(
        "reticulum link timers for {} ({} bit/s, {} ms per frame)",
        modulation,
        modulation.data_rate_bps(),
        frame.as_millis()
    );

    TimerConfig {
        in_link_stale: at_least(Duration::from_secs(30), 20),
        in_link_close: at_least(Duration::from_secs(15), 10),
        out_link_restart: at_least(Duration::from_secs(45), 30),
        out_link_stale: at_least(Duration::from_secs(30), 20),
        out_link_close: at_least(Duration::from_secs(15), 10),
        out_link_repeat: at_least(Duration::from_secs(10), 8),
        out_link_keep: at_least(Duration::from_secs(5), 4),
        ..TimerConfig::default()
    }
}

pub fn gateway_channel_id() -> ChannelId {
    match std::env::var("KAONIC_RADIO_CHANNEL") {
        Ok(name) if !name.trim().is_empty() && name.trim() != "0" => {
            let id = ChannelId::of(name.trim());
            log::info!(
                "gateway reticulum on channel {} ({:?}) — bundling enabled",
                id.raw(),
                name.trim()
            );
            id
        }
        _ => ChannelId::DEFAULT,
    }
}

pub async fn attach_radio_channel(
    transport: &Arc<Mutex<Transport>>,
    radio_client: SharedRadioClient,
    radio: &HardwareRadioConfig,
    rns_module: usize,
    tx_observer: Option<SharedTxObserver>,
    rx_observer: Option<SharedRxObserver>,
    fec: Arc<FecSelector>,
) -> Result<ChannelTx, Box<dyn std::error::Error + Send + Sync>> {
    apply_boot_configs(&radio_client, radio).await;

    let channel = Radio::from_shared(radio_client)
        .channel(gateway_channel_id())
        .await
        .modules([rns_module])
        .profile(profiles::Bulk {
            class: fec.default_class(),
        })
        .build()
        .await?;
    let tx = channel.tx().clone();

    let mut iface = ReticulumChannel::new(channel);
    // The local selector keeps the link picture the UI shows (last RSSI per
    // peer); the code itself is chosen in the daemon.
    let local_fec = fec.clone();
    iface = iface.with_observer(Arc::new(move |packet, rssi| {
        local_fec.observe_rssi(packet.destination, rssi);
        if let Some(observer) = rx_observer.as_ref() {
            observer(rns_module, rssi, packet);
        }
    }));
    if let Some(tx_observer) = tx_observer {
        iface = iface.with_tx_observer(Arc::new(move |bytes| tx_observer(rns_module, bytes)));
    }

    let iface_mgr = transport.lock().await.iface_manager();
    iface_mgr
        .lock()
        .await
        .spawn(iface, ReticulumChannel::spawn);

    Ok(tx)
}

pub async fn transmit_test_frame(
    radio_client: Option<SharedRadioClient>,
    tx_observer: Option<SharedTxObserver>,
    module: usize,
    payload: &[u8],
) -> Result<(), String> {
    if module > 1 {
        return Err(format!("radio module {module} not found"));
    }

    if payload.is_empty() {
        return Err("message is required".into());
    }

    let max_len = RADIO_FRAME_SIZE.min(2047);
    if payload.len() > max_len {
        return Err(format!("message exceeds {max_len} bytes"));
    }

    let Some(radio_client) = radio_client else {
        return Err("radio backend unavailable".into());
    };

    let mut frame = Frame::<RADIO_FRAME_SIZE>::new();
    frame.copy_from_slice(payload);

    let transmit_result = radio_client
        .lock()
        .await
        .transmit(module, &frame)
        .await
        .map_err(|err| format!("transmit: {err:?}"));

    if transmit_result.is_ok() {
        if let Some(observer) = tx_observer {
            observer(module, payload);
        }
    }

    transmit_result
}
