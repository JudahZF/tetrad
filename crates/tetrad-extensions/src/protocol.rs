//! Host ↔ runner wire protocol: one JSON object per line on the runner's stdin and stdout.
//!
//! The host owns every authority. A runner only proposes calls; the host decodes, validates
//! and answers each one. Frames over [`MAX_FRAME`] are a protocol fault on either side.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire version. A runner that sees a different version refuses to start.
pub const PROTOCOL_VERSION: u32 = 1;

/// Largest frame in bytes, newline excluded. Block IR and asset text can be large.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Host → runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostMessage {
    /// Starts the read/register-only activation phase. Sent once, first.
    Activate {
        protocol: u32,
        extension: ExtensionInfo,
        config: Value,
    },
    /// One event for one handler scope. The runner dispatches events serially and answers
    /// each with [`RunnerMessage::Done`].
    Event { scope: u64, event: Event },
    /// The answer to [`RunnerMessage::Request`].
    Response { id: u64, result: CallResult },
    /// Run the disposer and exit. The host kills the runner after a bounded wait.
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInfo {
    pub id: String,
    pub version: String,
}

/// Runner → host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RunnerMessage {
    /// Activation resolved; the host commits these registrations and then sends
    /// [`Event::Running`].
    Activated {
        registrations: Registrations,
    },
    /// A host call. `scope` is `None` for read-only activation-context calls.
    Request {
        id: u64,
        scope: Option<u64>,
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// The handler of `scope` settled; the host closes the scope.
    Done {
        scope: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Log {
        level: LogLevel,
        text: String,
    },
    /// The runner cannot continue (activation failure, timeout, memory). It exits next.
    Fault {
        message: String,
    },
}

/// What an extension declared during activation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registrations {
    /// Exclusive MIDI channels (1..=16) whose voice messages go to this extension only.
    #[serde(default)]
    pub midi_channels: Vec<u8>,
    /// Command IDs with a handler; the settings page shows them as buttons.
    #[serde(default)]
    pub commands: Vec<String>,
    /// The extension wants [`Event::ControlChanged`] for native edits.
    #[serde(default)]
    pub control_changes: bool,
    /// Triggers the extension offers to Mappings, in declaration order.
    #[serde(default)]
    pub triggers: Vec<TriggerSpec>,
    /// Block IDs with a provider (`ctx.blocks.provide`). The host checks each against the
    /// manifest's generated blocks and the `blocks` capability.
    #[serde(default)]
    pub blocks: Vec<String>,
}

/// Most triggers one extension may declare.
pub const MAX_TRIGGERS: usize = 8;
/// Longest trigger ID.
pub const MAX_TRIGGER_ID: usize = 32;
/// Longest trigger label, declared or fired.
pub const MAX_TRIGGER_LABEL: usize = 48;

/// One trigger from `ctx.triggers.declare`. Mappings show it as "`<extension name> · <label>`".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerSpec {
    pub id: String,
    pub label: String,
}

impl TriggerSpec {
    /// The ID is 1..=[`MAX_TRIGGER_ID`] characters of `[a-z0-9-]`; the label is
    /// 1..=[`MAX_TRIGGER_LABEL`] characters without control characters.
    pub fn is_valid(&self) -> bool {
        let id_ok = (1..=MAX_TRIGGER_ID).contains(&self.id.len())
            && (self.id.bytes()).all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let label = self.label.chars().count();
        id_ok
            && (1..=MAX_TRIGGER_LABEL).contains(&label)
            && !self.label.chars().any(char::is_control)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    /// Activation committed: initialization writes are allowed in this scope.
    Running,
    Midi {
        message: MidiEvent,
    },
    Command {
        id: String,
    },
    /// A native edit of a block (touch, footswitch, Cortex Control). An extension never hears
    /// edits that an extension made.
    ControlChanged {
        change: ControlChange,
    },
    /// A different preset is loaded; every target handle from before is stale.
    PresetLoaded,
    /// Build the IR of a generated block from an asset. The handler ends with one
    /// [`PUBLISH_BLOCK`] call in this event's scope. This event has the longer
    /// [`Limits::build_handler`](crate::runner::Limits::build_handler) budget.
    BuildBlock {
        block: String,
        /// The name the host gives the result, such as the asset's file stem.
        variant: String,
        asset: AssetText,
    },
}

/// An asset file as UTF-8 text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetText {
    /// The file name, such as `Plexi.nam`.
    pub name: String,
    pub text: String,
}

