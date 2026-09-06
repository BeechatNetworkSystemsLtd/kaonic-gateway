//! UDP bridge between local media applications (PTT, video plugins) and the
//! remote media transport.
//!
//! A *channel* pairs one paired node and a stream id with two local UDP
//! endpoints: datagrams arriving on `ingress` are sent to the node as media
//! shards, and shards received from that node on that stream are delivered
//! as datagrams to `egress`. Each datagram is one codec packet; the app keeps
//! its own jitter buffer and packet numbering.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use kaonic_remote::media::{MediaConfig, MediaPacket, MAX_MEDIA_PACKET};
use kaonic_remote::RemoteRuntime;
use parking_lot::Mutex;
use reticulum::hash::AddressHash;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaChannelSpec {
    pub stream: u8,
    /// Local UDP address the application sends to (e.g. 127.0.0.1:5004).
    pub ingress: SocketAddr,
    /// UDP address received packets are delivered to (e.g. 127.0.0.1:5006).
    pub egress: SocketAddr,
    /// "voice", "video" or "custom".
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default)]
    pub k: Option<u8>,
    #[serde(default)]
    pub m: Option<u8>,
    #[serde(default)]
    pub block_timeout_ms: Option<u64>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    /// Coalesce datagrams arriving within this window into one radio frame.
    #[serde(default)]
    pub pack_ms: Option<u64>,
}

fn default_profile() -> String {
    "voice".into()
}

/// The bridge exists for plugins running on this device. Binding a public
/// port or forwarding to an arbitrary host would turn a paired radio link
/// into an open relay, so both endpoints must be loopback.
fn require_loopback(what: &str, addr: SocketAddr) -> Result<(), String> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(format!("{what} must be a loopback address, got {addr}"))
    }
}

