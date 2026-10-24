//! Pure stock footswitch LED policy. VAs refer to ZenUI; C = confirmed, I = inferred. The caller
//! supplies mode ownership, effective stomp state and the [`LedView`]; no HAL or timers live here.

use super::footswitch::{BankOverlay, LOGICAL_TO_PHYSICAL};
use super::mode::{FootswitchKind, FootswitchMode};
use super::scene::SCENES;
use super::stomp::{STOMPS, StompIndex};
use crate::workflow::{BankSlot, SceneIndex};

/// C: logical A..H, DOWN, TEMPO, UP → hardware LEDs (`0xa1be88`).
pub const LOGICAL_TO_LED: [u8; 11] = LOGICAL_TO_PHYSICAL;

/// C: eight stock QRgb values (`0xa1bf00`).
pub const PALETTE: [[u8; 3]; 8] = [
    [0x45, 0xf8, 0x62],
    [0x0a, 0x74, 0xe0],
    [0xff, 0x70, 0x00],
    [0xff, 0x02, 0xc2],
    [0x87, 0xda, 0xff],
    [0x69, 0x54, 0xff],
    [0xff, 0xd2, 0x36],
    [0xff, 0x27, 0x27],
];

const WHITE: [u8; 3] = [255; 3];
const RED: [u8; 3] = [255, 0, 0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    QuadCortex,
    Mini,
    /// Stock platform 3: mini policy displayed on QC hardware (`0x50b548`).
    MiniEmulation,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Led {
    pub rgb: [u8; 3],
    /// Resolved intensity; never the stock −1 sentinel.
    pub brightness: f32,
}

impl Led {
    pub const OFF: Self = Self {
        rgb: [0; 3],
        brightness: 0.0,
    };
}

/// Percent values are caller-validated settings in 0..=100 (`0x409228`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedSettings {
    pub led_brightness: i32,
    pub dimmed_led_brightness: i32,
    pub dimmed_preset: bool,
    pub dimmed_scene: bool,
    pub dimmed_stomp: bool,
}

/// One logical A..H owner and its mode-local slot (`0x58b830`, `0x58b098`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchSlot {
    Preset(BankSlot),
    Scene(SceneIndex),
    Stomp(StompIndex),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedBlock {
    /// Stock block word `[6]`.
    pub category: u32,
    /// Stock block word `[0x38]`, used by category 0x0e.
    pub sub_type: u32,
    pub model_id: u32,
}

/// Blocks grouped by existing `StompAssignment::stomp` (`0x2e2c44`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StompLed<'a> {
    pub blocks: &'a [LedBlock],
    /// Effective switch state from the caller; the engaged name is I (`0x513144`).
    pub engaged: bool,
}

/// C: looper footswitch functions (`0x379cac`); stock ids 0..=7 in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperFunction {
    Record,
    Play,
    Undo,
    Duplicate,
    OneShot,
    HalfSpeed,
    PunchIn,
    Reverse,
}

impl LooperFunction {
    pub const ALL: [Self; 8] = [
        Self::Record,
        Self::Play,
        Self::Undo,
        Self::Duplicate,
        Self::OneShot,
        Self::HalfSpeed,
        Self::PunchIn,
        Self::Reverse,
    ];

    /// `LooperSettings/stompAssignments*` value; None outside 0..=7.
    pub fn from_stock(id: i32) -> Option<Self> {
        usize::try_from(id)
            .ok()
            .and_then(|id| Self::ALL.get(id).copied())
    }

    /// C: `#FF0000` for mask 0x49 (`0x37a85c`) and for table `0x11e6b90` values
    /// {0, 3, 4} (`0x613c1c`); both select RECORD, DUPLICATE and PUNCH IN.
    fn rgb(self) -> [u8; 3] {
        match self {
            Self::Record | Self::Duplicate | Self::PunchIn => RED,
            _ => WHITE,
        }
    }
}

/// What owns the footswitch LEDs in one present pass (`0x5b2048` and the dialog painters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedView {
    /// C: preset, scene, stomp painters (`0x5b21e4`), then the bank overlay (`0x3406e8`).
    Normal,
    /// C: keep the last frame: a guard view (`0x5ad4c8`) or a held dialog (`0x5b2048`).
    Hold,
    /// C: Settings off the Brightness page, or the factory test dialog (`0x232c70`).
    AllOff,
    /// C: assign mode (`0x5adac8`): logical A..H white.
    Assign,
    /// C: an all-black LED layer (`0x50a82c`): keyboard, model menu, standby.
    Black,
    /// C: text entry (`0x50ac64(1)`, `0x50ad88(1)`): black, confirm LED white
    /// (hardware 10 on QC, 1 on mini).
    TextEntry,
    /// C: `zenUI::TunerDialog` painter (`0x3eeb30`). `mute_selected` is the mini
    /// "+0x1bc is the selected control" test (`0x4920d0`).
    Tuner { muted: bool, mute_selected: bool },
    /// C: live looper view (`0x613c1c` via `0x4f310c`), slot i on logical i.
    /// `inactive` slots are off; the tile states are open (`0x611d14`).
    LooperView {
        functions: [LooperFunction; 8],
        inactive: [bool; 8],
    },
    /// C: `zenUI::LooperSwapActionsDialog` (`0x37a600`); `assigning` is its
    /// "Assign footswitch" state (+0x144).
    SwapActions {
        functions: [LooperFunction; 8],
        assigning: bool,
    },
    /// (O) A Custom footswitch mode: slot i (logical A..=H, DOWN, TEMPO) in its colour, at
    /// the LED brightness when engaged and at the dimmed brightness when not; `None` is an
    /// empty slot (off). UP, the mode switch, is dimmed white.
    Custom { slots: [Option<CustomLed>; 10] },
}

