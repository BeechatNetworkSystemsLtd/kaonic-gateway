//! Handling of requests arriving on inbound links (the target role).

use std::sync::Arc;

use reticulum::destination::link::LinkId;
use reticulum::hash::AddressHash;
use reticulum::identity::Identity;

use crate::handler::{Command, RemoteError, Reply};
use crate::protocol::{
    blob, decode_body, op, perm, status, BlobBeginBody, BlobBeginResponse, BlobRefBody, DetailBody,
    Frame, PluginActionBody, RadioGetBody, ServiceRestartBody, ShellExecBody, ShellFetchBody,
};
use crate::transfer::BlobReceiver;

use super::*;

impl RemoteRuntime {
    pub(super) async fn handle_in_data(self: &Arc<Self>, link_id: LinkId, payload: &[u8]) {
        let Some(frame) = Frame::decode(payload) else {
            log::debug!(
                "remote: undecodable frame on in-link {link_id} ({} B)",
                payload.len()
            );
            return;
        };
        match frame {
            Frame::Chunk {
                transfer,
                index,
                data,
            } => {
                let Some(identity) = self.authorized_identity(&link_id, perm::PLUGINS) else {
                    return;
                };
                let mut receivers = self.receivers.lock().await;
                if let Some(receiver) = receivers.get_mut(&(identity.address_hash, transfer)) {
                    if let Err(err) = receiver.write_chunk(index, &data, now_secs()).await {
                        log::warn!("remote: chunk {index} on {link_id}: {err}");
                    }
                }
            }
            Frame::Parity {
                transfer,
                block,
                shard,
                data,
            } => {
                let Some(identity) = self.authorized_identity(&link_id, perm::PLUGINS) else {
                    return;
                };
                let mut receivers = self.receivers.lock().await;
                if let Some(receiver) = receivers.get_mut(&(identity.address_hash, transfer)) {
                    match receiver.write_parity(block, shard, &data, now_secs()).await {
                        Ok(0) => {}
                        Ok(n) => {
                            log::debug!("remote: parity rebuilt {n} chunk(s) of block {block}")
                        }
                        Err(err) => {
                            log::warn!("remote: parity {block}/{shard} on {link_id}: {err}")
                        }
                    }
                }
            }
            Frame::Media(shard) => {
                let Some(identity) = self.authorized_identity(&link_id, perm::MEDIA) else {
                    return;
                };
                self.media.receive(identity.address_hash, shard).await;
            }
            Frame::Request { id, op, body } => {
                // Identify is a separate packet and the air does not promise
                // order: a request can land before the identify that was sent
                // ahead of it. Refusing it outright would make the controller
                // re-identify and race the same way again, so on a link that
                // has not identified yet the request waits a moment for the
                // identify to catch up — off the queue, so other links are
                // not held up.
                let identified = self
                    .in_sessions
                    .lock()
                    .get(&link_id)
                    .map(|session| session.remote.is_some())
                    .unwrap_or(false);
                if !identified {
                    let runtime = self.clone();
                    tokio::spawn(async move {
                        runtime.await_identify(&link_id).await;
                        runtime.handle_request(link_id, id, op, body).await;
                    });
                    return;
                }
                self.handle_request(link_id, id, op, body).await;
            }
            Frame::Response { .. } => {
                // Targets never issue requests on inbound links.
            }
        }
    }