impl MediaChannelSpec {
    pub fn config(&self) -> Result<MediaConfig, String> {
        require_loopback("ingress", self.ingress)?;
        require_loopback("egress", self.egress)?;
        let mut config = match self.profile.as_str() {
            "voice" => MediaConfig::voice(self.stream),
            "video" => MediaConfig::video(self.stream),
            "custom" => MediaConfig::voice(self.stream),
            other => return Err(format!("unknown profile {other}")),
        };
        if let Some(k) = self.k {
            config.k = k;
        }
        if let Some(m) = self.m {
            config.m = m;
        }
        if let Some(ms) = self.block_timeout_ms {
            config.block_timeout = Duration::from_millis(ms.clamp(5, 2000));
        }
        if let Some(ms) = self.latency_ms {
            config.latency_budget = Duration::from_millis(ms.clamp(20, 5000));
        }
        if let Some(ms) = self.pack_ms {
            config.pack_window = Duration::from_millis(ms.min(500));
        }
        config.validate()?;
        Ok(config)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaChannelDto {
    pub node: String,
    pub stream: u8,
    pub ingress: SocketAddr,
    pub egress: SocketAddr,
    pub profile: String,
    pub k: u8,
    pub m: u8,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub dropped: u64,
}

struct Channel {
    spec: MediaChannelSpec,
    config: MediaConfig,
    cancel: CancellationToken,
    datagrams_in: Arc<std::sync::atomic::AtomicU64>,
    datagrams_out: Arc<std::sync::atomic::AtomicU64>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
}

pub struct MediaBridge {
    remote: Arc<RemoteRuntime>,
    channels: Mutex<HashMap<(AddressHash, u8), Channel>>,
    egress_socket: UdpSocket,
}

impl MediaBridge {
    pub async fn new(remote: Arc<RemoteRuntime>) -> std::io::Result<Arc<Self>> {
        let egress_socket = UdpSocket::bind("0.0.0.0:0").await?;
        let bridge = Arc::new(Self {
            remote: remote.clone(),
            channels: Mutex::new(HashMap::new()),
            egress_socket,
        });
        let sink_bridge = bridge.clone();
        remote.set_media_sink(Arc::new(move |from, packet| {
            sink_bridge.deliver(from, packet);
        }));
        Ok(bridge)
    }

    pub fn list(&self) -> Vec<MediaChannelDto> {
        use std::sync::atomic::Ordering;
        let mut out: Vec<MediaChannelDto> = self
            .channels
            .lock()
            .iter()
            .map(|((node, stream), ch)| MediaChannelDto {
                node: node.to_hex_string(),
                stream: *stream,
                ingress: ch.spec.ingress,
                egress: ch.spec.egress,
                profile: ch.spec.profile.clone(),
                k: ch.config.k,
                m: ch.config.m,
                datagrams_in: ch.datagrams_in.load(Ordering::Relaxed),
                datagrams_out: ch.datagrams_out.load(Ordering::Relaxed),
                dropped: ch.dropped.load(Ordering::Relaxed),
            })
            .collect();
        out.sort_by(|a, b| a.node.cmp(&b.node).then(a.stream.cmp(&b.stream)));
        out
    }

    pub async fn start(self: &Arc<Self>, node: AddressHash, spec: MediaChannelSpec) -> Result<(), String> {
        let config = spec.config()?;
        let key = (node, spec.stream);
        let cancel = CancellationToken::new();
        let channel = Channel {
            spec: spec.clone(),
            config,
            cancel: cancel.clone(),
            datagrams_in: Arc::default(),
            datagrams_out: Arc::default(),
            dropped: Arc::default(),
        };
        let datagrams_in = channel.datagrams_in.clone();
        let dropped = channel.dropped.clone();
        // Claim the slot before any await so two concurrent starts cannot
        // both bind and leak a socket.
        {
            let mut channels = self.channels.lock();
            if channels.contains_key(&key) {
                return Err("channel already open".into());
            }
            channels.insert(key, channel);
        }
        let started = async {
            let ingress = UdpSocket::bind(spec.ingress)
                .await
                .map_err(|err| format!("bind {}: {err}", spec.ingress))?;
            self.remote
                .media_open(node, config)
                .await
                .map_err(|err| err.to_string())?;
            Ok::<_, String>(ingress)
        }
        .await;
        let ingress = match started {
            Ok(ingress) => ingress,
            Err(err) => {
                self.channels.lock().remove(&key);
                return Err(err);
            }
        };

        let remote = self.remote.clone();
        let stream = spec.stream;
        tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            let mut buf = vec![0u8; 2048];
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    recv = ingress.recv_from(&mut buf) => match recv {
                        Ok((n, _)) => {
                            if n == 0 || n > MAX_MEDIA_PACKET {
                                dropped.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            datagrams_in.fetch_add(1, Ordering::Relaxed);
                            if remote.media_send(node, stream, &buf[..n]).await.is_err() {
                                dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(err) => {
                            log::warn!("media bridge: ingress recv: {err}");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        });
        log::info!(
            "media bridge: channel to {node} stream {stream} ingress {} egress {} ({}, {}+{})",
            spec.ingress,
            spec.egress,
            spec.profile,
            config.k,
            config.m
        );
        Ok(())
    }

    pub async fn stop(&self, node: AddressHash, stream: u8) -> Result<(), String> {
        let channel = self
            .channels
            .lock()
            .remove(&(node, stream))
            .ok_or_else(|| "channel not open".to_string())?;
        channel.cancel.cancel();
        self.remote
            .media_close(node, stream)
            .await
            .map_err(|err| err.to_string())
    }

    /// Packets received from `from` go to the egress of the matching channel.
    fn deliver(&self, from: AddressHash, packet: MediaPacket) {
        use std::sync::atomic::Ordering;
        let (egress, counter) = {
            let channels = self.channels.lock();
            let Some(channel) = channels.get(&(from, packet.stream)) else {
                return;
            };
            (channel.spec.egress, channel.datagrams_out.clone())
        };
        match self.egress_socket.try_send_to(&packet.data, egress) {
            Ok(_) => {
                counter.fetch_add(1, Ordering::Relaxed);
            }
            Err(err) => log::debug!("media bridge: egress {egress}: {err}"),
        }
    }
}
