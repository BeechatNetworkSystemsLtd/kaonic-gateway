use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::PluginConfig;

#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(target_os = "linux")]
use tokio::process::Command;

#[cfg(target_os = "linux")]
pub const CHANNELS: usize = 1;
#[cfg(target_os = "linux")]
const ALSA_STARTUP_PROBE_MS: u64 = 250;

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
struct PlaybackFormat {
    sample_rate_hz: u32,
    channels: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AudioDevices {
    pub capture: Vec<String>,
    pub playback: Vec<String>,
}

pub fn frame_samples(cfg: &PluginConfig) -> usize {
    ((cfg.sample_rate_hz as usize) * (cfg.frame_ms as usize)) / 1000
}

#[cfg(target_os = "linux")]
pub fn frame_bytes(cfg: &PluginConfig) -> usize {
    frame_samples(cfg) * CHANNELS * std::mem::size_of::<i16>()
}

pub async fn list_devices() -> AudioDevices {
    #[cfg(target_os = "linux")]
    {
        AudioDevices {
            capture: list_linux_devices("arecord", "-L").await,
            playback: list_linux_devices("aplay", "-L").await,
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        AudioDevices {
            capture: vec!["mock-capture".into()],
            playback: vec!["mock-playback".into()],
        }
    }
}

#[cfg(target_os = "linux")]
async fn list_linux_devices(command: &str, arg: &str) -> Vec<String> {
    let output = Command::new(command).arg(arg).output().await;
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                !line.trim().is_empty() && !line.starts_with(' ') && !line.starts_with('\t')
            })
            .map(|line| line.trim().to_string())
            .collect(),
        Ok(output) => {
            log::warn!(
                "{} {} failed: {}",
                command,
                arg,
                String::from_utf8_lossy(&output.stderr)
            );
            Vec::new()
        }
        Err(err) => {
            log::warn!("{command} {arg} failed: {err}");
            Vec::new()
        }
    }
}

pub async fn capture_loop(
    cfg: PluginConfig,
    frames: mpsc::Sender<Vec<i16>>,
    cancel: CancellationToken,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let bytes_per_frame = frame_bytes(&cfg);
        let (mut child, mut stdout, stderr_task, _) = spawn_validated_arecord(&cfg).await?;
        let mut buf = vec![0u8; bytes_per_frame];

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                read = stdout.read_exact(&mut buf) => {
                    read.map_err(|err| format!("read arecord frame: {err}"))?;
                    let pcm = buf
                        .chunks_exact(2)
                        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                        .collect::<Vec<_>>();
                    if frames.send(pcm).await.is_err() {
                        break;
                    }
                }
            }
        }

        let _ = child.start_kill();
        let _ = child.wait().await;
        if let Some(task) = stderr_task {
            let _ = task.await;
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let silence = vec![0i16; frame_samples(&cfg)];
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(cfg.frame_ms as u64)) => {
                    if frames.send(silence.clone()).await.is_err() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

pub async fn playback_loop(
    cfg: PluginConfig,
    mut frames: mpsc::Receiver<Vec<i16>>,
    cancel: CancellationToken,
    stats: std::sync::Arc<crate::Stats>,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let (mut child, mut stdin, stderr_task, _, playback_format) =
            spawn_validated_aplay(&cfg).await?;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                next = frames.recv() => {
                    let Some(frame) = next else { break; };
                    let sample_count = frame.len();
                    let raw = convert_playback_frame(
                        &frame,
                        cfg.sample_rate_hz,
                        playback_format.sample_rate_hz,
                        playback_format.channels,
                    );
                    stdin
                        .write_all(&raw)
                        .await
                        .map_err(|err| format!("write aplay frame: {err}"))?;
                    stats.record_played_frame(sample_count);
                }
            }
        }

        let _ = stdin.shutdown().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        if let Some(task) = stderr_task {
            let _ = task.await;
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = &cfg;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                next = frames.recv() => {
                    if let Some(frame) = next {
                        stats.record_played_frame(frame.len());
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn spawn_alsa_stderr_logger(
    program: &'static str,
    stderr: tokio::process::ChildStderr,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let message = line.trim();
            if !message.is_empty() {
                log::warn!("{program}: {message}");
            }
        }
    })
}

#[cfg(target_os = "linux")]
async fn spawn_validated_arecord(
    cfg: &PluginConfig,
) -> Result<
    (
        tokio::process::Child,
        tokio::process::ChildStdout,
        Option<tokio::task::JoinHandle<()>>,
        String,
    ),
    String,
