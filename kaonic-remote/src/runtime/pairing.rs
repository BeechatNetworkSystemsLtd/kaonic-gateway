//! Operator-approved pairing: requests, approvals and their persistence.

use std::sync::Arc;
use std::time::Duration;

use reticulum::destination::link::LinkId;
use reticulum::hash::AddressHash;
use reticulum::identity::Identity;

use crate::handler::RemoteError;
use crate::nodes::remote_destination_for;
use crate::protocol::{
    decode_body, encode_body, op, status, DetailBody, PairRequestBody, PairResultBody,
};
use crate::trust::{default_permissions, PairedNode, PairingDirection, PairingRecord};
use crate::types::PairingState;

use super::*;

#[derive(Debug, Clone)]
pub(super) struct IncomingRequest {
    pub(super) identity: Identity,
    pub(super) codename: String,
    pub(super) received_ts: u64,
}

#[derive(Debug, Clone)]
pub(super) struct OutgoingRequest {
    pub(super) identity: Identity,
    pub(super) codename: String,
    pub(super) state: PairingState,
    pub(super) detail: String,
    pub(super) ts: u64,
    pub(super) retries: u32,
}

impl RemoteRuntime {
    // ── Pairing (controller side) ────────────────────────────────────────────

    /// Adds a node from its public keys rather than from an announce, and
    /// starts pairing with it.
    ///
    /// The normal path is: hear an announce, then pair. That fails exactly
    /// when it is most needed — a node that is out of range, powered off, or
    /// simply has not announced yet cannot be queued up. A scanned code
    /// carries the peer's public keys, which is everything needed to address
    /// it, so the node can be added now and the request delivered whenever it
    /// comes within reach.
    ///
    /// This adds a *pending* relationship, not a trusted one: pairing still
    /// requires the operator on the other device to approve, so a scanned code
    /// grants nothing on its own.
    pub async fn add_node_from_identity(
        self: &Arc<Self>,
        identity: Identity,
        codename: &str,
    ) -> Result<AddressHash, RemoteError> {
        let hash = identity.address_hash;
        if hash == self.identity_hash() {
            return Err(RemoteError::bad_request("that code is this device's own"));
        }
        self.nodes.seed(identity, codename);
        if !codename.is_empty() {
            self.nodes.update_codename(&hash, codename);
        }
        Ok(hash)
    }

    pub async fn request_pairing(
        self: &Arc<Self>,
        node: AddressHash,
    ) -> Result<PairingState, RemoteError> {
        if self.paired.read().contains_key(&node) {
            return Ok(PairingState::Paired);
        }
        let entry = self
            .nodes
            .get(&node)
            .ok_or_else(|| RemoteError::not_found("unknown node"))?;
        self.set_outgoing(
            &entry.identity,
            &entry.codename,
            PairingState::Requested,
            "sending request",
        );
        let body = encode_body(&PairRequestBody {
            codename: self.local.read().codename.clone(),
        });
        let result = self
            .call_desc(entry.desc(), op::PAIR_REQUEST, body, None)
            .await;
        let state = match result {
            Ok((status::OK, body)) => {
                let codename = decode_body::<PairResultBody>(&body)
                    .map(|r| r.codename)
                    .unwrap_or_else(|_| entry.codename.clone());
                self.add_paired(entry.identity, &codename, "pairing accepted");
                PairingState::Paired
            }
            Ok((status::PENDING, _)) => {
                self.set_outgoing(
                    &entry.identity,
                    &entry.codename,
                    PairingState::Requested,
                    "waiting for approval on remote node",
                );
                PairingState::Requested
            }
            Ok((status::REJECTED, _)) => {
                self.set_outgoing(
                    &entry.identity,
                    &entry.codename,
                    PairingState::Rejected,
                    "rejected by remote node",
                );
                PairingState::Rejected
            }
            Ok((code, body)) => {
                let detail = decode_body::<DetailBody>(&body)
                    .map(|d| d.detail)
                    .unwrap_or_default();
                self.set_outgoing(
                    &entry.identity,
                    &entry.codename,
                    PairingState::Failed,
                    format!("{}: {detail}", status::name(code)),
                );
                PairingState::Failed
            }
            Err(err) => {
                self.set_outgoing(
                    &entry.identity,
                    &entry.codename,
                    PairingState::Failed,
                    err.to_string(),
                );
                PairingState::Failed
            }
        };
        self.push_event("pair-request", Some(node), format!("{state:?}"));
        Ok(state)
    }

    pub fn cancel_pairing(&self, node: AddressHash) {
        self.outgoing.lock().remove(&node);
        self.forget_request(&node, PairingDirection::Outgoing);
        self.notify_changed();
    }

    // ── Pairing (target side) ────────────────────────────────────────────────

