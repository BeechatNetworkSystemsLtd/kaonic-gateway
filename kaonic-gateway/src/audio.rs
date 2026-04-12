use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioOutput {
    Speaker,
    Headphones,
    Microphone,
}

impl AudioOutput {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "speaker" => Some(Self::Speaker),
            "headphones" => Some(Self::Headphones),
            "microphone" => Some(Self::Microphone),
            _ => None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn backend_label(self) -> &'static str {
        match self {
            Self::Speaker | Self::Headphones | Self::Microphone => "Mock",
        }
    }

    #[cfg(not(target_os = "linux"))]
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
            Self::Microphone => AudioControlState {
                volume: 55,
                muted: false,
            },
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Speaker => "speaker",
            Self::Headphones => "headphones",
            Self::Microphone => "microphone",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Speaker => "Speaker",
            Self::Headphones => "Headphones",
            Self::Microphone => "Microphone",
        }
    }

    #[cfg(target_os = "linux")]
    fn preferred_controls(self) -> &'static [&'static str] {
        match self {
            Self::Speaker => &["Speaker", "Master", "PCM", "Line Out", "Playback"],
            Self::Headphones => &["Headphone", "Headphones", "Headset", "Headset Playback"],
            Self::Microphone => &[
                "Capture",
                "Mic",
                "Microphone",
                "Internal Mic",
                "Headset Mic",
            ],
        }
    }

    #[cfg(target_os = "linux")]
    fn fallback_fragments(self) -> &'static [&'static str] {
        match self {
            Self::Speaker => &["speaker", "master", "pcm", "playback", "line out"],
            Self::Headphones => &["head", "headphone", "headset"],
            Self::Microphone => &["mic", "capture"],
        }
    }

    #[cfg(target_os = "linux")]
    fn switch_tokens(self) -> &'static [&'static str; 2] {
        match self {
            Self::Speaker | Self::Headphones => &["unmute", "mute"],
            Self::Microphone => &["cap", "nocap"],
        }
    }

    #[cfg(target_os = "linux")]
    fn preferred_switch_controls(self) -> &'static [&'static str] {
        match self {
            Self::Speaker => &[
                "Speaker Playback Switch",
                "Master Playback Switch",
                "PCM Playback Switch",
                "Line Out Playback Switch",
            ],
            Self::Headphones => &[
                "Headphone Playback Switch",
                "Headphones Playback Switch",
                "Headset Playback Switch",
            ],
            Self::Microphone => &[
                "Capture Switch",
                "Mic Capture Switch",
                "Microphone Capture Switch",
                "Headset Mic Capture Switch",
            ],
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioControlSnapshot {
    pub control_id: String,
    pub label: String,
    pub volume: u8,
    pub muted: bool,
    pub backend: String,
}

impl AudioControlSnapshot {
    fn new(kind: AudioOutput, state: AudioControlState, backend: impl Into<String>) -> Self {
        Self {
            control_id: kind.id().into(),
            label: kind.label().into(),
            volume: state.volume,
            muted: state.muted,
            backend: backend.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioCardSnapshot {
    pub card_id: usize,
    pub card_name: String,
    pub controls: Vec<AudioControlSnapshot>,
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
    #[error("audio control not found: {0}")]
    NotFound(String),
}

#[derive(Debug)]
pub struct AudioService {
    #[cfg(not(target_os = "linux"))]
    speaker: Mutex<AudioControlState>,
    #[cfg(not(target_os = "linux"))]
    headphones_mock: Mutex<AudioControlState>,
    #[cfg(not(target_os = "linux"))]
    microphone_mock: Mutex<AudioControlState>,
    #[cfg(target_os = "linux")]
    soft_mute_levels: Mutex<HashMap<(usize, AudioOutput), u8>>,
}

impl Default for AudioService {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "linux"))]
            speaker: Mutex::new(AudioOutput::Speaker.default_state()),
            #[cfg(not(target_os = "linux"))]
            headphones_mock: Mutex::new(AudioOutput::Headphones.default_state()),
            #[cfg(not(target_os = "linux"))]
            microphone_mock: Mutex::new(AudioOutput::Microphone.default_state()),
            #[cfg(target_os = "linux")]
            soft_mute_levels: Mutex::new(HashMap::new()),
        }
    }
}