> {
    let sample_rate = cfg.sample_rate_hz.to_string();
    let mut last_err = None;
    for device in alsa_device_candidates(&cfg.capture_device) {
        log::info!(
            "starting ALSA capture via arecord configured_device={} resolved_device={} format=S16_LE channels=1 rate={} frame_ms={}",
            cfg.capture_device,
            device,
            cfg.sample_rate_hz,
            cfg.frame_ms
        );
        let mut child = match Command::new("arecord")
            .arg("-q")
            .arg("-D")
            .arg(&device)
            .arg("-f")
            .arg("S16_LE")
            .arg("-c")
            .arg("1")
            .arg("-r")
            .arg(&sample_rate)
            .arg("-t")
            .arg("raw")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                last_err = Some(format!("spawn arecord for {device}: {err}"));
                continue;
            }
        };
        let stderr_task = child
            .stderr
            .take()
            .map(|stderr| spawn_alsa_stderr_logger("arecord", stderr));
        let Some(stdout) = child.stdout.take() else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            if let Some(task) = stderr_task {
                let _ = task.await;
            }
            last_err = Some(format!("arecord stdout unavailable for device {device}"));
            continue;
        };
        tokio::time::sleep(Duration::from_millis(ALSA_STARTUP_PROBE_MS)).await;
        match child.try_wait() {
            Ok(None) => return Ok((child, stdout, stderr_task, device)),
            Ok(Some(status)) => {
                log::warn!(
                    "arecord exited early for ALSA device={} status={}",
                    device,
                    status
                );
                if let Some(task) = stderr_task {
                    let _ = task.await;
                }
                last_err = Some(format!(
                    "arecord exited early for device {device} with status {status}"
                ));
            }
            Err(err) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                if let Some(task) = stderr_task {
                    let _ = task.await;
                }
                last_err = Some(format!("probe arecord on device {device}: {err}"));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "no usable ALSA capture device candidates".into()))
}

#[cfg(target_os = "linux")]
async fn spawn_validated_aplay(
    cfg: &PluginConfig,
) -> Result<
    (
        tokio::process::Child,
        tokio::process::ChildStdin,
        Option<tokio::task::JoinHandle<()>>,
        String,
        PlaybackFormat,
    ),
    String,
> {
    let mut last_err = None;
    for device in alsa_device_candidates(&cfg.playback_device) {
        for playback_format in playback_format_candidates(cfg) {
            log::info!(
                "starting ALSA playback via aplay configured_device={} resolved_device={} format=S16_LE channels={} rate={} frame_ms={} (received audio plays on local node sound card, not in browser)",
                cfg.playback_device,
                device,
                playback_format.channels,
                playback_format.sample_rate_hz,
                cfg.frame_ms
            );
            let mut child = match Command::new("aplay")
                .arg("-q")
                .arg("-D")
                .arg(&device)
                .arg("-f")
                .arg("S16_LE")
                .arg("-c")
                .arg(playback_format.channels.to_string())
                .arg("-r")
                .arg(playback_format.sample_rate_hz.to_string())
                .arg("-t")
                .arg("raw")
                .stdin(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(err) => {
                    last_err = Some(format!(
                        "spawn aplay for {device} rate={} channels={}: {err}",
                        playback_format.sample_rate_hz, playback_format.channels
                    ));
                    continue;
                }
            };
            let stderr_task = child
                .stderr
                .take()
                .map(|stderr| spawn_alsa_stderr_logger("aplay", stderr));
            let Some(stdin) = child.stdin.take() else {
                let _ = child.start_kill();
                let _ = child.wait().await;
                if let Some(task) = stderr_task {
                    let _ = task.await;
                }
                last_err = Some(format!("aplay stdin unavailable for device {device}"));
                continue;
            };
            let mut stdin = stdin;
            let probe_bytes = convert_playback_frame(
                &vec![0i16; frame_samples(cfg)],
                cfg.sample_rate_hz,
                playback_format.sample_rate_hz,
                playback_format.channels,
            );
            if let Err(err) = stdin.write_all(&probe_bytes).await {
                let _ = child.start_kill();
                let _ = child.wait().await;
                if let Some(task) = stderr_task {
                    let _ = task.await;
                }
                last_err = Some(format!(
                    "write playback probe to device {device} rate={} channels={}: {err}",
                    playback_format.sample_rate_hz, playback_format.channels
                ));
                continue;
            }
            tokio::time::sleep(Duration::from_millis(ALSA_STARTUP_PROBE_MS)).await;
            match child.try_wait() {
                Ok(None) => return Ok((child, stdin, stderr_task, device, playback_format)),
                Ok(Some(status)) => {
                    log::warn!(
                        "aplay exited early for ALSA device={} rate={} channels={} status={}",
                        device,
                        playback_format.sample_rate_hz,
                        playback_format.channels,
                        status
                    );
                    if let Some(task) = stderr_task {
                        let _ = task.await;
                    }
                    last_err = Some(format!(
                        "aplay exited early for device {device} rate={} channels={} with status {status}",
                        playback_format.sample_rate_hz,
                        playback_format.channels
                    ));
                }
                Err(err) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    if let Some(task) = stderr_task {
                        let _ = task.await;
                    }
                    last_err = Some(format!(
                        "probe aplay on device {device} rate={} channels={}: {err}",
                        playback_format.sample_rate_hz, playback_format.channels
                    ));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "no usable ALSA playback device candidates".into()))
}

#[cfg(target_os = "linux")]
fn playback_format_candidates(cfg: &PluginConfig) -> Vec<PlaybackFormat> {
    let candidates = vec![
        PlaybackFormat {
            sample_rate_hz: 48_000,
            channels: 2,
        },
        PlaybackFormat {
            sample_rate_hz: 48_000,
            channels: 1,
        },
        PlaybackFormat {
            sample_rate_hz: 32_000,
            channels: 2,
        },
        PlaybackFormat {
            sample_rate_hz: 32_000,
            channels: 1,
        },
        PlaybackFormat {
            sample_rate_hz: cfg.sample_rate_hz,
            channels: 1,
        },
    ];
    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.iter().any(|existing: &PlaybackFormat| {
            existing.sample_rate_hz == candidate.sample_rate_hz
                && existing.channels == candidate.channels
        }) {
            deduped.push(candidate);
        }
    }
    deduped
}

