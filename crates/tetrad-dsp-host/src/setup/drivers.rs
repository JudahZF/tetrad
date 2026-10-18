//! Stock model drivers: the factory (VA `0x50eef8`), the driver vtable
//! hooks, and the recovered drivers. Driver class names are the stock
//! `zenUI::<Name>Driver` names; constructor VAs are in the c-dsp2 record.

use tetrad_dsp_protocol::DeploymentTarget;

use super::tables::{Segment, StockPayload, TableError};
use super::{
    BlockBypass, CatalogParameter, DriverMidi, DspWrite, Factory, LinkedEdit, ModelIdentity,
    ParamSetMode, SceneValue, SetupError,
};

mod amp;
pub mod amps;
mod arpeggio_delay;
mod cab;
pub mod capture;
pub mod catalog;
mod digital_delay;
mod drives;
mod dual_delay;
mod dynamics;
mod freeze;
mod glitch;
mod gru_amp;
mod ir;
mod jm_overdrive;
pub mod lane_io;
pub mod looper;
pub mod media;
mod optimised;
mod parametric;
mod pcom_skews;
mod pitch;
pub mod preblock;
mod reload;
mod reverb;
pub mod sidechain;
mod sync_mod;
mod tape_delay;
pub mod tempo_mod;
pub mod voices;

/// Stock driver classes the factory creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverKind {
    ArpeggioDelay,
    BognaUber,
    /// Mono cab engine: categories 12 and 21, and model 32089.
    CabinetSimulator,
    /// Stereo cab engine: categories 32 and 33.
    CabinetSimulatorStereo,
    Capture2,
    CoryWongAmpSnob,
    DigitalDelay,
    DualDelay,
    DualReverseDelay,
    DumbleOdsLerche,
    DunlopCryBabyLoad,
    FenderDeluxeReverb,
    FenderHighPowerTweedTwin,
    FenderVibroverb,
    FortinNameless,
    Freeze,
    FxLoop,
    FxReturn,
    FxSend,
    Glitch,
    GojiraWow,
    GruAmp,
    HorizonClarityComp,
    IrLoader,
    JmDumbleSss,
    JmOverdrive,
    JmTwoRockCustomReverb,
    Jp2c,
    KeeleyKatana,
    Looper,
    MishaLaser,
    MishaOctaver,
    MishaThor,
    Mishaphyre,
    Minivoicer,
    MonosynthMidi,
    Multivoicer,
    MxrFlanger,
    /// Not a stock class: model 14000 has no driver, and the block push
    /// loads its capture (VA `0x334390`). [`driver_kind`] never returns it;
    /// only `Block::build` gives it to model 14000.
    NeuralCapture,
    NollyAmp,
    Odr1,
    OptimisedAdvancedChorus,
    OptimisedAdvancedFlanger,
    Parallax,
    Parametric4,
    PcomSkews,
    Plates,
    RabeaFuzz,
    Reverb,
    SoloRevD,
    SslBusComp,
    /// Constructor `+0x24..=+0x28` = `0..=range_end` (VA `0x55c330`): the NOTIFICATION_START
    /// (index 0) and NOTIFICATION_END (`range_end`) markers around the parameter writes.
    TapeDelay {
        range_end: u8,
    },
    TempoModulation,
    Transpose,
    Ts808Unified,
    VictoryKraken,
}

impl DriverKind {
    /// The driver takes every stomp press and release of its block (`+0x40`
    /// and `+0x44` return 0), so stock skips the block's bypass, secondary
    /// and momentary toggles (`0x5239d0`, `0x523f80`). Only Freeze overrides
    /// them (`0x55acd8`, `0x55ac5c`); the base returns 1 (`0xed520`,
    /// `0xed538`).
    pub fn takes_footswitch(self) -> bool {
        self == Self::Freeze
    }
}

const CABINET_MONO: [u32; 2] = [12, 21];
const CABINET_STEREO: [u32; 2] = [32, 33];
const FX_LOOP_CATEGORY: u32 = 13;
const LOOPER_CATEGORY: u32 = 27;