    pub fn approve_pairing(self: &Arc<Self>, node: AddressHash) -> Result<(), RemoteError> {
        let request = self
            .incoming
            .lock()
            .remove(&node)
            .ok_or_else(|| RemoteError::not_found("no pending request"))?;
        self.forget_request(&node, PairingDirection::Incoming);
        self.add_paired(request.identity, &request.codename, "pairing approved");
        self.spawn_pair_notify(request.identity, true);
        Ok(())
    }

    pub fn reject_pairing(self: &Arc<Self>, node: AddressHash) -> Result<(), RemoteError> {
        let request = self
            .incoming
            .lock()
            .remove(&node)
            .ok_or_else(|| RemoteError::not_found("no pending request"))?;
        self.forget_request(&node, PairingDirection::Incoming);
        self.push_event("pair-reject", Some(node), "pairing rejected");
        self.spawn_pair_notify(request.identity, false);
        self.notify_changed();
        Ok(())
    }

    pub async fn unpair(self: &Arc<Self>, node: AddressHash) -> Result<(), RemoteError> {
        let removed = self.paired.write().remove(&node);
        if removed.is_none() {
            return Err(RemoteError::not_found("node is not paired"));
        }
        if let Err(err) = self.store.remove(&node.to_hex_string()) {
            log::warn!("remote: trust store remove failed: {err}");
        }
        self.outgoing.lock().remove(&node);
        self.forget_request(&node, PairingDirection::Outgoing);
        self.push_event("unpair", Some(node), "pairing removed");
        self.notify_changed();
        // Best effort: tell the other side so it drops us too, then close.
        if let Some(entry) = self.nodes.get(&node) {
            let _ = self
                .call_desc(
                    entry.desc(),
                    op::UNPAIR,
                    Vec::new(),
                    Some(Duration::from_secs(8)),
                )
                .await;
            self.close_out_link(&entry.destination).await;
        }
        Ok(())
    }

    // ── Internals: pairing bookkeeping ───────────────────────────────────────

    pub(super) fn set_outgoing(
        &self,
        identity: &Identity,
        codename: &str,
        state: PairingState,
        detail: impl Into<String>,
    ) {
        let node = identity.address_hash;
        let detail = detail.into();
        let record = {
            let mut outgoing = self.outgoing.lock();
            let retries = outgoing.get(&node).map(|r| r.retries).unwrap_or(0);
            let request = OutgoingRequest {
                identity: *identity,
                codename: codename.to_string(),
                state,
                detail,
                ts: now_secs(),
                retries,
            };
            let record = outgoing_record(&request);
            outgoing.insert(node, request);
            record
        };
        if let Err(err) = self.store.save_request(&record) {
            log::error!("remote: trust store save request failed: {err}");
        }
        self.notify_changed();
    }

    pub(super) fn forget_request(&self, node: &AddressHash, direction: PairingDirection) {
        if let Err(err) = self.store.remove_request(&node.to_hex_string(), direction) {
            log::error!("remote: trust store remove request failed: {err}");
        }
    }

    pub(super) fn add_paired(&self, identity: Identity, codename: &str, reason: &str) {
        let hash = identity.address_hash;
        let node = PairedNode {
            identity_hash: hash.to_hex_string(),
            identity_hex: identity.to_hex_string(),
            codename: codename.to_string(),
            paired_at: now_secs(),
            permissions: default_permissions(),
        };
        if let Err(err) = self.store.save(&node) {
            log::error!("remote: trust store save failed: {err}");
        }
        self.nodes.seed(identity, codename);
        self.nodes.update_codename(&hash, codename);
        self.paired.write().insert(hash, node);
        self.outgoing.lock().remove(&hash);
        self.incoming.lock().remove(&hash);
        self.forget_request(&hash, PairingDirection::Outgoing);
        self.forget_request(&hash, PairingDirection::Incoming);
        self.push_event("paired", Some(hash), reason);
        self.notify_changed();
    }

    pub(super) fn spawn_pair_notify(self: &Arc<Self>, identity: Identity, accepted: bool) {
        let runtime = self.clone();
        tokio::spawn(async move {
            let desc = runtime
                .nodes
                .get(&identity.address_hash)
                .map(|entry| entry.desc())
                .unwrap_or_else(|| remote_destination_for(identity));
            let body = encode_body(&PairResultBody {
                accepted,
                codename: runtime.local.read().codename.clone(),
            });
            let attempts = if accepted { PAIR_NOTIFY_ATTEMPTS } else { 1 };
            for attempt in 1..=attempts {
                match runtime
                    .call_desc(desc, op::PAIR_RESULT, body.clone(), None)
                    .await
                {
                    Ok((status::OK, _)) => {
                        log::info!(
                            "remote: pairing result delivered to {}",
                            identity.address_hash
                        );
                        runtime.push_event(
                            "pair-notify",
                            Some(identity.address_hash),
                            "result delivered",
                        );
                        break;
                    }
                    Ok((code, _)) => {
                        log::warn!(
                            "remote: pairing result to {} answered {}",
                            identity.address_hash,
                            status::name(code)
                        );
                        break;
                    }
                    Err(err) => {
                        log::warn!(
                            "remote: pairing result to {} attempt {attempt} failed: {err}",
                            identity.address_hash
                        );
                        if attempt < attempts {
                            tokio::time::sleep(Duration::from_secs(5 * attempt as u64)).await;
                        }
                    }
                }
            }
            runtime.close_out_link(&desc.address_hash).await;
        });
    }

