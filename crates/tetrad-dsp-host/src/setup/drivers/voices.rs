//! The MIDI voice drivers. Notes and CC 120/123 come from the controller's
//! MIDI dispatch (0x514270, 0x5141b8) through [`Driver::midi`]; each note
//! allocator runs from a repeating 20 ms `QTimer`.
//!
//! MultivoicerDriver (18012): constructor 0x5772e8, vtable 0x1264548 with
//! `+0x30` 0x5779a4, MIDI filter `+0x4c`/`+0x50` 0x576e08/0x576df0, CC
//! `+0x54` 0x5770a0, note `+0x58` 0x576f28, `+0x60` 0x578aac, `+0x64`
//! 0x57712c; timer slot 0x579a28 → allocator 0x578c30; voice send 0x576fa4.
//! VOICING writes chord voices through the controller (0x5788ec, 0x57712c).
//!
//! MinivoicerDriver (18007): constructor 0x5755e8, vtable 0x1264408 with
//! `+0x30` 0x57518c, MIDI filter 0x574f7c/0x574f64, CC 0x57511c, note
//! 0x574f94; timer slot → allocator 0x575944; voice send 0x575010.
//!
//! MonosynthMidiDriver (30001): constructor 0x579c04, vtable 0x1264750 with
//! `+0x30` 0x579f00, MIDI filter 0x579a54/0x579a3c, CC 0x579bb0, note
//! 0x579a6c; timer slot 0x57c39c → allocator 0x57b70c; voice send 0x579ae8.
//! Its USB slots (0x579e70, 0x579eb8) have no event source yet.

use std::time::Duration;

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{
    CatalogParameter, DriverMidi, DspWrite, LinkedEdit, MIDI_OMNI, ParamSetMode, SceneValue,
    StockPayload, StockTables, TableError,
};
use super::catalog::{to_display, to_normalized};
use super::reload::{payload, words};
use super::{Driver, DriverKind};

/// The allocators' repeating 20 ms `QTimer` on the runtime's single-shot
/// driver timer: each tick re-arms it while it runs; after a stop the
/// pending tick lapses.
#[derive(Debug, Default)]
struct Repeating {
    running: bool,
    armed: bool,
}

impl Repeating {
    const PERIOD: Duration = Duration::from_millis(20);

    fn start(&mut self) {
        self.running = true;
        self.armed = true;
    }

    fn stop(&mut self) {
        self.running = false;
        self.armed = false;
    }

    fn take(&mut self) -> Option<Duration> {
        std::mem::take(&mut self.armed).then_some(Self::PERIOD)
    }

    /// One tick: true while the timer runs.
    fn tick(&mut self) -> bool {
        self.armed = self.running;
        self.running
    }
}

/// Each note queue drops an event when it already holds this many.
const QUEUE: usize = 24;
const HELD: usize = 128;

/// One voice or note event; note 0xff is a silent voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Note {
    note: u8,
    velocity: u8,
}

const SILENT: Note = Note {
    note: 0xff,
    velocity: 0,
};

/// MIDI note number or velocity as stock sends it: `to_normalized(n, 0,
/// 127, 1)`.
fn midi(value: u8) -> f32 {
    to_normalized(f32::from(value), 0.0, 127.0, 1.0)
}

/// The note allocator template (0x578c30 with N = 4, 0x575944 with N = 2,
/// 0x57b70c with N = 1).
#[derive(Debug)]
struct Allocator<const N: usize> {
    voices: [Note; N],
    /// Free voices, ascending.
    free: Vec<usize>,
    /// Sounding voices, oldest first.
    active: Vec<usize>,
    /// Held notes, newest last.
    held: Vec<Note>,
    on: Vec<Note>,
    off: Vec<u8>,
    dirty: bool,
}

impl<const N: usize> Allocator<N> {
    fn new() -> Self {
        Self {
            voices: [SILENT; N],
            free: (0..N).collect(),
            active: Vec::new(),
            held: Vec::new(),
            on: Vec::new(),
            off: Vec::new(),
            dirty: true,
        }
    }

    /// The note hook: velocity 0 is an off.
    fn queue(&mut self, note: u8, velocity: u8, on: bool) {
        if on && velocity != 0 {
            if self.on.len() < QUEUE {
                self.on.push(Note { note, velocity });
            }
        } else if self.off.len() < QUEUE {
            self.off.push(note);
        }
    }

    /// Silences every voice; held notes and queues stay.
    fn reset(&mut self) {
        self.voices = [SILENT; N];
        self.free = (0..N).collect();
        self.active.clear();
        self.dirty = true;
    }

    fn sounding(&self, note: u8) -> bool {
        self.voices.iter().any(|voice| voice.note == note)
    }

    fn release_held(&mut self, note: u8) {
        if let Some(at) = self.held.iter().position(|held| held.note == note) {
            self.held.remove(at);
        }
    }

