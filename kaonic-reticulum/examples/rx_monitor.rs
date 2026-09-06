//! On-device RX monitor: per-second radio frame counts and LDPC decode
//! outcomes, independent of the gateway. `rx_monitor [server] [seconds]`.
use std::time::{Duration, Instant};

use kaonic_ctrl::protocol::RADIO_FRAME_SIZE;
use kaonic_frame::frame::{Frame, FrameSegment};
use kaonic_net::coder::LdpcPacketCoder;
use kaonic_net::network::Network;
use kaonic_reticulum::KaonicCtrlInterface;
use tokio_util::sync::CancellationToken;

type Net = Network<RADIO_FRAME_SIZE, 3, 32, LdpcPacketCoder<RADIO_FRAME_SIZE>>;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let server: std::net::SocketAddr = args
        .get(1)
        .map(|s| s.parse().unwrap())
        .unwrap_or_else(|| "192.168.10.1:9090".parse().unwrap());
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
    let decode = args.get(3).map(|s| s != "nodecode").unwrap_or(true);
    let client = KaonicCtrlInterface::connect_client::<1400, 5>(
        "0.0.0.0:0".parse().unwrap(),
        server,
        CancellationToken::new(),
    )
    .await
    .expect("connect");
    // commd only fans RX frames out to clients it has heard from.
    client.lock().await.ping().await.expect("ping");
    let mut rx = client.lock().await.module_receive();
    let mut net: Net = Network::new(LdpcPacketCoder::new());
    let mut seg = FrameSegment::<RADIO_FRAME_SIZE, 3>::new();
    let start = Instant::now();
    let mut tick = Instant::now();
    let (mut frames, mut bytes, mut ok, mut bad, mut decode_ns, mut rssi_sum) =
        (0u32, 0usize, 0u32, 0u32, 0u128, 0i64);
    println!("sec frames bytes ldpc_ok ldpc_bad avg_decode_ms avg_rssi");
    while start.elapsed() < Duration::from_secs(seconds) {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(m)) if m.module == 0 => {
                frames += 1;
                bytes += m.frame.len as usize;
                rssi_sum += m.rssi as i64;
                let mut frame = Frame::<RADIO_FRAME_SIZE>::new();
                frame.copy_from_slice(m.frame.as_slice());
                let t = Instant::now();
                let ts = start.elapsed().as_millis();
                if decode {
                    match net.receive(ts, &frame) {
                        Ok(()) => {
                            ok += 1;
                            while net.process(ts, &mut seg).is_ok() {}
                        }
                        Err(_) => bad += 1,
                    }
                }
                decode_ns += t.elapsed().as_nanos();
            }
            Ok(Err(e)) => {
                println!("rx error {e:?}");
            }
            _ => {}
        }
        if tick.elapsed() >= Duration::from_secs(1) {
            println!(
                "{:>3} {:>6} {:>6} {:>7} {:>8} {:>13.1} {:>8.1}",
                start.elapsed().as_secs(),
                frames,
                bytes,
                ok,
                bad,
                if frames > 0 {
                    decode_ns as f64 / frames as f64 / 1e6
                } else {
                    0.0
                },
                if frames > 0 {
                    rssi_sum as f64 / frames as f64
                } else {
                    0.0
                }
            );
            frames = 0;
            bytes = 0;
            ok = 0;
            bad = 0;
            decode_ns = 0;
            rssi_sum = 0;
            tick = Instant::now();
        }
    }
}
