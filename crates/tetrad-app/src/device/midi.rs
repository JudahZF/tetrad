//! MIDI bridge: `tetrad_midi::MidiController` between the DIN UART, the DSP (USB MIDI, clock) and
//! device actions.
//!
//! - [`Midi::on_din_byte`]: one `receive_din` call per byte with its read time.
//! - [`Midi::on_dsp`]: DSP `MidiReceived` (USB `0x07`, mini MIDI in `0x26`) and
//!   `MidiClockTempo` / `Flag0f` (`0x0e`/`0x0f`).
//! - Outputs: `Din` → [`Effect::MidiOut`]; `Dsp` → [`DspCommand::Control`] (the mini's MIDI
//!   out is a tagged `0x18`); actions → device [`Action`]s, HAL writes or named notices.
//! - [`Midi::poll`] runs from [`Timer::MidiClock`] at the controller's next deadline.
//! - [`Midi::trigger`]: Preset MIDI Out for physical footswitches and pedals.
//! - The tempo context (mode, current BPM) is read from `State` before each controller step.
//! - The mini (`0x499a60`, [`MidiPort::Dsp`]) has no MIDI UART.
//! - tetrad-settings owns `[MidiSettings]`; changes persist with the stock settings flush delay.

use std::time::Duration;

use tetrad_dsp_host::responses::Event as DspEvent;
use tetrad_dsp_host::setup::DriverMidi;
use tetrad_dsp_protocol::controls::{ControlRequest, UsbMidiDelivery};
use tetrad_dsp_protocol::responses::ResponseType;
use tetrad_engine::SceneIndex;
use tetrad_hal::analog::Verification;
use tetrad_hal::hal::Command as HalCommand;
use tetrad_hal::node::Pedal;
use tetrad_hal::zenjack::{Control, MidiType, Setting, Value};
use tetrad_midi::{
    BlockMidi, ChannelSetting, ClockOut, DspCommand as MidiDsp, ExpressionPedal,
    GeneralMidiMessage, JackMidiMode, LooperControl, MidiAction, MidiController, MidiMessage,
    MidiMessageInfo, MidiOutKind, MidiOutput, MidiPort, MidiSettings, PresetMidiFields,
    PresetMidiOut, Realtime, SetlistSelect, TempoContext, TempoMode, UsbMidiEvent,
};
use tetrad_preset::MidiList;
use tetrad_settings::midi::{MidiChannel, MidiClockOut, MidiSettings as StoredMidiSettings};
use tetrad_settings::tuner::TempoSettings;
use tetrad_ui::{MidiMessageKind, MidiOutMessage, MidiOutTarget};

use super::actions::{self, Action};
use super::{DspCommand, Effect, PresetRef, State, Subsystem, Timer};

#[derive(Debug, thiserror::Error)]
pub enum MidiError {
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error("MIDI channel {0} is outside 1..=17")]
    Channel(i32),
}

/// A physical control that sends Preset MIDI Out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    /// Stomp footswitch A..H (0..=7); QC sends on press, the mini on release.
    Footswitch(u8),
    Expression {
        pedal: Pedal,
        position: f32,
    },
}

/// MidiController clock flags and tempo, mirrored in [`State::midi_clock`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClockStatus {
    /// `+0xfd` (`0x32f514`).
    pub usb_running: bool,
    /// `+0xfe` (`0x32fbb4`).
    pub din_running: bool,
    /// `+0x104` (`0x32fbf0`): the external clock tempo in BPM.
    pub tempo: f32,
    /// `+0xff` (`0x3303c4`, set by `0x32fdbc`): the external clock is out of range ("MIDI
    /// Clock In Out Of Range", `0x38087c`); Cortex Control GeneralSettings field 41.
    pub out_of_range: bool,
}

impl ClockStatus {
    pub fn running(self) -> bool {
        self.usb_running || self.din_running
    }

    /// The BPM that overrides the scoped tempo: a running clock with a positive tempo.
    pub fn bpm(self) -> Option<i32> {
        (self.running() && self.tempo > 0.0).then(|| self.tempo.round() as i32)
    }
}

#[derive(Debug, Default)]
pub struct Midi {
    controller: Option<MidiController>,
    preset_ready: bool,
    din_clocks: u8,
    /// Extension channel reservations, kept across controller rebuilds.
    reserved: u16,
}