/// The host method that ends an [`Event::BuildBlock`] handler. Its params are a
/// [`BlockPublish`].
pub const PUBLISH_BLOCK: &str = "blocks.publish";

/// The params of [`PUBLISH_BLOCK`]: `{block, variant, ir}` or `{block, variant, error}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockPublish {
    pub block: String,
    pub variant: String,
    #[serde(flatten)]
    pub outcome: BuildOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildOutcome {
    /// The provider's IR, not validated: decode it with `qcdsp::Block::from_json` or
    /// deserialize and `validate()` it.
    Ir(Value),
    /// The message of what the provider threw.
    Error(String),
}

/// A channel voice message on a reserved channel. Channels count from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MidiEvent {
    #[serde(rename = "cc")]
    ControlChange {
        channel: u8,
        controller: u8,
        value: u8,
    },
    #[serde(rename = "pc")]
    ProgramChange {
        channel: u8,
        program: u8,
    },
    NoteOn {
        channel: u8,
        key: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        key: u8,
        velocity: u8,
    },
}

/// Where a control lives: one block site of the loaded preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "site", rename_all = "camelCase")]
pub enum Site {
    /// Grid slot; row 0..=3, column 0..=7.
    Slot {
        row: u8,
        column: u8,
    },
    Input {
        row: u8,
    },
    Output {
        row: u8,
    },
    Splitter {
        pair: Pair,
    },
    Mixer {
        pair: Pair,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Pair {
    Upper,
    Lower,
}

/// A saved reference to one control: the site, the model expected there and what to drive.
/// It never follows a block that moves; a different model at the site is unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    #[serde(flatten)]
    pub site: Site,
    pub model: u32,
    pub control: ControlKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ControlKind {
    Parameter { index: u32 },
    Bypass,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlChange {
    pub binding: Binding,
    /// Normalized 0..=1 for a parameter; 0 or 1 (bypassed) for bypass.
    pub value: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CallResult {
    Ok(Value),
    Error(CallError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallError {
    pub code: ErrorCode,
    pub message: String,
}

/// Typed refusals. `StaleContext` and `ScopeClosed` are expected cancellations, not faults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    UnknownMethod,
    InvalidParams,
    PermissionDenied,
    NotRunning,
    ScopeClosed,
    StaleContext,
    Unresolved,
    Unsupported,
    LimitExceeded,
    Failed,
}

impl CallResult {
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error(CallError {
            code,
            message: message.into(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes is over the {MAX_FRAME} byte limit")]
    TooLarge(usize),
    #[error("bad frame: {0}")]
    Json(#[from] serde_json::Error),
}

/// One encoded line, newline included.
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let mut line = serde_json::to_vec(message)?;
    if line.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(line.len()));
    }
    line.push(b'\n');
    Ok(line)
}

pub fn decode<T: for<'de> Deserialize<'de>>(line: &[u8]) -> Result<T, FrameError> {
    if line.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(line.len()));
    }
    Ok(serde_json::from_slice(line)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_wire_shape_is_flat() {
        let binding = Binding {
            site: Site::Slot { row: 1, column: 3 },
            model: 42,
            control: ControlKind::Parameter { index: 2 },
        };
        let json = serde_json::to_value(binding).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"site": "slot", "row": 1, "column": 3, "model": 42,
                "control": {"kind": "parameter", "index": 2}})
        );
        assert_eq!(serde_json::from_value::<Binding>(json).unwrap(), binding);
    }

    #[test]
    fn oversize_frames_are_refused() {
        let text = "x".repeat(MAX_FRAME);
        assert!(matches!(
            encode(&RunnerMessage::Log {
                level: LogLevel::Info,
                text
            }),
            Err(FrameError::TooLarge(_))
        ));
        assert!(matches!(
            decode::<HostMessage>(&vec![b' '; MAX_FRAME + 1]),
            Err(FrameError::TooLarge(_))
        ));
    }
}
