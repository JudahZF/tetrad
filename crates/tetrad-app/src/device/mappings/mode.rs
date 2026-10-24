//! Value modes (owned, Tetrad): how a mapping's trigger value drives its action. The rules
//! here are pure; [`super::Mappings`] keeps the runtime state (held and set values, the last
//! value of each CC).

use serde::{Deserialize, Serialize};
use tetrad_catalog::{ParamSpec, ParameterType};
use tetrad_ui::{FieldId, ModeKind};

/// Margin for float compares of normalized values.
const EPSILON: f32 = 1e-4;
/// Step amounts go from -MAX_AMOUNT to MAX_AMOUNT, never 0.
const MAX_AMOUNT: i8 = 64;

/// A stored mode. Values are the target's normalized value (0..=1); switch targets use only
/// `Set::value` (0.5 or more is on).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(super) enum Mode {
    OnPress,
    /// The window's Low…High maps to `min…max`; `min > max` inverts.
    Follow {
        min: f32,
        max: f32,
        #[serde(default)]
        pickup: bool,
    },
    Relative {
        encoding: Encoding,
    },
    Set {
        value: f32,
    },
    Toggle {
        a: f32,
        b: f32,
    },
    /// The release restores the value from before the press.
    Momentary {
        value: f32,
    },
    /// `amount` target steps per press.
    Step {
        amount: i8,
        #[serde(default)]
        wrap: bool,
    },
}

/// How an endless encoder codes its ticks in a CC value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Encoding {
    /// 1..=63 up, 127 down to 65 (-1..=-63).
    TwosComplement,
    /// 64 is still: 65 is +1, 63 is -1.
    Offset,
    /// Bit 6 is the sign: 1..=63 up, 65..=127 down.
    SignBit,
}

const ENCODINGS: [Encoding; 3] = [
    Encoding::TwosComplement,
    Encoding::Offset,
    Encoding::SignBit,
];

/// The CC values (inclusive) where a press mode presses, or that Follow maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Window {
    pub low: u8,
    pub high: u8,
}

impl Window {
    pub const FULL: Window = Window { low: 0, high: 127 };
    pub const UPPER: Window = Window { low: 64, high: 127 };

    pub fn valid(self) -> bool {
        self.low <= self.high && self.high <= 127
    }

    pub fn contains(self, value: u8) -> bool {
        (self.low..=self.high).contains(&value)
    }
}

/// What a trigger reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Input {
    Press,
    Release,
    /// A CC value and the one before it, when there was one.
    Level {
        value: u8,
        previous: Option<u8>,
    },
}

/// How a trigger reports, as the mode rules see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// Preset loaded: one press.
    Load,
    /// Footswitch, note, extension press: press and release.
    Press,
    /// CC, extension CC.
    Level,
    /// Expression pedal: the stock expression link.
    Pedal,
}

/// What an action drives, as the mode rules see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Parameter,
    /// Block bypass (on = engaged), tuner, Gig View.
    Switch,
    Looper,
    /// One-shot system actions: preset, scene, mode, tap tempo.
    Command,
    SendMidi,
}

/// The modes a mapping may use, the default first. A pedal on a block has none: its stock
/// expression link decides (shown as Follow).
pub(super) fn modes(source: Source, target: Target) -> &'static [ModeKind] {
    use ModeKind::*;
    match (target, source) {
        (Target::Command | Target::SendMidi, _) => &[OnPress],
        (_, Source::Pedal) => &[],
        (Target::Parameter, Source::Load) => &[Set],
        (Target::Parameter, Source::Press) => &[Set, Toggle, Momentary, Step],
        (Target::Parameter, Source::Level) => &[Follow, Relative, Set, Toggle, Momentary, Step],
        (Target::Switch, Source::Load) => &[Toggle, Set],
        (Target::Switch, Source::Press) => &[Toggle, Momentary, Set],
        (Target::Switch, Source::Level) => &[Follow, Toggle, Momentary, Set],
        (Target::Looper, Source::Load) => &[OnPress],
        (Target::Looper, Source::Press) => &[OnPress, Momentary],
        (Target::Looper, Source::Level) => &[Momentary, OnPress],
    }
}

impl Mode {
    pub fn kind(self) -> ModeKind {
        match self {
            Mode::OnPress => ModeKind::OnPress,
            Mode::Follow { .. } => ModeKind::Follow,
            Mode::Relative { .. } => ModeKind::Relative,
            Mode::Set { .. } => ModeKind::Set,
            Mode::Toggle { .. } => ModeKind::Toggle,
            Mode::Momentary { .. } => ModeKind::Momentary,
            Mode::Step { .. } => ModeKind::Step,
        }
    }

    /// A new mode of `kind` for a target that is at `current` now, with `other` as the second
    /// value (see [`Param::other`]). Switches pass on and off.
    pub fn fresh(kind: ModeKind, current: f32, other: f32) -> Mode {
        match kind {
            ModeKind::OnPress => Mode::OnPress,
            ModeKind::Follow => Mode::Follow {
                min: 0.0,
                max: 1.0,
                pickup: false,
            },
            ModeKind::Relative => Mode::Relative {
                encoding: Encoding::TwosComplement,
            },
            ModeKind::Set => Mode::Set { value: current },
            ModeKind::Toggle => Mode::Toggle {
                a: current,
                b: other,
            },
            ModeKind::Momentary => Mode::Momentary { value: other },
            ModeKind::Step => Mode::Step {
                amount: 1,
                wrap: false,
            },
        }
    }

