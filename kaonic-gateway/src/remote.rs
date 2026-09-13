//! Gateway integration for Kaonic Remote: trust persistence in the settings
//! database and execution of authorized remote commands against this node
//! (radio hardware, plugin installer, systemd).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kaonic_remote::handler::{
    BoxFuture, Command, CommandHandler, LinkClass, LinkPolicy, Reply, RemoteError,
};
use kaonic_reticulum::{FecSelector, TrafficClass};
use kaonic_remote::protocol as proto;
use kaonic_remote::protocol::{
    blob, mod_kind, plugin_action, InfoBody, PluginInfoWire, RadioConfigWire, ShellResultBody,
    PROTOCOL_VERSION, SHELL_MAX_OUTPUT, SHELL_SLICE,
};
use reticulum::destination::DestinationName;
use reticulum::hash::AddressHash;
use kaonic_remote::trust::{PairedNode, PairingDirection, PairingRecord, TrustStore};
use kaonic_remote::{LocalInfo, RemoteConfig, RemoteRuntime};
use radio_common::modulation::{
    OfdmBandwidthOption, OfdmMcs, OfdmModulation, QpskChipFrequency, QpskModulation,
    QpskRateMode,
};
use radio_common::frequency::BandwidthFilter;
use radio_common::{Accelerator, Antenna, Hertz, Modulation, RadioConfigBuilder};
use reticulum::identity::PrivateIdentity;
use reticulum::transport::Transport;
use tokio_util::sync::CancellationToken;

use crate::radio::{apply_module_config, RadioModuleConfig, SharedRadioClient};
use crate::settings::Settings;
use crate::state::SharedSettings;
use crate::system_metrics::is_gateway_service_unit;

pub type SharedRemote = Arc<RemoteRuntime>;

const INSTALLER_BASE: &str = "http://127.0.0.1:8682";
const SETTING_ANNOUNCE_SECS: &str = "remote_announce_secs";
const SETTING_ACCEPT_PAIRING: &str = "remote_accept_pairing";
pub const SETTING_AUTO_LINK: &str = "remote_auto_link";
pub const SETTING_CHUNK_GAP_MS: &str = "remote_chunk_gap_ms";
pub const SETTING_BULK_PARITY: &str = "remote_bulk_parity";
/// Remote shell runs unsandboxed as root, so the device owner must opt in.
pub const SETTING_SHELL_ENABLED: &str = "remote_shell_enabled";
/// Feature switches read at startup (see `load_feature_flags`).
pub const SETTING_VPN_ENABLED: &str = "vpn_enabled";
pub const SETTING_REMOTE_ENABLED: &str = "remote_enabled";
const DEFAULT_ANNOUNCE_SECS: u32 = 20;
/// Default chunk pacing: just above the receiver's LDPC decode time per
/// frame on the STM32MP1 (measured ~18 ms for an 806 B chunk).
pub const DEFAULT_CHUNK_GAP_MS: u32 = 12;
const SPOOL_DIR: &str = "/var/tmp/kaonic-remote";

// ── Link policy (runtime FEC selection) ───────────────────────────────────────

/// Maps a session's traffic class onto the radio interface's FEC selector.
/// Peers that did not advertise code selection always get the
/// wire-compatible robust code, whatever the class.
pub struct GatewayLinkPolicy {
    pub fec: Arc<FecSelector>,
    /// When Reticulum runs over a channel, the code is chosen in the daemon:
    /// class changes are forwarded there as well.
    pub channel: Option<kaonic_reticulum::channel::ChannelTx>,
}

impl LinkPolicy for GatewayLinkPolicy {
    fn set_class(&self, destination: reticulum::hash::AddressHash, class: LinkClass, capable: bool) {
        let traffic = if !capable {
            TrafficClass::Robust
        } else {
            match class {
                LinkClass::Control => TrafficClass::Robust,
                LinkClass::Bulk => TrafficClass::Auto,
                LinkClass::Media => TrafficClass::Fast,
            }
        };
        self.fec.set_class(destination, traffic);
        if let Some(channel) = self.channel.as_ref() {
            channel.observe_peer(kaonic_reticulum::channel::peer_key(&destination), None, Some(traffic));
        }
    }

    fn clear(&self, destination: &reticulum::hash::AddressHash) {
        self.fec.clear_class(destination);
        if let Some(channel) = self.channel.as_ref() {
            channel.observe_peer(
                kaonic_reticulum::channel::peer_key(destination),
                None,
                Some(self.fec.default_class()),
            );
        }
    }

    fn link_rssi(&self, destination: &reticulum::hash::AddressHash) -> Option<i8> {
        self.fec.last_rssi(destination)
    }
}

pub fn traffic_class_name(class: TrafficClass) -> &'static str {
    match class {
        TrafficClass::Robust => "robust",
        TrafficClass::Auto => "auto",
        TrafficClass::Fast => "fast",
        TrafficClass::Fastest => "fastest",
        TrafficClass::Fixed(code) => code.name(),
    }
}

