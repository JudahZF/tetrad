//! Owned icons (y grows downward). Line icons use a 10×10 design grid; owned
//! redraws of stock glyph shapes use the stock 24 px grid (see `traced`).

use std::sync::OnceLock;

use crate::device_art;
use crate::raster::{
    Art, Mask, Placement, Shape, a, circle, disc, dot, fill, l, path, pie, rasterize, rect,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Back,
    Forward,
    Up,
    Down,
    Close,
    Plus,
    Minus,
    Check,
    Gear,
    Search,
    Tuner,
    Metronome,
    Looper,
    Wifi,
    /// Owned (Tetrad): Settings › Tetrad › Bluetooth.
    Bluetooth,
    Lock,
    Play,
    Stop,
    Record,
    Undo,
    Redo,
    Trash,
    Copy,
    Paste,
    Pencil,
    Grid,
    Gig,
    Menu,
    Folder,
    Star,
    Info,
    Download,
    Refresh,
    Midi,
    Display,
    Footswitch,
    Pedal,
    InOut,
    Capture,
    Archive,
    Power,
    Shift,
    Backspace,
    Enter,
    Headphones,
    Save,
    /// Vertical ⋮ that opens a row or page menu.
    More,
    // Mode indicator glyphs (stock `navigationBar/modes`); STOMP uses
    // `Footswitch` (a sneaker on a stomp switch).
    ModePreset,
    ModeScene,
    /// Two opposed arrows (swap scene, swap tempo and tuner).
    Swap,
    /// Pencil in an open square (Gig View EDIT marks).
    Edit,
    // Stock 4.1.0 ⋮ menu glyphs (seen on the QC, 2026-10-16): Create New, Delete Preset,
    // CPU Monitor (Settings uses `Gear`).
    Spark,
    TrashCross,
    Chip,
    /// Scene stepper keys: a solid triangle and a dim one (shots 086, 127).
    StepBack,
    StepForward,
    // Settings page glyphs (shots 034, 045, 091).
    Sync,
    Brightness,
    Bolt,
    Knob,
    Storage,
    Factory,
    Timer,
    Clock,
    Device,
    /// Owned: Settings → Appearance.
    Palette,
    // Global EQ band types (TYPE dropdown, shot 009).
    EqPeak,
    EqHighPass,
    EqLowPass,
    EqHighShelf,
    EqLowShelf,
    // Settings Account and Support glyphs (shots 117, 044).
    Cloud,
    Person,
    /// `icons/24/no_signal.png` without its red cross ([`Self::CornerCross`]): six signal bars
    /// (capture browser REFRESH, offline).
    NoSignal,
    /// `icons/24/not_logged_in.png` without its red cross: the person outline right of it
    /// (capture browser REFRESH, online and signed out).
    NotLoggedIn,
    /// The red cross at the top left of `no_signal` and `not_logged_in`.
    CornerCross,
    /// Solid cloud with the sync arrows cut out (Backups).
    CloudSync,
    /// Solid clouds with an arrow cut out (Backups rows, shot 117: upload mark
    /// before the creation date).
    CloudUpload,
    CloudDownload,
    Envelope,
    /// Heartbeat line (About and Contact).
    Pulse,
    /// Solid ⓘ disc (Device Information).
    InfoFilled,
    Bug,
    PlusBold,
    Document,
    /// Solid folder with its tab (capture save page, shot 030).
    FolderFilled,
    /// Grid header save states (shots 013, 025): floppy with an arrow; floppy
    /// with its corner open for `SaveBadge`, a disc with a ✓ cut out that the
    /// caller draws in the accent colour in the same box.
    SaveArrow,
    SaveCheck,
    SaveBadge,
    // Directory glyphs (shots 033, 054).
    /// Solid ▼ of a drop-down.
    Caret,
    Binoculars,
    HeartBroken,
    /// Grid sidechain marks (`icons/24/sidechain_source.png`, `sidechain_sink.png`): a white
    /// rounded square with a dot and a ▶ cut out, ●▶ on a source, ▶● on a sink. Drawn at
    /// [`IconSize::Small`] on the 32 px device grid (`device_art`).
    SidechainSource,
    SidechainSink,
    /// `icons/24/neural_capture.png`, the ⋮ menu "New Neural Capture" glyph (`m3.png`): a 24 px
    /// disc of 2 px stripes every 4 px, bright on the right, dim and offset on the left.
    /// [`Icon::NeuralCapture`] is the 32 px `NC.png` of the picker and tiles.
    NeuralCaptureMenu,
    /// `icons/24/capture_type.png`, the Grid and Gig View capture badge: a 20 px disc of
    /// 2.25 px stripes every 4.5 px, bright on the right, dim and offset on the left. Drawn at
    /// [`IconSize::Medium`] on its own black 24 px disc.
    CaptureType,
    /// `icons/16/bundle_n100.png`, the block editor's Virtual Device Preset match mark after
    /// the model name (`b4-stock1`): a diamond over two chevrons, a stack of layers. Drawn at
    /// [`IconSize::Small`] in the model name colour.
    PresetMatch,
    // Virtual device glyphs: owned redraws of the stock `icons/virtual devices/*.png` set on its
    // 32 px grid (`device_art`; `0x002eb99c` picks one per block). Menus keep `Looper`; Grid
    // Looper blocks use `LooperReels` and Tuner blocks `Utility`.
    Plugin,
    Amp,
    /// `comboamp.png`: Neural Capture device type Combo Amp.
    ComboAmp,
    /// `NC_ampcab.png`: capture type Amp + Cab and model 1184.
    NcAmpCab,
    NeuralCapture,
    /// `NC_pedal.png`: capture type Pedal.
    NcPedal,
    Cab,
    Overdrive,
    /// `fuzz.png`: capture type Fuzz.
    Fuzz,
    Delay,
    Reverb,
    Compressor,
    Pitch,
    /// `multivoicer.png`: model 18012.
    Multivoicer,
    Modulation,
    Morph,
    /// `freeze.png`: model 26001.
    Freeze,
    /// `BitCrusher.png`: models 26003 and 26004.
    BitCrusher,
    Synth,
    Filter,
    Eq,
    IrLoader,
    Wah,
    /// `loop.png`: FX Loop blocks with both send and return.
    FxLoop,
    /// `send.png`: FX Loop send-only blocks.
    FxSend,
    /// `return.png`: FX Loop return-only blocks.
    FxReturn,
    /// `icons/looper/looper_icon.png`: tape reels of the Looper X block.
    LooperReels,
    /// `blend.png`: models 16008 and 16013.
    Blend,
    /// `doubler.png`: models 16011 and 16012.
    Doubler,
    /// `utility.png`: other Utility models and the Tuner category.
    Utility,
    /// `multiple_stomps.png`: a Gig View stomp that controls more than one block.
    MultipleDevices,
}

const ICON_COUNT: usize = Icon::MultipleDevices as usize + 1;

/// The cut top-right corner of the floppy outline: a 2 px band at 45° from the top edge
/// (x 19.1) to the right edge (y 5.9), as `save.png` on `st-mp1c`.
const SAVE_CORNER: Shape = fill(&[
    (17.0, 1.0),
    (19.1, 1.0),
    (24.0, 5.9),
    (24.0, 8.0),
    (22.0, 8.0),
    (22.0, 6.7),
    (18.3, 3.0),
    (17.0, 3.0),
]);

