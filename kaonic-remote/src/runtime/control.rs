//! Controller-side API: the commands an operator issues to a paired node.

use std::sync::Arc;
use std::time::Duration;

use reticulum::hash::AddressHash;

use crate::handler::RemoteError;
use crate::protocol::{
    self as proto, decode_body, encode_body, op, PluginActionBody, RadioConfigWire, RadioGetBody,
    ServiceRestartBody, ShellExecBody, ShellFetchBody, ShellResultBody,
};

use super::*;

impl RemoteRuntime {
    // ── Controller RPC API ───────────────────────────────────────────────────

    pub async fn ping(self: &Arc<Self>, node: AddressHash) -> Result<Duration, RemoteError> {
        let started = std::time::Instant::now();
        self.call_ok(node, op::PING, Vec::new(), None).await?;
        Ok(started.elapsed())
    }

    pub async fn info(self: &Arc<Self>, node: AddressHash) -> Result<proto::InfoBody, RemoteError> {
        let body = self.call_ok(node, op::INFO, Vec::new(), None).await?;
        decode_body(&body).map_err(RemoteError::error)
    }

    pub async fn radio_get(
        self: &Arc<Self>,
        node: AddressHash,
        module: u8,
    ) -> Result<RadioConfigWire, RemoteError> {
        let body = self
            .call_ok(
                node,
                op::RADIO_GET,
                encode_body(&RadioGetBody { module }),
                None,
            )
            .await?;
        decode_body(&body).map_err(RemoteError::error)
    }

    pub async fn radio_set(
        self: &Arc<Self>,
        node: AddressHash,
        config: RadioConfigWire,
    ) -> Result<String, RemoteError> {
        let body = self
            .call_ok(node, op::RADIO_SET, encode_body(&config), None)
            .await?;
        Ok(detail_of(&body))
    }

    pub async fn plugin_list(
        self: &Arc<Self>,
        node: AddressHash,
    ) -> Result<Vec<proto::PluginInfoWire>, RemoteError> {
        let body = self
            .call_ok(node, op::PLUGIN_LIST, Vec::new(), None)
            .await?;
        decode_body(&body).map_err(RemoteError::error)
    }

    pub async fn plugin_action(
        self: &Arc<Self>,
        node: AddressHash,
        id: String,
        action: u8,
    ) -> Result<String, RemoteError> {
        let body = self
            .call_ok(
                node,
                op::PLUGIN_ACTION,
                encode_body(&PluginActionBody { id, action }),
                Some(Duration::from_secs(40)),
            )
            .await?;
        Ok(detail_of(&body))
    }

    /// Run a shell command on a paired node and collect the whole output,
    /// paging it in radio-frame-sized slices.
    pub async fn shell_exec(
        self: &Arc<Self>,
        node: AddressHash,
        command: String,
        timeout_secs: u32,
    ) -> Result<ShellResultBody, RemoteError> {
        let body = self
            .call_ok(
                node,
                op::SHELL_EXEC,
                encode_body(&ShellExecBody {
                    command,
                    timeout_secs,
                }),
                Some(Duration::from_secs(
                    u64::from(timeout_secs.clamp(5, 300)) + 20,
                )),
            )
            .await?;
        let mut result: ShellResultBody = decode_body(&body).map_err(RemoteError::error)?;
        let mut output = result.chunk.clone();
        while output.len() < result.total as usize {
            let next = self
                .call_ok(
                    node,
                    op::SHELL_FETCH,
                    encode_body(&ShellFetchBody {
                        offset: output.len() as u32,
                    }),
                    None,
                )
                .await?;
            let page: ShellResultBody = decode_body(&next).map_err(RemoteError::error)?;
            if page.chunk.is_empty() {
                break;
            }
            output.push_str(&page.chunk);
        }
        result.chunk = output;
        result.offset = 0;
        Ok(result)
    }

    pub async fn system_reboot(self: &Arc<Self>, node: AddressHash) -> Result<String, RemoteError> {
        let body = self
            .call_ok(node, op::SYSTEM_REBOOT, Vec::new(), None)
            .await?;
        Ok(detail_of(&body))
    }

    pub async fn service_restart(
        self: &Arc<Self>,
        node: AddressHash,
        unit: String,
    ) -> Result<String, RemoteError> {
        let body = self
            .call_ok(
                node,
                op::SERVICE_RESTART,
                encode_body(&ServiceRestartBody { unit }),
                Some(Duration::from_secs(40)),
            )
            .await?;
        Ok(detail_of(&body))
    }

    /// Generic RPC to a node without the local paired check — the target
    /// still enforces its own trust policy. Returns `(status, body)`.
    pub async fn call_raw(
        self: &Arc<Self>,
        node: AddressHash,
        op: u8,
        body: Vec<u8>,
        timeout: Option<Duration>,
    ) -> Result<(u8, Vec<u8>), RemoteError> {
        let desc = self.desc_for(&node)?;
        self.call_desc(desc, op, body, timeout).await
    }
}