    /// The timer slot body: offs in arrival order (a release while every
    /// voice sounded revoices the newest held note below the top N), then
    /// ons by ascending note (stealing the oldest voice when none is free).
    fn allocate(&mut self) {
        for note in std::mem::take(&mut self.off) {
            self.release_held(note);
            let Some(at) = self
                .active
                .iter()
                .position(|&voice| self.voices[voice].note == note)
            else {
                continue;
            };
            let full = self.active.len() == N;
            let voice = self.active.remove(at);
            self.voices[voice] = SILENT;
            self.free.push(voice);
            self.free.sort_unstable();
            self.dirty = true;
            let held = self.held.len();
            if full && held >= N && !self.sounding(self.held[held - N].note) {
                let candidate = self.held.remove(held - N);
                self.held.push(candidate);
                let voice = self.free.remove(0);
                self.voices[voice] = candidate;
                self.active.push(voice);
            }
        }
        let mut on = std::mem::take(&mut self.on);
        // Stock `std::sort` on the note only; stable here (owned).
        on.sort_by_key(|event| event.note);
        for event in on {
            if self.sounding(event.note) {
                continue;
            }
            self.release_held(event.note);
            if self.held.len() < HELD {
                self.held.push(event);
            }
            if self.free.is_empty() {
                let oldest = self.active.remove(0);
                self.voices[oldest] = SILENT;
                self.free.push(oldest);
            }
            let voice = self.free.remove(0);
            self.voices[voice] = event;
            self.active.push(voice);
            self.dirty = true;
        }
    }
}

/// Per-voice parameters are `5 + 6·voice + field`.
const VOICE_FIELDS: u32 = 6;
const ACTIVE: u32 = 0;
const LEVEL: u32 = 1;
const INTERVAL: u32 = 2;
const DETUNE: u32 = 3;
const DELAY: u32 = 4;
const PAN: u32 = 5;
/// V LEVEL display law (`+0x5c..+0x64`).
const LEVEL_SKEW: u32 = 0x4046_cd4b;

fn voice_index(voice: usize, field: u32) -> u32 {
    5 + VOICE_FIELDS * voice as u32 + field
}

const VOICING: u32 = 49;
const MIDI_CH: u32 = 50;
/// VOICING chord rows (`0xf57330`): four `i32` per row, one per voice; row
/// 26 (custom) is past the table.
const CHORDS: StockPayload = payload!(0x00f5_7330 => 26 * 16);
/// A silent voice in a chord row.
const CHORD_SILENT: i32 = -255;

pub(super) fn payloads() -> impl Iterator<Item = StockPayload> {
    std::iter::once(CHORDS)
}

/// Multivoicer `+0x30`, MIDI, `+0x60` and `+0x64` hooks. Values cached
/// before the first push start at 0.0 where stock leaves the heap
/// uninitialized (`+0xb4..+0xf0`).
#[derive(Debug)]
pub(super) struct Multivoicer {
    target: DeploymentTarget,
    /// Raw LEVEL, DETUNE, DELAY and PAN per voice, by field.
    controls: [[f32; 6]; 4],
    /// INTERVAL after its law (`+0xf4`); −1.0 until the first value.
    interval: [f32; 4],
    /// Raw SEMITONES (`+0x104`); −1.0 until the first value.
    semitones: [f32; 4],
    /// SCALE rounds to 0: INTERVAL slots take SEMITONES (`+0xb0`).
    chromatic: bool,
    /// LEVEL in dB (`+0x114`).
    level: f32,
    pan: f32,
    detune: f32,
    delay: f32,
    /// V ACTIVE per voice, kept while MIDI is off (`+0x34..+0x37`).
    active: [bool; 4],
    chords: Vec<[i32; 4]>,
    /// MIDI CH on (`+0x2c`) and its channel (`+0x30`).
    midi_on: bool,
    channel: i32,
    voices: Allocator<4>,
    timer: Repeating,
    edits: Vec<LinkedEdit>,
}

impl Multivoicer {
    /// No DSP writes; the timer is not started. Reads the VOICING chords.
    pub(super) fn new(
        target: DeploymentTarget,
        tables: &impl StockTables,
    ) -> Result<Self, TableError> {
        let bytes = tables.read(CHORDS)?;
        if bytes.len() != CHORDS.len() {
            return Err(TableError::Malformed);
        }
        let word: Vec<i32> = words(&bytes).map(|word| word as i32).collect();
        Ok(Self {
            target,
            controls: [[0.0; 6]; 4],
            interval: [-1.0; 4],
            semitones: [-1.0; 4],
            chromatic: false,
            level: 0.0,
            pan: 1.0,
            detune: 1.0,
            delay: 1.0,
            active: [false; 4],
            chords: word
                .chunks_exact(4)
                .map(|row| [row[0], row[1], row[2], row[3]])
                .collect(),
            midi_on: false,
            channel: MIDI_OMNI,
            voices: Allocator::new(),
            timer: Repeating::default(),
            edits: Vec::new(),
        })
    }

    /// One per-voice value from its cache (V LEVEL adds LEVEL in dB, the
    /// others apply PAN, DETUNE or DELAY with a fused or plain product).
    fn voice_value(&self, voice: usize, field: u32) -> f32 {
        let raw = self.controls[voice][field as usize];
        match field {
            LEVEL => {
                let skew = f32::from_bits(LEVEL_SKEW);
                to_normalized(
                    to_display(raw, -48.0, 12.0, skew) + self.level,
                    -48.0,
                    12.0,
                    skew,
                )
            }
            DETUNE => (raw - 0.5).mul_add(self.detune, 0.5),
            DELAY => raw * self.delay,
            _ => (raw - 0.5).mul_add(self.pan, 0.5),
        }
    }