/// (O) One assigned slot of [`LedView::Custom`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomLed {
    pub rgb: [u8; 3],
    pub engaged: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct LedInputs<'a> {
    pub product: Product,
    pub mode: FootswitchMode,
    /// Logical A..H; None is unowned. Supply mini and hybrid ownership explicitly.
    pub switches: [Option<SwitchSlot>; 8],
    /// Absolute loaded preset. Stock reads the mark from `0x7c28cc` and the palette
    /// group from `0x7c2908`; they are equal in normal use (I).
    pub current_preset: u32,
    /// C: `0x7c2974`, the current folder is a Downloads `CloudProductFolder` (+0x40 == 1)
    /// or a `LocalPluginPresetFolder`. The preset painter then blanks logical 0..8 (`0x5ad778`).
    pub plugin_preset_folder: bool,
    pub current_scene: SceneIndex,
    pub scene_colours: [[u8; 3]; SCENES],
    /// Indexed by the existing logical `StompIndex`, including non-lettered slots.
    pub stomps: [StompLed<'a>; STOMPS as usize],
    /// Painted only in [`LedView::Normal`] (`0x5b2048` step 4).
    pub overlay: Option<BankOverlay>,
    /// Odd animation loop = true (1.0); even = false (0.3), `0x3406e8`.
    pub blink_on: bool,
    pub view: LedView,
    pub settings: LedSettings,
}

/// C: worker `0x508a38` writes four LEDs on platform 2, eleven otherwise.
pub const fn led_count(product: Product) -> u8 {
    match product {
        Product::Mini => 4,
        Product::QuadCortex | Product::MiniEmulation => 11,
    }
}

/// C: `0x49d05c`, g = 8/k. Only preset-owning modes reach it, so its `0x589d98`
/// test is always true: single QC is identity, single mini and hybrid QC use g = 4,
/// hybrid mini uses g = 2. MiniEmulation counts as mini (`0x49a754`).
fn preset_palette_index(
    product: Product,
    mode: FootswitchMode,
    current_preset: u32,
    slot: BankSlot,
) -> u8 {
    let g = match (product == Product::QuadCortex, mode.is_hybrid()) {
        (true, false) => return slot.get(),
        (false, true) => 2,
        _ => 4,
    };
    ((current_preset / g * g + u32::from(slot.get()) % g) & 7) as u8
}

/// C: ARM jump tables at `0x2eb3b8` / `0x2eb490`, not Ghidra's case ordering.
pub fn block_colour(block: &LedBlock) -> [u8; 3] {
    match (block.category, block.model_id, block.sub_type) {
        (0x1a, 0x6591, _) => PALETTE[4],          // 0x2eb440 → 0x2eb4e4
        (0x1a, 0x6593..=0x6594, _) => PALETTE[2], // 0x2eb454 → 0x2eb464
        (3, 0x0bc0, _) => WHITE,                  // 0x2eb4b0 → 0x2eb4bc
        (0x0e, _, 1 | 2) => PALETTE[7],           // 0x2eb47c / 0x2eb490 → 0x2eb4f4
        (0x0e, _, 3 | 4) => PALETTE[5],           // 0x2eb498 → 0x2eb55c
        (0x0e, _, 5 | 6 | 8) => PALETTE[2],       // 0x2eb4a0 / 0x2eb4ac → 0x2eb524
        (0x0e, _, 7) => PALETTE[0],               // 0x2eb4a8 → 0x2eb510
        (0 | 3, _, _) => PALETTE[2],              // 0x2eb524
        (1 | 2 | 0x1b, _, _) => PALETTE[7],       // 0x2eb4f4
        (4 | 0x0a, _, _) => PALETTE[1],           // 0x2eb538
        (5 | 0x0f, _, _) => PALETTE[0],           // 0x2eb510
        (6 | 8, _, _) => [0x00, 0xff, 0xdd],      // 0x2eb54c
        (7, _, _) => [0x35, 0x00, 0xf1],          // 0x2eb584
        (9 | 0x0d | 0x0e | 0x10 | 0x1a, _, _) => [0x95; 3], // 0x2eb4c8
        (0x0b, _, _) => [0xff, 0x07, 0x56],       // 0x2eb5c0, literal at 0xa27784
        (0x0c | 0x15 | 0x1d | 0x20 | 0x21, _, _) => PALETTE[5], // 0x2eb55c
        (0x12, _, _) => PALETTE[6],               // 0x2eb598
        (0x18, _, _) => PALETTE[4],               // 0x2eb5ac
        (0x1e, _, _) => [0xe4, 0x4a, 0x5d],       // 0x2eb570
        _ => WHITE,                               // 0x2eb610 → 0x2eb4bc, stock default
    }
}

