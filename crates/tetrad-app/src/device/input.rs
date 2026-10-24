//! Hardware input bridge: HAL events to tetrad-core performance decisions, then device [`Action`]s,
//! UI input and Preset MIDI Out.
//!
//! The bridge keeps only raw-input state: the held mask, hold deadline, chord releases, knob
//! acceleration, pedal calibration and the last driver tempo. Everything shared (preset, scene,
//! stomps, mode, screens, tempo, overload) is an action; the QC Master Volume knob and takeover
//! live in [`State::master_volume`] and the jack status in [`State::jacks`] because Cortex
//! Control reads them.
//! Input-type writes use the jack scheduler. Footswitch LED painting is a separate bridge.

use std::time::Duration;

use tetrad_core::performance::footswitch::{
    BankOverlay, BankSize, LOGICAL_TO_PHYSICAL, stepped_preset,
};
use tetrad_core::performance::{
    ChordReleases, Footswitch, FootswitchAction, GigButton, HalStomp, PedalCalibration, TapContext,
    TapOutcome, accept_tempo_event, hold_outcome,
};
use tetrad_core::workflow::Millis;
use tetrad_engine::SceneIndex;
use tetrad_hal::hal::Event as HalEvent;
use tetrad_hal::node::{Element, Pedal};
use tetrad_hal::platform::Platform as HalPlatform;
use tetrad_hal::zencoder::{
    KnobAccelerator, VolumeCalibration, VolumeCalibrationError, VolumeEvent, VolumeKey,
};
use tetrad_settings::controls::ModeSettings;
use tetrad_settings::io::{ExpressionCalibration as StoredCalibration, ExpressionRange};
use tetrad_storage::media::SETLIST_SLOTS;
use tetrad_ui::{DeviceAction, InputEvent, KnobIndex, Screen, UiCommand as ViewCommand};
use tetrad_watchdog::power::{LongPress, power_button};

use super::actions::{self, Action, SceneClipboard};
use super::features::pedal_index;
use super::midi::Trigger;
use super::ui::UiInput;
use super::{DspCommand, Effect, PowerAction, PresetRef, State, Timer};

/// Presets per bank for this product and mode cycle (`0x58b968`). [`State::new`] and the
/// settings mirror keep `performance.footswitch.bank_size` equal to it.
pub fn bank_size(platform: HalPlatform, modes: &ModeSettings) -> BankSize {
    let cycle = modes
        .available()
        .iter()
        .filter_map(|&mode| actions::core_mode(mode));
    BankSize::of_cycle(platform.is_atma(), cycle)
}

/// Last bank of one setlist (tetrad-core `BankOverlay::bank` is caller-validated).
pub fn last_bank(size: BankSize) -> u32 {
    size.bank(SETLIST_SLOTS as u32 - 1)
}
/// HAL mask of footswitches A (HAL 0) and H (HAL 8): held together during the startup
/// screen they open Recovery Mode (manual §12; no stock check is found in ZenUI, inferred).
const RECOVERY_CHORD: u16 = (1 << 0) | (1 << 8);

/// `0x458914`, `0x6a86c4`.
pub const VOLUME_REPEAT: Duration = Duration::from_millis(50);
pub const POWER_LONG_PRESS: Duration = Duration::from_millis(3000);

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hold {
    switch: Footswitch,
    until: Duration,
}

#[derive(Debug, Default)]
pub struct Input {
    chord: ChordReleases,
    hold: Option<Hold>,
    /// Physical mask of footswitches whose long press ran; their release is quiet.
    long: u16,
    volume_keys: VolumeKeys,
    power_held: bool,
    expression: [PedalCalibration; 2],
    /// Per element (`0x00508e68` keeps one window per knob).
    knobs: [KnobAccelerator; Element::COUNT as usize],
    /// Last accepted zencoder tempo (`HalEvent::Tempo`), applied on the third TEMPO tap.
    driver_tempo: Option<u16>,
    /// Physical mask of momentary stomps that MIDI CC#35..=42 left pressed (`+0x1a0`).
    midi_pressed: u16,
    /// Physical mask of letters whose press ended a Looper X assignment; their release is quiet.
    assigned: u16,
    /// Owned: stomps (bit per index) whose press reached the mappings. Their release goes there
    /// too, even when the press changed the footswitch mode.
    mapping_held: u8,
}

#[derive(Debug, Default)]
struct VolumeKeys {
    /// Hold ticks. Stock adds 0.05 in f32 and steps above 0.5 (`0x458d24`); that sum
    /// passes 0.5 on tick 10, so the first repeat is at 500 ms.
    held: [Option<u8>; 2],
    blocked: bool,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pedal ranges from `ExpressionsSettings` (`expression{1,2}{Min,Max}`).
    pub fn calibrated(saved: &StoredCalibration) -> Self {
        let mut input = Self::default();
        input.calibration_changed(saved);
        input
    }