/// Stock factory choice (VA `0x50eef8`): the model-id switch, then the
/// category overrides (cabs, FX loops, loopers). `None` = no driver; the
/// block push then sends every value unchanged.
///
/// Stock also drops the previous drivers first and, for an old 27000
/// Looper, flags whether the new grid keeps one (`+0xd4`, then VA
/// `0x875c5c`); that is not modelled.
pub fn driver_kind(model_id: u32, category: u32) -> Option<DriverKind> {
    use DriverKind as K;
    let mut kind = match model_id {
        26 => Some(K::Odr1),
        30 | 32 | 35 | 39 | 41 | 48 | 51 => Some(K::Ts808Unified),
        42 | 49 | 1154..=1156 | 1171 | 1184 => Some(K::GruAmp),
        45 => Some(K::KeeleyKatana),
        46 => Some(K::JmOverdrive),
        47 => Some(K::RabeaFuzz),
        1130..=1131 => Some(K::BognaUber),
        1132..=1135 => Some(K::FenderHighPowerTweedTwin),
        1140..=1141 | 1179..=1180 => Some(K::Jp2c),
        1157..=1158 | 1185..=1186 => Some(K::VictoryKraken),
        1163..=1164 | 6033 | 8025 | 8038..=8039 | 16008 | 16012..=16013 | 18011 | 18017 => {
            Some(K::PcomSkews)
        }
        1165 => Some(K::FortinNameless),
        1166..=1167 => Some(K::FenderDeluxeReverb),
        1170 => Some(K::SoloRevD),
        1172..=1174 => Some(K::NollyAmp),
        1175 => Some(K::CoryWongAmpSnob),
        1176 => Some(K::DumbleOdsLerche),
        1181 => Some(K::FenderVibroverb),
        1182 => Some(K::JmTwoRockCustomReverb),
        1183 => Some(K::JmDumbleSss),
        1189 => Some(K::Mishaphyre),
        3008 => Some(K::Parallax),
        4008 => Some(K::Parametric4),
        5017 | 5022..=5023 | 5030 => Some(K::SslBusComp),
        5029 => Some(K::HorizonClarityComp),
        6004 | 6017 | 6020..=6021 => Some(K::TapeDelay { range_end: 0x17 }),
        6005 | 6011 | 6022..=6024 | 6032 => Some(K::DigitalDelay),
        6007 => Some(K::TapeDelay { range_end: 0x15 }),
        6012 => Some(K::DualDelay),
        6014 => Some(K::DualReverseDelay),
        6016 | 6018..=6019 => Some(K::TapeDelay { range_end: 0x1a }),
        6027 => Some(K::TapeDelay { range_end: 0x0f }),
        6031 => Some(K::ArpeggioDelay),
        7000..=7004 | 7006..=7009 | 7031..=7033 | 7035..=7038 => Some(K::TempoModulation),
        7014 => Some(K::OptimisedAdvancedFlanger),
        7015 => Some(K::OptimisedAdvancedChorus),
        7021 | 7042 => Some(K::MxrFlanger),
        7046 => Some(K::MishaThor),
        8000
        | 8003
        | 8007..=8010
        | 8015..=8019
        | 8021
        | 8023..=8024
        | 8032
        | 8037
        | 8040..=8041 => Some(K::Reverb),
        8012..=8014 => Some(K::Plates),
        9014 => Some(K::DunlopCryBabyLoad),
        14001 => Some(K::Capture2),
        18001 | 18010 => Some(K::Transpose),
        18007 => Some(K::Minivoicer),
        18009 => Some(K::GojiraWow),
        18012 => Some(K::Multivoicer),
        18018 => Some(K::MishaLaser),
        18019 => Some(K::MishaOctaver),
        26001 => Some(K::Freeze),
        26005 => Some(K::Glitch),
        29001..=29008 => Some(K::IrLoader),
        30001 => Some(K::MonosynthMidi),
        _ => None,
    };
    if CABINET_MONO.contains(&category) || model_id == 32089 {
        kind = Some(K::CabinetSimulator);
    } else if CABINET_STEREO.contains(&category) {
        kind = Some(K::CabinetSimulatorStereo);
    }
    if category == FX_LOOP_CATEGORY {
        kind = match model_id {
            13000 | 13001 | 13006 => Some(K::FxSend),
            13002 | 13003 | 13007 => Some(K::FxReturn),
            13004 | 13005 | 13008 => Some(K::FxLoop),
            _ => kind,
        };
    }
    if category == LOOPER_CATEGORY && matches!(model_id, 27000 | 27002) {
        kind = Some(K::Looper);
    }
    kind
}

