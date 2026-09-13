//! The Kaonic plugin SDK.
//!
//! A plugin is its own process with its own Reticulum identity. This crate is
//! everything it needs to join the system: a radio channel, a Reticulum
//! destination, and a link to the gateway it runs on.
//!
//! ```no_run
//! use kaonic_plugin_sdk::{profiles, Plugin};
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let plugin = Plugin::builder("kaonic-audio-ptt")
//!     .identity_seed("a-stable-secret")
//!     .module(0)
//!     .profile(profiles::Voice::default())
//!     .start()
//!     .await?;
//!
//! // Nodes the operator paired with that also run this plugin.
//! for (contact, destination) in plugin.peers().await? {
//!     println!("{} serves us at {destination}", contact.display_name());
//! }
//!
//! // State that outlives a reinstall of the plugin.
//! if let Some(store) = plugin.store() {
//!     store.set("favourites", &vec!["b4o0cvts"]).await?;
//! }
//! # Ok(()) }
//! ```
//!
//! # Trust
//!
//! Plugins do not do their own pairing. The operator pairs *nodes* once, in
//! the gateway's remote feature; each node's plugins register where they
//! listen; and the gateway exchanges those directories with paired peers over
//! the link the pairing already authenticated. [`Plugin::peers`] is the result:
//! the destinations this plugin may link to, each bound to a node the operator
//! chose to trust.
//!
//! # Compatibility
//!
//! The gateway API is versioned and only ever grows within a version, and this
//! SDK treats every response field as optional. A plugin built against this
//! SDK keeps working against a newer gateway, and degrades — rather than
//! fails — against an older one.

pub mod error;
pub mod gateway;
pub mod node;
pub mod store;

pub use error::SdkError;
pub use gateway::{Contact, GatewayClient, Identity, API_MAJOR};
pub use node::{default_timers, Plugin, PluginBuilder};
pub use store::Store;

/// The channel layer: profiles, channel handles, and the Reticulum interface
/// over a channel. Re-exported so a plugin depends on this crate alone.
pub use kaonic_reticulum::channel::{
    self, profiles, Channel, ChannelId, ChannelRx, ChannelTx, FecCode, ProfileSpec, Radio,
    ReticulumChannel, TrafficClass,
};

/// Reticulum itself, for the parts a plugin drives directly: announces, links,
/// and its own destination.
pub use reticulum;
