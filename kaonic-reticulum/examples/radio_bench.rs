//! On-device benchmark: LDPC encode/decode cost and raw radio TX pipeline
//! cost through kaonic-commd. Run on a node: `radio_bench [server_addr] [frames]`.

use std::time::{Duration, Instant};

use kaonic_ctrl::protocol::RADIO_FRAME_SIZE;
use kaonic_frame::frame::{Frame, FrameSegment};
use kaonic_net::coder::LdpcPacketCoder;
use kaonic_net::network::Network;
use kaonic_reticulum::KaonicCtrlInterface;
use rand::rngs::OsRng;
use tokio_util::sync::CancellationToken;

type Net = Network<RADIO_FRAME_SIZE, 3, 32, LdpcPacketCoder<RADIO_FRAME_SIZE>>;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let server: std::net::SocketAddr = args
        .get(1)
        .map(|s| s.parse().unwrap())
        .unwrap_or_else(|| "192.168.10.1:9090".parse().unwrap());
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let pace_ms: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);

    // ── LDPC CPU cost (single 800 B payload → 1 frame) ──────────────────
    let payload = vec![0x5a_u8; 806];
    let mut tx_net: Net = Network::new(LdpcPacketCoder::new());
    let mut rx_net: Net = Network::new(LdpcPacketCoder::new());
    let mut frames = [Frame::<RADIO_FRAME_SIZE>::new(); 3];
    let mut seg = FrameSegment::<RADIO_FRAME_SIZE, 3>::new();
    let iters = 50;
    let t = Instant::now();
    for _ in 0..iters {
        let _ = tx_net.transmit(&payload, OsRng, &mut frames).unwrap();
    }
    let enc = t.elapsed() / iters;
    let encoded = tx_net.transmit(&payload, OsRng, &mut frames).unwrap();
    println!(
        "ldpc: {} B payload -> {} frame(s) of {} B",
        payload.len(),
        encoded.len(),
        encoded[0].len()
    );
    let t = Instant::now();
    for i in 0..iters {
        let ts = i as u128;
        rx_net.receive(ts, &encoded[0]).unwrap();
        let _ = rx_net.process(ts, &mut seg);
    }
    let dec = t.elapsed() / iters;
    println!("ldpc: encode {:?}/frame  decode {:?}/frame", enc, dec);

    // ── Radio TX pipeline through commd ─────────────────────────────────
    let cancel = CancellationToken::new();
    let client = KaonicCtrlInterface::connect_client::<1400, 5>(
        "0.0.0.0:0".parse().unwrap(),
        server,
        cancel,
    )
    .await
    .expect("connect kaonic-ctrl");
    let frame = encoded[0];
    let mut min = Duration::MAX;
    let mut max = Duration::ZERO;
    let t = Instant::now();
    for _ in 0..count {
        let t1 = Instant::now();
        let (sent, errors) = client
            .lock()
            .await
            .transmit_batch(0, std::slice::from_ref(&frame))
            .await
            .unwrap();
        let d = t1.elapsed();
        min = min.min(d);
        max = max.max(d);
        if sent != 1 || errors != 0 {
            println!("tx error sent={sent} errors={errors}");
        }
        if pace_ms > 0 {
            tokio::time::sleep(Duration::from_millis(pace_ms)).await;
        }
    }
    let total = t.elapsed();
    println!(
        "radio tx: {count} x {} B frames in {:?} -> {:?}/frame (min {:?}, max {:?}) = {:.1} kbit/s payload",
        frame.len(),
        total,
        total / count as u32,
        min,
        max,
        (count * payload.len() * 8) as f64 / total.as_secs_f64() / 1000.0
    );
}
