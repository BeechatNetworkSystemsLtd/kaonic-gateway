//! Real-time media streams in both directions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex as PlMutex, RwLock as PlRwLock};
use reticulum::hash::AddressHash;

use crate::handler::{LinkClass, RemoteError};
use crate::media::{MediaConfig, MediaPacket, MediaReceiver, MediaSender, MediaStats};
use crate::protocol::{self as proto, status, Frame};
use crate::types::MediaStreamDto;

use super::*;

pub type MediaSink = Arc<dyn Fn(AddressHash, MediaPacket) + Send + Sync>;

/// Media streams in both directions. Senders are keyed by the target node's
/// identity and the stream id; receivers by the remote identity that sends.
#[derive(Default)]
pub struct MediaHub {
    pub(super) senders: PlMutex<HashMap<(AddressHash, u8), MediaSender>>,
    pub(super) receivers: PlMutex<HashMap<(AddressHash, u8), MediaReceiver>>,
    pub(super) sink: PlRwLock<Option<MediaSink>>,
}

/// Receiver-side repair window when the sender's budget is not known.
pub(super) const MEDIA_RX_BUDGET: Duration = Duration::from_millis(400);
/// How often partial media blocks are checked for their parity timeout.
pub(super) const MEDIA_POLL: Duration = Duration::from_millis(20);
/// A receive stream is forgotten after this long without a shard.
pub(super) const MEDIA_STREAM_IDLE: Duration = Duration::from_secs(30);

impl MediaHub {
    pub(super) async fn receive(&self, from: AddressHash, shard: proto::MediaShard) {
        let stream = shard.stream;
        let packets = {
            let mut receivers = self.receivers.lock();
            let receiver = receivers
                .entry((from, stream))
                .or_insert_with(|| MediaReceiver::new(MEDIA_RX_BUDGET));
            receiver.receive(shard, std::time::Instant::now())
        };
        if packets.is_empty() {
            return;
        }
        let sink = self.sink.read().clone();
        if let Some(sink) = sink {
            for packet in packets {
                sink(from, packet);
            }
        }
    }
}

impl RemoteRuntime {
    // ── Media streams ────────────────────────────────────────────────────────

    /// Where received media packets go (UDP bridge, plugin, ...).
    pub fn set_media_sink(&self, sink: MediaSink) {
        *self.media.sink.write() = Some(sink);
    }

    /// Open an outbound media stream to a paired node: brings the link up,
    /// asks the radio layer for the media class, and starts a sender.
    pub async fn media_open(
        self: &Arc<Self>,
        node: AddressHash,
        config: MediaConfig,
    ) -> Result<(), RemoteError> {
        if !self.paired.read().contains_key(&node) {
            return Err(RemoteError::new(status::UNAUTHORIZED, "node is not paired"));
        }
        let sender = MediaSender::new(config).map_err(RemoteError::bad_request)?;
        let desc = self.desc_for(&node)?;
        self.ensure_link(desc).await?;
        self.set_node_class(node, LinkClass::Media)?;
        self.media
            .senders
            .lock()
            .insert((node, config.stream), sender);
        self.push_event(
            "media-open",
            Some(node),
            format!("stream {}", config.stream),
        );
        self.notify_changed();
        Ok(())
    }

    /// Send one application packet (≤ [`crate::media::MAX_MEDIA_PACKET`] bytes).
    pub async fn media_send(
        self: &Arc<Self>,
        node: AddressHash,
        stream: u8,
        payload: &[u8],
    ) -> Result<(), RemoteError> {
        let frames = {
            let mut senders = self.media.senders.lock();
            let sender = senders
                .get_mut(&(node, stream))
                .ok_or_else(|| RemoteError::not_found("stream not open"))?;
            sender
                .push(payload, std::time::Instant::now())
                .map_err(RemoteError::bad_request)?
        };
        self.media_transmit(node, frames).await
    }

    pub async fn media_close(
        self: &Arc<Self>,
        node: AddressHash,
        stream: u8,
    ) -> Result<(), RemoteError> {
        let (frames, remaining) = {
            let mut senders = self.media.senders.lock();
            let mut sender = senders
                .remove(&(node, stream))
                .ok_or_else(|| RemoteError::not_found("stream not open"))?;
            let frames = sender.flush();
            (frames, senders.keys().any(|(n, _)| *n == node))
        };
        let _ = self.media_transmit(node, frames).await;
        if !remaining {
            let _ = self.set_node_class(node, LinkClass::Control);
        }
        self.push_event("media-close", Some(node), format!("stream {stream}"));
        self.notify_changed();
        Ok(())
    }

    pub(super) async fn media_transmit(
        self: &Arc<Self>,
        node: AddressHash,
        frames: Vec<Frame>,
    ) -> Result<(), RemoteError> {
        if frames.is_empty() {
            return Ok(());
        }
        let desc = self.desc_for(&node)?;
        let dest = desc.address_hash;
        // Media never waits for a handshake: if the link is gone, drop.
        for frame in frames {
            self.send_out(&dest, &frame.encode()).await?;
        }
        self.touch_out(&dest);
        Ok(())
    }

    pub(super) fn media_snapshot(&self) -> Vec<MediaStreamDto> {
        let mut out = Vec::new();
        let dto = |node: &AddressHash, stream: u8, direction: &str, k: u8, m: u8, s: MediaStats| {
            MediaStreamDto {
                node: node.to_hex_string(),
                codename: self.nodes.get(node).map(|e| e.codename).unwrap_or_default(),
                stream,
                direction: direction.into(),
                k,
                m,
                packets_sent: s.packets_sent,
                parity_sent: s.parity_sent,
                packets_received: s.packets_received,
                packets_recovered: s.packets_recovered,
                packets_lost: s.packets_lost,
                shards_late: s.shards_late,
            }
        };
        for ((node, stream), sender) in self.media.senders.lock().iter() {
            let cfg = sender.config();
            out.push(dto(node, *stream, "out", cfg.k, cfg.m, sender.stats));
        }
        for ((node, stream), receiver) in self.media.receivers.lock().iter() {
            out.push(dto(node, *stream, "in", 0, 0, receiver.stats));
        }
        out.sort_by(|a, b| a.codename.cmp(&b.codename).then(a.stream.cmp(&b.stream)));
        out
    }
}