    fn send(&self, index: u32, value: f32, out: &mut Vec<DspWrite>) {
        out.push(DspWrite::parameter(self.target, index, value));
    }

    /// A controller write of this block (`0x523644`; `0x526bd4` in the
    /// scene hook, without echo): the value re-enters `+0x30` in `mode` and
    /// its pass sends the `0x02`; the app stores the value.
    fn write(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        echo: bool,
        out: &mut Vec<DspWrite>,
    ) {
        if self.update_parameter(index, value, mode, out) {
            self.send(index, value, out);
        }
        self.edits.push(LinkedEdit { index, value, echo });
    }

    /// The VOICING chord (0x5788ec, 0x57712c): each voice's V ACTIVE, then
    /// its V INTERVAL when active. Row `trunc(display 0..26)`; row 26 is
    /// custom and writes nothing.
    fn write_chord(
        &mut self,
        voicing: f32,
        mode: ParamSetMode,
        echo: bool,
        out: &mut Vec<DspWrite>,
    ) {
        let row = to_display(voicing, 0.0, 26.0, 1.0) as usize;
        let Some(&chord) = self.chords.get(row) else {
            return;
        };
        for (voice, step) in chord.into_iter().enumerate() {
            let active = step != CHORD_SILENT;
            let index = voice_index(voice, ACTIVE);
            self.write(index, f32::from(u8::from(active)), mode, echo, out);
            if active {
                // Steps count away from 0: ±1 and 0 are unison.
                let semitones = match step {
                    1.. => step - 1,
                    0 => 0,
                    _ => step + 1,
                };
                let value = to_normalized(semitones as f32, -14.0, 14.0, 1.0);
                self.write(voice_index(voice, INTERVAL), value, mode, echo, out);
            }
        }
    }

    /// 0x576fa4: V ACTIVE per voice, then its V NOTE when it sounds.
    fn send_voices(&mut self, out: &mut Vec<DspWrite>) {
        for (voice, note) in self.voices.voices.iter().enumerate() {
            let sounding = note.note != SILENT.note;
            out.push(DspWrite::parameter(
                self.target,
                voice_index(voice, ACTIVE),
                f32::from(u8::from(sounding)),
            ));
            if sounding {
                out.push(DspWrite::parameter(
                    self.target,
                    36 + voice as u32,
                    midi(note.note),
                ));
            }
        }
        self.voices.dirty = false;
    }
}

impl Driver for Multivoicer {
    fn kind(&self) -> DriverKind {
        DriverKind::Multivoicer
    }

