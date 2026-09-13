//! Two runtimes talking over local UDP: discovery → pairing → RPC → blob
//! transfer → idle link close → recovery, plus the zero-trust deny path.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kaonic_remote::handler::{BoxFuture, Command, CommandHandler, RemoteError, Reply};
use kaonic_remote::protocol::{self, op, status, InfoBody, PluginInfoWire, RadioConfigWire};
use kaonic_remote::{
    JobState, LinkState, LocalInfo, MemoryTrustStore, PairingState, RemoteConfig, RemoteRuntime,
};
use parking_lot::Mutex;
use rand::rngs::OsRng;
use reticulum::hash::AddressHash;
use reticulum::identity::PrivateIdentity;
use reticulum::iface::udp::UdpInterface;
use reticulum::transport::{Transport, TransportConfig};
use tokio_util::sync::CancellationToken;

struct TestHandler {
    codename: String,
    radio_sets: Mutex<Vec<RadioConfigWire>>,
    blobs: Mutex<Vec<(String, Vec<u8>)>>,
}

impl CommandHandler for TestHandler {
    fn handle(&self, command: Command) -> BoxFuture<'_, Result<Reply, RemoteError>> {
        Box::pin(async move {
            match command {
                Command::Ping => Ok(Reply::Empty),
                Command::Info => Ok(Reply::Info(InfoBody {
                    codename: self.codename.clone(),
                    serial: "serial".into(),
                    gateway_version: "0.2.5".into(),
                    uptime_secs: 42,
                    protocol: protocol::PROTOCOL_VERSION,
                    radio_modules: 2,
                })),
                Command::RadioGet { module } => Ok(Reply::Radio(RadioConfigWire {
                    module,
                    freq_hz: 869_535_000,
                    spacing_hz: 200_000,
                    channel: 3,
                    mod_kind: protocol::mod_kind::OFDM,
                    mod_a: 6,
                    tx_power: 10,
                    ..Default::default()
                })),
                Command::RadioSet(config) => {
                    self.radio_sets.lock().push(config);
                    Ok(Reply::Detail("applied".into()))
                }
                Command::PluginList => Ok(Reply::Plugins(vec![PluginInfoWire {
                    id: "kaonic-audio-ptt".into(),
                    name: "PTT".into(),
                    version: "1.0.0".into(),
                    active: true,
                    enabled: true,
                }])),
                Command::ApplyBlob { name, path, .. } => {
                    let bytes = tokio::fs::read(&path)
                        .await
                        .map_err(|e| RemoteError::error(e.to_string()))?;
                    self.blobs.lock().push((name, bytes));
                    Ok(Reply::Detail("installed".into()))
                }
                other => Err(RemoteError::unsupported(op::name(other.op()))),
            }
        })
    }
}

struct Node {
    runtime: Arc<RemoteRuntime>,
    handler: Arc<TestHandler>,
    identity: AddressHash,
    transport: Arc<tokio::sync::Mutex<Transport>>,
}

async fn node(
    name: &str,
    bind: u16,
    forward: u16,
    cancel: CancellationToken,
    accept_pairing: bool,
) -> Node {
    node_with(name, bind, forward, cancel, accept_pairing, false).await
}

async fn node_with(
    name: &str,
    bind: u16,
    forward: u16,
    cancel: CancellationToken,
    accept_pairing: bool,
    auto_link: bool,
) -> Node {
    let id = PrivateIdentity::new_from_rand(OsRng);
    let transport = Transport::new(TransportConfig::new(name, &id));
    transport.iface_manager().lock().await.spawn(
        UdpInterface::new(
            format!("127.0.0.1:{bind}"),
            Some(format!("127.0.0.1:{forward}")),
            false,
        ),
        UdpInterface::spawn,
    );
    let transport = Arc::new(tokio::sync::Mutex::new(transport));
    let handler = Arc::new(TestHandler {
        codename: name.into(),
        radio_sets: Mutex::new(Vec::new()),
        blobs: Mutex::new(Vec::new()),
    });
    let config = RemoteConfig {
        announce_secs: 5,
        spool_dir: PathBuf::from(std::env::temp_dir())
            .join(format!("kaonic-remote-e2e-{name}-{}", std::process::id())),
        accept_pairing,
        auto_link,
        link_idle_close_secs: 4,
        rpc_timeout: Duration::from_secs(6),
        link_timeout: Duration::from_secs(12),
        chunk_gap: Duration::from_millis(20),
    };
    let runtime = RemoteRuntime::start(
        config,
        id.clone(),
        LocalInfo {
            codename: name.into(),
            gateway_version: "0.2.5".into(),
            serial: "serial".into(),
            services_digest: 0,
        },
        transport.clone(),
        handler.clone(),
        Arc::new(MemoryTrustStore::default()),
        cancel,
    )
    .await;
    Node {
        runtime,
        handler,
        identity: *id.address_hash(),
        transport,
    }
}

