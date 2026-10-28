//! `zenUI::MidiController` (metaobject 0x982b08) with its receive worker
//! (0x982a34) and send worker (0x9829e8), as one transport-agnostic state
//! machine. The caller injects DIN bytes, USB records and time, and routes
//! the returned [`MidiOutput`]s in order.
//!
//! The QC mini has no MIDI UART ([`MidiPort::Dsp`]): its input arrives as DSP
//! response `0x26` ([`MidiController::receive_port`]) and its output leaves as a
//! tagged `0x18` ([`DspCommand::PortMidiOut`]).

use std::time::Duration;

use crate::clock::{ClockEnv, ClockState, TempoContext};
use crate::dispatch::{BlockMidi, DispatchState, ExpressionPedal, JackMidiMode, MidiAction};
use crate::message::{MidiMessage, Realtime};
use crate::output::{DspCommand, MidiOutput, SettingReport};
use crate::parser::{MidiParser, ParseEvent};
use crate::preset_out::{
    GeneralMidiMessage, MidiOutSource, PlannedSend, PresetMidiOut, RemoteMidiError, ToggleState,
};
use crate::settings::{ChannelSetting, ClockOut, MidiSettings};
use crate::usb::{UsbMidiEvent, UsbMidiReceiver};

/// Tap tempo state (+0xe0 BPM, +0xe8/+0xf0 tap times). The first tap after
/// boot applies the initial 40 BPM.
#[derive(Debug, Clone)]
struct TapTempo {
    last: Option<Duration>,
    bpm: f32,
}

impl Default for TapTempo {
    fn default() -> Self {
        Self {
            last: None,
            bpm: 40.0,
        }
    }
}

/// Where the unit's own MIDI port is. Stock selects with `0x00499a60`
/// (product revision 2 or 3: the QC mini or a QC in mini emulation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MidiPort {
    /// QC: the DIN UART `/dev/ttySC1`.
    #[default]
    Uart,
    /// QC mini: the SHARC. In: DSP `0x26` records (`0x8a118c` → `CMIDI::RxInsertQueue`).
    /// Out: tagged `0x18` (`0x331340` → `0x00876200`).
    Dsp,
}

#[derive(Debug, Clone)]
pub struct MidiController {
    port: MidiPort,
    settings: MidiSettings,
    parser: MidiParser,
    usb: UsbMidiReceiver,
    dispatch: DispatchState,
    clock: ClockState,
    toggles: ToggleState,
    tap: TapTempo,
    tempo: TempoContext,
    preset_ready: bool,
    receive_enabled: bool,
    /// Bit `n`: channel index `n` is reserved by an extension.
    reserved: u16,
    /// Owned: a BLE MIDI host listens and MIDI over Bluetooth is on.
    ble_out: bool,
}

impl MidiController {
    pub fn new(settings: MidiSettings) -> Self {
        Self {
            port: MidiPort::Uart,
            settings,
            parser: MidiParser::default(),
            usb: UsbMidiReceiver::default(),
            dispatch: DispatchState::default(),
            clock: ClockState::default(),
            toggles: ToggleState::default(),
            tap: TapTempo::default(),
            tempo: TempoContext::default(),
            preset_ready: false,
            receive_enabled: true,
            reserved: 0,
            ble_out: false,
        }
    }

    /// The QC mini uses [`MidiPort::Dsp`]; [`Self::new`] starts with [`MidiPort::Uart`].
    pub fn with_port(mut self, port: MidiPort) -> Self {
        self.port = port;
        self
    }

    pub fn port(&self) -> MidiPort {
        self.port
    }

    pub fn settings(&self) -> &MidiSettings {
        &self.settings
    }

    pub fn clock(&self) -> &ClockState {
        &self.clock
    }

    /// Construction-time outputs (`0x0032e7e4`, then `CMIDI::Open` and
    /// `SetMIDIType`): 0x22 OverUSB, 0x25 ClockOutEn, 0x26 ClockInEn, jack mode.
    pub fn boot(&self) -> Vec<MidiOutput> {
        vec![
            MidiOutput::Dsp(DspCommand::EnableUsbMidiClock(self.settings.over_usb)),
            MidiOutput::Dsp(DspCommand::EnableMidiClockOut(self.settings.clock_out)),
            MidiOutput::Dsp(DspCommand::EnableMidiClockIn(self.settings.clock_in)),
            MidiOutput::Action(MidiAction::SetJackMidiMode(jack_mode(self.settings.thru))),
        ]
    }

