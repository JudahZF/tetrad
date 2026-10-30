//! View models: the caller-owned projection that the application supplies.
//! A UI request never changes this projection; only `UiState::set_snapshot` does.

use crate::ItemKind;

use std::collections::BTreeMap;

use tetrad_assets::{Icon, Palette, Rgb};

use crate::control::CaptureDevice;
use crate::settings::{SettingId, SettingValue};
use crate::types::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SceneView {
    pub label: String,
    /// Stored opaque color. Rendering does not infer its RGB interpretation.
    pub raw_color: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridCell {
    pub label: String,
    pub bypassed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetItem {
    pub id: u64,
    pub title: String,
}

/// Stock bank addressing is bank×8+slot (`0x4a2d00 → 0x7c289c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BankSlot {
    pub bank: u16,
    pub slot: u8,
}

impl BankSlot {
    /// Display form such as `1A`. Owned format.
    pub fn label(self) -> String {
        format!(
            "{}{}",
            u32::from(self.bank) + 1,
            char::from(b'A' + self.slot % 8)
        )
    }
}

/// Footswitch mode as shown on the mode button (`0x674ed0`: PRESET, SCENE,
/// STOMP, HYB). The numeric stock mode ID mapping is the application's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FootswitchMode {
    Preset,
    Scene,
    Stomp,
    Hybrid,
    /// (O) An extension's Custom mode ([`crate::CustomModeView`]): ten slots and pages.
    Custom,
}

impl FootswitchMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preset => "PRESET",
            Self::Scene => "SCENE",
            Self::Stomp => "STOMP",
            Self::Hybrid => "HYB",
            Self::Custom => "CUSTOM",
        }
    }
}

/// Hybrid mode row assignment. Upper row A–D, lower row E–H (stock `0x678304`,
/// `0x65dbcc`): `upper` is the mode's first kind, `lower` the second. The default
/// (Scene over Stomp) is stock mode 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HybridRows {
    pub upper: FootswitchMode,
    pub lower: FootswitchMode,
}

impl Default for HybridRows {
    fn default() -> Self {
        Self {
            upper: FootswitchMode::Scene,
            lower: FootswitchMode::Stomp,
        }
    }
}

/// One stomp-mode footswitch. An empty label means unassigned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StompView {
    pub label: String,
    pub engaged: bool,
    pub category: Option<ModelCategory>,
    /// One switch toggles several blocks ("Multiple devices (%1)", `0x11ea9e0`).
    pub multiple: bool,
    /// The first block of the switch: Gig View EDIT STOMP opens its editor.
    pub position: Option<GridPosition>,
    /// Momentary (`stomp_is_momentary`) instead of Latching.
    pub momentary: bool,
    /// Every block assigned to the switch (Gig View EDIT STOMP).
    pub blocks: Vec<GridPosition>,
}

/// Presets of the bank shown in preset mode and during bank preview (≤ 8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BankView {
    pub index: u16,
    pub presets: Vec<PresetItem>,
    /// Slot of the footswitch bank overlay that a first press armed; a second
    /// press of the same switch loads it.
    pub highlighted: Option<u8>,
}

/// Stock category IDs; model ID = category×1000 + n. Labels are the ZenUI
/// strings listed in the g-ui doc (`0xa24cd4`…).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ModelCategory {
    GuitarOverdrive = 0,
    GuitarAmp = 1,
    BassAmp = 2,
    BassOverdrive = 3,
    Eq = 4,
    Compressor = 5,
    Delay = 6,
    Modulation = 7,
    Reverb = 8,
    Wah = 9,
    Splitter = 10,
    Mixer = 11,
    GuitarCabMono = 12,
    FxLoop = 13,
    NeuralCapture = 14,
    Tuner = 15,
    Utility = 16,
    Pitch = 18,
    BassCabMono = 21,
    Filter = 24,
    Morph = 26,
    Looper = 27,
    IrLoader = 29,
    Synth = 30,
    GuitarCabStereo = 32,
    BassCabStereo = 33,
}

impl ModelCategory {
    /// Categories the Virtual Device List offers (manual list, `w4-ui-spec-grid` §3); the
    /// drawer orders them by [`DeviceGroup::ALL`].
    pub const PICKER: [Self; 23] = [
        Self::NeuralCapture,
        Self::GuitarAmp,
        Self::BassAmp,
        Self::GuitarCabMono,
        Self::GuitarCabStereo,
        Self::BassCabMono,
        Self::BassCabStereo,
        Self::IrLoader,
        Self::GuitarOverdrive,
        Self::BassOverdrive,
        Self::Compressor,
        Self::Eq,
        Self::Wah,
        Self::Filter,
        Self::Modulation,
        Self::Pitch,
        Self::Synth,
        Self::Morph,
        Self::Delay,
        Self::Reverb,
        Self::FxLoop,
        Self::Looper,
        Self::Utility,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::GuitarOverdrive => "Guitar Overdrive",
            Self::GuitarAmp => "Guitar Amp",
            Self::BassAmp => "Bass Amp",
            Self::BassOverdrive => "Bass Overdrive",
            Self::Eq => "EQ",
            Self::Compressor => "Compressor",
            Self::Delay => "Delay",
            Self::Modulation => "Modulation",
            Self::Reverb => "Reverb",
            Self::Wah => "Wah",
            Self::Splitter => "Splitter",
            Self::Mixer => "Mixer",
            Self::GuitarCabMono => "Guitar Cab (M)",
            Self::FxLoop => "FX Loop",
            Self::NeuralCapture => "Neural Capture",
            Self::Tuner => "Tuner",
            Self::Utility => "Utility",
            Self::Pitch => "Pitch",
            Self::BassCabMono => "Bass Cab (M)",
            Self::Filter => "Filter",
            Self::Morph => "Morph",
            Self::Looper => "Looper",
            Self::IrLoader => "IR Loader",
            Self::Synth => "Synth",
            Self::GuitarCabStereo => "Guitar Cab (ST)",
            Self::BassCabStereo => "Bass Cab (ST)",
        }
    }

    /// Category of a stock model ID (category×1000 + n; inferred rule).
    pub fn of_model(model_id: u32) -> Option<Self> {
        let category = model_id / 1000;
        Self::PICKER
            .into_iter()
            .chain([Self::Splitter, Self::Mixer, Self::Tuner])
            .find(|candidate| u32::from(*candidate as u8) == category)
    }

    /// Stock category colour for blocks and stomp tiles (`0x2eb3b8`); the
    /// discriminant is the stock category id.
    pub fn hue(self) -> Rgb {
        tetrad_assets::block::category(self as u8)
    }

    /// Plugin tag text colour (`0x002eb7f0`): black on the light category colours, else
    /// white. Stock under every theme: the tag sits on a fixed category colour.
    pub fn tag_ink(self) -> Rgb {
        match self {
            Self::GuitarOverdrive
            | Self::BassOverdrive
            | Self::Compressor
            | Self::Delay
            | Self::Reverb
            | Self::Wah
            | Self::Splitter
            | Self::Mixer
            | Self::FxLoop
            | Self::NeuralCapture
            | Self::Tuner
            | Self::Utility
            | Self::Pitch
            | Self::Filter
            | Self::Morph => Palette::STOCK.canvas,
            _ => Palette::STOCK.text,
        }
    }

    /// Category glyph for block tiles and picker rows: the category switch of `0x002eb99c`
    /// (`icons/virtual devices/*.png`, owned redraws). Tuner shares `utility.png`; Looper
    /// draws `icons/looper/looper_icon.png`. Splitter and Mixer have none (stock "Invalid");
    /// the Grid shows them as "S"/"M" tokens, so `InOut` is only a fallback. Model and
    /// capture-type cases: [`Self::model_icon`].
    pub fn icon(self) -> Icon {
        match self {
            Self::GuitarOverdrive | Self::BassOverdrive => Icon::Overdrive,
            Self::GuitarAmp | Self::BassAmp => Icon::Amp,
            Self::Eq => Icon::Eq,
            Self::Compressor => Icon::Compressor,
            Self::Delay => Icon::Delay,
            Self::Modulation => Icon::Modulation,
            Self::Reverb => Icon::Reverb,
            Self::Wah => Icon::Wah,
            Self::Splitter | Self::Mixer => Icon::InOut,
            Self::GuitarCabMono
            | Self::BassCabMono
            | Self::GuitarCabStereo
            | Self::BassCabStereo => Icon::Cab,
            Self::FxLoop => Icon::FxLoop,
            Self::NeuralCapture => Icon::NeuralCapture,
            Self::Tuner | Self::Utility => Icon::Utility,
            Self::Pitch => Icon::Pitch,
            Self::Filter => Icon::Filter,
            Self::Morph => Icon::Morph,
            Self::Looper => Icon::LooperReels,
            Self::IrLoader => Icon::IrLoader,
            Self::Synth => Icon::Synth,
        }
    }

    /// Block glyph of `model` in this category (`0x002eb99c`): the model and Neural Capture
    /// type cases, else [`Self::icon`]. `capture` is the capture `deviceType` (model word
    /// `[0x38]`); stock tests it only for category 14.
    pub fn model_icon(self, model: u32, capture: Option<CaptureDevice>) -> Icon {
        match (self, model) {
            // `13000 + n`: bits 0x43 (n = 0, 1, 6) are sends, bits 0x8c (n = 2, 3, 7) returns.
            (Self::FxLoop, 13_000 | 13_001 | 13_006) => Icon::FxSend,
            (Self::FxLoop, 13_002 | 13_003 | 13_007) => Icon::FxReturn,
            (Self::NeuralCapture, _) => match capture.unwrap_or_default() {
                CaptureDevice::Default => Icon::NeuralCapture,
                CaptureDevice::Amp => Icon::Amp,
                CaptureDevice::ComboAmp => Icon::ComboAmp,
                CaptureDevice::AmpAndCab => Icon::NcAmpCab,
                CaptureDevice::Cab => Icon::Cab,
                CaptureDevice::Pedal => Icon::NcPedal,
                CaptureDevice::Fuzz => Icon::Fuzz,
                CaptureDevice::Compressor => Icon::Compressor,
                CaptureDevice::Overdrive => Icon::Overdrive,
            },
            (Self::Morph, 26_001) => Icon::Freeze,
            (Self::Morph, 26_003 | 26_004) => Icon::BitCrusher,
            (Self::Utility, 16_008 | 16_013) => Icon::Blend,
            (Self::Utility, 16_011 | 16_012) => Icon::Doubler,
            (Self::Pitch, 18_012) => Icon::Multivoicer,
            // `0x4a0`, tested for every other category.
            (_, 1184) => Icon::NcAmpCab,
            _ => self.icon(),
        }
    }

    /// Block colour of `model` in this category (`0x002eb380`): the Freeze and Bit-Crusher
    /// models, the Neural Capture types and model 3008 (Parallax) differ from [`Self::hue`].
    pub fn model_hue(self, model: u32, capture: Option<CaptureDevice>) -> Rgb {
        use tetrad_assets::block;
        match (self, model, capture) {
            (Self::Morph, 26_001, _) => block::FILTER,
            (Self::Morph, 26_003 | 26_004, _) => block::DRIVE,
            (Self::NeuralCapture, _, Some(device)) => match device {
                CaptureDevice::Amp | CaptureDevice::ComboAmp => block::AMP,
                CaptureDevice::AmpAndCab | CaptureDevice::Cab => block::CAB,
                CaptureDevice::Pedal | CaptureDevice::Fuzz | CaptureDevice::Overdrive => {
                    block::DRIVE
                }
                CaptureDevice::Compressor => block::DYNAMICS,
                CaptureDevice::Default => self.hue(),
            },
            (Self::BassOverdrive, 3008, _) => block::OTHER,
            _ => self.hue(),
        }
    }
}

/// Grid tag text of a plugin id. Stock `0x003a23c4` reads a 64-entry table (empty past 63); the
/// texts come from Cortex Control, the ids from ModelRepo model names. The ids are the 12 supported
/// plugins.
fn plugin_tag(id: u32) -> Option<&'static str> {
    Some(match id {
        1 => "NAM",
        3 => "PLI",
        4 => "PLX",
        5 => "NLY",
        9 => "WON",
        10 => "GOJ",
        11 => "SLO",
        12 => "HEN",
        13 => "PET",
        15 => "RAB",
        20 => "MSH",
        21 => "MAY",
        _ => return None,
    })
}

/// Grid block metadata beside `GridCell` (label and bypass stay in `grid`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockInfo {
    pub category: Option<ModelCategory>,
    pub model_id: Option<u32>,
    /// The model's ModelRepo `plugin_id` (model `+0xf8`, getter `0x2ed1b4`; stock −1 is
    /// `None`).
    pub plugin: Option<u32>,
}

