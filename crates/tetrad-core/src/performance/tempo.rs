//! Tempo footswitch tap gesture.
//!
//! The tap period itself is measured by the `zencoder` kernel driver: libzenhal
//! `CZencoder::GetTempoEvent` (`0x168b0`) returns the value of an input event of
//! type 3. ZenUI only counts presses and applies that value. Evidence:
//! press handler `0x20db38`, tempo apply gate `0x382850`, double-tap timer
//! `+0x184` single-shot 3000 ms (`0x210260`), count `+0x19c`, reset `0x20d1c4`
//! (also called when `swapTempoAndTunerAccess` changes, `0x4201d4`).

use crate::workflow::Millis;

/// Interval of the single-shot double-tap timer.
pub const TAP_WINDOW_MS: u64 = 3000;

/// TEMPO held for `holdTiming` (`0x20db38`): tuner, or tempo when
/// `swapTempoAndTunerAccess` is set.
pub fn hold_outcome(swap_tempo_and_tuner: bool) -> TapOutcome {
    if swap_tempo_and_tuner {
        TapOutcome::ShowTempo
    } else {
        TapOutcome::ShowTuner
    }
}
pub const MIN_TEMPO_BPM: u16 = 40;
pub const MAX_TEMPO_BPM: u16 = 240;
/// `0x382850` ignores values outside this range before clamping.
pub const ACCEPTED_TEMPO_EVENT: std::ops::RangeInclusive<i32> = 20..=1022;

/// Applies the `0x382850` gate: unsigned `(t − 20) < 1003`, then clamp 40..=240.
pub fn accept_tempo_event(value: i32) -> Option<u16> {
    ACCEPTED_TEMPO_EVENT
        .contains(&value)
        .then(|| value.clamp(i32::from(MIN_TEMPO_BPM), i32::from(MAX_TEMPO_BPM)) as u16)
}

/// Facts the caller reads at press time. Field meanings follow the code paths;
/// the identity of each visibility item is a UI fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapContext {
    /// ModeController predicate `0x589924`; true ignores the press entirely.
    pub blocked: bool,
    /// The press is the configured tempo footswitch alone (exact stomp mask)
    /// and that footswitch is not disabled.
    pub tempo_switch_alone: bool,
    /// Visibility tested before counting; visible resets the count.
    pub counting_suppressed: bool,
    /// Tempo popup visibility tested for the third and later taps.
    pub tempo_popup_visible: bool,
    /// `swapTempoAndTunerAccess` setting (default false).
    pub swap_tempo_and_tuner: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapOutcome {
    Ignored,
    Counted,
    /// Second tap: show the tempo popup with the current tempo (gated by
    /// [`accept_tempo_event`]).
    ShowTempo,
    /// Second tap with swapped access: open the tuner. The count resets.
    ShowTuner,
    /// Third or later tap while the popup is visible: apply the driver tempo.
    ApplyDriverTempo,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TapTempo {
    count: u32,
    deadline: Option<Millis>,
}

impl TapTempo {
    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn deadline(&self) -> Option<Millis> {
        self.deadline
    }

    /// A footswitch value change. Values below 0.5 are releases.
    pub fn footswitch(&mut self, value: f32, now: Millis, ctx: TapContext) -> TapOutcome {
        self.expire(now);
        if ctx.blocked {
            return TapOutcome::Ignored;
        }
        if value < 0.5 || value.is_nan() {
            return TapOutcome::Ignored;
        }
        if ctx.tempo_switch_alone && !ctx.counting_suppressed {
            self.count = self.count.saturating_add(1);
            self.deadline = Some(Millis(now.0.saturating_add(TAP_WINDOW_MS)));
        } else {
            self.count = 0;
        }
        match self.count {
            2 if ctx.swap_tempo_and_tuner => {
                self.reset();
                TapOutcome::ShowTuner
            }
            2 => TapOutcome::ShowTempo,
            n if n > 2 && ctx.tempo_popup_visible => TapOutcome::ApplyDriverTempo,
            0 => TapOutcome::Ignored,
            _ => TapOutcome::Counted,
        }
    }

    /// Timer expiry resets the count (inferred link: `0x20d1c4` is the reset body).
    /// Stock stops only this timer when the popup opens, so the window stays.
    pub fn expire(&mut self, now: Millis) {
        if self.deadline.is_some_and(|deadline| now.0 >= deadline.0) {
            self.reset();
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAP: TapContext = TapContext {
        blocked: false,
        tempo_switch_alone: true,
        counting_suppressed: false,
        tempo_popup_visible: false,
        swap_tempo_and_tuner: false,
    };

    #[test]
    fn driver_tempo_gate_ignores_out_of_range_then_clamps() {
        assert_eq!(accept_tempo_event(19), None);
        assert_eq!(accept_tempo_event(20), Some(40));
        assert_eq!(accept_tempo_event(120), Some(120));
        assert_eq!(accept_tempo_event(1022), Some(240));
        assert_eq!(accept_tempo_event(1023), None);
        assert_eq!(accept_tempo_event(-5), None);
    }

    #[test]
    fn second_tap_opens_tempo_or_tuner_and_window_expiry_resets() {
        let mut tap = TapTempo::default();
        assert_eq!(tap.footswitch(1.0, Millis(0), TAP), TapOutcome::Counted);
        assert_eq!(tap.footswitch(0.0, Millis(10), TAP), TapOutcome::Ignored);
        assert_eq!(tap.footswitch(1.0, Millis(500), TAP), TapOutcome::ShowTempo);
        let visible = TapContext {
            tempo_popup_visible: true,
            ..TAP
        };
        assert_eq!(
            tap.footswitch(1.0, Millis(900), visible),
            TapOutcome::ApplyDriverTempo
        );

        let mut tap = TapTempo::default();
        tap.footswitch(1.0, Millis(0), TAP);
        assert_eq!(tap.footswitch(1.0, Millis(3000), TAP), TapOutcome::Counted);
        let swapped = TapContext {
            swap_tempo_and_tuner: true,
            ..TAP
        };
        assert_eq!(
            tap.footswitch(1.0, Millis(3100), swapped),
            TapOutcome::ShowTuner
        );
        assert_eq!(tap.count(), 0);

        let other = TapContext {
            tempo_switch_alone: false,
            ..TAP
        };
        tap.footswitch(1.0, Millis(4000), TAP);
        assert_eq!(
            tap.footswitch(1.0, Millis(4100), other),
            TapOutcome::Ignored
        );
        assert_eq!(tap.count(), 0);
    }

    #[test]
    fn popup_keeps_the_tap_window_running() {
        let mut tap = TapTempo::default();
        tap.footswitch(1.0, Millis(0), TAP);
        assert_eq!(tap.footswitch(1.0, Millis(500), TAP), TapOutcome::ShowTempo);
        assert_eq!(tap.deadline(), Some(Millis(3500)));
        tap.expire(Millis(3500));
        assert_eq!(tap.count(), 0);
    }

    #[test]
    fn held_tempo_opens_tuner_unless_access_is_swapped() {
        assert_eq!(hold_outcome(false), TapOutcome::ShowTuner);
        assert_eq!(hold_outcome(true), TapOutcome::ShowTempo);
    }
}