    pub fn set_tempo_context(&mut self, tempo: TempoContext) {
        self.tempo = tempo;
    }

    /// A preset is loaded and not busy (`0x006a8d3c` / `0x006a8244`).
    pub fn set_preset_ready(&mut self, ready: bool) {
        self.preset_ready = ready;
    }

    /// Pages disconnect MIDI receive with `0x0032f408`; re-enabling drops
    /// any partly received DIN input.
    pub fn set_receive_enabled(&mut self, enabled: bool) {
        if enabled && !self.receive_enabled {
            self.parser = MidiParser::default();
        }
        self.receive_enabled = enabled;
    }

    /// Channels (bit `n` = channel index `n`) whose voice messages go to extensions as
    /// [`MidiOutput::Reserved`] instead of native dispatch. Realtime and system messages are
    /// never reserved.
    pub fn set_reserved_channels(&mut self, channels: u16) {
        self.reserved = channels;
    }

    pub fn reserved_channels(&self) -> u16 {
        self.reserved
    }

    /// Owned: [`Self::transmit`] also gives [`MidiOutput::Ble`] while this is on.
    pub fn set_ble_out(&mut self, on: bool) {
        self.ble_out = on;
    }

    /// The application refused the last `LoadPreset` (null or auditioned setlist).
    pub fn program_change_refused(&mut self) {
        self.dispatch.program_change_refused();
    }

