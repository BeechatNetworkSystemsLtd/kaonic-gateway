//! Per-plugin persistence, held by the gateway.
//!
//! A plugin's own directory is wiped when it is reinstalled; the gateway
//! database is not. Anything a plugin wants to survive an upgrade — a contact
//! list, a user's settings, a saved identity — belongs here.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::SdkError;
use crate::gateway::GatewayClient;

/// A key/value namespace scoped to one plugin. Clone it freely.
#[derive(Clone)]
pub struct Store {
    client: GatewayClient,
    plugin: String,
}

impl Store {
    pub fn new(client: GatewayClient, plugin: impl Into<String>) -> Self {
        Self {
            client,
            plugin: plugin.into(),
        }
    }

    /// Reads `key`, or `None` when nothing is stored there.
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, SdkError> {
        match self.client.store_get::<T>(&self.plugin, key).await {
            Ok(value) => Ok(Some(value)),
            Err(err) if err.is_not_found() => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Reads `key`, falling back to `default` for both "never stored" and
    /// "the gateway is unreachable" — what most callers want at startup.
    pub async fn get_or<T: DeserializeOwned>(&self, key: &str, default: T) -> T {
        match self.get(key).await {
            Ok(Some(value)) => value,
            Ok(None) => default,
            Err(err) => {
                log::debug!("plugin store: {key}: {err}");
                default
            }
        }
    }

    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), SdkError> {
        self.client.store_set(&self.plugin, key, value).await
    }

    pub async fn delete(&self, key: &str) -> Result<(), SdkError> {
        self.client.store_delete(&self.plugin, key).await
    }

    pub async fn keys(&self) -> Result<Vec<String>, SdkError> {
        self.client.store_keys(&self.plugin).await
    }
}
