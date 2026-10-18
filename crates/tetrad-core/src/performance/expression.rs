//! Expression pedals: calibration, heel/toe parameter ranges and bypass
//! auto-engage.
//!
//! Evidence: `ExpressionController` (ctor `0x321798`, pedal step `0x320cf0`,
//! dispatch `0x322cf0`/`0x323050`, override `0x3221e8`/`0x322108`), bypass state
//! `0x6df298`, live event `0x6df4e0`, delay timer `0x6df618`, static state
//! `0x6df7fc`, preset load `0x680614`, and the in-binary `ExpressionPedalTest`
//! bodies (`0x437eac`, `0x4386d8`, `0x43a9b0`, `0x43bd6c`, `0x437914`).

use crate::workflow::Millis;

/// Saved calibration defaults (`ExpressionsSettings/expressionNMin|Max`).
pub const DEFAULT_CALIBRATION: (i32, i32) = (0, 255);
/// Calibration shrinks each observed extreme by this dead zone.
pub const CALIBRATION_DEAD_ZONE: i32 = 5;
/// Heel zone for latch emulation and the heel-toe bypass type.
pub const HEEL_ZONE: f32 = 0.05;
/// Defaults of the bypass assignment dialog (`0x34d9b8`, `DAT_01547b50`).
pub const DEFAULT_BYPASS_DELAY_MS: u32 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Pedal {
    Exp1 = 1,
    Exp2 = 2,
}

impl Pedal {
    /// `Param.expression`: 0 unassigned, 1 EXP1, any other value EXP2 (`0x3242ec`).
    pub fn from_assignment(value: i32) -> Option<Self> {
        match value {
            0 => None,
            1 => Some(Self::Exp1),
            _ => Some(Self::Exp2),
        }
    }

    /// Plug-state bit in the PluggedController word (`0x67a9b8`).
    pub fn plug_mask(self) -> u16 {
        match self {
            Self::Exp1 => 0x80,
            Self::Exp2 => 0x40,
        }
    }
}

/// Raw HAL value to pedal position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PedalCalibration {
    min: i32,
    max: i32,
    calibrating: bool,
}

impl Default for PedalCalibration {
    fn default() -> Self {
        Self::saved(DEFAULT_CALIBRATION.0, DEFAULT_CALIBRATION.1)
    }
}

impl PedalCalibration {
    pub fn saved(min: i32, max: i32) -> Self {
        Self {
            min,
            max,
            calibrating: false,
        }
    }

    /// `0x32103c`: reset extremes so the next samples define them.
    pub fn begin(&mut self) {
        *self = Self {
            min: i32::MAX,
            max: i32::MIN,
            calibrating: true,
        };
    }

    /// Both extremes have been observed; only then may it be saved (`0x320a34`).
    pub fn is_valid(&self) -> bool {
        self.min != i32::MAX && self.max != i32::MIN
    }

    /// Ends calibration and returns the values to persist, if valid.
    pub fn finish(&mut self) -> Option<(i32, i32)> {
        self.is_valid().then(|| {
            self.calibrating = false;
            (self.min, self.max)
        })
    }

    pub fn range(&self) -> (i32, i32) {
        (self.min, self.max)
    }

    /// `p = clamp((raw − min) · 1/(max − min), 0, 1)` with no smoothing. `None`
    /// for NaN, which stock drops before dispatch.
    pub fn position(&mut self, raw: i32) -> Option<f32> {
        if self.calibrating {
            if raw < self.min {
                self.min = raw.saturating_add(CALIBRATION_DEAD_ZONE);
            }
            if raw > self.max {
                self.max = raw.saturating_sub(CALIBRATION_DEAD_ZONE);
            }
        }
        let scale = 1.0 / (i64::from(self.max) - i64::from(self.min)) as f32;
        let p = (i64::from(raw) - i64::from(self.min)) as f32 * scale;
        let p = p.clamp(0.0, 1.0);
        (!p.is_nan()).then_some(p)
    }
}

/// MIDI injection (`0x321500`): `cc / 127`.
pub fn midi_position(cc: u8) -> f32 {
    f32::from(cc) / 127.0
}