impl BlockInfo {
    /// Stock block glyph ([`ModelCategory::model_icon`]); `None` without a category.
    /// `capture` is the loaded capture's `deviceType` ([`UiSnapshot::captures`]).
    pub fn icon(self, capture: Option<CaptureDevice>) -> Option<Icon> {
        let category = self.category?;
        Some(match self.model_id {
            Some(model) => category.model_icon(model, capture),
            None => category.icon(),
        })
    }

    /// Stock block colour ([`ModelCategory::model_hue`]); `None` without a category.
    pub fn hue(self, capture: Option<CaptureDevice>) -> Option<Rgb> {
        let category = self.category?;
        Some(match self.model_id {
            Some(model) => category.model_hue(model, capture),
            None => category.hue(),
        })
    }

    /// Grid plugin tag text (`0x003a23c4`, by [`Self::plugin`]); `None` draws no tag.
    pub fn plugin_tag(self) -> Option<&'static str> {
        plugin_tag(self.plugin?)
    }

    /// Stock tile fill (`0x002eb624`): a plugin block's colour at alpha 0.3 over the black
    /// tile (`3d-worship-v5`: amp `#4a0c08`, cab `#18184a`), else the tile (canvas). The
    /// 76/255 weight reproduces both measured 565 pixels; 77 does not. Stock also tints some
    /// models by a parameter (8036, 8039, 18018); Tetrad does not model that.
    pub fn fill(self, capture: Option<CaptureDevice>, p: &Palette) -> Rgb {
        match (self.plugin, self.hue(capture)) {
            (Some(_), Some(hue)) => hue.mix(p.canvas, 255 - 76),
            _ => p.canvas,
        }
    }

    /// Stock `0x002ebf94`: a Neural Capture whose `deviceType` is set shows the capture
    /// type badge.
    pub fn capture_badge(self, capture: Option<CaptureDevice>) -> bool {
        self.category == Some(ModelCategory::NeuralCapture)
            && capture.is_some_and(|device| device != CaptureDevice::Default)
    }
}

/// Stock `InputPortId` values (descriptor `cortex_protobuf_v2`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum InputPort {
    #[default]
    Empty = 0,
    Input1 = 1,
    Input2 = 2,
    Input1And2 = 3,
    Return1 = 4,
    Return2 = 5,
    Return1And2 = 6,
    PrevRow = 7,
    UsbIn5 = 8,
    UsbIn6 = 9,
    UsbIn7 = 10,
    UsbIn8 = 11,
    UsbIn5And6 = 12,
    UsbIn7And8 = 13,
    SidechainBuffer = 14,
}

impl InputPort {
    pub const ALL: [Self; 15] = [
        Self::Empty,
        Self::Input1,
        Self::Input2,
        Self::Input1And2,
        Self::Return1,
        Self::Return2,
        Self::Return1And2,
        Self::PrevRow,
        Self::UsbIn5,
        Self::UsbIn6,
        Self::UsbIn7,
        Self::UsbIn8,
        Self::UsbIn5And6,
        Self::UsbIn7And8,
        Self::SidechainBuffer,
    ];

    /// Port selector labels (`0x48b5c`); singles follow the same pattern.
    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "Not In Use",
            Self::Input1 => "Input 1",
            Self::Input2 => "Input 2",
            Self::Input1And2 => "Input 1/2",
            Self::Return1 => "Return 1",
            Self::Return2 => "Return 2",
            Self::Return1And2 => "Return 1/2",
            Self::PrevRow => "Prev. Row",
            Self::UsbIn5 => "USB input 5",
            Self::UsbIn6 => "USB input 6",
            Self::UsbIn7 => "USB input 7",
            Self::UsbIn8 => "USB input 8",
            Self::UsbIn5And6 => "USB input 5/6",
            Self::UsbIn7And8 => "USB input 7/8",
            Self::SidechainBuffer => "Sidechain",
        }
    }

    /// Compact port item text.
    pub fn short(self) -> &'static str {
        match self {
            Self::Empty => "NONE",
            Self::Input1 => "IN 1",
            Self::Input2 => "IN 2",
            Self::Input1And2 => "IN 1/2",
            Self::Return1 => "RET 1",
            Self::Return2 => "RET 2",
            Self::Return1And2 => "RET 1/2",
            Self::PrevRow => "PREV ROW",
            Self::UsbIn5 => "USB 5",
            Self::UsbIn6 => "USB 6",
            Self::UsbIn7 => "USB 7",
            Self::UsbIn8 => "USB 8",
            Self::UsbIn5And6 => "USB 5/6",
            Self::UsbIn7And8 => "USB 7/8",
            Self::SidechainBuffer => "SC",
        }
    }
}

/// Stock `OutputPortId` values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum OutputPort {
    #[default]
    Empty = 0,
    Xlr1And2 = 1,
    Output3And4 = 2,
    Send1And2 = 3,
    Xlr1 = 4,
    Xlr2 = 5,
    Output3 = 6,
    Output4 = 7,
    Send1 = 8,
    Send2 = 9,
    UsbOut5 = 10,
    UsbOut6 = 11,
    UsbOut7 = 12,
    UsbOut8 = 13,
    UsbOut5And6 = 14,
    UsbOut7And8 = 15,
    NextRow3 = 16,
    NextRow4 = 17,
    NextRow3And4 = 18,
    MultipleOuts = 19,
    UsbOut3 = 20,
    UsbOut4 = 21,
    UsbOut3And4 = 22,
}

impl OutputPort {
    pub const ALL: [Self; 23] = [
        Self::Empty,
        Self::Xlr1And2,
        Self::Output3And4,
        Self::Send1And2,
        Self::Xlr1,
        Self::Xlr2,
        Self::Output3,
        Self::Output4,
        Self::Send1,
        Self::Send2,
        Self::UsbOut5,
        Self::UsbOut6,
        Self::UsbOut7,
        Self::UsbOut8,
        Self::UsbOut5And6,
        Self::UsbOut7And8,
        Self::NextRow3,
        Self::NextRow4,
        Self::NextRow3And4,
        Self::MultipleOuts,
        Self::UsbOut3,
        Self::UsbOut4,
        Self::UsbOut3And4,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "Not In Use",
            Self::Xlr1And2 => "Output 1/2",
            Self::Output3And4 => "Output 3/4",
            Self::Send1And2 => "Send 1/2",
            Self::Xlr1 => "Output 1",
            Self::Xlr2 => "Output 2",
            Self::Output3 => "Output 3",
            Self::Output4 => "Output 4",
            Self::Send1 => "Send 1",
            Self::Send2 => "Send 2",
            Self::UsbOut5 => "USB Output 5",
            Self::UsbOut6 => "USB Output 6",
            Self::UsbOut7 => "USB Output 7",
            Self::UsbOut8 => "USB Output 8",
            Self::UsbOut5And6 => "USB Output 5/6",
            Self::UsbOut7And8 => "USB Output 7/8",
            Self::NextRow3 => "Row 3",
            Self::NextRow4 => "Row 4",
            Self::NextRow3And4 => "Row 3/4",
            Self::MultipleOuts => "Multiple Outputs",
            Self::UsbOut3 => "USB Output 3",
            Self::UsbOut4 => "USB Output 4",
            Self::UsbOut3And4 => "USB Output 3/4",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Empty => "NONE",
            Self::Xlr1And2 => "OUT 1/2",
            Self::Output3And4 => "OUT 3/4",
            Self::Send1And2 => "SND 1/2",
            Self::Xlr1 => "OUT 1",
            Self::Xlr2 => "OUT 2",
            Self::Output3 => "OUT 3",
            Self::Output4 => "OUT 4",
            Self::Send1 => "SND 1",
            Self::Send2 => "SND 2",
            Self::UsbOut5 => "USB 5",
            Self::UsbOut6 => "USB 6",
            Self::UsbOut7 => "USB 7",
            Self::UsbOut8 => "USB 8",
            Self::UsbOut5And6 => "USB 5/6",
            Self::UsbOut7And8 => "USB 7/8",
            Self::NextRow3 => "ROW 3",
            Self::NextRow4 => "ROW 4",
            Self::NextRow3And4 => "ROW 3/4",
            Self::MultipleOuts => "MULTI",
            Self::UsbOut3 => "USB 3",
            Self::UsbOut4 => "USB 4",
            Self::UsbOut3And4 => "USB 3/4",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RowView {
    pub input: InputPort,
    pub output: OutputPort,
    /// Path B only: the stored ports that the pair's Splitter (input) and Mixer (output)
    /// replace. They do not route, but stock keeps them and labels the faded port with them
    /// (`0x59d50c`: the label comes from the stored port id). Else `Empty`.
    pub replaced_input: InputPort,
    pub replaced_output: OutputPort,
}

/// Grid sidechain marks in the shown scene. Stock `0x0052e53c` sets them on each block
/// (`+0xf4` source, `+0xf5` sink); the tile paint `0x005a6004` draws them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidechainMarks {
    /// Blocks whose SOURCE selects anything but Off (option index > 0).
    pub sinks: [bool; GRID_CELLS],
    /// Blocks a sink selects (`"R<row> C<column>"`).
    pub sources: [bool; GRID_CELLS],
    /// Row inputs whose stored `Chain.input_control` has the source flag (field 8; the
    /// preset load `0x00681d44` copies it to the input item).
    pub inputs: [bool; GRID_ROWS],
}

/// Split and mix columns (0..8) of one row pair; `None` is the stock −1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PairView {
    pub split: Option<u8>,
    pub mix: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParameterKind {
    /// `position` 0..=1 places the knob arc. Units and taper are the
    /// application's; the editor signal carries no conversion (`0x4b49cc`).
    Continuous {
        position: f32,
    },
    /// A continuous knob whose arc grows from the top (12 o'clock) to the
    /// value, empty at `position` 0.5. Stock marks such cells with
    /// `0x004c05e0(cell, 1)`: Tuner FREQ (`0x3f09b4`) and Tempo PAN
    /// (`0x382bbc`); block editors pass 0.
    Centred {
        position: f32,
    },
    /// A knob that a drag does not turn (stock type 2 Int; mask `0x1013` at `0x4c9e20`): the
    /// numeric keyboard and the encoders set it. Drawn as [`Self::Continuous`].
    Int {
        position: f32,
    },
    Toggle {
        on: bool,
    },
    /// A stepped choice; `position` 0..=1 is the normalized value (step / (steps − 1)).
    /// `steps` is the catalog step count: a knob drag changes one step per
    /// 100 / (steps − 1) px (stock RotarySwitch, `0x5cc994`).
    Choice {
        position: f32,
        steps: u16,
    },
    /// A choice that a drag does not change (stock type 11 ComboBox, mask `0x1013`); a tap
    /// steps it. Drawn as [`Self::Choice`].
    ComboBox {
        position: f32,
    },
    /// A choice in the compact stock field: black, a leading glyph for the
    /// current value, small text (Global EQ TYPE filter shape, shot 009).
    IconChoice {
        icon: Icon,
    },
    /// A string parameter that names an IR or a Neural Capture; a tap opens the
    /// media picker and the value text is the item's display name.
    Media(MediaKind),
}

/// Library a media parameter selects from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// Cab `ir selector`, IR Loader `IR PATH`.
    Ir,
    /// Capture `file_name`.
    Capture,
}

/// A media picker folder. "My IRs" (`0xa09cec`) and "My Captures"
/// (`0xa09ce0`) are the stock user roots; the factory label is owned (I).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFolder {
    FactoryIrs,
    MyIrs,
    MyCaptures,
}

impl MediaFolder {
    pub const ALL: [Self; 3] = [Self::FactoryIrs, Self::MyIrs, Self::MyCaptures];

    pub fn kind(self) -> MediaKind {
        match self {
            Self::FactoryIrs | Self::MyIrs => MediaKind::Ir,
            Self::MyCaptures => MediaKind::Capture,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::FactoryIrs => "Factory IRs",
            Self::MyIrs => "My IRs",
            Self::MyCaptures => "My Captures",
        }
    }
}

/// One selectable IR or capture: names only, never IR or capture data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaEntry {
    pub folder: MediaFolder,
    pub name: String,
    /// Exact text the preset stores in the parameter (factory selector
    /// `<tag>_<cab>_<mic>`, `CIR_…` product id, capture key).
    pub value: String,
}

/// Where an Add device capture is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOrigin {
    /// `factory.db` (read-only).
    Factory,
    /// A user capture product ("My Captures").
    User,
    /// A cloud download (`downloads_product_capture`); its key is the capture hash.
    Download,
}