/// Build one hardware-order frame. None holds the last frame.
pub fn frame(inputs: &LedInputs<'_>) -> Option<[Led; 11]> {
    let mini = inputs.product != Product::QuadCortex;
    // C: every painter here passes brightness −1, not dimmed (`0x509e40`).
    let normal = |rgb| resolve(rgb, -1.0, false, &inputs.settings);
    // C: normal painters and layers start from all off (`0x232714`, `0x50a82c`).
    let mut output = [Led::OFF; 11];
    match inputs.view {
        LedView::Hold => return None,
        LedView::AllOff | LedView::Black => {}
        LedView::Normal => {
            paint_modes(inputs, &mut output);
            if let Some(overlay) = inputs.overlay {
                paint_overlay(inputs, overlay, &mut output);
            }
        }
        LedView::Assign => {
            // C: `0x5adac8` → `0x232a58`.
            for &hardware in &LOGICAL_TO_LED[..8] {
                output[usize::from(hardware)] = normal(WHITE);
            }
        }
        LedView::TextEntry => output[if mini { 1 } else { 10 }] = normal(WHITE),
        LedView::Tuner {
            muted,
            mute_selected,
        } => {
            let mute = if muted { PALETTE[7] } else { WHITE };
            if mini {
                // C: `0x3eeb4c`: 0, 1 black; 2 white; 3 mute colour only when selected.
                output[2] = normal(WHITE);
                output[3] = normal(if mute_selected { mute } else { PALETTE[0] });
            } else {
                // C: `0x3eec14`–`0x3eedb4`: 0, 1, 3 green; 2 mute colour; 4..=10 black.
                for hardware in [0, 1, 3] {
                    output[hardware] = normal(PALETTE[0]);
                }
                output[2] = normal(mute);
            }
        }
        // I: the mini branch of `0x4f310c` paints the selected control only; not recovered.
        LedView::LooperView { .. } if mini => return None,
        LedView::LooperView {
            functions,
            inactive,
        } => {
            // C: `0x613c1c` lights hardware {0..3, 5..8}, which is logical A..H.
            for (logical, (function, inactive)) in functions.into_iter().zip(inactive).enumerate() {
                if !inactive {
                    output[usize::from(LOGICAL_TO_LED[logical])] = normal(function.rgb());
                }
            }
        }
        LedView::Custom { .. } if mini => return None,
        LedView::Custom { slots } => {
            for (logical, slot) in slots.into_iter().enumerate() {
                if let Some(slot) = slot {
                    output[usize::from(LOGICAL_TO_LED[logical])] =
                        resolve(slot.rgb, -1.0, !slot.engaged, &inputs.settings);
                }
            }
            output[usize::from(LOGICAL_TO_LED[10])] = resolve(WHITE, -1.0, true, &inputs.settings);
        }
        LedView::SwapActions {
            functions,
            assigning,
        } => {
            // C: `0x37a600` maps logical A..H through `0xa30b78` (= `LOGICAL_TO_LED`).
            for (logical, function) in functions.into_iter().enumerate() {
                let rgb = if assigning { WHITE } else { function.rgb() };
                output[usize::from(LOGICAL_TO_LED[logical])] = normal(rgb);
            }
        }
    }
    if inputs.product == Product::MiniEmulation {
        // C: `0x50b548` copies 2→5 and 3→6, then keeps {0,1,5,6} (`0xa4dfa0`).
        output[5] = output[2];
        output[6] = output[3];
        for (index, led) in output.iter_mut().enumerate() {
            if !matches!(index, 0 | 1 | 5 | 6) {
                *led = Led::OFF;
            }
        }
    }
    Some(output)
}

fn resolve(rgb: [u8; 3], brightness: f32, dimmed: bool, settings: &LedSettings) -> Led {
    // C: `0x509e40`; RGB black forces zero even for explicit brightness.
    let brightness = if rgb == [0; 3] {
        0.0
    } else if brightness == -1.0 {
        let percent = if dimmed {
            settings.dimmed_led_brightness
        } else {
            settings.led_brightness
        };
        percent as f32 / 100.0
    } else {
        brightness
    };
    Led { rgb, brightness }
}

