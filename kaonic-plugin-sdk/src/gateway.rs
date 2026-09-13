//! Client for the gateway's local plugin API.
//!
//! The gateway serves this on loopback only, behind a bearer token in a 0600
//! file. A plugin runs on the same device, so it reads the file and asks.
//!
//! # Stability
//!
//! The client pins a major version in the path (`/api/plugin/v1`) and treats
//! every response field as optional: a gateway older than this SDK simply
//! leaves new fields at their defaults, and one newer sends fields this SDK
//! ignores. Neither is an error. Check [`GatewayClient::api_version`] when a
//! feature genuinely requires a newer gateway, and degrade rather than fail.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::SdkError;

/// Major version of the API this SDK speaks.
pub const API_MAJOR: u32 = 1;

/// Loopback port the gateway serves the plugin API on.
pub const DEFAULT_PORT: u16 = 8781;

fn default_base_url() -> String {
    std::env::var("KAONIC_PLUGIN_API")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{DEFAULT_PORT}"))
}

fn default_token_path() -> PathBuf {
    std::env::var("KAONIC_PLUGIN_TOKEN_PATH")
        .unwrap_or_else(|_| "/etc/kaonic/plugin-api.token".to_string())
        .into()
}

/// This node, as the gateway knows it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Identity {
    pub api_version: u32,
    pub codename: String,
    pub serial: String,
    pub gateway_version: String,
    /// The *gateway's* Reticulum identity — not the plugin's.
    pub identity_hash: String,
    pub destination_hash: String,
    pub vpn_hash: String,
    /// False when the remote feature is off; [`GatewayClient::contacts`] is
    /// then empty for that reason rather than because nothing is paired.
    pub remote_enabled: bool,
}

/// A node this operator has paired with, and what its plugins serve.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Contact {
    pub identity_hash: String,
    pub destination_hash: String,
    pub codename: String,
    /// The operator's own label for this node, when they set one.
    pub tag: String,
    pub paired: bool,
    pub online: bool,
    pub hops: Option<u8>,
    pub rssi: Option<i8>,
    pub last_seen_ts: u64,
    pub vpn_tunnel_ip: String,
    /// Service name → destination hash, as this contact's gateway published
    /// it over the paired link.
    pub services: BTreeMap<String, String>,
}

impl Contact {
    /// The destination this contact serves `service` on, if it serves it.
    ///
    /// This is the value a plugin should link to: it arrived over a link the
    /// pairing authenticated, so it is bound to *this* contact rather than
    /// claimed by whoever announced a matching name.
    pub fn service(&self, service: &str) -> Option<&str> {
        self.services.get(service).map(String::as_str)
    }