pub fn parse_traffic_class(value: &str) -> Option<TrafficClass> {
    use kaonic_reticulum::FecCode;
    Some(match value {
        "robust" => TrafficClass::Robust,
        "auto" => TrafficClass::Auto,
        "fast" => TrafficClass::Fast,
        "fastest" => TrafficClass::Fastest,
        "tm2048" => TrafficClass::Fixed(FecCode::Tm2048),
        "tm1536" => TrafficClass::Fixed(FecCode::Tm1536),
        "tm1280" => TrafficClass::Fixed(FecCode::Tm1280),
        "tc512" => TrafficClass::Fixed(FecCode::Tc512),
        "none" => TrafficClass::Fixed(FecCode::None),
        _ => return None,
    })
}

/// Fill in the parts of a snapshot only the gateway knows: the operator's
/// own labels (local database) and the VPN peer that shares each node's
/// identity, so the map can show which 192.168.x.0/24 space a node exports.
pub async fn enrich_snapshot(state: &crate::state::AppState, snapshot: &mut kaonic_remote::RemoteSnapshot) {
    let tags: std::collections::HashMap<String, String> = state
        .settings
        .lock()
        .ok()
        .and_then(|s| s.load_remote_tags().ok())
        .unwrap_or_default()
        .into_iter()
        .collect();

    let vpn = match state.vpn.as_ref() {
        Some(vpn) => Some(vpn.snapshot().await),
        None => None,
    };
    let remote = state.remote.clone();

    for node in &mut snapshot.nodes {
        if let Some(tag) = tags.get(&node.identity_hash) {
            node.tag = tag.clone();
        }
        let (Some(vpn), Some(remote)) = (vpn.as_ref(), remote.as_ref()) else {
            continue;
        };
        // The VPN and remote destinations are different aspects of the same
        // identity, so derive the peer's VPN address from its identity.
        let Some(identity) = reticulum::hash::AddressHash::new_from_hex_string(&node.identity_hash)
            .ok()
            .and_then(|hash| remote.sibling_destination(&hash, "kaonic", "vpn"))
        else {
            continue;
        };
        let identity = identity.to_hex_string();
        if let Some(peer) = vpn.peers.iter().find(|peer| peer.destination == identity) {
            node.vpn_tunnel_ip = peer.tunnel_ip.clone().unwrap_or_default();
            node.vpn_routes = peer.announced_routes.clone();
        }
    }
}

// ── Trust store ───────────────────────────────────────────────────────────────

pub struct SqliteTrustStore {
    settings: SharedSettings,
}

impl SqliteTrustStore {
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }

    fn with<T>(&self, f: impl FnOnce(&Settings) -> rusqlite::Result<T>) -> Result<T, String> {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        f(&settings).map_err(|err| err.to_string())
    }
}

impl TrustStore for SqliteTrustStore {
    fn load(&self) -> Vec<PairedNode> {
        self.with(|s| s.load_remote_paired()).unwrap_or_else(|err| {
            log::error!("remote: failed to load paired nodes: {err}");
            Vec::new()
        })
    }

    fn save(&self, node: &PairedNode) -> Result<(), String> {
        self.with(|s| s.save_remote_paired(node))
    }

    fn remove(&self, identity_hash: &str) -> Result<(), String> {
        self.with(|s| s.remove_remote_paired(identity_hash))
    }

    fn load_requests(&self) -> Vec<PairingRecord> {
        self.with(|s| s.load_remote_requests()).unwrap_or_else(|err| {
            log::error!("remote: failed to load pairing requests: {err}");
            Vec::new()
        })
    }

    fn save_request(&self, record: &PairingRecord) -> Result<(), String> {
        self.with(|s| s.save_remote_request(record))
    }

    fn remove_request(&self, identity_hash: &str, direction: PairingDirection) -> Result<(), String> {
        self.with(|s| s.remove_remote_request(identity_hash, direction))
    }
}

// ── Configuration ─────────────────────────────────────────────────────────────

pub fn load_remote_config(settings: &SharedSettings) -> RemoteConfig {
    let settings = settings.lock().unwrap_or_else(|e| e.into_inner());
    let announce_secs = settings
        .get_setting(SETTING_ANNOUNCE_SECS)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_ANNOUNCE_SECS)
        .clamp(5, 255);
    let accept_pairing = settings
        .get_setting(SETTING_ACCEPT_PAIRING)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(true);
    let auto_link = settings
        .get_setting(SETTING_AUTO_LINK)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(true);
    let chunk_gap_ms = settings
        .get_setting(SETTING_CHUNK_GAP_MS)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_CHUNK_GAP_MS)
        .clamp(0, 500);
    RemoteConfig {
        announce_secs,
        accept_pairing,
        auto_link,
        spool_dir: PathBuf::from(SPOOL_DIR),
        chunk_gap: std::time::Duration::from_millis(u64::from(chunk_gap_ms)),
        ..RemoteConfig::default()
    }
}