/// One capture of the Add device capture browser (`zenUI::VirtualDevicesCaptures`, ctor
/// `0x841f60`, list `0x775aec`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryCapture {
    /// The capture key the block stores in parameter 5 (`0x704530`): the product id, or the
    /// hash of a Downloads row (`0x214b84`).
    pub value: String,
    pub name: String,
    /// The metadata `date` (`yyyy-MM-ddThh:mm:ssZ`); the row shows it as `dd MMM yyyy`
    /// (role 0x25, `0x74c420`), "--" when it does not parse.
    pub date: String,
    pub device: Option<CaptureDevice>,
    /// Metadata `version` "2": the block is model 14001 (`0x6f6a40`), else 14000.
    pub nc2: bool,
    pub origin: CaptureOrigin,
    /// The [`CaptureLibrary::folders`] that hold it (`folder_content`).
    pub folders: Vec<u16>,
}

/// A Captures Library folder: the `factory.db` tree ("Factory Captures V1" / "V2", their
/// device type folders and device folders, built by `0x6e86fc`) and the `zenui.db` capture
/// folders ("My Captures" and its subfolders).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryFolder {
    pub name: String,
    /// Index of the parent folder; `None`: a child of the Captures Library root.
    pub parent: Option<u16>,
}

/// The captures the Add device browser lists. The application reads it while the browser
/// shows; the snapshot shares it (`Arc`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureLibrary {
    /// Library root children first in stock order (factory, then user).
    pub folders: Vec<LibraryFolder>,
    pub captures: Vec<LibraryCapture>,
}

impl CaptureLibrary {
    fn validate(&self) -> Result<(), UiError> {
        let folders = self.folders.len();
        if self.captures.len() > MAX_MEDIA_ITEMS
            || folders > MAX_MEDIA_ITEMS
            || (self.captures.iter()).any(|capture| capture.folders.len() > MAX_LIST_ITEMS)
        {
            return Err(UiError::ListTooLong);
        }
        let texts = (self.folders.iter().map(|folder| &folder.name)).chain(
            (self.captures.iter())
                .flat_map(|capture| [&capture.value, &capture.name, &capture.date]),
        );
        if texts.into_iter().any(|text| text.len() > MAX_TEXT_BYTES) {
            return Err(UiError::TextTooLong);
        }
        let known = |index: &u16| usize::from(*index) < folders;
        let parents = (self.folders.iter()).filter_map(|folder| folder.parent.as_ref());
        let members = (self.captures.iter()).flat_map(|capture| &capture.folders);
        if !parents.chain(members).all(known) {
            return Err(UiError::InvalidNumber);
        }
        Ok(())
    }
}

/// One parameter of the selected block, already formatted by the application.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterView {
    /// Stock parameter index (`0x523644` rejects > 99).
    pub index: u16,
    pub label: String,
    pub value: String,
    pub kind: ParameterKind,
    /// Display value and range for the numeric keyboard (`ParameterValueDialog`); `None` for
    /// parameters without one (stock types 3, 5–8 and 10).
    pub entry: Option<NumericRange>,
    /// Scene-assigned (`scene_mode`): the editor shows the stock A B / C D mark (manual
    /// "SCENE ASSIGNMENTS").
    pub scene: bool,
}

/// A parameter in display units (`Parameter::displayValue` `0x5cd110`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumericRange {
    pub value: f32,
    pub min: f32,
    pub max: f32,
    /// Integer entry (`ParameterValueDialog::setInteger`): no '.' key.
    pub integer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: u32,
    pub name: String,
    pub category: ModelCategory,
    /// (O) A community block from an extension package.
    pub extension: bool,
    /// The ModelRepo `plugin_id` of a plugin model, as [`BlockInfo::plugin`].
    pub plugin: Option<u32>,
}

/// Stock Virtual Device List categories in manual order (spec `w4-ui-spec-grid`
/// §3, shots 076/090; 19 rows), plus the owned [`Self::Extensions`] row. Guitar and
/// bass model categories share one group and split into the GUITAR / BASS tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceGroup {
    Plugins,
    Amp,
    NeuralCapture,
    Cab,
    Overdrive,
    Delay,
    Reverb,
    Compressor,
    Pitch,
    Modulation,
    Morph,
    Synth,
    Filter,
    Eq,
    IrLoader,
    Wah,
    FxLoop,
    Looper,
    Utility,
    /// (O) Every block from an extension, whatever its category. Not in [`Self::ALL`]:
    /// the picker shows it after the stock rows when an extension block is installed.
    Extensions,
}

impl DeviceGroup {
    pub const ALL: [Self; 19] = [
        Self::Plugins,
        Self::Amp,
        Self::NeuralCapture,
        Self::Cab,
        Self::Overdrive,
        Self::Delay,
        Self::Reverb,
        Self::Compressor,
        Self::Pitch,
        Self::Modulation,
        Self::Morph,
        Self::Synth,
        Self::Filter,
        Self::Eq,
        Self::IrLoader,
        Self::Wah,
        Self::FxLoop,
        Self::Looper,
        Self::Utility,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Plugins => "Plugins",
            Self::Amp => "Amp",
            Self::NeuralCapture => "Neural Capture",
            Self::Cab => "Cab",
            Self::Overdrive => "Overdrive",
            Self::Delay => "Delay",
            Self::Reverb => "Reverb",
            Self::Compressor => "Compressor",
            Self::Pitch => "Pitch",
            Self::Modulation => "Modulation",
            Self::Morph => "Morph",
            Self::Synth => "Synth",
            Self::Filter => "Filter",
            Self::Eq => "EQ",
            Self::IrLoader => "IR Loader",
            Self::Wah => "Wah",
            Self::FxLoop => "FX Loop",
            Self::Looper => "Looper",
            Self::Utility => "Utility",
            Self::Extensions => "Extensions",
        }
    }

    /// Group and tab of a model category; `None` for categories that are
    /// placed on the Grid, not picked (Splitter, Mixer, Tuner).
    pub fn of(category: ModelCategory) -> Option<(Self, Instrument)> {
        use ModelCategory as C;
        let (group, instrument) = match category {
            C::GuitarAmp => (Self::Amp, Instrument::Guitar),
            C::BassAmp => (Self::Amp, Instrument::Bass),
            C::GuitarCabMono | C::GuitarCabStereo => (Self::Cab, Instrument::Guitar),
            C::BassCabMono | C::BassCabStereo => (Self::Cab, Instrument::Bass),
            C::GuitarOverdrive => (Self::Overdrive, Instrument::Guitar),
            C::BassOverdrive => (Self::Overdrive, Instrument::Bass),
            C::NeuralCapture => (Self::NeuralCapture, Instrument::Guitar),
            C::Delay => (Self::Delay, Instrument::Guitar),
            C::Reverb => (Self::Reverb, Instrument::Guitar),
            C::Compressor => (Self::Compressor, Instrument::Guitar),
            C::Pitch => (Self::Pitch, Instrument::Guitar),
            C::Modulation => (Self::Modulation, Instrument::Guitar),
            C::Morph => (Self::Morph, Instrument::Guitar),
            C::Synth => (Self::Synth, Instrument::Guitar),
            C::Filter => (Self::Filter, Instrument::Guitar),
            C::Eq => (Self::Eq, Instrument::Guitar),
            C::IrLoader => (Self::IrLoader, Instrument::Guitar),
            C::Wah => (Self::Wah, Instrument::Guitar),
            C::FxLoop => (Self::FxLoop, Instrument::Guitar),
            C::Looper => (Self::Looper, Instrument::Guitar),
            C::Utility => (Self::Utility, Instrument::Guitar),
            C::Splitter | C::Mixer | C::Tuner => return None,
        };
        Some((group, instrument))
    }

    /// Groups with GUITAR / BASS tabs (shot 090). Other groups list one tab (I).
    pub fn has_bass(self) -> bool {
        matches!(self, Self::Amp | Self::Cab | Self::Overdrive)
    }

    /// Tile border colour: the stock colour of the group's categories; Plugins
    /// (stock green, shot 076) and Extensions are the accent.
    pub fn hue(self, p: &Palette) -> Rgb {
        ModelCategory::PICKER
            .into_iter()
            .find(|category| Self::of(*category).is_some_and(|(group, _)| group == self))
            .map_or(p.accent, ModelCategory::hue)
    }
}

/// Virtual Device List tab.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Instrument {
    #[default]
    Guitar,
    Bass,
}

/// One Virtual Device Preset (stock `model_presets` row; names only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePresetItem {
    pub id: u64,
    pub model_id: u32,
    pub name: String,
    /// Factory presets are read-only (manual "Virtual Device Presets").
    pub factory: bool,
    /// The model's default preset (`default_model_presets`); row subtitle "Default".
    pub default: bool,
}

/// The Virtual Device Preset whose values a block holds (stock `ModelPresetID`, `0x1f7f50`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetMatch {
    /// A listed user or factory preset ([`DevicePresetItem::id`]).
    Preset(u64),
    /// The model's catalog defaults (stock `SpecialFactoryModelPresetID`, "Neural DSP®
    /// Default"), which the list does not show.
    CatalogDefault,
}

/// The preset each settings page matches, if any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SettingsPresetMatch {
    pub global_eq: Option<u64>,
    pub io: Option<u64>,
}

impl SettingsPresetMatch {
    pub fn get(self, page: crate::SettingsPresetPage) -> Option<u64> {
        match page {
            crate::SettingsPresetPage::GlobalEq => self.global_eq,
            crate::SettingsPresetPage::Io => self.io,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpressionPedal {
    Exp1,
    Exp2,
}

impl ExpressionPedal {
    pub const ALL: [Self; 2] = [Self::Exp1, Self::Exp2];

    pub fn label(self) -> &'static str {
        match self {
            Self::Exp1 => "Expression 1",
            Self::Exp2 => "Expression 2",
        }
    }
}

/// What a pedal drives: one parameter of the block (stock index) or its bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpressionTarget {
    Parameter(u16),
    Bypass,
}

/// One EXP link of the selected block (`model_preset_parameters.exp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpressionLink {
    pub pedal: ExpressionPedal,
    pub target: ExpressionTarget,
}

/// MIN RANGE / MAX RANGE of a linked parameter in percent (`0x34d9b8`: 0..=100, step 1;
/// min above max reverses the sweep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpressionRangeView {
    pub parameter: u16,
    pub min: u8,
    pub max: u8,
}

/// Stock bypass "SWITCH ON" choices (shot 068, `expSwitchType`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExpressionSwitch {
    #[default]
    HeelToe,
    Switch,
    Stop,
}

impl ExpressionSwitch {
    pub const ALL: [Self; 3] = [Self::HeelToe, Self::Switch, Self::Stop];

    pub fn label(self) -> &'static str {
        match self {
            Self::HeelToe => "Heel-Toe",
            Self::Switch => "Switch",
            Self::Stop => "Stop",
        }
    }
}

/// Stock longest SWITCH DELAY (manual: up to 5000 ms).
pub const MAX_SWITCH_DELAY_MS: u16 = 5000;

/// Pedal bypass settings of the selected block (`expSwitch*` columns), shown
/// while BYPASS is linked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExpressionSwitchView {
    pub switch_on: ExpressionSwitch,
    pub inverted: bool,
    pub delay_ms: u16,
    pub latch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetlistItem {
    pub id: u64,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoView {
    pub bpm: f32,
    pub metronome: bool,
    /// Current beat 0-based while the metronome runs.
    pub beat: Option<u8>,
    pub beats_per_bar: u8,
    /// Per-beat marks, beat 0 first (tempo parameters 10..=22).
    pub accents: [BeatAccent; BeatAccent::BEATS],
}

impl Default for TempoView {
    /// 120 BPM is the stock default text (`0xa31214`).
    fn default() -> Self {
        Self {
            bpm: 120.0,
            metronome: false,
            beat: None,
            beats_per_bar: 4,
            accents: [BeatAccent::Dot; BeatAccent::BEATS],
        }
    }
}

/// How a Tempo beat dot draws: tempo parameter `10 + beat` rounded, stock
/// `0x0037e89c` (`+0x1ec`: 1 black brush `0x37eb28`, 2 `0x37eb54`, 3 `0x37e920`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BeatAccent {
    /// 0: the plain dot.
    #[default]
    Dot,
    /// 1: the dot filled black (its ring stays).
    Black,
    /// 2: a small mark above the dot.
    Above,
    /// 3: a small mark below the dot.
    Below,
}