fn paint_modes(inputs: &LedInputs<'_>, output: &mut [Led; 11]) {
    for (logical, assignment) in inputs.switches.iter().enumerate() {
        let Some(assignment) = assignment else {
            continue;
        };
        let kind = match assignment {
            SwitchSlot::Preset(_) => FootswitchKind::Preset,
            SwitchSlot::Scene(_) => FootswitchKind::Scene,
            SwitchSlot::Stomp(_) => FootswitchKind::Stomp,
        };
        if !inputs.mode.includes(kind) {
            continue;
        }
        let (rgb, active, dimmed_enabled) = match *assignment {
            SwitchSlot::Preset(slot) => {
                // C: `0x5ad738` → `0x232820` blanks logical 0..8. The preset painter
                // runs before scene and stomp (`0x5b21e4`), so they paint over it.
                if inputs.plugin_preset_folder {
                    continue;
                }
                let index =
                    preset_palette_index(inputs.product, inputs.mode, inputs.current_preset, slot);
                // C: `0x232cb8` compares the mapped palette slot, not the switch.
                (
                    PALETTE[usize::from(index)],
                    index == (inputs.current_preset & 7) as u8,
                    inputs.settings.dimmed_preset,
                )
            }
            SwitchSlot::Scene(scene) => (
                // C: `0x5ad7e8` → `0x232cb8`, scene RGB from `0x2e1830`.
                inputs.scene_colours[usize::from(scene.get())],
                scene == inputs.current_scene,
                inputs.settings.dimmed_scene,
            ),
            SwitchSlot::Stomp(stomp) => {
                // C: `0x5b15e4` → `0x232ef4`: empty off, one block colour, group white.
                let stomp = inputs.stomps[usize::from(stomp.get())];
                let rgb = match stomp.blocks {
                    [] => [0; 3],
                    [block] => block_colour(block),
                    _ => WHITE,
                };
                (rgb, stomp.engaged, inputs.settings.dimmed_stomp)
            }
        };
        // C: `0x232cb8` / `0x232ef4`, dim settings selected by `0x408d20`.
        let brightness = if active || dimmed_enabled { -1.0 } else { 0.0 };
        output[usize::from(LOGICAL_TO_LED[logical])] =
            resolve(rgb, brightness, !active, &inputs.settings);
    }
}

fn paint_overlay(inputs: &LedInputs<'_>, overlay: BankOverlay, output: &mut [Led; 11]) {
    // C: `0x3406e8` clears 0..8 before target/bank LEDs. Normal auxiliary clearing
    // happens before this painter, so the bank lights survive (I: frame composition).
    output.fill(Led::OFF);
    if let Some(target) = overlay.highlighted {
        // I: stock indexes the palette with the raw slot (`0x232930`); callers pass 0..7.
        let index = target & 7;
        // C: `0x49cfac`: a hybrid preset half has half the switches; it starts at the
        // half's first switch when presets are the second kind (`0x58babc` == 2).
        let full = if inputs.product == Product::QuadCortex {
            8
        } else {
            4
        };
        let mode = inputs.mode;
        let width = if mode.is_hybrid() && mode.includes(FootswitchKind::Preset) {
            full / 2
        } else {
            full
        };
        let offset = if mode.kinds().1 == Some(FootswitchKind::Preset) {
            full / 2
        } else {
            0
        };
        let logical = usize::from(index % width + offset);
        let brightness = if inputs.blink_on { 1.0 } else { 0.3 };
        output[usize::from(LOGICAL_TO_LED[logical])] = resolve(
            PALETTE[usize::from(index)],
            brightness,
            false,
            &inputs.settings,
        );
    }
    // C: `0x232c40` takes hardware indices 10 and 4, not logical ones; colour is
    // `QColor::setRgb(0, 255, 0, 255)` (`0x340774`, `0x3408d4`).
    for hardware in [4, 10] {
        output[hardware] = resolve([0, 255, 0], -1.0, false, &inputs.settings);
    }
}

/// C: compare RGB and resolved brightness once per pass (`0x50b548`).
pub fn diff<'a>(
    previous: &'a [Led; 11],
    next: &'a [Led; 11],
) -> impl Iterator<Item = (u8, Led)> + 'a {
    previous
        .iter()
        .zip(next)
        .enumerate()
        .filter_map(|(index, (previous, next))| (previous != next).then_some((index as u8, *next)))
}

/// MidiController (`0x3312c8`) clock flags and the overlay state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TempoLedInputs {
    pub bank_overlay: bool,
    /// C: `ClockOutEn` (+0x4c) is not Off (`0x32e7e4`, `0x32fab0`).
    pub clock_out_enabled: bool,
    /// C: USB clock-in running (+0xfd, `0x32f52c`).
    pub usb_clock_in: bool,
    /// C: DIN clock-in running (+0xfe, `0x32f8dc`).
    pub din_clock_in: bool,
}

