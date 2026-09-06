//! Background tasks: announces, link events, media pacing, watchdog.

use std::sync::Arc;
use std::time::Duration;

use reticulum::destination::link::LinkEvent;
use reticulum::hash::AddressHash;
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;

use crate::nodes::remote_destination_name;
use crate::protocol::{self as proto, decode_announce, encode_announce, Frame};
use crate::trust::{PairingDirection, PairingRecord};
use crate::types::{LinkState, PairingState};

use super::media_hub::{MEDIA_POLL, MEDIA_STREAM_IDLE};
use super::pairing::outgoing_record;
use super::*;

/// Requests whose handler may block for seconds, so they are run off the
/// inbound queue rather than in arrival order.
fn is_slow_request(payload: &[u8]) -> bool {
    if payload.first() != Some(&proto::KIND_REQUEST) {
        return false;
    }
    matches!(
        payload.get(3),
        Some(&proto::op::SHELL_EXEC)
            | Some(&proto::op::PLUGIN_ACTION)
            | Some(&proto::op::BLOB_END)
            | Some(&proto::op::SERVICE_RESTART)
            | Some(&proto::op::SYSTEM_REBOOT)
    )
}

/// Start every background task the runtime needs.
pub(super) fn spawn_tasks(runtime: &Arc<RemoteRuntime>, cancel: CancellationToken) {
    spawn_announce_tx(runtime.clone(), cancel.clone());
    spawn_announce_rx(runtime.clone(), cancel.clone());
    spawn_in_link_events(runtime.clone(), cancel.clone());
    spawn_out_link_events(runtime.clone(), cancel.clone());
    spawn_media_poll(runtime.clone(), cancel.clone());
    spawn_watchdog(runtime.clone(), cancel);
}

fn spawn_announce_tx(runtime: Arc<RemoteRuntime>, cancel: CancellationToken) {
    let period = Duration::from_secs(runtime.config.announce_secs.max(5) as u64);
    tokio::spawn(async move {
        // Small initial delay so the radio interface is attached.
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            let app_data = encode_announce(&runtime.announce_info());
            runtime
                .transport
                .lock()
                .await
                .send_announce(&runtime.destination, Some(&app_data))
                .await;
            // Jitter so co-located nodes do not announce in lock-step.
            let jitter = Duration::from_millis(rand::random::<u64>() % 2000);
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(period + jitter) => {}
            }
        }
    });
}

fn spawn_announce_rx(runtime: Arc<RemoteRuntime>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut rx = runtime.transport.lock().await.recv_announces().await;
        let name_hash = remote_destination_name().as_name_hash_slice().to_vec();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = rx.recv() => match recv {
                    Ok(announce) => {
                        let app_data = announce.app_data.as_slice();
                        if !proto::is_remote_announce(app_data) {
                            continue;
                        }
                        let desc = announce.destination.lock().await.desc;
                        if desc.name.as_name_hash_slice() != name_hash.as_slice() {
                            continue;
                        }
                        let Some(info) = decode_announce(app_data) else { continue };
                        if desc.identity.address_hash == runtime.identity_hash {
                            continue;
                        }
                        let is_new = runtime.nodes.observe_announce(&desc, &info, now_secs());
                        if is_new {
                            log::info!(
                                "remote: discovered node {} ({}) v{}",
                                info.codename,
                                desc.identity.address_hash,
                                proto::format_version(info.gateway_version)
                            );
                            runtime.push_event("discovered", Some(desc.identity.address_hash), "first announce");
                        }
                        runtime.notify_changed();
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }
    });
}