/// Feature switches. VPN/remote need a restart to take effect; the shell
/// switch is live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureFlags {
    pub vpn_enabled: bool,
    pub remote_enabled: bool,
    pub shell_enabled: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            vpn_enabled: true,
            remote_enabled: true,
            // Unsandboxed root shell: opt-in only.
            shell_enabled: false,
        }
    }
}

pub fn load_feature_flags(settings: &SharedSettings) -> FeatureFlags {
    let settings = settings.lock().unwrap_or_else(|e| e.into_inner());
    let flag = |key: &str, default: bool| {
        settings
            .get_setting(key)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<bool>().ok())
            .unwrap_or(default)
    };
    let defaults = FeatureFlags::default();
    FeatureFlags {
        vpn_enabled: flag(SETTING_VPN_ENABLED, defaults.vpn_enabled),
        remote_enabled: flag(SETTING_REMOTE_ENABLED, defaults.remote_enabled),
        shell_enabled: flag(SETTING_SHELL_ENABLED, defaults.shell_enabled),
    }
}

/// Start the remote runtime for this gateway.
pub async fn start_remote(
    settings: SharedSettings,
    identity: &PrivateIdentity,
    transport: Arc<tokio::sync::Mutex<Transport>>,
    radio_client: Option<SharedRadioClient>,
    codename: String,
    serial: String,
    cancel: CancellationToken,
) -> SharedRemote {
    let config = load_remote_config(&settings);
    let handler = Arc::new(GatewayCommandHandler {
        settings: settings.clone(),
        radio_client,
        serial: serial.clone(),
        codename: codename.clone(),
        started: Instant::now(),
        shell_output: parking_lot::Mutex::new(String::new()),
        shell_enabled: AtomicBool::new(load_feature_flags(&settings).shell_enabled),
    });
    let bulk_parity = settings
        .lock()
        .ok()
        .and_then(|s| s.get_setting(SETTING_BULK_PARITY).ok().flatten())
        .and_then(|v| v.parse::<u32>().ok());
    let services_digest = local_services_digest(&settings);
    let store = Arc::new(SqliteTrustStore::new(settings));
    let shell = handler.clone();
    let runtime = RemoteRuntime::start(
        config,
        identity.clone(),
        LocalInfo {
            codename,
            gateway_version: env!("CARGO_PKG_VERSION").to_string(),
            serial,
            services_digest,
        },
        transport,
        handler,
        store,
        cancel,
    )
    .await;
    if let Some(m) = bulk_parity {
        runtime.set_bulk_parity(m);
    }
    SHELL_HANDLER.set(shell).ok();
    runtime
}

/// The running handler, so the HTTP layer can flip the shell switch live.
static SHELL_HANDLER: std::sync::OnceLock<Arc<GatewayCommandHandler>> = std::sync::OnceLock::new();

pub fn set_shell_enabled(enabled: bool) {
    if let Some(handler) = SHELL_HANDLER.get() {
        handler.shell_enabled.store(enabled, Ordering::Relaxed);
    }
}

pub fn shell_enabled() -> Option<bool> {
    SHELL_HANDLER
        .get()
        .map(|handler| handler.shell_enabled.load(Ordering::Relaxed))
}

// ── Command handler ───────────────────────────────────────────────────────────

pub struct GatewayCommandHandler {
    settings: SharedSettings,
    radio_client: Option<SharedRadioClient>,
    serial: String,
    codename: String,
    started: Instant,
    /// Output of the last shell command, kept for paging.
    shell_output: parking_lot::Mutex<String>,
    /// Device-side switch for the (unsandboxed) remote shell.
    shell_enabled: AtomicBool,
}

impl CommandHandler for GatewayCommandHandler {
    fn handle(&self, command: Command) -> BoxFuture<'_, Result<Reply, RemoteError>> {
        Box::pin(async move {
            log::info!("remote: executing {:?}", command_label(&command));
            match command {
                Command::Ping => Ok(Reply::Empty),
                Command::Info => Ok(Reply::Info(self.info())),
                Command::Services => Ok(Reply::Services(self.services())),
                Command::RadioGet { module } => self.radio_get(module),
                Command::RadioSet(config) => self.radio_set(config).await,
                Command::PluginList => plugin_list().await,
                Command::PluginAction { id, action } => plugin_action_call(&id, action).await,
                Command::SystemReboot => {
                    request_system_reboot()
                        .await
                        .map(Reply::Detail)
                        .map_err(RemoteError::error)
                }
                Command::ServiceRestart { unit } => {
                    if !is_gateway_service_unit(&unit) {
                        return Err(RemoteError::bad_request("unsupported service"));
                    }
                    request_service_restart(&unit)
                        .await
                        .map(Reply::Detail)
                        .map_err(RemoteError::error)
                }
                Command::ShellExec {
                    command,
                    timeout_secs,
                } => self.shell_exec(command, timeout_secs).await,
                Command::ShellFetch { offset } => {
                    if self.shell_enabled.load(Ordering::Relaxed) {
                        Ok(Reply::Shell(self.shell_page(offset)))
                    } else {
                        Err(RemoteError::new(
                            kaonic_remote::protocol::status::UNAUTHORIZED,
                            "remote shell is disabled on this node",
                        ))
                    }
                }
                Command::ApplyBlob {
                    purpose,
                    name,
                    path,
                } => apply_blob(purpose, &name, path).await,
            }
        })
    }
}

