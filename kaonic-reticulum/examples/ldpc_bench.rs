//! Per-code LDPC decode cost on the target CPU (clean channel).
use labrador_ldpc::LDPCCode;
use std::time::Instant;

fn bench(code: LDPCCode, iters: u32) {
    let k = code.k() / 8;
    let n = code.n() / 8;
    let data: Vec<u8> = (0..k).map(|i| (i * 31 % 251) as u8).collect();
    let mut encoded = vec![0u8; n];
    code.copy_encode(&data, &mut encoded);
    let mut out = vec![0u8; code.output_len()];
    let mut work = vec![0u8; code.decode_bf_working_len()];
    let t = Instant::now();
    let mut ok = true;
    for _ in 0..iters {
        let (check, _) = code.decode_bf(&encoded, &mut out, &mut work, 20);
        ok &= check;
    }
    let per = t.elapsed() / iters;
    let frames_per_806b = (806 + k - 1) / k; // codewords needed for one 806 B chunk
    println!(
        "{:?}: k={} B n={} B rate={:.2} decode {:?}/codeword -> {:?} per 806 B payload, {} B on air, ok={}",
        code, k, n, k as f64 / n as f64, per, per * frames_per_806b as u32, frames_per_806b * n, ok
    );
}

fn main() {
    for code in [
        LDPCCode::TC256,
        LDPCCode::TC512,
        LDPCCode::TM1280,
        LDPCCode::TM1536,
        LDPCCode::TM2048,
    ] {
        bench(code, 30);
    }
}
