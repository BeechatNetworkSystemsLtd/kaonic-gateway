//! Link sessions and the request/response RPC that runs on them.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use reticulum::destination::link::{LinkId, LinkStatus};
use reticulum::destination::DestinationDesc;
use reticulum::hash::AddressHash;
use reticulum::identity::Identity;
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::handler::{LinkClass, RemoteError};
use crate::nodes::remote_destination_for;
use crate::protocol::{op, status, Frame};
use crate::types::LinkState;

use super::*;

/// How hard an RPC tries before it gives up on the link.
#[derive(Debug, Clone, Copy)]
pub(super) struct RpcOptions {
    pub timeout: Option<Duration>,
    /// Total attempts with the same request id (the target replays cached
    /// answers, so a retry never re-runs a command).
    pub attempts: u32,
    /// Unanswered attempts tolerated before the link is re-established.
    pub relink_after: u32,
    /// Shortest wait for the first attempt, before any RTT scaling.
    pub first_wait_floor: Duration,
    /// Escalate later attempts to the full timeout.
    pub escalate: bool,
}

impl RpcOptions {
    /// A normal request: retry quickly once, then assume the link is gone.
    pub fn request(timeout: Option<Duration>) -> Self {
        Self {
            timeout,
            attempts: RPC_ATTEMPTS,
            relink_after: 2,
            first_wait_floor: Duration::from_millis(1500),
            escalate: true,
        }
    }

    /// A streaming status poll: on a half-duplex radio the reply is often
    /// lost to our own transmissions, so try often and stay on the link.
    pub fn status_poll() -> Self {
        Self {
            timeout: Some(STATUS_POLL_TIMEOUT),
            attempts: 8,
            relink_after: 6,
            first_wait_floor: Duration::from_millis(300),
            escalate: false,
        }
    }
}

pub(super) struct InSession {
    pub(super) remote: Option<Identity>,
    pub(super) created_ts: u64,
    /// Recently answered requests, keyed by (request id, op) so a reused id
    /// with a different opcode never replays the wrong body.
    pub(super) responses: VecDeque<((u16, u8), u8, Vec<u8>)>,
    pub(super) unauthorized_strikes: u8,
    pub(super) pair_attempts: u8,
    pub(super) last_pair_ts: u64,
}

impl InSession {
    pub(super) fn new() -> Self {
        Self {
            remote: None,
            created_ts: now_secs(),
            responses: VecDeque::new(),
            unauthorized_strikes: 0,
            pair_attempts: 0,
            last_pair_ts: 0,
        }
    }

    pub(super) fn cached(&self, id: u16, op: u8) -> Option<(u8, Vec<u8>)> {
        self.responses
            .iter()
            .find(|(key, _, _)| *key == (id, op))
            .map(|(_, status, body)| (*status, body.clone()))
    }

    pub(super) fn remember(&mut self, id: u16, op: u8, status: u8, body: Vec<u8>) {
        self.responses.push_back(((id, op), status, body));
        while self.responses.len() > RESPONSE_CACHE {
            self.responses.pop_front();
        }
    }
}

pub(super) struct OutSession {
    pub(super) link_id: Option<LinkId>,
    pub(super) state: LinkState,
    pub(super) identified: bool,
    pub(super) next_req: u16,
    pub(super) waiters: HashMap<u16, oneshot::Sender<(u8, Vec<u8>)>>,
    pub(super) last_activity: u64,
    pub(super) setup: Arc<AsyncMutex<()>>,
    /// Blob jobs currently streaming to this destination.
    pub(super) busy: u32,
    /// Smoothed request/response round trip, drives retransmit timing.
    pub(super) rtt_ms: u64,
    /// Link class currently requested from the host's link policy.
    pub(super) class: LinkClass,
    /// Consecutive failed automatic link attempts (drives the back-off).
    pub(super) auto_failures: u32,
    /// No automatic link attempt before this time.
    pub(super) auto_next_ts: u64,
}

impl OutSession {
    pub(super) fn new(now: u64) -> Self {
        Self {
            link_id: None,
            state: LinkState::None,
            identified: false,
            next_req: rand::random::<u16>(),
            waiters: HashMap::new(),
            last_activity: now,
            setup: Arc::new(AsyncMutex::new(())),
            busy: 0,
            rtt_ms: 0,
            class: LinkClass::Control,
            auto_failures: 0,
            auto_next_ts: 0,
        }
    }

    pub(super) fn fail_waiters(&mut self) {
        self.waiters.clear();
    }
}

impl RemoteRuntime {
    // ── Internals: links & RPC ───────────────────────────────────────────────