impl GatewayCommandHandler {
    /// The plugin destinations this node serves, as its plugins registered
    /// them on the local API. Sending it to a paired peer is what lets that
    /// peer's plugins know which links they may create: the pairing already
    /// authenticated the node, and this binds its plugin identities to it.
    fn services(&self) -> proto::ServicesBody {
        let entries = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .load_local_services()
            .unwrap_or_default();
        proto::ServicesBody {
            services: entries
                .into_iter()
                .take(proto::MAX_SERVICES)
                .map(|(name, destination)| proto::ServiceEntry { name, destination })
                .collect(),
        }
    }
    fn info(&self) -> InfoBody {
        let codename = self
            .settings
            .lock()
            .ok()
            .and_then(|s| s.load_or_create_codename().ok())
            .unwrap_or_else(|| self.codename.clone());
        InfoBody {
            codename,
            serial: self.serial.clone(),
            gateway_version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_secs: self.started.elapsed().as_secs(),
            protocol: PROTOCOL_VERSION,
            radio_modules: 2,
        }
    }

    fn radio_get(&self, module: u8) -> Result<Reply, RemoteError> {
        let config = {
            let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            settings.load_config().map_err(|err| RemoteError::error(err.to_string()))?
        };
        let cfg = config
            .radio
            .module_configs
            .get(module as usize)
            .ok_or_else(|| RemoteError::not_found(format!("radio module {module}")))?;
        Ok(Reply::Radio(to_wire(module, cfg)))
    }

    async fn radio_set(&self, wire: RadioConfigWire) -> Result<Reply, RemoteError> {
        let module = wire.module as usize;
        if module > 1 {
            return Err(RemoteError::not_found(format!("radio module {module}")));
        }
        let cfg = from_wire(&wire)?;
        {
            let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            settings
                .save_module_config(module, &cfg)
                .map_err(|err| RemoteError::error(err.to_string()))?;
        }
        match self.radio_client.as_ref() {
            Some(client) => {
                apply_module_config(client, module, &cfg)
                    .await
                    .map_err(|err| RemoteError::error(format!("saved, but apply failed: {err}")))?;
                Ok(Reply::Detail(format!("Radio {} updated", module_label(module))))
            }
            None => Ok(Reply::Detail("Saved (no radio backend)".into())),
        }
    }
}

impl GatewayCommandHandler {
    /// Run `command` under `sh -c` with a timeout, buffering combined output.
    async fn shell_exec(&self, command: String, timeout_secs: u32) -> Result<Reply, RemoteError> {
        if !self.shell_enabled.load(Ordering::Relaxed) {
            return Err(RemoteError::new(
                kaonic_remote::protocol::status::UNAUTHORIZED,
                "remote shell is disabled on this node",
            ));
        }
        let command = command.trim().to_string();
        if command.is_empty() {
            return Err(RemoteError::bad_request("empty command"));
        }
        if command.len() > 4096 {
            return Err(RemoteError::bad_request("command too long"));
        }
        let timeout = Duration::from_secs(u64::from(timeout_secs.clamp(1, 300)));
        log::info!("remote shell: {command}");
        let started = Instant::now();
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| RemoteError::error(format!("spawn: {err}")))?;

        let (code, mut output) = match tokio::time::timeout(timeout, child.wait_with_output()).await
        {
            Ok(Ok(out)) => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                let err = String::from_utf8_lossy(&out.stderr);
                if !err.is_empty() {
                    text.push_str(&err);
                }
                (out.status.code().unwrap_or(-1), text)
            }
            Ok(Err(err)) => return Err(RemoteError::error(format!("wait: {err}"))),
            Err(_) => (
                -1,
                format!("(timed out after {}s)\n", timeout.as_secs()),
            ),
        };
        let truncated = output.len() > SHELL_MAX_OUTPUT;
        if truncated {
            // Cut on a char boundary so the string stays valid UTF-8.
            let mut end = SHELL_MAX_OUTPUT;
            while end > 0 && !output.is_char_boundary(end) {
                end -= 1;
            }
            output.truncate(end);
            output.push_str("\n… output truncated …\n");
        }
        let duration_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        let total = output.len() as u32;
        *self.shell_output.lock() = output;
        let mut result = self.shell_page(0);
        result.code = code;
        result.total = total;
        result.truncated = truncated;
        result.duration_ms = duration_ms;
        Ok(Reply::Shell(result))
    }

    /// One radio-frame-sized slice of the buffered output.
    fn shell_page(&self, offset: u32) -> ShellResultBody {
        let output = self.shell_output.lock();
        // Both ends must sit on char boundaries: `offset` comes off the wire.
        let mut start = (offset as usize).min(output.len());
        while start > 0 && !output.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + SHELL_SLICE).min(output.len());
        while end > start && !output.is_char_boundary(end) {
            end -= 1;
        }
        ShellResultBody {
            code: 0,
            total: output.len() as u32,
            offset,
            chunk: output[start..end].to_string(),
            truncated: false,
            duration_ms: 0,
        }
    }
}

