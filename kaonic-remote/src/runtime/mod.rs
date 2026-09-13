//! Remote-control runtime: announces the local `kaonic.remote` destination,
//! tracks every remote node seen, and drives control sessions in both roles:
//!
//! * **controller** — opens an outbound link to a node, identifies on it and
//!   issues request/response RPCs (and blob pushes);
//! * **target** — accepts inbound links, and executes a request only when
//!   the link initiator has identified with a paired identity.
//!
//! Sessions are closed when idle so no keep-alive traffic is spent on the
//! radio while nobody is actively controlling anything.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex as PlMutex, RwLock as PlRwLock};
use reticulum::destination::link::LinkId;
use reticulum::destination::SingleInputDestination;
use reticulum::hash::AddressHash;
use reticulum::identity::PrivateIdentity;
use reticulum::packet::{DestinationType, Packet, PacketType};
use reticulum::transport::Transport;
use tokio::sync::{broadcast, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

use crate::handler::{CommandHandler, LinkClass, LinkPolicy, RemoteError};
use crate::nodes::{remote_destination_name, NodeRegistry};
use crate::protocol::{
    self as proto, decode_body, encode_body, status, AnnounceInfo, DetailBody,
    FLAG_ACCEPTS_PAIRING, FLAG_FEC_SELECT, PROTOCOL_VERSION,
};
use crate::transfer::BlobReceiver;
use crate::trust::{sas_code, PairedNode, PairingDirection, TrustStore};
use crate::types::{
    BlobJobDto, LinkState, LocalNodeDto, NodeDto, PairingRequestDto, PairingState, RemoteEventDto,
    RemoteSnapshot,
};

/// Max cached responses per inbound session (replay protection + retransmit).
const RESPONSE_CACHE: usize = 8;
/// Unauthorized requests tolerated on one link before it is torn down.
const MAX_UNAUTHORIZED_STRIKES: u8 = 3;
/// Pairing requests tolerated per link (each is an operator-visible event).
const MAX_PAIR_ATTEMPTS: u8 = 3;
const PAIR_ATTEMPT_MIN_GAP_SECS: u64 = 5;
/// Incoming pairing requests expire if nobody acts on them.
const INCOMING_REQUEST_TTL_SECS: u64 = 24 * 3600;
/// Cap on pending incoming requests so unknown identities cannot grow state
/// without bound; beyond this new requests are answered BUSY.
const MAX_INCOMING_REQUESTS: usize = 32;
/// A pending link is torn down and re-requested after this long.
const LINK_PENDING_TIMEOUT_SECS: u64 = 10;
const LINK_POLL_MILLIS: u64 = 200;
/// Receiver spool entries are discarded after this much silence.
const RECEIVER_IDLE_SECS: u64 = 10 * 60;
const WATCHDOG_SECS: u64 = 5;
const EVENT_BUF_SIZE: usize = 64;
/// Attempts to deliver an approval back to the requester.
const PAIR_NOTIFY_ATTEMPTS: u32 = 3;
const STALL_LIMIT: u32 = 6;
/// Link drops a blob job tolerates before giving up.
const MAX_BLOB_RECONNECTS: u32 = 6;
/// Per-try wait for a streaming status poll.
const STATUS_POLL_TIMEOUT: Duration = Duration::from_millis(1500);
/// RPC attempts per call (each after a fresh link if the previous one died).
const RPC_ATTEMPTS: u32 = 3;
/// While a pairing request is outstanding, re-send it at this cadence so a
/// lost approval notification heals without operator action.
const PAIR_RETRY_SECS: u64 = 120;
const PAIR_RETRY_LIMIT: u32 = 20;
/// Back-off after a failed automatic link attempt: doubles from here, capped
/// at [`AUTO_LINK_BACKOFF_MAX_SECS`]. A peer that announces but cannot be
/// linked (out of link range, asymmetric radio) is retried, not hammered.
const AUTO_LINK_BACKOFF_SECS: u64 = 30;
const AUTO_LINK_BACKOFF_MAX_SECS: u64 = 300;
/// An in-link whose initiator has not identified within this long is closed.
/// Identify is a single unacknowledged packet, so a lost one would otherwise
/// leave an anonymous link that keep-alives hold open indefinitely; closing
/// it makes the initiator re-link, and identify again.
const IDENTIFY_GRACE_SECS: u64 = 30;
/// How long a request on a not-yet-identified in-link waits for the identify
/// packet that may have been reordered behind it.
const IDENTIFY_WAIT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone)]
pub struct RemoteConfig {
    pub announce_secs: u32,
    pub spool_dir: PathBuf,
    pub accept_pairing: bool,
    /// Keep a link up to every paired node that is online, so commands are
    /// instant and "link active" on the map is a live reachability signal.
    /// One link per pair: the node with the lower identity hash initiates,
    /// the other side sees it as an in-link. Costs one keep-alive exchange
    /// per link per keep-alive period while idle.
    pub auto_link: bool,
    /// Idle out-links are closed after this many seconds without RPC traffic.
    pub link_idle_close_secs: u64,
    pub rpc_timeout: Duration,
    pub link_timeout: Duration,
    /// Pause between blob chunks so link control traffic can interleave.
    pub chunk_gap: Duration,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            announce_secs: 20,
            spool_dir: std::env::temp_dir().join("kaonic-remote"),
            accept_pairing: true,
            auto_link: true,
            link_idle_close_secs: 45,
            rpc_timeout: Duration::from_secs(12),
            link_timeout: Duration::from_secs(25),
            chunk_gap: Duration::from_millis(15),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalInfo {
    pub codename: String,
    pub gateway_version: String,
    pub serial: String,
    /// See [`proto::services_digest`]; advertised in every announce.
    pub services_digest: u32,
}

mod bulk;
mod control;
mod inbound;
mod media_hub;
mod pairing;
mod session;
mod tasks;

pub use media_hub::MediaSink;

use bulk::BlobJob;
use media_hub::MediaHub;
use pairing::{IncomingRequest, OutgoingRequest};
use session::{InSession, OutSession};
use tasks::spawn_tasks;

// ── Runtime ───────────────────────────────────────────────────────────────────

pub struct RemoteRuntime {
    identity: PrivateIdentity,
    identity_hash: AddressHash,
    destination: Arc<AsyncMutex<SingleInputDestination>>,
    destination_hash: AddressHash,
    transport: Arc<AsyncMutex<Transport>>,
    handler: Arc<dyn CommandHandler>,
    store: Arc<dyn TrustStore>,
    link_policy: PlRwLock<Option<Arc<dyn LinkPolicy>>>,
    config: RemoteConfig,
    local: PlRwLock<LocalInfo>,
    nodes: NodeRegistry,
    paired: PlRwLock<HashMap<AddressHash, PairedNode>>,
    incoming: PlMutex<HashMap<AddressHash, IncomingRequest>>,
    outgoing: PlMutex<HashMap<AddressHash, OutgoingRequest>>,
    in_sessions: PlMutex<HashMap<LinkId, InSession>>,
    out_sessions: PlMutex<HashMap<AddressHash, OutSession>>,
    /// Spooled inbound transfers keyed by (remote identity, transfer id) so a
    /// transfer resumes when its link is re-established.
    receivers: AsyncMutex<HashMap<(AddressHash, u8), BlobReceiver>>,
    jobs: PlMutex<BTreeMap<u32, BlobJob>>,
    next_job: AtomicU32,
    events: PlMutex<VecDeque<RemoteEventDto>>,
    changed: broadcast::Sender<()>,
    /// Live-tunable pause between blob chunks (ms); see [`Self::set_chunk_gap_ms`].
    chunk_gap_ms: AtomicU32,
    /// Parity shards per 16-chunk bulk block (0 = no outer code).
    bulk_parity: AtomicU32,
    /// Live copy of [`RemoteConfig::auto_link`].
    auto_link: AtomicBool,
    media: MediaHub,
}

impl RemoteRuntime {
    pub async fn start(
        config: RemoteConfig,
        identity: PrivateIdentity,
        local: LocalInfo,
        transport: Arc<AsyncMutex<Transport>>,
        handler: Arc<dyn CommandHandler>,
        store: Arc<dyn TrustStore>,
        cancel: CancellationToken,
    ) -> Arc<Self> {
        let destination = transport
            .lock()
            .await
            .add_destination(identity.clone(), remote_destination_name())
            .await;
        let destination_hash = destination.lock().await.desc.address_hash;
        let (changed, _) = broadcast::channel(16);

        let chunk_gap_ms = config.chunk_gap.as_millis() as u32;
        let auto_link = config.auto_link;
        let runtime = Arc::new(Self {
            identity_hash: *identity.address_hash(),
            identity,
            destination,
            destination_hash,
            transport: transport.clone(),
            handler,
            store,
            link_policy: PlRwLock::new(None),
            config,
            local: PlRwLock::new(local),
            nodes: NodeRegistry::new(),
            paired: PlRwLock::new(HashMap::new()),
            incoming: PlMutex::new(HashMap::new()),
            outgoing: PlMutex::new(HashMap::new()),
            in_sessions: PlMutex::new(HashMap::new()),
            out_sessions: PlMutex::new(HashMap::new()),
            receivers: AsyncMutex::new(HashMap::new()),
            jobs: PlMutex::new(BTreeMap::new()),
            next_job: AtomicU32::new(1),
            events: PlMutex::new(VecDeque::new()),
            changed,
            chunk_gap_ms: AtomicU32::new(chunk_gap_ms),
            bulk_parity: AtomicU32::new(u32::from(proto::BULK_BLOCK_M)),
            auto_link: AtomicBool::new(auto_link),
            media: MediaHub::default(),
        });

        // Restore trust and seed the registry so paired nodes are addressable
        // (and visible on the map) before their first announce.
        for node in runtime.store.load() {
            let Some(identity) = node.identity() else {
                log::warn!(
                    "remote: skipping paired node with invalid identity {}",
                    node.identity_hash
                );
                continue;
            };
            runtime.nodes.seed(identity, &node.codename);
            runtime.paired.write().insert(identity.address_hash, node);
        }
        // Pairings in flight survive restarts too: incoming ones still need
        // the operator's decision, outgoing ones keep auto-retrying.
        for record in runtime.store.load_requests() {
            let Some(identity) = record.identity() else {
                log::warn!(
                    "remote: skipping pairing record with invalid identity {}",
                    record.identity_hash
                );
                continue;
            };
            let hash = identity.address_hash;
            if runtime.paired.read().contains_key(&hash) {
                let _ = runtime
                    .store
                    .remove_request(&record.identity_hash, record.direction);
                continue;
            }
            runtime.nodes.seed(identity, &record.codename);
            match record.direction {
                PairingDirection::Incoming => {
                    runtime.incoming.lock().insert(
                        hash,
                        IncomingRequest {
                            identity,
                            codename: record.codename.clone(),
                            received_ts: record.ts,
                        },
                    );
                }
                PairingDirection::Outgoing => {
                    runtime.outgoing.lock().insert(
                        hash,
                        OutgoingRequest {
                            identity,
                            codename: record.codename.clone(),
                            state: PairingState::parse(&record.state)
                                .unwrap_or(PairingState::Requested),
                            detail: record.detail.clone(),
                            ts: record.ts,
                            retries: record.retries,
                        },
                    );
                }
            }
        }
        log::info!(
            "remote: destination {} identity {} paired={} codename={}",
            destination_hash,
            runtime.identity_hash,
            runtime.paired.read().len(),
            runtime.local.read().codename
        );

        // Spool files from a previous run cannot be resumed (the receiver
        // map is in memory), so start from a clean slate.
        clear_spool_dir(&runtime.config.spool_dir).await;

        spawn_tasks(&runtime, cancel);

        runtime
    }

