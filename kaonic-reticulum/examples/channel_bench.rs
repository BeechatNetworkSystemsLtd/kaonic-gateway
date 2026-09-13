//! On-device channel throughput benchmark. Measures what a channel actually
//! carries over the air, on one radio or bonded across both.
//!
//! Run the receiver first, then the sender:
//!
//! ```text
//! channel_bench rx <server> <modules> <seconds>
//! channel_bench tx <server> <modules> <payload_len> <count>
//! ```
//!
//! `modules` is a comma-separated list: `0` or `1` for one radio, `0,1` for a
//! bonded channel across both. Both ends must use the same list; the channel's
//! MTU depends on it and the far side has to agree.

use std::time::{Duration, Instant};

use kaonic_reticulum::channel::{
    profiles, ChannelEvent, ChannelId, CodingSpec, DispatchSpec, FecCode, ProfileSpec, Runtime,
    Tdd,
};
use kaonic_reticulum::Radio;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// A Robust-shaped profile with the coding swapped out, so a run differs from
/// the default only in which FEC code every frame carries.
fn fixed(code: FecCode) -> ProfileSpec {
    ProfileSpec::Custom {
        coding: CodingSpec::Fixed(code),
        tdd: tdd(),
        runtime: Runtime::default(),
        dispatch: DispatchSpec::RoundRobin,
    }
}

/// `KAONIC_BENCH_GAP_MS` inserts a listen gap after every six frames, to
/// throttle the offered rate and see whether a receiver that fails at full
/// rate is short of CPU or short of signal.
fn tdd() -> Tdd {
    match std::env::var("KAONIC_BENCH_GAP_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        Some(ms) => Tdd {
            burst_frames: 6,
            listen_gap: Duration::from_millis(ms),
            ..Tdd::default()
        },
        None => Tdd::default(),
    }
}

/// Derived, so both ends agree without exchanging anything. Not the default
/// channel, which carries Reticulum.
fn channel_id() -> ChannelId {
    // `KAONIC_BENCH_CHANNEL` names a different channel, so two benches can run
    // side by side on different radios without sharing one.
    let name = std::env::var("KAONIC_BENCH_CHANNEL")
        .unwrap_or_else(|_| "kaonic-channel-bench".to_string());
    ChannelId::of(&name)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let role = args.get(1).map(String::as_str).unwrap_or("rx");
    let server: std::net::SocketAddr = args
        .get(2)
        .map(|s| s.parse().expect("server addr"))
        .unwrap_or_else(|| "127.0.0.1:9090".parse().unwrap());
    let modules: Vec<usize> = args
        .get(3)
        .map(|s| {
            s.split(',')
                .filter_map(|m| m.trim().parse().ok())
                .collect::<Vec<usize>>()
        })
        .filter(|m: &Vec<usize>| !m.is_empty())
        .unwrap_or_else(|| vec![0]);

    let cancel = CancellationToken::new();
    let radio = Radio::connect(server, cancel.clone())
        .await
        .expect("connect to kaonic-commd");

    // Robust is what every node runs today: a fixed TM2048 code and no
    // bundling, so a single-radio run and a bonded run differ only in how many
    // radios carry the frames. `KAONIC_BENCH_FEC` swaps in a lighter code to
    // measure what the coding costs in CPU, SPI traffic and airtime.
    let profile: ProfileSpec = match std::env::var("KAONIC_BENCH_FEC").ok().as_deref() {
        Some("tm1536") => fixed(FecCode::Tm1536),
        Some("tm1280") => fixed(FecCode::Tm1280),
        Some("tc512") => fixed(FecCode::Tc512),
        Some("none") => fixed(FecCode::None),
        _ if std::env::var("KAONIC_BENCH_GAP_MS").is_ok() => fixed(FecCode::Tm2048),
        _ => profiles::Robust.into(),
    };

    match role {
        "tx" => {
            let payload_len: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let count: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(200);
            transmit(&radio, &modules, profile, payload_len, count).await;
        }
        "rx" => {
            let seconds: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(60);
            receive(&radio, &modules, profile, seconds).await;
        }
        other => {
            eprintln!("unknown role {other:?}; use tx or rx");
            std::process::exit(2);
        }
    }
}