    pub(super) fn desc_for(&self, node: &AddressHash) -> Result<DestinationDesc, RemoteError> {
        if let Some(entry) = self.nodes.get(node) {
            return Ok(entry.desc());
        }
        self.paired
            .read()
            .get(node)
            .and_then(|paired| paired.identity())
            .map(remote_destination_for)
            .ok_or_else(|| RemoteError::not_found("unknown node"))
    }

    pub(super) async fn call_ok(
        self: &Arc<Self>,
        node: AddressHash,
        op: u8,
        body: Vec<u8>,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, RemoteError> {
        if !self.paired.read().contains_key(&node) {
            return Err(RemoteError::new(status::UNAUTHORIZED, "node is not paired"));
        }
        let desc = self.desc_for(&node)?;
        match self.call_desc(desc, op, body, timeout).await? {
            (status::OK, body) => Ok(body),
            (code, body) => Err(RemoteError::new(code, detail_of(&body))),
        }
    }

    /// Full RPC: ensure an identified link, send, await the response.
    /// Retries once with the same request id (the target replays cached
    /// responses, so a retry never re-executes a command).
    pub(super) async fn call_desc(
        self: &Arc<Self>,
        desc: DestinationDesc,
        op: u8,
        body: Vec<u8>,
        timeout: Option<Duration>,
    ) -> Result<(u8, Vec<u8>), RemoteError> {
        self.call_desc_with(desc, op, body, RpcOptions::request(timeout))
            .await
    }

    /// `attempts` total tries with the same request id; the link is torn
    /// down and re-established once `relink_after` tries went unanswered.
    /// Streaming status polls use many short tries because on a half-duplex
    /// radio a lost reply is routine, not a sign of a dead link.
    pub(super) async fn call_desc_with(
        self: &Arc<Self>,
        desc: DestinationDesc,
        op: u8,
        body: Vec<u8>,
        options: RpcOptions,
    ) -> Result<(u8, Vec<u8>), RemoteError> {
        let RpcOptions {
            timeout,
            attempts,
            relink_after,
            first_wait_floor,
            escalate,
        } = options;
        let dest = desc.address_hash;
        let timeout = timeout.unwrap_or(self.config.rpc_timeout);
        let mut last_err = RemoteError::error("no attempts");
        let mut request_id: Option<u16> = None;
        let mut unanswered = 0u32;
        for attempt in 0..attempts {
            self.ensure_link(desc).await?;
            let (id, rx) = {
                let mut sessions = self.out_sessions.lock();
                let session = sessions
                    .entry(dest)
                    .or_insert_with(|| OutSession::new(now_secs()));
                let id = match request_id {
                    Some(id) => id,
                    None => {
                        session.next_req = session.next_req.wrapping_add(1);
                        session.next_req
                    }
                };
                let (tx, rx) = oneshot::channel();
                session.waiters.insert(id, tx);
                session.last_activity = now_secs();
                (id, rx)
            };
            request_id = Some(id);
            let payload = Frame::Request {
                id,
                op,
                body: body.clone(),
            }
            .encode();
            log::debug!(
                "remote: tx {} id={id} op={} len={} attempt={attempt}",
                dest,
                op::name(op),
                payload.len()
            );
            if let Err(err) = self.send_out(&dest, &payload).await {
                self.out_sessions
                    .lock()
                    .get_mut(&dest)
                    .map(|s| s.waiters.remove(&id));
                last_err = err;
                continue;
            }
            // A lost packet is cheap to retry; a dead link is not. Wait an
            // RTT-scaled interval first, the full timeout on the retransmit
            // (the target replays its cached reply), and only then re-link.
            let rtt_ms = self
                .out_sessions
                .lock()
                .get(&dest)
                .map(|s| s.rtt_ms)
                .unwrap_or(0);
            let wait = if attempt == 0 || !escalate {
                Duration::from_millis(rtt_ms * 4)
                    .max(first_wait_floor)
                    .min(timeout)
            } else {
                timeout
            };
            let started = std::time::Instant::now();
            match tokio::time::timeout(wait, rx).await {
                Ok(Ok((code, resp))) => {
                    self.touch_out(&dest);
                    let elapsed = started.elapsed().as_millis() as u64;
                    if let Some(session) = self.out_sessions.lock().get_mut(&dest) {
                        // Smoothed RTT for the adaptive timeout above.
                        session.rtt_ms = if session.rtt_ms == 0 {
                            elapsed
                        } else {
                            (session.rtt_ms * 7 + elapsed) / 8
                        };
                    }
                    if code == status::UNAUTHORIZED && attempt == 0 {
                        // Our identify packet may not have been processed yet
                        // (or the link was re-established); identify again —
                        // after a pause, so the retry does not overtake the
                        // identify the same way the first request did.
                        if let Some(session) = self.out_sessions.lock().get_mut(&dest) {
                            session.identified = false;
                        }
                        last_err = RemoteError::new(code, detail_of(&resp));
                        tokio::time::sleep(Duration::from_millis(rtt_ms.max(500))).await;
                        continue;
                    }
                    return Ok((code, resp));
                }
                Ok(Err(_)) => {
                    // The waiter was dropped: the link went away underneath
                    // us. Pause briefly so the retry does not spin.
                    last_err = RemoteError::error("link closed");
                    unanswered += 1;
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                Err(_) => {
                    self.out_sessions
                        .lock()
                        .get_mut(&dest)
                        .map(|s| s.waiters.remove(&id));
                    last_err = RemoteError::error(format!("timeout after {}s", timeout.as_secs()));
                    unanswered += 1;
                    if unanswered < relink_after {
                        log::debug!(
                            "remote: {} id={id} no reply in {}ms; retransmitting",
                            dest,
                            wait.as_millis()
                        );
                    } else {
                        unanswered = 0;
                        // No answer even to the retransmit: the peer may have
                        // lost its side (restart, stale in-link). Tear the link
                        // down so the next attempt re-handshakes instead of
                        // shouting into a dead link.
                        log::warn!("remote: {} id={id} timed out; re-establishing link", dest);
                        self.close_out_link(&dest).await;
                    }
                }
            }
        }
        Err(last_err)
    }

    /// Make sure there is an active outbound link to `desc` on which we have
    /// identified ourselves.
    pub(super) async fn ensure_link(
        self: &Arc<Self>,
        desc: DestinationDesc,
    ) -> Result<(), RemoteError> {
        let dest = desc.address_hash;
        let setup = {
            let mut sessions = self.out_sessions.lock();
            sessions
                .entry(dest)
                .or_insert_with(|| OutSession::new(now_secs()))
                .setup
                .clone()
        };
        let _guard = setup.lock().await;

        let deadline = tokio::time::Instant::now() + self.config.link_timeout;
        let mut requested = false;
        loop {
            let existing = self.transport.lock().await.find_out_link(&dest).await;
            let (status, age) = match existing.as_ref() {
                Some(link) => {
                    let link = link.lock().await;
                    (Some(link.status()), link.elapsed().as_secs())
                }
                None => (None, 0),
            };
            match status {
                Some(LinkStatus::Active) => break,
                Some(LinkStatus::Stale) => {
                    // Thirty seconds of unanswered keep-alives: the peer has
                    // torn down (or is about to tear down) its in-link, so a
                    // request here would only time out and re-link anyway.
                    // Re-handshake now — two frames instead of two timeouts.
                    log::info!("remote: link to {dest} is stale; re-establishing");
                    let _ = self.transport.lock().await.link_close(dest).await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    requested = false;
                }
                Some(LinkStatus::Pending) | Some(LinkStatus::Handshake)
                    if age < LINK_PENDING_TIMEOUT_SECS =>
                {
                    self.set_link_state(&dest, LinkState::Pending, None);
                }
                Some(LinkStatus::Pending) | Some(LinkStatus::Handshake) => {
                    log::warn!("remote: link to {dest} stuck pending {age}s; re-requesting");
                    let _ = self.transport.lock().await.link_close(dest).await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    requested = false;
                }
                Some(LinkStatus::Closed) | None => {
                    if requested {
                        // Closed while we were waiting — treat as failure.
                        if status.is_some() {
                            self.set_link_state(&dest, LinkState::None, None);
                            return Err(RemoteError::error("link closed during setup"));
                        }
                    }
                    log::info!("remote: requesting link to {dest}");
                    let transport = self.transport.lock().await;
                    let link = transport.link(desc).await;
                    let link_id = *link.lock().await.id();
                    drop(transport);
                    self.set_link_state(&dest, LinkState::Pending, Some(link_id));
                    requested = true;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                // Leave nothing behind: a pending out-link the transport
                // still holds would re-send its request every few seconds
                // until something closes it.
                let _ = self.transport.lock().await.link_close(dest).await;
                self.set_link_state(&dest, LinkState::None, None);
                return Err(RemoteError::error("link setup timeout"));
            }
            tokio::time::sleep(Duration::from_millis(LINK_POLL_MILLIS)).await;
        }

        let link = self
            .transport
            .lock()
            .await
            .find_out_link(&dest)
            .await
            .ok_or_else(|| RemoteError::error("link vanished"))?;
        let link_id = *link.lock().await.id();
        let needs_identify = {
            let mut sessions = self.out_sessions.lock();
            let session = sessions
                .entry(dest)
                .or_insert_with(|| OutSession::new(now_secs()));
            session.link_id = Some(link_id);
            session.state = LinkState::Active;
            !session.identified
        };
        self.apply_link_class(&dest);
        if needs_identify {
            let packet = link
                .lock()
                .await
                .identify(&self.identity)
                .map_err(|err| RemoteError::error(format!("identify: {err:?}")))?;
            self.transport.lock().await.send_packet(packet).await;
            if let Some(session) = self.out_sessions.lock().get_mut(&dest) {
                session.identified = true;
            }
            log::debug!("remote: identified on link {link_id} to {dest}");
        }
        Ok(())
    }

    /// One automatic link attempt to a paired peer; bookkeeping for the
    /// watchdog's back-off lives on the session.
    pub(super) async fn auto_link_attempt(self: &Arc<Self>, node: AddressHash) {
        let Ok(desc) = self.desc_for(&node) else {
            return;
        };
        let dest = desc.address_hash;
        // A link is only useful once the peer knows who opened it, and the
        // identify packet is not acknowledged. The ping proves it landed —
        // and if it did not, the RPC path answers UNAUTHORIZED by
        // identifying again, so this both verifies and repairs.
        let result = match self.ensure_link(desc).await {
            Ok(()) => self
                .call_desc(desc, op::PING, Vec::new(), None)
                .await
                .and_then(|(code, body)| match code {
                    status::OK => Ok(()),
                    code => Err(RemoteError::new(code, detail_of(&body))),
                }),
            Err(err) => Err(err),
        };
        match result {
            Ok(()) => {
                log::info!("remote: auto-linked to {node}");
                if let Some(session) = self.out_sessions.lock().get_mut(&dest) {
                    session.auto_failures = 0;
                    session.auto_next_ts = 0;
                }
            }
            Err(err) => {
                let mut sessions = self.out_sessions.lock();
                let session = sessions
                    .entry(dest)
                    .or_insert_with(|| OutSession::new(now_secs()));
                session.auto_failures = session.auto_failures.saturating_add(1);
                let backoff = (AUTO_LINK_BACKOFF_SECS << session.auto_failures.min(4))
                    .min(AUTO_LINK_BACKOFF_MAX_SECS);
                session.auto_next_ts = now_secs() + backoff;
                log::info!(
                    "remote: auto-link to {node} failed ({}); next try in {backoff}s",
                    err.detail
                );
            }
        }
    }

    pub(super) fn set_link_state(
        &self,
        dest: &AddressHash,
        state: LinkState,
        link_id: Option<LinkId>,
    ) {
        let mut sessions = self.out_sessions.lock();
        let session = sessions
            .entry(*dest)
            .or_insert_with(|| OutSession::new(now_secs()));
        let changed = session.state != state;
        session.state = state;
        if link_id.is_some() {
            session.link_id = link_id;
        }
        if state == LinkState::None {
            session.identified = false;
            session.link_id = None;
        }
        drop(sessions);
        if changed {
            self.notify_changed();
        }
    }

    pub(super) fn touch_out(&self, dest: &AddressHash) {
        if let Some(session) = self.out_sessions.lock().get_mut(dest) {
            session.last_activity = now_secs();
        }
    }

    pub(super) async fn send_out(
        &self,
        dest: &AddressHash,
        payload: &[u8],
    ) -> Result<(), RemoteError> {
        let sent = self
            .transport
            .lock()
            .await
            .send_to_out_links(dest, payload)
            .await;
        if sent.is_empty() {
            return Err(RemoteError::error("no active link"));
        }
        Ok(())
    }

    pub(super) async fn send_in(
        &self,
        link_id: &LinkId,
        payload: &[u8],
    ) -> Result<(), RemoteError> {
        let transport = self.transport.lock().await;
        let link = transport
            .find_in_link(link_id)
            .await
            .ok_or_else(|| RemoteError::error("in-link gone"))?;
        let packet = {
            let mut link = link.lock().await;
            let packet = link
                .data_packet(payload)
                .map_err(|err| RemoteError::error(format!("data packet: {err:?}")))?;
            link.touch();
            packet
        };
        transport.send_packet(packet).await;
        Ok(())
    }

    pub(super) async fn close_out_link(&self, dest: &AddressHash) {
        let has_link = self
            .transport
            .lock()
            .await
            .find_out_link(dest)
            .await
            .is_some();
        if has_link {
            if let Err(err) = self.transport.lock().await.link_close(*dest).await {
                log::debug!("remote: close link {dest}: {err:?}");
            }
        }
        self.set_link_state(dest, LinkState::None, None);
    }

    pub(super) async fn close_in_link(&self, link_id: &LinkId) {
        if let Err(err) = self.transport.lock().await.link_close(*link_id).await {
            log::debug!("remote: close in-link {link_id}: {err:?}");
        }
        self.drop_in_session(link_id).await;
    }

    pub(super) async fn drop_in_session(&self, link_id: &LinkId) {
        // Spooled transfers are intentionally kept: the controller resumes
        // them on a fresh link, and the watchdog reaps abandoned ones.
        self.in_sessions.lock().remove(link_id);
    }

    // ── Internals: inbound handling (target role) ────────────────────────────
}