    /// The stored ranges changed (a confirmed calibration or a settings reset). A running
    /// calibration ends with them.
    pub fn calibration_changed(&mut self, saved: &StoredCalibration) {
        self.expression = saved
            .pedals
            .map(|range| PedalCalibration::saved(range.min, range.max));
    }

    /// When the pending TEMPO or UP/DOWN hold is due. Call [`Self::on_hold_expired`] then.
    pub fn hold_deadline(&self) -> Option<Duration> {
        self.hold.map(|hold| hold.until)
    }

    /// Runs a due hold: TEMPO opens the tuner or tempo popup, UP/DOWN step the bank overlay.
    pub fn on_hold_expired(&mut self, state: &mut State, now: Duration) -> Vec<Effect> {
        let Some(hold) = self.hold.filter(|hold| hold.until <= now) else {
            return Vec::new();
        };
        self.hold = None;
        if state.performance.held & hold.switch.physical().mask() == 0 {
            return Vec::new();
        }
        match hold.switch {
            Footswitch::TEMPO => {
                let swap = state
                    .settings
                    .settings()
                    .general
                    .swap_tempo_and_tuner_access;
                self.tap_outcome(hold_outcome(swap)).into_iter().collect()
            }
            switch => {
                self.long |= switch.physical().mask();
                state
                    .performance
                    .footswitch
                    .long_press(switch)
                    .map_or_else(Vec::new, |action| self.apply(action, state))
            }
        }
    }

