//! Stock CC/PC dispatch (`SceneGraph::onMidiMessageReceived`, `0x005b5bd8`)
//! and the typed actions the application performs.
//!
//! Stock has no user CC-to-parameter mapping on input. Parameters follow MIDI
//! only through CC#1/CC#2, which drive expression pedals 1/2 and so every
//! expression assignment of the preset.

use crate::controller::MidiPort;
use crate::message::{Channel, U7};
use crate::output::SettingReport;
use crate::preset_out::SlotError;

/// Which setlist a program change addresses (CC#32 latch, `0x007c2794`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetlistSelect {
    /// No CC#32 since the last PC.
    Current,
    /// CC#32 = 0: the library root collection (inferred: "My Presets").
    Root,
    /// CC#32 = N >= 1: setlist N - 1. Out of range means the current setlist.
    Setlist(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpressionPedal {
    Exp1,
    Exp2,
}

/// Looper X buttons; the value is the stock action code passed to the
/// looper block (`vfunc+0x30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperButton {
    RecordOverdub = 0,
    PlayStop = 1,
    Undo = 2,
    Duplicate = 3,
    OneShot = 4,
    HalfSpeed = 5,
    PunchIn = 6,
    Reverse = 7,
}

/// CC#48..=61, sent to every looper block (category 27) of the grid
/// (`0x00514114` → `LooperDriver::handleMidiController` `0x0055d1bc`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LooperControl {
    /// CC#48: values below 64 open the Looper X editor, 64..=127 close it.
    Editor { open: bool },
    /// CC#49..=56. Stock runs the action only when the looper logic enables
    /// that button (`0x00612318`). `pressed` is `value >= 64` (inferred use).
    Button { button: LooperButton, pressed: bool },
    /// CC#57 (15 duplicate mode), #58 (18 quantize), #59 (19 MIDI clock
    /// start), #61 (17 routing mode): normalized block parameter values.
    Parameter { index: u32, value: f32 },
    /// CC#60 with `value == 0`, when the looper or Multivoicer editor is open.
    /// Meaning not recovered (`0x004b93f4`).
    EditorView(bool),
}

/// Messages for MIDI-enabled grid blocks (Multivoicer). Blocks filter by
/// their own MIDI channel setting (1..=16 or OMNI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockMidi {
    /// Note on/off; not filtered by the global channel (`0x00514270`).
    Note {
        channel: Channel,
        key: U7,
        velocity: U7,
        on: bool,
    },
    /// CC#120 All Sound Off and CC#123 All Notes Off (`0x005141b8`).
    Controller {
        channel: Channel,
        controller: U7,
        value: U7,
    },
}

/// Work for the application.
#[derive(Debug, Clone, PartialEq)]
pub enum MidiAction {
    /// Program change. The application resolves the setlist; it ignores the
    /// PC when the setlist is null or auditioned (then calls
    /// `MidiController::program_change_refused`), or when
    /// `ignore_if_current` is set and the setlist and index are current.
    /// Otherwise it loads the preset and keeps an open tuner open.
    LoadPreset {
        setlist: SetlistSelect,
        index: u16,
        ignore_if_current: bool,
    },
    /// CC#1/CC#2: virtual expression pedal position `value / 127`.
    Expression {
        pedal: ExpressionPedal,
        position: f32,
    },
    /// CC#35..=42: a press of stomp footswitch A..H (0..=7), any value, in
    /// any mode. For momentary stomps stock alternates press and release
    /// per message; otherwise it presses and releases at once.
    Footswitch {
        stomp: u8,
    },
    /// CC#43: scene A..H (0..=7); ignored when already current.
    SelectScene(u8),
    /// CC#44: apply this tempo (already range-checked and clamped 40..=240).
    TapTempo(i32),
    /// CC#45: show (true) or hide the tuner.
    Tuner(bool),
    /// CC#46: show (true) or hide Gig View.
    GigView(bool),
    /// CC#47: select mode slot 0..=2 (`setModeWithIndex(slot, persist)`).
    ModeSlot(u8),
    /// CC#64, QC mini only: footswitch page I (0) or II (1) for `value > 63`
    /// (`0x00589c98`, saved as `Mode/atmaPage` by `0x00589a60`).
    FootswitchPage(u8),
    Looper(LooperControl),
    Block(BlockMidi),
    /// The MIDI clock-followed state changed (`midiClockRunningChanged`).
    ClockRunning(bool),
    /// `extMidiTempoChanged`: the external clock tempo changed.
    ExternalTempoChanged,
    /// Set the metronome driver tempo (`0x00556e3c`).
    MetronomeTempo(i32),
    /// In global tempo mode: set global parameter 0x87 to this BPM.
    GlobalTempo(f32),
    /// Every 24th in-range DIN tick: refresh the tempo LED (`0x00513790`).
    TempoLedBeat,
    /// Out-of-range external clock: flash the tempo LED (`0x00513830`).
    TempoLedFlash,
    /// Recolour the tempo LED from the clock state (`0x00513924`).
    TempoLedRefresh,
    /// A USB clock tick while no DIN clock runs: stock updates the tempo LED
    /// (`CZencoder::SetTempo`) and sends 0x45 MetronomeStatusRequest.
    UsbClockTick,
    /// Set the zenjack MIDI OUT/THRU mux (`CZenjack::SetMIDIType`).
    SetJackMidiMode(JackMidiMode),
    /// Persist `[MidiSettings]`. Stock debounces this by 500 ms.
    SaveSettings,
    /// A Cortex Control `GeneralSettingsMessage` field changed.
    Report(SettingReport),
    /// A stored Preset MIDI Out slot had invalid values and was not sent.
    SlotRefused(SlotError),
}