/// Per-pedal live state: position, plug/MIDI sources and the global latch
/// emulation that toggles on each fall into the heel zone.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PedalState {
    pub position: Option<f32>,
    pub plugged: bool,
    pub midi_active: bool,
    emulated_switch: f32,
}

impl PedalState {
    /// A new position from the HAL or MIDI. Returns the position to dispatch.
    pub fn update(&mut self, position: f32, from_midi: bool) -> Option<f32> {
        if from_midi {
            self.midi_active = true;
        }
        self.observe(position);
        self.position.filter(|_| self.is_active())
    }

    /// Records a position and advances the latch emulation, without the dispatch gate.
    pub fn observe(&mut self, position: f32) {
        if let Some(previous) = self.position
            && previous > HEEL_ZONE
            && position <= HEEL_ZONE
        {
            self.emulated_switch = if self.emulated_switch <= 0.5 {
                1.0
            } else {
                0.0
            };
        }
        self.position = Some(position);
    }

    /// Dispatch gate of `0x322cf0`: plugged or fed by MIDI.
    pub fn is_active(&self) -> bool {
        self.plugged || self.midi_active
    }

    pub fn emulated_switch(&self) -> f32 {
        self.emulated_switch
    }

    /// Test hook `resetExpressionPedalEmulatedSwitchValues`.
    pub fn reset_emulated_switch(&mut self) {
        self.emulated_switch = 0.0;
    }
}

/// `Param.expression_min/max`: normalized values at heel (p=0) and toe (p=1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpressionRange {
    pub heel: f32,
    pub toe: f32,
}

impl ExpressionRange {
    /// Live dispatch (`0x322cf0`): separate multiply and add.
    pub fn live_value(self, p: f32) -> f32 {
        self.heel + p * (self.toe - self.heel)
    }

    /// DSP push override (`0x322108`): fused multiply-add. This path replaces
    /// the scene value while the assignment is active (`0x3221e8`).
    pub fn pushed_value(self, p: f32) -> f32 {
        p.mul_add(self.toe - self.heel, self.heel)
    }
}

/// The value sent to the DSP for a parameter: the pedal value while assigned
/// and active, else the stored scene value. Unassign/unplug restores the stored
/// scene values (`0x5216e4`); expression output never writes scene storage.
pub fn effective_value(
    scene_value: f32,
    assignment: Option<(ExpressionRange, &PedalState)>,
) -> f32 {
    match assignment {
        Some((range, pedal)) if pedal.is_active() => pedal
            .position
            .map_or(scene_value, |p| range.pushed_value(p)),
        _ => scene_value,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum BypassSwitchType {
    /// Bypass after the pedal is still for the delay; engage on movement.
    Stop = 0,
    /// Threshold at 0.5, immediate.
    Switch = 1,
    /// Engage above the heel zone; bypass after the delay in the heel zone.
    HeelToe = 2,
}

impl BypassSwitchType {
    pub fn from_raw(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Stop),
            1 => Some(Self::Switch),
            2 => Some(Self::HeelToe),
            _ => None,
        }
    }
}

/// `ExpressionBypassInfo` (Model field 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BypassInfo {
    pub kind: BypassSwitchType,
    pub invert: bool,
    pub delay_ms: u32,
    pub latch_emulation: bool,
}

impl Default for BypassInfo {
    fn default() -> Self {
        Self {
            kind: BypassSwitchType::HeelToe,
            invert: false,
            delay_ms: DEFAULT_BYPASS_DELAY_MS,
            latch_emulation: false,
        }
    }
}

impl BypassInfo {
    /// Static state used on preset load and plug-in (`0x6df7fc`), from the
    /// untrimmed position or the emulated switch. `true` means bypassed.
    pub fn bypassed_at(self, value: f32) -> bool {
        let v = if self.invert { 1.0 - value } else { value };
        match self.kind {
            BypassSwitchType::Switch => v > 0.5,
            BypassSwitchType::HeelToe => v <= HEEL_ZONE,
            BypassSwitchType::Stop => true,
        }
    }
}