fn command_label(command: &Command) -> String {
    match command {
        Command::ApplyBlob { purpose, name, .. } => format!("apply-blob purpose={purpose} name={name}"),
        other => format!("{other:?}"),
    }
}

fn module_label(module: usize) -> &'static str {
    if module == 0 {
        "A"
    } else {
        "B"
    }
}

// ── Radio config conversion ───────────────────────────────────────────────────

pub fn to_wire(module: u8, cfg: &RadioModuleConfig) -> RadioConfigWire {
    let (kind, a, b, tx_power) = match cfg.modulation {
        Modulation::Off => (mod_kind::OFF, 0, 0, 0),
        Modulation::Ofdm(ofdm) => (mod_kind::OFDM, ofdm.mcs as u8, ofdm.opt as u8, ofdm.tx_power),
        Modulation::Qpsk(qpsk) => (
            mod_kind::QPSK,
            qpsk.fchip as u8,
            qpsk.mode as u8,
            qpsk.tx_power,
        ),
        Modulation::Fsk => (mod_kind::FSK, 0, 0, 0),
    };
    RadioConfigWire {
        module,
        freq_hz: cfg.radio_config.freq.as_hz(),
        spacing_hz: cfg.radio_config.channel_spacing.as_hz().min(u32::MAX as u64) as u32,
        channel: cfg.radio_config.channel,
        bw_filter: cfg.radio_config.bandwidth_filter as u8,
        mod_kind: kind,
        mod_a: a,
        mod_b: b,
        tx_power,
        accelerator: match cfg.accelerator {
            Accelerator::Native => 0,
            Accelerator::Hardware => 1,
        },
        antenna: match cfg.antenna {
            Antenna::Internal => 0,
            Antenna::External => 1,
        },
    }
}

pub fn from_wire(wire: &RadioConfigWire) -> Result<RadioModuleConfig, RemoteError> {
    let mcs = |v: u8| -> Result<OfdmMcs, RemoteError> {
        Ok(match v {
            0 => OfdmMcs::BpskC1_2_4x,
            1 => OfdmMcs::BpskC1_2_2x,
            2 => OfdmMcs::QpskC1_2_2x,
            3 => OfdmMcs::QpskC1_2,
            4 => OfdmMcs::QpskC3_4,
            5 => OfdmMcs::QamC1_2,
            6 => OfdmMcs::QamC3_4,
            other => return Err(RemoteError::bad_request(format!("ofdm mcs {other}"))),
        })
    };
    let opt = |v: u8| -> Result<OfdmBandwidthOption, RemoteError> {
        Ok(match v {
            0 => OfdmBandwidthOption::Option1,
            1 => OfdmBandwidthOption::Option2,
            2 => OfdmBandwidthOption::Option3,
            3 => OfdmBandwidthOption::Option4,
            other => return Err(RemoteError::bad_request(format!("ofdm option {other}"))),
        })
    };
    let fchip = |v: u8| -> Result<QpskChipFrequency, RemoteError> {
        Ok(match v {
            0 => QpskChipFrequency::Fchip100,
            1 => QpskChipFrequency::Fchip200,
            2 => QpskChipFrequency::Fchip1000,
            3 => QpskChipFrequency::Fchip2000,
            other => return Err(RemoteError::bad_request(format!("qpsk fchip {other}"))),
        })
    };
    let mode = |v: u8| -> Result<QpskRateMode, RemoteError> {
        Ok(match v {
            0 => QpskRateMode::RateMode0,
            1 => QpskRateMode::RateMode1,
            2 => QpskRateMode::RateMode2,
            3 => QpskRateMode::RateMode3,
            4 => QpskRateMode::RateMode4,
            other => return Err(RemoteError::bad_request(format!("qpsk mode {other}"))),
        })
    };
    let modulation = match wire.mod_kind {
        mod_kind::OFF => Modulation::Off,
        mod_kind::OFDM => Modulation::Ofdm(OfdmModulation {
            mcs: mcs(wire.mod_a)?,
            opt: opt(wire.mod_b)?,
            tx_power: wire.tx_power,
        }),
        mod_kind::QPSK => Modulation::Qpsk(QpskModulation {
            fchip: fchip(wire.mod_a)?,
            mode: mode(wire.mod_b)?,
            tx_power: wire.tx_power,
        }),
        mod_kind::FSK => Modulation::Fsk,
        other => return Err(RemoteError::bad_request(format!("modulation kind {other}"))),
    };
    if wire.freq_hz == 0 {
        return Err(RemoteError::bad_request("frequency must be > 0"));
    }
    Ok(RadioModuleConfig {
        radio_config: RadioConfigBuilder::new()
            .freq(Hertz::new(wire.freq_hz))
            .channel_spacing(Hertz::new(wire.spacing_hz as u64))
            .channel(wire.channel)
            .bandwidth_filter(match wire.bw_filter {
                0 => BandwidthFilter::Narrow,
                1 => BandwidthFilter::Wide,
                other => return Err(RemoteError::bad_request(format!("bandwidth filter {other}"))),
            })
            .build(),
        modulation,
        accelerator: match wire.accelerator {
            0 => Accelerator::Native,
            1 => Accelerator::Hardware,
            other => return Err(RemoteError::bad_request(format!("accelerator {other}"))),
        },
        antenna: match wire.antenna {
            0 => Antenna::Internal,
            1 => Antenna::External,
            other => return Err(RemoteError::bad_request(format!("antenna {other}"))),
        },
    })
}