/// `midi_type_t` for `CZenjack::SetMIDIType` (ioctl 0x4004e008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JackMidiMode {
    Thru = 0,
    Out = 1,
}

/// Dispatcher latches (`SceneGraph` +0x2e04 bank, +0x2e08 CC#32).
#[derive(Debug, Clone, Default)]
pub struct DispatchState {
    bank: u8,
    setlist_latch: Option<u32>,
    /// The latch a pending PC used, restored if the application refuses it.
    last_pc_latch: Option<u32>,
}

impl DispatchState {
    /// A program change on an accepted channel.
    pub fn program_change(&mut self, program: U7, ignore_duplicates: bool) -> MidiAction {
        let latch = self.setlist_latch.take();
        self.last_pc_latch = latch;
        let setlist = match latch {
            None => SetlistSelect::Current,
            Some(0) => SetlistSelect::Root,
            Some(n) => u8::try_from(n - 1).map_or(SetlistSelect::Current, SetlistSelect::Setlist),
        };
        MidiAction::LoadPreset {
            setlist,
            index: u16::from(program.get()) + 128 * u16::from(self.bank),
            ignore_if_current: ignore_duplicates,
        }
    }

    /// Stock keeps the CC#32 latch when a PC is refused for a null or
    /// auditioned setlist.
    pub fn program_change_refused(&mut self) {
        if self.setlist_latch.is_none() {
            self.setlist_latch = self.last_pc_latch.take();
        }
    }

    /// A control change on an accepted channel. CC#1/CC#2 never get here
    /// (expression path). `tap_bpm` is the controller's tap tempo. CC#64 acts
    /// only on the QC mini ([`MidiPort::Dsp`], `0x00499a60` in `0x5b5bd8`).
    pub fn control_change(
        &mut self,
        channel: Channel,
        controller: U7,
        value: U7,
        tap_bpm: f32,
        port: MidiPort,
    ) -> Option<MidiAction> {
        let v = value.get();
        match controller.get() {
            0 => {
                self.bank = u8::from(v > 0);
                return None;
            }
            32 => {
                self.setlist_latch = Some(u32::from(v));
                return None;
            }
            _ => self.setlist_latch = None,
        }
        let high = v >= 64;
        Some(match controller.get() {
            cc @ 35..=42 => MidiAction::Footswitch { stomp: cc - 35 },
            43 if v <= 7 => MidiAction::SelectScene(v),
            44 => {
                // 0x00382850: applied only for 20..=1022, then clamped.
                let bpm = tap_bpm as i32;
                if !(20..=1022).contains(&bpm) {
                    return None;
                }
                MidiAction::TapTempo(bpm.clamp(40, 240))
            }
            45 => MidiAction::Tuner(high),
            46 => MidiAction::GigView(high),
            47 if v <= 2 => MidiAction::ModeSlot(v),
            64 if port == MidiPort::Dsp => MidiAction::FootswitchPage(u8::from(high)),
            48 => MidiAction::Looper(LooperControl::Editor { open: !high }),
            cc @ 49..=56 => MidiAction::Looper(LooperControl::Button {
                button: [
                    LooperButton::Duplicate,
                    LooperButton::OneShot,
                    LooperButton::HalfSpeed,
                    LooperButton::PunchIn,
                    LooperButton::RecordOverdub,
                    LooperButton::PlayStop,
                    LooperButton::Reverse,
                    LooperButton::Undo,
                ][usize::from(cc - 49)],
                pressed: high,
            }),
            57 => looper_parameter(15, if v == 0 { 0.0 } else { 1.0 }),
            58 => looper_parameter(18, (f32::from(v) * 0.111_111_11).min(1.0)),
            59 => looper_parameter(19, if v == 0 { 0.0 } else { 1.0 }),
            60 => MidiAction::Looper(LooperControl::EditorView(v == 0)),
            61 => looper_parameter(17, (f32::from(v) * 0.076_923_08).min(1.0)),
            120 | 123 => MidiAction::Block(BlockMidi::Controller {
                channel,
                controller,
                value,
            }),
            _ => return None,
        })
    }
}