impl BeatAccent {
    /// Beat widgets behind tempo parameters 10..=22.
    pub const BEATS: usize = 13;

    /// Stored parameter value to accent; unknown values draw the plain dot.
    pub fn from_value(value: f32) -> Self {
        match value.round() as i32 {
            1 => Self::Black,
            2 => Self::Above,
            3 => Self::Below,
            _ => Self::Dot,
        }
    }
}

/// Twelve-tone pitch class, 0 = C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note {
    pub pitch_class: u8,
    pub octave: i8,
}

impl Note {
    /// Stock sharp/flat names (`0xa38f8c`…), sharp form shown large.
    pub fn name(self) -> &'static str {
        [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ][usize::from(self.pitch_class % 12)]
    }

    pub fn flat_name(self) -> Option<&'static str> {
        match self.pitch_class % 12 {
            1 => Some("Db"),
            3 => Some("Eb"),
            6 => Some("Gb"),
            8 => Some("Ab"),
            10 => Some("Bb"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TunerView {
    pub note: Option<Note>,
    /// Detune in cents, shown on the −50…+50 scale (`0xa390f4`).
    pub cents: f32,
    pub frequency_hz: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LooperState {
    #[default]
    Empty,
    Armed,
    Recording,
    Playing,
    PlayingToEnd,
    Overdubbing,
    Stopped,
    /// Cached states 8 and 9 (`0x614e48`).
    Duplicating,
    /// Cached states 10 and 11 (`0x614e48`).
    Punching,
}

impl LooperState {
    /// Status line strings (`0x614e48`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Empty => "EMPTY",
            Self::Armed => "ARMED",
            Self::Recording => "RECORDING",
            Self::Playing => "PLAYING",
            Self::PlayingToEnd => "PLAYING TO END",
            Self::Overdubbing => "OVERDUBBING",
            Self::Stopped => "STOPPED",
            Self::Duplicating => "DUPLICATING",
            Self::Punching => "PUNCHING",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LooperWait {
    Signal,
    MidiClockStart,
    PrerollSync,
    Quantize,
    CycleEnd,
}

impl LooperWait {
    pub fn label(self) -> &'static str {
        match self {
            Self::Signal => "WAITING FOR SIGNAL",
            Self::MidiClockStart => "WAITING FOR MIDI CLOCK START",
            Self::PrerollSync => "WAITING FOR PREROLL SYNC",
            Self::Quantize => "WAITING FOR QUANTIZE",
            Self::CycleEnd => "WAITING FOR CYCLE END",
        }
    }
}

/// Looper X status, a projection of `LooperStatus` (control message 28).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LooperView {
    pub state: LooperState,
    pub waiting: Option<LooperWait>,
    /// Playback position 0..=1.
    pub progress: f32,
    pub loop_seconds: Option<f32>,
    pub undo_available: bool,
    pub redo_available: bool,
    pub reverse: bool,
    pub half_speed: bool,
    pub one_shot: bool,
    /// Status word 2 `undo_progress` > 0 (editor `+0x185`): tiles 0–6 are inactive.
    pub undo_in_progress: bool,
    /// Free record time (status `free_samples`), "AVAILABLE m:ss".
    pub available_seconds: Option<f32>,
    /// Editor `+0x187`: footswitches act as Looper tiles (the mini lock button toggles it).
    pub editor_view: bool,
    /// Quad Cortex mini (platform 2/3): B+D closes the view and a lock button shows.
    pub mini: bool,
    /// The action on each footswitch tile A–H (stored `stompAssignments`).
    pub tiles: LooperTiles,
}

/// Looper X action per footswitch letter A–H. "Reassign Looper X actions" swaps
/// two entries (`0x37ab08`); the editor draws tile n with action n.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LooperTiles(pub [crate::LooperAction; FOOTSWITCH_COUNT]);

impl Default for LooperTiles {
    /// The factory order (`LooperAction::TILES`).
    fn default() -> Self {
        Self(crate::LooperAction::TILES)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptureStep {
    #[default]
    Idle,
    ConnectionDiagram,
    Calibration,
    RecordingSignals,
    SanityCheck,
    Training,
    Refine,
    Audition,
    Saving,
    Saved,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AuditionSource {
    #[default]
    Reference,
    Capture,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CaptureView {
    pub step: CaptureStep,
    /// Stage progress 0..=1 during recording/training.
    pub progress: Option<f32>,
    /// IN 1 / IN 2 meters 0..=1.
    pub input_levels: [f32; 2],
    pub audition: AuditionSource,
    /// Application message, such as a recovered calibration error text.
    pub message: String,
    /// Connection diagram page during `ConnectionDiagram`: 0..=4 for manual §6
    /// "01 REFERENCE INSTRUMENT" … "05 REVIEW".
    pub diagram_step: u8,
    /// Back waits for the "Unsaved capture" answer (`0x3cea8c`):
    /// `CaptureRequest::Discard` answers it.
    pub unsaved_prompt: bool,
    /// Parameters of the running capture preset's (1, 4) Cab block; empty: no cab, the
    /// capture block tile is not a control.
    pub cab: Vec<ParameterView>,
    /// The NC2 dialog shows over every screen while Cortex Control runs Neural Capture 2
    /// (`showDialog` `0x3d791c` until `hideDialog` `0x3d7098`). `audition` is its A/B choice.
    pub nc2: Option<Nc2Dialog>,
}

/// `NeuralCapture2Dialog` mode (`+0x1ac`) from the NC2 state slot `0x3dcc2c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nc2Dialog {
    /// Mode 0 (`0x3d7714`): states 1–2 with Cortex Control connected, and state 3.
    InProgress,
    /// Mode 2 (`0x3d7a4c`): states 1–2 without Cortex Control.
    LostConnection,
    /// Mode 1 (`0x3dca14`): the A/B test, states 4 (REFERENCE) and 5 (CAPTURE).
    Ready,
    /// Mode 3 (`0x3d7dc0`, slot `newCaptureSaved()` `0x3d7fc8`): "Neural Capture Saved."
    /// and EXIT.
    Saved,
}

/// Cortex Control's link: connected (`0x6e0444`) and its KeepAlive online (`0x6e045c`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ControlLinkState {
    #[default]
    Disconnected,
    Offline,
    Online,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WifiStatus {
    #[default]
    Idle,
    Scanning,
    NoNetwork,
    Connecting,
    ObtainingIp,
    NetworkCheck,
    InternetConnected,
    NoInternet,
    ConnectionFailed,
    ModuleFailed,
}

impl WifiStatus {
    /// `.data` status table at `0x153f570`… and cell states.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::Scanning => "Scanning Wi-Fi networks...",
            Self::NoNetwork => "No network found. Searching...",
            Self::Connecting => "Connecting...",
            Self::ObtainingIp => "Obtaining IP address...",
            Self::NetworkCheck => "Network check...",
            Self::InternetConnected => "Internet Connected",
            Self::NoInternet => "No Internet Connection",
            Self::ConnectionFailed => "Internet Connection Failed",
            Self::ModuleFailed => {
                "WiFi module failed. Please, contact support at support@neuraldsp.com"
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    /// Stock signal icon index 0..=6 (`0x5d2028`: 0 is `disconnected.png`).
    pub signal: u8,
    pub secured: bool,
    pub connected: bool,
    pub saved: bool,
    /// As the supplicant reports it (lowercase, colons).
    pub bssid: String,
    pub frequency_mhz: u32,
    /// Clamped to -90..=-30 dBm, as the stock scan parser stores it (`0x88c000`).
    pub rssi_dbm: i32,
}

/// 2.4 GHz channel frequencies (`0x11e3be8`): the channel is the index + 1.
const CHANNEL_MHZ: [u32; 14] = [
    2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462, 2467, 2472, 2484,
];

impl WifiNetwork {
    /// `%1, ch: %2, rssi: %3 dBm` (`0x11e3e40`, `0x5d2028`); a frequency outside the 2.4 GHz
    /// table shows "---" (`0x5d11b4`).
    pub fn detail(&self) -> String {
        let channel = CHANNEL_MHZ
            .iter()
            .position(|&mhz| mhz == self.frequency_mhz)
            .map_or_else(|| "---".to_owned(), |index| (index + 1).to_string());
        format!("{}, ch: {channel}, rssi: {} dBm", self.bssid, self.rssi_dbm)
    }

    /// Stock "Weak connection" (`0x5d2028`): the joined network at -60 dBm or below.
    pub fn weak(&self) -> bool {
        self.connected && self.rssi_dbm <= -60
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiView {
    pub status: WifiStatus,
    pub networks: Vec<WifiNetwork>,
    /// Address of the current lease, shown after "IP address " (`0x11e3de4`).
    pub ip_address: Option<String>,
    /// Last failed attempt; the next attempt clears it.
    pub alert: Option<WifiAlert>,
    /// The network of the last attempt, named in the alert popups.
    pub joining: Option<String>,
}

/// Wi-Fi failures, each a stock popup with one OK button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiAlert {
    /// Wi-Fi manager `0x895ca0`.
    AuthenticationFailed,
    /// `connectionRejected` `0x5d5404`.
    ConnectionError,
    /// DHCP failed, `0x5d514c`.
    NoIpAddress,
}

impl WifiAlert {
    /// Popup title and body (`0xa29bbc`, `0x11e42cc`, `0x11e42e0`, `0x11e42fc`); stock bolds
    /// the network name.
    pub fn popup(self, ssid: &str) -> (&'static str, String) {
        match self {
            Self::AuthenticationFailed => {
                ("Login error", format!("Failed to authenticate to {ssid}."))
            }
            Self::ConnectionError => ("Connection error", format!("Failed to reach {ssid}.")),
            Self::NoIpAddress => ("Connection error", "Failed to obtain IP address".into()),
        }
    }
}

/// `UpdaterMessage.state` (control message 60).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UpdaterState {
    #[default]
    Idle,
    RequestingData,
    DownloadingData,
    Updating,
    Reboot,
    Failed,
}

/// `UpdaterMessage.status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UpdaterStatus {
    #[default]
    None,
    NewVersionAvailable,
    NewVersionDownloaded,
    NoNewVersion,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpdaterView {
    pub current_version: String,
    pub available_version: Option<String>,
    pub state: UpdaterState,
    pub status: UpdaterStatus,
    pub progress: Option<f32>,
    pub changelog: String,
    /// Internet reachable. Stock shows the offline page without it unless an update is
    /// downloaded or a reboot is due (`0x42522c`, `0x42501c`).
    pub online: bool,
    /// Update channel; DOWNLOAD UPDATE asks first off the official one (`0x42483c`).
    pub channel: UpdateChannel,
    /// Owned (Tetrad): Tetrad has no profile for `available_version`, so stock CorOS starts
    /// after the install until Tetrad supports it. UPDATE then asks first.
    pub stock_after_install: bool,
}

/// Owned (Tetrad): the Tetrad Updates page. A Tetrad update changes only the Tetrad files on
/// p4; CorOS stays as it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TetradUpdateView {
    /// The running Tetrad.
    pub current_version: String,
    pub state: TetradUpdateState,
    /// Internet reachable.
    pub online: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum TetradUpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// `usb`: the release came over USB (`tetrad-usb install`) and is already on the unit,
    /// so it installs without the internet.
    Available {
        version: String,
        usb: bool,
    },
    /// `progress`: the share of the new binary on p4, `0.0..=1.0`.
    Installing {
        version: String,
        progress: Option<f32>,
    },
    /// `retry`: the version UPDATE TETRAD tries again; `None`: check again.
    /// `usb`: a release that came over USB failed; its files are gone, so the computer must
    /// send it again. No retry, and the page shows the error offline too.
    Failed {
        error: String,
        retry: Option<String>,
        usb: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpdateChannel {
    #[default]
    Official,
    /// Account in a beta tester group (`0xa3dd98`).
    Beta,
    /// Updater suffix set on the device (`0xa3de04`).
    Suffix(String),
}

/// The names in "Neural Capture %3 already exists on your %1 (%2). Would you like to
/// overwrite it?" (`0x465178` mode 1, C): `name` is the cloud capture (`%3`), `existing` the
/// user capture with the same key (`%2`); `%1` is the product name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureQuestion {
    pub name: String,
    pub existing: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupItem {
    pub id: u64,
    pub title: String,
    /// Timestamp or firmware text, shown as given (stock timestamps are UTC).
    pub detail: String,
    /// Local download time, when the backup is on this device (tray mark, shot 117).
    pub downloaded: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackupView {
    pub items: Vec<BackupItem>,
    /// Running operation text and progress, such as a download.
    pub busy: Option<(String, Option<f32>)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountView {
    /// Linked account e-mail; `None` when logged out.
    pub username: Option<String>,
}

/// The two cloud folder kinds of a Directory category (`CloudFolderModel`, folder `+0x40`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CloudScope {
    /// CLOUD PRESETS / CAPTURES / IRs: the account's own products (`ownedByMe`, `0x718684`).
    Cloud,
    /// DOWNLOADS: the account's download queue (folder `+0x40` == 1).
    Downloads,
}

/// One Directory cloud folder as listed so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudFolderView {
    pub kind: ItemKind,
    pub scope: CloudScope,
    pub rows: Vec<CloudItem>,
    /// The last page was full, so the list end asks the next page (`0x43023c`).
    pub more: bool,
    /// A page request is out.
    pub fetching: bool,
    /// Why the last page request failed.
    pub error: Option<String>,
}

/// One cloud product row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloudItem {
    /// The cloud product id (`+0x30`): the download key.
    pub id: String,
    pub name: String,
    pub author: String,
    /// A DOWNLOADS row whose product is stored on this unit (`downloads_product_*` by id,
    /// vtable `+0x6c` in `0x78d204`): a tap loads it instead of a second download.
    pub stored: bool,
}

impl UiSnapshot {
    /// The cloud can be used (`0x7166d0`): the Wi-Fi internet check passed or Cortex Control
    /// is online (`0x669580`, re-r44 §3), and an account is logged in.
    /// `updateCloudFoldersEnabled` (`0x78ff0c`) follows this state.
    pub fn cloud_ready(&self) -> bool {
        (self.wifi.status == WifiStatus::InternetConnected || self.control_online)
            && self.account.username.is_some()
    }

    pub fn cloud_folder(&self, kind: ItemKind, scope: CloudScope) -> Option<&CloudFolderView> {
        (self.cloud.iter()).find(|folder| folder.kind == kind && folder.scope == scope)
    }
}

/// Device Information rows (`zenUI::DeviceInformation` `0x40d364`); `None` shows "-". The
/// stock `Display:` row shows only after a service footswitch combination (`0x40d0c0`) and
/// is not drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AboutView {
    /// `General/customName`; `None` shows the default name (`Neural DSP Quad Cortex`).
    pub device_name: Option<String>,
    /// First 9 characters of the serial file (`0x66b55c`).
    pub serial: Option<String>,
    /// `/sys/class/net/wlan0/address` (`0x66c4b8`).
    pub mac: Option<String>,
    /// The QC with Zenjack and Zencoder boards: their four firmware rows show (hardware types
    /// 0/1; the mini and ESS boards list CorOS, kernel, U-Boot and Zenwireless only).
    pub zen_boards: bool,
    pub coros: Option<String>,
    pub kernel: Option<String>,
    pub uboot: Option<String>,
    pub wireless_fw: Option<String>,
    pub zenjack_app: Option<String>,
    pub zenjack_bootloader: Option<String>,
    pub zencoder_app: Option<String>,
    pub zencoder_bootloader: Option<String>,
}

/// One core column of DSP Diagnostics (update slot `0x364ba0`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DspCoreView {
    /// "Drp Msg:".
    pub dropped_messages: u32,
    /// "FP exc:".
    pub fp_exceptions: u32,
    /// "CPU Av/Pk(%):": three values, printed `a/b/c`.
    pub cpu: [f32; 3],
    /// "EMDMA (%):".
    pub emdma: f32,
    /// DM, PM and SD heap use in %, one row each.
    pub heap: [f32; 3],
}

/// One USB Statistics value (`0x68a90c`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbValue {
    pub text: String,
    pub tone: UsbTone,
}

/// The colour `0x68a90c` gives a value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UsbTone {
    /// The constructor colour, white.
    #[default]
    Plain,
    /// `Qt::white`: a read/write distance in its range.
    InRange,
    /// `Qt::red`: a read/write distance out of its range.
    OutOfRange,
    /// `Qt::green`: the +1 sample count.
    Plus,
    /// `Qt::blue`: the −1 sample count.
    Minus,
}

/// Diagnostics list pages (`_INIT_561`). Gain and Master Volume Knob calibration stay hidden
/// behind stock service combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsPage {
    Dsp,
    Footswitch,
    Usb,
}

impl DiagnosticsPage {
    pub const ALL: [Self; 3] = [Self::Dsp, Self::Footswitch, Self::Usb];

    pub fn label(self) -> &'static str {
        match self {
            Self::Dsp => "DSP Diagnostics",
            Self::Footswitch => "Footswitch Statistics",
            Self::Usb => "USB Statistics",
        }
    }
}