    pub fn sibling_destination(
        &self,
        node: &AddressHash,
        app: &str,
        aspect: &str,
    ) -> Option<AddressHash> {
        self.nodes
            .get(node)
            .map(|entry| crate::nodes::sibling_destination(entry.identity, app, aspect))
    }

    pub fn identity_hash(&self) -> AddressHash {
        self.identity_hash
    }

    pub fn destination_hash(&self) -> AddressHash {
        self.destination_hash
    }

    /// Install the host's per-destination link tuning hook.
    pub fn set_link_policy(&self, policy: Arc<dyn LinkPolicy>) {
        *self.link_policy.write() = Some(policy);
    }

    pub(super) fn peer_fec_capable(&self, dest: &AddressHash) -> bool {
        self.nodes
            .identity_for_destination(dest)
            .and_then(|id| self.nodes.get(&id))
            .map(|entry| entry.flags & FLAG_FEC_SELECT != 0)
            .unwrap_or(false)
    }

    /// Apply the session's class to its current link id.
    pub(super) fn apply_link_class(&self, dest: &AddressHash) {
        let Some(policy) = self.link_policy.read().clone() else {
            return;
        };
        let (link_id, class) = {
            let sessions = self.out_sessions.lock();
            let Some(session) = sessions.get(dest) else {
                return;
            };
            (session.link_id, session.class)
        };
        let capable = self.peer_fec_capable(dest);
        if let Some(link_id) = link_id {
            policy.set_class(link_id, class, capable);
        }
        policy.set_class(*dest, class, capable);
    }