impl AudioService {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn read(&self, output: AudioOutput) -> Result<AudioControlSnapshot, AudioError> {
        match output {
            #[cfg(target_os = "linux")]
            AudioOutput::Speaker => self.read_control(0, output).await,
            #[cfg(not(target_os = "linux"))]
            AudioOutput::Speaker => self.read_mock(output, &self.speaker, output.backend_label()),
            AudioOutput::Headphones => self.read_control(0, output).await,
            AudioOutput::Microphone => self.read_control(0, output).await,
        }
    }

    pub async fn write(
        &self,
        output: AudioOutput,
        state: AudioControlState,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let state = state.validate()?;

        match output {
            #[cfg(target_os = "linux")]
            AudioOutput::Speaker => self.write_control(0, output, state).await,
            #[cfg(not(target_os = "linux"))]
            AudioOutput::Speaker => {
                self.write_mock(output, &self.speaker, state, output.backend_label())
            }
            AudioOutput::Headphones => self.write_control(0, output, state).await,
            AudioOutput::Microphone => self.write_control(0, output, state).await,
        }
    }

    pub async fn list_cards(&self) -> Result<Vec<AudioCardSnapshot>, AudioError> {
        #[cfg(target_os = "linux")]
        {
            tokio::task::spawn_blocking(list_audio_cards_linux)
                .await
                .map_err(|err| AudioError::TaskJoin(err.to_string()))?
        }

        #[cfg(not(target_os = "linux"))]
        {
            self.list_mock_cards()
        }
    }

    pub async fn read_control(
        &self,
        card_id: usize,
        output: AudioOutput,
    ) -> Result<AudioControlSnapshot, AudioError> {
        #[cfg(target_os = "linux")]
        {
            let mut snapshot =
                tokio::task::spawn_blocking(move || read_audio_control_linux(card_id, output))
                    .await
                    .map_err(|err| AudioError::TaskJoin(err.to_string()))??;
            if !self.control_has_switch_linux(card_id, output).await? && snapshot.volume == 0 {
                if let Some(saved_volume) = self.soft_mute_level(card_id, output)? {
                    snapshot.volume = saved_volume;
                    snapshot.muted = true;
                }
            }
            Ok(snapshot)
        }

        #[cfg(not(target_os = "linux"))]
        {
            self.read_mock_control(card_id, output)
        }
    }

    pub async fn write_control(
        &self,
        card_id: usize,
        output: AudioOutput,
        state: AudioControlState,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let state = state.validate()?;

        #[cfg(target_os = "linux")]
        {
            if self.control_has_switch_linux(card_id, output).await? {
                return tokio::task::spawn_blocking(move || {
                    write_audio_control_linux(card_id, output, state)
                })
                .await
                .map_err(|err| AudioError::TaskJoin(err.to_string()))?;
            }

            if state.muted {
                if state.volume > 0 {
                    self.set_soft_mute_level(card_id, output, state.volume)?;
                }
                tokio::task::spawn_blocking(move || {
                    write_audio_control_linux(
                        card_id,
                        output,
                        AudioControlState {
                            volume: 0,
                            muted: false,
                        },
                    )
                })
                .await
                .map_err(|err| AudioError::TaskJoin(err.to_string()))??;

                return Ok(AudioControlSnapshot::new(
                    output,
                    AudioControlState {
                        volume: self
                            .soft_mute_level(card_id, output)?
                            .unwrap_or(state.volume),
                        muted: true,
                    },
                    format!("ALSA {}", output.label()),
                ));
            }

            if state.volume > 0 {
                self.set_soft_mute_level(card_id, output, state.volume)?;
            } else {
                self.clear_soft_mute_level(card_id, output)?;
            }

            tokio::task::spawn_blocking(move || write_audio_control_linux(card_id, output, state))
                .await
                .map_err(|err| AudioError::TaskJoin(err.to_string()))?
        }

        #[cfg(not(target_os = "linux"))]
        {
            self.write_mock_control(card_id, output, state)
        }
    }