/// A 3rd party licence text the application read for the viewer (`0x6c9434`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LicenceView {
    pub package: String,
    /// `None` while the file is missing (stock logs "Could not find license file!").
    pub text: Option<String>,
}

/// `StorageInfoEntity` usage per category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageUse {
    pub current: u32,
    pub maximum: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageView {
    pub presets: StorageUse,
    pub captures: StorageUse,
    pub impulse_responses: StorageUse,
}

/// Data that changes many times a second, for [`crate::UiState::update_live`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveView {
    pub tempo: TempoView,
    pub tuner: TunerView,
    pub looper: LooperView,
    /// Capture IN 1 / IN 2 meters 0..=1 (`CaptureView::input_levels`).
    pub input_levels: [f32; 2],
    pub cpu_percent: Option<u8>,
}

fn unit(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

impl LiveView {
    pub(crate) fn validate(&self) -> Result<(), UiError> {
        let ok = self.tempo.bpm.is_finite()
            && self.tuner.cents.is_finite()
            && self.tuner.frequency_hz.is_none_or(f32::is_finite)
            && unit(self.looper.progress)
            && self.looper.loop_seconds.is_none_or(f32::is_finite)
            && self.input_levels.into_iter().all(unit);
        if ok {
            Ok(())
        } else {
            Err(UiError::InvalidNumber)
        }
    }
}

/// Messages per Preset MIDI Out target (manual §8: up to 12; tetrad-midi `SLOTS`).
pub const MIDI_OUT_MESSAGES: usize = 12;

/// A Preset MIDI Out target: the on-load list, a footswitch A–H (`Footswitch::TILES`) or
/// an expression pedal (shot 016).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MidiOutTarget {
    OnLoad,
    Footswitch(Footswitch),
    Expression1,
    Expression2,
    /// (O) The messages of a Custom mode slot action ([`crate::CustomSlotView::messages`]) in
    /// the layout of `scope`. They are not Preset MIDI Out data.
    Slot {
        scope: crate::LayoutScope,
        page: u8,
        slot: u8,
        gesture: crate::Gesture,
    },
}

/// Message types of the editor TYPE list. The on-load list offers CC and PC only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MidiMessageKind {
    Cc,
    CcToggle,
    Pc,
}

/// One Preset MIDI Out message. `values` by kind: CC → CC#, VALUE; CC Toggle → CC#,
/// MIN VALUE, MAX VALUE; PC → BANK CC#0, BANK CC#32, PROGRAM#. `None` leaves a part out
/// (stock `u32::MAX`, PC banks only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiOutMessage {
    pub kind: MidiMessageKind,
    /// 1..=16.
    pub channel: u8,
    pub values: [Option<u8>; 3],
}

/// The loaded preset's Preset MIDI Out tables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetMidiView {
    pub on_load: Vec<MidiOutMessage>,
    /// Footswitches A–H in `Footswitch::TILES` order.
    pub footswitches: [Vec<MidiOutMessage>; FOOTSWITCH_COUNT],
    pub expression: [Vec<MidiOutMessage>; 2],
    /// Some slot holds a message type, listed above or not (stock `0x0032da4c`: the first
    /// byte of any of the 12 on-load and 10 × 12 source slots). The Grid mode button then
    /// shows the MIDI glyph.
    pub stored: bool,
}

impl PresetMidiView {
    pub fn messages(&self, target: MidiOutTarget) -> &[MidiOutMessage] {
        match target {
            MidiOutTarget::OnLoad => &self.on_load,
            MidiOutTarget::Footswitch(switch) => switch
                .tile()
                .map_or(&[][..], |tile| self.footswitches[tile].as_slice()),
            MidiOutTarget::Expression1 => &self.expression[0],
            MidiOutTarget::Expression2 => &self.expression[1],
            MidiOutTarget::Slot { .. } => &[],
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.on_load.is_empty()
            && self.footswitches.iter().all(Vec::is_empty)
            && self.expression.iter().all(Vec::is_empty)
    }
}

/// I/O page meters in dB, as the DSP reports them (stock meter ids 1710..1748). The application
/// polls them only while the I/O page shows; [`IoMeters::FLOOR`] means no reading. A cell shows
/// `max(0, (max(-40, dB) + 40) / 40)` (`0x4c9bfc`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IoMeters {
    /// IN 1, IN 2 (1710, 1711).
    pub inputs: [f32; 2],
    /// RET 1, RET 2 (1712, 1713).
    pub returns: [f32; 2],
    /// OUT 1..4 (1714..1717).
    pub outputs: [f32; 4],
    /// SEND 1, 2 (1718, 1719).
    pub sends: [f32; 2],
    /// Limiter of OUT 1..4 as stock shows it: minus the gain reduction, at most 0 dB
    /// (1728, 1729 per pair).
    pub limiters: [f32; 4],
    /// Headphones left, right (1730).
    pub headphones: [f32; 2],
    /// USB IN 1/2 .. 7/8 to the host, each a left/right pair (1741..1744).
    pub usb_in: [[f32; 2]; 4],
    /// USB OUT 1/2 .. 7/8 from the host (1745..1748).
    pub usb_out: [[f32; 2]; 4],
    /// Limiter of SEND 1, 2, the same law as [`Self::limiters`] (values 4, 5 of `0x4022`,
    /// ids 1726, 1727 through `0x4c9af8`).
    pub send_limiters: [f32; 2],
    /// Headphones feed jacks (`0x493464`): each Grid lane output level, folded by its output
    /// port into OUT 1..4, SEND 1, 2 by the louder value. The ten feed meters (1731..1740)
    /// read these six ([`HpFeed::jacks`]).
    pub hp_feed: [f32; 6],
}

/// Grid row port clip lights (`0x595590`, `0x5937cc`): a reading at or above 0 dB lights the
/// port for 1 s (`0x5a41d4`); inputs from the row's input jacks, outputs from its lane.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridClips {
    pub inputs: [bool; GRID_ROWS],
    pub outputs: [bool; GRID_ROWS],
}

/// A Grid row Input Gate meter (`InputPortItem`, `0x5a43e8`): the open part of a 24 px bar,
/// in px. The rest of the bar is the gate's gain reduction, drawn orange.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateMeter(u8);

impl GateMeter {
    pub const WIDTH: u8 = 24;

    /// The meter for a level 0..=1 (`0x4df394`: 1 at 0 dB of reduction, 0 at the floor) as
    /// `0x5a163c` rounds it: the whole px below, the full bar from 23.5 px.
    pub fn from_level(level: f32) -> Self {
        let width = f32::from(Self::WIDTH);
        let open = width * level.clamp(0.0, 1.0);
        Self(if open > width - 0.5 {
            Self::WIDTH
        } else {
            open as u8
        })
    }

    pub fn open(self) -> u8 {
        self.0
    }
}

impl IoMeters {
    pub const FLOOR: f32 = -40.0;

    /// Bar level 0..=1 for a reading in dB (`0x4c9bfc`).
    pub fn level(db: f32) -> f32 {
        ((db.max(Self::FLOOR) - Self::FLOOR) / -Self::FLOOR).max(0.0)
    }
}

impl Default for IoMeters {
    fn default() -> Self {
        let floor = Self::FLOOR;
        Self {
            inputs: [floor; 2],
            returns: [floor; 2],
            outputs: [floor; 4],
            sends: [floor; 2],
            limiters: [floor; 4],
            headphones: [floor; 2],
            usb_in: [[floor; 2]; 4],
            usb_out: [[floor; 2]; 4],
            send_limiters: [floor; 2],
            hp_feed: [floor; 6],
        }
    }
}

/// A Headphones feed: the headphone mix level of one Grid output (`ConnectorsSettings`
/// 1700..1709 in this order, ctors `0x54a28..0x550d0`; meters 1731..1740).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HpFeed {
    Out1,
    Out2,
    Out3,
    Out4,
    Send1,
    Send2,
    Out12,
    MultiOut,
    Out34,
    Send12,
}

impl HpFeed {
    pub const ALL: [Self; 10] = [
        Self::Out1,
        Self::Out2,
        Self::Out3,
        Self::Out4,
        Self::Send1,
        Self::Send2,
        Self::Out12,
        Self::MultiOut,
        Self::Out34,
        Self::Send12,
    ];