    /// Ask the radio layer to treat traffic to a node with `class` for the
    /// rest of the session (bulk transfers call this; it is not persisted).
    pub fn set_node_class(&self, node: AddressHash, class: LinkClass) -> Result<(), RemoteError> {
        let entry = self
            .nodes
            .get(&node)
            .ok_or_else(|| RemoteError::not_found("unknown node"))?;
        let dest = entry.destination;
        self.out_sessions
            .lock()
            .entry(dest)
            .or_insert_with(|| OutSession::new(now_secs()))
            .class = class;
        self.apply_link_class(&dest);
        Ok(())
    }

    /// Subscribe to "something changed" ticks for UI push updates.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }

    /// Pause between blob chunks. The receiver decodes LDPC on the CPU
    /// (~18 ms per frame on the STM32MP1), so pacing slightly above that
    /// avoids overrunning it — retransmits cost far more airtime than a gap.
    pub fn set_chunk_gap_ms(&self, ms: u32) {
        self.chunk_gap_ms.store(ms.clamp(0, 500), Ordering::Relaxed);
    }

    pub fn chunk_gap_ms(&self) -> u32 {
        self.chunk_gap_ms.load(Ordering::Relaxed)
    }

    /// Parity shards per bulk block (0..=8). More parity survives longer
    /// loss bursts at the cost of airtime; 2 suits a ~10 % random loss.
    pub fn set_bulk_parity(&self, m: u32) {
        self.bulk_parity.store(m.min(8), Ordering::Relaxed);
    }

