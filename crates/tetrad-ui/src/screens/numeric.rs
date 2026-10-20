//! Numeric value entry (manual "PARAMETER EDITOR LAYOUT": tap the numeric display):
//! `ParameterValueDialog` (`0x64b518`) with `KeyboardNumeric` (`0x2d7f1c`). A full page with
//! ✕ and ✓, the typed value, the range line and the 4 × 3 keypad. ✓ stays disabled while the
//! text is not a number inside the range (`0x64b774`); the dialog never clamps. Tetrad also
//! types the MIDI number of a mapping trigger with it.

use tetrad_assets::{ACCENT, BACKGROUND, FAINT, Icon, PANEL, SURFACE, TEXT};

use crate::layout::{CAPTION, Layout, Look, XLARGE_BOLD};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::*;

/// What ✓ of the numeric keyboard sets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NumericPurpose {
    /// A parameter value ([`UiCommand::EnterParameter`]).
    Parameter {
        target: ParameterTarget,
        parameter: u16,
    },
    /// Owned (Tetrad): the CC or note number of a MIDI mapping trigger.
    MappingMidi {
        scope: MappingScope,
        id: MappingId,
        note: bool,
    },
}

/// The open numeric keyboard of one value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NumericEntry {
    pub purpose: NumericPurpose,
    pub range: NumericRange,
    pub text: String,
    /// The start text is selected: the first key replaces it (I).
    pub fresh: bool,
}

impl NumericEntry {
    /// Start text `0x64b2e0`: two decimals, or the rounded integer.
    pub fn new(purpose: NumericPurpose, range: NumericRange) -> Self {
        let text = if range.integer {
            format!("{}", range.value.round() as i64)
        } else {
            format!("{:.2}", range.value)
        };
        Self {
            purpose,
            range,
            text,
            fresh: true,
        }
    }

    /// Negative values exist (`0x64b25c`).
    fn signed(&self) -> bool {
        self.range.min < 0.0 || self.range.max < 0.0
    }

    /// `max(len(min), len(max))`, plus 3 for decimals.
    fn max_len(&self) -> usize {
        let len = number_g(self.range.min)
            .len()
            .max(number_g(self.range.max).len());
        if self.range.integer { len } else { len + 3 }
    }

    /// The typed value when ✓ may accept it.
    pub fn value(&self) -> Option<f32> {
        let value: f32 = self.text.parse().ok()?;
        (value.is_finite() && value >= self.range.min && value <= self.range.max).then_some(value)
    }

    fn enabled(&self, key: NumericKey) -> bool {
        let text = if self.fresh { "" } else { self.text.as_str() };
        match key {
            NumericKey::Digit(0) => {
                !(text.is_empty() && self.range.integer && self.range.min.round() > 0.0)
            }
            NumericKey::Point => !self.range.integer && !text.contains('.'),
            // The sign key flips a value that is not zero (`0x4d5bcc`).
            NumericKey::Minus => {
                self.signed() && text.parse::<f32>().is_ok_and(|value| value.abs() > 1e-5)
            }
            NumericKey::Done => self.value().is_some(),
            _ => true,
        }
    }

    /// Applies a key; returns the request of ✓.
    pub fn press(&mut self, key: NumericKey) -> Option<UiCommand> {
        if !self.enabled(key) {
            return None;
        }
        let fresh = std::mem::take(&mut self.fresh);
        match key {
            NumericKey::Done => {
                return Some(match &self.purpose {
                    NumericPurpose::Parameter { target, parameter } => UiCommand::EnterParameter {
                        target: *target,
                        parameter: *parameter,
                        text: self.text.clone(),
                    },
                    // ✓ is enabled only inside 0..=127.
                    NumericPurpose::MappingMidi { scope, id, note } => {
                        let number = self.value()? as u8;
                        let input = match note {
                            true => MidiInput::Note(number),
                            false => MidiInput::Cc(number),
                        };
                        UiCommand::Mapping(MappingCommand::SetTrigger {
                            scope: *scope,
                            id: id.clone(),
                            trigger: Trigger::Midi(input),
                        })
                    }
                });
            }
            NumericKey::Cancel => {}
            NumericKey::Backspace if fresh => self.text.clear(),
            NumericKey::Backspace => {
                self.text.pop();
            }
            NumericKey::Minus => match self.text.strip_prefix('-') {
                Some(rest) => self.text = rest.to_owned(),
                None => self.text.insert(0, '-'),
            },
            NumericKey::Digit(_) | NumericKey::Point => {
                if fresh {
                    self.text.clear();
                }
                if self.text.len() < self.max_len() {
                    match key {
                        NumericKey::Digit(digit) => self.text.push(char::from(b'0' + digit % 10)),
                        _ => self.text.push('.'),
                    }
                }
            }
        }
        None
    }
}