    /// `+0x30` (0x5779a4..0x578827); it never reads the mode.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            // SCALE: refill the INTERVAL slots from the active source. A slot
            // still at −1 makes stock fetch the current scene value; that
            // value is pushed again later in the same push, so it is
            // skipped here (owned, same final DSP state).
            2 => {
                self.chromatic = (value * 4.0).round() == 0.0;
                for voice in 0..4 {
                    let slot = if self.chromatic {
                        self.semitones[voice]
                    } else {
                        self.interval[voice]
                    };
                    if slot.partial_cmp(&0.0) != Some(std::cmp::Ordering::Less) {
                        self.send(voice_index(voice, INTERVAL), slot, out);
                    }
                }
                return true;
            }
            5..=28 => {
                let voice = ((index - 5) / VOICE_FIELDS) as usize;
                match (index - 5) % VOICE_FIELDS {
                    // 0x5781e8..: kept only while MIDI is off (`vcmp`: NaN on).
                    ACTIVE => {
                        if !self.midi_on {
                            self.active[voice] = value != 0.0;
                        }
                        return true;
                    }
                    // V INTERVAL: −14..14 semitones over the −24..24 slot.
                    INTERVAL => {
                        let slot =
                            to_normalized(to_display(value, -14.0, 14.0, 1.0), -24.0, 24.0, 1.0);
                        self.interval[voice] = slot;
                        if !self.chromatic {
                            self.send(index, slot, out);
                        }
                    }
                    field => {
                        self.controls[voice][field as usize] = value;
                        self.send(index, self.voice_value(voice, field), out);
                    }
                }
            }
            // V SEMITONES go raw to the INTERVAL slots.
            40..=43 => {
                let voice = (index - 40) as usize;
                self.semitones[voice] = value;
                if self.chromatic {
                    self.send(voice_index(voice, INTERVAL), value, out);
                }
            }
            44..=47 => {
                let field = match index {
                    44 => {
                        self.level = to_display(value, -24.0, 0.0, 1.0);
                        LEVEL
                    }
                    45 => {
                        self.pan = value;
                        PAN
                    }
                    46 => {
                        self.detune = value;
                        DETUNE
                    }
                    _ => {
                        self.delay = value;
                        DELAY
                    }
                };
                for voice in 0..4 {
                    self.send(
                        voice_index(voice, field),
                        self.voice_value(voice, field),
                        out,
                    );
                }
            }
            // VOICE VIEW writes nothing; VOICING acts in `+0x60`/`+0x64`.
            48 | VOICING => {}
            // MIDI CH (0x57848c): `v > 0` (NaN off); the channel truncates.
            MIDI_CH => {
                self.voices.reset();
                self.voices.dirty = false;
                self.midi_on = value > 0.0;
                if self.midi_on {
                    self.channel = to_display(value, 0.0, 17.0, 1.0) as i32;
                    self.timer.start();
                } else {
                    self.timer.stop();
                }
                self.send(MIDI_CH, f32::from(u8::from(self.midi_on)), out);
            }
            51 => self.send(51, value, out),
            _ => return true,
        }
        false
    }

    /// `+0x60` (0x578aac): VOICING writes its chord; MIDI CH writes each V
    /// ACTIVE: 0.0 while MIDI is on, else the kept value.
    fn parameter_edited(&mut self, index: u32, value: f32, out: &mut Vec<DspWrite>) {
        match index {
            VOICING => self.write_chord(value, ParamSetMode::Live, true, out),
            MIDI_CH => {
                for voice in 0..4 {
                    let active = !self.midi_on && self.active[voice];
                    let index = voice_index(voice, ACTIVE);
                    self.write(
                        index,
                        f32::from(u8::from(active)),
                        ParamSetMode::Live,
                        true,
                        out,
                    );
                }
            }
            _ => {}
        }
    }

    /// `+0x64` (0x57712c): only a VOICING in scene mode (`0x2ea908(…, 49)`)
    /// writes the chord of its stored scene value (`0x2ea164`) into the
    /// scene (`0x526bd4`: store, then a recall-mode write).
    fn scene_recalled(&mut self, scene_value: SceneValue<'_>, out: &mut Vec<DspWrite>) {
        if let Some(voicing) = scene_value(VOICING) {
            self.write_chord(voicing, ParamSetMode::Recall, false, out);
        }
    }

    fn midi_channel(&self) -> Option<i32> {
        self.midi_on.then_some(self.channel)
    }

    /// Note 0x576f28 (ignored while MIDI is off); CC 0x5770a0: 120 and 123
    /// silence every voice, sent only while MIDI is on.
    fn midi(&mut self, message: DriverMidi, out: &mut Vec<DspWrite>) {
        match message {
            DriverMidi::Note { note, velocity, on } => {
                if self.midi_on {
                    self.voices.queue(note, velocity, on);
                }
            }
            DriverMidi::Control(120 | 123) => {
                self.voices.reset();
                if self.midi_on {
                    self.send_voices(out);
                }
            }
            DriverMidi::Control(_) => {}
        }
    }

    fn take_timer(&mut self) -> Option<Duration> {
        self.timer.take()
    }

    /// Timer slot: allocate, then send the voices when one changed.
    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.tick() {
            self.voices.allocate();
            if self.voices.dirty {
                self.send_voices(out);
            }
        }
    }

    fn take_linked_edits(&mut self, out: &mut Vec<LinkedEdit>) {
        out.append(&mut self.edits);
    }
}

/// Minivoicer V ACTIVE, V INTERVAL and V NOTE per voice; V SEMIT are 20, 21.
const MINI_ACTIVE: [u32; 2] = [5, 9];
const MINI_INTERVAL: [u32; 2] = [7, 11];
const MINI_NOTE: [u32; 2] = [18, 19];
const MINI_MIDI: u32 = 15;
const MINI_MIDI_CH: u32 = 22;

/// Minivoicer `+0x30` (0x57518c, jump table 0x5751ac) and MIDI hooks.
#[derive(Debug)]
pub(super) struct Minivoicer {
    target: DeploymentTarget,
    /// MIDI (15) on (`+0x2c`) and MIDI CH (`+0x30`, 17 at first).
    midi_on: bool,
    channel: i32,
    /// V ACTIVE per voice, kept while MIDI is off or in a push (`+0x34`, `+0x35`).
    active: [bool; 2],
    /// MODE is the last of five (`+0x90`).
    chromatic: bool,
    /// V INTERVAL after its law (`+0x94`, `+0x98`) and raw V SEMIT (`+0x9c`,
    /// `+0xa0`); all start at 0.5.
    interval: [f32; 2],
    semitones: [f32; 2],
    voices: Allocator<2>,
    timer: Repeating,
    /// The notes of the last voice send, for [`Driver::take_voice_notes`].
    sent: Option<[Option<u8>; 2]>,
}

impl Minivoicer {
    /// No DSP writes; the timer is not started.
    pub(super) fn new(target: DeploymentTarget) -> Self {
        Self {
            target,
            midi_on: false,
            channel: MIDI_OMNI,
            active: [false; 2],
            chromatic: false,
            interval: [0.5; 2],
            semitones: [0.5; 2],
            voices: Allocator::new(),
            timer: Repeating::default(),
            sent: None,
        }
    }

    fn send(&self, index: u32, value: f32, out: &mut Vec<DspWrite>) {
        out.push(DspWrite::parameter(self.target, index, value));
    }