    /// The feed a Grid lane output shows on the Headphones page (`0x3597ac`).
    pub fn of(output: OutputPort) -> Option<Self> {
        Some(match output {
            OutputPort::Xlr1And2 => Self::Out12,
            OutputPort::Output3And4 => Self::Out34,
            OutputPort::Send1And2 => Self::Send12,
            OutputPort::Xlr1 => Self::Out1,
            OutputPort::Xlr2 => Self::Out2,
            OutputPort::Output3 => Self::Out3,
            OutputPort::Output4 => Self::Out4,
            OutputPort::Send1 => Self::Send1,
            OutputPort::Send2 => Self::Send2,
            OutputPort::MultipleOuts => Self::MultiOut,
            _ => return None,
        })
    }

    /// Stock knob and meter label (ctor names).
    pub fn label(self) -> &'static str {
        match self {
            Self::Out1 => "OUT 1",
            Self::Out2 => "OUT 2",
            Self::Out3 => "OUT 3",
            Self::Out4 => "OUT 4",
            Self::Send1 => "SEND 1",
            Self::Send2 => "SEND 2",
            Self::Out12 => "OUT 1/2",
            Self::MultiOut => "MULTI OUT",
            Self::Out34 => "OUT 3/4",
            Self::Send12 => "SEND 1/2",
        }
    }

    /// The [`IoMeters::hp_feed`] jacks its meter shows (`0x3562a0`): one for a mono meter
    /// (kind 5), left and right for a pair (kind 6). MULTI OUT shows OUT 1/2.
    pub fn jacks(self) -> &'static [usize] {
        match self {
            Self::Out1 => &[0],
            Self::Out2 => &[1],
            Self::Out3 => &[2],
            Self::Out4 => &[3],
            Self::Send1 => &[4],
            Self::Send2 => &[5],
            Self::Out12 | Self::MultiOut => &[0, 1],
            Self::Out34 => &[2, 3],
            Self::Send12 => &[4, 5],
        }
    }
}

/// Caller-owned workflow projection. A request never changes this projection.
#[derive(Debug, Clone, PartialEq)]
pub struct UiSnapshot {
    /// Shared: a snapshot copies the pointer (the QC copied ~24 ms per sync, 2026-10-15).
    pub directory: std::sync::Arc<DirectoryView>,
    pub preset_id: Option<u64>,
    pub preset_title: String,
    pub dirty: bool,
    pub selected_scene: SceneIndex,
    pub scenes: [SceneView; SCENE_COUNT],
    pub grid: [GridCell; GRID_CELLS],
    /// Presets of the open setlist, in bank order (8 per bank, 4 with a hybrid-with-preset
    /// mode cycle, `0x58b968`). Shared: a snapshot copies the pointer, not the list.
    pub presets: std::sync::Arc<Vec<PresetItem>>,
    pub bank_preview_active: bool,
    /// Raw stock mode ID; shown only when `footswitch_mode` is `None`.
    pub mode_id: u8,
    pub status: String,
    pub preset_slot: Option<BankSlot>,
    pub footswitch_mode: Option<FootswitchMode>,
    /// Modes in the cycle; stock constructor list is `[0, 1, 2]`.
    pub allowed_modes: Vec<FootswitchMode>,
    pub hybrid: HybridRows,
    pub stomps: [StompView; FOOTSWITCH_COUNT],
    pub bank: BankView,
    /// Presets per bank (tetrad-core `BankSize`): 8, or 4 when the mode cycle holds a hybrid
    /// with PRESET (`0x58b968`); mini 4 or 2. The application sets it.
    pub bank_size: u8,
    pub blocks: [BlockInfo; GRID_CELLS],
    pub rows: [RowView; GRID_ROWS],
    pub pairs: [PairView; 2],
    pub sidechain: SidechainMarks,
    /// The `deviceType` of the capture each Neural Capture block has loaded (stock model word
    /// `[0x38]`, read from the capture metadata); `None` before it loads.
    pub captures: [Option<CaptureDevice>; GRID_CELLS],
    pub selected_block: Option<GridPosition>,
    pub parameters: Vec<ParameterView>,
    pub clipboard: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub favorite: bool,
    /// The loaded preset has a file. Without one, Save opens the name keyboard and Save as,
    /// Edit Details, Delete and favorites are greyed (`0x003bbb90`).
    pub preset_file: bool,
    /// The load generation: the application bumps it when another preset file or slot loads.
    /// A Grid ⋮ Delete Preset confirm closes on a change (review r35 G1).
    pub preset_load: u64,
    /// The loaded preset's sidecar `instrument_type`; the Save As PREFERRED INSTRUMENT
    /// page starts on it (`0x58ed3c` → `0x7e7c50`).
    pub preset_instrument: PreferredInstrument,
    /// Preset favorites, newest first (`favorites`, at most 64); `id` is the list index.
    pub favorite_presets: Vec<PresetItem>,
    pub cpu_percent: Option<u8>,
    /// The long lists are `Arc<Vec<_>>`, not `Arc<[_]>`: only a sized `Arc` compares by
    /// pointer first, and the device compares each new snapshot with the shown one (the QC
    /// spent 3–8 ms per compare on these lists, r53).
    pub models: std::sync::Arc<Vec<ModelEntry>>,
    /// IRs and captures the media picker lists, in display order.
    pub media: std::sync::Arc<Vec<MediaEntry>>,
    /// The captures of the Add device capture browser; empty until it first shows.
    pub capture_library: std::sync::Arc<CaptureLibrary>,
    pub setlists: Vec<SetlistItem>,
    pub active_setlist: Option<u64>,
    pub settings: BTreeMap<SettingId, SettingValue>,
    pub tempo: TempoView,
    pub tuner: TunerView,
    pub looper: LooperView,
    pub capture: CaptureView,
    pub wifi: WifiView,
    /// Extensions pages (`crate::extensions`).
    pub extensions: crate::ExtensionsView,
    /// Mappings page (`crate::mappings`).
    pub mappings: crate::MappingsView,
    /// (O) The Custom footswitch mode; `None` when no extension adds one.
    pub custom: Option<crate::CustomModeView>,
    /// Remote page (`crate::remote`).
    pub remote: crate::RemoteView,
    /// Bluetooth page (`crate::bluetooth`).
    pub bluetooth: crate::BluetoothView,
    /// Settings › Tetrad › Boot Selector: the launcher shows the boot selector. `None` until
    /// the application read it, or when it could not.
    pub boot_selector: Option<bool>,
    /// The application's last toast (`crate::remote::ToastView`).
    pub toast: Option<crate::ToastView>,
    pub updater: UpdaterView,
    pub tetrad_update: TetradUpdateView,
    pub backups: BackupView,
    /// A backup restore owns the user data: the stock loading overlay covers every view and
    /// takes every touch until the device restarts (`0x81b74c`, text from `0x3fba64`).
    pub restoring: bool,
    pub account: AccountView,
    /// Cortex Control is connected (`0x6e0444`) and its KeepAlive says online (`0x6e045c`):
    /// the cloud calls go over Control (re-r44 §3), so the cloud is usable without Wi-Fi.
    pub control_online: bool,
    /// Cortex Control's link for the cloud dialog (`0x5b0958`), whatever the cloud route.
    pub control_link: ControlLinkState,
    /// Directory cloud folders listed so far (CLOUD and DOWNLOADS rows), in no order.
    pub cloud: std::sync::Arc<Vec<CloudFolderView>>,
    pub about: AboutView,
    /// DSP Diagnostics rows, Core 1–4: the last block each core sent; `None` shows "-1".
    pub dsp_diagnostics: [Option<DspCoreView>; 4],
    /// USB Statistics values in the stock row order (`0x68a90c`) from the last SOC2 ARM
    /// block; `None` shows "---" (`0x689de8`).
    pub usb_statistics: Option<[UsbValue; 21]>,
    pub storage: StorageView,
    /// Virtual Device Presets the application offers, for any model (at most
    /// [`MAX_DEVICE_PRESETS`]); checked only when the `Arc` changes.
    pub device_presets: std::sync::Arc<Vec<DevicePresetItem>>,
    /// Preset whose parameters the selected block matches or auditions.
    pub device_preset: Option<u64>,
    /// The preset the open editor's block matches (stock watches only that block,
    /// `0x4b4764`): the editor header draws `Icon::PresetMatch` after the model name.
    pub editor_preset: Option<PresetMatch>,
    /// The Global EQ and I/O presets the current page values match (`0x1f8600`): the
    /// popup button tint, the green row, and Save As / Overwrite disabled.
    pub settings_preset_match: SettingsPresetMatch,
    /// The device is in Copy/Swap Scene mode (`perform.scene_clipboard`). The mode ends only
    /// on a real copy or a dismiss (`0x20eec4`, `0x5b23c8`); its end closes the dialog.
    pub scene_copy: bool,
    /// Pattern Tremolo (model 7040) STEP n CONNECTED of the selected block, n = 1..=15
    /// (hidden parameters 27..=41): step n joins step n + 1. All off for other blocks.
    pub step_connected: [bool; 15],
    /// Pinned Virtual Devices in pin order (`pinned_models`): the list puts them on top of
    /// their category (manual "BROWSING & LOADING VIRTUAL DEVICES").
    pub pinned_models: Vec<u32>,
    /// The licence the 3rd Party Licenses viewer asked for ([`crate::UiCommand::ShowLicence`]).
    pub licence: Option<LicenceView>,
    /// EXP links of the selected block's parameters and bypass.
    pub expression: Vec<ExpressionLink>,
    pub expression_switch: ExpressionSwitchView,
    pub preset_midi: PresetMidiView,
    pub io_meters: IoMeters,
    pub grid_clips: GridClips,
    /// Input Gate meter over each row's input port; `None` hides it (`0x5a3868`).
    pub gate_meters: [Option<GateMeter>; GRID_ROWS],
    /// Quad Cortex mini (`0x499a60`): POWER acts on release and one press unlocks; the
    /// Master Volume overlay stays 2 s instead of 1 s.
    pub mini: bool,
    /// Plugin models whose licence is not active: the Grid tile and the model picker show a
    /// lock (`plugin_lock.png`, `0x005a6004`: drawn unless the plugin status is 2).
    pub locked_models: std::collections::BTreeSet<u32>,
    /// (O) Plugin ids with no licence at all (status 0). The model picker and the Directory
    /// Plugin Presets leave their models and folders out; stock lists them with the lock. A
    /// licensed plugin that waits for its activation stays, with the lock.
    pub unlicensed_plugins: std::collections::BTreeSet<u32>,
    /// The newest device notice a user action raised: the stock MessageDialog shows it once.
    pub notice: Option<NoticeView>,
    /// The preset name of a Directory upload whose name is already on the account: the
    /// overwrite dialog (`0x46485c`) asks while it is set ([`crate::UiCommand::UploadOverwrite`]).
    pub upload_overwrite: Option<String>,
    /// A cloud capture download that waits for "Neural Capture already exists" (`0x465178`
    /// mode 1): the dialog asks while it is set ([`crate::UiCommand::CaptureOverwrite`]).
    pub capture_overwrite: Option<CaptureQuestion>,
    /// The name of a capture upload that waits for "Neural Capture already exists"
    /// (`0x465178` mode 0): the dialog asks while it is set
    /// ([`crate::UiCommand::CaptureUploadOverwrite`]).
    pub capture_upload_overwrite: Option<String>,
    /// An upload runs (name check or send): the `%1` of the stock status "Uploading %1, please
    /// wait." (`0x785dcc`: "Presets" for a preset folder, else "Neural Captures"), shown until
    /// its result (review r31 D2, review r36 H4).
    pub uploading: Option<&'static str>,
    /// A batch upload runs (Multi Select UPLOAD, `0x785dcc`): the stock progress dialog shows
    /// over every screen, also under a batch question.
    pub upload_progress: Option<UploadProgress>,
    /// The upload overwrite question belongs to a batch (`0x463d2c` batch form): the batch
    /// texts and OVERWRITE / SKIP; one answer serves the rest of the batch.
    pub upload_batch: bool,
    /// The Add to Grid cells by row, then column, while the mode waits for a tap; `None`
    /// when no [`crate::UiCommand::StartAddToGrid`] is open.
    pub add_to_grid: Option<[[crate::CellFit; GRID_COLUMNS]; GRID_ROWS]>,
    /// The DSPErrorDialog ("Something went wrong", ctor `0x6c6248`) shows over every screen:
    /// a DSP fault latched with its dialog (slot `0x5149dc`) until CANCEL or REBOOT.
    pub dsp_error: bool,
    /// Footswitch tile (0..=7) of the selected block's stomp assignment.
    pub selected_stomp: Option<u8>,
    /// Ranges of the selected block's linked parameters.
    pub expression_ranges: Vec<ExpressionRangeView>,
    /// The lane control or junction whose editor is open; `parameters` are its own.
    pub selected_site: Option<EditorSite>,
    /// The last meter values of the selected block by parameter index, normalized as the
    /// stock meter widget takes them (Parallax GAIN REDUCTION 41, `0x636054`).
    pub editor_meters: BTreeMap<u16, f32>,
    /// The notes the selected Minivoicer's two voices play: `MinivoicerDriver` voice send
    /// `0x575010` → editor cache `0x61d994`. `None` is a silent voice ("-").
    pub voice_notes: [Option<u8>; 2],
    /// Captures the corruption check moved out of the library (O: stock deletes them), in
    /// quarantine order. Device Storage lists them with RESTORE.
    pub quarantine: Vec<QuarantineRow>,
    /// The active palette and the themes of Settings → Appearance (owned).
    pub appearance: AppearanceView,
}