    pub fn bulk_parity(&self) -> u32 {
        self.bulk_parity.load(Ordering::Relaxed)
    }

    /// Switch automatic links to paired nodes on or off (live). Turning it
    /// off lets existing links close when they go idle.
    pub fn set_auto_link(&self, enabled: bool) {
        self.auto_link.store(enabled, Ordering::Relaxed);
    }

    pub fn auto_link(&self) -> bool {
        self.auto_link.load(Ordering::Relaxed)
    }

    /// One automatic link per pair: the lower identity hash initiates.
    pub(super) fn initiates_link_to(&self, peer: &AddressHash) -> bool {
        self.identity_hash.as_slice() < peer.as_slice()
    }

    /// True when this node is responsible for keeping a link to `peer` up.
    pub(super) fn keeps_link_to(&self, peer: &AddressHash, now: u64) -> bool {
        self.auto_link()
            && self.initiates_link_to(peer)
            && self.paired.read().contains_key(peer)
            && self
                .nodes
                .get(peer)
                .map(|entry| entry.online(now))
                .unwrap_or(false)
    }

    pub fn set_codename(&self, codename: &str) {
        self.local.write().codename = codename.to_string();
        self.notify_changed();
    }

    /// Advertise a new plugin service directory digest. Peers compare it with
    /// what they have cached and ask for the directory only when it differs,
    /// so a changed directory costs one request per peer instead of a
    /// periodic poll from every node in the mesh.
    pub fn set_services_digest(&self, digest: u32) {
        self.local.write().services_digest = digest;
    }