async fn wait_for<F: Fn() -> bool>(what: &str, secs: u64, f: F) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The `kaonic.remote` destination `node` knows for `identity`.
fn dest_of(node: &Node, identity: &AddressHash) -> AddressHash {
    AddressHash::new_from_hex_string(
        &node_state(&node.runtime, identity)
            .unwrap()
            .destination_hash,
    )
    .unwrap()
}

fn node_state(runtime: &RemoteRuntime, identity: &AddressHash) -> Option<kaonic_remote::NodeDto> {
    let hex = identity.to_hex_string();
    runtime
        .snapshot()
        .nodes
        .into_iter()
        .find(|n| n.identity_hash == hex)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_rpc_blob_and_recovery_over_udp() {
    let _ = env_logger::Builder::new()
        .parse_filters("kaonic_remote=debug,reticulum=warn")
        .is_test(true)
        .try_init();
    let cancel = CancellationToken::new();
    let a = node("nodea", 47101, 47102, cancel.clone(), true).await;
    let b = node("nodeb", 47102, 47101, cancel.clone(), true).await;

    // Discovery through announces.
    wait_for("mutual discovery", 20, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.online)
            .unwrap_or(false)
            && node_state(&b.runtime, &a.identity)
                .map(|n| n.online)
                .unwrap_or(false)
    })
    .await;
    let seen = node_state(&a.runtime, &b.identity).unwrap();
    assert_eq!(seen.codename, "nodeb");
    assert_eq!(seen.gateway_version, "0.2.5");

    // Zero trust: an unpaired node is refused by the target, not just locally.
    let (code, _) = a
        .runtime
        .call_raw(b.identity, op::PLUGIN_LIST, Vec::new(), None)
        .await
        .expect("rpc transport");
    assert_eq!(code, status::UNAUTHORIZED);
    assert!(a.runtime.plugin_list(b.identity).await.is_err());

    // Pairing: request lands as a pending approval on B with a matching SAS.
    let state = a.runtime.request_pairing(b.identity).await.unwrap();
    assert_eq!(state, PairingState::Requested);
    wait_for("incoming request on B", 10, || {
        !b.runtime.snapshot().incoming_requests.is_empty()
    })
    .await;
    let request = b.runtime.snapshot().incoming_requests.remove(0);
    assert_eq!(request.codename, "nodea");
    assert_eq!(
        request.sas,
        node_state(&a.runtime, &b.identity).unwrap().sas
    );
    assert_eq!(
        node_state(&b.runtime, &a.identity).unwrap().pairing,
        PairingState::Incoming
    );

    // Approval on B is pushed back to A → both paired.
    b.runtime.approve_pairing(a.identity).unwrap();
    wait_for("A learns it is paired", 25, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.paired)
            .unwrap_or(false)
    })
    .await;
    assert!(node_state(&b.runtime, &a.identity).unwrap().paired);

    // RPCs.
    let info = a.runtime.info(b.identity).await.unwrap();
    assert_eq!(info.codename, "nodeb");
    let radio = a.runtime.radio_get(b.identity, 1).await.unwrap();
    assert_eq!(radio.module, 1);
    let mut new_radio = radio.clone();
    new_radio.channel = 7;
    assert_eq!(
        a.runtime
            .radio_set(b.identity, new_radio.clone())
            .await
            .unwrap(),
        "applied"
    );
    assert_eq!(b.handler.radio_sets.lock().last().unwrap().channel, 7);
    let plugins = a.runtime.plugin_list(b.identity).await.unwrap();
    assert_eq!(plugins[0].id, "kaonic-audio-ptt");
    assert!(a.runtime.ping(b.identity).await.is_ok());

    // Idle link is closed to keep the radio quiet, then transparently re-opened.
    wait_for("idle link close", 20, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.link == LinkState::None)
            .unwrap_or(false)
    })
    .await;
    assert!(a.runtime.ping(b.identity).await.is_ok());
    assert_eq!(
        node_state(&a.runtime, &b.identity).unwrap().link,
        LinkState::Active
    );

    // Blob transfer (multi-window, out-of-order tolerant) applied on B.
    let payload: Vec<u8> = (0..30_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let job = a
        .runtime
        .push_blob(
            b.identity,
            protocol::blob::PLUGIN_PACKAGE,
            "kaonic-audio-ptt".into(),
            payload.clone(),
        )
        .unwrap();
    wait_for("blob job completion", 60, || {
        a.runtime
            .snapshot()
            .jobs
            .iter()
            .any(|j| j.id == job && matches!(j.state, JobState::Done | JobState::Failed))
    })
    .await;
    let done = a
        .runtime
        .snapshot()
        .jobs
        .into_iter()
        .find(|j| j.id == job)
        .unwrap();
    assert_eq!(done.state, JobState::Done, "job failed: {}", done.detail);
    assert_eq!(done.detail, "installed");
    let blobs = b.handler.blobs.lock();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].0, "kaonic-audio-ptt");
    assert_eq!(blobs[0].1, payload);
    drop(blobs);

    // Unpair propagates: B refuses A afterwards.
    a.runtime.unpair(b.identity).await.unwrap();
    wait_for("B drops pairing", 10, || {
        !node_state(&b.runtime, &a.identity).unwrap().paired
    })
    .await;
    assert!(a.runtime.info(b.identity).await.is_err());

    cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_is_rejected_when_target_disallows_it() {
    let cancel = CancellationToken::new();
    let a = node("nodec", 47201, 47202, cancel.clone(), true).await;
    let b = node("noded", 47202, 47201, cancel.clone(), false).await;
    wait_for("discovery", 20, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.online)
            .unwrap_or(false)
    })
    .await;
    assert!(!node_state(&a.runtime, &b.identity).unwrap().accepts_pairing);
    let state = a.runtime.request_pairing(b.identity).await.unwrap();
    assert_eq!(state, PairingState::Rejected);
    assert!(b.runtime.snapshot().incoming_requests.is_empty());
    cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blob_transfer_resumes_after_link_drop() {
    let _ = env_logger::Builder::new()
        .parse_filters("kaonic_remote=debug,reticulum=warn")
        .is_test(true)
        .try_init();
    let cancel = CancellationToken::new();
    let a = node("nodee", 47301, 47302, cancel.clone(), true).await;
    let b = node("nodef", 47302, 47301, cancel.clone(), true).await;
    wait_for("discovery", 20, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.online)
            .unwrap_or(false)
            && node_state(&b.runtime, &a.identity)
                .map(|n| n.online)
                .unwrap_or(false)
    })
    .await;
    a.runtime.request_pairing(b.identity).await.unwrap();
    wait_for("incoming", 10, || {
        !b.runtime.snapshot().incoming_requests.is_empty()
    })
    .await;
    b.runtime.approve_pairing(a.identity).unwrap();
    wait_for("paired", 25, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.paired)
            .unwrap_or(false)
    })
    .await;

    let payload: Vec<u8> = (0..160_000u32).map(|i| (i * 13 % 253) as u8).collect();
    let job = a
        .runtime
        .push_blob(
            b.identity,
            protocol::blob::PLUGIN_PACKAGE,
            String::new(),
            payload.clone(),
        )
        .unwrap();

    // Let a few windows through, then tear the link down from B's side (the
    // target closes its in-link, as a rebooting or stale peer would).
    wait_for("some progress", 20, || {
        a.runtime
            .snapshot()
            .jobs
            .iter()
            .any(|j| j.id == job && j.sent > 20_000)
    })
    .await;
    let dest = AddressHash::new_from_hex_string(
        &node_state(&a.runtime, &b.identity)
            .unwrap()
            .destination_hash,
    )
    .unwrap();
    let out_link = a
        .transport
        .lock()
        .await
        .find_out_link(&dest)
        .await
        .expect("out link");
    let link_id = *out_link.lock().await.id();
    b.transport.lock().await.link_close(link_id).await.unwrap();
    a.transport.lock().await.link_close(dest).await.unwrap();

    wait_for("job completion", 120, || {
        a.runtime
            .snapshot()
            .jobs
            .iter()
            .any(|j| j.id == job && matches!(j.state, JobState::Done | JobState::Failed))
    })
    .await;
    let done = a
        .runtime
        .snapshot()
        .jobs
        .into_iter()
        .find(|j| j.id == job)
        .unwrap();
    assert_eq!(done.state, JobState::Done, "job failed: {}", done.detail);
    let blobs = b.handler.blobs.lock();
    assert_eq!(blobs.len(), 1, "blob must be applied exactly once");
    assert_eq!(blobs[0].1, payload);
    cancel.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn media_stream_delivers_packets_to_sink() {
    use kaonic_remote::media::MediaConfig;
    let cancel = CancellationToken::new();
    let a = node("nodeg", 47401, 47402, cancel.clone(), true).await;
    let b = node("nodeh", 47402, 47401, cancel.clone(), true).await;
    wait_for("discovery", 20, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.online)
            .unwrap_or(false)
            && node_state(&b.runtime, &a.identity)
                .map(|n| n.online)
                .unwrap_or(false)
    })
    .await;
    a.runtime.request_pairing(b.identity).await.unwrap();
    wait_for("incoming", 10, || {
        !b.runtime.snapshot().incoming_requests.is_empty()
    })
    .await;
    b.runtime.approve_pairing(a.identity).unwrap();
    wait_for("paired", 25, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.paired)
            .unwrap_or(false)
    })
    .await;

    let received = Arc::new(Mutex::new(Vec::new()));
    {
        let received = received.clone();
        b.runtime.set_media_sink(Arc::new(move |from, packet| {
            received.lock().push((from, packet));
        }));
    }
    let mut media = MediaConfig::voice(7);
    // One packet per shard so the block/parity counts are deterministic.
    media.pack_window = Duration::ZERO;
    a.runtime.media_open(b.identity, media).await.unwrap();
    for i in 0..10u8 {
        let payload: Vec<u8> = (0..120).map(|j| (i as u32 * 7 + j) as u8 + 1).collect();
        a.runtime.media_send(b.identity, 7, &payload).await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let snap = a.runtime.snapshot();
    let out = snap.media.iter().find(|m| m.direction == "out").unwrap();
    assert_eq!(out.packets_sent, 10);
    assert!(out.parity_sent >= 2);
    a.runtime.media_close(b.identity, 7).await.unwrap();
    wait_for("media delivery", 10, || received.lock().len() >= 10).await;
    let got = received.lock();
    assert!(got
        .iter()
        .all(|(from, p)| *from == a.identity && p.stream == 7 && !p.recovered));
    assert_eq!(got.iter().filter(|(_, p)| p.block == 0).count(), 4);
    assert!(a
        .runtime
        .snapshot()
        .media
        .iter()
        .all(|m| m.direction != "out"));
    cancel.cancel();
}

