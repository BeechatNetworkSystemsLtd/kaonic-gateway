use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::PluginConfig;

#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(target_os = "linux")]
use tokio::process::Command;

#[cfg(target_os = "linux")]
pub const CHANNELS: usize = 1;

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
        let sample_rate = cfg.sample_rate_hz.to_string();
        let mut child = Command::new("arecord")
            .arg("-q")
            .arg("-D")
            .arg(&cfg.capture_device)
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
            .map_err(|err| format!("spawn arecord: {err}"))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "arecord stdout unavailable".to_string())?;
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
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let sample_rate = cfg.sample_rate_hz.to_string();
        let mut child = Command::new("aplay")
            .arg("-q")
            .arg("-D")
            .arg(&cfg.playback_device)
            .arg("-f")
            .arg("S16_LE")
            .arg("-c")
            .arg("1")
            .arg("-r")
            .arg(&sample_rate)
            .arg("-t")
            .arg("raw")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("spawn aplay: {err}"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "aplay stdin unavailable".to_string())?;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                next = frames.recv() => {
                    let Some(frame) = next else { break; };
                    let mut raw = Vec::with_capacity(frame.len() * 2);
                    for sample in frame {
                        raw.extend_from_slice(&sample.to_le_bytes());
                    }
                    stdin
                        .write_all(&raw)
                        .await
                        .map_err(|err| format!("write aplay frame: {err}"))?;
                }
            }
        }

        let _ = stdin.shutdown().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = &cfg;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                next = frames.recv() => {
                    if next.is_none() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}
