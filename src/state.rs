//! What the app reports about itself, to the GUI and as JSON over the HTTP API.

use serde::Serialize;

use crate::config::{BadgePosition, Config};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Idle,
    Live,
    Delayed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitionKind {
    Rewind,
    Forward,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub clamped: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
}

impl CommandResult {
    pub fn error(msg: impl Into<String>) -> Self {
        CommandResult { ok: false, error: Some(msg.into()), ..Default::default() }
    }

    pub fn delay(delay: f64) -> Self {
        CommandResult { ok: true, delay: Some(delay), ..Default::default() }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Phases {
    pub intro: f64,
    pub scrub: f64,
    pub land: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct TransitionEvent {
    pub kind: TransitionKind,
    pub from: f64,
    pub to: f64,
    /// seconds; the GUI mirrors these to stay in sync with the stream
    pub phases: Phases,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransitionState {
    pub kind: TransitionKind,
    pub from: f64,
    pub to: f64,
    pub progress: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputState {
    pub width: u32,
    pub height: u32,
    pub running: bool,
    pub codec: Option<String>,
    pub destinations: Vec<String>,
    pub preview_only: bool,
    /// Unix milliseconds
    pub since: Option<u64>,
    pub dropped: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayoutState {
    pub mode: Mode,
    pub target_delay: f64,
    pub effective_delay: f64,
    pub max_delay: f64,
    pub transition: Option<TransitionState>,
    pub output: OutputState,
    pub underruns: u64,
}

impl PlayoutState {
    pub fn idle(cfg: &Config) -> Self {
        PlayoutState {
            mode: Mode::Idle,
            target_delay: 0.0,
            effective_delay: 0.0,
            max_delay: 0.0,
            transition: None,
            output: OutputState { width: cfg.output.width, height: cfg.output.height, ..Default::default() },
            underruns: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IngestInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestState {
    pub connected: bool,
    pub server: String,
    pub key: String,
    pub info: Option<IngestInfo>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BufferState {
    pub recorded: f64,
    pub capacity: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub ingest: IngestState,
    pub buffer: BufferState,
    pub playout: PlayoutState,
    pub delay_seconds: f64,
    pub badge: BadgePosition,
    pub restart_pending: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigResponse {
    pub config: Config,
    pub restart_pending: bool,
}
