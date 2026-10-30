//! Double presses and combos of the Custom mode (owned, Tetrad): which action a footswitch
//! press runs. A switch with neither runs its press at once. A switch in a combo waits up to
//! [`COMBO_PRESS`] for a partner; a switch with a double press waits up to [`DOUBLE_PRESS`]
//! from its first press for a second one. A press that waited runs when the wait ends: held
//! if the switch is still down (its release ends it), else as a tap.
//!
//! Time comes in with each edge. [`Gestures::deadline`] says when to call
//! [`Gestures::expire`].

use std::time::Duration;

use tetrad_ui::{COMBO_PRESS, CUSTOM_SLOTS, DOUBLE_PRESS, Gesture};

/// What a switch of the current page has beyond its press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Shape {
    pub double: bool,
    /// Bit `1 << n`: a combo with switch `n`.
    pub partners: u16,
}

/// An action to start, or a switch that came up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fire {
    /// Start the `gesture` action of `slot`; a combo starts on its lower switch.
    Down { slot: u8, gesture: Gesture },
    /// End what the switch holds.
    Up(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pending {
    /// The first press.
    at: Duration,
    /// The switch is still down.
    down: bool,
    /// Waiting for a combo partner; after that, for a second press if it has a double press.
    combo: bool,
}

impl Pending {
    fn until(self) -> Duration {
        self.at
            + if self.combo {
                COMBO_PRESS
            } else {
                DOUBLE_PRESS
            }
    }
}

/// The presses that wait, one per switch of the current page.
#[derive(Debug, Default)]
pub struct Gestures {
    pending: [Option<Pending>; CUSTOM_SLOTS],
}

impl Gestures {
    pub fn deadline(&self) -> Option<Duration> {
        self.pending
            .iter()
            .flatten()
            .map(|pending| pending.until())
            .min()
    }

    /// Drops every waiting press: it does not run late.
    pub fn clear(&mut self) {
        self.pending = Default::default();
    }

    /// A press or release of `slot` at `now`.
    pub fn edge(
        &mut self,
        slot: u8,
        pressed: bool,
        now: Duration,
        shapes: &[Shape; CUSTOM_SLOTS],
    ) -> Vec<Fire> {
        let mut fired = self.expire(now, shapes);
        let index = usize::from(slot);
        let Some(shape) = shapes.get(index).copied() else {
            return fired;
        };
        let press = |gesture| Fire::Down { slot, gesture };
        match (pressed, self.pending[index]) {
            (true, Some(Pending { down: false, .. })) => {
                self.pending[index] = None;
                fired.push(press(Gesture::Double));
            }
            (true, Some(_)) => {}
            (true, None) => {
                let partner = (0..CUSTOM_SLOTS as u8).find(|&other| {
                    shape.partners & (1 << other) != 0
                        && self.pending[usize::from(other)]
                            .is_some_and(|pending| pending.combo && pending.down)
                });
                if let Some(partner) = partner {
                    self.pending[usize::from(partner)] = None;
                    fired.push(Fire::Down {
                        slot: slot.min(partner),
                        gesture: Gesture::Combo(slot.max(partner)),
                    });
                } else if shape.partners != 0 || shape.double {
                    self.pending[index] = Some(Pending {
                        at: now,
                        down: true,
                        combo: shape.partners != 0,
                    });
                } else {
                    fired.push(press(Gesture::Press));
                }
            }
            // Up inside the combo window: no combo now.
            (false, Some(pending)) if pending.combo && shape.double => {
                self.pending[index] = Some(Pending {
                    down: false,
                    combo: false,
                    ..pending
                });
            }
            (false, Some(Pending { combo: true, .. })) => {
                self.pending[index] = None;
                fired.extend([press(Gesture::Press), Fire::Up(slot)]);
            }
            (false, Some(pending)) => {
                self.pending[index] = Some(Pending {
                    down: false,
                    ..pending
                });
            }
            (false, None) => fired.push(Fire::Up(slot)),
        }
        fired
    }

    /// The waits that ended by `now`.
    pub fn expire(&mut self, now: Duration, shapes: &[Shape; CUSTOM_SLOTS]) -> Vec<Fire> {
        let mut fired = Vec::new();
        for (slot, entry) in (0..).zip(&mut self.pending) {
            let Some(mut pending) = *entry else {
                continue;
            };
            if pending.until() > now {
                continue;
            }
            // No partner came: a switch with a double press waits on for a second press.
            if pending.combo && shapes[usize::from(slot)].double {
                pending.combo = false;
                *entry = Some(pending);
                if pending.until() > now {
                    continue;
                }
            }
            *entry = None;
            fired.push(Fire::Down {
                slot,
                gesture: Gesture::Press,
            });
            if !pending.down {
                fired.push(Fire::Up(slot));
            }
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: u8 = 3;
    const C: u8 = 2;

    fn ms(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }

    /// Runs `edges` (switch, pressed, at ms) and then the wait at 1 s, and lists what fired.
    fn run(shapes: &[Shape; CUSTOM_SLOTS], edges: &[(u8, bool, u64)]) -> Vec<(u64, Fire)> {
        let mut gestures = Gestures::default();
        let mut fired = Vec::new();
        for &(slot, pressed, at) in edges {
            let now = ms(at);
            while let Some(due) = gestures.deadline().filter(|&due| due <= now) {
                let at = due.as_millis() as u64;
                fired.extend(gestures.expire(due, shapes).into_iter().map(|f| (at, f)));
            }
            let out = gestures.edge(slot, pressed, now, shapes);
            fired.extend(out.into_iter().map(|f| (at, f)));
        }
        while let Some(due) = gestures.deadline() {
            let at = due.as_millis() as u64;
            fired.extend(gestures.expire(due, shapes).into_iter().map(|f| (at, f)));
        }
        fired
    }

    /// The press timing of the plan: what runs, and when, for a switch with nothing, a double
    /// press, a combo, or both.
    #[test]
    fn presses_run_the_press_double_press_or_combo_on_time() {
        let down = |slot, gesture| Fire::Down { slot, gesture };
        let press = |slot| down(slot, Gesture::Press);
        let plain = [Shape::default(); CUSTOM_SLOTS];
        let mut double = plain;
        double[usize::from(D)].double = true;
        let mut combo = plain;
        combo[usize::from(C)].partners = 1 << D;
        combo[usize::from(D)].partners = 1 << C;
        let mut both = combo;
        both[usize::from(D)].double = true;
        let tap = |at| [(D, true, at), (D, false, at + 50)];

        // 1. No double press, no combo: at once.
        assert_eq!(run(&plain, &tap(0)), [(0, press(D)), (50, Fire::Up(D))]);
        // 2. One tap of a switch with a double press: a tap at 300 ms.
        assert_eq!(run(&double, &tap(0)), [(300, press(D)), (300, Fire::Up(D))]);
        // 3. Two taps: the double press at the second press; the press never runs.
        assert_eq!(
            run(&double, &[tap(0), tap(150)].concat()),
            [(150, down(D, Gesture::Double)), (200, Fire::Up(D))]
        );
        // 4. Held: the press at 300 ms, ended by the release.
        assert_eq!(
            run(&double, &[(D, true, 0), (D, false, 900)]),
            [(300, press(D)), (900, Fire::Up(D))]
        );
        // 5. C then D within 80 ms: the combo; the first release ends it, the other is quiet.
        assert_eq!(
            run(
                &combo,
                &[
                    (C, true, 0),
                    (D, true, 60),
                    (D, false, 400),
                    (C, false, 450)
                ]
            ),
            [
                (60, down(C, Gesture::Combo(D))),
                (400, Fire::Up(D)),
                (450, Fire::Up(C))
            ]
        );
        // 6. D alone: its press after 80 ms; a release inside the window taps at once.
        assert_eq!(
            run(&combo, &[(D, true, 0), (D, false, 500)]),
            [(80, press(D)), (500, Fire::Up(D))]
        );
        assert_eq!(run(&combo, &tap(0)), [(50, press(D)), (50, Fire::Up(D))]);
        // 7. Too late for a combo: two presses.
        assert_eq!(
            run(&combo, &[(C, true, 0), (D, true, 120)]),
            [(80, press(C)), (200, press(D))]
        );
        // 8. A combo and a double press: two quick taps are the double press; one tap waits
        // the double window from the first press.
        assert_eq!(
            run(&both, &[tap(0), tap(100)].concat()),
            [(100, down(D, Gesture::Double)), (150, Fire::Up(D))]
        );
        assert_eq!(run(&both, &tap(0)), [(300, press(D)), (300, Fire::Up(D))]);
    }

    /// A cleared wait (a page change, a layout edit) runs nothing, and its release is quiet
    /// to the waits: it only ends what the switch holds.
    #[test]
    fn a_cleared_wait_runs_nothing() {
        let mut shapes = [Shape::default(); CUSTOM_SLOTS];
        shapes[usize::from(D)].double = true;
        let mut gestures = Gestures::default();
        assert!(gestures.edge(D, true, ms(0), &shapes).is_empty());
        gestures.clear();
        assert_eq!(gestures.deadline(), None);
        assert_eq!(gestures.edge(D, false, ms(50), &shapes), [Fire::Up(D)]);
    }
}