/// The stock driver vtable hooks that run during setup and edits.
pub trait Driver: std::fmt::Debug + Send {
    fn kind(&self) -> DriverKind;
    /// The block push sets driver `+0x1c` to 1 first (VA `0x5174a8`).
    fn begin_push(&mut self) {}
    /// Vtable `+0x30`: true lets the generic `0x02` go out. Pure in the base
    /// class; the default is the table drivers' `return 1`.
    fn update_parameter(
        &mut self,
        _index: u32,
        _value: f32,
        _mode: ParamSetMode,
        _out: &mut Vec<DspWrite>,
    ) -> bool {
        true
    }
    /// Vtable `+0x3c` (base forwards to `+0x38`): true lets `0x0c` go out.
    fn bypass(&mut self, _bypass: BlockBypass, _out: &mut Vec<DspWrite>) -> bool {
        true
    }
    /// Vtable `+0x48`, after the block push, after a block's scene recall
    /// and from some hooks.
    fn parameters_pushed(&mut self, _out: &mut Vec<DspWrite>) {}
    /// Vtable `+0x64`, after a block's values in scene recall (VA
    /// `0x526cc0`); empty in the base class. `scene_value` is the block's
    /// stored value of a parameter at the recalled scene, `None` when the
    /// parameter is not in scene mode (`0x2ea908`, `0x2ea164`).
    fn scene_recalled(&mut self, _scene_value: SceneValue<'_>, _out: &mut Vec<DspWrite>) {}
    /// A single-shot QTimer the driver (re)started since the last call; the
    /// runtime calls [`Self::timer_elapsed`] after this delay. A new start
    /// replaces a pending one (`QTimer::start`).
    fn take_timer(&mut self) -> Option<std::time::Duration> {
        None
    }
    /// The driver's timer slot.
    fn timer_elapsed(&mut self, _out: &mut Vec<DspWrite>) {}
    /// Vtable `+0x5c`, called for every driver by the tempo setter (VA
    /// `0x875a48`) with the new integer BPM.
    fn tempo_changed(&mut self, _bpm: i32, _out: &mut Vec<DspWrite>) {}
    /// Vtable `+0x34` for one string value, plus the stock IR and capture
    /// managers that read cab and capture strings (c-dsp3a).
    fn update_text(
        &mut self,
        _index: u32,
        _text: &str,
        _mode: ParamSetMode,
        _library: &dyn media::MediaLibrary,
        _out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        Ok(())
    }
    /// True for drivers that perform the push's capture load (VA
    /// `0x334390`); the push refuses capture models without one.
    fn loads_capture(&self) -> bool {
        false
    }
    /// Vtable `+0x40`: a press of a stomp assigned to the block.
    fn footswitch_pressed(&mut self, _out: &mut Vec<DspWrite>) {}
    /// Vtable `+0x44`: the release, before the momentary test.
    fn footswitch_released(&mut self, _out: &mut Vec<DspWrite>) {}
    /// Vtables `+0x4c`/`+0x50`: the channel the driver takes MIDI on
    /// (1..=16, or [`super::MIDI_OMNI`]); `None` (base `+0x4c` = 0) takes none.
    fn midi_channel(&self) -> Option<i32> {
        None
    }
    /// Vtables `+0x58` (notes) and `+0x54` (CC 120/123).
    fn midi(&mut self, _message: DriverMidi, _out: &mut Vec<DspWrite>) {}
    /// Vtable `+0x60`, the end of a controller write (`0x523644`) of a float
    /// at `index`; empty in the base class (`0xed5d0`).
    fn parameter_edited(&mut self, _index: u32, _value: f32, _out: &mut Vec<DspWrite>) {}
    /// Moves the preset writes made since the last call into `out`.
    fn take_linked_edits(&mut self, _out: &mut Vec<LinkedEdit>) {}
    /// The voice notes of the last voice send since the last call (`None`: a
    /// silent voice): Minivoicer 0x575010 gives each voice to the editor
    /// read-out 0x61d994.
    fn take_voice_notes(&mut self) -> Option<[Option<u8>; 2]> {
        None
    }
}

/// Factory step for one block (VA `0x50eef8`), the only driver choice:
/// `Ok(None)` when stock creates no driver. Drivers that read stock tables at
/// construction refuse a factory without them.
pub(super) fn create(
    identity: &ModelIdentity,
    catalog: &[CatalogParameter],
    factory: &Factory<'_>,
    out: &mut Vec<DspWrite>,
) -> Result<Option<Box<dyn Driver>>, SetupError> {
    use DriverKind as K;
    let ModelIdentity {
        target, model_id, ..
    } = *identity;
    let Some(kind) = driver_kind(model_id, identity.category) else {
        let capture = media::CAPTURE_V1_MODEL == model_id;
        return Ok(capture
            .then(|| -> Box<dyn Driver> { Box::new(capture::CaptureLoad::new(identity, false)) }));
    };
    let tables = || {
        factory.tables.ok_or(SetupError::MissingTables {
            model_id,
            driver: kind,
        })
    };
    let media = || {
        tables().map(|tables| media::Media {
            stock: tables.media(),
            library: factory.library,
        })
    };
    let driver: Box<dyn Driver> = match kind {
        K::CabinetSimulator | K::CabinetSimulatorStereo | K::Parallax => {
            Box::new(cab::Cab::new(identity, kind, &media()?, out))
        }
        K::IrLoader => Box::new(ir::IrLoader::new(identity, &media()?, out)?),
        K::Capture2 => Box::new(capture::CaptureLoad::new(identity, true)),
        K::TempoModulation => Box::new(
            tempo_mod::TempoModulation::new(identity, &tables()?.notes).ok_or(
                SetupError::UnsupportedDriver {
                    model_id,
                    driver: kind,
                },
            )?,
        ),
        K::FortinNameless => Box::new(amps::fortin::Fortin::new(identity, &tables()?.amps, out)),
        K::FenderDeluxeReverb => Box::new(amps::deluxe::Deluxe::new(identity, &tables()?.amps)),
        K::CoryWongAmpSnob => Box::new(amps::cory_wong::CoryWong::new(
            identity,
            &tables()?.amps,
            out,
        )),
        K::DumbleOdsLerche | K::FenderVibroverb | K::JmTwoRockCustomReverb | K::JmDumbleSss => {
            use amps::header::{Circuit, HeaderAmp};
            let circuit = match kind {
                K::DumbleOdsLerche => Circuit::Ods,
                K::FenderVibroverb => Circuit::Vibroverb,
                K::JmTwoRockCustomReverb => Circuit::TwoRock,
                _ => Circuit::Sss,
            };
            Box::new(HeaderAmp::new(identity, circuit, &tables()?.amps, out))
        }
        K::FxSend => Box::new(FxSend::new(target)),
        K::FxReturn => Box::new(FxReturn::new(target, model_id)),
        K::FxLoop => {
            let send = identity
                .fx_loop_send
                .ok_or(SetupError::MissingFxLoopSend { model_id })?;
            Box::new(FxLoop::new(target, send, model_id))
        }
        K::TapeDelay { range_end } => Box::new(tape_delay::TapeDelay::new(identity, range_end)),
        K::DigitalDelay => Box::new(digital_delay::DigitalDelay::new(identity)),
        K::DualDelay => Box::new(dual_delay::DualDelay::new(identity, false)),
        K::DualReverseDelay => Box::new(dual_delay::DualDelay::new(identity, true)),
        K::ArpeggioDelay => Box::new(arpeggio_delay::ArpeggioDelay::new(identity)),
        K::Reverb => Box::new(reverb::Reverb::new(identity, catalog, out)?),
        K::PcomSkews => Box::new(pcom_skews::PcomSkews::new(identity, catalog)?),
        K::VictoryKraken => Box::new(amp::VictoryKraken::new(identity, out)),
        K::GruAmp => Box::new(gru_amp::GruAmp::new(identity, out)?),
        K::Jp2c => Box::new(amps::Jp2c::new(identity, out)?),
        K::SoloRevD => Box::new(amps::SoloRevD::new(identity, out)),
        // r14-misc drivers.
        K::Glitch => Box::new(glitch::Glitch::new(identity)),
        K::SslBusComp => Box::new(dynamics::SslBusComp::new(identity).ok_or(
            SetupError::UnsupportedDriver {
                model_id,
                driver: kind,
            },
        )?),
        K::Transpose => Box::new(pitch::Transpose::new(identity)),
        K::MishaOctaver => Box::new(pitch::Octaver::new(identity)),
        K::GojiraWow => Box::new(pitch::GojiraWow::new(identity)),
        K::OptimisedAdvancedChorus => Box::new(optimised::Optimised::chorus(identity)),
        // These upload stock tables later (push, `+0x48` or their slot), so
        // they are refused without them (review r14 D1).
        K::HorizonClarityComp | K::RabeaFuzz | K::Odr1 | K::KeeleyKatana => {
            tables()?;
            match kind {
                K::HorizonClarityComp => Box::new(dynamics::Horizon::new(identity)),
                K::RabeaFuzz => Box::new(drives::Rabea::new(identity)),
                _ => {
                    let pedal = match kind {
                        K::Odr1 => drives::Pedal::Odr1,
                        _ => drives::Pedal::Katana,
                    };
                    Box::new(drives::SwitchedDrive::new(identity, pedal, out))
                }
            }
        }
        // r15-drv drivers.
        K::Freeze => Box::new(freeze::Freeze::new(identity, factory.mini)),
        K::Minivoicer => Box::new(voices::Minivoicer::new(target)),
        K::MonosynthMidi => Box::new(voices::Monosynth::new(target, catalog, out)),
        K::Parametric4 => Box::new(parametric::Parametric4::new(identity, catalog)?),
        // These read stock rows at construction.
        K::MishaLaser
        | K::OptimisedAdvancedFlanger
        | K::JmOverdrive
        | K::MxrFlanger
        | K::MishaThor
        | K::Multivoicer => {
            let tables = tables()?;
            let built: Result<Box<dyn Driver>, _> = match kind {
                K::MishaLaser => pitch::Laser::new(identity, tables).map(|d| Box::new(d) as _),
                K::Multivoicer => {
                    voices::Multivoicer::new(target, tables).map(|d| Box::new(d) as _)
                }
                K::MxrFlanger => {
                    sync_mod::MxrFlanger::new(identity, tables).map(|d| Box::new(d) as _)
                }
                K::MishaThor => {
                    sync_mod::MishaThor::new(identity, tables).map(|d| Box::new(d) as _)
                }
                K::JmOverdrive => {
                    jm_overdrive::JmOverdrive::new(identity, tables).map(|d| Box::new(d) as _)
                }
                _ => optimised::Optimised::flanger(identity, tables).map(|d| Box::new(d) as _),
            };
            built.map_err(|source| match source {
                TableError::Absent => SetupError::MissingTables {
                    model_id,
                    driver: kind,
                },
                source => SetupError::StockTable {
                    model_id,
                    driver: kind,
                    source,
                },
            })?
        }
        // A recovered planner (c-dsp4b) without a `Block` path yet.
        K::Looper => {
            return Err(SetupError::UnrecoveredPath {
                model_id,
                driver: kind,
                va: 0x55e140,
                missing: "global Looper requests and playback state",
            });
        }
        _ => {
            let payload =
                constructor_table(kind, model_id).ok_or(SetupError::UnsupportedDriver {
                    model_id,
                    driver: kind,
                })?;
            out.push(DspWrite::StockData {
                target,
                buffer: 0,
                payload,
            });
            Box::new(TableOnly(kind))
        }
    };
    Ok(Some(driver))
}

macro_rules! table {
    ($($va:expr => $len:expr),+ $(,)?) => {
        StockPayload::new(&[$(Segment { va: $va, len: $len }),+])
    };
}

/// ReceiveData buffer-0 payload sent by constructors that only send a table
/// and keep the base hooks (`+0x30` returns true). `None` = not such a driver.
fn constructor_table(kind: DriverKind, model_id: u32) -> Option<StockPayload> {
    Some(match (kind, model_id) {
        // TS808UnifiedDriver (VA 0x567300): 0x1a8 bytes per model.
        (DriverKind::Ts808Unified, 30) => table!(0x00f5_4718 => 0x1a8),
        (DriverKind::Ts808Unified, 32) => table!(0x00f5_48c0 => 0x1a8),
        (DriverKind::Ts808Unified, 35) => table!(0x00f5_4a68 => 0x1a8),
        (DriverKind::Ts808Unified, 39) => table!(0x00f5_4db8 => 0x1a8),
        (DriverKind::Ts808Unified, 41) => table!(0x00f5_4c10 => 0x1a8),
        (DriverKind::Ts808Unified, 48) => table!(0x00f5_5108 => 0x1a8),
        (DriverKind::Ts808Unified, 51) => table!(0x00f5_4f60 => 0x1a8),
        // BognaUberDriver (VAs 0x55fd68, 0x55fc88).
        (DriverKind::BognaUber, 1130) => table!(0x00a7_403c => 0xe584),
        (DriverKind::BognaUber, 1131) => table!(
            0x00a6_ced8 => 0x6864,
            0x0153_99e8 => 0x3e44,
            0x00a7_373c => 0x900,
        ),
        // FenderHighPowerTweedTwinDriver (VA 0x560c70).
        (DriverKind::FenderHighPowerTweedTwin, 1132) => table!(0x00ab_6f00 => 0xe584),
        (DriverKind::FenderHighPowerTweedTwin, 1133) => table!(0x00ac_5484 => 0xe584),
        (DriverKind::FenderHighPowerTweedTwin, 1134) => table!(0x00ad_3a08 => 0xe584),
        (DriverKind::FenderHighPowerTweedTwin, 1135) => table!(0x00ae_1f8c => 0xe584),
        // NollyAmpDriver (VA 0x57cfc0).
        (DriverKind::NollyAmp, 1172) => table!(0x00f6_95a8 => 0xcba4),
        (DriverKind::NollyAmp, 1173) => table!(0x00f7_614c => 0xcba4),
        (DriverKind::NollyAmp, 1174) => table!(0x00f8_2cf0 => 0xcaa4),
        // MishaphyreDriver (VA 0x55f2e4).
        (DriverKind::Mishaphyre, 1189) => table!(0x00a5_e588 => 0xc9a4),
        // PlatesDriver (VA 0x57c718).
        (DriverKind::Plates, 8012) => table!(0x00f5_7840 => 0x68),
        (DriverKind::Plates, 8013) => table!(0x00f5_78a8 => 0x68),
        (DriverKind::Plates, 8014) => table!(0x00f5_7910 => 0x68),
        // DunlopCryBabyLoadDriver (VA 0x57c9c8).
        (DriverKind::DunlopCryBabyLoad, 9014) => table!(0x00f5_7a40 => 0xa8),
        _ => return None,
    })
}

/// Every stock payload a driver may send; [`super::ElfTables::drivers`]
/// keeps exactly these ranges.
pub fn stock_payloads() -> impl Iterator<Item = StockPayload> {
    constructor_tables()
        .map(|(_, _, payload)| payload)
        .chain(reverb::payloads())
        .chain(amp::payloads())
        .chain(tempo_mod::payloads())
        .chain(gru_amp::payloads())
        .chain(amps::payloads())
        .chain(looper::payloads())
        .chain(dynamics::payloads())
        .chain(pitch::payloads())
        .chain(drives::payloads())
        .chain(jm_overdrive::payloads())
        .chain(optimised::payloads())
        .chain(sync_mod::payloads())
        .chain(voices::payloads())
}

/// Every model whose driver only sends a constructor table, with its payload.
pub fn constructor_tables() -> impl Iterator<Item = (DriverKind, u32, StockPayload)> {
    use DriverKind as K;
    [
        (K::Ts808Unified, &[30, 32, 35, 39, 41, 48, 51][..]),
        (K::BognaUber, &[1130, 1131]),
        (K::FenderHighPowerTweedTwin, &[1132, 1133, 1134, 1135]),
        (K::NollyAmp, &[1172, 1173, 1174]),
        (K::Mishaphyre, &[1189]),
        (K::Plates, &[8012, 8013, 8014]),
        (K::DunlopCryBabyLoad, &[9014]),
    ]
    .into_iter()
    .flat_map(|(kind, models)| {
        models.iter().filter_map(move |&model| {
            constructor_table(kind, model).map(|payload| (kind, model, payload))
        })
    })
}

#[derive(Debug)]
struct TableOnly(DriverKind);

impl Driver for TableOnly {
    fn kind(&self) -> DriverKind {
        self.0
    }
}

/// `powf(10, fma(v, range, -40) · 0.05)` with the stock fused multiply-add.
fn gain_db(value: f32, range: f32) -> f32 {
    10_f32.powf(value.mul_add(range, -40.0) * 0.05)
}

/// FX Loop gain: `f32(v·range)` widened, `− 37.74`, `· f64(0.05f32)`, then
/// `powf` (VA 0x55c80c).
fn loop_gain(value: f32, range: f32) -> f32 {
    let exponent = (f64::from(value * range) - 37.74) * f64::from(0.05_f32);
    10_f32.powf(exponent as f32)
}

/// Mix law shared by FX return and loop: `(dry, wet)`. NaN fails both stock
/// tests (`bls`, `bge`) and gets both products, so dry and wet are NaN
/// (VAs 0x55c770, 0x55c8e8).
fn mix(value: f32) -> (f32, f32) {
    match value.partial_cmp(&0.75) {
        Some(std::cmp::Ordering::Less) => (1.0, value * 1.333_33),
        Some(std::cmp::Ordering::Equal) => (1.0, 1.0),
        Some(std::cmp::Ordering::Greater) => (value.mul_add(-4.0, 4.0), 1.0),
        None => (value.mul_add(-4.0, 4.0), value * 1.333_33),
    }
}

/// +3 dB applied by FX return and loop drivers except the 1/2 variants.
const BOOST: f32 = 1.412_537_5;

/// Stores `value` and marks `dirty` when it differs (`vcmp`, NaN differs).
fn store(slot: &mut f32, value: f32, dirty: &mut bool) {
    *dirty |= *slot != value;
    *slot = value;
}

/// FxSendDriver (VAs 0x55c9c4, 0x55c3ec, 0x55c350, 0x55c5cc). LEVEL (0) and
/// THRU (1) never pass through; `+0x48` sends parameter 3 = LEVEL gain and
/// 5 = THRU gain (40 dB range). Bypass sends no `0x0c`: LEVEL goes to 0 and
/// THRU to unity.
#[derive(Debug)]
struct FxSend {
    target: DeploymentTarget,
    dirty: bool,
    level: f32,
    thru: f32,
    bypassed: bool,
}

impl FxSend {
    fn new(target: DeploymentTarget) -> Self {
        Self {
            target,
            dirty: true,
            level: 0.0,
            thru: 0.0,
            bypassed: false,
        }
    }
}

impl Driver for FxSend {
    fn kind(&self) -> DriverKind {
        DriverKind::FxSend
    }
    fn begin_push(&mut self) {
        self.dirty = true;
    }
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            0 => store(&mut self.level, value, &mut self.dirty),
            1 => store(&mut self.thru, value, &mut self.dirty),
            _ => {}
        }
        if mode == ParamSetMode::Live {
            self.parameters_pushed(out);
        }
        false
    }
    fn bypass(&mut self, bypass: BlockBypass, out: &mut Vec<DspWrite>) -> bool {
        self.dirty |= self.bypassed != bypass.bypassed;
        self.bypassed = bypass.bypassed;
        self.parameters_pushed(out);
        false
    }
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        if self.dirty {
            let level = if self.level == 0.0 || self.bypassed {
                0.0
            } else {
                gain_db(self.level, 40.0)
            };
            let thru = match (self.bypassed, self.thru == 0.0) {
                (true, _) => 1.0,
                (false, true) => 0.0,
                (false, false) => gain_db(self.thru, 40.0),
            };
            out.push(DspWrite::parameter(self.target, 3, level));
            out.push(DspWrite::parameter(self.target, 5, thru));
        }
        self.dirty = false;
    }
}