    /// 0x575010: V ACTIVE per voice, then its V NOTE when it sounds. Each
    /// voice then goes to the editor read-out 0x61d994 (sounding ? note :
    /// −1), here [`Driver::take_voice_notes`].
    fn send_voices(&mut self, out: &mut Vec<DspWrite>) {
        let mut notes = [None; 2];
        for (voice, note) in self.voices.voices.iter().enumerate() {
            let sounding = note.note != SILENT.note;
            self.send(MINI_ACTIVE[voice], f32::from(u8::from(sounding)), out);
            if sounding {
                self.send(MINI_NOTE[voice], midi(note.note), out);
                notes[voice] = Some(note.note);
            }
        }
        self.sent = Some(notes);
        self.voices.dirty = false;
    }

    fn silence(&mut self, out: &mut Vec<DspWrite>) {
        for index in MINI_ACTIVE {
            self.send(index, 0.0, out);
        }
    }
}

impl Driver for Minivoicer {
    fn kind(&self) -> DriverKind {
        DriverKind::Minivoicer
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            // MODE: the qFuzzyCompare of `roundf(4v)` with 4.0 (1e-5 at
            // 0x5755e4) is equality for whole numbers. Refills both slots.
            2 => {
                self.chromatic = (value * 4.0).round() == 4.0;
                let slots = if self.chromatic {
                    self.semitones
                } else {
                    self.interval
                };
                for (index, slot) in MINI_INTERVAL.into_iter().zip(slots) {
                    self.send(index, slot, out);
                }
                true
            }
            // `vcmp`: NaN is on. While MIDI is on only a push keeps it.
            5 | 9 => {
                if !self.midi_on || mode == ParamSetMode::Push {
                    self.active[usize::from(index == 9)] = value != 0.0;
                }
                true
            }
            7 | 11 => {
                let voice = usize::from(index == 11);
                let slot = to_normalized(to_display(value, -14.0, 14.0, 1.0), -24.0, 24.0, 1.0);
                self.interval[voice] = slot;
                if !self.chromatic {
                    self.send(index, slot, out);
                }
                false
            }
            // MIDI on/off: an unchanged value only passes. A change resets
            // and sends the voices, then mutes both voices and starts the
            // timer, or restores the kept V ACTIVE and stops it.
            MINI_MIDI => {
                let on = value != 0.0;
                if on != self.midi_on {
                    self.voices.reset();
                    self.send_voices(out);
                    if on {
                        self.silence(out);
                        self.timer.start();
                    } else {
                        for (index, active) in MINI_ACTIVE.into_iter().zip(self.active) {
                            self.send(index, f32::from(u8::from(active)), out);
                        }
                        self.timer.stop();
                    }
                    self.midi_on = on;
                }
                true
            }
            20 | 21 => {
                let voice = usize::from(index == 21);
                self.semitones[voice] = value;
                if self.chromatic {
                    self.send(MINI_INTERVAL[voice], value, out);
                }
                false
            }
            // MIDI CH: `lroundf(display 0..16) + 1`; while MIDI is on, mute
            // both voices, reset and send them.
            MINI_MIDI_CH => {
                self.channel = to_display(value, 0.0, 16.0, 1.0).round() as i32 + 1;
                if self.midi_on {
                    self.silence(out);
                    self.voices.reset();
                    self.send_voices(out);
                }
                false
            }
            _ => true,
        }
    }

    fn midi_channel(&self) -> Option<i32> {
        self.midi_on.then_some(self.channel)
    }

    /// Note 0x574f94 (ignored while MIDI is off); CC 0x57511c: 120 and 123
    /// reset the voices and send them while MIDI is on.
    fn midi(&mut self, message: DriverMidi, out: &mut Vec<DspWrite>) {
        match message {
            DriverMidi::Note { note, velocity, on } => {
                if self.midi_on {
                    self.voices.queue(note, velocity, on);
                }
            }
            DriverMidi::Control(120 | 123) => {
                self.voices.reset();
                if self.midi_on {
                    self.send_voices(out);
                }
            }
            DriverMidi::Control(_) => {}
        }
    }

    fn take_timer(&mut self) -> Option<Duration> {
        self.timer.take()
    }

    /// Slot 0x575944: allocate, then send the voices when one changed.
    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.tick() {
            self.voices.allocate();
            if self.voices.dirty {
                self.send_voices(out);
            }
        }
    }

    fn take_voice_notes(&mut self) -> Option<[Option<u8>; 2]> {
        self.sent.take()
    }
}

/// Monosynth `+0x30` and MIDI hooks.
#[derive(Debug)]
pub(super) struct Monosynth {
    target: DeploymentTarget,
    /// Catalog S/C SOURCE (37), snapshot at construction; stock re-reads
    /// the block's live `ParameterInfo` each time (0x57aa3c).
    source: CatalogParameter,
    /// `+0x4c`: MIDI CH is nonzero (NaN on).
    midi_on: bool,
    channel: i32,
    voice: Allocator<1>,
    timer: Repeating,
}

const MIDI_NOTE: u32 = 52;
const MIDI_VELOCITY: u32 = 53;
const MIDI_TRIGGER: u32 = 54;