    /// MIDI CC#35..=42 (`0x0020fb78`): footswitch A..H (`letter` 0..=7) as hardware edges
    /// of its element (table `0xa19268` = [`LOGICAL_TO_PHYSICAL`]), so the result follows the
    /// current mode. A stomp stored momentary (`0x002e220c`) gets one edge per message,
    /// alternating press and release (`+0x1a0`; the first edge being a press is inferred).
    /// Any other switch gets a press, then a release.
    pub fn midi_footswitch(
        &mut self,
        letter: u8,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, InputError> {
        let element = LOGICAL_TO_PHYSICAL[..8]
            .get(usize::from(letter))
            .and_then(|&index| Element::try_from(index).ok())
            .ok_or(InputError::Unsupported("MIDI footswitch outside A..=H"))?;
        let momentary = state.current.as_ref().is_some_and(|current| {
            let stored = &current.preset.stomp_is_momentary;
            stored.get(u32::from(letter)).copied().unwrap_or(false)
        });
        if momentary {
            let bit = 1 << element.index();
            self.midi_pressed ^= bit;
            let value = i32::from(self.midi_pressed & bit != 0);
            return self.stomp(element, value, state, now);
        }
        let mut effects = self.stomp(element, 1, state, now)?;
        effects.extend(self.stomp(element, 0, state, now)?);
        Ok(effects)
    }

    pub fn expression_heel(&self, state: &State, pedal: Pedal) -> bool {
        state.pedals[pedal_index(pedal)].emulated_switch() > 0.5
    }

    pub fn on_hal(
        &mut self,
        event: HalEvent,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, InputError> {
        Ok(match event {
            HalEvent::Stomp { element, value } => self.stomp(element, value, state, now)?,
            HalEvent::Volume(VolumeEvent::Level(raw)) => {
                if state.platform.is_atma() {
                    Vec::new()
                } else {
                    knob_volume(state, raw)
                }
            }
            HalEvent::Volume(VolumeEvent::Key { key, pressed }) => {
                if state.platform.is_atma() {
                    self.volume_key(key, pressed)
                } else {
                    Vec::new()
                }
            }
            HalEvent::Expression { pedal, value } => {
                let index = pedal_index(pedal);
                match self.expression[index].position(value) {
                    Some(position) => {
                        vec![
                            Effect::PresetMidiOut(Trigger::Expression { pedal, position }),
                            Effect::Action(Action::Expression {
                                pedal,
                                position,
                                from_midi: false,
                            }),
                        ]
                    }
                    None => Vec::new(),
                }
            }
            HalEvent::Jacks(status) => {
                state.jacks = status;
                vec![Effect::Action(Action::PedalPlugs(status.plugged))]
            }
            HalEvent::Tempo(value) => {
                if let Some(bpm) = accept_tempo_event(value) {
                    self.driver_tempo = Some(bpm);
                }
                Vec::new()
            }
            HalEvent::Knob { element, delta } => self.knob(element, delta, now),
            HalEvent::PowerButton(value) => {
                let product = tetrad_watchdog::cmdline::Product::from(state.platform);
                let button = power_button(product, value);
                let mut effects = vec![Effect::UiInput(UiInput::PowerButton {
                    pressed: button.pressed,
                })];
                match button.long_press {
                    LongPress::Keep => {}
                    LongPress::Start if !self.power_held => {
                        self.power_held = true;
                        effects.push(Effect::StartTimer {
                            timer: Timer::PowerLongPress,
                            after: POWER_LONG_PRESS,
                        });
                    }
                    LongPress::Start => {}
                    LongPress::Stop => {
                        self.power_held = false;
                        effects.push(Effect::StopTimer(Timer::PowerLongPress));
                    }
                }
                effects
            }
            HalEvent::Overload(channel) => {
                vec![Effect::Action(Action::Overload { channel, at: now })]
            }
        })
    }

    /// The mini hold repeats after the counter passes 0.5 (`0x458d24`, `0x458dbc`).
    pub fn on_volume_timer(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (index, held) in self.volume_keys.held.iter_mut().enumerate() {
            if let Some(ticks) = held {
                *ticks = ticks.saturating_add(1);
                if *ticks >= 10 && !self.volume_keys.blocked {
                    effects.push(Effect::Action(Action::StepVolume(if index == 0 {
                        -0.05
                    } else {
                        0.05
                    })));
                }
            }
        }
        if self.volume_keys.held.iter().any(Option::is_some) {
            effects.push(Effect::StartTimer {
                timer: Timer::VolumeRepeat,
                after: VOLUME_REPEAT,
            });
        }
        effects
    }

    /// Same SHUTDOWN action as the power menu (`0x6a87d4`).
    pub fn on_power_timer(&mut self) -> Vec<Effect> {
        if std::mem::take(&mut self.power_held) {
            vec![Effect::Power(PowerAction::PowerOff)]
        } else {
            Vec::new()
        }
    }

    fn volume_key(&mut self, key: VolumeKey, pressed: bool) -> Vec<Effect> {
        let index = match key {
            VolumeKey::Down => 0,
            VolumeKey::Up => 1,
        };
        let keys = &mut self.volume_keys;
        let active = keys.held.iter().any(Option::is_some);
        let mut effects = Vec::new();
        if pressed {
            if keys.held[index].is_some() {
                return effects;
            }
            keys.held[index] = Some(0);
            keys.blocked |= keys.held.iter().all(Option::is_some);
            if !active {
                effects.push(Effect::StartTimer {
                    timer: Timer::VolumeRepeat,
                    after: VOLUME_REPEAT,
                });
            }
        } else {
            if keys.held[index].take().is_some() && !keys.blocked {
                effects.push(Effect::Action(Action::StepVolume(if index == 0 {
                    -0.05
                } else {
                    0.05
                })));
            }
            if keys.held.iter().all(Option::is_none) {
                // Stock keeps one block flag per key and clears it on that key's press; a
                // press while the other key is held sets both again (`0x458aa8`, `0x458b10`).
                // One flag cleared when both keys are up gives the same steps.
                keys.blocked = false;
                effects.push(Effect::StopTimer(Timer::VolumeRepeat));
            }
        }
        effects
    }

    /// Expression calibration from the UI (`CALIBRATE EXP n`, n = 1 or 2): the device action
    /// starts tracking (`0x0032103c`); confirm stores the tracked range in
    /// `ExpressionsSettings`, cancel restores the saved range.
    pub fn on_ui(
        &mut self,
        command: &ViewCommand,
        state: &mut State,
    ) -> Result<Vec<Effect>, InputError> {
        let pedal = |number: u8| {
            usize::from(number)
                .checked_sub(1)
                .filter(|&index| index < 2)
                .ok_or(InputError::Unsupported("expression pedal outside 1..=2"))
        };
        match *command {
            ViewCommand::Device {
                action: DeviceAction::CalibrateExpression(number),
            } => {
                self.expression[pedal(number)?].begin();
                Ok(Vec::new())
            }
            ViewCommand::ExpressionCalibration {
                pedal: number,
                save,
            } => {
                let index = pedal(number)?;
                if !save {
                    let saved = state.settings.settings().expression.pedals[index];
                    self.expression[index] = PedalCalibration::saved(saved.min, saved.max);
                    return Ok(Vec::new());
                }
                let Some((min, max)) = self.expression[index].finish() else {
                    return Ok(Vec::new());
                };
                let range = ExpressionRange { min, max };
                Ok(actions::update_settings(state, |settings| {
                    settings.expression.pedals[index] = range;
                }))
            }
            _ => Err(InputError::Unsupported(
                "UI command without an input meaning",
            )),
        }
    }

    fn stomp(
        &mut self,
        element: Element,
        value: i32,
        state: &mut State,
        now: Duration,
    ) -> Result<Vec<Effect>, InputError> {
        let stomp = HalStomp::new(u32::from(element.index()))
            .ok_or(InputError::Unsupported("stomp index outside 0..=10"))?;
        let switch = Footswitch::from_physical(stomp);
        let pressed = value > 0;
        // The press that completes A + H while the splash shows opens Recovery (QC only);
        // both releases stay quiet.
        let held = state.performance.held | stomp.mask();
        if pressed
            && !state.splash_dismissed
            && !state.platform.is_atma()
            && stomp.mask() & RECOVERY_CHORD != 0
            && held & RECOVERY_CHORD == RECOVERY_CHORD
        {
            state.performance.held = held;
            self.long |= RECOVERY_CHORD;
            return Ok(vec![Effect::UiInput(UiInput::Show(Screen::Recovery))]);
        }
        // Looper X swap dialog (`0x37ab08`): the ModeController block (`0x5898f4(1)`) keeps every
        // footswitch from its normal meaning; a letter takes the picked tile's action.
        let view = |pressed| {
            view_switch(switch).map(|switch| {
                Effect::UiInput(UiInput::View(InputEvent::Footswitch { switch, pressed }))
            })
        };
        if let Some(tile) = state.looper_assign.filter(|_| pressed) {
            let Some(letter) = switch.letter() else {
                return Ok(Vec::new());
            };
            self.assigned |= stomp.mask();
            let mut effects = super::features::swap_looper_actions(state, tile, letter);
            effects.extend(view(true));
            return Ok(effects);
        }
        // "Assign footswitch" and Gig EDIT STOMP set the same block (`0x4b569c`, `0x2c5710`):
        // the press goes to the dialog only, and its release stays quiet (review r14 U-1).
        if footswitch_modal(state) && pressed {
            self.assigned |= stomp.mask();
            return Ok(view(true).into_iter().collect());
        }
        if !pressed && self.assigned & stomp.mask() != 0 {
            self.assigned &= !stomp.mask();
            return Ok(view(false).into_iter().collect());
        }
        // Looper editor hook `0x613510`: with `+0x187` set, letters drive the tiles (with the
        // tile gates and the store path) and DOWN, TEMPO and UP do nothing; with it clear,
        // only DOWN and UP are consumed.
        if state.screen == Screen::Looper {
            match switch.letter() {
                Some(letter) if state.looper_view => {
                    use tetrad_midi::LooperButton as B;
                    // A stored value is a button number; 8 and above select Duplicate (tile 0).
                    let button = match state.settings.settings().looper.stomp_assignments()
                        [usize::from(letter)]
                    {
                        0 => B::RecordOverdub,
                        1 => B::PlayStop,
                        2 => B::Undo,
                        4 => B::OneShot,
                        5 => B::HalfSpeed,
                        6 => B::PunchIn,
                        7 => B::Reverse,
                        _ => B::Duplicate,
                    };
                    return Ok(vec![
                        Effect::Action(Action::LooperTile { button, pressed }),
                        Effect::UiInput(UiInput::View(InputEvent::Footswitch {
                            switch: view_switch(switch).expect("letter switch"),
                            pressed,
                        })),
                    ]);
                }
                None if state.looper_view || switch != Footswitch::TEMPO => {
                    return Ok(Vec::new());
                }
                _ => {}
            }
        }
        let mut effects = if pressed {
            state.performance.held |= stomp.mask();
            self.press(switch, state, now)
        } else {
            state.performance.held &= !stomp.mask();
            let mut effects = self.release(switch, state);
            if let Some(letter) = switch.letter()
                && self.mapping_held & (1 << letter) != 0
            {
                self.mapping_held &= !(1 << letter);
                let release = Effect::MappingStomp {
                    stomp: letter,
                    pressed: false,
                };
                if !effects.contains(&release) {
                    effects.push(release);
                }
            }
            effects
        };
        // tetrad-ui lights its tiles from the raw edge; tetrad-core above owns the meaning.
        effects.extend(view(pressed));
        Ok(effects)
    }

    fn press(&mut self, switch: Footswitch, state: &mut State, now: Duration) -> Vec<Effect> {
        let held = state.performance.held;
        let action = state
            .performance
            .footswitch
            .press(switch, held, &mut self.chord);
        // Copy / Swap Scene (`0x5ad9d4`) turns the assign mode on (ModeController `+0x14`,
        // `0x5898f4(1)`, predicate `0x589924`): the footswitches lose their normal meaning
        // (taps, holds, presets, stomps, banks) and only the dialog's scene footswitch acts
        // (`0x20eec4`). tetrad-core still sees the edge for its chord state.
        if state.performance.footswitch.scene_copy {
            self.hold = None;
            return match action {
                Some(action @ FootswitchAction::CopyScene(_)) => self.apply(action, state),
                _ => Vec::new(),
            };
        }
        let mut effects = Vec::new();
        // While the Tuner shows, one TEMPO click closes it and is not a tap (gtest
        // `testQCTunerAccess` `0x43c288`). The press handler `0x20db38` resets the count and
        // starts no timer when its visibility test passes.
        let closes_tuner = switch == Footswitch::TEMPO && state.screen == Screen::Tuner;
        if switch == Footswitch::TEMPO {
            let ctx = TapContext {
                blocked: false,
                tempo_switch_alone: held == HalStomp::QC_TEMPO.mask(),
                counting_suppressed: closes_tuner,
                tempo_popup_visible: state.screen == Screen::Tempo,
                swap_tempo_and_tuner: state
                    .settings
                    .settings()
                    .general
                    .swap_tempo_and_tuner_access,
            };
            let outcome = state.performance.tap.footswitch(1.0, millis(now), ctx);
            effects.extend(self.tap_outcome(outcome));
        }
        if closes_tuner {
            effects.push(Effect::Action(Action::ShowTuner(false)));
            self.hold = None;
        } else if action == Some(FootswitchAction::CycleMode) {
            self.hold = None;
        } else if matches!(
            switch,
            Footswitch::UP | Footswitch::DOWN | Footswitch::TEMPO
        ) {
            self.hold = Some(Hold {
                switch,
                until: now + hold_timing(state),
            });
        }
        if let Some(action) = action {
            effects.extend(self.apply(action, state));
        }
        effects
    }

    fn release(&mut self, switch: Footswitch, state: &mut State) -> Vec<Effect> {
        if self.hold.is_some_and(|hold| hold.switch == switch) {
            self.hold = None;
        }
        let bit = switch.physical().mask();
        let long = self.long & bit != 0;
        self.long &= !bit;
        let action = state.performance.footswitch.release(
            switch,
            state.performance.held,
            long,
            &mut self.chord,
        );
        // The assign mode of Copy / Swap Scene blocks every release action as well.
        let action = action.filter(|_| !state.performance.footswitch.scene_copy);
        action.map_or_else(Vec::new, |action| self.apply(action, state))
    }

    /// Cortex Control GigViewButton (55, `0x109ab4`; r16-control MR 5): a press or release of
    /// Gig View tile `tile` (below 8, mini 4) as `FootswitchContext::gig_button` decides. A
    /// stomp tile runs the stomp toggle or release and the driver hooks as a footswitch does.
    /// I: no Preset MIDI Out (a remote press, not a physical switch). Owned: like a physical
    /// press it does nothing while the update install masks the inputs (`masked`), the
    /// Assign/EDIT STOMP or NC2 dialog is open or Copy/Swap Scene mode is on (stock not
    /// verified).
    pub fn gig_view_button(
        &mut self,
        tile: u8,
        pressed: bool,
        state: &State,
        masked: bool,
    ) -> Vec<Effect> {
        if masked || footswitch_modal(state) || state.performance.footswitch.scene_copy {
            return Vec::new();
        }
        let tiles = if state.platform.is_atma() { 4 } else { 8 };
        let button = (tile < tiles)
            .then(|| state.performance.footswitch.gig_button(tile, pressed))
            .flatten();
        let stomp = |action, stomp, pressed| {
            vec![
                Effect::Action(action),
                Effect::Dsp(DspCommand::Footswitch { stomp, pressed }),
            ]
        };
        match button {
            Some(GigButton::LoadPreset(index)) => vec![Effect::Action(Action::LoadPreset {
                target: PresetRef {
                    setlist: state.settings.settings().last_preset.setlist.clone(),
                    index,
                },
                reload: false,
            })],
            Some(GigButton::SelectScene(scene)) => {
                self.apply(FootswitchAction::SelectScene(scene), state)
            }
            Some(GigButton::StompPress(index)) => stomp(Action::ToggleStomp(index), index, true),
            Some(GigButton::StompRelease(index)) => {
                stomp(Action::ReleaseStomp(index), index, false)
            }
            None => Vec::new(),
        }
    }

    /// One tetrad-core footswitch decision as device actions.
    fn apply(&mut self, action: FootswitchAction, state: &State) -> Vec<Effect> {
        let footswitch = &state.performance.footswitch;
        let size = footswitch.bank_size;
        let act = |action| vec![Effect::Action(action)];
        match action {
            FootswitchAction::CycleMode => act(Action::CycleMode),
            FootswitchAction::CancelBankOverlay => act(Action::BankOverlay(None)),
            FootswitchAction::LoadPreset(index) => act(load(state, index)),
            FootswitchAction::PreviewPreset(index) => {
                // The preview keeps the overlay bank; split-bank E–H may reach past it.
                let bank = footswitch
                    .overlay
                    .map_or(size.bank(index), |overlay| overlay.bank);
                act(Action::BankOverlay(Some(BankOverlay {
                    bank,
                    highlighted: u8::try_from(index.wrapping_sub(size.first(bank))).ok(),
                })))
            }
            FootswitchAction::StepBank(step) => {
                let bank = footswitch
                    .overlay
                    .map_or(size.bank(footswitch.current_preset), |overlay| overlay.bank);
                // Inferred: the overlay stops at the first and last bank (as the UI preview).
                let bank = bank
                    .saturating_add_signed(i32::from(step))
                    .min(last_bank(size));
                act(Action::BankOverlay(Some(BankOverlay {
                    bank,
                    highlighted: None,
                })))
            }
            // `0x5b57b8`: an auditioned plugin preset steps inside its folder.
            FootswitchAction::StepPreset(step) => super::media::audition_step(state, step)
                .unwrap_or_else(|| {
                    act(load(state, stepped_preset(footswitch.current_preset, step)))
                }),
            FootswitchAction::SelectScene(scene) => match SceneIndex::new(scene.get()) {
                Ok(scene) => act(Action::SelectScene(scene)),
                Err(_) => Vec::new(),
            },
            // `0x20eec4`: a scene other than the clipboard source takes the copy or swap
            // (`0x51a2a8`), is forced (`0x5899d8(dst, 1)`) and the mode ends (`0x459de4`).
            // Inferred: on the mini too the press acts (stock: the release).
            FootswitchAction::CopyScene(scene) => {
                let (Ok(target), Some(clipboard)) =
                    (SceneIndex::new(scene.get()), state.perform.scene_clipboard)
                else {
                    return Vec::new();
                };
                match clipboard.source {
                    Some(source) if source != target => vec![
                        Effect::Action(Action::CopyScene {
                            first: source,
                            second: target,
                            swap: clipboard.swap,
                        }),
                        Effect::Action(Action::SceneCopyMode(None)),
                    ],
                    _ => act(Action::SceneCopyMode(Some(SceneClipboard {
                        source: Some(target),
                        ..clipboard
                    }))),
                }
            }
            FootswitchAction::StompPress(stomp) => {
                let mut effects = act(Action::ToggleStomp(stomp));
                self.mapping_held |= 1 << stomp.get();
                effects.push(Effect::MappingStomp {
                    stomp: stomp.get(),
                    pressed: true,
                });
                // Driver hooks `+0x40`/`+0x44` of the stomp's blocks (`0x5239d0`, `0x523f80`).
                effects.push(Effect::Dsp(DspCommand::Footswitch {
                    stomp,
                    pressed: true,
                }));
                if !state.platform.is_atma() {
                    effects.extend(stomp_midi(state, stomp.get()));
                }
                effects
            }
            FootswitchAction::StompRelease(stomp) => {
                let mut effects = act(Action::ReleaseStomp(stomp));
                effects.push(Effect::MappingStomp {
                    stomp: stomp.get(),
                    pressed: false,
                });
                effects.push(Effect::Dsp(DspCommand::Footswitch {
                    stomp,
                    pressed: false,
                }));
                if state.platform.is_atma() {
                    effects.extend(stomp_midi(state, stomp.get()));
                }
                effects
            }
        }
    }

    fn tap_outcome(&mut self, outcome: TapOutcome) -> Option<Effect> {
        let action = match outcome {
            TapOutcome::ShowTuner => Action::ShowTuner(true),
            TapOutcome::ShowTempo => Action::ShowTempo(true),
            TapOutcome::ApplyDriverTempo => Action::SetTempo(f32::from(self.driver_tempo?)),
            TapOutcome::Ignored | TapOutcome::Counted => return None,
        };
        Some(Effect::Action(action))
    }

    /// Accelerated encoder steps (`0x00508e68`) for the on-screen parameter knob. Inferred
    /// map (cell order A–H, BANK DOWN, TEMPO): the logical index 0..=9 is the knob index; the
    /// BANK UP encoder (logical 10) is [`Action::BankEncoder`].
    fn knob(&mut self, element: Element, delta: i32, now: Duration) -> Vec<Effect> {
        let now_ms = i64::try_from(now.as_millis()).unwrap_or(i64::MAX);
        let delta = self.knobs[usize::from(element.index())].push(now_ms, delta);
        let Some(switch) = HalStomp::new(u32::from(element.index())).map(Footswitch::from_physical)
        else {
            return Vec::new();
        };
        if delta == 0 {
            return Vec::new();
        }
        if switch == Footswitch::UP {
            return vec![Effect::Action(Action::BankEncoder(delta))];
        }
        KnobIndex::new(switch.logical())
            .map(|knob| Effect::UiInput(UiInput::View(InputEvent::Knob { knob, delta })))
            .into_iter()
            .collect()
    }
}

/// Who drives the QC Master Volume (`+0x10`; transitions `0x1fb86c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VolumeOwner {
    /// 1: the knob drives the level.
    #[default]
    Knob,
    /// 2: Cortex Control set the level.
    Control,
    /// 3: the knob is above the level; it takes over when it comes down to it.
    PickupFromAbove,
    /// 4: the knob is below the level; it takes over when it comes up to it.
    PickupFromBelow,
}

/// QC Master Volume (`+8` level, `+0xc` knob, `+0x10` owner, `+0x28`/`+0x2c` knob range).
/// Cortex Control READ (`0x1962c8`) reports [`Self::level`] (`0x1fb830`) and
/// [`Self::engaged`] (`0x1fb848`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MasterVolume {
    /// The level the DSP has (`0x04`), 0..=1.
    pub level: f32,
    /// The last knob position, 0..=1.
    pub knob: f32,
    pub owner: VolumeOwner,
    /// Lock Touchscreen and Master Volume (`0x6a8984`), set by [`super::Action::SetLocked`].
    pub locked: bool,
    /// The knob range ([`load_volume_calibration`]; defaults 2..=242).
    pub calibration: VolumeCalibration,
}