    /// Values in range: a stored list can come from anyone.
    pub fn valid(self) -> bool {
        let unit = |v: f32| (0.0..=1.0).contains(&v);
        match self {
            Mode::OnPress | Mode::Relative { .. } => true,
            Mode::Follow { min, max, .. } => unit(min) && unit(max),
            Mode::Set { value } | Mode::Momentary { value } => unit(value),
            Mode::Toggle { a, b } => unit(a) && unit(b),
            Mode::Step { amount, .. } => amount != 0 && amount.abs() <= MAX_AMOUNT,
        }
    }

    /// The window a mode uses when the row stores none: Follow on a parameter maps the whole
    /// CC, everything else presses (or is on) at 64 and above, as stock CC switches do.
    pub fn default_window(self, target: Target) -> Window {
        match (self, target) {
            (Mode::Follow { .. }, Target::Parameter) => Window::FULL,
            _ => Window::UPPER,
        }
    }
}

/// A CC against a press mode's window: a press when it enters the window or repeats its value
/// inside it (a button that sends only 127), a release when it leaves.
pub(super) fn edge(value: u8, previous: Option<u8>, window: Window) -> Option<Input> {
    let inside = window.contains(value);
    let was_inside = previous.is_some_and(|p| window.contains(p));
    match (was_inside, inside) {
        (_, true) if previous == Some(value) || !was_inside => Some(Input::Press),
        (true, false) => Some(Input::Release),
        _ => None,
    }
}

/// Follow's map of a CC value to a target value.
pub(super) fn follow(value: u8, window: Window, min: f32, max: f32) -> f32 {
    let t = if window.high == window.low {
        f32::from(u8::from(value >= window.low))
    } else {
        let clamped = value.clamp(window.low, window.high);
        f32::from(clamped - window.low) / f32::from(window.high - window.low)
    };
    min + t * (max - min)
}

/// Encoder ticks in a CC value; 0 moves nothing.
pub(super) fn ticks(encoding: Encoding, value: u8) -> i32 {
    let value = i32::from(value.min(127));
    match encoding {
        Encoding::TwosComplement if value < 64 => value,
        Encoding::TwosComplement => value - 128,
        Encoding::Offset => value - 64,
        Encoding::SignBit if value & 64 == 0 => value,
        Encoding::SignBit => -(value & 63),
    }
}

/// A block parameter as the loaded preset has it now.
pub(super) struct Param<'a> {
    pub spec: &'a ParamSpec,
    pub current: f32,
}

impl Param<'_> {
    /// Positions and the size of one step, as the encoder steps them.
    fn grid(&self) -> (i32, f32) {
        match self.spec.scale.steps() {
            steps if steps < 2 => (101, 0.01),
            steps => (steps, 1.0 / (steps - 1) as f32),
        }
    }

    pub fn near(&self, a: f32, b: f32) -> bool {
        (a - b).abs() <= self.grid().1 / 2.0 + EPSILON
    }

    /// Within one step: where Pickup catches.
    pub fn within_step(&self, a: f32, b: f32) -> bool {
        (a - b).abs() <= self.grid().1 + EPSILON
    }

    /// The second value of a new Toggle or Momentary: the default, or the far end when the
    /// parameter is at its default.
    pub fn other(&self) -> f32 {
        let default = self.spec.default_normalized();
        if default.is_finite() && !self.near(default, self.current) {
            default.clamp(0.0, 1.0)
        } else if self.current < 0.5 {
            1.0
        } else {
            0.0
        }
    }

    /// `value` moved by `amount` steps, clamped or wrapped at the ends.
    pub fn stepped(&self, value: f32, amount: i8, wrap: bool) -> f32 {
        let (count, size) = self.grid();
        let index = (value / size).round() as i32 + i32::from(amount);
        let index = if wrap {
            index.rem_euclid(count)
        } else {
            index.clamp(0, count - 1)
        };
        (index as f32 * size).clamp(0.0, 1.0)
    }

    /// The value as the block editor shows it ("12.0 dB", "Plate", "On").
    pub fn text(&self, value: f32) -> String {
        match self.spec.kind {
            ParameterType::Switch | ParameterType::ToggleButton => {
                if value >= 0.5 { "On" } else { "Off" }.into()
            }
            _ => (self.spec.scale.display_with_units(value)).unwrap_or_else(|_| "-".into()),
        }
    }
}