impl Midi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Boot sends (`0x22`, `0x25`, `0x26`, then the jack mode). The ctor `0x330a4c` has no
    /// product gate; the port follows `0x499a60` (the encoder `0x331340`).
    pub fn boot(&mut self, state: &State) -> Result<Vec<Effect>, MidiError> {
        let port = if state.platform.is_atma() {
            MidiPort::Dsp
        } else {
            MidiPort::Uart
        };
        let mut controller =
            MidiController::new(from_stored(&state.settings.settings().midi)?).with_port(port);
        controller.set_preset_ready(self.preset_ready);
        controller.set_tempo_context(tempo_context(state));
        controller.set_reserved_channels(self.reserved);
        let routed = route(controller.boot(), state);
        self.controller = Some(controller);
        routed.read_only()
    }

    /// Rebuilds the controller from the stored `[MidiSettings]`. Clock state restarts, so
    /// the external BPM is cleared until the next clock byte.
    pub fn settings_changed(&mut self, state: &State) -> Result<Vec<Effect>, MidiError> {
        if self.controller.is_none() {
            return Ok(Vec::new());
        }
        let mut effects = self.boot(state)?;
        effects.push(Effect::Action(Action::MidiClock(ClockStatus::default())));
        Ok(effects)
    }

    pub fn on_din_byte(
        &mut self,
        byte: u8,
        at: Duration,
        state: &mut State,
    ) -> Result<Vec<Effect>, MidiError> {
        let mut effects =
            self.drive(state, at, |controller| controller.receive_din(&[byte], at))?;
        if byte == 0xf8 {
            effects.extend(self.din_clock());
        }
        Ok(effects)
    }

    /// The status request after every 24th DIN clock (r16-perform §9 gap 2).
    fn din_clock(&mut self) -> Option<Effect> {
        self.din_clocks = (self.din_clocks + 1) % 24;
        (self.din_clocks == 0).then_some(Effect::Dsp(DspCommand::Control(
            ControlRequest::MetronomeStatusRequest,
        )))
    }

    pub fn on_dsp(
        &mut self,
        event: &DspEvent,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, MidiError> {
        match event {
            DspEvent::MidiReceived {
                record,
                response_type: ResponseType::MidiFromType07,
            } => {
                let usb = UsbMidiEvent {
                    code: record.code(),
                    status: record.status(),
                    data: [record.data1(), record.data2()],
                };
                self.drive(state, now, |controller| controller.receive_usb(usb, now))
            }
            // Mini MIDI in: the drain inserts it in the DIN queue (`0x8a118c`).
            DspEvent::MidiReceived {
                record,
                response_type: ResponseType::MidiFromType26,
            } => {
                let event = UsbMidiEvent {
                    code: record.code(),
                    status: record.status(),
                    data: [record.data1(), record.data2()],
                };
                let mut effects = self.drive(state, now, |controller| {
                    controller.receive_port(event, record.words[2], now)
                })?;
                if event.to_message() == Some(MidiMessage::Realtime(Realtime::TimingClock)) {
                    effects.extend(self.din_clock());
                }
                Ok(effects)
            }
            DspEvent::MidiClockTempo(tempo) => self.drive(state, now, |controller| {
                controller.usb_clock_tempo(tempo.int_arg, tempo.uint_arg)
            }),
            DspEvent::Flag0f => self.drive(state, now, MidiController::usb_clock_stopped),
            _ => Ok(Vec::new()),
        }
    }

    pub fn poll(&mut self, state: &mut State, now: Duration) -> Result<Vec<Effect>, MidiError> {
        self.drive(state, now, |controller| controller.poll(now))
    }

    /// `0x331c28`: the on-load Preset MIDI Out of `preset`. `0x527d80` sends it first,
    /// before the compare and the deploy, so it goes out even when the deploy fails
    /// (review r11 N7).
    pub fn preset_loading(
        &mut self,
        preset: &tetrad_preset::Preset,
        state: &State,
    ) -> Result<Vec<Effect>, MidiError> {
        let Some(controller) = self.controller.as_ref() else {
            return Ok(Vec::new());
        };
        let outputs = table(preset)
            .on_load_messages()
            .into_iter()
            .flat_map(|send| match send {
                Ok(messages) => messages
                    .iter()
                    .flat_map(|message| controller.transmit(message))
                    .collect(),
                Err(error) => vec![MidiOutput::Action(MidiAction::SlotRefused(error))],
            })
            .collect();
        route(outputs, state).read_only()
    }

    /// After the commit: the program change gate opens and `0x32e700` resets the CC Toggle
    /// states and the editor source. The on-load sends went out in [`Self::preset_loading`].
    pub fn preset_loaded(&mut self, state: &State) -> Result<Vec<Effect>, MidiError> {
        self.preset_ready = true;
        let Some(controller) = self.controller.as_mut() else {
            return Ok(Vec::new());
        };
        controller.set_preset_ready(true);
        controller.set_tempo_context(tempo_context(state));
        // An empty table: only the reset, no second on-load send.
        route(controller.preset_loaded(&PresetMidiOut::default()), state).read_only()
    }

    /// Preset MIDI Out for a physical footswitch or pedal (never for MIDI-sourced moves).
    pub fn trigger(&mut self, trigger: Trigger, state: &State) -> Result<Vec<Effect>, MidiError> {
        let Some(controller) = self.controller.as_mut() else {
            return Ok(Vec::new());
        };
        let table = preset_table(state);
        let outputs = match trigger {
            Trigger::Footswitch(stomp) => controller.footswitch(&table, stomp),
            Trigger::Expression { pedal, position } => {
                let pedal = match pedal {
                    Pedal::One => ExpressionPedal::Exp1,
                    Pedal::Two => ExpressionPedal::Exp2,
                };
                controller.expression(&table, pedal, position)
            }
        };
        route(outputs, state).read_only()
    }

    /// The Preset MIDI Out editor stored the list of `source` (stomp 0..=7, pedal 8, 9):
    /// the editor's source becomes `LastMidiMessageSource` (`0x3bf66c` → `0x32dce8`) and the
    /// CC Toggle states reset (`0x32dda0`). The table itself lives in the preset.
    pub fn source_edited(
        &mut self,
        source: u8,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, MidiError> {
        self.drive(state, now, |controller| {
            let edited = GeneralMidiMessage {
                source: u32::from(source),
                messages: Vec::new(),
            };
            controller
                .apply_remote_midi_settings(&mut PresetMidiOut::default(), None, Some(&[edited]))
                .unwrap_or_default()
        })
    }

    /// Channels whose voice messages go to extensions (bit `n` = channel index `n`).
    pub fn set_reserved_channels(&mut self, channels: u16) {
        self.reserved = channels;
        if let Some(controller) = self.controller.as_mut() {
            controller.set_reserved_channels(channels);
        }
    }

    /// The fixed native receive channel (1..=16); `None` for OMNI or before boot.
    pub fn native_channel(&self) -> Option<u8> {
        match self.controller.as_ref()?.settings().channel {
            ChannelSetting::Channel(channel) => Some(channel.number()),
            _ => None,
        }
    }

    /// One message an extension sends, as the controller sends any message.
    pub fn transmit(&self, message: &MidiMessage, state: &State) -> Result<Vec<Effect>, MidiError> {
        let Some(controller) = self.controller.as_ref() else {
            return Ok(Vec::new());
        };
        route(controller.transmit(message), state).read_only()
    }

    /// Runs one controller step, routes its outputs and re-arms the clock timer.
    fn drive(
        &mut self,
        state: &mut State,
        now: Duration,
        step: impl FnOnce(&mut MidiController) -> Vec<MidiOutput>,
    ) -> Result<Vec<Effect>, MidiError> {
        let Some(controller) = self.controller.as_mut() else {
            return Ok(Vec::new());
        };
        controller.set_tempo_context(tempo_context(state));
        let outputs = step(controller);
        let clock = controller.clock();
        let clock = ClockStatus {
            usb_running: clock.usb_running(),
            din_running: clock.din_running(),
            tempo: clock.external_tempo(),
            out_of_range: clock.out_of_range().is_some(),
        };
        let deadline = controller.next_deadline();
        let routed = route(outputs, state);
        if routed.refused {
            controller.program_change_refused();
        }
        let (save, beat, page) = (routed.save, routed.beat, routed.page);
        let mut effects = routed.into_effects();
        if beat {
            effects.extend(super::leds::clock_beat(state, clock));
        }
        effects.push(Effect::Action(Action::MidiClock(clock)));
        if save {
            effects.extend(persist(controller.settings(), state)?);
        }
        // `0x589c98` acts on a changed page; Control pushes Mode from the settings diff.
        if let Some(page) = page.filter(|&page| state.settings.settings().mode.atma_page != page) {
            effects.extend(actions::update_settings(state, |settings| {
                settings.mode.atma_page = page
            }));
        }
        if let Some(deadline) = deadline {
            effects.push(Effect::StartTimer {
                timer: Timer::MidiClock,
                after: deadline.saturating_sub(now),
            });
        }
        Ok(effects)
    }
}

#[derive(Default)]
struct Routed {
    effects: Vec<Effect>,
    pending: Vec<&'static str>,
    save: bool,
    /// A program change the device cannot load (no library).
    refused: bool,
    /// A tempo LED beat ([`super::leds::clock_beat`]) after the clock state of this step.
    beat: bool,
    /// CC#64 on the mini: the footswitch page (`Mode/atmaPage`).
    page: Option<i32>,
}

impl Routed {
    fn defer(&mut self, gap: &'static str) {
        if !self.pending.contains(&gap) {
            self.pending.push(gap);
        }
    }

    fn act(&mut self, action: Action) {
        self.effects.push(Effect::Action(action));
    }

    fn into_effects(mut self) -> Vec<Effect> {
        for detail in self.pending {
            self.effects
                .push(Effect::Notice(Subsystem::Midi, detail.to_owned()));
        }
        self.effects
    }

    fn read_only(self) -> Result<Vec<Effect>, MidiError> {
        if self.save || self.page.is_some() {
            return Err(MidiError::Unsupported(
                "MIDI settings save outside a state event",
            ));
        }
        Ok(self.into_effects())
    }
}

fn route(outputs: Vec<MidiOutput>, state: &State) -> Routed {
    let mut routed = Routed::default();
    for output in outputs {
        match output {
            MidiOutput::Din(bytes) => routed.effects.push(Effect::MidiOut(bytes)),
            // Tempo changes reach every driver's tempo hook before `0x16` (`0x875a48`).
            MidiOutput::Dsp(MidiDsp::TempoChange(bpm)) => {
                routed.effects.push(Effect::Dsp(DspCommand::Tempo(bpm)));
            }
            MidiOutput::Dsp(command) => routed
                .effects
                .push(Effect::Dsp(DspCommand::Control(control_request(command)))),
            MidiOutput::Action(action) => route_action(action, state, &mut routed),
            MidiOutput::Reserved(message) => routed.effects.push(Effect::ExtensionMidi(message)),
            MidiOutput::Unclaimed(message) => routed.effects.push(Effect::MappingMidi(message)),
        }
    }
    routed
}

/// The tetrad-midi DSP command as its `tetrad_dsp_protocol::controls` request.
fn control_request(command: MidiDsp) -> ControlRequest {
    match command {
        MidiDsp::TempoChange(bpm) => ControlRequest::TempoChange { bpm },
        MidiDsp::UsbMidiOut(event) => ControlRequest::UsbMidiOut {
            delivery: UsbMidiDelivery::Immediate,
            event: usb_event(event),
        },
        // `0x876200`: selector 1, word +4 = MIDI over USB.
        MidiDsp::PortMidiOut { event, usb } => ControlRequest::UsbMidiOut {
            delivery: UsbMidiDelivery::Tagged(u32::from(usb)),
            event: usb_event(event),
        },
        MidiDsp::EnableUsbMidiClock(on) => ControlRequest::EnableUsbMidiClock { value: on.into() },
        MidiDsp::EnableInternalMidiClock(on) => {
            ControlRequest::EnableInternalMidiClock { value: on.into() }
        }
        MidiDsp::ResetInternalMidiClock(value) => ControlRequest::ResetInternalMidiClock { value },
        // ClockOutEn: 0 off, 1 DIN, 2 USB, 3 both (i-midi §settings).
        MidiDsp::EnableMidiClockOut(mode) => ControlRequest::EnableMidiClockOut {
            value: match mode {
                ClockOut::Off => 0,
                ClockOut::Din => 1,
                ClockOut::Usb => 2,
                ClockOut::Both => 3,
            },
        },
        MidiDsp::EnableMidiClockIn(on) => ControlRequest::EnableMidiClockIn { value: on.into() },
        MidiDsp::MidiClockCountAndTempo { tempo, count } => {
            ControlRequest::MidiClockCountAndTempo {
                tempo_bits: tempo.to_bits(),
                count,
            }
        }
        MidiDsp::MidiClockStart => ControlRequest::MidiClockStart,
    }
}

fn usb_event(event: UsbMidiEvent) -> tetrad_dsp_protocol::controls::UsbMidiEvent {
    tetrad_dsp_protocol::controls::UsbMidiEvent {
        code: event.code,
        status: event.status,
        data: event.data,
    }
}

fn route_action(action: MidiAction, state: &State, routed: &mut Routed) {
    match action {
        MidiAction::LoadPreset {
            setlist,
            index,
            ignore_if_current,
        } => match program_target(setlist, index, state) {
            // `0x5b5bd8` case 3 → `0x5b2f40`: no load frame (review r39 K13, C).
            Some(target) => routed.act(Action::ProgramChange {
                target,
                reload: !ignore_if_current,
            }),
            None => routed.refused = true,
        },
        MidiAction::Expression { pedal, position } => routed.act(Action::Expression {
            pedal: match pedal {
                ExpressionPedal::Exp1 => Pedal::One,
                ExpressionPedal::Exp2 => Pedal::Two,
            },
            position,
            from_midi: true,
        }),
        MidiAction::Footswitch { stomp } => routed.effects.push(Effect::MidiFootswitch(stomp)),
        MidiAction::SelectScene(scene) => {
            if let Ok(scene) = SceneIndex::new(scene) {
                routed.act(Action::SelectScene(scene));
            }
        }
        MidiAction::TapTempo(bpm) => routed.act(Action::SetTempo(bpm as f32)),
        MidiAction::GlobalTempo(bpm) => routed.act(Action::SetTempo(bpm)),
        MidiAction::Tuner(show) => routed.act(Action::ShowTuner(show)),
        MidiAction::GigView(show) => routed.act(Action::ShowGigView(show)),
        MidiAction::ModeSlot(slot) => routed.act(Action::ModeSlot(slot)),
        MidiAction::FootswitchPage(page) => routed.page = Some(i32::from(page)),
        MidiAction::Looper(LooperControl::Button { button, pressed }) => {
            routed.act(Action::Looper { button, pressed })
        }
        MidiAction::Looper(LooperControl::Editor { open }) => {
            routed.effects.push(Effect::UiInput(if open {
                super::ui::UiInput::Show(tetrad_ui::Screen::Looper)
            } else {
                super::ui::UiInput::Hide(tetrad_ui::Screen::Looper)
            }))
        }
        MidiAction::Looper(LooperControl::Parameter { index, value }) => {
            if let Some(slot) = super::features::looper_slot(state) {
                routed.act(Action::SetParameter {
                    site: tetrad_engine::Site::Slot(slot),
                    index,
                    value,
                });
            } else {
                routed.defer("MIDI Looper X parameter needs a deployed Looper X block");
            }
        }
        // CC#60 writes editor `+0x187` (`0x55d1bc` case `0x3c`).
        MidiAction::Looper(LooperControl::EditorView(shown)) => {
            routed.act(Action::LooperView(shown))
        }
        // `0x514270` (notes) and `0x5141b8` (CC 120/123) to the grid drivers.
        MidiAction::Block(message) => {
            let (channel, message) = match message {
                BlockMidi::Note {
                    channel,
                    key,
                    velocity,
                    on,
                } => (
                    channel,
                    DriverMidi::Note {
                        note: key.get(),
                        velocity: velocity.get(),
                        on,
                    },
                ),
                BlockMidi::Controller {
                    channel,
                    controller,
                    ..
                } => (channel, DriverMidi::Control(controller.get())),
            };
            routed.effects.push(Effect::Dsp(DspCommand::BlockMidi {
                channel: channel.number(),
                message,
            }));
        }
        MidiAction::SetJackMidiMode(mode) => {
            let kind = match mode {
                JackMidiMode::Thru => MidiType::Thru,
                JackMidiMode::Out => MidiType::Out,
            };
            match Setting::new(Control::MidiType, Value::MidiType(kind)) {
                Ok(setting) => routed.effects.push(Effect::Hal(HalCommand::Set(
                    setting,
                    Verification::Unverified,
                ))),
                Err(_) => routed.defer("zenjack MIDI type setting refused"),
            }
        }
        // Control pushes each changed GeneralSettings field from its State diff: the settings
        // through `persist`, the clock fields (19, 40, 41) through `Action::MidiClock`.
        MidiAction::Report(_) => {}
        MidiAction::SaveSettings => routed.save = true,
        MidiAction::SlotRefused(error) => routed.effects.push(Effect::Notice(
            Subsystem::Midi,
            format!("MIDI slot refused: {error:?}"),
        )),
        MidiAction::MetronomeTempo(bpm) => routed.act(Action::MetronomeClockTempo(bpm)),
        // `0x89ba68`: the tempo LED beat and `0x557dbc` (metronome status request). tetrad-midi
        // emits this action only while `+0xfe` (DIN clock) and `+0xff` (out of range) are 0,
        // the stock gate. Stock also tests `QCoreApplication + 0x168` (`0x544dc4`) before
        // `0x557dbc`; Tetrad does not model that field. The `nopoll` marker is tested where the
        // request is sent.
        MidiAction::UsbClockTick => {
            routed.beat = true;
            routed.effects.push(Effect::Dsp(DspCommand::Control(
                ControlRequest::MetronomeStatusRequest,
            )));
        }
        MidiAction::TempoLedBeat => routed.beat = true,
        MidiAction::TempoLedFlash => routed.effects.extend(super::leds::clock_flash(state)),
        // The next LED and UI frame draws them from `State::midi_clock` (`Action::MidiClock`
        // follows every byte).
        MidiAction::ClockRunning(_)
        | MidiAction::ExternalTempoChanged
        | MidiAction::TempoLedRefresh => {}
    }
}

/// The library slot of a program change (`0x007c2794`): CC#32 0 is the Factory Library
/// (library field `+0x24`, built by `0x0076ff74` from `/opt/neuraldsp/Factory Library`);
/// N ≥ 1 is user setlist N − 1 (list `+8`, `user_preset_folders` order); out of range or no
/// CC#32 is the setlist of the loaded preset. Keys are setlist directory paths. `None` when
/// there is no library to load from, or when the loaded preset's folder is an auditioned
/// plugin folder (`0x5b5bd8`: the folder's `+0x64`/`+0x68` checks refuse the PC and keep the
/// CC#32 latch; a CC#32 setlist still loads).
fn program_target(setlist: SetlistSelect, index: u16, state: &State) -> Option<PresetRef> {
    let library = state.library.as_ref()?;
    let auditioned = state
        .current
        .as_ref()
        .and_then(|current| current.path.as_deref())
        .is_some_and(super::media::auditioned_path);
    let current = || (!auditioned).then(|| state.settings.settings().last_preset.setlist.clone());
    let setlist = match setlist {
        SetlistSelect::Current => current()?,
        SetlistSelect::Root => Some(super::FACTORY_SETLIST.to_owned()),
        SetlistSelect::Setlist(n) => match library
            .presets
            .setlists()
            .ok()
            .and_then(|setlists| setlists.into_iter().nth(usize::from(n)))
        {
            Some(setlist) => Some(setlist.path),
            None => current()?,
        },
    };
    Some(PresetRef {
        setlist,
        index: u32::from(index),
    })
}

/// Tempo scope parameter 1 (1 = global) and the stored BPM (`0x0064c828`).
fn tempo_context(state: &State) -> TempoContext {
    let tempo = &state.settings.settings().tempo;
    let global = tempo
        .params
        .get(&TempoSettings::SCOPE_PARAM)
        .is_some_and(|scope| scope.round() >= 1.0);
    TempoContext {
        mode: if global {
            TempoMode::Global
        } else {
            TempoMode::Preset
        },
        current_bpm: actions::tempo_bpm(state),
    }
}

/// Stores the controller's settings in tetrad-settings with the stock flush delay. The change
/// events are dropped: a `SettingsChanged(Midi)` would rebuild this controller.
fn persist(settings: &MidiSettings, state: &mut State) -> Result<Option<Effect>, MidiError> {
    let stored = to_stored(settings)?;
    state.settings.update(|all| all.midi = stored);
    Ok(actions::settings_flush(&state.settings))
}

/// The preset's three MIDI tables, as the controller's per-source output table.
fn preset_table(state: &State) -> PresetMidiOut {
    state
        .current
        .as_ref()
        .map_or_else(PresetMidiOut::default, |current| table(&current.preset))
}

fn table(preset: &tetrad_preset::Preset) -> PresetMidiOut {
    PresetMidiOut::from_fields(&PresetMidiFields {
        on_load: message_infos(&preset.midi_messages),
        general: message_infos(&preset.midi_messages_general),
        general_v2: message_infos(&preset.midi_messages_general_v2),
    })
}

/// The preset list behind an editor target and, for footswitch and pedal lists, the
/// controller source (`None` for a footswitch without a stomp tile).
pub(super) fn preset_midi_list(target: MidiOutTarget) -> Option<(MidiList, Option<u8>)> {
    let source = match target {
        MidiOutTarget::OnLoad => return Some((MidiList::OnLoad, None)),
        MidiOutTarget::Footswitch(switch) => u8::try_from(switch.tile()?).ok()?,
        MidiOutTarget::Expression1 => 8,
        MidiOutTarget::Expression2 => 9,
    };
    Some((MidiList::Group(usize::from(source)), Some(source)))
}

/// One editor message as the stock 40-byte entry: types 1..=3, one-based channel; an
/// OFF PC bank is `u32::MAX`, an unused CC part 0.
pub(super) fn stored_message(message: &MidiOutMessage) -> tetrad_preset::MidiMessage {
    let (kind, off) = match message.kind {
        MidiMessageKind::Cc => (MidiOutKind::ControlChange, 0),
        MidiMessageKind::CcToggle => (MidiOutKind::ControlToggle, 0),
        MidiMessageKind::Pc => (MidiOutKind::ProgramChange, u32::MAX),
    };
    let [param1, param2, param3] = message.values.map(|part| part.map_or(off, u32::from));
    tetrad_preset::MidiMessage {
        kind: u32::from(kind.raw()),
        channel: u32::from(message.channel),
        param1,
        param2,
        param3,
        ..tetrad_preset::MidiMessage::default()
    }
}

fn message_infos(messages: &[tetrad_preset::MidiMessage]) -> Vec<MidiMessageInfo> {
    messages
        .iter()
        .map(|message| MidiMessageInfo {
            message_type: message.kind,
            channel: message.channel,
            param1: message.param1,
            param2: message.param2,
            param3: message.param3,
        })
        .collect()
}

fn from_stored(stored: &StoredMidiSettings) -> Result<MidiSettings, MidiError> {
    let raw = stored.channel.get();
    Ok(MidiSettings {
        last_message_source: stored.last_message_source,
        channel: ChannelSetting::from_raw(raw).ok_or(MidiError::Channel(raw))?,
        thru: stored.thru,
        over_usb: stored.over_usb,
        clock_out: clock_out_from(stored.clock_out),
        ignore_duplicate_pc: stored.ignore_duplicate_pcs,
        clock_in: stored.clock_in,
        pc_input: stored.pc_input,
        pc_output: stored.pc_output,
    })
}

fn to_stored(settings: &MidiSettings) -> Result<StoredMidiSettings, MidiError> {
    let raw = settings.channel.raw();
    Ok(StoredMidiSettings {
        last_message_source: settings.last_message_source,
        channel: MidiChannel::new(raw).map_err(|_| MidiError::Channel(raw))?,
        thru: settings.thru,
        over_usb: settings.over_usb,
        clock_out: clock_out_to(settings.clock_out),
        ignore_duplicate_pcs: settings.ignore_duplicate_pc,
        clock_in: settings.clock_in,
        pc_input: settings.pc_input,
        pc_output: settings.pc_output,
    })
}

fn clock_out_from(mode: MidiClockOut) -> ClockOut {
    match mode {
        MidiClockOut::Off => ClockOut::Off,
        MidiClockOut::DinOnly => ClockOut::Din,
        MidiClockOut::UsbOnly => ClockOut::Usb,
        MidiClockOut::UsbAndDin => ClockOut::Both,
    }
}

fn clock_out_to(mode: ClockOut) -> MidiClockOut {
    match mode {
        ClockOut::Off => MidiClockOut::Off,
        ClockOut::Din => MidiClockOut::DinOnly,
        ClockOut::Usb => MidiClockOut::UsbOnly,
        ClockOut::Both => MidiClockOut::UsbAndDin,
    }
}