// ── Plugins via the local installer ───────────────────────────────────────────

#[derive(serde::Deserialize)]
struct InstallerPlugin {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    systemd_status: Option<InstallerSystemd>,
}

#[derive(serde::Deserialize)]
struct InstallerSystemd {
    #[serde(default)]
    active_state: String,
}

async fn plugin_list() -> Result<Reply, RemoteError> {
    let plugins = reqwest::Client::new()
        .get(format!("{INSTALLER_BASE}/api/plugins"))
        .send()
        .await
        .map_err(|err| RemoteError::error(format!("installer unreachable: {err}")))?
        .json::<Vec<InstallerPlugin>>()
        .await
        .map_err(|err| RemoteError::error(format!("installer response: {err}")))?;
    Ok(Reply::Plugins(
        plugins
            .into_iter()
            .map(|plugin| PluginInfoWire {
                active: plugin
                    .systemd_status
                    .as_ref()
                    .map(|s| s.active_state == "active")
                    .unwrap_or(false),
                id: plugin.id,
                name: plugin.name,
                version: plugin.version,
                enabled: plugin.enabled,
            })
            .collect(),
    ))
}

async fn plugin_action_call(id: &str, action: u8) -> Result<Reply, RemoteError> {
    if !valid_plugin_id(id) {
        return Err(RemoteError::bad_request("invalid plugin id"));
    }
    let client = reqwest::Client::new();
    let request = match action {
        plugin_action::START => client.post(format!("{INSTALLER_BASE}/api/plugins/{id}/start")),
        plugin_action::STOP => client.post(format!("{INSTALLER_BASE}/api/plugins/{id}/stop")),
        plugin_action::RESTART => client.post(format!("{INSTALLER_BASE}/api/plugins/{id}/restart")),
        plugin_action::DELETE => client.delete(format!("{INSTALLER_BASE}/api/plugins/{id}")),
        other => return Err(RemoteError::bad_request(format!("plugin action {other}"))),
    };
    installer_result(request.send().await).await
}

fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

async fn apply_blob(purpose: u8, name: &str, path: PathBuf) -> Result<Reply, RemoteError> {
    if purpose != blob::PLUGIN_PACKAGE {
        return Err(RemoteError::unsupported("blob purpose"));
    }
    if !name.is_empty() && !valid_plugin_id(name) {
        return Err(RemoteError::bad_request("invalid plugin id"));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|err| RemoteError::error(format!("read package: {err}")))?;
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("plugin.zip")
        .mime_str("application/zip")
        .map_err(|err| RemoteError::error(err.to_string()))?;
    let form = reqwest::multipart::Form::new().part("file", part);
    // `name` carries the plugin id when the controller is updating a known
    // plugin (the installer treats built-in targets specially there); an
    // empty name means "install this package".
    let url = if name.is_empty() {
        format!("{INSTALLER_BASE}/api/plugins/install")
    } else {
        format!("{INSTALLER_BASE}/api/plugins/{name}/upload")
    };
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(240))
        .build()
        .map_err(|err| RemoteError::error(err.to_string()))?
        .post(url)
        .multipart(form)
        .send()
        .await;
    installer_result(response).await
}

async fn installer_result(response: reqwest::Result<reqwest::Response>) -> Result<Reply, RemoteError> {
    let response = response.map_err(|err| RemoteError::error(format!("installer unreachable: {err}")))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
        .unwrap_or(body);
    if status.is_success() {
        Ok(Reply::Detail(detail))
    } else {
        Err(RemoteError::error(format!("installer {}: {detail}", status.as_u16())))
    }
}

// ── System ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
async fn request_system_reboot() -> Result<String, String> {
    systemctl(&["--no-block", "reboot"], "Reboot requested").await
}

#[cfg(not(target_os = "linux"))]
async fn request_system_reboot() -> Result<String, String> {
    Ok("Mock reboot requested".into())
}

#[cfg(target_os = "linux")]
async fn request_service_restart(unit: &str) -> Result<String, String> {
    systemctl(
        &["--no-block", "restart", unit],
        &format!("Restart requested for {unit}"),
    )
    .await
}