/// FxReturnDriver (VAs 0x55cbf8, 0x55c470, 0x55c384, 0x55c6d0). LEVEL (0,
/// 52 dB range) and MIX (1) never pass through; `+0x48` sends parameter 9 =
/// LEVEL gain × wet and 7 = dry. Bypass: 9 = 0, 7 = unity, no `0x0c`.
#[derive(Debug)]
struct FxReturn {
    target: DeploymentTarget,
    dirty: bool,
    mix: f32,
    level: f32,
    bypassed: bool,
    boost: bool,
}

impl FxReturn {
    fn new(target: DeploymentTarget, model_id: u32) -> Self {
        Self {
            target,
            dirty: true,
            mix: 0.5,
            level: 1.0,
            bypassed: false,
            boost: model_id != 13007,
        }
    }
}

impl Driver for FxReturn {
    fn kind(&self) -> DriverKind {
        DriverKind::FxReturn
    }
    fn begin_push(&mut self) {
        self.dirty = true;
    }
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            0 => store(&mut self.level, value, &mut self.dirty),
            1 => store(&mut self.mix, value, &mut self.dirty),
            _ => {}
        }
        if mode == ParamSetMode::Live {
            self.parameters_pushed(out);
        }
        false
    }
    fn bypass(&mut self, bypass: BlockBypass, out: &mut Vec<DspWrite>) -> bool {
        self.dirty |= self.bypassed != bypass.bypassed;
        self.bypassed = bypass.bypassed;
        self.parameters_pushed(out);
        false
    }
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        if self.dirty {
            let gain = if self.level == 0.0 {
                0.0
            } else {
                gain_db(self.level, 52.0)
            };
            let (mut dry, wet) = mix(self.mix);
            let mut level = if self.bypassed {
                dry = 1.0;
                0.0
            } else {
                gain * wet
            };
            if self.boost {
                level *= BOOST;
                dry *= BOOST;
            }
            out.push(DspWrite::parameter(self.target, 9, level));
            out.push(DspWrite::parameter(self.target, 7, dry));
        }
        self.dirty = false;
    }
}

