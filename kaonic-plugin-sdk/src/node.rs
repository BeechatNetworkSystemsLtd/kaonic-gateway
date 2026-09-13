//! Bootstrap: one call that gives a plugin its Reticulum identity, its radio
//! channel, its destination and its link to the gateway.
//!
//! Every Kaonic plugin needs the same six things, in the same order, and each
//! one is easy to get subtly wrong (retransmit left on turns a plugin into a
//! transport node; a channel opened with the wrong profile shares nothing with
//! the peer). [`Plugin::builder`] does it once, correctly.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use kaonic_reticulum::channel::{ChannelId, ProfileSpec, Radio, ReticulumChannel};
use kaonic_reticulum::KaonicCtrlInterface;
use reticulum::destination::{DestinationName, SingleInputDestination};
use reticulum::identity::PrivateIdentity;
use reticulum::transport::{TimerConfig, Transport, TransportConfig};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::error::SdkError;
use crate::gateway::{Contact, GatewayClient, Identity};
use crate::store::Store;

/// Timers that suit a plugin holding conversational links over a slow radio:
/// long enough that a link survives a burst of loss, short enough that a dead
/// peer is noticed. Override with [`PluginBuilder::timers`].
pub fn default_timers() -> TimerConfig {
    TimerConfig {
        in_link_stale: Duration::from_secs(30),
        in_link_close: Duration::from_secs(15),
        out_link_restart: Duration::from_secs(45),
        out_link_stale: Duration::from_secs(30),
        out_link_close: Duration::from_secs(15),
        out_link_repeat: Duration::from_secs(10),
        out_link_keep: Duration::from_secs(20),
        ..TimerConfig::default()
    }
}

/// A running plugin: its mesh identity, its radio path and its gateway.
#[derive(Clone)]
pub struct Plugin {
    name: String,
    transport: Arc<Mutex<Transport>>,
    destination: Arc<Mutex<SingleInputDestination>>,
    destination_hash: String,
    radio: Radio,
    gateway: Option<GatewayClient>,
    cancel: CancellationToken,
    /// Largest payload the channel accepts, or `None` on the legacy path.
    channel_mtu: Option<usize>,
}

impl Plugin {
    /// Largest single payload the plugin's channel carries, so a protocol can
    /// size what it sends instead of modelling the coding underneath.
    /// `None` on the legacy in-process radio path, which has no channel.
    pub fn channel_mtu(&self) -> Option<usize> {
        self.channel_mtu
    }

    /// Starts building a plugin. `name` is the protocol name: it is the
    /// channel id both ends derive, the service name published to paired
    /// peers, and the namespace of the persistent store — so it must be the
    /// same string on every node running this plugin.
    pub fn builder(name: impl Into<String>) -> PluginBuilder {
        PluginBuilder {
            name: name.into(),
            identity_seed: None,
            daemon: "127.0.0.1:9090".parse().expect("loopback daemon address"),
            app: "kaonic".to_string(),
            aspect: None,
            module: 0,
            profile: ProfileSpec::Robust,
            timers: default_timers(),
            retransmit: false,
            restart_outlinks: true,
            bonded: false,
            legacy_radio: std::env::var("KAONIC_RADIO_LEGACY")
                .map(|value| value == "1")
                .unwrap_or(false),
            cancel: None,
        }
    }