fn spawn_in_link_events(runtime: Arc<RemoteRuntime>, cancel: CancellationToken) {
    // Reticulum fans every link event (including all VPN traffic) through a
    // 16-slot broadcast channel. Anything slow on the consumer side lags and
    // silently drops events, so the broadcast consumer only filters and
    // hands our events to an unbounded queue processed by a second task.
    // Two queues: control (requests, link state) ahead of bulk chunks, so a
    // status poll is answered promptly even with a backlog of chunks.
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
    let (bulk_tx, mut bulk_rx) = tokio::sync::mpsc::unbounded_channel();
    {
        let runtime = runtime.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut rx = runtime.transport.lock().await.in_link_events();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    recv = rx.recv() => match recv {
                        Ok(event) => {
                            if event.address_hash != runtime.destination_hash {
                                continue;
                            }
                            let is_chunk = matches!(
                                &event.event,
                                LinkEvent::Data(payload)
                                    if matches!(payload.as_slice().first(), Some(&proto::KIND_CHUNK) | Some(&proto::KIND_PARITY))
                            );
                            let _ = if is_chunk { bulk_tx.send(event) } else { control_tx.send(event) };
                        }
                        Err(RecvError::Lagged(n)) => {
                            log::warn!("remote: in-link event stream lagged by {n} events");
                            continue;
                        }
                        Err(_) => break,
                    }
                }
            }
        });
    }
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                recv = control_rx.recv() => recv,
                recv = bulk_rx.recv() => recv,
            };
            {
                match event {
                    Some(event) => match event.event {
                        LinkEvent::Activated => {
                            log::debug!("remote: in-link {} activated", event.id);
                            runtime
                                .in_sessions
                                .lock()
                                .insert(event.id, InSession::new());
                        }
                        LinkEvent::RemoteIdentified(identity) => {
                            let hash = identity.address_hash;
                            let paired = runtime.paired.read().contains_key(&hash);
                            log::info!(
                                "remote: in-link {} identified as {hash} paired={paired}",
                                event.id
                            );
                            runtime
                                .in_sessions
                                .lock()
                                .entry(event.id)
                                .or_insert_with(InSession::new)
                                .remote = Some(*identity);
                            runtime.notify_changed();
                        }
                        LinkEvent::Data(payload) => {
                            // Commands that can run for seconds (a shell
                            // command, a plugin install) leave this task so
                            // they cannot hold up other links. Everything
                            // else is answered in arrival order: on a
                            // single-core node a spawned reply would starve
                            // behind queued chunk writes.
                            if is_slow_request(payload.as_slice()) {
                                let runtime = runtime.clone();
                                let id = event.id;
                                let payload = payload.as_slice().to_vec();
                                tokio::spawn(async move {
                                    runtime.handle_in_data(id, &payload).await;
                                });
                            } else {
                                runtime.handle_in_data(event.id, payload.as_slice()).await;
                                // Chunk writes never await, so hand the
                                // worker thread back explicitly.
                                tokio::task::yield_now().await;
                            }
                        }
                        LinkEvent::Closed => {
                            log::debug!("remote: in-link {} closed", event.id);
                            runtime.drop_in_session(&event.id).await;
                            runtime.notify_changed();
                        }
                        LinkEvent::Proof(_) => {}
                    },
                    None => break,
                }
            }
        }
    });
}

fn spawn_out_link_events(runtime: Arc<RemoteRuntime>, cancel: CancellationToken) {
    let (queue_tx, mut queue_rx) = tokio::sync::mpsc::unbounded_channel();
    {
        let runtime = runtime.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let mut rx = runtime.transport.lock().await.out_link_events();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    recv = rx.recv() => match recv {
                        Ok(event) => {
                            let dest = event.address_hash;
                            let ours = runtime.out_sessions.lock().contains_key(&dest)
                                || runtime.nodes.identity_for_destination(&dest).is_some();
                            if ours {
                                let _ = queue_tx.send(event);
                            }
                        }
                        Err(RecvError::Lagged(n)) => {
                            log::warn!("remote: out-link event stream lagged by {n} events");
                            continue;
                        }
                        Err(_) => break,
                    }
                }
            }
        });
    }
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                recv = queue_rx.recv() => match recv {
                    Some(event) => {
                        let dest = event.address_hash;
                        match event.event {
                            LinkEvent::Activated => {
                                log::debug!("remote: out-link {} to {dest} activated", event.id);
                                // A (re)activated link has fresh keys: identify again.
                                let mut sessions = runtime.out_sessions.lock();
                                let session = sessions.entry(dest).or_insert_with(|| OutSession::new(now_secs()));
                                session.state = LinkState::Active;
                                session.link_id = Some(event.id);
                                session.identified = false;
                                drop(sessions);
                                runtime.notify_changed();
                            }
                            LinkEvent::Data(payload) => {
                                if let Some(Frame::Response { id, status, body }) = Frame::decode(payload.as_slice()) {
                                    let waiter = runtime
                                        .out_sessions
                                        .lock()
                                        .get_mut(&dest)
                                        .and_then(|session| session.waiters.remove(&id));
                                    match waiter {
                                        Some(tx) => { let _ = tx.send((status, body)); }
                                        None => log::debug!("remote: unexpected response id={id} from {dest}"),
                                    }
                                }
                            }
                            LinkEvent::Closed => {
                                log::debug!("remote: out-link to {dest} closed");
                                if let Some(policy) = runtime.link_policy.read().clone() {
                                    policy.clear(&event.id);
                                    policy.clear(&dest);
                                }
                                if let Some(session) = runtime.out_sessions.lock().get_mut(&dest) {
                                    session.state = LinkState::None;
                                    session.identified = false;
                                    session.link_id = None;
                                    session.fail_waiters();
                                }
                                runtime.notify_changed();
                            }
                            LinkEvent::Proof(_) | LinkEvent::RemoteIdentified(_) => {}
                        }
                    }
                    None => break,
                }
            }
        }
    });
}

