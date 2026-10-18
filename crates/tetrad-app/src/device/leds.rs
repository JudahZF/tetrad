//! Footswitch and tempo LED glue: one `tetrad_core::performance::leds` frame per present pass, sent
//! as the HAL writes that differ from the last frame. Inputs `State` does not hold yet take a fixed
//! value (listed in the W4 doc §12).

use std::time::Duration;

use tetrad_core::performance::footswitch::{Footswitch, HalStomp};
use tetrad_core::performance::leds::{
    self, LOGICAL_TO_LED, Led, LedBlock, LedInputs, LedSettings, LedView, LooperFunction, Product,
    StompLed, SwitchSlot, TempoLedInputs,
};
use tetrad_core::performance::stomp::STOMPS;
use tetrad_core::performance::{FootswitchKind, StompIndex};
use tetrad_core::workflow::{BankSlot, SceneIndex};
use tetrad_engine::Site;
use tetrad_hal::hal::Command;
use tetrad_hal::platform::Platform;
use tetrad_hal::requests::ElementLed;
use tetrad_hal::zencoder::{TempoConfig, led_output};
use tetrad_settings::midi::MidiClockOut;
use tetrad_ui::{Screen, SettingsPage};

use super::actions::{self, stomp_slots};
use super::midi::ClockStatus;
use super::{Effect, State};

/// `SetTempoLedColor` index byte.
const TEMPO_LED_INDEX: u8 = 1;
/// `TempoSettings` parameter 2: the tempo LED switch (`tempoLedIsOff` migration).
const TEMPO_LED_PARAM: u16 = 2;

/// C: one bank overlay blink phase (`0x340bbc`, `0x340c60`: two 250 ms steps).
pub const BLINK_PHASE: Duration = Duration::from_millis(500);

/// UI state the LED view needs beyond [`State::screen`]; tetrad-ui reports it
/// ([`super::Action::UiLeds`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiLeds {
    /// `zenUI::Keyboard` is open (`0x4d76e0`).
    pub keyboard: bool,
    /// The keyboard confirm key accepts the text (`0x50ad88`).
    pub confirm_valid: bool,
    /// The modes overlay: the stock hybrid mode config dialog (ctl+0x2e78, `0x5acd30`).
    pub modes_overlay: bool,
    /// A block parameter editor is open (`ParameterEditor`, guard `0x5ad4c8`).
    pub editor: bool,
    /// Be Right Back: `SurfaceController` standby (`0x50ac14`) blacks every footswitch LED
    /// and stops the tempo blink.
    pub standby: bool,
}

/// The last LED state sent; `None` sends everything on the next pass (boot).
#[derive(Debug, Default)]
pub struct Leds {
    footswitches: Option<[Led; 11]>,
    tempo_colour: Option<ElementLed>,
    tempo_blink: Option<TempoConfig>,
    /// When the bank overlay opened. C: a bank step does not restart the blink
    /// (`0x3406e8`); I: a reopen does.
    overlay_since: Option<Duration>,
}

impl Leds {
    /// The HAL writes for one present pass: changed footswitch LEDs below
    /// `led_count(product)`, then the tempo LED colour and blink when they changed.
    pub fn update(&mut self, state: &State) -> Vec<Effect> {
        let product = product(state);
        let overlay = state.performance.footswitch.overlay.is_some();
        self.overlay_since = overlay.then(|| self.overlay_since.unwrap_or(state.now));
        // C: the blink starts on the 0.3 phase and loops forever (`0x340934`, `0x340cc4`).
        let blink_on = self.overlay_since.is_some_and(|since| {
            (state.now.saturating_sub(since).as_millis() / BLINK_PHASE.as_millis()) % 2 == 1
        });
        let mut effects = Vec::new();
        if let Some(frame) = footswitch_frame(state, product, blink_on) {
            let changed: Vec<(u8, Led)> = match &self.footswitches {
                Some(last) => leds::diff(last, &frame).collect(),
                None => (0..).zip(frame).collect(),
            };
            effects.extend(
                changed
                    .into_iter()
                    .filter(|&(index, _)| index < leds::led_count(product))
                    .map(|(index, led)| {
                        Effect::Hal(Command::ElementLed(element_led(
                            index,
                            led.rgb,
                            led.brightness,
                        )))
                    }),
            );
            self.footswitches = Some(frame);
        }
        let display = &state.settings.settings().display;
        let tempo = TempoLedInputs {
            // C: red only from the QC overlay entry (`0x3408d0`); mini writes +0x69 instead.
            bank_overlay: product == Product::QuadCortex && overlay,
            clock_out_enabled: state.settings.settings().midi.clock_out != MidiClockOut::Off,
            usb_clock_in: state.midi_clock.usb_running,
            din_clock_in: state.midi_clock.din_running,
        };
        let colour = element_led(
            TEMPO_LED_INDEX,
            leds::tempo_led_colour(tempo),
            display.led_brightness.get() as f32 / 100.0,
        );
        if self.tempo_colour.replace(colour) != Some(colour) {
            effects.push(Effect::Hal(Command::TempoLed(colour)));
        }
        let blink = tempo_blink(state, actions::tempo_bpm(state));
        if self.tempo_blink.replace(blink) != Some(blink) {
            effects.push(Effect::Hal(Command::Tempo(blink)));
        }
        effects
    }