    /// The protocol name this plugin was built with.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn transport(&self) -> Arc<Mutex<Transport>> {
        self.transport.clone()
    }

    pub fn destination(&self) -> Arc<Mutex<SingleInputDestination>> {
        self.destination.clone()
    }

    /// This plugin's own destination hash, hex. Peers reach it here.
    pub fn destination_hash(&self) -> &str {
        &self.destination_hash
    }

    pub fn radio(&self) -> &Radio {
        &self.radio
    }

    /// Cancelling stops the radio client and the channel forwarding tasks.
    pub fn cancel(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// The gateway API, when it could be reached. `None` means the plugin is
    /// running without a gateway: contacts and persistence are unavailable,
    /// everything on the radio still works.
    pub fn gateway(&self) -> Option<&GatewayClient> {
        self.gateway.as_ref()
    }

    /// Persistent storage for this plugin, or `None` without a gateway.
    pub fn store(&self) -> Option<Store> {
        self.gateway
            .as_ref()
            .map(|client| Store::new(client.clone(), self.name.clone()))
    }

    /// This node's identity as the gateway reports it.
    pub async fn identity(&self) -> Result<Identity, SdkError> {
        match self.gateway.as_ref() {
            Some(client) => client.identity().await,
            None => Err(SdkError::gateway("no gateway")),
        }
    }

    /// Nodes the operator paired with. An empty list without an error means
    /// nothing is paired (or the remote feature is off — see
    /// [`Identity::remote_enabled`]).
    pub async fn contacts(&self) -> Result<Vec<Contact>, SdkError> {
        match self.gateway.as_ref() {
            Some(client) => client.contacts().await,
            None => Ok(Vec::new()),
        }
    }

    /// Contacts that run this same plugin, paired with the destination each
    /// serves it on.
    ///
    /// This is the answer to "which links may I create?". The destination came
    /// over a link the pairing authenticated, so it identifies the contact —
    /// unlike a codename in an announce, which anyone can claim. Note that the
    /// hash alone cannot open a link: Reticulum needs the peer's public keys,
    /// which arrive in its announce. Use this to decide *whether* to link to
    /// an announced destination, not as a substitute for the announce.
    pub async fn peers(&self) -> Result<Vec<(Contact, String)>, SdkError> {
        Ok(self
            .contacts()
            .await?
            .into_iter()
            .filter_map(|contact| {
                contact
                    .service(&self.name)
                    .map(|destination| destination.to_string())
                    .map(|destination| (contact, destination))
            })
            .collect())
    }

    /// Re-publishes this plugin's destination to the gateway. Called once by
    /// [`PluginBuilder::start`]; call it again only if the destination changes.
    pub async fn register(&self) -> Result<(), SdkError> {
        match self.gateway.as_ref() {
            Some(client) => {
                client
                    .register_service(&self.name, &self.destination_hash)
                    .await
            }
            None => Err(SdkError::gateway("no gateway")),
        }
    }
}

pub struct PluginBuilder {
    name: String,
    identity_seed: Option<String>,
    daemon: SocketAddr,
    app: String,
    aspect: Option<String>,
    module: usize,
    profile: ProfileSpec,
    timers: TimerConfig,
    retransmit: bool,
    restart_outlinks: bool,
    legacy_radio: bool,
    bonded: bool,
    cancel: Option<CancellationToken>,
}

impl PluginBuilder {
    /// Seed the plugin's Reticulum identity is derived from. The same seed
    /// gives the same destination across restarts, which is what makes a saved
    /// contact list meaningful.
    pub fn identity_seed(mut self, seed: impl Into<String>) -> Self {
        self.identity_seed = Some(seed.into());
        self
    }

    /// Address of the radio daemon; `127.0.0.1:9090` on a node.
    pub fn daemon(mut self, daemon: SocketAddr) -> Self {
        self.daemon = daemon;
        self
    }

    /// Reticulum destination name. Defaults to app `kaonic` and the plugin
    /// name with any `kaonic-` prefix removed, so `kaonic-audio-ptt` becomes
    /// `kaonic.audio-ptt`.
    pub fn destination_name(mut self, app: impl Into<String>, aspect: impl Into<String>) -> Self {
        self.app = app.into();
        self.aspect = Some(aspect.into());
        self
    }

    /// Radio module, 0 or 1.
    pub fn module(mut self, module: usize) -> Self {
        self.module = module.min(1);
        self
    }

    /// How the daemon should code and schedule this plugin's traffic.
    pub fn profile(mut self, profile: impl Into<ProfileSpec>) -> Self {
        self.profile = profile.into();
        self
    }

    pub fn timers(mut self, timers: TimerConfig) -> Self {
        self.timers = timers;
        self
    }

    /// Retransmission makes this node a transport node that rebroadcasts every
    /// announce it hears, so mesh traffic grows with the square of the nodes.
    /// Off unless the plugin is deliberately acting as a relay.
    pub fn retransmit(mut self, retransmit: bool) -> Self {
        self.retransmit = retransmit;
        self
    }

    pub fn restart_outlinks(mut self, restart: bool) -> Self {
        self.restart_outlinks = restart;
        self
    }

    /// Use the in-process frame pipeline instead of a daemon channel, for a
    /// daemon that predates channels. Defaults to the `KAONIC_RADIO_LEGACY`
    /// environment variable.
    /// Spread the channel across every radio the node has. Throughput scales
    /// with the radios and the MTU grows with them; both ends must choose the
    /// same. On a node with one radio this is the same as `module(0)`, so a
    /// protocol can ask for it unconditionally. Overrides `module`.
    pub fn bonded(mut self, bonded: bool) -> Self {
        self.bonded = bonded;
        self
    }