    /// What to show a person: their label if they set one, else the codename,
    /// else a short hash.
    pub fn display_name(&self) -> String {
        if !self.tag.is_empty() {
            return self.tag.clone();
        }
        if !self.codename.is_empty() {
            return self.codename.clone();
        }
        self.identity_hash.chars().take(8).collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct ContactsResponse {
    contacts: Vec<Contact>,
}

impl Default for ContactsResponse {
    fn default() -> Self {
        Self {
            contacts: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ServicesResponse {
    services: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct KeysResponse {
    keys: Vec<String>,
}

#[derive(Serialize)]
struct ServiceRequest<'a> {
    service: &'a str,
    destination: &'a str,
}

/// Connection to the local gateway. Cheap to clone.
#[derive(Clone)]
pub struct GatewayClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl GatewayClient {
    /// Connects using the token file and default port. Fails when the token
    /// cannot be read — that is the normal case off-device, and a plugin
    /// should carry on without gateway features rather than exit.
    pub fn from_env() -> Result<Self, SdkError> {
        let token = match std::env::var("KAONIC_PLUGIN_TOKEN") {
            Ok(token) if !token.trim().is_empty() => token.trim().to_string(),
            _ => {
                let path = default_token_path();
                std::fs::read_to_string(&path)
                    .map_err(|err| {
                        SdkError::gateway(format!("read token {}: {err}", path.display()))
                    })?
                    .trim()
                    .to_string()
            }
        };
        Self::new(default_base_url(), token)
    }

    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Result<Self, SdkError> {
        let http = reqwest::Client::builder()
            // Loopback: a request that has not answered in a second is not
            // going to, and a plugin's UI should not stall on it.
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|err| SdkError::gateway(format!("http client: {err}")))?;
        Ok(Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/plugin/v{API_MAJOR}{path}", self.base)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SdkError> {
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|err| SdkError::gateway(format!("GET {path}: {err}")))?;
        Self::decode(path, response).await
    }

    async fn decode<T: DeserializeOwned>(
        path: &str,
        response: reqwest::Response,
    ) -> Result<T, SdkError> {
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(SdkError::NotFound);
        }
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(SdkError::gateway(format!("{path}: {status} {detail}")));
        }
        response
            .json()
            .await
            .map_err(|err| SdkError::gateway(format!("{path}: decode {err}")))
    }

    /// Who this node is. Also the cheapest liveness check on the gateway.
    pub async fn identity(&self) -> Result<Identity, SdkError> {
        self.get("/identity").await
    }

    /// The API version the gateway serves, for a feature that needs a newer
    /// one than this SDK's minimum.
    pub async fn api_version(&self) -> Result<u32, SdkError> {
        Ok(self.identity().await?.api_version)
    }

    /// Every node the operator paired with, newest state each time.
    pub async fn contacts(&self) -> Result<Vec<Contact>, SdkError> {
        let response: ContactsResponse = self.get("/contacts").await?;
        Ok(response.contacts)
    }

    /// Tells the gateway where this plugin listens, so it can publish the
    /// destination to paired peers. Call it once the Reticulum destination
    /// exists, and again if it ever changes.
    pub async fn register_service(
        &self,
        service: &str,
        destination: &str,
    ) -> Result<(), SdkError> {
        let response = self
            .http
            .post(self.url("/services"))
            .bearer_auth(&self.token)
            .json(&ServiceRequest {
                service,
                destination,
            })
            .send()
            .await
            .map_err(|err| SdkError::gateway(format!("register service: {err}")))?;
        let _: ServicesResponse = Self::decode("/services", response).await?;
        Ok(())
    }

    /// What this node's own plugins have registered.
    pub async fn services(&self) -> Result<BTreeMap<String, String>, SdkError> {
        let response: ServicesResponse = self.get("/services").await?;
        Ok(response.services)
    }

    // ── Persistence ──────────────────────────────────────────────────────────

    /// Reads a stored value. [`SdkError::NotFound`] means nothing was ever
    /// stored under that key, which is usually a first-run default, not an
    /// error worth logging.
    pub async fn store_get<T: DeserializeOwned>(
        &self,
        plugin: &str,
        key: &str,
    ) -> Result<T, SdkError> {
        self.get(&format!("/store/{plugin}/{key}")).await
    }

    pub async fn store_set<T: Serialize>(
        &self,
        plugin: &str,
        key: &str,
        value: &T,
    ) -> Result<(), SdkError> {
        let path = format!("/store/{plugin}/{key}");
        let response = self
            .http
            .put(self.url(&path))
            .bearer_auth(&self.token)
            .json(value)
            .send()
            .await
            .map_err(|err| SdkError::gateway(format!("PUT {path}: {err}")))?;
        let _: serde_json::Value = Self::decode(&path, response).await?;
        Ok(())
    }

    pub async fn store_delete(&self, plugin: &str, key: &str) -> Result<(), SdkError> {
        let path = format!("/store/{plugin}/{key}");
        let response = self
            .http
            .delete(self.url(&path))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|err| SdkError::gateway(format!("DELETE {path}: {err}")))?;
        let _: serde_json::Value = Self::decode(&path, response).await?;
        Ok(())
    }

    pub async fn store_keys(&self, plugin: &str) -> Result<Vec<String>, SdkError> {
        let response: KeysResponse = self.get(&format!("/store/{plugin}")).await?;
        Ok(response.keys)
    }
}