    fn env<'a>(
        settings: &'a MidiSettings,
        tempo: TempoContext,
        ready: bool,
        out: &'a mut Vec<MidiOutput>,
    ) -> ClockEnv<'a> {
        ClockEnv {
            settings,
            tempo,
            preset_ready: ready,
            out,
        }
    }

    /// Bytes from the DIN UART. Clock and tap tempo use the time of each call,
    /// so the caller must pass one byte per call, stamped with that byte's own
    /// read time (monotonic). Bytes in one call share a time: two F8 bytes in
    /// one call give a zero interval.
    pub fn receive_din(&mut self, bytes: &[u8], now: Duration) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        for event in self.parser.feed(bytes) {
            // Stock drops what it cannot parse; malformed input has no action.
            if let ParseEvent::Message(message) = event {
                self.handle(message, now, &mut out);
            }
        }
        out
    }

    /// A USB MIDI record (DSP response 0x07), drained like `0x0089ece0`. Every kind
    /// decodes, SysEx after reassembly ([`UsbMidiReceiver`]); the stock map then drops what
    /// it gives no job, and reserved channels take any voice message.
    pub fn receive_usb(&mut self, event: UsbMidiEvent, now: Duration) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        // Every packet reaches the receiver before the gates, so a gate never splices a SysEx.
        let Some(message) = self.usb.receive(event) else {
            return out;
        };
        // Busy presets still take Stop; MIDI over USB off still takes ticks.
        let stop = message == MidiMessage::Realtime(Realtime::Stop);
        let tick = message == MidiMessage::Realtime(Realtime::TimingClock);
        if !(self.preset_ready || stop) || !(self.settings.over_usb || tick) {
            return out;
        }
        let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
        match message {
            MidiMessage::Realtime(Realtime::TimingClock) => {
                if !self.clock.din_running() && self.clock.out_of_range().is_none() {
                    env.out.push(MidiOutput::Action(MidiAction::UsbClockTick));
                }
            }
            MidiMessage::Realtime(Realtime::Stop) => self.clock.usb_stop(&mut env),
            // Start/Continue from USB have no effect in stock.
            MidiMessage::Realtime(_) => {}
            message => self.handle(message, now, &mut out),
        }
        out
    }

    /// Owned: one complete message from a BLE MIDI host (docs/bluetooth.md), status first.
    /// It takes the DIN path's gates: a ready preset (or Stop while busy) and MIDI receive on.
    /// Clock and the other real-time messages are dropped: BLE packets come at the
    /// connection interval, so a BLE clock jitters by several milliseconds.
    pub fn receive_ble(&mut self, bytes: &[u8], now: Duration) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        if !self.preset_ready || !self.receive_enabled {
            return out;
        }
        let mut parser = MidiParser::new(crate::parser::DEFAULT_SYSEX_LIMIT);
        for event in parser.feed(bytes) {
            match event {
                ParseEvent::Message(MidiMessage::Realtime(_)) | ParseEvent::Error(_) => {}
                ParseEvent::Message(message) => self.handle(message, now, &mut out),
            }
        }
        out
    }

    /// A QC mini MIDI-in record (DSP response 0x26, `0x0089ece0` at `0x8a0fe8`):
    /// the drain turns it into a DIN message for `CMIDI::RxInsertQueue`, so it takes
    /// the DIN path. Gates: a ready preset, or Stop while busy; MIDI over USB does
    /// not apply. `stamp` is record word 2, the SHARC stamp that times F8
    /// (`0x004f1288`).
    pub fn receive_port(
        &mut self,
        event: UsbMidiEvent,
        stamp: u32,
        now: Duration,
    ) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        let stop = matches!(event.code, 0x05 | 0x0f) && event.status == 0xfc;
        if !(self.preset_ready || stop) || !self.receive_enabled {
            return out;
        }
        match event.to_message() {
            Some(MidiMessage::Realtime(Realtime::TimingClock)) => {
                let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
                self.clock.din_tick(&mut env, now, Some(stamp));
            }
            // The stock drain decodes only these kinds for the mini's port; the rest stays
            // dropped, as on stock.
            Some(
                message @ (MidiMessage::NoteOff { .. }
                | MidiMessage::NoteOn { .. }
                | MidiMessage::ControlChange { .. }
                | MidiMessage::ProgramChange { .. }
                | MidiMessage::Realtime(_)),
            ) => self.handle(message, now, &mut out),
            _ => {}
        }
        out
    }

    /// DSP response 0x0e: USB clock tempo and tick count.
    pub fn usb_clock_tempo(&mut self, bpm: i32, count: u32) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
        self.clock.usb_tempo(&mut env, bpm, count);
        out
    }

    /// DSP response 0x0f: the USB clock stopped.
    pub fn usb_clock_stopped(&mut self) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
        self.clock.usb_stop(&mut env);
        out
    }

    /// Fire due timers (the 200 ms DIN clock timeout).
    pub fn poll(&mut self, now: Duration) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        if self
            .clock
            .din_timeout_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
            self.clock.din_timeout(&mut env);
        }
        out
    }

    /// The earliest time `poll` has work, if any.
    pub fn next_deadline(&self) -> Option<Duration> {
        self.clock.din_timeout_deadline()
    }

    /// Receive worker filtering plus `onMessageReceived` (`0x0032ff40`)
    /// and the dispatch slots it feeds.
    fn handle(&mut self, message: MidiMessage, now: Duration, out: &mut Vec<MidiOutput>) {
        if !self.receive_enabled {
            return;
        }
        // Owned: an extension's reserved channel is consumed here, after admission and before
        // the native channel filter, so OMNI or a matching native channel never acts on it.
        if let Some(channel) = message.channel()
            && self.reserved & (1 << channel.index()) != 0
        {
            out.push(MidiOutput::Reserved(message));
            return;
        }
        let action = |out: &mut Vec<MidiOutput>, action| out.push(MidiOutput::Action(action));
        match message {
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } if matches!(controller.get(), 1 | 2) && self.settings.channel.accepts(channel) => {
                let pedal = if controller.get() == 1 {
                    ExpressionPedal::Exp1
                } else {
                    ExpressionPedal::Exp2
                };
                let position = f32::from(value.get()) / 127.0;
                action(out, MidiAction::Expression { pedal, position });
            }
            MidiMessage::ControlChange {
                channel,
                controller,
                value,
            } => {
                if !self.settings.channel.accepts(channel) {
                    return;
                }
                if controller.get() == 44 {
                    self.tap(now);
                }
                if controller.get() == 62 {
                    out.extend(self.set_ignore_duplicate_pc(value.get() > 63));
                }
                if let Some(next) = self.dispatch.control_change(
                    channel,
                    controller,
                    value,
                    self.tap.bpm,
                    self.port,
                ) {
                    action(out, next);
                }
                if !crate::dispatch::is_stock_controller(controller.get()) {
                    out.push(MidiOutput::Unclaimed(message));
                }
            }
            MidiMessage::ProgramChange { channel, program } => {
                if !self.settings.channel.accepts(channel) {
                    return;
                }
                // Stock also counts PC#44 as a tap (it only updates timing).
                if program.get() == 44 {
                    self.tap(now);
                }
                let next = self
                    .dispatch
                    .program_change(program, self.settings.ignore_duplicate_pc);
                action(out, next);
            }
            MidiMessage::NoteOn {
                channel,
                key,
                velocity,
            }
            | MidiMessage::NoteOff {
                channel,
                key,
                velocity,
            } => {
                let on = matches!(message, MidiMessage::NoteOn { .. });
                // Note-offs too: a mapping releases on them.
                if self.settings.channel.accepts(channel) {
                    out.push(MidiOutput::Unclaimed(message));
                }
                action(
                    out,
                    MidiAction::Block(BlockMidi::Note {
                        channel,
                        key,
                        velocity,
                        on,
                    }),
                );
            }
            MidiMessage::Realtime(realtime) => {
                let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, out);
                match realtime {
                    Realtime::TimingClock => self.clock.din_tick(&mut env, now, None),
                    Realtime::Start | Realtime::Continue => self.clock.start(&mut env),
                    Realtime::Stop => self.clock.stop(&mut env),
                    Realtime::ActiveSensing | Realtime::Reset => {}
                }
            }
            MidiMessage::SongPosition(beats) => {
                let env = Self::env(&self.settings, self.tempo, self.preset_ready, out);
                self.clock.song_position(&env, beats);
            }
            // Stock drops aftertouch, pitch bend, SysEx and other system messages.
            _ => {}
        }
    }

    /// `bpm = 60000 / Δms` from the previous tap; the first tap keeps the
    /// current value.
    fn tap(&mut self, now: Duration) {
        if let Some(previous) = self.tap.last.replace(now) {
            let ms = now.saturating_sub(previous).as_millis() as f32;
            self.tap.bpm = 60_000.0 / ms;
        }
    }

    /// Send one message as `0x00331340` does. QC: DIN first, then USB
    /// (0x18 immediate, one per packet) when MIDI over USB is on. Real-time
    /// stays on DIN; the SHARC paces USB clock out. Stock sends only CC and PC
    /// over USB; Tetrad sends every other kind too, SysEx as several packets
    /// ([`UsbMidiEvent::packets`]). QC mini: CC and PC go out as one tagged
    /// 0x18 (`0x00876200`) with the MIDI over USB flag; the ARM sends no timing
    /// clock (the SHARC paces clock out). Stock never encodes other kinds, so
    /// the mini sends nothing for them. Owned: every kind but real-time also goes to
    /// BLE MIDI hosts ([`Self::set_ble_out`]).
    pub fn transmit(&self, message: &MidiMessage) -> Vec<MidiOutput> {
        let ble = (self.ble_out && !matches!(message, MidiMessage::Realtime(_)))
            .then(|| MidiOutput::Ble(message.to_bytes()));
        if self.port == MidiPort::Dsp {
            if !matches!(
                message,
                MidiMessage::ControlChange { .. } | MidiMessage::ProgramChange { .. }
            ) {
                return ble.into_iter().collect();
            }
            return UsbMidiEvent::packets(message)
                .into_iter()
                .map(|event| {
                    MidiOutput::Dsp(DspCommand::PortMidiOut {
                        event,
                        usb: self.settings.over_usb,
                    })
                })
                .chain(ble)
                .collect();
        }
        let mut out = vec![MidiOutput::Din(message.to_bytes())];
        out.extend(ble);
        if self.settings.over_usb && !matches!(message, MidiMessage::Realtime(_)) {
            out.extend(
                UsbMidiEvent::packets(message)
                    .into_iter()
                    .map(|event| MidiOutput::Dsp(DspCommand::UsbMidiOut(event))),
            );
        }
        out
    }

    fn transmit_planned(&self, planned: Vec<PlannedSend>) -> Vec<MidiOutput> {
        planned
            .into_iter()
            .flat_map(|send| match send {
                Ok(messages) => messages
                    .iter()
                    .flat_map(|message| self.transmit(message))
                    .collect(),
                Err(error) => vec![MidiOutput::Action(MidiAction::SlotRefused(error))],
            })
            .collect()
    }

    /// A preset finished loading (any cause, including PC): send its
    /// on-load messages (`0x00331c28`), then reset the CC Toggle states and
    /// the editor source without saving (`0x0032e700`).
    pub fn preset_loaded(&mut self, table: &PresetMidiOut) -> Vec<MidiOutput> {
        let out = self.transmit_planned(table.on_load_messages());
        self.toggles = ToggleState::default();
        self.settings.last_message_source = 0;
        out
    }

    /// Preset MIDI Out for a stomp footswitch (A..H as 0..=7). Pass the stomp
    /// index, not the physical switch index. Other values send nothing.
    /// Call once per press on QC. On ATMA (Quad Cortex mini) stock sends on
    /// release, so the caller calls on release there; this method does not
    /// check the product. The caller applies the stock gating of
    /// `0x00332064` (stomp context, no tuner or blocking overlay).
    pub fn footswitch(&mut self, table: &PresetMidiOut, stomp: u8) -> Vec<MidiOutput> {
        let planned = self.toggles.footswitch(table, stomp);
        self.transmit_planned(planned)
    }

    /// Preset MIDI Out for a physical expression pedal move (0.0..=1.0).
    /// Stock never echoes positions that came from MIDI CC#1/CC#2.
    pub fn expression(
        &self,
        table: &PresetMidiOut,
        pedal: ExpressionPedal,
        position: f32,
    ) -> Vec<MidiOutput> {
        let source = match pedal {
            ExpressionPedal::Exp1 => MidiOutSource::Expression1,
            ExpressionPedal::Exp2 => MidiOutSource::Expression2,
        };
        self.transmit_planned(table.expression_messages(source, position))
    }

    /// Cortex Control `MIDISettingsMessage` (8) Set, `0x001912d4`: edits
    /// the current preset's table. A general edit also selects its source.
    pub fn apply_remote_midi_settings(
        &mut self,
        table: &mut PresetMidiOut,
        preset_load: Option<&[GeneralMidiMessage]>,
        general: Option<&[GeneralMidiMessage]>,
    ) -> Result<Vec<MidiOutput>, RemoteMidiError> {
        let selected = table.apply_remote(preset_load, general)?;
        Ok(selected.map_or_else(Vec::new, |source| {
            self.toggles = ToggleState::default();
            self.set_last_message_source(source)
        }))
    }

    fn saved(&self, report: Option<SettingReport>) -> Vec<MidiOutput> {
        let mut out = vec![MidiOutput::Action(MidiAction::SaveSettings)];
        out.extend(report.map(|report| MidiOutput::Action(MidiAction::Report(report))));
        out
    }

    /// `0x0032dc9c`.
    pub fn set_channel(&mut self, channel: ChannelSetting) -> Vec<MidiOutput> {
        self.settings.channel = channel;
        self.saved(Some(SettingReport::MidiChannel(channel.raw())))
    }

    /// `0x0032dc38`; also driven by CC#62.
    pub fn set_ignore_duplicate_pc(&mut self, on: bool) -> Vec<MidiOutput> {
        self.settings.ignore_duplicate_pc = on;
        self.saved(Some(SettingReport::IgnoreDuplicatePc(on)))
    }

    /// `0x0032dce8`.
    pub fn set_last_message_source(&mut self, source: u8) -> Vec<MidiOutput> {
        self.settings.last_message_source = source;
        self.saved(None)
    }

    /// `0x00330478`: switch the OUT jack between OUT and THRU.
    pub fn set_thru(&mut self, on: bool) -> Vec<MidiOutput> {
        self.settings.thru = on;
        let mut out = vec![MidiOutput::Action(MidiAction::SetJackMidiMode(jack_mode(
            on,
        )))];
        out.extend(self.saved(None));
        out
    }

    /// Settings page toggle 1 (`0x004161d8`).
    pub fn set_over_usb(&mut self, on: bool) -> Vec<MidiOutput> {
        let mut out = Vec::new();
        let stop_usb_clock = self.settings.over_usb && !on && self.clock.usb_running();
        self.settings.over_usb = on;
        out.push(MidiOutput::Action(MidiAction::SaveSettings));
        out.push(MidiOutput::Dsp(DspCommand::EnableUsbMidiClock(on)));
        if stop_usb_clock {
            let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
            self.clock.set_usb_running(&mut env, false);
            out.push(MidiOutput::Dsp(DspCommand::TempoChange(
                self.tempo.current_bpm,
            )));
        }
        out.push(MidiOutput::Action(MidiAction::Report(
            SettingReport::MidiOverUsb(on),
        )));
        out
    }

    /// `0x0032fab0`.
    pub fn set_clock_out(&mut self, mode: ClockOut) -> Vec<MidiOutput> {
        self.settings.clock_out = mode;
        let mut out = Vec::new();
        let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
        self.clock.set_usb_running(&mut env, false);
        self.clock.set_din_running(&mut env, false);
        out.push(MidiOutput::Action(MidiAction::SaveSettings));
        out.push(MidiOutput::Dsp(DspCommand::EnableMidiClockOut(mode)));
        out.push(MidiOutput::Action(MidiAction::TempoLedRefresh));
        out.push(MidiOutput::Action(MidiAction::Report(
            SettingReport::MidiClockOut(mode),
        )));
        out.push(MidiOutput::Action(MidiAction::ClockRunning(false)));
        out
    }

    /// `0x0032f79c`.
    pub fn set_clock_in(&mut self, on: bool) -> Vec<MidiOutput> {
        self.settings.clock_in = on;
        let mut out = vec![
            MidiOutput::Action(MidiAction::SaveSettings),
            MidiOutput::Dsp(DspCommand::EnableMidiClockIn(on)),
            MidiOutput::Action(MidiAction::Report(SettingReport::MidiClockIn(on))),
        ];
        if !on {
            if self.clock.usb_running() {
                let mut env = Self::env(&self.settings, self.tempo, self.preset_ready, &mut out);
                self.clock.set_usb_running(&mut env, false);
            }
            out.push(MidiOutput::Dsp(DspCommand::TempoChange(
                self.tempo.current_bpm,
            )));
            out.push(MidiOutput::Dsp(DspCommand::ResetInternalMidiClock(0)));
        }
        out
    }

    /// Reset to defaults (`0x0033099c`). Stock does not re-apply the jack
    /// mode or DSP clock flags here.
    pub fn reset_settings(&mut self) -> Vec<MidiOutput> {
        self.settings = MidiSettings::default();
        self.saved(None)
    }
}

