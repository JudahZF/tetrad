use super::*;

/// DSPController Looper state. Keep one across block replacement.
/// Raw states and status words are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooperPlayback {
    /// `+0xb824`, setter 0x513bbc.
    pub cached_state: u32,
    /// `+0xb828`: the last record the controller slot 0x513c94 received.
    pub incoming: LooperStatus,
    /// `+0xb880`, setter 0x513dbc.
    pub oneshot: bool,
    /// `+0xb881`, setter 0x513e24.
    pub dupwait: bool,
    /// `+0xf058`: set it when a Looper block is created (factory 0x50eef8).
    /// The next status is then delivered even if unchanged, and its state
    /// becomes `cached_state` (0x513c94).
    pub adopt_next_state: bool,
    /// `+0xb882` (setter 0x513df0): the last QUANTIZE send was above 0 (0x55e5ac, after the
    /// send). Stock pushes it to Cortex Control (0x190c4c).
    pub quantize_enabled: bool,
}

/// Clock-reset inputs to 0x55743c and 0x5138dc. No setting is guessed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LooperClock {
    /// Metronome slot 4 (0x55743c: `vcmp` with 0.0). Only ±0.0 permits a
    /// reset; NaN blocks it.
    pub metronome_slot_4: f32,
    /// 0x32f514: MidiController `+0xfd`, the USB clock is running.
    pub midi_gate_f514: bool,
    /// 0x32fbb4: MidiController `+0xfe`, the DIN clock is running.
    pub midi_gate_fbb4: bool,
}

/// P19's MIDI settings (0x55e93c). It sends 1 only when `enabled` and not
/// `inhibited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooperMidiClockStart {
    /// 0x32dba4: MidiController `+0x51`, ClockInEn.
    pub enabled: bool,
    /// 0x32db8c: MidiController `+0x4c`, ClockOutEn is not Off.
    pub inhibited: bool,
}

/// Device gate 0x499a60 (platform 2 or 3) at construction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LooperProduct {
    #[default]
    Qc,
    Mini,
}

/// `None` means unavailable, not disabled. P20's index is kept in the block.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LooperContext {
    /// Whether 0x49a754 permits P13–16 changes.
    pub mode_changes_allowed: Option<bool>,
    pub clock: Option<LooperClock>,
    pub midi_clock_start: Option<LooperMidiClockStart>,
}

/// Driver byte `+0xd4`, which selects the destructor branch (0x55d534).
/// The constructor clears it; the factory 0x50eef8 sets it before it deletes
/// an old driver (see [`Looper::replace`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownMode {
    Local,
    Global,
}

fn parameter(parameter: u32, value: f32) -> LooperWrite {
    LooperWrite::Request(TunerLooperRequest::LooperParameter {
        parameter,
        value_bits: value.to_bits(),
    })
}

fn event(event: u32) -> LooperWrite {
    LooperWrite::Request(TunerLooperRequest::LooperEvent { track: 0, event })
}

impl LooperPlayback {
    fn transition(&mut self, action: u32, state: u32, out: &mut Vec<LooperWrite>) {
        out.push(event(action)); // 0x55e140: event precedes cached-state assignment.
        self.cached_state = state;
    }

    fn clear_oneshot(&mut self, out: &mut Vec<LooperWrite>) {
        if self.oneshot {
            self.oneshot = false; // 0x513dbc, then 0x875bd0.
            out.push(parameter(8, 0.0));
        }
    }
}

impl Looper {
    fn clock_reset(&self, context: &LooperContext) -> Result<bool, SetupError> {
        if self.pre_roll {
            return Ok(false); // 0x55e140/0x55db24: +0xd5 gates the check.
        }
        let clock = context.clock.ok_or_else(|| {
            self.unrecovered(
                0x5138dc,
                "metronome slot 4 and MIDI gates 0x32f514/0x32fbb4",
            )
        })?;
        Ok(clock.metronome_slot_4 == 0.0 && !clock.midi_gate_f514 && !clock.midi_gate_fbb4)
    }