    /// The next bank overlay blink phase edge while the overlay is open
    /// (`Timer::OverlayBlink`).
    pub fn blink_deadline(&self, now: Duration) -> Option<Duration> {
        let since = self.overlay_since?;
        let phases = now.saturating_sub(since).as_millis() / BLINK_PHASE.as_millis() + 1;
        Some(since + BLINK_PHASE * u32::try_from(phases).unwrap_or(u32::MAX))
    }
}

fn product(state: &State) -> Product {
    match state.platform {
        Platform::QcOriginal | Platform::QcEss => Product::QuadCortex,
        Platform::Atma => Product::Mini,
        Platform::AtmaEmulation => Product::MiniEmulation,
    }
}

/// The tempo LED blink at `bpm` (`0x513790`), or off.
fn tempo_blink(state: &State, bpm: i32) -> TempoConfig {
    if tempo_led_on(state) {
        TempoConfig::blink(u32::try_from(bpm).unwrap_or(0))
    } else {
        TempoConfig::OFF
    }
}

/// The tempo LED gates of `0x513790` and `0x513830` (enable `+0x89e6`, standby `0x6a8244`).
/// C: the mini overlay clears the tempo-LED enable mirror sched+0x69 (`0x3409a8`); closing
/// restores the setting (`0x54559c`). C: standby stops the blink (`0x50ac14`). I: a stored
/// 0.0 is off; a missing value is on (migration default).
fn tempo_led_on(state: &State) -> bool {
    let overlay = state.performance.footswitch.overlay.is_some();
    let off = state.ui_leds.standby || (overlay && product(state) != Product::QuadCortex);
    !off && (state.settings.settings().tempo.params)
        .get(&TEMPO_LED_PARAM)
        .is_none_or(|&value| value >= 0.5)
}

/// A MIDI clock beat: `SetTempo` again (`0x513790`) at the BPM getter `0x64c6e0` after the
/// tick (`clock`), so the zencoder blink starts its phase on the beat (I: the zencoder
/// restarts the blink on each write). Callers: every 24th in-range DIN clock (`0x32ff40`) and
/// each USB `0xF8` while no DIN clock runs (`0x89ece0` → `0x89ba68`). Nothing while the tempo
/// LED is off.
pub fn clock_beat(state: &State, clock: ClockStatus) -> Option<Effect> {
    let bpm = clock.bpm().unwrap_or_else(|| actions::scoped_bpm(state));
    tempo_led_on(state).then(|| Effect::Hal(Command::Tempo(tempo_blink(state, bpm))))
}

/// An out-of-range MIDI clock (`0x513830`): the TEMPO footswitch LED (9; mini 3) blue at the
/// LED brightness with fifth byte `0x20`. The footswitch frame paints over it at its next
/// change of that LED (I).
pub fn clock_flash(state: &State) -> Option<Effect> {
    let brightness = state.settings.settings().display.led_brightness.get() as f32 / 100.0;
    let index = if product(state) == Product::Mini {
        3
    } else {
        9
    };
    let led = ElementLed {
        opaque: 0x20,
        ..element_led(index, [0, 0, 0xff], brightness)
    };
    tempo_led_on(state).then_some(Effect::Hal(Command::ElementLed(led)))
}

/// C: `0x5b2048` steps 1..6, the guard `0x5ad4c8` and the dialog painters, first match.
/// The tetrad-ui screen mapping is I (W4 §4, §11).
fn view(state: &State) -> LedView {
    let ui = state.ui_leds;
    if ui.standby {
        return LedView::Black;
    }
    match state.screen {
        // C: the tuner paints its own frame (`0x3eeb30`). I: tetrad-ui has no mute-control
        // selection, so the mini third LED shows the mute colour.
        Screen::Tuner => LedView::Tuner {
            muted: state.tuner_muted,
            mute_selected: true,
        },
        _ if ui.keyboard && ui.confirm_valid => LedView::TextEntry,
        _ if ui.keyboard => LedView::Black,
        Screen::Settings(page) if page != SettingsPage::Brightness => LedView::AllOff,
        _ if state.performance.footswitch.overlay.is_some() => LedView::Normal,
        Screen::Settings(_)
        | Screen::Tempo
        | Screen::Io(_)
        | Screen::Presets
        | Screen::MediaPicker
        | Screen::Capture => LedView::Hold,
        _ if ui.modes_overlay || ui.editor => LedView::Hold,
        _ if state.performance.footswitch.scene_copy => LedView::Assign,
        Screen::Looper => LedView::LooperView {
            functions: state
                .settings
                .settings()
                .looper
                .stomp_assignments()
                .map(|id| {
                    LooperFunction::from_stock(id)
                        .expect("tetrad-settings keeps a permutation of 0..=7")
                }),
            // I: tile "Inactive" states are not recovered (`0x611d14`).
            inactive: [false; 8],
        },
        _ => LedView::Normal,
    }
}