/// Settings → Appearance (owned): the active palette and the themes to pick from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppearanceView {
    /// The active theme's colours. Recovery and the boot menu stay stock.
    pub palette: tetrad_theme::Palette,
    /// Built-in themes first, then theme files, then extension themes.
    pub themes: Vec<ThemeRow>,
}

/// One theme on the Appearance page.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeRow {
    pub id: tetrad_theme::ThemeId,
    pub name: String,
    /// Where it comes from: "Built-in", "Theme file" or the extension's name.
    pub source: String,
    /// Its colours, or why it failed its checks (it cannot be picked then).
    pub palette: Result<tetrad_theme::Palette, String>,
    pub selected: bool,
}

/// A capture in the Tetrad quarantine table (O, review r23 K5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineRow {
    /// The library id (Downloads: the id or hash key).
    pub id: String,
    /// The Downloads NC store; else the user capture store.
    pub downloads: bool,
    /// Why the check removed it.
    pub reason: String,
}

/// A running batch upload (`0x785dcc`, `0x847934`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadProgress {
    /// The `%1` of "Uploading %1, please wait.": "Presets" or "Neural Captures".
    pub noun: &'static str,
    /// The items started so far, the one in flight included (`progress(handled, total, 0)`).
    pub handled: usize,
    pub total: usize,
}

/// A device refusal or failure as the stock MessageDialog shows it (helper `0x5462bc`:
/// title, text, one button). `id` changes for each new notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeView {
    pub id: u64,
    pub title: String,
    pub body: String,
    /// The button text: "OK" (`0x5462bc`), or "GOT IT" in the info popup (`0x54617c`).
    pub button: &'static str,
}

impl Default for UiSnapshot {
    fn default() -> Self {
        Self {
            directory: Default::default(),
            preset_id: None,
            preset_title: "NO PRESET LOADED".into(),
            dirty: false,
            selected_scene: SceneIndex(0),
            scenes: Default::default(),
            grid: std::array::from_fn(|_| GridCell::default()),
            presets: Default::default(),
            bank_preview_active: false,
            bank_size: FOOTSWITCH_COUNT as u8,
            mode_id: 0,
            status: "OFFLINE VIEW - COMMANDS REQUIRE APP".into(),
            preset_slot: None,
            footswitch_mode: None,
            allowed_modes: vec![
                FootswitchMode::Preset,
                FootswitchMode::Scene,
                FootswitchMode::Stomp,
            ],
            hybrid: HybridRows::default(),
            stomps: Default::default(),
            bank: BankView::default(),
            blocks: [BlockInfo::default(); GRID_CELLS],
            rows: [RowView::default(); GRID_ROWS],
            pairs: [PairView::default(); 2],
            sidechain: SidechainMarks::default(),
            captures: [None; GRID_CELLS],
            selected_block: None,
            parameters: Vec::new(),
            clipboard: false,
            can_undo: false,
            can_redo: false,
            favorite: false,
            preset_file: false,
            preset_load: 0,
            preset_instrument: PreferredInstrument::NotSet,
            favorite_presets: Vec::new(),
            cpu_percent: None,
            models: Default::default(),
            media: Default::default(),
            capture_library: Default::default(),
            setlists: Vec::new(),
            active_setlist: None,
            settings: BTreeMap::new(),
            tempo: TempoView::default(),
            tuner: TunerView::default(),
            looper: LooperView::default(),
            capture: CaptureView::default(),
            wifi: WifiView::default(),
            extensions: crate::ExtensionsView::default(),
            mappings: crate::MappingsView::default(),
            custom: None,
            remote: crate::RemoteView::default(),
            bluetooth: crate::BluetoothView::default(),
            boot_selector: None,
            toast: None,
            updater: UpdaterView::default(),
            tetrad_update: TetradUpdateView::default(),
            backups: BackupView::default(),
            restoring: false,
            account: AccountView::default(),
            control_online: false,
            control_link: ControlLinkState::Disconnected,
            cloud: Default::default(),
            about: AboutView::default(),
            dsp_diagnostics: [None; 4],
            usb_statistics: None,
            storage: StorageView::default(),
            device_presets: Default::default(),
            pinned_models: Vec::new(),
            licence: None,
            device_preset: None,
            editor_preset: None,
            settings_preset_match: SettingsPresetMatch::default(),
            scene_copy: false,
            step_connected: [false; 15],
            expression: Vec::new(),
            expression_switch: ExpressionSwitchView::default(),
            preset_midi: PresetMidiView::default(),
            io_meters: IoMeters::default(),
            grid_clips: GridClips::default(),
            gate_meters: [None; GRID_ROWS],
            mini: false,
            locked_models: std::collections::BTreeSet::new(),
            unlicensed_plugins: std::collections::BTreeSet::new(),
            notice: None,
            upload_overwrite: None,
            capture_overwrite: None,
            capture_upload_overwrite: None,
            uploading: None,
            upload_progress: None,
            upload_batch: false,
            add_to_grid: None,
            dsp_error: false,
            selected_stomp: None,
            expression_ranges: Vec::new(),
            selected_site: None,
            editor_meters: BTreeMap::new(),
            voice_notes: [None; 2],
            quarantine: Vec::new(),
            appearance: AppearanceView::default(),
        }
    }
}

impl UiSnapshot {
    pub fn setting(&self, id: SettingId) -> Option<&SettingValue> {
        self.settings.get(&id)
    }

    /// The name of the open setlist: the stable key of a preset COPY source (review r23 K11).
    pub fn active_setlist_name(&self) -> Option<&str> {
        let id = self.active_setlist?;
        let setlist = self.setlists.iter().find(|setlist| setlist.id == id)?;
        Some(&setlist.title)
    }

    /// The live part of this snapshot, as [`crate::UiState::update_live`] takes it.
    pub fn live(&self) -> LiveView {
        LiveView {
            tempo: self.tempo,
            tuner: self.tuner,
            looper: self.looper,
            input_levels: self.capture.input_levels,
            cpu_percent: self.cpu_percent,
        }
    }

    pub(crate) fn cell(&self, position: GridPosition) -> &GridCell {
        &self.grid[position.index()]
    }

    pub(crate) fn occupied(&self, position: GridPosition) -> bool {
        !self.cell(position).label.is_empty() || self.blocks[position.index()].category.is_some()
    }

    /// Validate every bound before the snapshot replaces the current one.
    /// Checks the snapshot that replaces `old`. The shared lists (directory, models, media,
    /// presets) are checked only when their `Arc` changed: on the QC they made each check 7–17 ms.
    pub(crate) fn validate(&self, old: &UiSnapshot) -> Result<(), UiError> {
        use std::sync::Arc;
        if !Arc::ptr_eq(&self.directory, &old.directory) {
            self.directory.validate()?;
        }
        if !Arc::ptr_eq(&self.capture_library, &old.capture_library) {
            self.capture_library.validate()?;
        }
        let new_models = !Arc::ptr_eq(&self.models, &old.models);
        let new_media = !Arc::ptr_eq(&self.media, &old.media);
        if (new_models && self.models.len() > MAX_LIST_ITEMS)
            || (new_media && self.media.len() > MAX_MEDIA_ITEMS)
        {
            return Err(UiError::ListTooLong);
        }
        if new_models && has_duplicates(self.models.iter().map(|model| u64::from(model.id))) {
            return Err(UiError::DuplicateId);
        }
        let new_device_presets = !Arc::ptr_eq(&self.device_presets, &old.device_presets);
        if new_device_presets {
            let presets = &self.device_presets;
            if presets.len() > MAX_DEVICE_PRESETS {
                return Err(UiError::ListTooLong);
            }
            if has_duplicates(presets.iter().map(|preset| preset.id)) {
                return Err(UiError::DuplicateId);
            }
            if presets
                .iter()
                .any(|preset| preset.name.len() > MAX_TEXT_BYTES)
            {
                return Err(UiError::TextTooLong);
            }
        }
        let model_names = new_models.then(|| self.models.iter().map(|model| &model.name));
        let media_texts =
            new_media.then(|| (self.media.iter()).flat_map(|entry| [&entry.name, &entry.value]));
        if (model_names.into_iter().flatten())
            .chain(media_texts.into_iter().flatten())
            .any(|text| text.len() > MAX_TEXT_BYTES)
        {
            return Err(UiError::TextTooLong);
        }
        let new_presets = !Arc::ptr_eq(&self.presets, &old.presets);
        if new_presets && self.presets.len() > MAX_PRESETS {
            return Err(UiError::TooManyPresets);
        }
        if new_presets && has_duplicates(self.presets.iter().map(|preset| preset.id)) {
            return Err(UiError::DuplicatePreset);
        }
        if self.bank.presets.len() > FOOTSWITCH_COUNT
            || self.allowed_modes.len() > 4
            || [
                self.parameters.len(),
                self.setlists.len(),
                self.wifi.networks.len(),
                self.backups.items.len(),
                self.settings.len(),
                self.pinned_models.len(),
                self.expression.len(),
            ]
            .into_iter()
            .any(|len| len > MAX_LIST_ITEMS)
        {
            return Err(UiError::ListTooLong);
        }
        let midi = &self.preset_midi;
        let midi_lists = std::iter::once(&midi.on_load)
            .chain(&midi.footswitches)
            .chain(&midi.expression);
        for list in midi_lists {
            if list.len() > MIDI_OUT_MESSAGES {
                return Err(UiError::ListTooLong);
            }
            if list.iter().any(|message| {
                !(1..=16).contains(&message.channel)
                    || message.values.iter().flatten().any(|value| *value > 127)
            }) {
                return Err(UiError::InvalidNumber);
            }
        }
        if has_duplicates(self.setlists.iter().map(|setlist| setlist.id))
            || has_duplicates(self.backups.items.iter().map(|item| item.id))
            || has_duplicates(self.parameters.iter().map(|p| u64::from(p.index)))
        {
            return Err(UiError::DuplicateId);
        }
        self.live().validate()?;
        let numbers_ok = self.capture.progress.is_none_or(unit)
            && self.updater.progress.is_none_or(unit)
            && self
                .backups
                .busy
                .as_ref()
                .is_none_or(|(_, progress)| progress.is_none_or(unit))
            && self
                .parameters
                .iter()
                .all(|parameter| match parameter.kind {
                    ParameterKind::Continuous { position }
                    | ParameterKind::Centred { position }
                    | ParameterKind::Int { position } => unit(position),
                    _ => true,
                });
        if !numbers_ok || self.expression_switch.delay_ms > MAX_SWITCH_DELAY_MS {
            return Err(UiError::InvalidNumber);
        }
        let preset_titles = new_presets.then(|| self.presets.iter().map(|preset| &*preset.title));
        if (self.texts().chain(preset_titles.into_iter().flatten()))
            .any(|text| text.len() > MAX_TEXT_BYTES)
        {
            return Err(UiError::TextTooLong);
        }
        Ok(())
    }