    /// Buttons 0–4 and 6 (0x55e140). A release is `!pressed`.
    pub(super) fn button(
        &mut self,
        index: u32,
        pressed: bool,
        playback: &mut LooperPlayback,
        context: &LooperContext,
        out: &mut Vec<LooperWrite>,
    ) -> Result<(), SetupError> {
        let state = playback.cached_state;
        let incoming = playback.incoming;
        let armed = self.last_status.armed != 0; // Block +0x8c.
        let qmask = incoming.events_waiting_for_quantize & 4 != 0;
        // Validate external inputs before changing either state copy.
        let needs_clock = pressed
            && ((index == 0 && state == 1) || (index == 1 && matches!(state, 2 | 3) && qmask));
        let reset_clock = needs_clock && self.clock_reset(context)?;

        match index {
            0 => match state {
                1 if pressed => {
                    if incoming.in_reverse != 0 {
                        out.push(LooperWrite::RigParameter {
                            index: 7,
                            value_bits: 0.0_f32.to_bits(),
                        }); // 0x55ed80 → 0x523644, before one-shot/event writes.
                    }
                    playback.clear_oneshot(out);
                    playback.transition(3, 5, out);
                }
                2 | 3 | 9 | 10 if pressed => playback.transition(4, 6, out),
                4 | 5 if pressed || !self.record_latched => {
                    if armed {
                        playback.transition(1, 1, out);
                    } else if pressed {
                        playback.transition(4, 6, out);
                    } else {
                        playback.transition(2, 2, out);
                    }
                }
                6 if pressed || !self.overdub_latched => playback.transition(2, 2, out),
                _ => {}
            },
            1 if pressed => match state {
                1 if self.last_status.loop_length != 0 => playback.transition(2, 3, out),
                2 if !qmask => {
                    playback.transition(1, 1, out);
                    playback.clear_oneshot(out); // 0x55eb18: after event 1.
                }
                2 | 3 if qmask => playback.transition(0x10, 2, out),
                4 | 5 | 6 | 8 | 9 | 10 | 11 => playback.transition(2, 2, out),
                _ => {}
            },
            2 if pressed => {
                self.undo_count = incoming.undo_count; // 0x55e700: +0xc4.
                self.operation_pending = true; // +0xc8.
                out.push(event(5));
                playback.cached_state = match state {
                    3 | 4 | 5 | 8 | 11 => 1,
                    6 | 9 | 10 => 2,
                    _ => state,
                };
            }
            3 if pressed => {
                playback.clear_oneshot(out); // 0x55eca8: even for an unhandled state.
                match state {
                    1 | 2 | 4 | 6 | 10 => {
                        self.duplicate_again = false; // 0x55ec50: +0xbc.
                        playback.dupwait = self.duplicate_sync && state != 1;
                        playback.transition(
                            if playback.dupwait { 9 } else { 6 },
                            if state == 1 { 8 } else { 9 },
                            out,
                        );
                    }
                    8 => playback.transition(1, 1, out),
                    9 if playback.dupwait => {
                        let (action, state) = match incoming.state {
                            6 => (4, 6),
                            10 => (7, 10),
                            4 => (3, 4),
                            _ => (2, 2),
                        };
                        playback.transition(action, state, out);
                    }
                    9 => {
                        self.duplicate_again = true; // 0x55ede4: +0xbc.
                        playback.transition(6, 9, out);
                    }
                    _ => {}
                }
            }
            4 if pressed => {
                if state == 9 {
                    playback.transition(8, 2, out);
                } else {
                    playback.oneshot = !playback.oneshot; // 0x55e8c0 → 0x513dbc.
                    out.push(parameter(8, if playback.oneshot { 1.0 } else { 0.0 }));
                    match state {
                        2 if incoming.state == 9 => playback.transition(8, 2, out),
                        1..=3 if playback.oneshot && self.last_status.loop_length != 0 => {
                            playback.transition(2, 2, out);
                        }
                        6 | 10 if incoming.state == 9 => playback.transition(8, 2, out),
                        _ => {}
                    }
                }
            }
            6 => match state {
                1 if pressed => playback.transition(7, 11, out),
                2..=6 if pressed => playback.transition(7, 10, out),
                10 | 11 if pressed || !self.punch_latched => {
                    if armed {
                        playback.transition(1, 1, out);
                    } else {
                        playback.transition(2, 2, out);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        if reset_clock {
            out.push(LooperWrite::Control(
                ControlRequest::ResetInternalMidiClock { value: 1 },
            )); // 0x55ea7c → 0x5138dc → 0x876038, wait=true/timeout=1000 ms.
        }
        Ok(())
    }

    /// One status record through the controller slot (0x513c94) and this
    /// block's reconcile slot (0x55db24). Errors change nothing.
    pub fn status(
        &mut self,
        status: &LooperStatus,
        playback: &mut LooperPlayback,
        context: &LooperContext,
        out: &mut Vec<LooperWrite>,
    ) -> Result<(), SetupError> {
        // 0x513c94 signals a changed record, or any record after block creation.
        if !playback.adopt_next_state && playback.incoming == *status {
            return Ok(());
        }
        let old = self.last_status;
        // 0x55dc08: armed fell to zero. A changed armed word implies a changed record.
        let reset_clock =
            old.armed != status.armed && status.armed == 0 && self.clock_reset(context)?;
        playback.incoming = *status;
        if old != *status {
            self.reconcile(old, status, reset_clock, playback, out);
        }
        if std::mem::take(&mut playback.adopt_next_state) {
            playback.cached_state = status.state; // 0x513c94: after the signal.
        }
        Ok(())
    }

    /// 0x55db24 after its 22-word `memcmp` found a change.
    fn reconcile(
        &mut self,
        old: LooperStatus,
        status: &LooperStatus,
        reset_clock: bool,
        playback: &mut LooperPlayback,
        out: &mut Vec<LooperWrite>,
    ) {
        let n = status.state;
        if old.state != n {
            match (playback.cached_state, n) {
                (5, 1) => playback.cached_state = 1,
                (3, 2) => playback.cached_state = 2,
                (3 | 11 | 8, 1) => {
                    playback.cached_state = 1; // Assignment precedes event 1.
                    out.push(event(1));
                }
                (11, 10) => playback.cached_state = 10,
                (5, 4) => playback.cached_state = 4,
                _ => {}
            }
        }
        if reset_clock {
            out.push(LooperWrite::Control(
                ControlRequest::ResetInternalMidiClock { value: 1 },
            )); // 0x55de64 → 0x5138dc.
        }
        if playback.dupwait && old.state == 9 {
            playback.dupwait = false; // 0x55de44: before the old-9/new-2 rule.
        }
        self.last_status = *status; // 0x55dce8: copies all 22 words.
        let cached = playback.cached_state; // 0x55dd1c: snapshot for the remaining rules.
        if playback.oneshot
            && status.one_shot_stopped != 0
            && status.events_waiting_for_quantize == 0
            && matches!(cached, 2 | 3 | 6 | 10)
            && n == 1
        {
            playback.clear_oneshot(out);
            playback.cached_state = 1;
        }
        if old.state == 9 && !playback.dupwait && n == 2 {
            playback.cached_state = 2;
            self.duplicate_again = false; // 0x55e034: +0xbc.
        }
        if (cached == 8 && n == 9) || (cached == 4 && old.state == 4 && n == 2) {
            playback.cached_state = n;
        }
        if self.operation_pending && status.undo_count > self.undo_count {
            self.operation_pending = false; // 0x55dd88: unsigned `bls`.
            if cached != 1 && n == 1 {
                playback.cached_state = 1;
            }
        }
    }

    /// Destructor DSP writes (0x55d534). The caller stops the status timer.
    pub fn teardown(
        self,
        mode: TeardownMode,
        playback: &mut LooperPlayback,
        out: &mut Vec<LooperWrite>,
    ) {
        let state = playback.cached_state;
        match mode {
            TeardownMode::Local => match state {
                4 | 6 | 7 | 9 | 10 => out.push(event(2)),
                5 | 8 | 11 => out.push(event(1)),
                _ => {}
            },
            TeardownMode::Global => {
                for destination in [0, 1] {
                    out.push(LooperWrite::Request(
                        TunerLooperRequest::GlobalLooperInsertPath {
                            destination,
                            path: u32::MAX,
                        },
                    )); // 0x55d5e8: insert disconnects precede the events.
                }
                match state {
                    2 | 3 | 5 | 8 | 11 => out.push(event(1)),
                    4 | 6 | 7 | 9 | 10 => out.extend([event(2), event(1)]),
                    _ => {}
                }
                playback.cached_state = 1; // 0x55d64c: only this branch resets it.
            }
        }
    }

    /// The factory deletes this driver for a new grid (0x50eef8). An old 27000
    /// takes the global branch when `incoming` has no 27000 (`+0xd4`,
    /// 0x50ef94), then sends one track-0 status request unless `nopoll`
    /// (0x50f360 → 0x875c5c). An old 27002 tears down locally, no request.
    pub fn replace(
        self,
        incoming: impl IntoIterator<Item = u32>,
        polling_disabled: bool,
        playback: &mut LooperPlayback,
        out: &mut Vec<LooperWrite>,
    ) {
        let looper_x = self.model_id == 27000;
        let mode = if looper_x && !incoming.into_iter().any(|model| model == 27000) {
            TeardownMode::Global
        } else {
            TeardownMode::Local
        };
        self.teardown(mode, playback, out);
        if looper_x && !polling_disabled {
            out.push(LooperWrite::Request(
                TunerLooperRequest::LooperStatusRequest { track: 0 },
            ));
        }
    }
}
