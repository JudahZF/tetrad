//! Bounded, explicit JSONL input for owned UI replay. No file or device I/O.

use serde::Deserialize;
use tetrad_ui::{Control, GridPosition, InputEvent, SceneIndex, Screen};

pub const MAX_TRACE_BYTES: usize = 1024 * 1024;
pub const MAX_TRACE_EVENTS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayEvent {
    Input(InputEvent),
    Quit,
}

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("input trace exceeds {MAX_TRACE_BYTES} bytes")]
    Bytes,
    #[error("input trace exceeds {MAX_TRACE_EVENTS} events")]
    Events,
    #[error("input trace is not UTF-8")]
    Text,
    #[error("input trace line {line} is invalid: {source}")]
    Json {
        line: usize,
        source: serde_json::Error,
    },
    #[error("input trace line {line}: {source}")]
    Ui {
        line: usize,
        source: tetrad_ui::UiError,
    },
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum WireEvent {
    PointerDown { x: i32, y: i32 },
    PointerMove { x: i32, y: i32 },
    PointerUp { x: i32, y: i32 },
    CancelPointer {},
    Back {},
    Text { value: String },
    Color { value: u32 },
    Activate { control: WireControl },
    Quit {},
}

#[derive(Deserialize)]
#[serde(tag = "name", rename_all = "snake_case", deny_unknown_fields)]
enum WireControl {
    Navigate {
        screen: WireScreen,
    },
    SelectScene {
        scene: u8,
    },
    SelectCell {
        row: u8,
        column: u8,
    },
    SelectPreset {
        id: u64,
    },
    EditScenes {},
    /// The Grid scene badge: opens or closes the scene dropdown.
    SceneMenu {},
    ConfirmScenes {},
    CloseDialog {},
    CycleMode {},
    BeginBankPreview {},
    EndBankPreview {},
    CancelBankPreview {},
    PreviousPresetPage {},
    NextPresetPage {},
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireScreen {
    Grid,
    Gig,
    Presets,
}

/// Every nonblank line is one event. Parse the full trace before applying any
/// event. Scene numbers are one-based; grid row/column numbers are zero-based.
/// A quit event does not exempt later lines from syntax or admission checks.
pub fn parse_trace(bytes: &[u8]) -> Result<Vec<ReplayEvent>, TraceError> {
    if bytes.len() > MAX_TRACE_BYTES {
        return Err(TraceError::Bytes);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| TraceError::Text)?;
    let mut events = Vec::new();
    for (index, text) in text.lines().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        if events.len() == MAX_TRACE_EVENTS {
            return Err(TraceError::Events);
        }
        let line = index + 1;
        let event =
            serde_json::from_str(text).map_err(|source| TraceError::Json { line, source })?;
        let ui_error = |source| TraceError::Ui { line, source };
        let input = match event {
            WireEvent::Quit {} => {
                events.push(ReplayEvent::Quit);
                continue;
            }
            WireEvent::PointerDown { x, y } => InputEvent::PointerDown { x, y },
            WireEvent::PointerMove { x, y } => InputEvent::PointerMove { x, y },
            WireEvent::PointerUp { x, y } => InputEvent::PointerUp { x, y },
            WireEvent::CancelPointer {} => InputEvent::CancelPointer,
            WireEvent::Back {} => InputEvent::Back,
            WireEvent::Text { value } => {
                if value.len() > tetrad_ui::MAX_TEXT_BYTES {
                    return Err(ui_error(tetrad_ui::UiError::TextTooLong));
                }
                InputEvent::TextChanged(value)
            }
            WireEvent::Color { value } => InputEvent::DraftColor(value),
            WireEvent::Activate { control } => InputEvent::Activate(match control {
                WireControl::Navigate { screen } => Control::Navigate(match screen {
                    WireScreen::Grid => Screen::Grid,
                    WireScreen::Gig => Screen::Gig,
                    WireScreen::Presets => Screen::Presets,
                }),
                WireControl::SelectScene { scene } => Control::SelectScene(
                    SceneIndex::new(
                        scene
                            .checked_sub(1)
                            .ok_or_else(|| ui_error(tetrad_ui::UiError::InvalidScene))?,
                    )
                    .map_err(ui_error)?,
                ),
                WireControl::SelectCell { row, column } => {
                    Control::SelectCell(GridPosition::new(row, column).map_err(ui_error)?)
                }
                WireControl::SelectPreset { id } => Control::SelectPreset(id),
                WireControl::EditScenes {} => Control::EditScenes,
                WireControl::SceneMenu {} => Control::SceneMenu,
                WireControl::ConfirmScenes {} => Control::ConfirmScenes,
                WireControl::CloseDialog {} => Control::CloseDialog,
                WireControl::CycleMode {} => Control::CycleMode,
                WireControl::BeginBankPreview {} => Control::BeginBankPreview,
                WireControl::EndBankPreview {} => Control::EndBankPreview,
                WireControl::CancelBankPreview {} => Control::CancelBankPreview,
                WireControl::PreviousPresetPage {} => Control::PreviousPresetPage,
                WireControl::NextPresetPage {} => Control::NextPresetPage,
            }),
        };
        events.push(ReplayEvent::Input(input));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_maps_pointer_scene_and_dialog_events_without_applying_them() {
        let trace = br#"
{"event":"pointer_down","x":16,"y":20}
{"event":"pointer_up","x":16,"y":20}
{"event":"activate","control":{"name":"select_scene","scene":2}}
{"event":"text","value":"Bridge"}
{"event":"quit"}
"#;
        assert_eq!(
            parse_trace(trace).unwrap(),
            [
                ReplayEvent::Input(InputEvent::PointerDown { x: 16, y: 20 }),
                ReplayEvent::Input(InputEvent::PointerUp { x: 16, y: 20 }),
                ReplayEvent::Input(InputEvent::Activate(Control::SelectScene(
                    SceneIndex::new(1).unwrap()
                ))),
                ReplayEvent::Input(InputEvent::TextChanged("Bridge".into())),
                ReplayEvent::Quit,
            ]
        );
    }

    #[test]
    fn malformed_ambiguous_or_out_of_range_events_refuse_the_full_trace() {
        for text in [
            r#"{"event":"color","value":-1}"#,
            r#"{"event":"color","value":1,"value":2}"#,
            r#"{"event":"quit","unexpected":1}"#,
            r#"{"event":"quit","event":"back"}"#,
            r#"{"event":"activate","control":{"name":"edit_scenes","unexpected":1}}"#,
            r#"{"event":"back","unexpected":1}"#,
            r#"{"event":"activate","control":{"name":"select_scene","scene":0}}"#,
            r#"{"event":"activate","control":{"name":"select_scene","scene":9}}"#,
            r#"{"event":"activate","control":{"name":"select_cell","row":4,"column":0}}"#,
            r#"{"event":"pointer_down","x":2147483648,"y":0}"#,
            r#"{"event":"unknown"}"#,
        ] {
            assert!(parse_trace(text.as_bytes()).is_err(), "{text}");
        }
        assert!(parse_trace(b"{\"event\":\"quit\"}\nnot json").is_err());
    }

    #[test]
    fn trace_limits_bound_bytes_events_and_decoded_text() {
        assert!(matches!(
            parse_trace(&vec![b' '; MAX_TRACE_BYTES + 1]),
            Err(TraceError::Bytes)
        ));
        let one = "{\"event\":\"back\"}\n";
        assert_eq!(
            parse_trace(one.repeat(MAX_TRACE_EVENTS).as_bytes())
                .unwrap()
                .len(),
            MAX_TRACE_EVENTS
        );
        assert!(matches!(
            parse_trace(one.repeat(MAX_TRACE_EVENTS + 1).as_bytes()),
            Err(TraceError::Events)
        ));
        let value = "x".repeat(tetrad_ui::MAX_TEXT_BYTES + 1);
        let text = serde_json::json!({"event":"text", "value":value}).to_string();
        assert!(matches!(
            parse_trace(text.as_bytes()),
            Err(TraceError::Ui {
                source: tetrad_ui::UiError::TextTooLong,
                ..
            })
        ));
        assert!(matches!(parse_trace(&[0xff]), Err(TraceError::Text)));
    }
}
