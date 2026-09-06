//! Outbound bulk transfer: chunk streaming with erasure-coded parity.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use reticulum::destination::DestinationDesc;
use reticulum::hash::AddressHash;

use crate::handler::{LinkClass, RemoteError};
use crate::nodes::NodeEntry;
use crate::protocol::{
    self as proto, decode_body, encode_body, op, status, BlobBeginBody, BlobBeginResponse,
    BlobRefBody, BlobStatusResponse, Frame, CHUNK_SIZE,
};
use crate::transfer::{sha256_of, BlobReceiver, MAX_IN_FLIGHT, WINDOW_CHUNKS};
use crate::types::JobState;

use super::session::RpcOptions;
use super::*;

pub(super) struct BlobJob {
    pub(super) id: u32,
    pub(super) node: AddressHash,
    pub(super) codename: String,
    pub(super) name: String,
    pub(super) purpose: u8,
    pub(super) size: u32,
    pub(super) sent: u32,
    pub(super) state: JobState,
    pub(super) detail: String,
    pub(super) started_ts: u64,
    pub(super) updated_ts: u64,
}

impl RemoteRuntime {
    pub fn push_blob(
        self: &Arc<Self>,
        node: AddressHash,
        purpose: u8,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<u32, RemoteError> {
        if !self.paired.read().contains_key(&node) {
            return Err(RemoteError::new(status::UNAUTHORIZED, "node is not paired"));
        }
        if bytes.is_empty() || bytes.len() as u64 > proto::MAX_BLOB_SIZE as u64 {
            return Err(RemoteError::bad_request("blob size out of range"));
        }
        let entry = self
            .nodes
            .get(&node)
            .ok_or_else(|| RemoteError::not_found("unknown node"))?;
        let already_running = self.jobs.lock().values().any(|job| {
            job.node == node
                && matches!(
                    job.state,
                    JobState::Starting | JobState::Transferring | JobState::Applying
                )
        });
        if already_running {
            return Err(RemoteError::new(
                status::BUSY,
                "a transfer to this node is already running",
            ));
        }
        let id = self.next_job.fetch_add(1, Ordering::Relaxed);
        let now = now_secs();
        self.jobs.lock().insert(
            id,
            BlobJob {
                id,
                node,
                codename: entry.codename.clone(),
                name: name.clone(),
                purpose,
                size: bytes.len() as u32,
                sent: 0,
                state: JobState::Starting,
                detail: String::new(),
                started_ts: now,
                updated_ts: now,
            },
        );
        // Keep the job list bounded.
        {
            let mut jobs = self.jobs.lock();
            while jobs.len() > 16 {
                let oldest_done = jobs
                    .iter()
                    .find(|(_, job)| matches!(job.state, JobState::Done | JobState::Failed))
                    .map(|(id, _)| *id);
                match oldest_done {
                    Some(id) => {
                        jobs.remove(&id);
                    }
                    None => break,
                }
            }
        }
        self.notify_changed();
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.run_blob_job(id, entry, purpose, name, bytes).await;
        });
        Ok(id)
    }

    pub(super) async fn run_blob_job(
        self: Arc<Self>,
        id: u32,
        entry: NodeEntry,
        purpose: u8,
        name: String,
        bytes: Vec<u8>,
    ) {
        let desc = entry.desc();
        let dest = desc.address_hash;
        self.adjust_busy(&dest, 1);
        let _ = self.set_node_class(entry.identity_hash, LinkClass::Bulk);
        let result = self.blob_job_inner(id, desc, purpose, name, bytes).await;
        let _ = self.set_node_class(entry.identity_hash, LinkClass::Control);
        self.adjust_busy(&dest, -1);
        match result {
            Ok(detail) => self.update_job(id, JobState::Done, detail, None),
            Err(err) => {
                log::warn!("remote: blob job {id} failed: {err}");
                self.update_job(id, JobState::Failed, err.to_string(), None);
            }
        }
    }

    pub(super) async fn blob_job_inner(
        self: &Arc<Self>,
        id: u32,
        desc: DestinationDesc,
        purpose: u8,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<String, RemoteError> {
        let dest = desc.address_hash;
        let size = bytes.len() as u32;
        let chunk = CHUNK_SIZE as u32;
        let total = BlobReceiver::total_chunks(size, chunk);
        let sha256 = sha256_of(&bytes).to_vec();
        let mut reconnects = 0u32;

        // Each pass renegotiates (BLOB_BEGIN resumes an existing spool)
        // and streams until complete; link failures restart the loop.
        'transfer: loop {
            let block_m = self.bulk_parity();
            let begin = self
                .call_desc(
                    desc,
                    op::BLOB_BEGIN,
                    encode_body(&BlobBeginBody {
                        purpose,
                        name: name.clone(),
                        size,
                        sha256: sha256.clone(),
                        chunk_size: chunk as u16,
                        block_k: proto::BULK_BLOCK_K,
                        block_m: block_m as u8,
                    }),
                    None,
                )
                .await;
            let begin = match begin {
                Ok((status::OK, body)) => {
                    decode_body::<BlobBeginResponse>(&body).map_err(RemoteError::error)?
                }
                Ok((code, body)) => return Err(RemoteError::new(code, detail_of(&body))),
                Err(err) => {
                    reconnects += 1;
                    if reconnects > MAX_BLOB_RECONNECTS {
                        return Err(RemoteError::error(format!(
                            "giving up after {reconnects} link failures: {err}"
                        )));
                    }
                    self.update_job(
                        id,
                        JobState::Transferring,
                        format!("reconnecting ({reconnects}): {err}"),
                        None,
                    );
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue 'transfer;
                }
            };
            self.update_job(
                id,
                JobState::Transferring,
                if begin.next_index > 0 {
                    format!("resumed at chunk {}", begin.next_index)
                } else {
                    String::new()
                },
                Some((begin.next_index * chunk).min(size)),
            );

            // Selective repeat: chunks stream at the pacing rate and a
            // status request every WINDOW_CHUNKS says only what to resend.
            // Sending stays within MAX_IN_FLIGHT of the confirmed position,
            // so a stalled receiver throttles the sender.
            let mut receiver_next = begin.next_index;
            let mut cursor = begin.next_index;
            let mut highest_sent: Option<u32> = None;
            let mut sent_at: HashMap<u32, std::time::Instant> = HashMap::new();
            let mut retransmit: VecDeque<u32> = VecDeque::new();
            let mut since_status = 0u32;
            let mut stalls = 0u32;
            let mut complete = begin.next_index >= total;
            let mut last_status_rtt;
            let (status_tx, mut status_rx) = tokio::sync::mpsc::channel::<
                Result<(BlobStatusResponse, Duration), RemoteError>,
            >(4);
            let mut status_in_flight = false;
            let mut link_error: Option<RemoteError> = None;
            let mut pending: VecDeque<Result<(BlobStatusResponse, Duration), RemoteError>> =
                VecDeque::new();
            // Outer erasure code: after the last data chunk of each block its
            // parity shards go out, so up to `m` lost frames per block are
            // rebuilt by the receiver with no round trip.
            let block_k = u32::from(proto::BULK_BLOCK_K);
            let coder = (block_m > 0)
                .then(|| {
                    crate::erasure::BlockCoder::new(proto::BULK_BLOCK_K as usize, block_m as usize)
                })
                .transpose()
                .map_err(|err| RemoteError::error(err.to_string()))?;
            let mut parity_queue: VecDeque<(u32, u8, Vec<u8>)> = VecDeque::new();
            let block_parity = |block: u32| -> Vec<Vec<u8>> {
                let shards: Vec<Vec<u8>> = (0..block_k)
                    .map(|slot| {
                        let index = block * block_k + slot;
                        if index >= total {
                            vec![0u8; chunk as usize]
                        } else {
                            let start = (index * chunk) as usize;
                            let end = ((index + 1) * chunk).min(size) as usize;
                            crate::erasure::pad_shard(&bytes[start..end], chunk as usize)
                        }
                    })
                    .collect();
                coder
                    .as_ref()
                    .map(|c| c.parity(&shards).unwrap_or_default())
                    .unwrap_or_default()
            };

            while !complete {
                // Absorb any status answers that arrived.
                while let Some(result) = pending.pop_front().or_else(|| status_rx.try_recv().ok()) {
                    status_in_flight = false;
                    match result {
                        Ok((next, rtt)) => {
                            last_status_rtt = rtt;
                            if next.complete {
                                complete = true;
                                receiver_next = total;
                                break;
                            }
                            if next.next_index == receiver_next {
                                stalls += 1;
                                if stalls >= STALL_LIMIT * 2 {
                                    return Err(RemoteError::error(
                                        "transfer stalled: target is not receiving chunks",
                                    ));
                                }
                            } else {
                                stalls = 0;
                            }
                            receiver_next = next.next_index;
                            sent_at.retain(|index, _| *index >= receiver_next);
                            // Everything the receiver reports missing below
                            // its high-water mark is a confirmed loss; below
                            // the mask horizon we also trust the mask. Only
                            // chunks that have had a round trip to arrive
                            // are re-queued, so in-flight ones are not sent
                            // twice.
                            let retx_after = last_status_rtt + Duration::from_millis(150);
                            let horizon = highest_sent.unwrap_or(0);
                            let mut missing: Vec<u32> = next.missing.clone();
                            if next.missing.is_empty()
                                && receiver_next <= horizon
                                && receiver_next < total
                            {
                                missing.push(receiver_next);
                            }
                            for bit in 0..32u32 {
                                let index = receiver_next + 1 + bit;
                                if index <= horizon
                                    && index < total
                                    && index >= next.highest
                                    && next.received_mask & (1 << bit) == 0
                                {
                                    missing.push(index);
                                }
                            }
                            for index in missing {
                                let due = sent_at
                                    .get(&index)
                                    .map(|at| at.elapsed() >= retx_after)
                                    .unwrap_or(true);
                                if due && !retransmit.contains(&index) {
                                    retransmit.push_back(index);
                                }
                            }
                            log::debug!(
                                "remote: blob job {id} status next={receiver_next} cursor={cursor} retx={} rtt={}ms",
                                retransmit.len(),
                                last_status_rtt.as_millis()
                            );
                            self.update_job(
                                id,
                                JobState::Transferring,
                                String::new(),
                                Some((receiver_next * chunk).min(size)),
                            );
                        }
                        Err(err) => {
                            link_error = Some(err);
                        }
                    }
                }
                if complete {
                    break;
                }
                if let Some(err) = link_error.take() {
                    reconnects += 1;
                    if reconnects > MAX_BLOB_RECONNECTS {
                        return Err(RemoteError::error(format!(
                            "giving up after {reconnects} link failures: {err}"
                        )));
                    }
                    self.update_job(
                        id,
                        JobState::Transferring,
                        format!("reconnecting ({reconnects}): {err}"),
                        None,
                    );
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue 'transfer;
                }

                // Pick what to send: retransmits first, then pending parity,
                // then new chunks while we are within the in-flight budget.
                let mut parity_to_send = None;
                let next_index = retransmit.pop_front().or_else(|| {
                    if let Some(p) = parity_queue.pop_front() {
                        parity_to_send = Some(p);
                        return None;
                    }
                    (cursor < total && cursor.saturating_sub(receiver_next) < MAX_IN_FLIGHT).then(
                        || {
                            let index = cursor;
                            cursor += 1;
                            index
                        },
                    )
                });
                if let Some((block, shard, data)) = parity_to_send {
                    if status_in_flight {
                        parity_queue.push_front((block, shard, data));
                    } else {
                        let frame = Frame::Parity {
                            transfer: begin.transfer,
                            block,
                            shard,
                            data,
                        }
                        .encode();
                        if let Err(err) = self.send_out(&dest, &frame).await {
                            link_error = Some(err);
                            continue;
                        }
                        since_status += 1;
                        self.touch_out(&dest);
                        tokio::time::sleep(Duration::from_millis(u64::from(self.chunk_gap_ms())))
                            .await;
                        continue;
                    }
                }

                let need_status = !status_in_flight
                    && (since_status >= WINDOW_CHUNKS
                        || (next_index.is_none() && highest_sent.is_some()));
                if need_status {
                    since_status = 0;
                    status_in_flight = true;
                    // Let the receiver drain its RX buffer before the poll:
                    // a frame right on the heels of a chunk is lost as often
                    // as any back-to-back frame on this radio.
                    tokio::time::sleep(Duration::from_millis(
                        u64::from(self.chunk_gap_ms()).max(40),
                    ))
                    .await;
                    let runtime = self.clone();
                    let status_tx = status_tx.clone();
                    let transfer = begin.transfer;
                    tokio::spawn(async move {
                        let started = std::time::Instant::now();
                        // Short, repeated tries: on a half-duplex radio the
                        // reply is routinely lost to our own transmissions.
                        let result = runtime
                            .call_desc_with(
                                desc,
                                op::BLOB_STATUS,
                                encode_body(&BlobRefBody { transfer }),
                                RpcOptions::status_poll(),
                            )
                            .await;
                        let result = match result {
                            Ok((status::OK, body)) => decode_body::<BlobStatusResponse>(&body)
                                .map(|s| (s, started.elapsed()))
                                .map_err(RemoteError::error),
                            Ok((status::NOT_FOUND, _)) => {
                                Err(RemoteError::not_found("transfer lost on target"))
                            }
                            Ok((code, body)) => Err(RemoteError::new(code, detail_of(&body))),
                            Err(err) => Err(err),
                        };
                        let _ = status_tx.send(result).await;
                    });
                }
                // Our radio cannot hear the reply while we transmit, and a
                // retransmitted poll suffers the same fate, so no chunks go
                // out while a poll is unanswered.
                if status_in_flight {
                    match tokio::time::timeout(STATUS_POLL_TIMEOUT * 8, status_rx.recv()).await {
                        Ok(Some(result)) => pending.push_back(result),
                        Ok(None) => return Err(RemoteError::error("status channel closed")),
                        Err(_) => {
                            link_error = Some(RemoteError::error("no status reply"));
                            status_in_flight = false;
                        }
                    }
                    if let Some(index) = next_index {
                        // Give back the chunk we picked but did not send.
                        if cursor == index + 1 {
                            cursor = index;
                        } else {
                            retransmit.push_front(index);
                        }
                    }
                    continue;
                }

                match next_index {
                    Some(index) => {
                        let start = (index * chunk) as usize;
                        let end = ((index + 1) * chunk).min(size) as usize;
                        let frame = Frame::Chunk {
                            transfer: begin.transfer,
                            index,
                            data: bytes[start..end].to_vec(),
                        }
                        .encode();
                        if let Err(err) = self.send_out(&dest, &frame).await {
                            link_error = Some(err);
                            continue;
                        }
                        sent_at.insert(index, std::time::Instant::now());
                        let first_send = highest_sent.is_none_or(|h| index > h);
                        highest_sent = Some(highest_sent.map_or(index, |h| h.max(index)));
                        if first_send && ((index + 1) % block_k == 0 || index + 1 == total) {
                            let block = index / block_k;
                            for (shard, data) in block_parity(block).into_iter().enumerate() {
                                parity_queue.push_back((block, shard as u8, data));
                            }
                        }
                        since_status += 1;
                        self.touch_out(&dest);
                        tokio::time::sleep(Duration::from_millis(u64::from(self.chunk_gap_ms())))
                            .await;
                    }
                    None => {
                        // Nothing to send until the receiver reports back.
                        match tokio::time::timeout(self.config.rpc_timeout * 2, status_rx.recv())
                            .await
                        {
                            Ok(Some(result)) => pending.push_back(result),
                            Ok(None) => return Err(RemoteError::error("status channel closed")),
                            Err(_) => {
                                link_error = Some(RemoteError::error("no status reply"));
                                status_in_flight = false;
                            }
                        }
                    }
                }
            }

            self.update_job(id, JobState::Applying, String::new(), Some(size));
            return match self
                .call_desc(
                    desc,
                    op::BLOB_END,
                    encode_body(&BlobRefBody {
                        transfer: begin.transfer,
                    }),
                    Some(Duration::from_secs(300)),
                )
                .await
            {
                Ok((status::OK, body)) => Ok(detail_of(&body)),
                // The retry inside call_desc replays a cached answer while the
                // link lives; if the link died after the target applied the
                // blob, the spool is gone and we cannot tell — say so.
                Ok((status::NOT_FOUND, _)) => Err(RemoteError::error(
                    "transfer completed but the final confirmation was lost; verify the plugin state",
                )),
                Ok((code, body)) => Err(RemoteError::new(code, detail_of(&body))),
                Err(err) => Err(err),
            };
        }
    }

    pub(super) fn adjust_busy(&self, dest: &AddressHash, delta: i32) {
        if let Some(session) = self.out_sessions.lock().get_mut(dest) {
            session.busy = (session.busy as i32 + delta).max(0) as u32;
        }
    }

    pub(super) fn update_job(&self, id: u32, state: JobState, detail: String, sent: Option<u32>) {
        if let Some(job) = self.jobs.lock().get_mut(&id) {
            job.state = state;
            job.updated_ts = now_secs();
            if !detail.is_empty() || matches!(state, JobState::Done | JobState::Failed) {
                job.detail = detail;
            }
            if let Some(sent) = sent {
                job.sent = sent;
            }
        }
        self.notify_changed();
    }

    // ── Internals: events ────────────────────────────────────────────────────
}