impl MasterVolume {
    /// Control `engaged`: the knob drives the level.
    pub fn engaged(&self) -> bool {
        self.owner == VolumeOwner::Knob
    }

    /// `0x1fb86c`: locked, Cortex Control owns the level ([`VolumeOwner::Control`]), so the
    /// knob picks it up after the unlock. The volume input bit (`0x10`) is not in [`State`]
    /// and is taken as set (I); stock forces [`VolumeOwner::Control`] without it too.
    fn settle(&mut self) {
        use VolumeOwner::*;
        if self.locked {
            self.owner = Control;
            return;
        }
        self.owner = match self.owner {
            Control if self.knob > self.level => PickupFromAbove,
            Control if self.knob < self.level => PickupFromBelow,
            Control => Knob,
            PickupFromAbove if self.knob <= self.level => Knob,
            PickupFromBelow if self.knob >= self.level => Knob,
            owner => owner,
        };
    }
}

/// Stock float equality of the volume paths (`|a − b| ≤ min(|a|, |b|) · 1e-5`).
fn same_level(a: f32, b: f32) -> bool {
    (a - b).abs() <= a.abs().min(b.abs()) * 1e-5
}

/// `0x1fba5c`–`0x1fba7c`: a knob move counts when `|old − new|` (an f32 difference) is at
/// least 0.005 or the new level is below 0.0001, both compared in double (`vcvt.f64.f32`),
/// and the levels are not [`same_level`].
fn knob_moved(old: f32, new: f32) -> bool {
    (f64::from((old - new).abs()) >= 0.005 || f64::from(new) < 0.0001) && !same_level(old, new)
}