    pub fn legacy_radio(mut self, legacy: bool) -> Self {
        self.legacy_radio = legacy;
        self
    }

    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Connects the radio, opens the channel, creates the destination and
    /// registers it with the gateway.
    ///
    /// A gateway that cannot be reached is not fatal: the plugin still runs on
    /// the radio, without contacts or persistence. A radio that cannot be
    /// reached is, since nothing else would work.
    pub async fn start(self) -> Result<Plugin, SdkError> {
        let cancel = self.cancel.unwrap_or_default();
        let seed = self
            .identity_seed
            .unwrap_or_else(|| format!("{}-default-identity", self.name));
        let id = PrivateIdentity::new_from_name(&seed);

        let radio = Radio::connect(self.daemon, cancel.clone())
            .await
            .map_err(|err| SdkError::radio(format!("connect {}: {err:?}", self.daemon)))?;

        // A plugin works in payloads; it never wants the daemon's push of
        // every frame off the air, which costs a 2 KB decode per frame on the
        // same single core the daemon needs. Off by default; a plugin that
        // really wants frames can ask through `radio().client()`.
        if let Err(err) = radio.client().lock().await.set_frame_stream(false).await {
            log::warn!("{}: can't turn the frame stream off: {err:?}", self.name);
        }

        let transport_cfg = TransportConfig::new(&self.name, &id)
            .set_retransmit(self.retransmit)
            .set_timer_config(self.timers)
            .set_restart_outlinks(self.restart_outlinks);
        let transport = Arc::new(Mutex::new(Transport::new(transport_cfg)));

        let iface_mgr = transport.lock().await.iface_manager();
        let mut channel_mtu = None;
        if self.legacy_radio {
            let iface = KaonicCtrlInterface::new(radio.client(), self.module, None, None);
            iface_mgr
                .lock()
                .await
                .spawn(iface, KaonicCtrlInterface::spawn);
            log::info!("{}: radio path legacy in-process (module {})", self.name, self.module);
        } else {
            let id = ChannelId::of(&self.name);
            let builder = if self.bonded {
                radio
                    .bonded_channel(id)
                    .await
                    .map_err(|err| SdkError::radio(format!("bonded channel: {err:?}")))?
            } else {
                radio.channel(id).await.modules([self.module])
            };
            let channel = builder
                .profile(self.profile.clone())
                .build()
                .await
                .map_err(|err| SdkError::radio(format!("open channel: {err}")))?;
            channel_mtu = Some(channel.tx().mtu());
            log::info!(
                "{}: channel {} on {} (mtu {} B)",
                self.name,
                channel.id().raw(),
                if self.bonded { "all modules".to_string() } else { format!("module {}", self.module) },
                channel.tx().mtu()
            );
            // A newest-wins queue means the daemon drops what cannot go; the
            // plugin gains nothing by waiting to hear that per packet.
            let drop_oldest = matches!(
                self.profile,
                ProfileSpec::Voice { .. }
                    | ProfileSpec::Custom {
                        runtime: kaonic_reticulum::channel::Runtime {
                            queue: kaonic_reticulum::channel::QueuePolicy::DropOldest { .. },
                            ..
                        },
                        ..
                    }
            );
            let mut iface = ReticulumChannel::new(channel);
            if drop_oldest {
                iface = iface.with_posted_sends();
            }
            iface_mgr
                .lock()
                .await
                .spawn(iface, ReticulumChannel::spawn);
        }

        let aspect = self
            .aspect
            .unwrap_or_else(|| self.name.trim_start_matches("kaonic-").to_string());
        let destination = transport
            .lock()
            .await
            .add_destination(id, DestinationName::new(&self.app, &aspect))
            .await;
        let destination_hash = destination.lock().await.desc.address_hash.to_hex_string();

        // Without a gateway the plugin is still usable on the radio, so this
        // is a warning and not a failure.
        let gateway = match GatewayClient::from_env() {
            Ok(client) => match client.register_service(&self.name, &destination_hash).await {
                Ok(()) => {
                    log::info!(
                        "{}: registered destination {} with the gateway",
                        self.name,
                        destination_hash
                    );
                    Some(client)
                }
                Err(err) => {
                    log::warn!("{}: gateway registration failed: {err}", self.name);
                    Some(client)
                }
            },
            Err(err) => {
                log::warn!(
                    "{}: no gateway API ({err}); contacts and persistence are unavailable",
                    self.name
                );
                None
            }
        };

        log::info!("{}: destination {}", self.name, destination_hash);
        Ok(Plugin {
            name: self.name,
            transport,
            destination,
            destination_hash,
            radio,
            gateway,
            cancel,
            channel_mtu,
        })
    }
}