fn spawn_media_poll(runtime: Arc<RemoteRuntime>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(MEDIA_POLL);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    let now = std::time::Instant::now();
                    let due: Vec<(AddressHash, Vec<Frame>)> = {
                        let mut senders = runtime.media.senders.lock();
                        if senders.is_empty() {
                            continue;
                        }
                        senders
                            .iter_mut()
                            .filter_map(|((node, _), sender)| {
                                let frames = sender.poll(now);
                                (!frames.is_empty()).then_some((*node, frames))
                            })
                            .collect()
                    };
                    for (node, frames) in due {
                        let _ = runtime.media_transmit(node, frames).await;
                    }
                    // Age out stale receive blocks even when nothing arrives.
                    let mut receivers = runtime.media.receivers.lock();
                    for receiver in receivers.values_mut() {
                        receiver.expire(now);
                    }
                    // Streams that stopped arriving leave nothing behind.
                    receivers.retain(|_, receiver| !receiver.is_idle(now, MEDIA_STREAM_IDLE));
                }
            }
        }
    });
}

fn spawn_watchdog(runtime: Arc<RemoteRuntime>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(WATCHDOG_SECS));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    let now = now_secs();

                    // Close idle out-links so the radio stays quiet.
                    let idle: Vec<AddressHash> = runtime
                        .out_sessions
                        .lock()
                        .iter()
                        .filter(|(_, s)| {
                            s.state == LinkState::Active
                                && s.waiters.is_empty()
                                && s.busy == 0
                                && now.saturating_sub(s.last_activity) >= runtime.config.link_idle_close_secs
                        })
                        .map(|(dest, _)| *dest)
                        .collect();
                    for dest in idle {
                        log::debug!("remote: closing idle link to {dest}");
                        runtime.close_out_link(&dest).await;
                    }

                    // Expire stale incoming pairing requests.
                    {
                        let expired: Vec<AddressHash> = {
                            let mut incoming = runtime.incoming.lock();
                            let expired: Vec<AddressHash> = incoming
                                .iter()
                                .filter(|(_, r)| now.saturating_sub(r.received_ts) >= INCOMING_REQUEST_TTL_SECS)
                                .map(|(hash, _)| *hash)
                                .collect();
                            for hash in &expired {
                                incoming.remove(hash);
                            }
                            expired
                        };
                        for hash in &expired {
                            runtime.forget_request(hash, PairingDirection::Incoming);
                        }
                        if !expired.is_empty() {
                            runtime.notify_changed();
                        }
                    }

                    // Drop abandoned spool files.
                    {
                        let mut receivers = runtime.receivers.lock().await;
                        let stale: Vec<(AddressHash, u8)> = receivers
                            .iter()
                            .filter(|(_, r)| now.saturating_sub(r.last_activity) > RECEIVER_IDLE_SECS)
                            .map(|(k, _)| *k)
                            .collect();
                        for key in stale {
                            if let Some(receiver) = receivers.remove(&key) {
                                receiver.discard().await;
                            }
                        }
                    }

                    // Re-send outstanding pairing requests while the target is
                    // online: a lost approval push is healed by the target
                    // answering OK to a repeated request.
                    let retry: Vec<(AddressHash, PairingRecord)> = {
                        let mut outgoing = runtime.outgoing.lock();
                        outgoing
                            .iter_mut()
                            .filter(|(node, r)| {
                                r.state == PairingState::Requested
                                    && r.retries < PAIR_RETRY_LIMIT
                                    && now.saturating_sub(r.ts) >= PAIR_RETRY_SECS
                                    && runtime.nodes.get(node).map(|n| n.online(now)).unwrap_or(false)
                            })
                            .map(|(node, r)| {
                                r.retries += 1;
                                r.ts = now;
                                (*node, outgoing_record(r))
                            })
                            .collect()
                    };
                    for (node, record) in retry {
                        let _ = runtime.store.save_request(&record);
                        let runtime = runtime.clone();
                        tokio::spawn(async move {
                            log::debug!("remote: re-sending pairing request to {node}");
                            let _ = runtime.request_pairing(node).await;
                        });
                    }

                    // Periodic refresh so "online" flags age out in the UI.
                    runtime.notify_changed();
                }
            }
        }
    });
}