/// `QString::number(v, 'g', 6)`: six significant digits, no trailing zeros.
fn number_g(value: f32) -> String {
    if value == 0.0 || !value.is_finite() {
        return "0".into();
    }
    let exponent = value.abs().log10().floor() as i32;
    if !(-4..6).contains(&exponent) {
        return format!("{value:e}");
    }
    let text = format!("{:.*}", (5 - exponent).max(0) as usize, value);
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

/// Keypad x columns and row y inside the 800 px page (`NumKeysContainer` at y 200).
const KEY_X: [i32; 3] = [224, 344, 464];
const KEY_Y: [i32; 4] = [215, 280, 344, 408];

pub(super) fn layout(l: &mut Layout, entry: &NumericEntry) {
    l.overlay();
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(BACKGROUND));
    let key = |key| Control::Numeric(key);
    l.control(
        Rect::new(8, 8, 98, 44),
        key(NumericKey::Cancel),
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Close,
            fill: PANEL,
        },
    );
    l.control(
        crate::widgets::CLOSE_BUTTON,
        key(NumericKey::Done),
        entry.enabled(NumericKey::Done),
        "DONE",
        Look::IconButton {
            icon: Icon::Check,
            fill: PANEL,
        },
    );
    let field = Rect::new(40, 79, 720, 60);
    l.panel(field, SURFACE, 10.0, Some(ACCENT));
    let area = field.inset(16, 0);
    if entry.text.is_empty() {
        l.text(area, "0.00", XLARGE_BOLD, FAINT, Align::Left);
    } else {
        l.text(area, entry.text.clone(), XLARGE_BOLD, TEXT, Align::Left);
    }
    l.text(
        Rect::new(0, 176, WIDTH, 18),
        format!(
            "Values from {} to {}",
            number_g(entry.range.min),
            number_g(entry.range.max)
        ),
        CAPTION,
        FAINT,
        Align::Center,
    );
    let mut keys: Vec<(Rect, NumericKey, &str)> = (1..=9u8)
        .map(|digit| {
            let index = usize::from(digit - 1);
            (
                Rect::new(KEY_X[index % 3], KEY_Y[index / 3], 112, 56),
                NumericKey::Digit(digit),
                ["1", "2", "3", "4", "5", "6", "7", "8", "9"][index],
            )
        })
        .collect();
    // Row 4 by mode (`setNumericMode` `0x4d76e0`).
    let bottom = KEY_Y[3];
    let first = match (entry.range.integer, entry.signed()) {
        (true, _) => vec![(Rect::new(224, bottom, 112, 56), NumericKey::Minus, "-")],
        (false, false) => vec![(Rect::new(224, bottom, 112, 56), NumericKey::Point, ".")],
        (false, true) => vec![
            (Rect::new(224, bottom, 52, 56), NumericKey::Minus, "-"),
            (Rect::new(284, bottom, 52, 56), NumericKey::Point, "."),
        ],
    };
    keys.extend(first);
    keys.push((Rect::new(344, bottom, 112, 56), NumericKey::Digit(0), "0"));
    let negative = entry.text.starts_with('-');
    for (bounds, code, label) in keys {
        l.control(
            bounds,
            key(code),
            entry.enabled(code),
            label,
            Look::Key {
                icon: None,
                accent: code == NumericKey::Minus && negative,
            },
        );
    }
    l.control(
        Rect::new(464, bottom, 112, 56),
        key(NumericKey::Backspace),
        true,
        "Backspace",
        Look::Key {
            icon: Some(Icon::Backspace),
            accent: false,
        },
    );
}