/// `bypass_expression` trim. Load repairs `min == max == 0` to max 1 (`0x680614`).
pub fn loaded_bypass_trim(min: f32, max: f32) -> ExpressionRange {
    let toe = if min == 0.0 && max == 0.0 { 1.0 } else { max };
    ExpressionRange { heel: min, toe }
}

/// Auto-engage state of one bypass assignment (`0x6df298`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoEngage {
    pub info: BypassInfo,
    pub trim: ExpressionRange,
    last: f32,
    first: bool,
    last_event: Millis,
}

impl AutoEngage {
    /// `initial` seeds the last value (first constructor argument). The event
    /// time starts at the clock epoch, so an idle Stop assignment bypasses on
    /// the first poll after its delay.
    pub fn new(info: BypassInfo, trim: ExpressionRange, initial: f32) -> Self {
        Self {
            info,
            trim,
            last: initial,
            first: true,
            last_event: Millis(0),
        }
    }

    /// Live pedal event (`0x6df4e0`). Returns the new bypass state to apply.
    pub fn event(
        &mut self,
        pedal: &PedalState,
        p: f32,
        from_midi: bool,
        bypassed: bool,
        now: Millis,
    ) -> Option<bool> {
        let first = std::mem::take(&mut self.first);
        let mut v = if self.info.latch_emulation {
            pedal.emulated_switch()
        } else {
            self.trim.live_value(p)
        };
        if self.info.invert {
            v = 1.0 - v;
        }
        self.last_event = now;
        match self.info.kind {
            BypassSwitchType::Switch => {
                if from_midi {
                    v = 1.0 - v;
                }
                let target = v >= 0.5;
                (target != bypassed).then_some(target)
            }
            BypassSwitchType::HeelToe => {
                self.last = v;
                (v > HEEL_ZONE && bypassed).then_some(false)
            }
            BypassSwitchType::Stop => {
                if v == self.last {
                    return None;
                }
                self.last = v;
                if first {
                    return None;
                }
                bypassed.then_some(false)
            }
        }
    }