    /// Wait (bounded) for the initiator of `link_id` to identify.
    async fn await_identify(&self, link_id: &LinkId) {
        let deadline = tokio::time::Instant::now() + IDENTIFY_WAIT;
        while tokio::time::Instant::now() < deadline {
            let identified = self
                .in_sessions
                .lock()
                .get(link_id)
                .map(|session| session.remote.is_some());
            match identified {
                Some(true) | None => return,
                Some(false) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    async fn handle_request(self: &Arc<Self>, link_id: LinkId, id: u16, op: u8, body: Vec<u8>) {
        {
            {
                let cached = self
                    .in_sessions
                    .lock()
                    .get(&link_id)
                    .and_then(|session| session.cached(id, op));
                let (code, resp) = match cached {
                    Some(cached) => cached,
                    None => {
                        let (code, resp) = self.dispatch_in(&link_id, op, &body).await;
                        // A refusal is not a result: the controller answers it
                        // by identifying and retrying the same request id, so
                        // caching it would make that retry permanent.
                        if code != status::UNAUTHORIZED {
                            if let Some(session) = self.in_sessions.lock().get_mut(&link_id) {
                                session.remember(id, op, code, resp.clone());
                            }
                        }
                        (code, resp)
                    }
                };
                log::debug!(
                    "remote: rx {link_id} id={id} op={} -> {} ({} B)",
                    op::name(op),
                    status::name(code),
                    resp.len()
                );
                let frame = Frame::Response {
                    id,
                    status: code,
                    body: resp,
                }
                .encode();
                if let Err(err) = self.send_in(&link_id, &frame).await {
                    log::warn!("remote: response on {link_id} failed: {err}");
                }
                if code == status::UNAUTHORIZED {
                    let strikes = {
                        let mut sessions = self.in_sessions.lock();
                        sessions
                            .get_mut(&link_id)
                            .map(|session| {
                                session.unauthorized_strikes += 1;
                                session.unauthorized_strikes
                            })
                            .unwrap_or(0)
                    };
                    if strikes >= MAX_UNAUTHORIZED_STRIKES {
                        log::warn!("remote: closing in-link {link_id} after {strikes} unauthorized requests");
                        self.close_in_link(&link_id).await;
                    }
                }
            }
        }
    }

    /// Remote identity on an inbound link, if it identified *and* is paired
    /// with at least `required` permissions.
    pub(super) fn authorized_identity(&self, link_id: &LinkId, required: u32) -> Option<Identity> {
        let identity = self.in_sessions.lock().get(link_id)?.remote?;
        let paired = self.paired.read();
        let node = paired.get(&identity.address_hash)?;
        node.allows(required).then_some(identity)
    }

    pub(super) async fn dispatch_in(
        self: &Arc<Self>,
        link_id: &LinkId,
        op: u8,
        body: &[u8],
    ) -> (u8, Vec<u8>) {
        let remote = self
            .in_sessions
            .lock()
            .get(link_id)
            .and_then(|session| session.remote);
        let Some(remote) = remote else {
            return err_body(status::UNAUTHORIZED, "link not identified");
        };

        match op {
            op::PAIR_REQUEST => self.handle_pair_request(link_id, remote, body).await,
            op::PAIR_RESULT => self.handle_pair_result(remote, body),
            _ => {
                let required = perm::required(op);
                let Some(_) = self.authorized_identity(link_id, required) else {
                    let paired = self.paired.read().contains_key(&remote.address_hash);
                    return err_body(
                        status::UNAUTHORIZED,
                        if paired {
                            "permission denied"
                        } else {
                            "not paired"
                        },
                    );
                };
                match op {
                    op::UNPAIR => {
                        let hash = remote.address_hash;
                        self.paired.write().remove(&hash);
                        let _ = self.store.remove(&hash.to_hex_string());
                        self.push_event("unpair", Some(hash), "remote node removed pairing");
                        self.notify_changed();
                        ok_body(&DetailBody {
                            detail: "unpaired".into(),
                        })
                    }
                    op::BLOB_BEGIN | op::BLOB_STATUS | op::BLOB_END | op::BLOB_ABORT => {
                        self.handle_blob(remote, op, body).await
                    }
                    _ => {
                        let command = match decode_command(op, body) {
                            Ok(command) => command,
                            Err(err) => return err_body(err.status, &err.detail),
                        };
                        self.push_event("command", Some(remote.address_hash), op::name(op));
                        match self.handler.handle(command).await {
                            Ok(reply) => encode_reply(reply),
                            Err(err) => err_body(err.status, &err.detail),
                        }
                    }
                }
            }
        }
    }

    pub(super) async fn handle_blob(
        self: &Arc<Self>,
        remote: Identity,
        op: u8,
        body: &[u8],
    ) -> (u8, Vec<u8>) {
        let now = now_secs();
        match op {
            op::BLOB_BEGIN => {
                let begin = match decode_body::<BlobBeginBody>(body) {
                    Ok(begin) => begin,
                    Err(err) => return err_body(status::BAD_REQUEST, &err),
                };
                if begin.purpose != blob::PLUGIN_PACKAGE {
                    return err_body(status::UNSUPPORTED, "unknown blob purpose");
                }
                let Ok(sha256) = <[u8; 32]>::try_from(begin.sha256.as_slice()) else {
                    return err_body(status::BAD_REQUEST, "sha256 must be 32 bytes");
                };
                if begin.block_m > 0
                    && (begin.block_k == 0
                        || usize::from(begin.block_k) + usize::from(begin.block_m)
                            > crate::erasure::MAX_BLOCK_SHARDS)
                {
                    return err_body(status::BAD_REQUEST, "invalid erasure block");
                }
                let owner = remote.address_hash;
                let mut receivers = self.receivers.lock().await;

                // Resume: the same blob (hash + size + chunking) is already
                // spooled for this identity — continue where it stopped.
                let resumable = receivers
                    .iter()
                    .find(|((id, _), r)| {
                        *id == owner
                            && r.sha256 == sha256
                            && r.size == begin.size
                            && r.chunk_size == begin.chunk_size as u32
                            && r.block_k() == u32::from(begin.block_k.max(1))
                    })
                    .map(|((_, transfer), r)| (*transfer, r.next_index()));
                if let Some((transfer, next_index)) = resumable {
                    if let Some(receiver) = receivers.get_mut(&(owner, transfer)) {
                        receiver.last_activity = now;
                    }
                    log::info!(
                        "remote: resuming transfer {transfer} from {owner} at chunk {next_index}"
                    );
                    return ok_body(&BlobBeginResponse {
                        transfer,
                        next_index,
                    });
                }

                // One transfer per identity at a time: drop any other spool.
                let stale: Vec<(AddressHash, u8)> = receivers
                    .keys()
                    .filter(|(id, _)| *id == owner)
                    .copied()
                    .collect();
                let mut transfer = 0u8;
                for key in stale {
                    transfer = transfer.max(key.1);
                    if let Some(old) = receivers.remove(&key) {
                        old.discard().await;
                    }
                }
                let transfer = transfer.wrapping_add(1);
                let file_name = format!("{}-{transfer}.part", owner.to_hex_string());
                let spec = crate::transfer::BlobSpec {
                    purpose: begin.purpose,
                    name: sanitize_name(&begin.name),
                    size: begin.size,
                    sha256,
                    chunk_size: begin.chunk_size as u32,
                    block_k: begin.block_k,
                    block_m: begin.block_m,
                };
                let receiver =
                    match BlobReceiver::create(&self.config.spool_dir, &file_name, spec, now).await
                    {
                        Ok(receiver) => receiver,
                        Err(err) => return err_body(status::ERROR, &err),
                    };
                receivers.insert((owner, transfer), receiver);
                self.push_event(
                    "blob-begin",
                    Some(owner),
                    format!("{} ({} B)", sanitize_name(&begin.name), begin.size),
                );
                ok_body(&BlobBeginResponse {
                    transfer,
                    next_index: 0,
                })
            }
            op::BLOB_STATUS => {
                let reference = match decode_body::<BlobRefBody>(body) {
                    Ok(reference) => reference,
                    Err(err) => return err_body(status::BAD_REQUEST, &err),
                };
                let mut receivers = self.receivers.lock().await;
                match receivers.get_mut(&(remote.address_hash, reference.transfer)) {
                    Some(receiver) => {
                        receiver.last_activity = now;
                        ok_body(&receiver.status())
                    }
                    None => err_body(status::NOT_FOUND, "no such transfer"),
                }
            }
            op::BLOB_END => {
                let reference = match decode_body::<BlobRefBody>(body) {
                    Ok(reference) => reference,
                    Err(err) => return err_body(status::BAD_REQUEST, &err),
                };
                let mut receiver = {
                    let mut receivers = self.receivers.lock().await;
                    match receivers.remove(&(remote.address_hash, reference.transfer)) {
                        Some(receiver) => receiver,
                        None => return err_body(status::NOT_FOUND, "no such transfer"),
                    }
                };
                let path = match receiver.finish().await {
                    Ok(path) => path,
                    Err(err) => {
                        receiver.discard().await;
                        return err_body(status::ERROR, &err);
                    }
                };
                self.push_event(
                    "blob-apply",
                    Some(remote.address_hash),
                    receiver.name.clone(),
                );
                let result = self
                    .handler
                    .handle(Command::ApplyBlob {
                        purpose: receiver.purpose,
                        name: receiver.name.clone(),
                        path: path.clone(),
                    })
                    .await;
                let _ = tokio::fs::remove_file(&path).await;
                match result {
                    Ok(reply) => encode_reply(reply),
                    Err(err) => err_body(err.status, &err.detail),
                }
            }
            _ => {
                let reference = match decode_body::<BlobRefBody>(body) {
                    Ok(reference) => reference,
                    Err(err) => return err_body(status::BAD_REQUEST, &err),
                };
                if let Some(receiver) = self
                    .receivers
                    .lock()
                    .await
                    .remove(&(remote.address_hash, reference.transfer))
                {
                    receiver.discard().await;
                }
                ok_body(&DetailBody {
                    detail: "aborted".into(),
                })
            }
        }
    }

    // ── Internals: blob sender (controller role) ─────────────────────────────
}

pub(super) fn decode_command(op: u8, body: &[u8]) -> Result<Command, RemoteError> {
    Ok(match op {
        op::PING => Command::Ping,
        op::INFO => Command::Info,
        op::SERVICES => Command::Services,
        op::RADIO_GET => Command::RadioGet {
            module: decode_body::<RadioGetBody>(body)
                .map_err(RemoteError::bad_request)?
                .module,
        },
        op::RADIO_SET => Command::RadioSet(decode_body(body).map_err(RemoteError::bad_request)?),
        op::PLUGIN_LIST => Command::PluginList,
        op::PLUGIN_ACTION => {
            let action = decode_body::<PluginActionBody>(body).map_err(RemoteError::bad_request)?;
            Command::PluginAction {
                id: action.id,
                action: action.action,
            }
        }
        op::SYSTEM_REBOOT => Command::SystemReboot,
        op::SERVICE_RESTART => Command::ServiceRestart {
            unit: decode_body::<ServiceRestartBody>(body)
                .map_err(RemoteError::bad_request)?
                .unit,
        },
        op::SHELL_EXEC => {
            let body = decode_body::<ShellExecBody>(body).map_err(RemoteError::bad_request)?;
            Command::ShellExec {
                command: body.command,
                timeout_secs: body.timeout_secs,
            }
        }
        op::SHELL_FETCH => Command::ShellFetch {
            offset: decode_body::<ShellFetchBody>(body)
                .map_err(RemoteError::bad_request)?
                .offset,
        },
        _ => return Err(RemoteError::unsupported(format!("op 0x{op:02x}"))),
    })
}

pub(super) fn encode_reply(reply: Reply) -> (u8, Vec<u8>) {
    match reply {
        Reply::Empty => (status::OK, Vec::new()),
        Reply::Detail(detail) => ok_body(&DetailBody { detail }),
        Reply::Info(info) => ok_body(&info),
        Reply::Services(services) => ok_body(&services),
        Reply::Radio(config) => ok_body(&config),
        Reply::Plugins(plugins) => ok_body(&plugins),
        Reply::Shell(result) => ok_body(&result),
    }
}