/// The window of a CC trigger, then the fields of the mode.
pub(super) fn fields(mode: Mode, source: Source, target: Target) -> Vec<FieldId> {
    use FieldId::*;
    if source == Source::Pedal {
        return Vec::new();
    }
    let own: &[FieldId] = match (target, mode) {
        (Target::Parameter, Mode::Set { .. } | Mode::Momentary { .. }) => &[Value],
        (Target::Parameter, Mode::Toggle { .. }) => &[A, B],
        (Target::Parameter, Mode::Step { .. }) => &[Amount, Wrap],
        (Target::Parameter, Mode::Follow { .. }) => &[Min, Max, Pickup],
        (Target::Parameter, Mode::Relative { .. }) => &[Encoding],
        (Target::Switch, Mode::Set { .. }) => &[Value],
        _ => &[],
    };
    let window: &[FieldId] = match mode {
        _ if source != Source::Level => &[],
        Mode::Relative { .. } => &[],
        _ => &[Low, High],
    };
    [window, own].concat()
}

/// One stepper tap on `field`: target values move by the parameter's own steps, choices go to
/// the next one. `false` when the field is not one of the mode's.
pub(super) fn step_field(
    mode: &mut Mode,
    window: &mut Window,
    field: FieldId,
    ticks: i32,
    param: Option<&Param>,
) -> bool {
    let ticks = ticks.clamp(-127, 127);
    let value = |v: &mut f32| {
        *v = match param {
            Some(param) => param.spec.scale.encoder(*v, ticks),
            // A switch: any tap flips on and off.
            None => f32::from(u8::from(*v < 0.5)),
        }
    };
    match (field, mode) {
        (FieldId::Value, Mode::Set { value: v } | Mode::Momentary { value: v }) => value(v),
        (FieldId::A, Mode::Toggle { a, .. }) => value(a),
        (FieldId::B, Mode::Toggle { b, .. }) => value(b),
        (FieldId::Min, Mode::Follow { min, .. }) => value(min),
        (FieldId::Max, Mode::Follow { max, .. }) => value(max),
        (FieldId::Pickup, Mode::Follow { pickup, .. }) => *pickup = !*pickup,
        (FieldId::Amount, Mode::Step { amount, .. }) => {
            let max = i32::from(MAX_AMOUNT);
            let next = (i32::from(*amount) + ticks).clamp(-max, max);
            // 0 would do nothing: skip over it.
            *amount = match next {
                0 => ticks.signum(),
                next => next,
            } as i8;
        }
        (FieldId::Wrap, Mode::Step { wrap, .. }) => *wrap = !*wrap,
        (FieldId::Encoding, Mode::Relative { encoding }) => {
            let at = ENCODINGS.iter().position(|e| e == encoding).unwrap_or(0) as i32;
            *encoding = ENCODINGS[(at + ticks).rem_euclid(ENCODINGS.len() as i32) as usize];
        }
        (FieldId::Low, _) => {
            window.low = (i32::from(window.low) + ticks).clamp(0, i32::from(window.high)) as u8
        }
        (FieldId::High, _) => {
            window.high = (i32::from(window.high) + ticks).clamp(i32::from(window.low), 127) as u8
        }
        _ => return false,
    }
    true
}

pub(super) fn encoding_text(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::TwosComplement => "2's compl.",
        Encoding::Offset => "Offset 64",
        Encoding::SignBit => "Sign bit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three encoder codings as controllers send them.
    #[test]
    fn encoder_ticks() {
        let cases = [
            (
                Encoding::TwosComplement,
                [1, 63, 127, 65, 0],
                [1, 63, -1, -63, 0],
            ),
            (Encoding::Offset, [65, 127, 63, 0, 64], [1, 63, -1, -64, 0]),
            (Encoding::SignBit, [1, 63, 65, 127, 64], [1, 63, -1, -63, 0]),
        ];
        for (encoding, values, expected) in cases {
            let got = values.map(|v| ticks(encoding, v));
            assert_eq!(got, expected, "{encoding:?}");
        }
    }

    /// A press on entering the window or on a repeated value inside it, a release on leaving;
    /// a knob moving inside the window presses once.
    #[test]
    fn window_edges() {
        let window = Window::UPPER;
        let run = |values: &[u8]| {
            let mut previous = None;
            (values.iter())
                .map(|&value| {
                    let edge = edge(value, previous, window);
                    previous = Some(value);
                    edge
                })
                .collect::<Vec<_>>()
        };
        let (press, release) = (Some(Input::Press), Some(Input::Release));
        assert_eq!(run(&[127, 0, 127, 0]), [press, release, press, release]);
        assert_eq!(run(&[127, 127, 127]), [press, press, press]);
        assert_eq!(
            run(&[10, 60, 64, 70, 90, 50]),
            [None, None, press, None, None, release]
        );
        let low = Window { low: 0, high: 20 };
        assert_eq!(edge(5, Some(30), low), press);
        assert_eq!(edge(30, Some(5), low), release);
    }

    #[test]
    fn follow_maps_the_window_and_clamps() {
        let window = Window { low: 10, high: 110 };
        assert_eq!(follow(0, window, 0.2, 0.8), 0.2);
        assert_eq!(follow(60, window, 0.2, 0.8), 0.5);
        assert_eq!(follow(127, window, 0.2, 0.8), 0.8);
        assert_eq!(follow(60, window, 1.0, 0.0), 0.5);
        assert_eq!(follow(127, Window::FULL, 1.0, 0.0), 0.0);
    }
}