#[cfg(not(target_os = "linux"))]
async fn request_service_restart(unit: &str) -> Result<String, String> {
    Ok(format!("Mock restart requested for {unit}"))
}

#[cfg(target_os = "linux")]
async fn systemctl(args: &[&str], ok: &str) -> Result<String, String> {
    let output = tokio::process::Command::new("systemctl")
        .args(args)
        .output()
        .await
        .map_err(|err| format!("failed to execute systemctl {}: {err}", args.join(" ")))?;
    if output.status.success() {
        return Ok(ok.to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(if message.is_empty() {
        format!("systemctl {} failed", args.join(" "))
    } else {
        message
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_config_round_trips_through_wire() {
        let defaults = crate::radio::HardwareRadioConfig::default();
        for (module, cfg) in defaults.module_configs.iter().enumerate() {
            let wire = to_wire(module as u8, cfg);
            let back = from_wire(&wire).unwrap();
            assert_eq!(to_wire(module as u8, &back), wire);
            assert_eq!(back.radio_config.freq.as_hz(), cfg.radio_config.freq.as_hz());
            assert_eq!(back.radio_config.channel, cfg.radio_config.channel);
        }
    }

    #[test]
    fn wire_rejects_out_of_range_values() {
        let mut wire = to_wire(0, &crate::radio::HardwareRadioConfig::default().module_configs[0]);
        wire.mod_a = 9;
        assert!(from_wire(&wire).is_err());
        wire.mod_a = 0;
        wire.freq_hz = 0;
        assert!(from_wire(&wire).is_err());
    }
}

// ── Plugin service directory ──────────────────────────────────────────────────

/// Reserved plugin name the peer directories are cached under in the plugin
/// store; must match the reader in `http::plugin_api`.
pub const PEER_SERVICE_PLUGIN: &str = "@peers";

/// Reserved plugin name the announced digest of each cached directory is
/// kept under, so a restart does not re-fetch directories that have not
/// changed.
const PEER_SERVICE_DIGEST_PLUGIN: &str = "@peers.digest";

/// How often the cache is compared with what peers announce. This is a local
/// check — it costs no radio time — so it can be frequent.
const SERVICE_SYNC_POLL: Duration = Duration::from_secs(15);
/// After a failed fetch, a peer is left alone for this long. The announce
/// keeps carrying the digest, so nothing is lost by waiting.
const SERVICE_SYNC_RETRY: Duration = Duration::from_secs(300);

/// Digest of this node's own service directory, as advertised in announces.
pub fn local_services_digest(settings: &SharedSettings) -> u32 {
    let entries = settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_local_services()
        .unwrap_or_default();
    proto::services_digest(
        entries
            .iter()
            .map(|(name, destination)| (name.as_str(), destination.as_str())),
    )
}

/// Re-read the local directory and advertise its digest. Called when a plugin
/// registers a service, so peers learn about it on the next announce.
pub fn refresh_local_services_digest(state: &crate::state::AppState) {
    if let Some(remote) = state.remote.as_ref() {
        remote.set_services_digest(local_services_digest(&state.settings));
    }
}

/// Keeps every paired peer's plugin service directory in the local store.
///
/// This is the second half of "pair once, then plugins know each other": the
/// remote feature decides *which nodes* are trusted, each node's plugins
/// register *where they listen*, and this carries the second across the link
/// the first authenticated. A plugin then reads `/api/plugin/v1/contacts` and
/// finds, per contact, exactly the destinations it is entitled to link to.
///
/// The directory is fetched only when a peer's announce carries a digest that
/// differs from the cached one. Directories change when a plugin is installed,
/// which is rare; polling them would have cost every node a link handshake
/// to every other node on each round — traffic that grows with the square of
/// the mesh size and collides with the operator's own commands on a
/// half-duplex radio. One peer is synced per tick, so even a digest change
/// heard by the whole mesh at once does not turn into a burst of link
/// requests at the node that changed.
pub fn spawn_service_directory_sync(state: crate::state::AppState) {
    let Some(remote) = state.remote.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut failed_at: std::collections::HashMap<String, std::time::Instant> =
            std::collections::HashMap::new();
        // Spread the first tick out so co-located nodes do not all sync in
        // the same second after a fleet-wide restart.
        tokio::time::sleep(Duration::from_millis(5000 + rand::random::<u64>() % 10_000)).await;
        loop {
            let due = {
                let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
                remote
                    .snapshot()
                    .nodes
                    .into_iter()
                    .filter(|node| node.paired && node.online && node.services_digest != 0)
                    .filter(|node| {
                        let cached = settings
                            .plugin_get(PEER_SERVICE_DIGEST_PLUGIN, &node.identity_hash)
                            .ok()
                            .flatten()
                            .and_then(|raw| raw.parse::<u32>().ok());
                        cached != Some(node.services_digest)
                    })
                    .find(|node| {
                        failed_at
                            .get(&node.identity_hash)
                            .map(|at| at.elapsed() >= SERVICE_SYNC_RETRY)
                            .unwrap_or(true)
                    })
            };
            if let Some(node) = due {
                let Ok(hash) = AddressHash::new_from_hex_string(&node.identity_hash) else {
                    tokio::time::sleep(SERVICE_SYNC_POLL).await;
                    continue;
                };
                match remote.services(hash).await {
                    Ok(body) => {
                        failed_at.remove(&node.identity_hash);
                        let map: std::collections::BTreeMap<String, String> = body
                            .services
                            .into_iter()
                            .filter(|entry| valid_service_entry(entry))
                            .map(|entry| (entry.name, entry.destination.to_lowercase()))
                            .collect();
                        if let Ok(encoded) = serde_json::to_string(&map) {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let settings = state.settings.lock().unwrap_or_else(|e| e.into_inner());
                            let stored = settings
                                .plugin_set(PEER_SERVICE_PLUGIN, &node.identity_hash, &encoded, now)
                                .and_then(|_| {
                                    settings.plugin_set(
                                        PEER_SERVICE_DIGEST_PLUGIN,
                                        &node.identity_hash,
                                        &node.services_digest.to_string(),
                                        now,
                                    )
                                });
                            match stored {
                                Ok(()) => log::info!(
                                    "service directory from {} ({}): {} entries",
                                    node.codename,
                                    node.identity_hash,
                                    map.len()
                                ),
                                Err(err) => log::warn!(
                                    "cache service directory for {}: {err}",
                                    node.identity_hash
                                ),
                            }
                        }
                    }
                    Err(err) => {
                        // A gateway older than the op answers `unsupported`;
                        // that is a normal state in a mixed mesh, not a fault.
                        log::debug!(
                            "service directory from {}: {}",
                            node.identity_hash,
                            err.detail
                        );
                        failed_at.insert(node.identity_hash.clone(), std::time::Instant::now());
                    }
                }
            }
            tokio::time::sleep(SERVICE_SYNC_POLL).await;
        }
    });
}

/// A peer controls both fields, so both are checked before they are stored and
/// handed to a plugin as something to link to.
fn valid_service_entry(entry: &proto::ServiceEntry) -> bool {
    let name_ok = !entry.name.is_empty()
        && entry.name.len() <= 64
        && entry
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    let dest = entry.destination.trim();
    name_ok && dest.len() == 32 && dest.bytes().all(|b| b.is_ascii_hexdigit())
}

// ── VPN membership ────────────────────────────────────────────────────────────

/// Makes VPN membership follow the operator's pairing decision.
///
/// Without this the VPN would need its own allowlist, maintained separately
/// from the pairing the operator already did — two lists that drift apart, and
/// the weaker one wins. Here there is one decision: pair a node, and it may
/// join the VPN; unpair it, and it may not.
pub struct PairedPeerAuthority {
    settings: SharedSettings,
}

impl PairedPeerAuthority {
    pub fn new(settings: SharedSettings) -> Self {
        Self { settings }
    }
}

/// The VPN aspect every node's tunnel destination is built under. Must match
/// what `VpnRuntime::start` registers.
const VPN_DESTINATION: (&str, &str) = ("kaonic", "vpn");

/// The VPN destination hash a paired node will announce from.
///
/// A node's VPN destination is *not* its identity hash — it is a hash of the
/// identity together with the destination name. The pairing record stores the
/// peer's public keys precisely so it can be addressed without waiting for an
/// announce, so the hash is derived here rather than learned over the air.
/// Deriving it also means an attacker cannot get in by announcing a VPN
/// destination that merely claims a paired identity.
pub fn vpn_destination_hash(node: &PairedNode) -> Option<AddressHash> {
    let identity = node.identity()?;
    Some(
        reticulum::destination::SingleOutputDestination::new(
            identity,
            DestinationName::new(VPN_DESTINATION.0, VPN_DESTINATION.1),
        )
        .desc
        .address_hash,
    )
}

/// Maps an operator-facing identity hash to the VPN hash the tunnel uses.
/// The two identifiers exist for different layers and must not be confused.
pub fn vpn_hash_for_identity(settings: &SharedSettings, identity_hash: &str) -> Option<String> {
    let paired = settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .load_remote_paired()
        .ok()?;
    paired
        .iter()
        .find(|node| node.identity_hash == identity_hash)
        .and_then(vpn_destination_hash)
        .map(|hash| hash.to_hex_string())
}

impl PairedPeerAuthority {
    fn vpn_hash(node: &PairedNode) -> Option<AddressHash> {
        vpn_destination_hash(node)
    }
}

impl kaonic_vpn::PeerAuthority for PairedPeerAuthority {
    fn is_trusted(&self, hash: &reticulum::hash::AddressHash) -> bool {
        let paired = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .load_remote_paired()
            .unwrap_or_default();
        paired
            .iter()
            .filter_map(Self::vpn_hash)
            .any(|candidate| candidate == *hash)
    }
}