/// QC knob word (`0x1fb980`): a move ([`knob_moved`]) updates the knob; the DSP level
/// follows only while the knob drives it. Then, as stock: Control `engaged` true when the
/// knob takes over (`0x19b518`), the DSP level, and the level echo (`0x19af74`).
fn knob_volume(state: &mut State, raw: i32) -> Vec<Effect> {
    let knob = state.master_volume.calibration.level(raw);
    let volume = &mut state.master_volume;
    if !knob_moved(volume.knob, knob) {
        return Vec::new();
    }
    let was_engaged = volume.engaged();
    volume.knob = knob;
    volume.settle();
    if !volume.engaged() {
        return Vec::new();
    }
    volume.level = knob;
    let mut effects = Vec::new();
    if !was_engaged {
        effects.push(super::control::engaged_notice(true));
    }
    effects.push(actions::master_volume(knob));
    effects.push(Effect::ControlNotify(Box::new(
        super::control::master_volume_echo(knob),
    )));
    effects
}

/// The MasterVolume ctor (`0x1fd8d0`) on the QC: the factory knob range from the U-Boot
/// environment (`0x1fcfb4`, [`VolumeCalibration::from_env`]; `env` is the `fw_printenv`
/// map). The mini keeps its range untouched. On an error the range is 2..=242, as stock.
pub fn load_volume_calibration(
    state: &mut State,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<(), VolumeCalibrationError> {
    if state.platform.is_atma() {
        return Ok(());
    }
    let calibration = VolumeCalibration::from_env(env);
    state.master_volume.calibration = calibration.as_ref().copied().unwrap_or_default();
    calibration.map(|_| ())
}

/// `0x1fc518` at boot, after a reset and on wake from Be Right Back: the QC knob read (`None`:
/// the read failed, stock 0.0) becomes the level and the knob drives it; the mini resends its
/// `AtmaVolume`. DSP `0x04` only, no Control push.
pub fn read_volume(state: &mut State, raw: Option<i32>) -> Effect {
    let level = if state.platform.is_atma() {
        state.settings.settings().atma_volume.get()
    } else {
        raw.map_or(0.0, |raw| state.master_volume.calibration.level(raw))
    };
    state.master_volume = MasterVolume {
        level,
        knob: level,
        owner: VolumeOwner::Knob,
        ..state.master_volume
    };
    actions::master_volume(level)
}

/// Cortex Control MasterVolume UPDATE on the QC (`0x1fbb7c`, QC branch): Control takes the
/// level, then the knob must pick it up ([`VolumeOwner`]). Stock pushes `engaged` false when
/// the owner was not Control (`0x19b518`) and then the level (`0x19af74`); compare
/// [`MasterVolume::engaged`] before and after. Owned: the level is clamped to 0..=1 and a
/// non-finite level is ignored (stock sends any float to the DSP).
pub fn control_volume(state: &mut State, level: f32) -> Vec<Effect> {
    let volume = &mut state.master_volume;
    if !level.is_finite() {
        return Vec::new();
    }
    let level = level.clamp(0.0, 1.0);
    if same_level(volume.level, level) {
        return Vec::new();
    }
    volume.level = level;
    volume.owner = VolumeOwner::Control;
    volume.settle();
    vec![actions::master_volume(level)]
}

/// Preset MIDI Out of a stomp switch, with part of the stock gate (`0x00332064`): no tuner
/// and no bank overlay. The other stock predicates (about eight UI states and the stomp
/// assignment) are not modelled (inferred gate, w2c-integrator review fixes).
fn stomp_midi(state: &State, stomp: u8) -> Option<Effect> {
    (state.screen != Screen::Tuner && state.performance.footswitch.overlay.is_none())
        .then_some(Effect::PresetMidiOut(Trigger::Footswitch(stomp)))
}

/// A dialog takes every footswitch press: "Assign footswitch" / Gig EDIT STOMP (`0x5898f4`
/// block) or the NC2 dialog (I: `showDialog` `0x3d791c` connects the raw surface
/// `stompUpdateSignal` for its A/B toggle `0x3d8284`; while the NC2 preset runs a normal
/// action would only change the user preset; review r18 F5).
fn footswitch_modal(state: &State) -> bool {
    state.footswitch_dialog || state.views.capture.nc2.is_some()
}

/// The tetrad-ui switch for a logical footswitch; TEMPO has no tile.
fn view_switch(switch: Footswitch) -> Option<tetrad_ui::Footswitch> {
    use tetrad_ui::Footswitch as V;
    match switch {
        Footswitch::DOWN => Some(V::BankDown),
        Footswitch::UP => Some(V::BankUp),
        switch => switch
            .letter()
            .map(|letter| [V::A, V::B, V::C, V::D, V::E, V::F, V::G, V::H][usize::from(letter)]),
    }
}

fn millis(now: Duration) -> Millis {
    Millis(u64::try_from(now.as_millis()).unwrap_or(u64::MAX))
}

fn hold_timing(state: &State) -> Duration {
    Duration::from_millis(u64::from(
        state.settings.settings().general.hold_timing.millis(),
    ))
}

/// A footswitch always reloads, in the setlist of the loaded preset.
fn load(state: &State, index: u32) -> Action {
    Action::LoadPreset {
        target: PresetRef {
            setlist: state.settings.settings().last_preset.setlist.clone(),
            index,
        },
        reload: true,
    }
}

#[cfg(test)]
mod tests {
    use super::knob_moved;

    /// Review-w6 C17: the dead-band compares in double. A move of the f32 nearest 0.005 is
    /// below the double 0.005, so it is not a move; an f32 comparison would take it.
    #[test]
    fn knob_dead_band_compares_in_double() {
        let step = 0.005_f32;
        // Doubling and the subtraction are exact, so the difference is `step` itself.
        let (old, new) = (step * 2.0, step);
        let delta = (old - new).abs();
        assert!(
            delta >= step && f64::from(delta) < 0.005,
            "the f32 and f64 tests differ"
        );
        assert!(!knob_moved(old, new));
        assert!(knob_moved(0.5, 0.494));
        assert!(
            knob_moved(0.002, 0.00005),
            "a move to below 0.0001 always counts"
        );
    }
}