    /// Delay timer (`0x6df618`) with the untrimmed position. Bypasses once the
    /// pedal has been idle for longer than the delay.
    pub fn poll(&self, p: f32, bypassed: bool, now: Millis) -> Option<bool> {
        if bypassed
            || self.info.kind == BypassSwitchType::Switch
            || now.0.saturating_sub(self.last_event.0) <= u64::from(self.info.delay_ms)
        {
            return None;
        }
        match self.info.kind {
            BypassSwitchType::Stop => Some(true),
            BypassSwitchType::HeelToe if self.last <= HEEL_ZONE => {
                // Stock compares the inverted last value with the raw position.
                (!self.info.invert || self.last != p).then_some(true)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active() -> PedalState {
        PedalState {
            plugged: true,
            ..PedalState::default()
        }
    }

    #[test]
    fn calibration_keeps_a_dead_zone_and_assignment_codes_map_to_pedals() {
        let mut cal = PedalCalibration::default();
        assert_eq!(cal.position(255), Some(1.0));
        cal.begin();
        assert!(!cal.is_valid());
        cal.position(10);
        cal.position(250);
        assert_eq!(cal.range(), (15, 245));
        assert_eq!(cal.position(10), Some(0.0));
        assert_eq!(cal.finish(), Some((15, 245)));
        assert_eq!(Pedal::from_assignment(0), None);
        assert_eq!(Pedal::from_assignment(7), Some(Pedal::Exp2));
        assert_eq!(midi_position(127), 1.0);
    }

    #[test]
    fn calibration_widens_extremes_inside_the_dead_zone() {
        let mut cal = PedalCalibration::default();
        cal.begin();
        // The first sample sets min = v + 5 and max = v - 5, so it reads as 0.5.
        assert_eq!(cal.position(100), Some(0.5));
        assert_eq!(cal.range(), (105, 95));
        assert_eq!(cal.position(200), Some(1.0));
        assert_eq!(cal.position(50), Some(0.0));
        assert_eq!(cal.range(), (55, 195));
        assert_eq!(cal.finish(), Some((55, 195)));
    }

    #[test]
    fn heel_toe_range_overrides_scene_value_only_while_active() {
        // testParameterRange: heel 0.5, toe 0.75.
        let range = ExpressionRange {
            heel: 0.5,
            toe: 0.75,
        };
        let mut pedal = active();
        assert_eq!(pedal.update(1.0, false), Some(1.0));
        assert_eq!(effective_value(0.7, Some((range, &pedal))), 0.75);
        pedal.update(0.0, false);
        assert_eq!(effective_value(0.7, Some((range, &pedal))), 0.5);
        // testUnpluggingExpPedal: unplugged restores the stored value.
        pedal.plugged = false;
        assert_eq!(effective_value(0.7, Some((range, &pedal))), 0.7);
        assert_eq!(pedal.update(0.3, false), None);
    }

    #[test]
    fn latch_emulation_toggles_on_each_fall_into_the_heel_zone() {
        let mut pedal = active();
        pedal.update(1.0, false);
        pedal.update(0.0, false);
        assert_eq!(pedal.emulated_switch(), 1.0);
        pedal.update(0.04, false);
        assert_eq!(pedal.emulated_switch(), 1.0);
        pedal.update(1.0, false);
        pedal.update(0.0, false);
        assert_eq!(pedal.emulated_switch(), 0.0);
    }

    #[test]
    fn inverted_heel_toe_poll_bypasses_only_when_the_position_moved() {
        let info = BypassInfo {
            invert: true,
            delay_ms: 500,
            ..BypassInfo::default()
        };
        let trim = ExpressionRange {
            heel: 0.0,
            toe: 1.0,
        };
        let engage = AutoEngage::new(info, trim, 0.0);
        assert_eq!(engage.poll(0.0, false, Millis(600)), None);
        assert_eq!(engage.poll(0.3, false, Millis(600)), Some(true));
    }

    #[test]
    fn bypass_types_follow_the_recovered_tests() {
        let trim = loaded_bypass_trim(0.0, 0.0);
        assert_eq!(trim.toe, 1.0);
        let pedal = active();
        // Heel-toe, delay 500: engage at toe now; bypass only after the delay.
        let info = BypassInfo {
            delay_ms: 500,
            ..BypassInfo::default()
        };
        let mut heel_toe = AutoEngage::new(info, trim, 0.0);
        assert_eq!(
            heel_toe.event(&pedal, 1.0, false, true, Millis(0)),
            Some(false)
        );
        assert_eq!(heel_toe.event(&pedal, 0.0, false, false, Millis(100)), None);
        assert_eq!(heel_toe.poll(0.0, false, Millis(600)), None);
        assert_eq!(heel_toe.poll(0.0, false, Millis(601)), Some(true));
        // Switch: immediate at 0.5; MIDI inverts.
        let switch = BypassInfo {
            kind: BypassSwitchType::Switch,
            ..BypassInfo::default()
        };
        let mut sw = AutoEngage::new(switch, trim, 0.0);
        assert_eq!(sw.event(&pedal, 1.0, false, false, Millis(0)), Some(true));
        assert_eq!(sw.event(&pedal, 1.0, true, true, Millis(1)), Some(false));
        // Stop, delay 1000: the first event only records, then movement engages
        // and stillness beyond the delay bypasses (testBypassStop).
        let stop = BypassInfo {
            kind: BypassSwitchType::Stop,
            delay_ms: 1000,
            ..BypassInfo::default()
        };
        let mut st = AutoEngage::new(stop, trim, 0.0);
        assert_eq!(st.event(&pedal, 0.0, false, true, Millis(0)), None);
        assert_eq!(st.event(&pedal, 0.5, false, true, Millis(10)), Some(false));
        assert_eq!(st.poll(0.5, false, Millis(700)), None);
        assert_eq!(st.poll(0.5, false, Millis(1011)), Some(true));
        assert!(stop.bypassed_at(0.3));
        assert!(BypassInfo::default().bypassed_at(0.0));
        assert!(!switch.bypassed_at(0.5));
    }
}