/// The mini restart step (tetrad-watchdog `PowerStep::SurfaceLedsOff`): every surface LED of
/// the mini to black.
pub fn surface_off() -> impl Iterator<Item = Command> {
    (0..leds::led_count(Product::Mini))
        .map(|index| Command::ElementLed(element_led(index, [0; 3], 0.0)))
}

fn element_led(index: u8, rgb: [u8; 3], brightness: f32) -> ElementLed {
    let ([red, green, blue], opaque) = led_output(rgb, brightness);
    ElementLed {
        index,
        red,
        green,
        blue,
        opaque,
    }
}

fn footswitch_frame(state: &State, product: Product, blink_on: bool) -> Option<[Led; 11]> {
    let footswitch = &state.performance.footswitch;
    let mode = footswitch.mode;
    let switches = std::array::from_fn(|letter| {
        let switch = Footswitch::from_physical(HalStomp::new(u32::from(LOGICAL_TO_LED[letter]))?);
        let letter = letter as u8;
        // I: mini hybrid ownership is not recovered; `kind_in` is the QC split.
        Some(match switch.kind_in(mode)? {
            FootswitchKind::Preset => SwitchSlot::Preset(BankSlot::new(i32::from(letter)).ok()?),
            FootswitchKind::Scene => SwitchSlot::Scene(SceneIndex::new(i32::from(letter)).ok()?),
            FootswitchKind::Stomp => SwitchSlot::Stomp(StompIndex::new(u32::from(letter))?),
        })
    });
    let current = state.current.as_ref();
    let scene = current.map_or(0, |current| current.deployment.scene().index());
    let mut scene_colours = [[0; 3]; 8];
    for (colour, &raw) in scene_colours
        .iter_mut()
        .zip(current.map_or(&[][..], |current| &current.preset.scene_colors))
    {
        // 0xAARRGGBB (`DEFAULT_SCENE_COLORS`).
        let [_, red, green, blue] = raw.to_be_bytes();
        *colour = [red, green, blue];
    }
    // Blocks per logical stomp, grouped by `stomp_mode_assignments`.
    let mut blocks: [Vec<LedBlock>; STOMPS as usize] = Default::default();
    let mut engaged = [false; STOMPS as usize];
    if let Some(current) = current {
        let layout = current.deployment.layout();
        let at = current.deployment.scene();
        for (stomp, (blocks, engaged)) in (0..).zip(blocks.iter_mut().zip(&mut engaged)) {
            for slot in stomp_slots(&current.preset, stomp) {
                let Some(block) = layout.instance(Site::Slot(slot)) else {
                    continue;
                };
                // `0x513144`: the stomp's first assignment (`0x2e2c44` order) gives the LED
                // state, its block's bypass. Not modelled: a SECONDARY role (`0x2ed564`),
                // model 26001 (`0x55af78`) and Looper X 27000 (always on).
                if blocks.is_empty() {
                    *engaged = !block.bypass.at(at);
                }
                blocks.push(LedBlock {
                    // C: catalog `category_id` is ModelRepo `<Category id>` = block word [6]
                    // (`0x2eb38c`). Word [0x38] is the capture `deviceType` for category
                    // 0x0e (`0x2eb47c`); Tetrad has no capture device type yet (W4 §1).
                    category: state
                        .specs
                        .as_ref()
                        .and_then(|specs| specs.model(block.model.0))
                        .map_or(u32::MAX, |spec| spec.category_id),
                    sub_type: 0,
                    model_id: block.model.0,
                });
            }
        }
    }
    let stomps = std::array::from_fn(|stomp| StompLed {
        blocks: &blocks[stomp],
        engaged: engaged[stomp],
    });
    let display = &state.settings.settings().display;
    leds::frame(&LedInputs {
        product,
        mode,
        switches,
        current_preset: footswitch.current_preset,
        plugin_preset_folder: false,
        current_scene: SceneIndex::new(scene as i32).expect("engine scenes are 0..=7"),
        scene_colours,
        stomps,
        overlay: footswitch.overlay,
        blink_on,
        view: view(state),
        settings: LedSettings {
            led_brightness: display.led_brightness.get(),
            dimmed_led_brightness: display.dimmed_led_brightness.get(),
            dimmed_preset: display.dimmed_preset,
            dimmed_scene: display.dimmed_scene,
            dimmed_stomp: display.dimmed_stomp,
        },
    })
}