    pub(super) async fn handle_pair_request(
        self: &Arc<Self>,
        link_id: &LinkId,
        remote: Identity,
        body: &[u8],
    ) -> (u8, Vec<u8>) {
        let now = now_secs();
        {
            let mut sessions = self.in_sessions.lock();
            let Some(session) = sessions.get_mut(link_id) else {
                return err_body(status::ERROR, "no session");
            };
            if session.pair_attempts >= MAX_PAIR_ATTEMPTS
                || now.saturating_sub(session.last_pair_ts) < PAIR_ATTEMPT_MIN_GAP_SECS
            {
                return err_body(status::BUSY, "too many pairing attempts");
            }
            session.pair_attempts += 1;
            session.last_pair_ts = now;
        }
        let request = match decode_body::<PairRequestBody>(body) {
            Ok(request) => request,
            Err(err) => return err_body(status::BAD_REQUEST, &err),
        };
        let codename = sanitize_codename(&request.codename);
        let local_codename = self.local.read().codename.clone();
        let hash = remote.address_hash;

        if self.paired.read().contains_key(&hash) {
            // Already trusted (e.g. requester lost its state) — confirm.
            self.nodes.update_codename(&hash, &codename);
            return ok_body(&PairResultBody {
                accepted: true,
                codename: local_codename,
            });
        }
        if !self.config.accept_pairing {
            return (
                status::REJECTED,
                encode_body(&DetailBody {
                    detail: "pairing disabled".into(),
                }),
            );
        }
        {
            let incoming = self.incoming.lock();
            if !incoming.contains_key(&hash) && incoming.len() >= MAX_INCOMING_REQUESTS {
                return err_body(status::BUSY, "too many pending pairing requests");
            }
        }
        self.nodes.seed(remote, &codename);
        self.nodes.update_codename(&hash, &codename);
        let request = IncomingRequest {
            identity: remote,
            codename: codename.clone(),
            received_ts: now,
        };
        if let Err(err) = self.store.save_request(&incoming_record(&request)) {
            log::error!("remote: trust store save request failed: {err}");
        }
        let is_new = self.incoming.lock().insert(hash, request).is_none();
        if is_new {
            log::info!("remote: pairing request from {codename} ({hash})");
            self.push_event("pair-incoming", Some(hash), "pairing request received");
        }
        self.notify_changed();
        (
            status::PENDING,
            encode_body(&PairResultBody {
                accepted: false,
                codename: local_codename,
            }),
        )
    }

    pub(super) fn handle_pair_result(&self, remote: Identity, body: &[u8]) -> (u8, Vec<u8>) {
        let hash = remote.address_hash;
        let result = match decode_body::<PairResultBody>(body) {
            Ok(result) => result,
            Err(err) => return err_body(status::BAD_REQUEST, &err),
        };
        if self.paired.read().contains_key(&hash) {
            return ok_body(&DetailBody {
                detail: "already paired".into(),
            });
        }
        let outstanding = self
            .outgoing
            .lock()
            .get(&hash)
            .map(|request| request.state == PairingState::Requested)
            .unwrap_or(false);
        if !outstanding {
            return err_body(status::BAD_REQUEST, "no pending request");
        }
        let codename = sanitize_codename(&result.codename);
        if result.accepted {
            self.add_paired(remote, &codename, "pairing accepted by remote");
        } else {
            self.set_outgoing(
                &remote,
                &codename,
                PairingState::Rejected,
                "rejected by remote node",
            );
            self.push_event("pair-reject", Some(hash), "pairing rejected by remote");
        }
        ok_body(&DetailBody {
            detail: "ok".into(),
        })
    }
}

pub(super) fn outgoing_record(request: &OutgoingRequest) -> PairingRecord {
    PairingRecord {
        identity_hash: request.identity.address_hash.to_hex_string(),
        identity_hex: request.identity.to_hex_string(),
        codename: request.codename.clone(),
        direction: PairingDirection::Outgoing,
        state: request.state.as_str().to_string(),
        detail: request.detail.clone(),
        ts: request.ts,
        retries: request.retries,
    }
}

pub(super) fn incoming_record(request: &IncomingRequest) -> PairingRecord {
    PairingRecord {
        identity_hash: request.identity.address_hash.to_hex_string(),
        identity_hex: request.identity.to_hex_string(),
        codename: request.codename.clone(),
        direction: PairingDirection::Incoming,
        state: PairingState::Incoming.as_str().to_string(),
        detail: String::new(),
        ts: request.received_ts,
        retries: 0,
    }
}