    fn texts(&self) -> impl Iterator<Item = &str> {
        let about = &self.about;
        [&self.preset_title, &self.status, &self.capture.message]
            .into_iter()
            .chain(self.scenes.iter().map(|scene| &scene.label))
            .chain(self.grid.iter().map(|cell| &cell.label))
            .chain(self.bank.presets.iter().map(|preset| &preset.title))
            .chain(self.stomps.iter().map(|stomp| &stomp.label))
            .chain(
                self.parameters
                    .iter()
                    .flat_map(|parameter| [&parameter.label, &parameter.value]),
            )
            .chain(self.setlists.iter().map(|setlist| &setlist.title))
            .chain(self.wifi.networks.iter().map(|network| &network.ssid))
            .chain(
                self.backups
                    .items
                    .iter()
                    .flat_map(|item| [&item.title, &item.detail]),
            )
            .chain(self.backups.busy.iter().map(|(text, _)| text))
            .chain([&self.updater.current_version, &self.updater.changelog])
            .chain(self.updater.available_version.iter())
            .chain(self.wifi.ip_address.iter())
            .chain(match &self.updater.channel {
                UpdateChannel::Suffix(suffix) => Some(suffix),
                _ => None,
            })
            .chain(self.account.username.iter())
            .chain(self.capture_upload_overwrite.iter())
            .chain(
                (self.capture_overwrite.iter())
                    .flat_map(|question| [&question.name, &question.existing]),
            )
            .chain(
                [
                    &about.device_name,
                    &about.serial,
                    &about.mac,
                    &about.coros,
                    &about.kernel,
                    &about.uboot,
                    &about.wireless_fw,
                    &about.zenjack_app,
                    &about.zenjack_bootloader,
                    &about.zencoder_app,
                    &about.zencoder_bootloader,
                ]
                .into_iter()
                .flatten(),
            )
            .chain(self.settings.values().filter_map(|value| match value {
                SettingValue::Text(text) => Some(text),
                _ => None,
            }))
            .map(String::as_str)
    }
}

fn has_duplicates(ids: impl Iterator<Item = u64>) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    ids.into_iter().any(|id| !seen.insert(id))
}

/// Stock Directory metadata, independent of names displayed by the media picker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectoryView {
    pub slots: Vec<DirectorySlot>,
    /// Every filled slot of every setlist: Directory search spans all setlists (manual
    /// "SEARCHING ITEMS").
    pub presets: Vec<DirectoryPreset>,
    /// Setlists (`SetlistItem::id`) whose slots could not be read: they have no rows in
    /// `presets`, so an empty slot there cannot be told from a full one.
    pub unread_setlists: Vec<u64>,
    pub media: Vec<DirectoryMedia>,
    pub folders: Vec<DirectoryFolder>,
    pub favorites: Vec<DirectoryItem>,
    pub recents: Vec<DirectoryItem>,
    /// Every capture Recents key, newest first, `factory.db` captures included (they have no
    /// Directory item): the capture browser's Recent folder (`VDCRecentNCModel`).
    pub capture_recents: Vec<String>,
    pub plugins: Vec<DirectoryPlugin>,
    /// Capture keys in the `pinned_models` table that were not pinned in this session
    /// (`zenUI::PinnedModels` sets `+0xc` and not `+0x24`, `0x495684`/`0x495704`): the
    /// Directory sort puts them on top (`0x755258`).
    pub pinned_captures: Vec<String>,
    /// The saving destination folder of each library (`downloadDestinationFolderIR` /
    /// `NC`, the user root when unset): its ⋮ menu shows "Saving destination".
    pub saving_destinations: Vec<i64>,
}

/// Stock capture device types by code (`deviceType`, labels `0x74b5f0`): the filter rows and
/// the Device Type sort and jump bar (`JumpScrollbarDevice`, `0x8050cc`).
pub const CAPTURE_DEVICE_TYPES: [&str; 9] = [
    "Default",
    "Amp",
    "Combo Amp",
    "Amp + Cab",
    "Cab",
    "Pedal",
    "Fuzz",
    "Compressor",
    "Overdrive",
];

/// Capture versions of the filter (`VERSIONS`, init list `"" "1" "2"`, labels `0x74b744`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum CaptureVersion {
    #[default]
    All,
    V1,
    V2,
}

impl CaptureVersion {
    pub const ALL: [Self; 3] = [Self::All, Self::V1, Self::V2];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::V1 => "Version 1",
            Self::V2 => "Version 2",
        }
    }

    /// The metadata `version` text this entry keeps; `None` for All.
    pub fn stock(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::V1 => Some("1"),
            Self::V2 => Some("2"),
        }
    }
}

/// A row of the BANK UP capture cycle ([`crate::UiState::capture_cycle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureCycleRow<'a> {
    /// The text the media picker writes into the block (`MediaEntry::value`).
    pub value: &'a str,
    pub name: &'a str,
    /// Metadata `version` ("1", "2"); empty when the metadata did not decode.
    pub version: &'a str,
}

/// The plain capture metadata fields the Directory uses (`0x7ee62c` case 2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureMeta {
    /// Index in [`CAPTURE_DEVICE_TYPES`].
    pub device: Option<u8>,
    /// `gainType` 0..=10 (stock writes it only when it is not negative).
    pub gain: Option<u8>,
    /// Metadata `version` text ("1", "2").
    pub version: String,
    /// `instrumentType` code 0..=5 (`0x7ec714`).
    pub instrument: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryPreset {
    /// `SetlistItem::id` of its setlist and the slot there.
    pub setlist: u64,
    pub slot: u64,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectorySlot {
    pub title: String,
    pub id: u64,
    /// Identity of the preset the slot holds (O): it changes when the slot names another
    /// file or its file is written again (a same-name OVERWRITE, a delete and a paste), so a
    /// row action that waits does not act on a new preset with the same title. 0 when empty.
    pub key: u64,
    pub empty: bool,
    pub author: String,
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryMedia {
    pub value: String,
    pub author: String,
    pub date: String,
    pub read_only: bool,
    pub folders: Vec<i64>,
    /// Capture filter and Device Type / Gain sort data the media side decoded; `None` for an
    /// IR or unreadable metadata.
    pub capture: Option<CaptureMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryFolder {
    pub id: i64,
    pub kind: MediaKind,
    pub name: String,
    pub parent: i64,
    pub read_only: bool,
    pub root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryItem {
    pub legacy_index: Option<u64>,
    pub kind: ItemKind,
    pub folder: String,
    pub value: String,
    pub name: String,
    pub author: String,
    pub date: String,
    /// Plugin presets: the sidecar `instrument_type` (sort role 0x24).
    pub instrument: PreferredInstrument,
}

/// Stock `ProductDataCommon::Instrument` in code order (`0x7ec714`, `0x7ec4c4`): the
/// Preferred Instrument sort compares these codes (`0x755258` role 0x24). The sidecar texts
/// are tetrad-library's `sidecar::Instrument` only; tetrad-app converts (review r30 C6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreferredInstrument {
    #[default]
    NotSet,
    Guitar,
    Bass,
    Synth,
    Vocal,
    Other,
}

impl PreferredInstrument {
    /// The PREFERRED INSTRUMENT buttons in stock order (`0x1561a08`).
    pub const CHOICES: [Self; 5] = [
        Self::Guitar,
        Self::Bass,
        Self::Synth,
        Self::Vocal,
        Self::Other,
    ];

    /// Row subtitle (I: stock shows an instrument icon set, `icons/instruments/*.png`).
    pub fn label(self) -> &'static str {
        match self {
            Self::NotSet => "",
            Self::Guitar => "Guitar",
            Self::Bass => "Bass",
            Self::Synth => "Synth",
            Self::Vocal => "Vocal",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryPlugin {
    pub name: String,
    pub path: String,
    pub read_only: bool,
    pub children: Vec<String>,
    /// Plugin presets (`folder` = folder below the plugin, `value` = `.pb` path).
    pub presets: Vec<DirectoryItem>,
    /// The plugin id of a stock plugin folder name; `None` for any other folder.
    pub plugin: Option<u32>,
}

/// Recents and favorites kept per kind: the stock recents push keeps 64 (`0x7ceb78`).
const DIRECTORY_ITEMS_PER_KIND: usize = 64;

impl DirectoryView {
    /// Bounds what a corrupt or foreign recents, favorites, sidecar or plugin file can put
    /// in the view, so that [`crate::UiState::set_snapshot`] does not reject the whole
    /// snapshot. Pinned capture keys are bounded the same way. Stock loads every valid pair (`0x7d0224`) and a QString has no limit; these
    /// bounds are owned. Recents and favorites keep the first 64 per kind, plugin lists keep
    /// [`MAX_LIST_ITEMS`], an entry with a path over [`MAX_TEXT_BYTES`] is dropped and a
    /// display text is cut to [`MAX_TEXT_BYTES`] at a char boundary.
    /// Whether [`Self::clamp`] would change nothing (it clones once to find out).
    pub fn is_clamped(&self) -> bool {
        let mut copy = self.clone();
        copy.clamp();
        copy == *self
    }

    pub fn clamp(&mut self) {
        fn cut(text: &mut String) {
            text.truncate(text.floor_char_boundary(MAX_TEXT_BYTES));
        }
        let fits = |item: &DirectoryItem| {
            item.folder.len() <= MAX_TEXT_BYTES && item.value.len() <= MAX_TEXT_BYTES
        };
        let cut_item = |item: &mut DirectoryItem| {
            for text in [&mut item.name, &mut item.author, &mut item.date] {
                cut(text);
            }
        };
        for list in [&mut self.favorites, &mut self.recents] {
            let mut kept = [0; 3];
            list.retain(|item| {
                let count = &mut kept[item.kind as usize];
                *count += usize::from(fits(item));
                fits(item) && *count <= DIRECTORY_ITEMS_PER_KIND
            });
            list.iter_mut().for_each(cut_item);
        }
        self.pinned_captures
            .retain(|key| key.len() <= MAX_TEXT_BYTES);
        self.pinned_captures.truncate(MAX_MEDIA_ITEMS);
        for meta in self
            .media
            .iter_mut()
            .filter_map(|media| media.capture.as_mut())
        {
            cut(&mut meta.version);
        }
        self.plugins
            .retain(|plugin| plugin.path.len() <= MAX_TEXT_BYTES);
        self.plugins.truncate(MAX_LIST_ITEMS);
        for plugin in &mut self.plugins {
            cut(&mut plugin.name);
            plugin
                .children
                .retain(|child| child.len() <= MAX_TEXT_BYTES);
            plugin.children.truncate(MAX_LIST_ITEMS);
            plugin.presets.retain(fits);
            plugin.presets.truncate(MAX_LIST_ITEMS);
            plugin.presets.iter_mut().for_each(cut_item);
        }
        for slot in &mut self.slots {
            for text in [&mut slot.title, &mut slot.author, &mut slot.date] {
                cut(text);
            }
        }
        self.presets.truncate(MAX_MEDIA_ITEMS);
        for preset in &mut self.presets {
            cut(&mut preset.title);
        }
        for item in &mut self.media {
            cut(&mut item.author);
            cut(&mut item.date);
        }
    }

    fn validate(&self) -> Result<(), UiError> {
        if self.slots.len() > 256
            || self.media.len() > MAX_MEDIA_ITEMS
            || self.folders.len() > MAX_LIST_ITEMS
            || self.plugins.len() > MAX_LIST_ITEMS
            || self.favorites.len() > 3 * 64
            || self.recents.len() > 3 * 64
            || self.plugins.iter().any(|plugin| {
                plugin.children.len() > MAX_LIST_ITEMS || plugin.presets.len() > MAX_LIST_ITEMS
            })
            || self
                .media
                .iter()
                .any(|item| item.folders.len() > MAX_LIST_ITEMS)
        {
            return Err(UiError::ListTooLong);
        }
        if self.slots.iter().any(|slot| slot.id >= 256) {
            return Err(UiError::InvalidNumber);
        }
        if has_duplicates(self.slots.iter().map(|slot| slot.id)) {
            return Err(UiError::DuplicateId);
        }
        let texts = self
            .slots
            .iter()
            .flat_map(|slot| [&slot.title, &slot.author, &slot.date])
            .chain(
                self.media
                    .iter()
                    .flat_map(|item| [&item.value, &item.author, &item.date]),
            )
            .chain(self.folders.iter().map(|folder| &folder.name))
            .chain(self.favorites.iter().chain(&self.recents).flat_map(|item| {
                [
                    &item.folder,
                    &item.value,
                    &item.name,
                    &item.author,
                    &item.date,
                ]
            }))
            .chain(self.plugins.iter().flat_map(|plugin| {
                [&plugin.name, &plugin.path]
                    .into_iter()
                    .chain(&plugin.children)
                    .chain(
                        plugin
                            .presets
                            .iter()
                            .flat_map(|item| [&item.folder, &item.value, &item.name]),
                    )
            }));
        if texts.into_iter().any(|text| text.len() > MAX_TEXT_BYTES) {
            return Err(UiError::TextTooLong);
        }
        Ok(())
    }
}