/// With automatic links on, paired nodes link up without any command, the
/// link is kept through the idle-close window, and exactly one side (the
/// lower identity hash) initiates while both report the link as active.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paired_nodes_link_automatically_from_one_side() {
    let _ = env_logger::Builder::new()
        .parse_filters("kaonic_remote=debug,reticulum=warn")
        .is_test(true)
        .try_init();
    let cancel = CancellationToken::new();
    let a = node_with("nodei", 47501, 47502, cancel.clone(), true, true).await;
    let b = node_with("nodej", 47502, 47501, cancel.clone(), true, true).await;

    wait_for("mutual discovery", 20, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.online)
            .unwrap_or(false)
            && node_state(&b.runtime, &a.identity)
                .map(|n| n.online)
                .unwrap_or(false)
    })
    .await;
    // Not paired yet: nobody links on their own.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(a
        .transport
        .lock()
        .await
        .find_out_link(&dest_of(&a, &b.identity))
        .await
        .is_none());
    assert!(b
        .transport
        .lock()
        .await
        .find_out_link(&dest_of(&b, &a.identity))
        .await
        .is_none());

    a.runtime.request_pairing(b.identity).await.unwrap();
    wait_for("incoming request on B", 10, || {
        !b.runtime.snapshot().incoming_requests.is_empty()
    })
    .await;
    b.runtime.approve_pairing(a.identity).unwrap();
    wait_for("both paired", 25, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.paired)
            .unwrap_or(false)
            && node_state(&b.runtime, &a.identity)
                .map(|n| n.paired)
                .unwrap_or(false)
    })
    .await;

    // The pairing RPCs' link closes when idle; the automatic one replaces
    // it and both sides show it, whichever side holds the out-link.
    wait_for("automatic link on both sides", 40, || {
        node_state(&a.runtime, &b.identity)
            .map(|n| n.link == LinkState::Active)
            .unwrap_or(false)
            && node_state(&b.runtime, &a.identity)
                .map(|n| n.link == LinkState::Active)
                .unwrap_or(false)
    })
    .await;
    // Well past the idle-close window (4 s) the link is still there.
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(
        node_state(&a.runtime, &b.identity).unwrap().link,
        LinkState::Active
    );
    assert_eq!(
        node_state(&b.runtime, &a.identity).unwrap().link,
        LinkState::Active
    );

    // Exactly one out-link between the two, held by the lower identity hash.
    let (low, high) = if a.identity.as_slice() < b.identity.as_slice() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    let high_dest = dest_of(low, &high.identity);
    let low_dest = dest_of(high, &low.identity);
    assert!(low
        .transport
        .lock()
        .await
        .find_out_link(&high_dest)
        .await
        .is_some());
    assert!(high
        .transport
        .lock()
        .await
        .find_out_link(&low_dest)
        .await
        .is_none());

    // Commands from either side work over what is there.
    assert_eq!(
        low.runtime.info(high.identity).await.unwrap().codename,
        high.handler.codename
    );
    assert_eq!(
        high.runtime.info(low.identity).await.unwrap().codename,
        low.handler.codename
    );

    cancel.cancel();
}