impl Icon {
    fn strokes(self) -> &'static [Shape] {
        match self {
            Self::Back => const { &[l(6.5, 1.5, 3.0, 5.0), l(3.0, 5.0, 6.5, 8.5)] },
            Self::Forward => const { &[l(3.5, 1.5, 7.0, 5.0), l(7.0, 5.0, 3.5, 8.5)] },
            Self::Up => const { &[l(1.5, 6.5, 5.0, 3.0), l(5.0, 3.0, 8.5, 6.5)] },
            Self::Down => const { &[l(1.5, 3.5, 5.0, 7.0), l(5.0, 7.0, 8.5, 3.5)] },
            Self::Close => const { &[l(2.0, 2.0, 8.0, 8.0), l(8.0, 2.0, 2.0, 8.0)] },
            Self::Plus => const { &[l(5.0, 1.5, 5.0, 8.5), l(1.5, 5.0, 8.5, 5.0)] },
            Self::Minus => const { &[l(1.5, 5.0, 8.5, 5.0)] },
            Self::Check => const { &[l(1.5, 5.2, 4.0, 7.8), l(4.0, 7.8, 8.6, 2.4)] },
            Self::Search => const { &[circle(4.2, 4.2, 3.0), l(6.4, 6.4, 9.0, 9.0)] },
            Self::Tuner => {
                const {
                    &[
                        l(3.5, 0.8, 3.5, 4.6),
                        l(6.5, 0.8, 6.5, 4.6),
                        a(5.0, 4.6, 1.5, 0.0, 180.0),
                        l(5.0, 6.1, 5.0, 9.4),
                    ]
                }
            }
            Self::Metronome => {
                const {
                    &[
                        l(2.8, 9.0, 7.2, 9.0),
                        l(2.8, 9.0, 4.2, 1.0),
                        l(7.2, 9.0, 5.8, 1.0),
                        l(4.2, 1.0, 5.8, 1.0),
                        l(5.0, 7.0, 7.8, 2.6),
                    ]
                }
            }
            Self::Looper => {
                const {
                    &[
                        a(5.0, 5.0, 3.5, 30.0, 300.0),
                        l(8.03, 3.25, 8.4, 0.9),
                        l(8.03, 3.25, 5.9, 2.4),
                    ]
                }
            }
            Self::Wifi => {
                const {
                    &[
                        a(5.0, 8.6, 6.8, 225.0, 90.0),
                        a(5.0, 8.6, 4.4, 225.0, 90.0),
                        a(5.0, 8.6, 2.0, 225.0, 90.0),
                        dot(5.0, 8.6),
                    ]
                }
            }
            // The Bluetooth rune: a stem, with the arrow of each half pointing right.
            Self::Bluetooth => {
                const {
                    &[
                        l(5.0, 0.8, 5.0, 9.2),
                        l(5.0, 0.8, 7.4, 3.0),
                        l(7.4, 3.0, 2.6, 7.0),
                        l(5.0, 9.2, 7.4, 7.0),
                        l(7.4, 7.0, 2.6, 3.0),
                    ]
                }
            }
            Self::Lock => {
                const {
                    &[
                        l(2.0, 4.6, 8.0, 4.6),
                        l(8.0, 4.6, 8.0, 9.2),
                        l(8.0, 9.2, 2.0, 9.2),
                        l(2.0, 9.2, 2.0, 4.6),
                        a(5.0, 4.4, 2.2, 180.0, 180.0),
                    ]
                }
            }
            Self::Trash => {
                const {
                    &[
                        l(1.5, 2.6, 8.5, 2.6),
                        l(3.8, 2.6, 4.2, 1.2),
                        l(4.2, 1.2, 5.8, 1.2),
                        l(5.8, 1.2, 6.2, 2.6),
                        l(2.6, 2.6, 3.2, 9.0),
                        l(3.2, 9.0, 6.8, 9.0),
                        l(6.8, 9.0, 7.4, 2.6),
                        l(4.3, 4.4, 4.4, 7.4),
                        l(5.7, 4.4, 5.6, 7.4),
                    ]
                }
            }
            Self::Paste => {
                const {
                    &[
                        l(2.0, 2.2, 8.0, 2.2),
                        l(8.0, 2.2, 8.0, 9.2),
                        l(8.0, 9.2, 2.0, 9.2),
                        l(2.0, 9.2, 2.0, 2.2),
                        l(3.8, 1.0, 6.2, 1.0),
                        l(3.8, 1.0, 3.8, 3.2),
                        l(6.2, 1.0, 6.2, 3.2),
                        l(3.8, 3.2, 6.2, 3.2),
                    ]
                }
            }
            Self::Pencil => {
                const {
                    &[
                        l(1.6, 8.4, 2.2, 6.2),
                        l(1.6, 8.4, 3.8, 7.8),
                        l(2.2, 6.2, 6.8, 1.6),
                        l(3.8, 7.8, 8.4, 3.2),
                        l(6.8, 1.6, 8.4, 3.2),
                    ]
                }
            }
            Self::Grid => {
                const {
                    &[
                        l(1.0, 1.0, 4.4, 1.0),
                        l(4.4, 1.0, 4.4, 4.4),
                        l(4.4, 4.4, 1.0, 4.4),
                        l(1.0, 4.4, 1.0, 1.0),
                        l(5.6, 1.0, 9.0, 1.0),
                        l(9.0, 1.0, 9.0, 4.4),
                        l(9.0, 4.4, 5.6, 4.4),
                        l(5.6, 4.4, 5.6, 1.0),
                        l(1.0, 5.6, 4.4, 5.6),
                        l(4.4, 5.6, 4.4, 9.0),
                        l(4.4, 9.0, 1.0, 9.0),
                        l(1.0, 9.0, 1.0, 5.6),
                        l(5.6, 5.6, 9.0, 5.6),
                        l(9.0, 5.6, 9.0, 9.0),
                        l(9.0, 9.0, 5.6, 9.0),
                        l(5.6, 9.0, 5.6, 5.6),
                    ]
                }
            }
            Self::Gig => {
                const {
                    &[
                        l(1.0, 1.0, 9.0, 1.0),
                        l(9.0, 1.0, 9.0, 4.6),
                        l(9.0, 4.6, 1.0, 4.6),
                        l(1.0, 4.6, 1.0, 1.0),
                        circle(2.2, 7.6, 1.2),
                        circle(5.0, 7.6, 1.2),
                        circle(7.8, 7.6, 1.2),
                    ]
                }
            }
            Self::Menu => {
                const {
                    &[
                        l(1.5, 2.5, 8.5, 2.5),
                        l(1.5, 5.0, 8.5, 5.0),
                        l(1.5, 7.5, 8.5, 7.5),
                    ]
                }
            }
            Self::Folder => {
                const {
                    &[
                        l(1.0, 2.4, 4.0, 2.4),
                        l(4.0, 2.4, 5.0, 3.6),
                        l(5.0, 3.6, 9.0, 3.6),
                        l(9.0, 3.6, 9.0, 8.6),
                        l(9.0, 8.6, 1.0, 8.6),
                        l(1.0, 8.6, 1.0, 2.4),
                    ]
                }
            }
            Self::Star => {
                const {
                    &[
                        l(5.0, 0.8, 6.12, 3.76),
                        l(6.12, 3.76, 9.28, 3.91),
                        l(9.28, 3.91, 6.81, 5.89),
                        l(6.81, 5.89, 7.65, 8.94),
                        l(7.65, 8.94, 5.0, 7.2),
                        l(5.0, 7.2, 2.35, 8.94),
                        l(2.35, 8.94, 3.19, 5.89),
                        l(3.19, 5.89, 0.72, 3.91),
                        l(0.72, 3.91, 3.88, 3.76),
                        l(3.88, 3.76, 5.0, 0.8),
                    ]
                }
            }
            Self::Info => const { &[circle(5.0, 5.0, 4.0), dot(5.0, 3.0), l(5.0, 4.6, 5.0, 7.2)] },
            Self::Download => {
                const {
                    &[
                        l(5.0, 1.0, 5.0, 6.6),
                        l(2.6, 4.4, 5.0, 6.8),
                        l(7.4, 4.4, 5.0, 6.8),
                        l(1.4, 7.2, 1.4, 9.0),
                        l(1.4, 9.0, 8.6, 9.0),
                        l(8.6, 9.0, 8.6, 7.2),
                    ]
                }
            }
            Self::Refresh => {
                const {
                    &[
                        a(5.0, 5.0, 3.6, -40.0, 290.0),
                        l(7.76, 2.69, 8.2, 0.4),
                        l(7.76, 2.69, 5.5, 2.2),
                    ]
                }
            }
            Self::Display => {
                const {
                    &[
                        l(1.0, 1.5, 9.0, 1.5),
                        l(9.0, 1.5, 9.0, 7.0),
                        l(9.0, 7.0, 1.0, 7.0),
                        l(1.0, 7.0, 1.0, 1.5),
                        l(5.0, 7.0, 5.0, 8.6),
                        l(3.0, 8.8, 7.0, 8.8),
                    ]
                }
            }
            Self::Pedal => {
                const {
                    &[
                        l(1.0, 8.8, 9.0, 8.8),
                        l(1.5, 7.6, 8.6, 3.0),
                        l(8.6, 3.0, 8.6, 8.8),
                    ]
                }
            }
            Self::InOut => {
                const {
                    &[
                        l(1.0, 3.4, 7.0, 3.4),
                        l(5.0, 1.4, 7.0, 3.4),
                        l(5.0, 5.4, 7.0, 3.4),
                        l(9.0, 6.6, 3.0, 6.6),
                        l(5.0, 4.6, 3.0, 6.6),
                        l(5.0, 8.6, 3.0, 6.6),
                    ]
                }
            }
            Self::Capture => {
                const {
                    &[
                        l(1.0, 5.0, 2.6, 5.0),
                        l(2.6, 5.0, 3.6, 1.6),
                        l(3.6, 1.6, 5.0, 8.4),
                        l(5.0, 8.4, 6.4, 3.0),
                        l(6.4, 3.0, 7.4, 5.0),
                        l(7.4, 5.0, 9.0, 5.0),
                    ]
                }
            }
            Self::Archive => {
                const {
                    &[
                        l(1.0, 1.6, 9.0, 1.6),
                        l(9.0, 1.6, 9.0, 4.0),
                        l(9.0, 4.0, 1.0, 4.0),
                        l(1.0, 4.0, 1.0, 1.6),
                        l(1.8, 4.0, 1.8, 8.8),
                        l(1.8, 8.8, 8.2, 8.8),
                        l(8.2, 8.8, 8.2, 4.0),
                        l(4.0, 5.8, 6.0, 5.8),
                    ]
                }
            }
            Self::Power => const { &[a(5.0, 5.4, 3.6, 300.0, 300.0), l(5.0, 0.8, 5.0, 5.0)] },
            Self::Shift => {
                const {
                    &[
                        l(5.0, 1.4, 1.4, 5.4),
                        l(1.4, 5.4, 3.4, 5.4),
                        l(3.4, 5.4, 3.4, 8.6),
                        l(3.4, 8.6, 6.6, 8.6),
                        l(6.6, 8.6, 6.6, 5.4),
                        l(6.6, 5.4, 8.6, 5.4),
                        l(8.6, 5.4, 5.0, 1.4),
                    ]
                }
            }
            Self::Backspace => {
                const {
                    &[
                        l(3.4, 2.0, 9.0, 2.0),
                        l(9.0, 2.0, 9.0, 8.0),
                        l(9.0, 8.0, 3.4, 8.0),
                        l(3.4, 8.0, 0.6, 5.0),
                        l(0.6, 5.0, 3.4, 2.0),
                        l(4.8, 3.6, 7.2, 6.4),
                        l(7.2, 3.6, 4.8, 6.4),
                    ]
                }
            }
            Self::Enter => {
                const {
                    &[
                        l(8.4, 1.6, 8.4, 6.0),
                        l(8.4, 6.0, 1.8, 6.0),
                        l(1.8, 6.0, 4.0, 3.8),
                        l(1.8, 6.0, 4.0, 8.2),
                    ]
                }
            }
            Self::Headphones => {
                const {
                    &[
                        a(5.0, 5.6, 3.6, 180.0, 180.0),
                        l(1.4, 5.6, 1.4, 8.6),
                        l(1.4, 8.6, 2.6, 8.6),
                        l(2.6, 8.6, 2.6, 5.6),
                        l(8.6, 5.6, 8.6, 8.6),
                        l(8.6, 8.6, 7.4, 8.6),
                        l(7.4, 8.6, 7.4, 5.6),
                    ]
                }
            }
            Self::Plugin => {
                const {
                    &[
                        l(3.8, 1.0, 3.8, 3.2),
                        l(6.2, 1.0, 6.2, 3.2),
                        l(2.2, 3.2, 7.8, 3.2),
                        l(2.2, 3.2, 2.2, 4.8),
                        l(7.8, 3.2, 7.8, 4.8),
                        a(5.0, 4.8, 2.8, 0.0, 180.0),
                        l(5.0, 7.6, 5.0, 9.2),
                    ]
                }
            }
            Self::Play
            | Self::Stop
            | Self::Record
            | Self::Undo
            | Self::Redo
            | Self::More
            | Self::Midi
            | Self::Footswitch
            | Self::ModePreset
            | Self::ModeScene
            | Self::Swap
            | Self::Edit
            | Self::StepBack
            | Self::StepForward
            | Self::Sync
            | Self::Brightness
            | Self::Bolt
            | Self::Knob
            | Self::Storage
            | Self::Factory
            | Self::Timer
            | Self::Clock
            | Self::Device
            | Self::Palette
            | Self::EqPeak
            | Self::EqHighPass
            | Self::EqLowPass
            | Self::EqHighShelf
            | Self::EqLowShelf
            | Self::Cloud
            | Self::Person
            | Self::NoSignal
            | Self::NotLoggedIn
            | Self::CornerCross
            | Self::CloudSync
            | Self::CloudUpload
            | Self::CloudDownload
            | Self::Envelope
            | Self::Pulse
            | Self::Copy
            | Self::Spark
            | Self::TrashCross
            | Self::Chip
            | Self::Gear
            | Self::InfoFilled
            | Self::Bug
            | Self::PlusBold
            | Self::Document
            | Self::FolderFilled
            | Self::Save
            | Self::SaveArrow
            | Self::SaveCheck
            | Self::SaveBadge
            | Self::Caret
            | Self::Binoculars
            | Self::HeartBroken
            | Self::SidechainSource
            | Self::SidechainSink
            | Self::NeuralCaptureMenu
            | Self::CaptureType
            | Self::PresetMatch
            | Self::Amp
            | Self::ComboAmp
            | Self::NcAmpCab
            | Self::NeuralCapture
            | Self::NcPedal
            | Self::Cab
            | Self::Overdrive
            | Self::Fuzz
            | Self::Delay
            | Self::Reverb
            | Self::Compressor
            | Self::Pitch
            | Self::Multivoicer
            | Self::Modulation
            | Self::Morph
            | Self::Freeze
            | Self::BitCrusher
            | Self::Synth
            | Self::Filter
            | Self::Eq
            | Self::IrLoader
            | Self::Wah
            | Self::FxLoop
            | Self::FxSend
            | Self::FxReturn
            | Self::LooperReels
            | Self::Blend
            | Self::Doubler
            | Self::Utility
            | Self::MultipleDevices => &[],
        }
    }

    /// Owned redraws of stock glyph shapes on the stock 24 px grid, traced
    /// from the manual screenshots (no stock pixels are stored).
    fn traced(self) -> Option<Art> {
        let art = match self {
            Self::Play => Art::ink(const { &[fill(&[(4.5, 2.5), (20.5, 12.0), (4.5, 21.5)])] }),
            Self::Stop => Art::ink(const { &[rect(4.0, 4.0, 20.0, 20.0)] }),
            Self::Record => Art::ink(const { &[disc(12.0, 12.0, 10.0)] }),
            // Circular arrows, open at the side, head at the top (shots 013, 086). Grid header
            // undo (`st-mp1c`): ring x 609–630, y 12–35 from the 24 px box at (608, 12), so the
            // ring is 22 px wide (radius 10 on the line) and the head tip is at x 615.
            Self::Undo => Art::ink(
                const {
                    &[
                        a(12.0, 13.0, 10.0, 180.0, -270.0),
                        fill(&[(6.8, 3.5), (13.0, 0.3), (13.0, 6.7)]),
                    ]
                },
            ),
            Self::Redo => Art::ink(
                const {
                    &[
                        a(12.0, 13.0, 10.0, 0.0, 270.0),
                        fill(&[(17.2, 3.5), (11.0, 0.3), (11.0, 6.7)]),
                    ]
                },
            ),
            // Grid header ⋮ (`st-mp1c`): three 3 px dots at x 764, y 15, 23 and 31 from the
            // 24 px box at (753, 12).
            Self::More => Art::ink(
                const {
                    &[
                        disc(11.5, 3.5, 1.6),
                        disc(11.5, 11.5, 1.6),
                        disc(11.5, 19.5, 1.6),
                    ]
                },
            ),
            Self::StepBack => Art {
                ink: const { &[fill(&[(1.5, 12.0), (12.5, 2.5), (12.5, 21.5)])] },
                dim: const {
                    &[fill(&[
                        (14.4, 9.5),
                        (22.5, 2.5),
                        (22.5, 21.5),
                        (14.4, 14.5),
                    ])]
                },
                tone: 0.25,
                cut: &[],
            },
            Self::StepForward => Art {
                ink: const { &[fill(&[(22.5, 12.0), (11.5, 2.5), (11.5, 21.5)])] },
                dim: const { &[fill(&[(9.6, 9.5), (1.5, 2.5), (1.5, 21.5), (9.6, 14.5)])] },
                tone: 0.25,
                cut: &[],
            },
            // Three rows of joined blocks; a cable drops from row 1 to row 2.
            Self::ModePreset => Art::ink(
                const {
                    &[
                        rect(0.0, 0.0, 6.0, 6.0),
                        rect(9.0, 0.0, 15.0, 6.0),
                        rect(18.0, 0.0, 24.0, 6.0),
                        rect(0.0, 2.0, 24.0, 4.0),
                        rect(2.0, 6.0, 4.0, 13.0),
                        rect(9.0, 9.0, 15.0, 15.0),
                        rect(18.0, 9.0, 24.0, 15.0),
                        rect(2.0, 11.0, 24.0, 13.0),
                        rect(0.0, 18.0, 6.0, 24.0),
                        rect(9.0, 18.0, 15.0, 24.0),
                        rect(18.0, 18.0, 24.0, 24.0),
                        rect(0.0, 20.0, 24.0, 22.0),
                    ]
                },
            ),
            // Four solid squares with the scene letters cut out.
            Self::ModeScene => Art {
                ink: const {
                    &[
                        rect(0.0, 0.0, 11.0, 11.0),
                        rect(13.0, 0.0, 24.0, 11.0),
                        rect(0.0, 13.0, 11.0, 24.0),
                        rect(13.0, 13.0, 24.0, 24.0),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[
                        l(2.6, 8.6, 5.5, 2.4),
                        l(5.5, 2.4, 8.4, 8.6),
                        l(3.8, 6.6, 7.2, 6.6),
                        l(16.0, 2.5, 16.0, 8.5),
                        l(16.0, 2.5, 18.8, 2.5),
                        a(18.8, 4.0, 1.5, 270.0, 180.0),
                        l(16.0, 5.5, 19.0, 5.5),
                        a(19.0, 7.0, 1.5, 270.0, 180.0),
                        l(19.0, 8.5, 16.0, 8.5),
                        a(5.6, 18.5, 3.0, -45.0, -270.0),
                        l(16.0, 15.5, 16.0, 21.5),
                        l(16.0, 15.5, 17.8, 15.5),
                        a(17.8, 18.5, 3.0, 270.0, 180.0),
                        l(17.8, 21.5, 16.0, 21.5),
                    ]
                },
            },
            // `navigationBar/modes/stomp.png`: a sneaker on a stomp switch. The upper, the
            // sole stripe and the heel strap are separate fills; the dark gaps between them
            // draw the outline (stock draws the 24 px PNG 1:1).
            Self::Footswitch => Art::ink(
                const {
                    &[
                        fill(&[
                            (1.0, 12.0),
                            (12.0, 4.0),
                            (13.0, 6.0),
                            (16.0, 7.0),
                            (20.0, 3.0),
                            (21.0, 3.0),
                            (21.0, 4.0),
                            (19.0, 7.0),
                            (18.0, 9.0),
                            (9.0, 12.0),
                            (1.0, 14.0),
                        ]),
                        // Sole stripe, from the toe up to the heel.
                        fill(&[
                            (1.0, 15.0),
                            (23.0, 9.0),
                            (23.0, 10.0),
                            (17.0, 12.0),
                            (2.0, 16.0),
                            (1.0, 16.0),
                        ]),
                        // Heel strap.
                        fill(&[(21.0, 5.0), (23.0, 8.0), (19.0, 9.0)]),
                        // Switch cap with a slanted top, on its base.
                        fill(&[(6.0, 17.0), (10.0, 16.0), (10.0, 20.0), (6.0, 20.0)]),
                        rect(3.0, 20.0, 13.0, 24.0),
                    ]
                },
            ),
            Self::Swap => Art::ink(
                const {
                    &[
                        l(1.5, 7.0, 21.5, 7.0),
                        l(17.0, 2.5, 21.5, 7.0),
                        l(21.5, 7.0, 17.0, 11.5),
                        l(2.5, 17.0, 22.5, 17.0),
                        l(7.0, 12.5, 2.5, 17.0),
                        l(2.5, 17.0, 7.0, 21.5),
                    ]
                },
            ),
            Self::Edit => Art::ink(
                const {
                    &[
                        l(5.0, 3.0, 10.5, 3.0),
                        a(5.0, 5.5, 2.5, 270.0, -90.0),
                        l(2.5, 5.5, 2.5, 20.5),
                        a(5.0, 20.5, 2.5, 180.0, -90.0),
                        l(5.0, 23.0, 20.0, 23.0),
                        a(20.0, 20.5, 2.5, 90.0, -90.0),
                        l(22.5, 20.5, 22.5, 15.0),
                        fill(&[(10.4, 13.9), (18.9, 5.4), (22.1, 8.6), (13.6, 17.1)]),
                        fill(&[(9.55, 14.75), (12.75, 17.95), (8.9, 18.6)]),
                    ]
                },
            ),
            // Two rounded squares: the front one whole, the back one's top and right edges
            // (⋮ menu Copy Scene, seen on the QC 2026-10-16).
            Self::Copy => Art::ink(
                const {
                    &[
                        l(5.5, 8.0, 12.5, 8.0),
                        a(12.5, 11.5, 3.5, 270.0, 90.0),
                        l(16.0, 11.5, 16.0, 18.5),
                        a(12.5, 18.5, 3.5, 0.0, 90.0),
                        l(12.5, 22.0, 5.5, 22.0),
                        a(5.5, 18.5, 3.5, 90.0, 90.0),
                        l(2.0, 18.5, 2.0, 11.5),
                        a(5.5, 11.5, 3.5, 180.0, 90.0),
                        a(11.5, 5.5, 3.5, 180.0, 90.0),
                        l(11.5, 2.0, 18.5, 2.0),
                        a(18.5, 5.5, 3.5, 270.0, 90.0),
                        l(22.0, 5.5, 22.0, 11.5),
                        a(18.5, 11.5, 3.5, 0.0, 90.0),
                    ]
                },
            ),
            // A vertical bar and four arms ending in blobs around a solid centre.
            Self::Spark => Art::ink(
                const {
                    &[
                        l(12.0, 2.0, 12.0, 22.0),
                        disc(12.0, 2.0, 1.9),
                        disc(12.0, 22.0, 1.9),
                        l(3.0, 7.0, 21.0, 17.0),
                        l(21.0, 7.0, 3.0, 17.0),
                        disc(3.0, 7.0, 2.3),
                        disc(21.0, 7.0, 2.3),
                        disc(3.0, 17.0, 2.3),
                        disc(21.0, 17.0, 2.3),
                        disc(12.0, 12.0, 4.3),
                    ]
                },
            ),
            // Lid bar with a handle over a round-bottomed bin with a ×.
            Self::TrashCross => Art::ink(
                const {
                    &[
                        rect(9.0, 0.0, 15.0, 2.0),
                        rect(2.0, 2.0, 22.0, 4.0),
                        path(&[(7.0, 23.0), (17.0, 23.0)]),
                        path(&[(4.0, 20.0), (4.0, 7.0), (20.0, 7.0), (20.0, 20.0)]),
                        a(7.0, 20.0, 3.0, 180.0, -90.0),
                        a(17.0, 20.0, 3.0, 90.0, -90.0),
                        l(9.5, 12.5, 14.5, 17.5),
                        l(14.5, 12.5, 9.5, 17.5),
                    ]
                },
            ),
            // Rounded package with two pins per side and a square die.
            Self::Chip => Art::ink(
                const {
                    &[
                        l(6.0, 3.0, 18.0, 3.0),
                        a(18.0, 6.0, 3.0, 270.0, 90.0),
                        l(21.0, 6.0, 21.0, 18.0),
                        a(18.0, 18.0, 3.0, 0.0, 90.0),
                        l(18.0, 21.0, 6.0, 21.0),
                        a(6.0, 18.0, 3.0, 90.0, 90.0),
                        l(3.0, 18.0, 3.0, 6.0),
                        a(6.0, 6.0, 3.0, 180.0, 90.0),
                        rect(7.0, 0.0, 9.0, 3.0),
                        rect(15.0, 0.0, 17.0, 3.0),
                        rect(7.0, 21.0, 9.0, 24.0),
                        rect(15.0, 21.0, 17.0, 24.0),
                        rect(0.0, 7.0, 3.0, 9.0),
                        rect(0.0, 15.0, 3.0, 17.0),
                        rect(21.0, 7.0, 24.0, 9.0),
                        rect(21.0, 15.0, 24.0, 17.0),
                        path(&[
                            (8.0, 8.0),
                            (16.0, 8.0),
                            (16.0, 16.0),
                            (8.0, 16.0),
                            (8.0, 8.0),
                        ]),
                    ]
                },
            ),
            // Seven-tooth cog outline with a ring hole (⋮ menu and Settings, seen on the QC).
            Self::Gear => Art::ink(
                const {
                    &[
                        path(&[
                            (10.3, 1.5),
                            (13.7, 1.5),
                            (15.4, 4.9),
                            (19.2, 4.2),
                            (21.2, 6.8),
                            (19.7, 10.2),
                            (22.6, 12.7),
                            (21.8, 15.9),
                            (18.2, 16.9),
                            (18.0, 20.7),
                            (15.0, 22.2),
                            (12.0, 19.9),
                            (9.0, 22.2),
                            (6.0, 20.7),
                            (5.8, 16.9),
                            (2.2, 15.9),
                            (1.4, 12.7),
                            (4.3, 10.2),
                            (2.8, 6.8),
                            (4.8, 4.2),
                            (8.6, 4.9),
                            (10.3, 1.5),
                        ]),
                        circle(12.0, 12.0, 3.2),
                    ]
                },
            ),
            // Two arcs chasing each other (Updates).
            Self::Sync => Art::ink(
                const {
                    &[
                        a(12.0, 12.25, 8.75, -85.0, 125.0),
                        a(12.0, 12.25, 8.75, 95.0, 125.0),
                        fill(&[(8.5, 3.5), (14.5, 0.5), (14.5, 6.5)]),
                        fill(&[(15.5, 21.0), (9.5, 24.0), (9.5, 18.0)]),
                    ]
                },
            ),
            Self::Brightness => Art::ink(
                const {
                    &[
                        circle(12.0, 12.0, 3.0),
                        l(12.0, 1.5, 12.0, 4.5),
                        l(12.0, 19.5, 12.0, 22.5),
                        l(1.5, 12.0, 4.5, 12.0),
                        l(19.5, 12.0, 22.5, 12.0),
                        l(7.76, 7.76, 5.85, 5.85),
                        l(16.24, 7.76, 18.15, 5.85),
                        l(7.76, 16.24, 5.85, 18.15),
                        l(16.24, 16.24, 18.15, 18.15),
                    ]
                },
            ),
            // A painter's palette: a ring with four paint dots (owned, Appearance).
            Self::Palette => Art::ink(
                const {
                    &[
                        circle(12.0, 12.0, 10.0),
                        disc(7.5, 9.0, 1.8),
                        disc(11.5, 6.5, 1.8),
                        disc(16.0, 8.5, 1.8),
                        disc(16.5, 13.5, 1.8),
                    ]
                },
            ),
            // Lightning bolt cut from a rounded tile (Power Functions).
            Self::Bolt => Art {
                ink: const {
                    &[
                        rect(3.0, 0.5, 21.0, 23.5),
                        rect(0.5, 3.0, 23.5, 21.0),
                        disc(3.0, 3.0, 2.5),
                        disc(21.0, 3.0, 2.5),
                        disc(3.0, 21.0, 2.5),
                        disc(21.0, 21.0, 2.5),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[fill(&[
                        (16.5, 2.0),
                        (7.0, 12.0),
                        (11.5, 12.0),
                        (8.0, 21.5),
                        (17.8, 11.6),
                        (13.2, 11.6),
                    ])]
                },
            },
            // Knob with its pointer cut out inside a dim travel arc (Master Volume).
            Self::Knob => Art {
                ink: const { &[disc(12.5, 11.75, 8.0)] },
                dim: const { &[a(12.5, 11.75, 11.0, 141.0, 173.0)] },
                tone: 0.5,
                cut: const { &[l(13.0, 11.25, 19.5, 4.75)] },
            },
            // Pie with a dim free quarter (Device Storage).
            Self::Storage => Art {
                ink: const { &[pie(11.75, 11.75, 11.5, 0.0, 270.0)] },
                dim: const { &[pie(11.75, 11.75, 11.5, 270.0, 90.0)] },
                tone: 0.5,
                cut: const { &[l(11.75, 11.75, 11.75, 0.0), l(11.75, 11.75, 24.0, 11.75)] },
            },
            // Saw-tooth roof factory with three windows (Factory Reset).
            Self::Factory => Art {
                ink: const {
                    &[
                        rect(0.0, 2.0, 6.0, 23.0),
                        rect(0.0, 10.0, 24.0, 23.0),
                        fill(&[(6.0, 10.0), (12.0, 6.0), (12.0, 10.0)]),
                        fill(&[(12.0, 10.0), (18.0, 6.0), (18.0, 10.0)]),
                        fill(&[(18.0, 10.0), (24.0, 6.0), (24.0, 10.0)]),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[
                        rect(6.0, 14.0, 9.0, 18.0),
                        rect(12.0, 14.0, 15.0, 18.0),
                        rect(18.0, 14.0, 21.0, 18.0),
                    ]
                },
            },
            // Stopwatch with a filled quarter (Hold Timing).
            Self::Timer => Art::ink(
                const {
                    &[
                        circle(12.0, 12.75, 9.0),
                        rect(11.0, 0.5, 13.0, 3.0),
                        pie(12.0, 12.75, 7.5, 270.0, 90.0),
                    ]
                },
            ),
            Self::Clock => Art::ink(
                const {
                    &[
                        circle(12.0, 11.75, 9.75),
                        l(12.0, 6.0, 12.0, 11.75),
                        l(12.0, 11.75, 18.0, 14.75),
                    ]
                },
            ),
            // Front panel: screen and two rows of switches (Device group).
            Self::Device => Art::ink(
                const {
                    &[
                        l(4.5, 1.5, 19.5, 1.5),
                        l(22.5, 4.5, 22.5, 19.5),
                        l(19.5, 22.5, 4.5, 22.5),
                        l(1.5, 19.5, 1.5, 4.5),
                        a(4.5, 4.5, 3.0, 180.0, 90.0),
                        a(19.5, 4.5, 3.0, 270.0, 90.0),
                        a(19.5, 19.5, 3.0, 0.0, 90.0),
                        a(4.5, 19.5, 3.0, 90.0, 90.0),
                        rect(7.0, 6.0, 17.0, 11.0),
                        rect(5.0, 14.0, 7.0, 16.0),
                        rect(11.0, 14.0, 13.0, 16.0),
                        rect(17.0, 14.0, 19.0, 16.0),
                        rect(5.0, 19.0, 7.0, 21.0),
                        rect(11.0, 19.0, 13.0, 21.0),
                        rect(17.0, 19.0, 19.0, 21.0),
                    ]
                },
            ),
            // Five-pin DIN socket (`icons/mainMenuPopup/midi.png`; `sn-0`: the side pins sit
            // 1 px below the centre, about 97° from the top pin).
            Self::Midi => Art {
                ink: const { &[disc(12.0, 11.0, 10.5)] },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[
                        disc(4.75, 12.0, 1.5),
                        disc(6.87, 5.87, 1.5),
                        disc(12.0, 3.75, 1.5),
                        disc(17.13, 5.87, 1.5),
                        disc(19.25, 12.0, 1.5),
                        rect(9.5, 19.25, 14.5, 22.0),
                    ]
                },
            },
            Self::EqPeak => Art::ink(
                const {
                    &[
                        l(2.0, 17.0, 7.0, 17.0),
                        a(12.0, 17.0, 5.0, 180.0, 180.0),
                        l(17.0, 17.0, 22.0, 17.0),
                    ]
                },
            ),
            Self::EqHighPass => Art::ink(
                const {
                    &[
                        l(3.0, 19.0, 3.0, 15.0),
                        a(9.0, 15.0, 6.0, 180.0, 90.0),
                        l(9.0, 9.0, 22.0, 9.0),
                    ]
                },
            ),
            Self::EqLowPass => Art::ink(
                const {
                    &[
                        l(2.0, 9.0, 15.0, 9.0),
                        a(15.0, 15.0, 6.0, 270.0, 90.0),
                        l(21.0, 15.0, 21.0, 19.0),
                    ]
                },
            ),
            Self::EqHighShelf => Art::ink(
                const {
                    &[
                        l(2.0, 16.0, 8.0, 16.0),
                        l(8.0, 16.0, 16.0, 8.0),
                        l(16.0, 8.0, 22.0, 8.0),
                    ]
                },
            ),
            Self::EqLowShelf => Art::ink(
                const {
                    &[
                        l(2.0, 8.0, 8.0, 8.0),
                        l(8.0, 8.0, 16.0, 16.0),
                        l(16.0, 16.0, 22.0, 16.0),
                    ]
                },
            ),
            // Account (shot 117): three lobes over a flat base.
            Self::Cloud => Art::ink(
                const {
                    &[
                        a(12.0, 9.0, 6.0, 170.0, 200.0),
                        a(7.0, 14.5, 5.0, 90.0, 175.0),
                        a(17.0, 14.5, 5.0, 275.0, 175.0),
                        l(7.0, 19.5, 17.0, 19.5),
                    ]
                },
            ),
            Self::Person => Art::ink(
                const {
                    &[
                        circle(12.0, 5.5, 3.5),
                        a(12.0, 18.5, 7.0, 180.0, 180.0),
                        l(5.0, 18.5, 5.0, 21.5),
                        l(19.0, 18.5, 19.0, 21.5),
                    ]
                },
            ),
            // Measured from the stock 24 px PNGs: 2 px bars at x 1, 5, …, 21 ending at y 22.
            Self::NoSignal => Art::ink(
                const {
                    &[
                        rect(1.0, 18.0, 3.0, 22.0),
                        rect(5.0, 16.0, 7.0, 22.0),
                        rect(9.0, 12.0, 11.0, 22.0),
                        rect(13.0, 8.0, 15.0, 22.0),
                        rect(17.0, 6.0, 19.0, 22.0),
                        rect(21.0, 2.0, 23.0, 22.0),
                    ]
                },
            ),
            // Head ring x 10–19, y 1–10; shoulders x 6–24 from y 11 to 22.
            Self::NotLoggedIn => Art::ink(
                const {
                    &[
                        circle(15.0, 5.75, 3.9),
                        a(15.0, 18.5, 7.85, 180.0, 180.0),
                        l(7.15, 18.5, 7.15, 21.0),
                        l(22.85, 18.5, 22.85, 21.0),
                    ]
                },
            ),
            // The cross spans x 0–9, y 0–9 in both stock icons.
            Self::CornerCross => {
                Art::ink(const { &[l(1.6, 1.6, 8.4, 8.4), l(8.4, 1.6, 1.6, 8.4)] })
            }
            Self::CloudSync => Art {
                ink: const {
                    &[
                        disc(12.0, 9.0, 7.0),
                        disc(7.0, 14.5, 6.0),
                        disc(17.0, 14.5, 6.0),
                        rect(7.0, 14.5, 17.0, 20.5),
                    ]
                },
                dim: &[],
                tone: 0.0,
                // `Sync` at half size.
                cut: const {
                    &[
                        a(11.0, 11.6, 4.4, -85.0, 125.0),
                        a(11.0, 11.6, 4.4, 95.0, 125.0),
                        fill(&[(9.25, 7.1), (12.25, 5.6), (12.25, 8.6)]),
                        fill(&[(12.75, 15.9), (9.75, 17.4), (9.75, 14.4)]),
                    ]
                },
            },
            Self::CloudUpload => Art {
                ink: const {
                    &[
                        disc(12.0, 9.0, 7.0),
                        disc(7.0, 14.5, 6.0),
                        disc(17.0, 14.5, 6.0),
                        rect(7.0, 14.5, 17.0, 20.5),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[
                        l(12.0, 12.0, 12.0, 21.0),
                        fill(&[(7.5, 14.0), (12.0, 8.5), (16.5, 14.0)]),
                    ]
                },
            },
            Self::CloudDownload => Art {
                ink: const {
                    &[
                        disc(12.0, 9.0, 7.0),
                        disc(7.0, 14.5, 6.0),
                        disc(17.0, 14.5, 6.0),
                        rect(7.0, 14.5, 17.0, 20.5),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[
                        l(12.0, 7.5, 12.0, 14.0),
                        fill(&[(7.5, 13.0), (12.0, 18.5), (16.5, 13.0)]),
                    ]
                },
            },
            // Support (shot 044).
            Self::Envelope => Art::ink(
                const {
                    &[
                        l(2.0, 3.0, 22.0, 3.0),
                        l(22.0, 3.0, 22.0, 21.0),
                        l(22.0, 21.0, 2.0, 21.0),
                        l(2.0, 21.0, 2.0, 3.0),
                        path(&[(2.5, 3.5), (12.0, 12.5), (21.5, 3.5)]),
                    ]
                },
            ),
            Self::Pulse => Art::ink(
                const {
                    &[path(&[
                        (3.5, 12.0),
                        (6.0, 12.0),
                        (9.0, 3.0),
                        (14.5, 21.0),
                        (18.0, 12.0),
                        (20.5, 12.0),
                    ])]
                },
            ),
            Self::InfoFilled => Art {
                ink: const { &[disc(12.0, 12.0, 10.5)] },
                dim: &[],
                tone: 0.0,
                cut: const { &[rect(11.0, 5.5, 13.0, 7.5), rect(11.0, 10.5, 13.0, 19.5)] },
            },
            Self::Bug => Art::ink(
                const {
                    &[
                        l(6.5, 2.0, 9.5, 4.5),
                        l(17.5, 2.0, 14.5, 4.5),
                        a(12.0, 8.5, 3.0, 180.0, 180.0),
                        l(9.0, 8.5, 15.0, 8.5),
                        circle(12.0, 15.5, 5.2),
                        l(4.5, 9.5, 7.5, 11.5),
                        l(19.5, 9.5, 16.5, 11.5),
                        l(3.5, 15.0, 6.5, 15.0),
                        l(20.5, 15.0, 17.5, 15.0),
                        l(4.5, 20.5, 8.0, 18.0),
                        l(19.5, 20.5, 16.0, 18.0),
                    ]
                },
            ),
            Self::PlusBold => {
                Art::ink(const { &[rect(4.0, 10.0, 20.0, 14.0), rect(10.0, 4.0, 14.0, 20.0)] })
            }
            // Page with a folded corner and three text lines cut out (Licenses).
            Self::Document => Art {
                ink: const {
                    &[fill(&[
                        (5.0, 2.0),
                        (14.5, 2.0),
                        (21.0, 8.5),
                        (21.0, 22.0),
                        (5.0, 22.0),
                    ])]
                },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[
                        rect(7.0, 10.0, 12.0, 12.0),
                        rect(7.0, 14.0, 16.0, 16.0),
                        rect(7.0, 18.0, 19.0, 20.0),
                    ]
                },
            },
            Self::FolderFilled => Art::ink(
                const {
                    &[fill(&[
                        (2.5, 4.0),
                        (9.0, 4.0),
                        (11.0, 6.5),
                        (21.5, 6.5),
                        (21.5, 20.0),
                        (2.5, 20.0),
                    ])]
                },
            ),
            // Floppy: clipped top-right corner, shutter with its slot (shot 076).
            // Grid header `save.png` (`st-mp1c`, box at (703, 12)): a crisp 2 px outline over
            // x 2–24, y 1–23 with the top-right corner cut at 45°, and the label window
            // x 6–17, y 5–11 with a notch. The outline is filled boxes, not strokes: a 2 px
            // stroke on the hinted pixel centre gives grey edges.
            Self::Save => Art {
                ink: const {
                    &[
                        rect(2.0, 1.0, 4.0, 23.0),
                        rect(2.0, 1.0, 17.5, 3.0),
                        SAVE_CORNER,
                        rect(22.0, 6.0, 24.0, 23.0),
                        rect(2.0, 21.0, 24.0, 23.0),
                        rect(6.0, 5.0, 17.0, 11.0),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const { &[rect(12.0, 5.0, 14.0, 8.0)] },
            },
            // The floppy opens at the bottom-right for an arrow (shot 013).
            Self::SaveArrow => Art {
                ink: const {
                    &[
                        rect(2.0, 1.0, 4.0, 23.0),
                        rect(2.0, 1.0, 17.5, 3.0),
                        SAVE_CORNER,
                        rect(22.0, 6.0, 24.0, 11.0),
                        rect(2.0, 21.0, 11.0, 23.0),
                        rect(6.0, 5.0, 17.0, 11.0),
                        l(10.5, 18.5, 22.5, 18.5),
                        path(&[(17.0, 13.0), (22.5, 18.5), (17.0, 24.0)]),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const { &[rect(12.0, 5.0, 14.0, 8.0)] },
            },
            // Shot 025: the badge sits in the open corner.
            Self::SaveCheck => Art {
                ink: const {
                    &[
                        rect(2.0, 1.0, 4.0, 23.0),
                        rect(2.0, 1.0, 17.5, 3.0),
                        SAVE_CORNER,
                        rect(22.0, 6.0, 24.0, 11.0),
                        rect(2.0, 21.0, 10.0, 23.0),
                        rect(6.0, 5.0, 17.0, 11.0),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const { &[rect(12.0, 5.0, 14.0, 8.0)] },
            },
            Self::SaveBadge => Art {
                ink: const { &[disc(17.0, 18.0, 6.0)] },
                dim: &[],
                tone: 0.0,
                cut: const { &[path(&[(14.0, 18.0), (16.0, 20.0), (19.8, 16.0)])] },
            },
            Self::Caret => Art::ink(const { &[fill(&[(6.0, 8.0), (18.0, 8.0), (12.0, 16.5)])] }),
            // Solid body over two lens rings with a glint in each (shot 033).
            Self::Binoculars => Art::ink(
                const {
                    &[
                        pie(7.25, 8.0, 4.0, 180.0, 180.0),
                        pie(16.75, 8.0, 4.0, 180.0, 180.0),
                        rect(2.5, 8.0, 21.5, 10.0),
                        rect(10.0, 8.0, 14.0, 16.5),
                        circle(6.0, 14.5, 4.5),
                        circle(18.0, 14.5, 4.5),
                        a(6.0, 14.5, 2.2, 195.0, 75.0),
                        a(18.0, 14.5, 2.2, 195.0, 75.0),
                    ]
                },
            ),
            // Solid heart split by a lightning crack (shot 033).
            // Measured on `m3.png`: right rows 4k, 4k + 1 lit; left rows 4k + 2, 4k + 3 at
            // about 23 % (57 and 82 on the #101010 menu).
            Self::NeuralCaptureMenu => Art {
                ink: const { &[pie(12.0, 12.0, 12.0, 270.0, 180.0)] },
                dim: const { &[pie(12.0, 12.0, 12.0, 90.0, 180.0)] },
                tone: 0.23,
                cut: const {
                    &[
                        rect(12.0, 2.0, 24.0, 4.0),
                        rect(12.0, 6.0, 24.0, 8.0),
                        rect(12.0, 10.0, 24.0, 12.0),
                        rect(12.0, 14.0, 24.0, 16.0),
                        rect(12.0, 18.0, 24.0, 20.0),
                        rect(12.0, 22.0, 24.0, 24.0),
                        rect(0.0, 0.0, 12.0, 2.0),
                        rect(0.0, 4.0, 12.0, 6.0),
                        rect(0.0, 8.0, 12.0, 10.0),
                        rect(0.0, 12.0, 12.0, 14.0),
                        rect(0.0, 16.0, 12.0, 18.0),
                        rect(0.0, 20.0, 12.0, 22.0),
                    ]
                },
            },
            // Measured on the stock png: right rows 4.5k – 4.5k + 2.25 lit (k = 1..4), left
            // rows 2.25 later at 82/255; the stripes stop on a radius 10 disc.
            Self::CaptureType => Art {
                ink: const { &[pie(12.0, 12.0, 10.0, 270.0, 180.0)] },
                dim: const { &[pie(12.0, 12.0, 10.0, 90.0, 180.0)] },
                tone: 0.32,
                cut: const {
                    &[
                        rect(12.0, 0.0, 24.0, 4.5),
                        rect(12.0, 6.75, 24.0, 9.0),
                        rect(12.0, 11.25, 24.0, 13.5),
                        rect(12.0, 15.75, 24.0, 18.0),
                        rect(12.0, 20.25, 24.0, 24.0),
                        rect(0.0, 0.0, 12.0, 2.25),
                        rect(0.0, 4.5, 12.0, 6.75),
                        rect(0.0, 9.0, 12.0, 11.25),
                        rect(0.0, 13.5, 12.0, 15.75),
                        rect(0.0, 18.0, 12.0, 20.25),
                        rect(0.0, 22.5, 12.0, 24.0),
                    ]
                },
            },
            // Stock 16 px shape at 1.5×: the diamond (8,0)-(16,4)-(8,8)-(0,4), then each chevron
            // is that diamond moved down 4 or 8 px less the diamond moved down 2 or 6 px. The
            // stock alpha edges (64, 191) are this area's exact coverage.
            Self::PresetMatch => Art::ink(
                const {
                    &[
                        fill(&[(12.0, 0.0), (24.0, 6.0), (12.0, 12.0), (0.0, 6.0)]),
                        fill(&[
                            (0.0, 12.0),
                            (3.0, 10.5),
                            (12.0, 15.0),
                            (21.0, 10.5),
                            (24.0, 12.0),
                            (12.0, 18.0),
                        ]),
                        fill(&[
                            (0.0, 18.0),
                            (3.0, 16.5),
                            (12.0, 21.0),
                            (21.0, 16.5),
                            (24.0, 18.0),
                            (12.0, 24.0),
                        ]),
                    ]
                },
            ),
            Self::HeartBroken => Art {
                ink: const {
                    &[
                        disc(6.25, 7.5, 5.75),
                        disc(17.75, 7.5, 5.75),
                        fill(&[(1.3, 10.4), (12.0, 21.5), (22.7, 10.4), (12.0, 7.5)]),
                    ]
                },
                dim: &[],
                tone: 0.0,
                cut: const {
                    &[fill(&[
                        (11.5, 1.0),
                        (13.5, 1.0),
                        (13.5, 8.0),
                        (16.5, 10.0),
                        (15.5, 13.5),
                        (13.5, 21.5),
                        (11.5, 21.5),
                        (13.5, 12.2),
                        (10.0, 13.8),
                        (9.5, 10.0),
                        (11.5, 8.0),
                    ])]
                },
            },
            _ => return None,
        };
        Some(art)
    }
}

/// Icon box sizes in pixels. Stock icon folders are 16, 24 and 32 px
/// (`icons/<px>/`; Grid block glyphs are 32, the device set); 40 (Gig View stomp
/// and picker glyphs) and 44 are owned sizes for large tiles, 20 for small marks
/// (capture badge, shot 030; Backups row marks, shot 117).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconSize {
    Small,
    Medium,
    Large,
    XLarge,
    Tile,
    Compact,
}

const SIZE_COUNT: usize = IconSize::Compact as usize + 1;

impl IconSize {
    pub const fn px(self) -> u32 {
        match self {
            Self::Small => 16,
            Self::Medium => 24,
            Self::Large => 32,
            Self::XLarge => 44,
            Self::Tile => 40,
            Self::Compact => 20,
        }
    }
}

static CACHE: [[OnceLock<Mask>; ICON_COUNT]; SIZE_COUNT] =
    [const { [const { OnceLock::new() }; ICON_COUNT] }; SIZE_COUNT];

/// Coverage mask placed relative to the top-left of a `px × px` box.
pub fn icon(icon: Icon, size: IconSize) -> &'static Mask {
    CACHE[size as usize][icon as usize].get_or_init(|| rasterize_for(icon, size))
}

fn rasterize_for(icon: Icon, size: IconSize) -> Mask {
    let px = size.px() as f32;
    // Stock glyphs keep a thin ~2 px line at every size (shot 013).
    let (art, grid, radius) = match (device_art::art(icon), icon.traced()) {
        // The stock device set draws 2 px lines on its 32 px grid; they scale with the box.
        (Some(art), _) => (art, device_art::GRID, px / device_art::GRID),
        (None, Some(art)) => (art, 24.0, (px * 0.045).clamp(0.75, 1.3)),
        (None, None) => (
            Art::ink(icon.strokes()),
            10.0,
            (px * 0.045).clamp(0.75, 1.3),
        ),
    };
    rasterize(
        &art,
        Placement {
            scale: px / grid,
            offset_x: 0.0,
            offset_y: 0.0,
            radius,
            // Hinting keeps thin glyphs crisp instead of grey. `capture_type.png` has its
            // stripe edges on quarter pixels at its own size, so it keeps them.
            hint: radius < 1.5 && icon != Icon::CaptureType,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first draw of a device glyph is cheap. When every shape was evaluated at every
    /// pixel, the first Grid frame of Factory 1A took 3.4 s on the QC (Cortex-A5, r49).
    /// Distance terms are a deterministic stand-in for time.
    #[test]
    fn device_glyph_rasterisation_work_is_bounded() {
        let terms = || crate::raster::TERMS.with(std::cell::Cell::get);
        let before = terms();
        for entry in ALL
            .into_iter()
            .filter(|&entry| device_art::art(entry).is_some())
        {
            rasterize_for(entry, IconSize::Large);
        }
        let work = terms() - before;
        // r49: 51 691 terms for the 29 glyphs; every shape at every pixel was 3 395 205.
        assert!(work < 70_000, "{work} distance terms");
    }

    #[test]
    fn every_icon_has_ink_inside_its_box() {
        for entry in ALL {
            // Icons are authored on the grid; ink must stay near the box.
            let mask = icon(entry, IconSize::Medium);
            assert!(mask.alpha.iter().any(|&alpha| alpha > 128), "{entry:?}");
            assert!(mask.left >= -3 && mask.top >= -3);
            let limit = IconSize::Medium.px() as i32 + 3;
            assert!(
                mask.left + mask.width as i32 <= limit && mask.top + mask.height as i32 <= limit
            );
        }
    }

    /// Fills are solid (hatched fake fills read as stripes, review R2) and
    /// knock-outs open the ink below them.
    #[test]
    fn fills_are_solid_and_cuts_are_open() {
        let probe = |entry: Icon, x: i32, y: i32| {
            let mask = icon(entry, IconSize::Medium);
            mask.coverage((x - mask.left) as u32, (y - mask.top) as u32)
        };
        for y in 4..20 {
            for x in 4..20 {
                assert_eq!(probe(Icon::Stop, x, y), 255, "stop ({x}, {y})");
            }
        }
        assert_eq!(probe(Icon::Record, 12, 12), 255);
        // The top DIN pin hole sits inside the solid disc.
        assert_eq!(probe(Icon::Midi, 12, 6), 255);
        assert_eq!(probe(Icon::Midi, 12, 3), 0);
        assert_eq!(probe(Icon::Caret, 12, 10), 255);
    }

    /// The STOMP mode glyph is an outlined shoe, not a solid one: at 24 px a dark line splits the
    /// upper from the sole stripe and the heel strap, as in stock `navigationBar/modes/stomp.png`.
    #[test]
    fn stomp_glyph_keeps_the_sole_stripe_apart() {
        let mask = icon(Icon::Footswitch, IconSize::Medium);
        let probe = |x: i32, y: i32| mask.coverage((x - mask.left) as u32, (y - mask.top) as u32);
        for (x, y) in [(10, 8), (9, 13), (5, 14), (21, 7), (8, 21)] {
            assert!(probe(x, y) > 240, "ink ({x}, {y})");
        }
        for (x, y) in [(8, 12), (12, 15)] {
            assert!(probe(x, y) < 64, "gap ({x}, {y})");
        }
    }

    /// Device glyphs sit on the stock 32 px grid: at 32 px these pixels have the coverage of
    /// `utility.png`, `looper_icon.png`, `doubler.png`, `reverb.png` and `NC.png` (ink, grey part,
    /// empty).
    #[test]
    fn device_glyphs_match_stock_pixels_at_32_px() {
        let probe = |entry: Icon, x: i32, y: i32| {
            let mask = icon(entry, IconSize::Large);
            let (mx, my) = (x - mask.left, y - mask.top);
            let inside =
                (0..mask.width as i32).contains(&mx) && (0..mask.height as i32).contains(&my);
            if inside {
                mask.coverage(mx as u32, my as u32)
            } else {
                0
            }
        };
        let expect = [
            // Fan: handle and ball, a grey blade; the old wrench shaft crossed (22, 22).
            (
                Icon::Utility,
                [(5, 9, 255), (4, 27, 255), (12, 12, 149), (22, 22, 0)],
            ),
            // Reels: rim, bottom spoke, grey rim edge, hollow between rim and hub.
            (
                Icon::LooperReels,
                [(4, 6, 255), (7, 18, 255), (12, 19, 152), (4, 13, 0)],
            ),
            (
                Icon::Doubler,
                [(9, 2, 255), (21, 20, 255), (9, 25, 255), (12, 7, 0)],
            ),
            // Wireframe cube: front edge, grey hidden edge, empty face.
            (
                Icon::Reverb,
                [(22, 10, 255), (12, 1, 255), (12, 4, 149), (16, 16, 0)],
            ),
            // Bright stripes right of centre, dim ones left, offset by three rows.
            (
                Icon::NeuralCapture,
                [(25, 12, 255), (8, 9, 82), (25, 10, 0), (8, 12, 0)],
            ),
        ];
        for (entry, points) in expect {
            for (x, y, coverage) in points {
                assert_eq!(probe(entry, x, y), coverage, "{entry:?} ({x}, {y})");
            }
        }
    }

    const ALL: [Icon; ICON_COUNT] = [
        Icon::Back,
        Icon::Forward,
        Icon::Up,
        Icon::Down,
        Icon::Close,
        Icon::Plus,
        Icon::Minus,
        Icon::Check,
        Icon::Gear,
        Icon::Search,
        Icon::Tuner,
        Icon::Metronome,
        Icon::Looper,
        Icon::Wifi,
        Icon::Bluetooth,
        Icon::Lock,
        Icon::Play,
        Icon::Stop,
        Icon::Record,
        Icon::Undo,
        Icon::Redo,
        Icon::Trash,
        Icon::Copy,
        Icon::Paste,
        Icon::Pencil,
        Icon::Grid,
        Icon::Gig,
        Icon::Menu,
        Icon::Folder,
        Icon::Star,
        Icon::Info,
        Icon::Download,
        Icon::Refresh,
        Icon::Midi,
        Icon::Display,
        Icon::Footswitch,
        Icon::Pedal,
        Icon::InOut,
        Icon::Capture,
        Icon::Archive,
        Icon::Power,
        Icon::Shift,
        Icon::Backspace,
        Icon::Enter,
        Icon::Headphones,
        Icon::Save,
        Icon::More,
        Icon::ModePreset,
        Icon::ModeScene,
        Icon::Swap,
        Icon::Edit,
        Icon::StepBack,
        Icon::StepForward,
        Icon::Sync,
        Icon::Brightness,
        Icon::Bolt,
        Icon::Knob,
        Icon::Storage,
        Icon::Factory,
        Icon::Timer,
        Icon::Clock,
        Icon::Device,
        Icon::Palette,
        Icon::EqPeak,
        Icon::EqHighPass,
        Icon::EqLowPass,
        Icon::EqHighShelf,
        Icon::EqLowShelf,
        Icon::Cloud,
        Icon::Person,
        Icon::NoSignal,
        Icon::NotLoggedIn,
        Icon::CornerCross,
        Icon::CloudSync,
        Icon::CloudUpload,
        Icon::CloudDownload,
        Icon::Envelope,
        Icon::Pulse,
        Icon::InfoFilled,
        Icon::Bug,
        Icon::PlusBold,
        Icon::Document,
        Icon::FolderFilled,
        Icon::SaveArrow,
        Icon::SaveCheck,
        Icon::SaveBadge,
        Icon::Caret,
        Icon::Binoculars,
        Icon::HeartBroken,
        Icon::SidechainSource,
        Icon::SidechainSink,
        Icon::NeuralCaptureMenu,
        Icon::CaptureType,
        Icon::PresetMatch,
        Icon::Plugin,
        Icon::Amp,
        Icon::ComboAmp,
        Icon::NcAmpCab,
        Icon::NeuralCapture,
        Icon::NcPedal,
        Icon::Cab,
        Icon::Overdrive,
        Icon::Fuzz,
        Icon::Delay,
        Icon::Reverb,
        Icon::Compressor,
        Icon::Pitch,
        Icon::Multivoicer,
        Icon::Modulation,
        Icon::Morph,
        Icon::Freeze,
        Icon::BitCrusher,
        Icon::Synth,
        Icon::Filter,
        Icon::Eq,
        Icon::IrLoader,
        Icon::Wah,
        Icon::FxLoop,
        Icon::FxSend,
        Icon::FxReturn,
        Icon::LooperReels,
        Icon::Blend,
        Icon::Doubler,
        Icon::Utility,
        Icon::Spark,
        Icon::TrashCross,
        Icon::Chip,
        Icon::MultipleDevices,
    ];
}