impl Monosynth {
    /// Sends the silent voice (TRIGGER = 0) and starts the timer.
    pub(super) fn new(
        target: DeploymentTarget,
        catalog: &[CatalogParameter],
        out: &mut Vec<DspWrite>,
    ) -> Self {
        // Owned: a short catalog uses the stock metadata default.
        let source = catalog
            .get(37)
            .cloned()
            .unwrap_or(CatalogParameter::DEFAULT);
        let mut synth = Self {
            target,
            source,
            midi_on: false,
            channel: MIDI_OMNI,
            voice: Allocator::new(),
            timer: Repeating::default(),
        };
        synth.timer.start();
        synth.send_voice(out);
        synth
    }

    /// 0x579ae8: TRIGGER, then NOTE and VELOCITY while the voice sounds.
    fn send_voice(&mut self, out: &mut Vec<DspWrite>) {
        let Note { note, velocity } = self.voice.voices[0];
        let sounding = note != SILENT.note;
        out.push(DspWrite::parameter(
            self.target,
            MIDI_TRIGGER,
            f32::from(u8::from(sounding)),
        ));
        if sounding {
            out.push(DspWrite::parameter(self.target, MIDI_NOTE, midi(note)));
            out.push(DspWrite::parameter(
                self.target,
                MIDI_VELOCITY,
                midi(velocity),
            ));
        }
        self.voice.dirty = false;
    }
}

impl Driver for Monosynth {
    fn kind(&self) -> DriverKind {
        DriverKind::MonosynthMidi
    }