/// FxLoopDriver (VAs 0x55ce48, 0x55c4f4, 0x55c3b8, 0x55c80c). SEND LEV (0),
/// RET LEV (1), MIX (2) and TRAILS (3) never pass through; `+0x48` sends 9
/// = return gain × wet and 7 = dry to the block, and 3 = send gain to the
/// loop's send placement. Bypass mutes the send, sets dry to unity and keeps
/// the wet return only with TRAILS on. TRAILS alone does not mark dirty.
#[derive(Debug)]
struct FxLoop {
    target: DeploymentTarget,
    send_target: DeploymentTarget,
    dirty: bool,
    mix: f32,
    return_level: f32,
    send_level: f32,
    trails: bool,
    bypassed: bool,
    boost: bool,
}

impl FxLoop {
    fn new(target: DeploymentTarget, send_target: DeploymentTarget, model_id: u32) -> Self {
        Self {
            target,
            send_target,
            dirty: true,
            mix: 0.5,
            return_level: 1.0,
            send_level: 1.0,
            trails: false,
            bypassed: false,
            boost: model_id != 13008,
        }
    }
}

impl Driver for FxLoop {
    fn kind(&self) -> DriverKind {
        DriverKind::FxLoop
    }
    fn begin_push(&mut self) {
        self.dirty = true;
    }
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            0 => store(&mut self.send_level, value, &mut self.dirty),
            1 => store(&mut self.return_level, value, &mut self.dirty),
            2 => store(&mut self.mix, value, &mut self.dirty),
            3 => self.trails = value >= 0.5,
            _ => {}
        }
        if mode == ParamSetMode::Live {
            self.parameters_pushed(out);
        }
        false
    }
    fn bypass(&mut self, bypass: BlockBypass, out: &mut Vec<DspWrite>) -> bool {
        self.dirty |= self.bypassed != bypass.bypassed;
        self.bypassed = bypass.bypassed;
        self.parameters_pushed(out);
        false
    }
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        if self.dirty {
            let gain = if self.return_level == 0.0 {
                0.0
            } else {
                loop_gain(self.return_level, 52.0)
            };
            let mut send = if self.send_level == 0.0 {
                0.0
            } else {
                loop_gain(self.send_level, 40.0)
            };
            let (mut dry, mut wet) = mix(self.mix);
            if self.bypassed {
                if !self.trails {
                    wet = 0.0;
                }
                dry = 1.0;
                send = 0.0;
            }
            let mut wet = wet * gain;
            if self.boost {
                wet *= BOOST;
                dry *= BOOST;
            }
            out.push(DspWrite::parameter(self.target, 9, wet));
            out.push(DspWrite::parameter(self.target, 7, dry));
            out.push(DspWrite::parameter(self.send_target, 3, send));
        }
        self.dirty = false;
    }
}