async fn transmit(
    radio: &Radio,
    modules: &[usize],
    profile: ProfileSpec,
    payload_len: usize,
    count: usize,
) {
    let tx = radio
        .channel(channel_id())
        .await
        .modules(modules.iter().copied())
        .profile(profile)
        .build_tx()
        .await
        .expect("open channel");

    // A payload of exactly the MTU fills every segment, so the run measures
    // frames that are all full rather than a tail of short ones.
    let len = if payload_len == 0 {
        tx.mtu()
    } else {
        payload_len.min(tx.mtu())
    };
    let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();

    println!(
        "tx: modules {:?}, mtu {} B, frame capacity {} B, {} payloads of {} B",
        modules,
        tx.mtu(),
        tx.frame_capacity(),
        count,
        len
    );

    let start = Instant::now();
    for _ in 0..count {
        if let Err(err) = tx.send(&payload).await {
            eprintln!("send failed: {err}");
            break;
        }
    }
    let enqueued = start.elapsed();

    // `send` returns once the daemon accepts the payload, not once it is on
    // the air, so the last queue-depth worth of payloads is still pending.
    // Wait for the frame counter to stop moving before calling it done.
    let mut last = 0u64;
    let mut still = 0;
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let stats = match tx.stats().await {
            Ok(stats) => stats,
            Err(err) => {
                eprintln!("stats failed: {err}");
                break;
            }
        };
        if stats.tx_frames == last {
            still += 1;
            if still >= 5 {
                break;
            }
        } else {
            still = 0;
            last = stats.tx_frames;
        }
    }
    // Discount the idle time spent proving the queue had drained.
    let drained = start.elapsed() - Duration::from_millis(1000);

    let stats = tx.stats().await.expect("stats");
    let bytes = (count * len) as f64;
    println!(
        "tx: enqueued in {:?}, drained in {:?}",
        enqueued, drained
    );
    println!(
        "tx: {} payloads, {} frames, {} errors, {} dropped",
        stats.tx_payloads, stats.tx_frames, stats.tx_errors, stats.tx_dropped
    );
    println!(
        "tx: {:.0} B/s ({:.1} kbit/s) offered, {:.1} ms/frame",
        bytes / drained.as_secs_f64(),
        bytes * 8.0 / drained.as_secs_f64() / 1000.0,
        drained.as_secs_f64() * 1000.0 / stats.tx_frames.max(1) as f64
    );

    close(tx).await;
}

/// The daemon is told to close a channel by a message sent without waiting for
/// a reply, so the handle has to be dropped and the sender given a moment
/// before the process exits, or the channel lingers until its opener times out.
async fn close<T>(handle: T) {
    drop(handle);
    tokio::time::sleep(Duration::from_millis(300)).await;
}

/// Frames one radio delivered to the channel, split by whether they decoded.
/// The daemon's `ChannelStats` sum both radios, which hides the case a bonded
/// run is really after: one radio decoding cleanly while the other fails
/// nearly everything it hears.
#[derive(Clone, Copy, Default)]
struct ModuleFrames {
    received: u64,
    failed: u64,
}

/// One entry per module the channel was opened on, plus any other module the
/// daemon reported frames from, so an unexpected source is not hidden.
fn per_module_line(modules: &[usize], frames: &[ModuleFrames]) -> String {
    let parts: Vec<String> = frames
        .iter()
        .enumerate()
        .filter(|(m, f)| modules.contains(m) || f.received > 0 || f.failed > 0)
        .map(|(m, f)| format!("m{m} recv {} failed {}", f.received, f.failed))
        .collect();
    format!("rx per module: {}", parts.join(", "))
}