    pub async fn test_control(
        &self,
        card_id: usize,
        output: AudioOutput,
    ) -> Result<AudioControlSnapshot, AudioError> {
        #[cfg(target_os = "linux")]
        {
            tokio::task::spawn_blocking(move || test_audio_control_linux(card_id, output))
                .await
                .map_err(|err| AudioError::TaskJoin(err.to_string()))?
        }

        #[cfg(not(target_os = "linux"))]
        {
            self.read_mock_control(card_id, output)
        }
    }

    pub async fn save_card(&self, card_id: usize) -> Result<String, AudioError> {
        #[cfg(target_os = "linux")]
        {
            tokio::task::spawn_blocking(move || save_audio_card_linux(card_id))
                .await
                .map_err(|err| AudioError::TaskJoin(err.to_string()))?
        }

        #[cfg(not(target_os = "linux"))]
        {
            if card_id != 0 {
                return Err(AudioError::NotFound(format!("mock card {card_id}")));
            }
            Ok("Mock audio settings saved".into())
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn read_mock(
        &self,
        output: AudioOutput,
        state: &Mutex<AudioControlState>,
        backend: &'static str,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let state = *state
            .lock()
            .map_err(|_| AudioError::StatePoisoned("mock audio"))?;
        Ok(AudioControlSnapshot::new(output, state, backend))
    }

    #[cfg(not(target_os = "linux"))]
    fn write_mock(
        &self,
        output: AudioOutput,
        current: &Mutex<AudioControlState>,
        next: AudioControlState,
        backend: &'static str,
    ) -> Result<AudioControlSnapshot, AudioError> {
        let mut current = current
            .lock()
            .map_err(|_| AudioError::StatePoisoned("mock audio"))?;
        *current = next;
        Ok(AudioControlSnapshot::new(output, *current, backend))
    }

    #[cfg(target_os = "linux")]
    async fn control_has_switch_linux(
        &self,
        card_id: usize,
        output: AudioOutput,
    ) -> Result<bool, AudioError> {
        tokio::task::spawn_blocking(move || control_has_switch_linux(card_id, output))
            .await
            .map_err(|err| AudioError::TaskJoin(err.to_string()))?
    }

    #[cfg(target_os = "linux")]
    fn soft_mute_level(
        &self,
        card_id: usize,
        output: AudioOutput,
    ) -> Result<Option<u8>, AudioError> {
        self.soft_mute_levels
            .lock()
            .map(|levels| levels.get(&(card_id, output)).copied())
            .map_err(|_| AudioError::StatePoisoned("soft mute audio"))
    }

    #[cfg(target_os = "linux")]
    fn set_soft_mute_level(
        &self,
        card_id: usize,
        output: AudioOutput,
        volume: u8,
    ) -> Result<(), AudioError> {
        self.soft_mute_levels
            .lock()
            .map(|mut levels| {
                levels.insert((card_id, output), volume);
            })
            .map_err(|_| AudioError::StatePoisoned("soft mute audio"))
    }

    #[cfg(target_os = "linux")]
    fn clear_soft_mute_level(&self, card_id: usize, output: AudioOutput) -> Result<(), AudioError> {
        self.soft_mute_levels
            .lock()
            .map(|mut levels| {
                levels.remove(&(card_id, output));
            })
            .map_err(|_| AudioError::StatePoisoned("soft mute audio"))
    }

    #[cfg(not(target_os = "linux"))]
    fn list_mock_cards(&self) -> Result<Vec<AudioCardSnapshot>, AudioError> {
        Ok(vec![AudioCardSnapshot {
            card_id: 0,
            card_name: "Mock Audio".into(),
            controls: vec![
                self.read_mock_control(0, AudioOutput::Speaker)?,
                self.read_mock_control(0, AudioOutput::Headphones)?,
                self.read_mock_control(0, AudioOutput::Microphone)?,
            ],
        }])
    }

    #[cfg(not(target_os = "linux"))]
    fn read_mock_control(
        &self,
        card_id: usize,
        output: AudioOutput,
    ) -> Result<AudioControlSnapshot, AudioError> {
        if card_id != 0 {
            return Err(AudioError::NotFound(format!("mock card {card_id}")));
        }
        match output {
            AudioOutput::Speaker => self.read_mock(output, &self.speaker, output.backend_label()),
            AudioOutput::Headphones => {
                self.read_mock(output, &self.headphones_mock, output.backend_label())
            }
            AudioOutput::Microphone => {
                self.read_mock(output, &self.microphone_mock, output.backend_label())
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn write_mock_control(
        &self,
        card_id: usize,
        output: AudioOutput,
        state: AudioControlState,
    ) -> Result<AudioControlSnapshot, AudioError> {
        if card_id != 0 {
            return Err(AudioError::NotFound(format!("mock card {card_id}")));
        }
        match output {
            AudioOutput::Speaker => {
                self.write_mock(output, &self.speaker, state, output.backend_label())
            }
            AudioOutput::Headphones => {
                self.write_mock(output, &self.headphones_mock, state, output.backend_label())
            }
            AudioOutput::Microphone => {
                self.write_mock(output, &self.microphone_mock, state, output.backend_label())
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxAudioCard {
    index: usize,
    name: String,
    controls: Vec<String>,
}

#[cfg(target_os = "linux")]
struct ResolvedLinuxControl {
    volume_control: String,
    switch_control: Option<String>,
}

#[cfg(target_os = "linux")]
fn list_audio_cards_linux() -> Result<Vec<AudioCardSnapshot>, AudioError> {
    let cards = enumerate_audio_cards_linux()?;
    cards
        .into_iter()
        .map(|card| {
            let controls = [
                AudioOutput::Speaker,
                AudioOutput::Headphones,
                AudioOutput::Microphone,
            ]
            .into_iter()
            .filter_map(|kind| {
                read_audio_control_from_card(&card, kind).ok().map(|state| {
                    AudioControlSnapshot::new(kind, state, format!("ALSA {}", kind.label()))
                })
            })
            .collect();
            Ok(AudioCardSnapshot {
                card_id: card.index,
                card_name: card.name,
                controls,
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn read_audio_control_linux(
    card_id: usize,
    kind: AudioOutput,
) -> Result<AudioControlSnapshot, AudioError> {
    let card = find_audio_card_linux(card_id)?;
    let state = read_audio_control_from_card(&card, kind)?;
    Ok(AudioControlSnapshot::new(
        kind,
        state,
        format!("ALSA {}", kind.label()),
    ))
}

#[cfg(target_os = "linux")]
fn write_audio_control_linux(
    card_id: usize,
    kind: AudioOutput,
    state: AudioControlState,
) -> Result<AudioControlSnapshot, AudioError> {
    let card = find_audio_card_linux(card_id)?;
    let control = resolve_audio_controls_for_card(&card, kind)?;
    write_linux_control(card.index, &control, state, kind.switch_tokens())?;
    read_audio_control_linux(card_id, kind)
}

#[cfg(target_os = "linux")]
fn test_audio_control_linux(
    card_id: usize,
    kind: AudioOutput,
) -> Result<AudioControlSnapshot, AudioError> {
    match kind {
        AudioOutput::Speaker | AudioOutput::Headphones => {
            let card = find_audio_card_linux(card_id)?;
            play_sample_linux(card.index)?;
            read_audio_control_linux(card_id, kind)
        }
        AudioOutput::Microphone => Err(AudioError::NotFound(format!(
            "test playback for {} on card {card_id} ({})",
            kind.label(),
            find_audio_card_linux(card_id)
                .map(|card| card.name)
                .unwrap_or_else(|_| "unknown".into())
        ))),
    }
}

#[cfg(target_os = "linux")]
fn save_audio_card_linux(card_id: usize) -> Result<String, AudioError> {
    let card = find_audio_card_linux(card_id)?;
    run_alsactl_store(card.index)?;
    Ok(format!("Saved {} settings", card.name))
}

#[cfg(target_os = "linux")]
fn read_audio_control_from_card(
    card: &LinuxAudioCard,
    kind: AudioOutput,
) -> Result<AudioControlState, AudioError> {
    let control = resolve_audio_controls_for_card(card, kind)?;
    let card_arg = card.index.to_string();
    let stdout = run_amixer(&["-c", &card_arg, "sget", &control.volume_control])?;
    let mut state = parse_amixer_state(&stdout)?;
    if let Some(switch_control) = &control.switch_control {
        let switch_stdout = run_amixer(&["-c", &card_arg, "sget", switch_control])?;
        if let Some(muted) = parse_amixer_switch_state(&switch_stdout) {
            state.muted = muted;
        }
    }
    Ok(state)
}

#[cfg(target_os = "linux")]
fn find_audio_card_linux(card_id: usize) -> Result<LinuxAudioCard, AudioError> {
    enumerate_audio_cards_linux()?
        .into_iter()
        .find(|card| card.index == card_id)
        .ok_or_else(|| AudioError::NotFound(format!("audio card {card_id}")))
}

#[cfg(target_os = "linux")]
fn enumerate_audio_cards_linux() -> Result<Vec<LinuxAudioCard>, AudioError> {
    let data = std::fs::read_to_string("/proc/asound/cards")
        .map_err(|err| AudioError::UnexpectedOutput(format!("/proc/asound/cards: {err}")))?;
    let mut cards = Vec::new();
    for line in data.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || !trimmed.as_bytes()[0].is_ascii_digit() {
            continue;
        }
        let Some((index_raw, rest)) = trimmed.split_once(' ') else {
            continue;
        };
        let Ok(index) = index_raw.parse::<usize>() else {
            continue;
        };
        let bracket_name = rest
            .split('[')
            .nth(1)
            .and_then(|value| value.split(']').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Audio");
        let card_name = rest
            .split(" - ")
            .nth(1)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(bracket_name)
            .to_string();
        let card_arg = index.to_string();
        let controls_out = run_amixer(&["-c", &card_arg, "scontrols"])?;
        cards.push(LinuxAudioCard {
            index,
            name: card_name,
            controls: parse_amixer_controls(&controls_out),
        });
    }
    Ok(cards)
}

#[cfg(target_os = "linux")]
fn resolve_audio_controls_for_card(
    card: &LinuxAudioCard,
    kind: AudioOutput,
) -> Result<ResolvedLinuxControl, AudioError> {
    let volume_control = resolve_named_control(
        &card.controls,
        kind.preferred_controls(),
        kind.fallback_fragments(),
    )
    .ok_or_else(|| {
        AudioError::NotFound(format!(
            "{} control on card {} ({})",
            kind.label(),
            card.index,
            card.name
        ))
    })?;

    let switch_control = resolve_named_control(
        &card.controls,
        kind.preferred_switch_controls(),
        match kind {
            AudioOutput::Speaker => &[
                "speaker switch",
                "master switch",
                "pcm switch",
                "line out switch",
            ],
            AudioOutput::Headphones => &["headphone switch", "headset switch", "head switch"],
            AudioOutput::Microphone => &["capture switch", "mic switch"],
        },
    )
    .filter(|candidate| candidate != &volume_control);

    Ok(ResolvedLinuxControl {
        volume_control,
        switch_control,
    })
}

#[cfg(target_os = "linux")]
fn control_has_switch_linux(card_id: usize, kind: AudioOutput) -> Result<bool, AudioError> {
    let card = find_audio_card_linux(card_id)?;
    let control = resolve_audio_controls_for_card(&card, kind)?;
    Ok(control.switch_control.is_some())
}

#[cfg(target_os = "linux")]
fn resolve_named_control(
    controls: &[String],
    preferred: &[&str],
    fallback_fragments: &[&str],
) -> Option<String> {
    for preferred_name in preferred {
        if let Some(found) = controls
            .iter()
            .find(|control| control.as_str() == *preferred_name)
        {
            return Some(found.clone());
        }
    }
    for fragment in fallback_fragments {
        if let Some(found) = controls
            .iter()
            .find(|control| control.to_ascii_lowercase().contains(fragment))
        {
            return Some(found.clone());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn write_linux_control(
    card_id: usize,
    control: &ResolvedLinuxControl,
    state: AudioControlState,
    switch_tokens: &[&str; 2],
) -> Result<(), AudioError> {
    let switch = if state.muted {
        switch_tokens[1]
    } else {
        switch_tokens[0]
    };
    let volume = format!("{}%", state.volume);
    let card_arg = card_id.to_string();
    run_amixer(&["-c", &card_arg, "sset", &control.volume_control, &volume])?;
    if let Some(switch_control) = &control.switch_control {
        run_amixer(&["-c", &card_arg, "sset", switch_control, switch])?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_amixer(args: &[&str]) -> Result<String, AudioError> {
    let command = format!("amixer {}", args.join(" "));
    let output = Command::new("amixer")
        .args(args)
        .output()
        .map_err(|source| AudioError::CommandIo {
            command: command.clone(),
            source,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let message = if stderr.is_empty() { stdout } else { stderr };
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
fn run_alsactl_store(card_id: usize) -> Result<(), AudioError> {
    let card_arg = card_id.to_string();
    let command = format!("alsactl store {card_arg}");
    let output = Command::new("alsactl")
        .args(["store", &card_arg])
        .output()
        .map_err(|source| AudioError::CommandIo {
            command: command.clone(),
            source,
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(AudioError::CommandFailed { command, message })
}

#[cfg(target_os = "linux")]
fn play_sample_linux(card_id: usize) -> Result<(), AudioError> {
    let sample = find_alsa_sample_path()?;
    let plughw = format!("plughw:{card_id}");
    let hw = format!("hw:{card_id}");

    if run_aplay(&["-q", "-D", &plughw, sample]).is_err() {
        run_aplay(&["-q", "-D", &hw, sample])?;
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn find_alsa_sample_path() -> Result<&'static str, AudioError> {
    const SAMPLE_PATHS: &[&str] = &[
        "/usr/share/sounds/alsa/FrontCenter.wav",
        "/usr/share/sounds/alsa/Front_Center.wav",
    ];

    SAMPLE_PATHS
        .iter()
        .copied()
        .find(|path| std::path::Path::new(path).exists())
        .ok_or_else(|| AudioError::NotFound("ALSA FrontCenter sample".into()))
}

#[cfg(target_os = "linux")]
fn run_aplay(args: &[&str]) -> Result<(), AudioError> {
    let command = format!("aplay {}", args.join(" "));
    let output = Command::new("aplay")
        .args(args)
        .output()
        .map_err(|source| AudioError::CommandIo {
            command: command.clone(),
            source,
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(AudioError::CommandFailed { command, message })
}

#[cfg(target_os = "linux")]
fn parse_amixer_state(stdout: &str) -> Result<AudioControlState, AudioError> {
    let volume = stdout
        .split('[')
        .filter_map(|segment| segment.split_once("%]").map(|(value, _)| value.trim()))
        .filter_map(|value| value.parse::<u8>().ok())
        .next_back()
        .ok_or_else(|| AudioError::UnexpectedOutput(stdout.trim().to_string()))?;

    let muted = parse_amixer_switch_state(stdout).unwrap_or(false);

    AudioControlState { volume, muted }.validate()
}

#[cfg(target_os = "linux")]
fn parse_amixer_switch_state(stdout: &str) -> Option<bool> {
    stdout
        .split('[')
        .filter_map(|segment| segment.split_once(']').map(|(value, _)| value.trim()))
        .filter_map(|value| match value {
            "on" => Some(false),
            "off" => Some(true),
            "cap" => Some(false),
            "nocap" => Some(true),
            _ => None,
        })
        .next_back()
}

#[cfg(target_os = "linux")]
fn parse_amixer_controls(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let start = line.find('\'')?;
            let rest = &line[start + 1..];
            let end = rest.find('\'')?;
            Some(rest[..end].to_string())
        })
        .collect()
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

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_amixer_controls_prefers_named_entries() {
        let stdout = r#"Simple mixer control 'Master',0
Simple mixer control 'Headphones',0
Simple mixer control 'PCM',0
"#;

        assert_eq!(
            super::parse_amixer_controls(stdout),
            vec![
                "Master".to_string(),
                "Headphones".to_string(),
                "PCM".to_string()
            ]
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