/// RGB only; caller applies normal LED brightness and configures hardware blink.
/// Blue follows an external clock, red sends clock, green is internal.
pub fn tempo_led_colour(inputs: TempoLedInputs) -> [u8; 3] {
    if inputs.bank_overlay {
        // C: overlay entry `0x3408a4` calls red `0x669320` on QC (`0x3408d0`); settings
        // re-apply it while overlay flag +0x140 is set (`0x409c94`, `0x409d60`).
        RED
    } else if inputs.usb_clock_in || inputs.din_clock_in {
        // C: +0xfd (`0x513930`) and +0xfe (`0x51394c`) both reach blue `0x6693e0`.
        [0, 0, 255]
    } else if inputs.clock_out_enabled {
        // C: `0x51395c` → `0x6694c0`.
        PALETTE[7]
    } else {
        // C: `0x513970` → `0x66925c`.
        PALETTE[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> LedInputs<'static> {
        LedInputs {
            product: Product::QuadCortex,
            mode: FootswitchMode::Preset,
            switches: std::array::from_fn(|slot| {
                Some(SwitchSlot::Preset(BankSlot::new(slot as i32).unwrap()))
            }),
            current_preset: 2,
            plugin_preset_folder: false,
            current_scene: SceneIndex::new(0).unwrap(),
            scene_colours: PALETTE,
            stomps: [StompLed::default(); STOMPS as usize],
            overlay: None,
            blink_on: false,
            view: LedView::Normal,
            settings: LedSettings {
                led_brightness: 27,
                dimmed_led_brightness: 6,
                dimmed_preset: true,
                dimmed_scene: true,
                dimmed_stomp: true,
            },
        }
    }

    #[test]
    fn preset_painter_maps_palette_and_current_brightness_to_hardware() {
        let frame = frame(&inputs()).unwrap();
        for (slot, rgb) in PALETTE.into_iter().enumerate() {
            assert_eq!(
                frame[usize::from(LOGICAL_TO_LED[slot])],
                Led {
                    rgb,
                    brightness: if slot == 2 { 0.27 } else { 0.06 }
                }
            );
        }
        for hardware in [4, 9, 10] {
            assert_eq!(frame[hardware], Led::OFF);
        }
    }

    #[test]
    fn plugin_folder_preset_painter_blanks_before_later_owners() {
        let mut inputs = inputs();
        inputs.plugin_preset_folder = true;
        assert_eq!(frame(&inputs), Some([Led::OFF; 11]));
        // Scene paints after the preset blank (`0x5b21e4`).
        inputs.mode = FootswitchMode::PresetScene;
        inputs.switches[4] = Some(SwitchSlot::Scene(SceneIndex::new(0).unwrap()));
        let output = frame(&inputs).unwrap();
        assert_eq!(output[0], Led::OFF);
        assert_eq!(output[5].rgb, inputs.scene_colours[0]);
    }

    #[test]
    fn scene_painter_uses_scene_slots_and_composes_hybrid_owners() {
        let mut inputs = inputs();
        inputs.mode = FootswitchMode::PresetScene;
        inputs.current_scene = SceneIndex::new(1).unwrap();
        inputs.scene_colours[1] = [12, 34, 56];
        for logical in 4..8 {
            inputs.switches[logical] = Some(SwitchSlot::Scene(
                SceneIndex::new(logical as i32 - 4).unwrap(),
            ));
        }
        let output = frame(&inputs).unwrap();
        assert_eq!(
            output[2],
            Led {
                rgb: PALETTE[2],
                brightness: 0.27
            }
        );
        for logical in 4..8 {
            let slot = logical - 4;
            assert_eq!(
                output[usize::from(LOGICAL_TO_LED[logical])],
                Led {
                    rgb: inputs.scene_colours[slot],
                    brightness: if slot == 1 { 0.27 } else { 0.06 },
                }
            );
        }
    }

    #[test]
    fn stomp_painter_handles_empty_single_group_and_effective_state() {
        let mut inputs = inputs();
        inputs.mode = FootswitchMode::Stomp;
        inputs.switches = std::array::from_fn(|slot| {
            Some(SwitchSlot::Stomp(StompIndex::new(slot as u32).unwrap()))
        });
        let blocks = [
            LedBlock {
                category: 3,
                sub_type: 0,
                model_id: 100,
            },
            LedBlock {
                category: 4,
                sub_type: 0,
                model_id: 200,
            },
        ];
        inputs.stomps[1] = StompLed {
            blocks: &blocks[..1],
            engaged: true,
        };
        inputs.stomps[2] = StompLed {
            blocks: &blocks,
            engaged: true,
        };
        inputs.stomps[4] = StompLed {
            blocks: &blocks[1..],
            engaged: false,
        };
        let output = frame(&inputs).unwrap();
        assert_eq!(output[0], Led::OFF);
        assert_eq!(
            output[1],
            Led {
                rgb: PALETTE[2],
                brightness: 0.27
            }
        );
        assert_eq!(
            output[2],
            Led {
                rgb: WHITE,
                brightness: 0.27
            }
        );
        assert_eq!(
            output[5],
            Led {
                rgb: PALETTE[1],
                brightness: 0.06
            }
        );
    }

    #[test]
    fn assign_painter_lights_all_letters_white_at_normal_brightness() {
        let mut inputs = inputs();
        inputs.view = LedView::Assign;
        inputs.switches = [None; 8];
        let output = frame(&inputs).unwrap();
        for &hardware in &LOGICAL_TO_LED[..8] {
            assert_eq!(
                output[usize::from(hardware)],
                Led {
                    rgb: WHITE,
                    brightness: 0.27
                }
            );
        }
        assert_eq!([output[4], output[9], output[10]], [Led::OFF; 3]);
    }

    #[test]
    fn layers_and_all_off_ignore_the_overlay_and_hold_keeps_the_frame() {
        let mut inputs = inputs();
        inputs.overlay = Some(BankOverlay {
            bank: 3,
            highlighted: Some(2),
        });
        for view in [LedView::AllOff, LedView::Black] {
            inputs.view = view;
            assert_eq!(frame(&inputs), Some([Led::OFF; 11]));
        }
        inputs.view = LedView::Hold;
        assert!(frame(&inputs).is_none());
    }

    #[test]
    fn bank_overlay_painter_blinks_target_and_keeps_green_bank_leds() {
        let mut inputs = inputs();
        inputs.mode = FootswitchMode::ScenePreset;
        inputs.overlay = Some(BankOverlay {
            bank: 3,
            highlighted: Some(6),
        });
        for (blink, brightness) in [(false, 0.3), (true, 1.0)] {
            inputs.blink_on = blink;
            let output = frame(&inputs).unwrap();
            assert_eq!(
                output[7],
                Led {
                    rgb: PALETTE[6],
                    brightness
                }
            );
            for (hardware, led) in output.into_iter().enumerate() {
                match hardware {
                    4 | 10 => assert_eq!(
                        led,
                        Led {
                            rgb: [0, 255, 0],
                            brightness: 0.27
                        }
                    ),
                    7 => {}
                    _ => assert_eq!(led, Led::OFF),
                }
            }
        }
        inputs.product = Product::Mini;
        inputs.overlay.as_mut().unwrap().highlighted = Some(7);
        assert_eq!(
            frame(&inputs).unwrap()[3],
            Led {
                rgb: PALETTE[7],
                brightness: 1.0
            }
        );
        // Hybrid mode without presets: the whole board is one group.
        inputs.product = Product::QuadCortex;
        inputs.mode = FootswitchMode::SceneStomp;
        inputs.overlay.as_mut().unwrap().highlighted = Some(5);
        assert_eq!(frame(&inputs).unwrap()[6].rgb, PALETTE[5]);
    }

    #[test]
    fn looper_view_and_swap_dialog_paint_red_record_duplicate_punch_in() {
        use LooperFunction::*;
        let functions = [
            Duplicate, OneShot, HalfSpeed, PunchIn, Record, Play, Reverse, Undo,
        ];
        let red = [true, false, false, true, true, false, false, false];
        let lit = |output: [Led; 11], logical: usize| output[usize::from(LOGICAL_TO_LED[logical])];
        let mut inputs = inputs();
        let mut inactive = [false; 8];
        inactive[1] = true;
        inputs.view = LedView::LooperView {
            functions,
            inactive,
        };
        let live = frame(&inputs).unwrap();
        inputs.view = LedView::SwapActions {
            functions,
            assigning: false,
        };
        let swap = frame(&inputs).unwrap();
        for (logical, red) in red.into_iter().enumerate() {
            let rgb = if red { RED } else { WHITE };
            assert_eq!(
                lit(swap, logical),
                Led {
                    rgb,
                    brightness: 0.27
                }
            );
            let expected = if logical == 1 {
                Led::OFF
            } else {
                lit(swap, logical)
            };
            assert_eq!(lit(live, logical), expected);
        }
        for output in [live, swap] {
            assert_eq!([output[4], output[9], output[10]], [Led::OFF; 3]);
        }
        // "Assign footswitch" lights every letter white (`0x37a600`, +0x144).
        inputs.view = LedView::SwapActions {
            functions,
            assigning: true,
        };
        let output = frame(&inputs).unwrap();
        assert!((0..8).all(|logical| lit(output, logical).rgb == WHITE));
        assert_eq!(LooperFunction::from_stock(6), Some(PunchIn));
        assert_eq!(LooperFunction::from_stock(8), None);
    }

    /// Custom mode lights DOWN and TEMPO as slots: engaged at the LED brightness, others
    /// dimmed, empty slots off, UP dimmed white.
    #[test]
    fn custom_view_paints_ten_slots_and_the_mode_switch() {
        let mut inputs = inputs();
        let mut slots = [None; 10];
        slots[0] = Some(CustomLed {
            rgb: PALETTE[1],
            engaged: true,
        });
        slots[8] = Some(CustomLed {
            rgb: PALETTE[2],
            engaged: false,
        });
        slots[9] = Some(CustomLed {
            rgb: PALETTE[3],
            engaged: true,
        });
        inputs.view = LedView::Custom { slots };
        let output = frame(&inputs).unwrap();
        let led = |rgb, brightness| Led { rgb, brightness };
        assert_eq!(output[0], led(PALETTE[1], 0.27));
        assert_eq!(output[4], led(PALETTE[2], 0.06), "DOWN is hardware 4");
        assert_eq!(output[9], led(PALETTE[3], 0.27), "TEMPO is hardware 9");
        assert_eq!(output[10], led(WHITE, 0.06));
        assert!([1, 2, 3, 5, 6, 7, 8].iter().all(|&h| output[h] == Led::OFF));
    }

    #[test]
    fn tuner_painter_qc_and_mini_branches() {
        let green = Led {
            rgb: PALETTE[0],
            brightness: 0.27,
        };
        let white = Led {
            rgb: WHITE,
            brightness: 0.27,
        };
        let red = Led {
            rgb: PALETTE[7],
            brightness: 0.27,
        };
        let mut inputs = inputs();
        for (muted, mute) in [(false, white), (true, red)] {
            inputs.product = Product::QuadCortex;
            inputs.view = LedView::Tuner {
                muted,
                mute_selected: false,
            };
            let mut expected = [Led::OFF; 11];
            expected[..4].copy_from_slice(&[green, green, mute, green]);
            assert_eq!(frame(&inputs), Some(expected));
            inputs.product = Product::Mini;
            for (selected, third) in [(false, green), (true, mute)] {
                inputs.view = LedView::Tuner {
                    muted,
                    mute_selected: selected,
                };
                let mut expected = [Led::OFF; 11];
                expected[2..4].copy_from_slice(&[white, third]);
                assert_eq!(frame(&inputs), Some(expected));
            }
        }
    }

    #[test]
    fn text_entry_lights_only_the_confirm_led() {
        let mut inputs = inputs();
        inputs.view = LedView::TextEntry;
        for (product, confirm) in [(Product::QuadCortex, 10), (Product::Mini, 1)] {
            inputs.product = product;
            let mut expected = [Led::OFF; 11];
            expected[confirm] = Led {
                rgb: WHITE,
                brightness: 0.27,
            };
            assert_eq!(frame(&inputs), Some(expected));
        }
    }

    #[test]
    fn preset_palette_formula_covers_identity_and_each_k_case() {
        use FootswitchMode::{Preset, StompPreset};
        use Product::{Mini, MiniEmulation, QuadCortex};
        for (product, mode, expected) in [
            (QuadCortex, Preset, [0, 1, 2, 3, 4, 5, 6, 7]),
            (Mini, Preset, [4, 5, 6, 7, 4, 5, 6, 7]),
            (QuadCortex, StompPreset, [4, 5, 6, 7, 4, 5, 6, 7]),
            (Mini, StompPreset, [6, 7, 6, 7, 6, 7, 6, 7]),
            (MiniEmulation, StompPreset, [6, 7, 6, 7, 6, 7, 6, 7]),
        ] {
            for (slot, expected) in expected.into_iter().enumerate() {
                assert_eq!(
                    preset_palette_index(product, mode, 14, BankSlot::new(slot as i32).unwrap()),
                    expected,
                    "{product:?} {mode:?} slot {slot}"
                );
            }
        }
    }

    #[test]
    fn mini_emulation_copies_full_led_values_then_blacks_other_hardware() {
        let mut inputs = inputs();
        inputs.product = Product::Mini;
        inputs.current_preset = 7;
        inputs.switches[4..].fill(None);
        let mini = frame(&inputs).unwrap();
        inputs.product = Product::MiniEmulation;
        let output = frame(&inputs).unwrap();
        assert_eq!(
            [output[0], output[1], output[5], output[6]],
            [mini[0], mini[1], mini[2], mini[3]]
        );
        for hardware in [2, 3, 4, 7, 8, 9, 10] {
            assert_eq!(output[hardware], Led::OFF);
        }
    }

    #[test]
    fn dimmed_and_off_preserve_colour_and_black_always_has_zero_brightness() {
        let mut inputs = inputs();
        let block = [LedBlock {
            category: 0,
            sub_type: 0,
            model_id: 1,
        }];
        for (mode, dimmed) in [
            (FootswitchMode::Preset, FootswitchKind::Preset),
            (FootswitchMode::Scene, FootswitchKind::Scene),
            (FootswitchMode::Stomp, FootswitchKind::Stomp),
        ] {
            inputs.mode = mode;
            inputs.stomps[0] = StompLed {
                blocks: &block,
                engaged: false,
            };
            inputs.switches = [None; 8];
            inputs.switches[0] = Some(match dimmed {
                FootswitchKind::Preset => SwitchSlot::Preset(BankSlot::new(0).unwrap()),
                FootswitchKind::Scene => SwitchSlot::Scene(SceneIndex::new(1).unwrap()),
                FootswitchKind::Stomp => SwitchSlot::Stomp(StompIndex::new(0).unwrap()),
            });
            let output = frame(&inputs).unwrap()[0];
            assert_eq!(output.brightness, 0.06);
            inputs.settings.dimmed_preset = false;
            inputs.settings.dimmed_scene = false;
            inputs.settings.dimmed_stomp = false;
            assert_eq!(
                frame(&inputs).unwrap()[0],
                Led {
                    brightness: 0.0,
                    ..output
                }
            );
            inputs.settings.dimmed_preset = true;
            inputs.settings.dimmed_scene = true;
            inputs.settings.dimmed_stomp = true;
        }
        assert_eq!(resolve([0; 3], -1.0, true, &inputs.settings), Led::OFF);
        assert_eq!(resolve([0; 3], 1.0, false, &inputs.settings), Led::OFF);
    }

    #[test]
    fn block_colours_cover_the_arm_jump_table_subtypes_and_model_overrides() {
        let expected = [
            PALETTE[2],
            PALETTE[7],
            PALETTE[7],
            PALETTE[2],
            PALETTE[1],
            PALETTE[0],
            [0, 255, 221],
            [53, 0, 241],
            [0, 255, 221],
            [149; 3],
            PALETTE[1],
            [255, 7, 86],
            PALETTE[5],
            [149; 3],
            [149; 3],
            PALETTE[0],
            [149; 3],
            WHITE,
            PALETTE[6],
            WHITE,
            WHITE,
            PALETTE[5],
            WHITE,
            WHITE,
            PALETTE[4],
            WHITE,
            [149; 3],
            PALETTE[7],
            WHITE,
            PALETTE[5],
            [228, 74, 93],
            WHITE,
            PALETTE[5],
            PALETTE[5],
        ];
        for (category, rgb) in expected.into_iter().enumerate() {
            assert_eq!(
                block_colour(&LedBlock {
                    category: category as u32,
                    sub_type: 0,
                    model_id: 0
                }),
                rgb
            );
        }
        for (sub_type, rgb) in [
            (1, PALETTE[7]),
            (2, PALETTE[7]),
            (3, PALETTE[5]),
            (4, PALETTE[5]),
            (5, PALETTE[2]),
            (6, PALETTE[2]),
            (7, PALETTE[0]),
            (8, PALETTE[2]),
            (9, [149; 3]),
        ] {
            assert_eq!(
                block_colour(&LedBlock {
                    category: 14,
                    sub_type,
                    model_id: 0
                }),
                rgb
            );
        }
        for (category, model_id, rgb) in [
            (26, 0x6591, PALETTE[4]),
            (26, 0x6592, [149; 3]),
            (26, 0x6593, PALETTE[2]),
            (26, 0x6594, PALETTE[2]),
            (26, 0x6595, [149; 3]),
            (3, 0x0bc0, WHITE),
            (99, 0, WHITE),
        ] {
            assert_eq!(
                block_colour(&LedBlock {
                    category,
                    sub_type: 0,
                    model_id
                }),
                rgb
            );
        }
    }

    #[test]
    fn diff_emits_only_changed_hardware_entries_and_caller_can_limit_mini() {
        let previous = [Led::OFF; 11];
        let mut next = previous;
        next[2] = Led {
            rgb: PALETTE[0],
            brightness: 0.27,
        };
        next[6] = Led {
            rgb: WHITE,
            brightness: 0.06,
        };
        assert_eq!(
            diff(&previous, &next).collect::<Vec<_>>(),
            [(2, next[2]), (6, next[6])]
        );
        assert_eq!(diff(&next, &next).count(), 0);
        let mut brighter = next;
        brighter[2].brightness = 0.06;
        assert_eq!(
            diff(&next, &brighter).collect::<Vec<_>>(),
            [(2, brighter[2])]
        );
        assert_eq!(
            diff(&previous, &next)
                .filter(|(index, _)| *index < led_count(Product::Mini))
                .count(),
            1
        );
        assert_eq!(led_count(Product::QuadCortex), 11);
        assert_eq!(led_count(Product::MiniEmulation), 11);
    }

    #[test]
    fn tempo_colour_clock_in_blue_clock_out_red_overlay_red() {
        let mut inputs = TempoLedInputs::default();
        assert_eq!(tempo_led_colour(inputs), PALETTE[0]);
        inputs.clock_out_enabled = true;
        assert_eq!(tempo_led_colour(inputs), PALETTE[7]);
        for (usb, din) in [(true, false), (false, true)] {
            inputs.usb_clock_in = usb;
            inputs.din_clock_in = din;
            assert_eq!(tempo_led_colour(inputs), [0, 0, 255]);
        }
        inputs.bank_overlay = true;
        assert_eq!(tempo_led_colour(inputs), RED);
    }
}