fn jack_mode(thru: bool) -> JackMidiMode {
    if thru {
        JackMidiMode::Thru
    } else {
        JackMidiMode::Out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::SetlistSelect;
    use crate::preset_out::{MidiOutKind, MidiOutSlot};

    fn actions(outputs: &[MidiOutput]) -> Vec<&MidiAction> {
        outputs
            .iter()
            .filter_map(|output| match output {
                MidiOutput::Action(action) => Some(action),
                _ => None,
            })
            .collect()
    }

    fn controller() -> MidiController {
        let mut controller = MidiController::new(MidiSettings::default());
        controller.set_preset_ready(true);
        controller
    }

    /// CC and PC obey the channel (or OMNI); notes do not. CC#1 maps to EXP 1.
    #[test]
    fn channel_filter_and_expression() {
        let mut midi = controller();
        let t = Duration::ZERO;
        assert!(midi.receive_din(&[0xb1, 43, 2, 0xc1, 5], t).is_empty());
        let notes = midi.receive_din(&[0x91, 60, 100], t);
        assert!(matches!(
            actions(&notes)[..],
            [MidiAction::Block(BlockMidi::Note { on: true, .. })]
        ));
        midi.set_channel(ChannelSetting::Omni);
        let out = midi.receive_din(&[0xb5, 1, 127, 0xb5, 43, 2], t);
        assert_eq!(
            actions(&out),
            [
                &MidiAction::Expression {
                    pedal: ExpressionPedal::Exp1,
                    position: 1.0
                },
                &MidiAction::SelectScene(2)
            ]
        );
    }

    /// A reserved channel's voice messages reach only the extension, even under OMNI and for
    /// the native expression, bank and note paths; clock stays native.
    #[test]
    fn reserved_channel_bypasses_native_dispatch() {
        let mut midi = controller();
        midi.set_channel(ChannelSetting::Omni);
        midi.set_reserved_channels(1 << 1);
        let t = Duration::ZERO;
        let out = midi.receive_din(&[0xb1, 1, 127, 0xb1, 0, 3, 0x91, 60, 100, 0xc1, 4], t);
        assert!(actions(&out).is_empty(), "{out:?}");
        assert_eq!(
            out.iter()
                .filter(|o| matches!(o, MidiOutput::Reserved(_)))
                .count(),
            4
        );
        let other = midi.receive_din(&[0xb0, 1, 127], t);
        assert_eq!(actions(&other).len(), 1);
        let realtime = midi.receive_din(&[0xfa, 0xf8, 0xfc], t);
        assert!(
            !realtime
                .iter()
                .any(|o| matches!(o, MidiOutput::Reserved(_)))
        );
    }

    /// Owned mappings get the native channel's free CCs, note-ons and note-offs; the stock map
    /// keeps its own CCs, and other channels give nothing.
    #[test]
    fn free_messages_are_unclaimed() {
        let mut midi = controller();
        midi.set_channel(ChannelSetting::Channel(
            crate::Channel::from_number(1).unwrap(),
        ));
        let unclaimed = |out: &[MidiOutput]| {
            out.iter()
                .filter_map(|o| match o {
                    MidiOutput::Unclaimed(message) => Some(message.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let t = Duration::ZERO;
        let free = midi.receive_din(&[0xb0, 20, 64, 0x90, 60, 100, 0x80, 60, 0], t);
        assert_eq!(unclaimed(&free).len(), 3, "{free:?}");
        let stock = midi.receive_din(&[0xb0, 43, 1], t);
        assert!(unclaimed(&stock).is_empty());
        assert_eq!(actions(&stock), [&MidiAction::SelectScene(1)]);
        assert!(unclaimed(&midi.receive_din(&[0xb5, 20, 64], t)).is_empty());
    }

    /// First CC#44 applies the boot value 40; the next uses 60000 / Δms.
    #[test]
    fn tap_tempo() {
        let mut midi = controller();
        let first = midi.receive_din(&[0xb0, 44, 127], Duration::from_millis(1000));
        assert_eq!(actions(&first), [&MidiAction::TapTempo(40)]);
        let second = midi.receive_din(&[0xb0, 44, 127], Duration::from_millis(1500));
        assert_eq!(actions(&second), [&MidiAction::TapTempo(120)]);
    }

    /// CC#62 >= 64 turns on duplicate-PC ignoring, which the next PC carries.
    #[test]
    fn cc62_sets_ignore_duplicate_pc() {
        let mut midi = controller();
        let out = midi.receive_din(&[0xb0, 62, 64, 0xc0, 3], Duration::ZERO);
        assert!(midi.settings().ignore_duplicate_pc);
        assert_eq!(
            actions(&out).last(),
            Some(&&MidiAction::LoadPreset {
                setlist: SetlistSelect::Current,
                index: 3,
                ignore_if_current: true
            })
        );
    }

    /// Preset MIDI Out goes to DIN, then USB 0x18 when MIDI over USB is on.
    #[test]
    fn preset_out_routing() {
        let mut table = PresetMidiOut::default();
        table.on_load[0] = MidiOutSlot {
            kind: MidiOutKind::ProgramChange,
            channel: 1,
            param1: u32::MAX,
            param2: u32::MAX,
            param3: 9,
        };
        let mut midi = controller();
        let out = midi.preset_loaded(&table);
        let usb = UsbMidiEvent {
            code: 0x0c,
            status: 0xc0,
            data: [9, 0],
        };
        assert_eq!(
            out,
            [
                MidiOutput::Din(vec![0xc0, 9]),
                MidiOutput::Dsp(DspCommand::UsbMidiOut(usb))
            ]
        );
        midi.set_over_usb(false);
        assert_eq!(midi.preset_loaded(&table), [MidiOutput::Din(vec![0xc0, 9])]);
    }

    /// USB carries the kinds stock left on DIN: SysEx goes out as one 0x18 per packet, and an
    /// inbound USB pitch bend on a reserved channel reaches the extension.
    #[test]
    fn usb_carries_every_kind() {
        let mut midi = controller();
        let sysex = MidiMessage::SysEx(vec![0x7d, 0x44]);
        let out = midi.transmit(&sysex);
        assert_eq!(out[0], MidiOutput::Din(vec![0xf0, 0x7d, 0x44, 0xf7]));
        let packets: Vec<_> = UsbMidiEvent::packets(&sysex)
            .into_iter()
            .map(|event| MidiOutput::Dsp(DspCommand::UsbMidiOut(event)))
            .collect();
        assert_eq!(packets.len(), 2);
        assert_eq!(out[1..], packets);

        midi.set_reserved_channels(1 << 1);
        let bend = UsbMidiEvent {
            code: 0x0e,
            status: 0xe1,
            data: [0x10, 0x50],
        };
        assert_eq!(
            midi.receive_usb(bend, Duration::ZERO),
            [MidiOutput::Reserved(MidiMessage::PitchBend {
                channel: crate::Channel::from_index(1).unwrap(),
                value: 0x2810,
            })]
        );
    }

    /// DIN clock in: enable sequence on the first in-range tick, 0x2d per
    /// tick, then the 200 ms timeout restores the internal clock and tempo.
    #[test]
    fn din_clock_follow_and_timeout() {
        let mut midi = MidiController::new(MidiSettings {
            clock_in: true,
            ..MidiSettings::default()
        });
        midi.set_preset_ready(true);
        let period = Duration::from_nanos(500_000_000 / 24);
        let mut out = Vec::new();
        for i in 0..96 {
            out = midi.receive_din(&[0xf8], period * i);
        }
        assert!(midi.clock().din_running());
        assert_eq!(
            out,
            [MidiOutput::Dsp(DspCommand::MidiClockCountAndTempo {
                tempo: 120.0,
                count: 96
            })]
        );
        assert!(midi.poll(period * 96).is_empty());
        let out = midi.poll(period * 95 + Duration::from_millis(200));
        assert!(!midi.clock().din_running());
        assert_eq!(
            out[..3],
            [
                MidiOutput::Dsp(DspCommand::EnableUsbMidiClock(true)),
                MidiOutput::Dsp(DspCommand::EnableInternalMidiClock(true)),
                MidiOutput::Dsp(DspCommand::TempoChange(120)),
            ]
        );
        assert_eq!(midi.clock().external_tempo(), 0.0);
    }

    /// BLE MIDI takes the native map like DIN, drops clock, and gets every sent kind but
    /// real-time while a host listens.
    #[test]
    fn ble_in_and_out() {
        let mut midi = controller();
        assert_eq!(
            actions(&midi.receive_ble(&[0xb0, 43, 1], Duration::ZERO)),
            [&MidiAction::SelectScene(1)]
        );
        assert!(midi.receive_ble(&[0xf8], Duration::ZERO).is_empty());
        midi.set_preset_ready(false);
        assert!(midi.receive_ble(&[0xb0, 43, 2], Duration::ZERO).is_empty());

        let pc = MidiMessage::ProgramChange {
            channel: crate::message::Channel::from_index(0).unwrap(),
            program: crate::message::U7::new(9).unwrap(),
        };
        assert!(
            !midi
                .transmit(&pc)
                .iter()
                .any(|o| matches!(o, MidiOutput::Ble(_)))
        );
        midi.set_ble_out(true);
        assert!(midi.transmit(&pc).contains(&MidiOutput::Ble(vec![0xc0, 9])));
        let clock = MidiMessage::Realtime(Realtime::TimingClock);
        assert!(
            !midi
                .transmit(&clock)
                .iter()
                .any(|o| matches!(o, MidiOutput::Ble(_)))
        );
    }

    /// USB records need MIDI over USB, except clock ticks.
    #[test]
    fn usb_gate() {
        let mut midi = MidiController::new(MidiSettings {
            over_usb: false,
            ..MidiSettings::default()
        });
        midi.set_preset_ready(true);
        let cc = UsbMidiEvent {
            code: 0x0b,
            status: 0xb0,
            data: [43, 1],
        };
        assert!(midi.receive_usb(cc, Duration::ZERO).is_empty());
        let tick = UsbMidiEvent {
            code: 0x0f,
            status: 0xf8,
            data: [0, 0],
        };
        assert_eq!(
            actions(&midi.receive_usb(tick, Duration::ZERO)),
            [&MidiAction::UsbClockTick]
        );
        midi.set_over_usb(true);
        assert_eq!(
            actions(&midi.receive_usb(cc, Duration::ZERO)),
            [&MidiAction::SelectScene(1)]
        );
    }

    /// USB clock (0x0e) in range starts following and applies the tempo;
    /// an out-of-range report after the first count stops it and clamps to 260.
    #[test]
    fn usb_clock_tempo() {
        let mut midi = controller();
        let out = midi.usb_clock_tempo(100, 5);
        assert!(midi.clock().usb_running());
        assert!(out.contains(&MidiOutput::Dsp(DspCommand::EnableInternalMidiClock(false))));
        assert_eq!(midi.clock().external_tempo(), 100.0);
        let out = midi.usb_clock_tempo(300, 6);
        assert!(!midi.clock().usb_running());
        assert_eq!(midi.clock().out_of_range(), Some(260.0));
        assert!(actions(&out).contains(&&MidiAction::TempoLedFlash));
    }
}