    /// Feed raw radio RX metrics (called from the interface RX observer).
    pub fn observe_packet(&self, rssi: i8, packet: &Packet) {
        let now = now_secs();
        match packet.header.packet_type {
            PacketType::Announce => {
                self.nodes.observe_metrics(
                    &packet.destination,
                    packet.header.hops.saturating_add(1),
                    rssi,
                    now,
                );
            }
            PacketType::Data if packet.header.destination_type == DestinationType::Link => {
                let identity_hash = {
                    let sessions = self.in_sessions.lock();
                    sessions
                        .get(&packet.destination)
                        .and_then(|session| session.remote.as_ref())
                        .map(|identity| identity.address_hash)
                }
                .or_else(|| {
                    let sessions = self.out_sessions.lock();
                    sessions
                        .iter()
                        .find(|(_, session)| session.link_id == Some(packet.destination))
                        .and_then(|(dest, _)| self.nodes.identity_for_destination(dest))
                });
                if let Some(identity_hash) = identity_hash {
                    self.nodes.observe_rssi(&identity_hash, rssi, now);
                }
            }
            _ => {}
        }
    }

    // ── Snapshot ─────────────────────────────────────────────────────────────

    pub fn snapshot(&self) -> RemoteSnapshot {
        let now = now_secs();
        let local = self.local.read().clone();
        let paired = self.paired.read().clone();
        let incoming = self.incoming.lock().clone();
        let outgoing = self.outgoing.lock().clone();
        let links: HashMap<AddressHash, LinkState> = self
            .out_sessions
            .lock()
            .iter()
            .map(|(dest, session)| (*dest, session.state))
            .collect();
        // A link the peer opened to us is just as much a link: with one
        // automatic link per pair, half the nodes only ever see the in-link.
        let linked_in: std::collections::HashSet<AddressHash> = self
            .in_sessions
            .lock()
            .values()
            .filter_map(|session| session.remote.as_ref().map(|id| id.address_hash))
            .collect();

        let mut nodes: Vec<NodeDto> = self
            .nodes
            .all(now)
            .into_iter()
            .filter(|entry| entry.identity_hash != self.identity_hash)
            .map(|entry| {
                let paired_node = paired.get(&entry.identity_hash);
                let (pairing, pairing_detail) = if paired_node.is_some() {
                    (PairingState::Paired, String::new())
                } else if incoming.contains_key(&entry.identity_hash) {
                    (PairingState::Incoming, "awaiting your approval".into())
                } else if let Some(request) = outgoing.get(&entry.identity_hash) {
                    (request.state, request.detail.clone())
                } else {
                    (PairingState::None, String::new())
                };
                NodeDto {
                    identity_hash: entry.identity_hash.to_hex_string(),
                    destination_hash: entry.destination.to_hex_string(),
                    codename: entry.codename.clone(),
                    gateway_version: proto::format_version(entry.gateway_version),
                    protocol: entry.protocol,
                    hops: entry.hops,
                    rssi: entry.rssi,
                    last_seen_ts: entry.last_announce_ts,
                    online: entry.online(now),
                    paired: paired_node.is_some(),
                    pairing,
                    pairing_detail,
                    link: match links.get(&entry.destination).copied().unwrap_or_default() {
                        LinkState::None if linked_in.contains(&entry.identity_hash) => {
                            LinkState::Active
                        }
                        state => state,
                    },
                    sas: sas_code(&self.identity_hash, &entry.identity_hash),
                    permissions: paired_node.map(|node| node.permissions).unwrap_or(0),
                    accepts_pairing: entry.flags & FLAG_ACCEPTS_PAIRING != 0,
                    fec_capable: entry.flags & FLAG_FEC_SELECT != 0,
                    services_digest: entry.services_digest,
                    // Filled in by the host, which knows the local database
                    // and the VPN.
                    tag: String::new(),
                    vpn_tunnel_ip: String::new(),
                    vpn_routes: Vec::new(),
                }
            })
            .collect();
        nodes.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| b.paired.cmp(&a.paired))
                .then_with(|| a.codename.cmp(&b.codename))
        });

        let mut incoming_requests: Vec<PairingRequestDto> = incoming
            .iter()
            .map(|(hash, request)| PairingRequestDto {
                identity_hash: hash.to_hex_string(),
                codename: request.codename.clone(),
                received_ts: request.received_ts,
                sas: sas_code(&self.identity_hash, hash),
            })
            .collect();
        incoming_requests.sort_by_key(|request| std::cmp::Reverse(request.received_ts));

        let jobs = self
            .jobs
            .lock()
            .values()
            .map(|job| BlobJobDto {
                id: job.id,
                node: job.node.to_hex_string(),
                codename: job.codename.clone(),
                name: job.name.clone(),
                purpose: job.purpose,
                size: job.size,
                sent: job.sent,
                state: job.state,
                detail: job.detail.clone(),
                started_ts: job.started_ts,
                updated_ts: job.updated_ts,
            })
            .collect();

        RemoteSnapshot {
            local: LocalNodeDto {
                identity_hash: self.identity_hash.to_hex_string(),
                identity_hex: self.identity.as_identity().to_hex_string(),
                destination_hash: self.destination_hash.to_hex_string(),
                codename: local.codename,
                gateway_version: local.gateway_version,
                announce_secs: self.config.announce_secs,
                accepts_pairing: self.config.accept_pairing,
            },
            nodes,
            incoming_requests,
            jobs,
            media: self.media_snapshot(),
            events: self.events.lock().iter().cloned().collect(),
        }
    }

    pub(super) fn push_event(
        &self,
        kind: &str,
        node: Option<AddressHash>,
        details: impl Into<String>,
    ) {
        let codename = node
            .and_then(|hash| self.nodes.get(&hash))
            .map(|entry| entry.codename)
            .unwrap_or_default();
        let mut events = self.events.lock();
        events.push_front(RemoteEventDto {
            ts: now_secs(),
            kind: kind.into(),
            node: node.map(|hash| hash.to_hex_string()).unwrap_or_default(),
            codename,
            details: details.into(),
        });
        events.truncate(EVENT_BUF_SIZE);
    }

    pub(super) fn notify_changed(&self) {
        let _ = self.changed.send(());
    }

    pub(super) fn announce_info(&self) -> AnnounceInfo {
        let local = self.local.read();
        AnnounceInfo {
            protocol: PROTOCOL_VERSION,
            flags: FLAG_FEC_SELECT
                | if self.config.accept_pairing {
                    FLAG_ACCEPTS_PAIRING
                } else {
                    0
                },
            codename: local.codename.clone(),
            gateway_version: proto::parse_version(&local.gateway_version),
            announce_secs: self.config.announce_secs.min(255) as u8,
            services_digest: local.services_digest,
        }
    }
}

fn ok_body<T: serde::Serialize>(value: &T) -> (u8, Vec<u8>) {
    (status::OK, encode_body(value))
}

fn err_body(code: u8, detail: &str) -> (u8, Vec<u8>) {
    (
        code,
        encode_body(&DetailBody {
            detail: detail.to_string(),
        }),
    )
}

fn detail_of(body: &[u8]) -> String {
    if body.is_empty() {
        return String::new();
    }
    decode_body::<DetailBody>(body)
        .map(|d| d.detail)
        .unwrap_or_default()
}

async fn clear_spool_dir(dir: &std::path::Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("part") {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

fn sanitize_codename(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(proto::CODENAME_LEN * 2)
        .collect();
    if cleaned.is_empty() {
        "unknown".into()
    } else {
        cleaned
    }
}

fn sanitize_name(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .take(64)
        .collect();
    // Never let a name start with a dot: no hidden files, no `..` path games
    // when the host turns the name into a path or URL segment.
    cleaned.trim_start_matches('.').to_string()
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