fn looper_parameter(index: u32, value: f32) -> MidiAction {
    MidiAction::Looper(LooperControl::Parameter { index, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u7(value: u8) -> U7 {
        U7::new(value).unwrap()
    }

    /// PC index is PC + 128 * (CC#0 > 0); the bank latch is sticky, the CC#32
    /// latch is consumed by a PC and cleared by any other CC.
    #[test]
    fn program_change_latches() {
        let channel = Channel::from_number(1).unwrap();
        let mut state = DispatchState::default();
        assert_eq!(
            state.control_change(channel, u7(0), u7(5), 0.0, MidiPort::Uart),
            None
        );
        assert_eq!(
            state.control_change(channel, u7(32), u7(3), 0.0, MidiPort::Uart),
            None
        );
        assert_eq!(
            state.program_change(u7(10), true),
            MidiAction::LoadPreset {
                setlist: SetlistSelect::Setlist(2),
                index: 138,
                ignore_if_current: true
            }
        );
        state.program_change_refused();
        assert!(matches!(
            state.program_change(u7(1), false),
            MidiAction::LoadPreset {
                setlist: SetlistSelect::Setlist(2),
                index: 129,
                ..
            }
        ));
        state.control_change(channel, u7(32), u7(0), 0.0, MidiPort::Uart);
        state.control_change(channel, u7(7), u7(0), 0.0, MidiPort::Uart);
        assert!(matches!(
            state.program_change(u7(1), false),
            MidiAction::LoadPreset {
                setlist: SetlistSelect::Current,
                index: 129,
                ..
            }
        ));
    }

    /// Fixed CC map edges: scene/mode bounds, inverted CC#48, looper button
    /// order, tap tempo range and clamp.
    #[test]
    fn fixed_cc_map() {
        let channel = Channel::from_number(1).unwrap();
        let mut state = DispatchState::default();
        let mut cc = |c, v, tap| state.control_change(channel, u7(c), u7(v), tap, MidiPort::Uart);
        assert_eq!(cc(43, 7, 0.0), Some(MidiAction::SelectScene(7)));
        assert_eq!(cc(43, 8, 0.0), None);
        assert_eq!(cc(47, 3, 0.0), None);
        assert_eq!(cc(42, 0, 0.0), Some(MidiAction::Footswitch { stomp: 7 }));
        assert_eq!(
            cc(48, 0, 0.0),
            Some(MidiAction::Looper(LooperControl::Editor { open: true }))
        );
        assert_eq!(
            cc(53, 127, 0.0),
            Some(MidiAction::Looper(LooperControl::Button {
                button: LooperButton::RecordOverdub,
                pressed: true
            }))
        );
        assert_eq!(cc(44, 0, 300.0), Some(MidiAction::TapTempo(240)));
        assert_eq!(cc(44, 0, 19.5), None);
        assert_eq!(cc(58, 9, 0.0), Some(looper_parameter(18, 1.0)));
    }

    /// CC#64 sets the footswitch page only on the QC mini (`0x00499a60` in
    /// `0x5b5bd8`); QC ignores it.
    #[test]
    fn cc64_is_the_mini_footswitch_page() {
        let channel = Channel::from_number(1).unwrap();
        let mut state = DispatchState::default();
        let mut cc = |v, port| state.control_change(channel, u7(64), u7(v), 0.0, port);
        assert_eq!(cc(64, MidiPort::Dsp), Some(MidiAction::FootswitchPage(1)));
        assert_eq!(cc(63, MidiPort::Dsp), Some(MidiAction::FootswitchPage(0)));
        assert_eq!(cc(127, MidiPort::Uart), None);
    }
}