async fn receive(radio: &Radio, modules: &[usize], profile: ProfileSpec, seconds: u64) {
    let mut rx = radio
        .channel(channel_id())
        .await
        .modules(modules.iter().copied())
        .profile(profile)
        .build_rx()
        .await
        .expect("open channel");

    // Subscribed before listening starts so no frame event is missed; the
    // daemon reports failed decodes only this way, since a frame that does not
    // decode never becomes a payload.
    let mut events = rx.events();
    let mut events_open = true;

    println!("rx: modules {:?}, listening for {}s", modules, seconds);

    let deadline = Duration::from_secs(seconds);
    let mut payloads = 0u64;
    let mut bytes = 0usize;
    let mut per_module = [0u64; 8];
    let mut frames = [ModuleFrames::default(); 8];
    // Time from the first payload, not from startup: the sender is started by
    // hand, so the gap before it would otherwise count against the rate.
    let mut first: Option<Instant> = None;
    let mut last = Instant::now();
    let started = Instant::now();

    // Events are drained alongside payloads rather than after the run: the
    // event buffer holds 64 entries, and a radio failing every decode at full
    // rate fills that in under half a second of a quiet payload stream.
    while started.elapsed() < deadline {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(received) => {
                    if first.is_none() {
                        first = Some(Instant::now());
                    }
                    last = Instant::now();
                    payloads += 1;
                    bytes += received.payload.len();
                    let module = usize::from(received.info.module).min(per_module.len() - 1);
                    per_module[module] += 1;
                }
                Err(err) => {
                    eprintln!("rx: {err}");
                    break;
                }
            },
            event = events.recv(), if events_open => match event {
                Ok(ChannelEvent::Received { module, .. }) => {
                    frames[usize::from(module).min(frames.len() - 1)].received += 1;
                }
                Ok(ChannelEvent::DecodeFailed { module, .. }) => {
                    frames[usize::from(module).min(frames.len() - 1)].failed += 1;
                }
                Ok(_) => {}
                // Lost events mean the counts below undercount; say so rather
                // than print them as if they were exact.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("rx: event stream lagged, {n} frame event(s) uncounted");
                }
                Err(broadcast::error::RecvError::Closed) => events_open = false,
            },
            // Idle: the sender has not started, or has finished.
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if first.is_some() && last.elapsed() > Duration::from_secs(10) {
                    break;
                }
            }
        }
    }

    let elapsed = match first {
        Some(first) => last.duration_since(first),
        None => {
            println!("rx: nothing received");
            // The daemon's counters still say what happened to the frames.
            if let Ok(stats) = rx.stats().await {
                println!(
                    "rx: {} frames, {} payloads, decode clean {} corrected {} failed {}, duplicates {}, reassembly dropped {}",
                    stats.rx_frames,
                    stats.rx_payloads,
                    stats.decode_clean,
                    stats.decode_corrected,
                    stats.decode_failed,
                    stats.rx_duplicates,
                    stats.rx_reassembly_dropped
                );
            }
            println!("{}", per_module_line(modules, &frames));
            close(rx).await;
            return;
        }
    };
    let secs = elapsed.as_secs_f64().max(1e-9);

    println!(
        "rx: {} payloads, {} B in {:?}",
        payloads, bytes, elapsed
    );
    println!(
        "rx: {:.0} B/s ({:.1} kbit/s) goodput",
        bytes as f64 / secs,
        bytes as f64 * 8.0 / secs / 1000.0
    );
    let split: Vec<String> = per_module
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(m, n)| format!("module {m}: {n}"))
        .collect();
    println!("rx: last frame per payload came from {}", split.join(", "));

    let stats = rx.stats().await.expect("stats");
    println!(
        "rx: {} frames, {} payloads, decode clean {} corrected {} failed {}, duplicates {}, reassembly dropped {}",
        stats.rx_frames,
        stats.rx_payloads,
        stats.decode_clean,
        stats.decode_corrected,
        stats.decode_failed,
        stats.rx_duplicates,
        stats.rx_reassembly_dropped
    );
    println!("{}", per_module_line(modules, &frames));

    close(rx).await;
}
