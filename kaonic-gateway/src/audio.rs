use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "linux")]
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutput {
    Speaker,
    Headphones,
}

impl AudioOutput {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "speaker" => Some(Self::Speaker),
            "headphones" => Some(Self::Headphones),
            _ => None,
        }
    }

    fn backend_label(self) -> &'static str {
        match self {
            Self::Speaker => "Mock",
            Self::Headphones => {
                #[cfg(target_os = "linux")]
                {
                    "ALSA Headphone"
                }
                #[cfg(not(target_os = "linux"))]
                {
                    "Mock"
                }
            }
        }
    }

    fn default_state(self) -> AudioControlState {
        match self {
            Self::Speaker => AudioControlState {
                volume: 75,
                muted: false,
            },
            Self::Headphones => AudioControlState {
                volume: 60,
                muted: false,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioControlState {
    pub volume: u8,
    pub muted: bool,
}

impl AudioControlState {
    pub fn validate(self) -> Result<Self, AudioError> {
        if self.volume > 100 {
            return Err(AudioError::InvalidVolume(self.volume));
        }

        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioControlSnapshot {
    pub volume: u8,
    pub muted: bool,
    pub backend: &'static str,
}

impl AudioControlSnapshot {
    fn new(state: AudioControlState, backend: &'static str) -> Self {
        Self {
            volume: state.volume,
            muted: state.muted,
            backend,
        }
    }
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("invalid volume {0}; expected 0-100")]
    InvalidVolume(u8),
    #[error("{0} state lock poisoned")]
    StatePoisoned(&'static str),
    #[error("blocking audio task failed: {0}")]
    TaskJoin(String),
    #[error("failed to execute `{command}`: {source}")]
    CommandIo {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{command}` failed: {message}")]
    CommandFailed { command: String, message: String },
    #[error("unexpected amixer output: {0}")]
    UnexpectedOutput(String),
}

#[derive(Debug)]
pub struct AudioService {
    speaker: Mutex<AudioControlState>,
    headphones_mock: Mutex<AudioControlState>,
}

impl Default for AudioService {
    fn default() -> Self {
        Self {
            speaker: Mutex::new(AudioOutput::Speaker.default_state()),
            headphones_mock: Mutex::new(AudioOutput::Headphones.default_state()),
        }
    }
}

impl AudioService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn read(&self, output: AudioOutput) -> Result<AudioControlSnapshot, AudioError> {
        match output {
            AudioOutput::Speaker => self.read_mock(&self.speaker, output.backend_label()),
            AudioOutput::Headphones => self.read_headphones().await,
        }
    }

    pub async fn write(
        &self,
        output: AudioOutput,
        state: AudioControlState,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let state = state.validate()?;

        match output {
            AudioOutput::Speaker => self.write_mock(&self.speaker, state, output.backend_label()),
            AudioOutput::Headphones => self.write_headphones(state).await,
        }
    }

    fn read_mock(
        &self,
        state: &Mutex<AudioControlState>,
        backend: &'static str,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let state = *state
            .lock()
            .map_err(|_| AudioError::StatePoisoned("mock audio"))?;
        Ok(AudioControlSnapshot::new(state, backend))
    }

    fn write_mock(
        &self,
        current: &Mutex<AudioControlState>,
        next: AudioControlState,
        backend: &'static str,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let mut current = current
            .lock()
            .map_err(|_| AudioError::StatePoisoned("mock audio"))?;
        *current = next;
        Ok(AudioControlSnapshot::new(*current, backend))
    }

    #[cfg(target_os = "linux")]
    async fn read_headphones(&self) -> Result<AudioControlSnapshot, AudioError> {
        let state = tokio::task::spawn_blocking(read_headphones_linux)
            .await
            .map_err(|err| AudioError::TaskJoin(err.to_string()))??;
        Ok(AudioControlSnapshot::new(
            state,
            AudioOutput::Headphones.backend_label(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    async fn read_headphones(&self) -> Result<AudioControlSnapshot, AudioError> {
        self.read_mock(
            &self.headphones_mock,
            AudioOutput::Headphones.backend_label(),
        )
    }

    #[cfg(target_os = "linux")]
    async fn write_headphones(
        &self,
        state: AudioControlState,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let state = tokio::task::spawn_blocking(move || write_headphones_linux(state))
            .await
            .map_err(|err| AudioError::TaskJoin(err.to_string()))??;
        Ok(AudioControlSnapshot::new(
            state,
            AudioOutput::Headphones.backend_label(),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    async fn write_headphones(
        &self,
        state: AudioControlState,
    ) -> Result<AudioControlSnapshot, AudioError> {
        self.write_mock(
            &self.headphones_mock,
            state,
            AudioOutput::Headphones.backend_label(),
        )
    }
}

#[cfg(target_os = "linux")]
fn read_headphones_linux() -> Result<AudioControlState, AudioError> {
    let stdout = run_amixer(["sget", "Headphone"])?;
    parse_amixer_state(&stdout)
}

#[cfg(target_os = "linux")]
fn write_headphones_linux(state: AudioControlState) -> Result<AudioControlState, AudioError> {
    let mute = if state.muted { "mute" } else { "unmute" };
    let volume = format!("{}%", state.volume);
    run_amixer(["sset", "Headphone", &volume, mute])?;
    read_headphones_linux()
}

#[cfg(target_os = "linux")]
fn run_amixer<const N: usize>(args: [&str; N]) -> Result<String, AudioError> {
    let command = format!("amixer {}", args.join(" "));
    let output = Command::new("amixer")
        .args(args)
        .output()
        .map_err(|source| AudioError::CommandIo {
            command: command.clone(),
            source,
        })?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AudioError::CommandFailed { command, message });
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if stdout.trim().is_empty() {
        return Err(AudioError::UnexpectedOutput(
            "amixer returned empty stdout".into(),
        ));
    }

    Ok(stdout)
}

#[cfg(target_os = "linux")]
fn parse_amixer_state(stdout: &str) -> Result<AudioControlState, AudioError> {
    let volume = stdout
        .split('[')
        .filter_map(|segment| segment.split_once("%]").map(|(value, _)| value.trim()))
        .filter_map(|value| value.parse::<u8>().ok())
        .next_back()
        .ok_or_else(|| AudioError::UnexpectedOutput(stdout.trim().to_string()))?;

    let muted = stdout
        .split('[')
        .filter_map(|segment| segment.split_once(']').map(|(value, _)| value.trim()))
        .filter_map(|value| match value {
            "on" => Some(false),
            "off" => Some(true),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| AudioError::UnexpectedOutput(stdout.trim().to_string()))?;

    AudioControlState { volume, muted }.validate()
}

#[cfg(test)]
mod tests {
    use super::{AudioControlState, AudioError};

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_amixer_on_state() {
        let stdout = r#"Simple mixer control 'Headphone',0
  Capabilities: pvolume pswitch
  Playback channels: Front Left - Front Right
  Limits: Playback 0 - 255
  Front Left: Playback 179 [70%] [-19.00dB] [on]
  Front Right: Playback 179 [70%] [-19.00dB] [on]
"#;

        let state = super::parse_amixer_state(stdout).expect("state should parse");
        assert_eq!(
            state,
            AudioControlState {
                volume: 70,
                muted: false
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_amixer_off_state() {
        let stdout = r#"Simple mixer control 'Headphone',0
  Front Left: Playback 128 [50%] [-32.00dB] [off]
"#;

        let state = super::parse_amixer_state(stdout).expect("state should parse");
        assert_eq!(
            state,
            AudioControlState {
                volume: 50,
                muted: true
            }
        );
    }

    #[test]
    fn reject_out_of_range_volume() {
        let err = AudioControlState {
            volume: 101,
            muted: false,
        }
        .validate()
        .expect_err("volume must be rejected");

        assert!(matches!(err, AudioError::InvalidVolume(101)));
    }
}