#[cfg(target_os = "linux")]
fn convert_playback_frame(
    input: &[i16],
    input_rate_hz: u32,
    output_rate_hz: u32,
    output_channels: usize,
) -> Vec<u8> {
    let resampled = resample_mono_pcm(input, input_rate_hz, output_rate_hz);
    let mut raw =
        Vec::with_capacity(resampled.len() * output_channels * std::mem::size_of::<i16>());
    for sample in resampled {
        for _ in 0..output_channels {
            raw.extend_from_slice(&sample.to_le_bytes());
        }
    }
    raw
}

#[cfg(target_os = "linux")]
fn resample_mono_pcm(input: &[i16], input_rate_hz: u32, output_rate_hz: u32) -> Vec<i16> {
    if input_rate_hz == output_rate_hz || input.is_empty() {
        return input.to_vec();
    }
    let output_len =
        ((input.len() as u64) * (output_rate_hz as u64) / (input_rate_hz as u64)).max(1) as usize;
    let mut output = Vec::with_capacity(output_len);
    for index in 0..output_len {
        let source_index =
            ((index as u64) * (input_rate_hz as u64) / (output_rate_hz as u64)) as usize;
        output.push(input[source_index.min(input.len() - 1)]);
    }
    output
}

#[cfg(target_os = "linux")]
fn alsa_device_candidates(configured: &str) -> Vec<String> {
    let configured = configured.trim();
    let configured = if configured.is_empty() {
        "default"
    } else {
        configured
    };
    let lower = configured.to_ascii_lowercase();
    let mut candidates = Vec::new();

    if configured.starts_with("plughw:") || configured.starts_with("plug:") {
        candidates.push(configured.to_string());
    } else if configured.starts_with("hw:") {
        candidates.push(format!("plughw:{}", &configured["hw:".len()..]));
        candidates.push(configured.to_string());
    } else if lower == "default" || lower.starts_with("default:") {
        candidates.push("plughw:0,0".to_string());
        candidates.push("sysdefault:CARD=Audio".to_string());
        candidates.push("default:CARD=Audio".to_string());
        candidates.push("plug:default".to_string());
        candidates.push("default".to_string());
        candidates.push("plug:sysdefault".to_string());
        candidates.push("sysdefault".to_string());
    } else if lower == "sysdefault" || lower.starts_with("sysdefault:") {
        candidates.push("plughw:0,0".to_string());
        candidates.push("sysdefault:CARD=Audio".to_string());
        candidates.push("default:CARD=Audio".to_string());
        candidates.push(format!("plug:{configured}"));
        candidates.push(configured.to_string());
        candidates.push("plug:default".to_string());
        candidates.push("default".to_string());
    } else if configured.contains(':') {
        candidates.push(format!("plug:{configured}"));
        candidates.push(configured.to_string());
    } else {
        candidates.push(configured.to_string());
        candidates.push(format!("plug:{configured}"));
    }

    let mut deduped = Vec::new();
    for candidate in candidates {
        if !deduped.contains(&candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}