    /// `+0x30` (0x579f00); it never reads the mode.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            // OSC WAVE: the DSP swaps waveforms 1.0 and 5/6 (exact compares).
            10 | 15 => {
                let swapped = if value == 1.0 {
                    0.833_333_3
                } else if value == 0.833_333_3 {
                    1.0
                } else {
                    value
                };
                out.push(DspWrite::parameter(self.target, index, swapped));
                false
            }
            // S/C SOURCE: the catalog step over 44.
            37 => {
                let step = self.source.step_index(value) as f32 / 44.0;
                out.push(DspWrite::parameter(self.target, 37, step));
                false
            }
            // MIDI CH: silence the voice (inferred: the CC reset), restart
            // or stop the timer, then the channel by `lroundf`.
            51 => {
                self.voice.reset();
                self.send_voice(out);
                self.midi_on = value != 0.0;
                if self.midi_on {
                    self.timer.start();
                } else {
                    self.timer.stop();
                }
                self.channel = to_display(value, 0.0, 17.0, 1.0).round() as i32;
                true
            }
            MIDI_NOTE..=MIDI_TRIGGER => false,
            _ => true,
        }
    }

    fn midi_channel(&self) -> Option<i32> {
        self.midi_on.then_some(self.channel)
    }

    /// Note 0x579a6c (ignored while MIDI is off); CC 0x579bb0: 120 and 123
    /// silence the voice.
    fn midi(&mut self, message: DriverMidi, out: &mut Vec<DspWrite>) {
        match message {
            DriverMidi::Note { note, velocity, on } => {
                if self.midi_on {
                    self.voice.queue(note, velocity, on);
                }
            }
            DriverMidi::Control(120 | 123) => {
                self.voice.reset();
                self.send_voice(out);
            }
            DriverMidi::Control(_) => {}
        }
    }

    fn take_timer(&mut self) -> Option<Duration> {
        self.timer.take()
    }

    /// Timer slot: nothing while both queues are empty.
    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        if !self.timer.tick() || (self.voice.on.is_empty() && self.voice.off.is_empty()) {
            return;
        }
        self.voice.allocate();
        if self.voice.dirty {
            self.send_voice(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::drivers::amps::tests::identity;
    use ParamSetMode::{Live, Recall};

    /// Synthetic VOICING rows: every row is [5, −255, 0, −3].
    struct Chords;

    impl StockTables for Chords {
        fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
            Ok([5_i32, CHORD_SILENT, 0, -3]
                .iter()
                .cycle()
                .take(payload.len() / 4)
                .flat_map(|word| word.to_le_bytes())
                .collect())
        }
    }

    fn multivoicer() -> (Multivoicer, impl Fn(u32, f32) -> DspWrite) {
        let target = identity(18012).target;
        let driver = Multivoicer::new(target, &Chords).unwrap();
        (driver, move |index, value| {
            DspWrite::parameter(target, index, value)
        })
    }

    fn slot(interval: f32) -> f32 {
        to_normalized(to_display(interval, -14.0, 14.0, 1.0), -24.0, 24.0, 1.0)
    }

    /// INTERVAL maps −14..14 onto the −24..24 slot and waits while SCALE
    /// rounds to 0, where SEMITONES drive the slot; PAN fans out fused.
    #[test]
    fn multivoicer_interval_scale_and_fused_fan_out() {
        let (mut driver, p) = multivoicer();
        let mut out = Vec::new();
        assert!(driver.update_parameter(2, 0.5, Live, &mut out));
        assert!(out.is_empty());
        assert!(!driver.update_parameter(7, 1.0, Live, &mut out));
        assert_eq!(out, [p(7, f32::from_bits(0x3f4a_aaab))]);
        out.clear();
        driver.update_parameter(41, 0.3, Live, &mut out);
        assert!(out.is_empty());
        driver.update_parameter(2, 0.1, Live, &mut out);
        assert_eq!(out, [p(13, 0.3)]);
        out.clear();
        driver.update_parameter(10, 0.8, Live, &mut out);
        driver.update_parameter(45, 0.25, Live, &mut out);
        assert_eq!(out[1], p(10, (0.8_f32 - 0.5).mul_add(0.25, 0.5)));
        assert_eq!(out.len(), 5);
    }

    /// Four voices: the fifth note steals the oldest; releasing a sounding
    /// note revoices the newest held note below the top four. The timer
    /// re-arms on each tick while MIDI is on.
    #[test]
    fn multivoicer_allocator_steals_oldest_and_revoices_held() {
        let (mut driver, p) = multivoicer();
        let mut out = Vec::new();
        driver.update_parameter(MIDI_CH, 0.5, Live, &mut out);
        assert_eq!(driver.take_timer(), Some(Repeating::PERIOD));
        assert_eq!(driver.midi_channel(), Some(8));
        for note in [64, 60, 62, 67, 69] {
            let on = DriverMidi::Note {
                note,
                velocity: 100,
                on: true,
            };
            driver.midi(on, &mut out);
        }
        out.clear();
        driver.timer_elapsed(&mut out);
        assert_eq!(driver.take_timer(), Some(Repeating::PERIOD));
        // Sorted ons 60, 62, 64, 67 fill voices 0..=3; 69 steals voice 0.
        assert_eq!(out[1], p(36, midi(69)));
        let off = DriverMidi::Note {
            note: 69,
            velocity: 0,
            on: true,
        };
        driver.midi(off, &mut out);
        out.clear();
        driver.timer_elapsed(&mut out);
        assert_eq!(out[1], p(36, midi(60)));
        driver.midi(DriverMidi::Control(123), &mut out);
        assert_eq!(
            out[out.len() - 4..],
            [5, 11, 17, 23].map(|index| p(index, 0.0))
        );
    }

    /// `+0x60` VOICING writes its chord through the controller: V ACTIVE
    /// passes, V INTERVAL goes through its law, and each write is a linked
    /// edit with echo. MIDI CH on writes every V ACTIVE as 0.0; off restores
    /// the kept values. `+0x64` writes the chord of a scene-mode VOICING's
    /// scene value without echo; no scene mode or row 26 (custom) writes
    /// nothing.
    #[test]
    fn multivoicer_voicing_and_midi_ch_write_through_the_controller() {
        let (mut driver, p) = multivoicer();
        let edit = |index, value, echo| LinkedEdit { index, value, echo };
        let (mut out, mut edits) = (Vec::new(), Vec::new());
        driver.parameter_edited(VOICING, 0.0, &mut out);
        driver.take_linked_edits(&mut edits);
        let chord = [
            (5, 1.0),
            (7, 18.0 / 28.0),
            (11, 0.0),
            (17, 1.0),
            (19, 0.5),
            (23, 1.0),
            (25, 12.0 / 28.0),
        ];
        assert_eq!(edits, chord.map(|(index, value)| edit(index, value, true)));
        assert_eq!(
            out,
            [
                p(5, 1.0),
                p(7, slot(18.0 / 28.0)),
                p(11, 0.0),
                p(17, 1.0),
                p(19, slot(0.5)),
                p(23, 1.0),
                p(25, slot(12.0 / 28.0)),
            ]
        );
        for (value, kept) in [(1.0, [0.0; 4]), (0.0, [1.0, 0.0, 1.0, 1.0])] {
            edits.clear();
            driver.update_parameter(MIDI_CH, value, Live, &mut out);
            driver.parameter_edited(MIDI_CH, value, &mut out);
            driver.take_linked_edits(&mut edits);
            let active = [5, 11, 17, 23].into_iter().zip(kept);
            assert_eq!(
                edits,
                active
                    .map(|(index, value)| edit(index, value, true))
                    .collect::<Vec<_>>()
            );
        }
        // Review r15 R5: a VOICING not in scene mode writes nothing.
        assert!(!driver.update_parameter(VOICING, 0.0, Recall, &mut out));
        out.clear();
        edits.clear();
        driver.scene_recalled(&|_| None, &mut out);
        driver.take_linked_edits(&mut edits);
        assert!(out.is_empty() && edits.is_empty());
        driver.scene_recalled(&|index| (index == VOICING).then_some(0.0), &mut out);
        driver.take_linked_edits(&mut edits);
        assert_eq!(edits, chord.map(|(index, value)| edit(index, value, false)));
        out.clear();
        driver.scene_recalled(&|_| Some(1.0), &mut out);
        assert!(out.is_empty());
    }

    /// Review r15 R12: an expression move (`0x520074`) does not reach the
    /// controller write `0x523644`, so VOICING writes no chord into the
    /// preset; a live edit does.
    #[test]
    fn expression_voicing_writes_no_chord() {
        use super::super::super::{Block, BlockBypass, SetupValue};
        let (driver, _) = multivoicer();
        let mut block = Block::with_driver(identity(18012), Some(Box::new(driver)));
        let mut out = Vec::new();
        let values = [SetupValue::Float(0.0_f32.to_bits()); 54];
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        block.take_linked_edits();
        block.expression_edit(VOICING, 0.0, &mut out).unwrap();
        assert!(block.take_linked_edits().is_empty());
        let value = SetupValue::Float(0.0_f32.to_bits());
        block.edit(VOICING, value, Live, &mut out).unwrap();
        assert!(!block.take_linked_edits().is_empty());
    }

    /// MODE's last step is chromatic: V SEMIT drive the INTERVAL slots. MIDI
    /// on sends the reset voices, mutes both and starts the timer; two notes
    /// fill voices 0 and 1; MIDI off restores the kept V ACTIVE. Each voice
    /// send reports its notes once (0x61d994). MIDI CH is
    /// `lroundf(display 0..16) + 1`.
    #[test]
    fn minivoicer_mode_midi_and_two_voices() {
        let target = identity(18007).target;
        let p = |index, value| DspWrite::parameter(target, index, value);
        let mut mini = Minivoicer::new(target);
        let mut out = Vec::new();
        assert!(mini.update_parameter(5, 1.0, Live, &mut out));
        assert!(!mini.update_parameter(7, 1.0, Live, &mut out));
        assert_eq!(out, [p(7, 38.0 / 48.0)]);
        out.clear();
        assert!(!mini.update_parameter(20, 0.3, Live, &mut out));
        assert!(out.is_empty());
        assert!(mini.update_parameter(2, 0.9, Live, &mut out));
        assert_eq!(out, [p(7, 0.3), p(11, 0.5)]);
        out.clear();
        assert!(!mini.update_parameter(MINI_MIDI_CH, 0.5, Live, &mut out));
        assert!(mini.update_parameter(MINI_MIDI, 1.0, Live, &mut out));
        assert_eq!(mini.take_timer(), Some(Repeating::PERIOD));
        assert_eq!(mini.midi_channel(), Some(9));
        assert_eq!(out, [p(5, 0.0), p(9, 0.0), p(5, 0.0), p(9, 0.0)]);
        out.clear();
        for note in [62, 60] {
            let on = DriverMidi::Note {
                note,
                velocity: 90,
                on: true,
            };
            mini.midi(on, &mut out);
        }
        assert_eq!(mini.take_voice_notes(), Some([None; 2]));
        mini.timer_elapsed(&mut out);
        assert_eq!(
            out,
            [p(5, 1.0), p(18, midi(60)), p(9, 1.0), p(19, midi(62))]
        );
        assert_eq!(mini.take_voice_notes(), Some([Some(60), Some(62)]));
        assert_eq!(mini.take_voice_notes(), None);
        out.clear();
        assert!(mini.update_parameter(MINI_MIDI, 0.0, Live, &mut out));
        assert_eq!(out, [p(5, 0.0), p(9, 0.0), p(5, 1.0), p(9, 0.0)]);
        assert_eq!(mini.take_voice_notes(), Some([None; 2]));
        assert_eq!(mini.take_timer(), None);
        assert_eq!(mini.midi_channel(), None);
    }

    /// Construction sends TRIGGER = 0 and starts the timer; OSC WAVE swaps
    /// 1.0 and 5/6; S/C SOURCE is the catalog step over 44; a note sends
    /// TRIGGER, NOTE and VELOCITY.
    #[test]
    fn monosynth_waveforms_source_and_single_voice() {
        let target = identity(30001).target;
        let mut catalog = vec![CatalogParameter::DEFAULT; 55];
        catalog[37].step = 1.0 / 44.0;
        let mut out = Vec::new();
        let mut synth = Monosynth::new(target, &catalog, &mut out);
        assert_eq!(out, [DspWrite::parameter(target, MIDI_TRIGGER, 0.0)]);
        assert_eq!(synth.take_timer(), Some(Repeating::PERIOD));
        out.clear();
        assert!(!synth.update_parameter(10, 1.0, Live, &mut out));
        assert!(!synth.update_parameter(15, 0.833_333_3, Live, &mut out));
        synth.update_parameter(37, 0.501, Live, &mut out);
        assert_eq!(
            out,
            [
                DspWrite::parameter(target, 10, 0.833_333_3),
                DspWrite::parameter(target, 15, 1.0),
                DspWrite::parameter(target, 37, 0.5),
            ]
        );
        out.clear();
        assert!(synth.update_parameter(51, 0.0, Live, &mut out));
        assert_eq!(synth.take_timer(), None);
        assert_eq!(synth.midi_channel(), None);
        synth.update_parameter(51, 1.0, Live, &mut out);
        let on = DriverMidi::Note {
            note: 60,
            velocity: 127,
            on: true,
        };
        synth.midi(on, &mut out);
        out.clear();
        synth.timer_elapsed(&mut out);
        assert_eq!(
            out,
            [
                DspWrite::parameter(target, MIDI_TRIGGER, 1.0),
                DspWrite::parameter(target, MIDI_NOTE, midi(60)),
                DspWrite::parameter(target, MIDI_VELOCITY, 1.0),
            ]
        );
    }
}
