//! Per-model setup after `BulkStartModel`: the stock driver factory (VA `0x50eef8`), the block push
//! (VA `0x5174a8`), bypass writes (VA `0x517228`) and single edits (VA `0x51e4d8`).
//!
//! Inputs are plain (model id, category, placement, values), so the engine's
//! deployment plan adapts to them without a dependency cycle. Planning is
//! pure: it appends [`DspWrite`]s, which [`crate::session::DspSession`]
//! executes in stock order (`execute`).

pub mod drivers;
mod execute;
pub mod tables;

use tetrad_dsp_protocol::DeploymentTarget;
use tetrad_dsp_protocol::requests::BypassState;

pub use drivers::catalog::CatalogParameter;
pub use drivers::media::{MediaLibrary, MediaStock};
pub use drivers::{Driver, DriverKind, driver_kind};
pub use tables::{ElfTables, HashedZenUi, Segment, StockPayload, StockTables, TableError};

use drivers::amps::AmpTables;
use drivers::tempo_mod::NoteTables;

/// Capture models load their capture through a separate path (VA `0x334390`).
pub const CAPTURE_MODELS: [u32; 2] = [14000, 14001];

/// Models whose bypass does not toggle their sidechain router (VA `0x517228`).
pub const SIDECHAIN_BYPASS_EXEMPT: [u32; 4] = [16008, 16013, 18012, 30001];

/// Sidechain router parameter that carries "block active" (VA `0x517228`).
pub const SIDECHAIN_ACTIVE_PARAMETER: u32 = 5;

/// Stock `ParamSetModes` passed to driver hooks. Names are inferred from the
/// callers: 0 from live edits, 1 from scene recall (`0x52570c`), 2 from the
/// block push (`0x5174a8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamSetMode {
    Live = 0,
    Recall = 1,
    Push = 2,
}

/// One runtime parameter as stock holds it. The QVariant type, not the
/// catalog type, picks the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupValue {
    /// Qt4 `QMetaType::Float` (135): the stored value, sent without
    /// conversion (normalized for catalog parameters). Raw bits keep NaN.
    Float(u32),
    /// `QVariant::Int` (2): `0x03` from the push; ignored by single edits.
    Int(u32),
    /// `QVariant::UInt` (3): `0x03` from single edits; ignored by the push.
    UInt(u32),
    /// `QVariant::String` (10): only the driver text hook (`+0x34`) reads it.
    Text,
    /// Catalog types 5..=8 (meters): never pushed.
    Meter,
}

/// Block bypass input of `0x517228`. Global bypass (VA `0x41111c`, per row
/// for cab, IR loader and type-4 capture blocks) forces `bypassed` and
/// clears `muted`; that is the caller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockBypass {
    pub bypassed: bool,
    /// The block's MUTE value is nonzero.
    pub muted: bool,
}

impl BlockBypass {
    pub const fn state(self) -> BypassState {
        match (self.bypassed, self.muted) {
            (false, _) => BypassState::Engaged,
            (true, false) => BypassState::Bypassed,
            (true, true) => BypassState::BypassedFlagged,
        }
    }
}

/// One deployed grid block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelIdentity {
    pub target: DeploymentTarget,
    pub model_id: u32,
    /// ModelRepo category id.
    pub category: u32,
    /// FX Loop blocks only: the loop's send placement from compiler table
    /// `+0x624` (VA `0x581890`, entry 1 for model 13005, else 0).
    pub fx_loop_send: Option<DeploymentTarget>,
}

/// Every stock table a driver reads, from the ZenUI image of the running build: the
/// driver payload ranges (constructor and update `ReceiveData`), the
/// TempoModulation NOTE tables, the table amps' words and the cab, IR Loader
/// and room tables of the media drivers.
pub struct DriverTables {
    elf: ElfTables,
    notes: NoteTables,
    amps: AmpTables,
    media: MediaStock,
}

impl DriverTables {
    /// Checks the image once (SHA-256 of the running build; a [`HashedZenUi`] is not hashed
    /// again), reads the note, amp and media tables from it, then keeps only the driver payload
    /// ranges (about 7.3 MB of the 22 MB image).
    pub fn load(zenui: impl Into<HashedZenUi>) -> Result<Self, TableError> {
        let image = ElfTables::new(zenui)?;
        let notes = NoteTables::read(&image)?;
        let amps = AmpTables::read(&image)?;
        let media = MediaStock::read(&image)?;
        Ok(Self {
            elf: image.keep(drivers::stock_payloads())?,
            notes,
            amps,
            media,
        })
    }

    pub fn media(&self) -> &MediaStock {
        &self.media
    }
}

impl StockTables for DriverTables {
    fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
        self.elf.read(payload)
    }
}

impl std::fmt::Debug for DriverTables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverTables")
            .field("elf", &self.elf)
            .finish_non_exhaustive()
    }
}

/// What the stock factory (VA `0x50eef8`) reads besides the block: the
/// stock tables (`None` without `/opt/neuraldsp/ZenUI`), the app's media
/// library and the product.
#[derive(Clone, Copy)]
pub struct Factory<'a> {
    pub tables: Option<&'a DriverTables>,
    pub library: &'a dyn MediaLibrary,
    /// The product is the Quad Cortex mini (VA `0x499a60`).
    pub mini: bool,
}

/// One preset write a driver makes through the controller (`0x52382c`,
/// `0x523644`, `0x526bd4`). The driver has already sent its DSP writes: the
/// stock re-entry into `+0x30` is flattened. The app stores a controller
/// write with the `0x51e4d8` scene rule and must not edit the block again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkedEdit {
    pub index: u32,
    pub value: f32,
    /// A controller write: the Control echo (`0x25d51c`) follows. False for
    /// the scene hook's `0x526bd4`, which only stores into the recalled
    /// scene slot (`0x2e0830(…, 0)` → `0x2e8160`).
    pub echo: bool,
}

/// The block's stored value of a parameter at the recalled scene, for the
/// driver scene hook; `None` when the parameter is not in scene mode
/// (`0x2ea908`) or has no stored float.
pub type SceneValue<'a> = &'a dyn Fn(u32) -> Option<f32>;

/// One MIDI message the controller routes to drivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverMidi {
    /// `0x514270` to `+0x58`; velocity 0 is an off.
    Note { note: u8, velocity: u8, on: bool },
    /// `0x5141b8` to `+0x54`: only CC 120 and 123 reach it (`0x5b6338`).
    Control(u8),
}

/// MIDI channel 17 accepts every channel (`+0x50`).
pub const MIDI_OMNI: i32 = 17;

/// One ARM→DSP write in stock order.
#[derive(Clone, PartialEq, Eq)]
pub enum DspWrite {
    /// `0x02` ParameterChange (VA `0x517010`). Bulk mode queues it as a
    /// `0x1d` record.
    Parameter {
        target: DeploymentTarget,
        index: u32,
        value_bits: u32,
    },
    /// `0x03` IntParameterChange (VA `0x511318`). Never queued.
    IntParameter {
        target: DeploymentTarget,
        index: u32,
        value: u32,
    },
    /// `0x0c` ModelBypass (VA `0x517228`). Bulk mode queues it as a `0x3d`
    /// record.
    Bypass {
        target: DeploymentTarget,
        state: BypassState,
    },
    /// `0x0b` ReceiveData of stock ZenUI bytes (VA `0x51965c`, wait 1).
    StockData {
        target: DeploymentTarget,
        buffer: u8,
        payload: StockPayload,
    },
    /// `0x0b` ReceiveData of computed bytes (rendered cab IRs, IR Loader
    /// records, captures; VA `0x51965c`, wait 1). Never queued.
    Data {
        target: DeploymentTarget,
        buffer: u8,
        payload: std::sync::Arc<[u8]>,
    },
    /// A control request sent at once with wait 0 and 100 ms, as the input
    /// gate writers do (`0x2f` VA `0x514e58`, `0x31` `0x5187fc`, `0x32`
    /// `0x514ee0`). Never queued.
    Control(tetrad_dsp_protocol::controls::ControlRequest),
    /// A request sent at once with its stock wait and timeout
    /// (`Request::send_policy`), such as the sidechain routes `0x48`/`0x49`
    /// (VA `0x5156b8`, wait 0, 1000 ms). Never queued.
    Request(tetrad_dsp_protocol::requests::Request),
    /// A tuner request sent at once with wait 1 and 100 ms, as the tuner
    /// path writer does (`0x34`, VA `0x87590c`). Never queued.
    Tuner(tetrad_dsp_protocol::tuner_looper::TunerLooperRequest),
    /// `syncAllCores`, then a pause (VA `0x5843a4`: 25 ms after it silences
    /// a sidechain sink router outside bulk mode).
    SyncAllCores { settle: std::time::Duration },
}

impl DspWrite {
    pub(crate) fn parameter(target: DeploymentTarget, index: u32, value: f32) -> Self {
        Self::Parameter {
            target,
            index,
            value_bits: value.to_bits(),
        }
    }
}

/// Stock tables and captures are large or proprietary, so `StockData` and
/// `Data` print their length, never their bytes.
impl std::fmt::Debug for DspWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parameter {
                target,
                index,
                value_bits,
            } => f
                .debug_struct("Parameter")
                .field("target", target)
                .field("index", index)
                .field("value_bits", value_bits)
                .finish(),
            Self::IntParameter {
                target,
                index,
                value,
            } => f
                .debug_struct("IntParameter")
                .field("target", target)
                .field("index", index)
                .field("value", value)
                .finish(),
            Self::Bypass { target, state } => f
                .debug_struct("Bypass")
                .field("target", target)
                .field("state", state)
                .finish(),
            Self::StockData {
                target,
                buffer,
                payload,
            } => f
                .debug_struct("StockData")
                .field("target", target)
                .field("buffer", buffer)
                .field("len", &payload.len())
                .finish(),
            Self::Data {
                target,
                buffer,
                payload,
            } => f
                .debug_struct("Data")
                .field("target", target)
                .field("buffer", buffer)
                .field("len", &payload.len())
                .finish(),
            Self::Control(request) => f.debug_tuple("Control").field(request).finish(),
            Self::Request(request) => f.debug_tuple("Request").field(request).finish(),
            Self::Tuner(request) => f.debug_tuple("Tuner").field(request).finish(),
            Self::SyncAllCores { settle } => f
                .debug_struct("SyncAllCores")
                .field("settle", settle)
                .finish(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SetupError {
    /// The stock driver for this model is not recovered. Stock converts or
    /// computes some of its values on the ARM, so a generic push is wrong.
    #[error("model {model_id}: stock {driver:?} driver is not recovered")]
    UnsupportedDriver { model_id: u32, driver: DriverKind },
    /// One stock driver path is not recovered: `va` is its stock function,
    /// `missing` the state or event source it needs.
    #[error("model {model_id}: {driver:?} path at VA {va:#x} is not recovered: {missing}")]
    UnrecoveredPath {
        model_id: u32,
        driver: DriverKind,
        va: u32,
        missing: &'static str,
    },
    /// The driver reads stock tables (constructor or NOTE tables, table amp
    /// words, media tables); the factory has none ([`Factory::tables`]).
    #[error("model {model_id}: {driver:?} driver needs the stock tables of /opt/neuraldsp/ZenUI")]
    MissingTables { model_id: u32, driver: DriverKind },
    /// The tables are loaded, but a driver's range is unmapped or has the
    /// wrong length: a code error, not a missing image.
    #[error("model {model_id}: {driver:?} stock table read failed")]
    StockTable {
        model_id: u32,
        driver: DriverKind,
        #[source]
        source: TableError,
    },
    #[error("model {model_id}: capture loading (VA 0x334390) is not recovered")]
    CaptureLoad { model_id: u32 },
    #[error("model {model_id}: FX Loop driver needs the loop's send placement")]
    MissingFxLoopSend { model_id: u32 },
    /// The driver converts with the block's `ParameterInfo` at `index`; the
    /// catalog has no such parameter.
    #[error("model {model_id}: driver needs catalog parameter {index}")]
    MissingCatalog { model_id: u32, index: u32 },
    /// Looper routers come from the compiler's looper send and return
    /// placements (`+0x694`, `+0x6a4`).
    #[error("model {model_id}: Looper driver needs its router placements")]
    MissingLooperPlacement { model_id: u32 },
    /// Stock lists end with MUTE and BYPASS, so a block has at least two.
    #[error("model {model_id}: {count} values; MUTE and BYPASS are missing")]
    MissingBypassPair { model_id: u32, count: usize },
    /// The value count comes from the first push.
    #[error("model {model_id}: edit before the block was pushed")]
    EditBeforePush { model_id: u32 },
    #[error("model {model_id}: index {index} is outside its {count} values")]
    IndexOutOfRange {
        model_id: u32,
        index: u32,
        count: usize,
    },
    /// MUTE and BYPASS go through [`Block::set_bypass`] (`0x0c`), never `0x02`.
    #[error("model {model_id}: index {index} is MUTE or BYPASS; use set_bypass")]
    BypassParameter { model_id: u32, index: u32 },
}

/// One deployed grid block and its stock driver (the factory's table at
/// controller `+0x88fc`, `[4][8]`).
#[derive(Debug)]
pub struct Block {
    identity: ModelIdentity,
    driver: Option<Box<dyn Driver>>,
    /// Runtime value count, known after the first push.
    value_count: Option<usize>,
}

impl Block {
    /// Stock factory step for one block (VA `0x50eef8`): picks the driver
    /// and appends its constructor writes. `catalog` is the block's parameter
    /// info in index order (ModelRepo ranges; the sidechain SOURCE with its
    /// per-block step list); only the Reverb catalog models and the PCOMSkews
    /// blend models read it. A driver that needs the stock tables is refused
    /// without them ([`SetupError::MissingTables`]); unrecovered drivers are
    /// refused. Nothing is appended on a refusal.
    pub fn build(
        identity: ModelIdentity,
        catalog: &[CatalogParameter],
        factory: &Factory<'_>,
        out: &mut Vec<DspWrite>,
    ) -> Result<Self, SetupError> {
        let start = out.len();
        let built = drivers::create(&identity, catalog, factory, out).and_then(|driver| {
            let stock = out[start..]
                .iter()
                .any(|write| matches!(write, DspWrite::StockData { .. }));
            match driver {
                Some(driver) if stock && factory.tables.is_none() => {
                    Err(SetupError::MissingTables {
                        model_id: identity.model_id,
                        driver: driver.kind(),
                    })
                }
                driver => Ok(Self::with_driver(identity, driver)),
            }
        });
        if built.is_err() {
            out.truncate(start);
        }
        built
    }

    fn with_driver(identity: ModelIdentity, driver: Option<Box<dyn Driver>>) -> Self {
        Self {
            identity,
            driver,
            value_count: None,
        }
    }

    pub fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    pub fn driver_kind(&self) -> Option<DriverKind> {
        self.driver.as_ref().map(|driver| driver.kind())
    }

    /// Stock block push (VA `0x5174a8`): bypass first, then every value in
    /// index order except MUTE and BYPASS (the last two), then the driver's
    /// after-push hook. `values` is the effective list (expression-active
    /// values already substituted, VA `0x3221e8`).
    pub fn push(
        &mut self,
        values: &[SetupValue],
        bypass: BlockBypass,
        sidechain_router: Option<DeploymentTarget>,
        out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        let model_id = self.identity.model_id;
        let count = values.len();
        if count < 2 {
            return Err(SetupError::MissingBypassPair { model_id, count });
        }
        // Captures load in the push; only `Block::with_media` drivers can.
        if CAPTURE_MODELS.contains(&model_id)
            && !self
                .driver
                .as_ref()
                .is_some_and(|driver| driver.loads_capture())
        {
            return Err(SetupError::CaptureLoad { model_id });
        }
        self.value_count = Some(count);
        if let Some(driver) = &mut self.driver {
            driver.begin_push();
        }
        self.set_bypass(bypass, sidechain_router, out);
        let target = self.identity.target;
        for (index, value) in (0_u32..).zip(&values[..count - 2]) {
            match *value {
                SetupValue::Float(bits) => {
                    let send = self.driver.as_mut().is_none_or(|driver| {
                        driver.update_parameter(
                            index,
                            f32::from_bits(bits),
                            ParamSetMode::Push,
                            out,
                        )
                    });
                    if send {
                        out.push(DspWrite::Parameter {
                            target,
                            index,
                            value_bits: bits,
                        });
                    }
                }
                SetupValue::Int(value) => out.push(DspWrite::IntParameter {
                    target,
                    index,
                    value,
                }),
                SetupValue::UInt(_) | SetupValue::Text | SetupValue::Meter => {}
            }
        }
        if let Some(driver) = &mut self.driver {
            driver.parameters_pushed(out);
        }
        Ok(())
    }

    /// Stock bypass write (VA `0x517228`): the driver may suppress `0x0c`;
    /// an active sidechain router then gets `0x03` parameter 5 = engaged.
    pub fn set_bypass(
        &mut self,
        bypass: BlockBypass,
        sidechain_router: Option<DeploymentTarget>,
        out: &mut Vec<DspWrite>,
    ) {
        let target = self.identity.target;
        let send = self
            .driver
            .as_mut()
            .is_none_or(|driver| driver.bypass(bypass, out));
        if send {
            out.push(DspWrite::Bypass {
                target,
                state: bypass.state(),
            });
        }
        if let Some(router) = sidechain_router
            && !SIDECHAIN_BYPASS_EXEMPT.contains(&self.identity.model_id)
        {
            out.push(DspWrite::IntParameter {
                target: router,
                index: SIDECHAIN_ACTIVE_PARAMETER,
                value: u32::from(!bypass.bypassed),
            });
        }
    }

    /// Stock single edit (VA `0x51e4d8`); scene recall (VA `0x527248`)
    /// sends each value this way with [`ParamSetMode::Recall`] inside bulk
    /// mode `Value8`, then calls [`Self::recalled`]. MUTE and BYPASS (the
    /// last two indices) are refused: stock sends them as `0x0c` through
    /// [`Self::set_bypass`]. A sidechain SOURCE or `sidechain_in` value goes
    /// through the driver like any other; the caller re-routes the sink first
    /// (VAs `0x5843a4`, `0x583028`). A [`ParamSetMode::Live`] float edit then
    /// runs the driver's `+0x60` hook (`0x523644`).
    pub fn edit(
        &mut self,
        index: u32,
        value: SetupValue,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        self.write(index, value, mode, mode == ParamSetMode::Live, out)
    }

    /// A live float write of an expression move (`0x520074`): [`Self::edit`]
    /// without the `+0x60` hook. Expression does not reach the controller
    /// write `0x523644` (r14-misc), so a driver writes nothing into the preset.
    pub fn expression_edit(
        &mut self,
        index: u32,
        value: f32,
        out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        let value = SetupValue::Float(value.to_bits());
        self.write(index, value, ParamSetMode::Live, false, out)
    }

    fn write(
        &mut self,
        index: u32,
        value: SetupValue,
        mode: ParamSetMode,
        controller: bool,
        out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        let model_id = self.identity.model_id;
        let count = self
            .value_count
            .ok_or(SetupError::EditBeforePush { model_id })?;
        // MUTE is `count − 2`, BYPASS `count − 1`.
        match (index as usize).saturating_add(2) {
            end if end < count => {}
            end if end - 1 <= count => {
                return Err(SetupError::BypassParameter { model_id, index });
            }
            _ => {
                return Err(SetupError::IndexOutOfRange {
                    model_id,
                    index,
                    count,
                });
            }
        }
        let target = self.identity.target;
        match value {
            SetupValue::Float(bits) => {
                let value = f32::from_bits(bits);
                let send = self
                    .driver
                    .as_mut()
                    .is_none_or(|driver| driver.update_parameter(index, value, mode, out));
                if send {
                    out.push(DspWrite::Parameter {
                        target,
                        index,
                        value_bits: bits,
                    });
                }
                // The controller write `0x523644`: `+0x60` last.
                if controller && let Some(driver) = &mut self.driver {
                    driver.parameter_edited(index, value, out);
                }
            }
            SetupValue::UInt(value) => out.push(DspWrite::IntParameter {
                target,
                index,
                value,
            }),
            SetupValue::Int(_) | SetupValue::Text | SetupValue::Meter => {}
        }
        Ok(())
    }

    /// End of one block in scene recall (VA `0x526cc0`, after its values):
    /// the driver's scene hook (`+0x64`), then its after-push hook (`+0x48`).
    /// Drivers that only store values in recall mode send them here.
    pub fn recalled(&mut self, scene_value: SceneValue<'_>, out: &mut Vec<DspWrite>) {
        if let Some(driver) = &mut self.driver {
            driver.scene_recalled(scene_value, out);
            driver.parameters_pushed(out);
        }
    }

    /// A driver timer the runtime must (re)start after a push, edit, bypass
    /// or recall of this block; then it calls [`Self::timer_elapsed`]. The
    /// slot's writes go out at once, outside bulk mode.
    pub fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.driver.as_mut().and_then(|driver| driver.take_timer())
    }

    pub fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        if let Some(driver) = &mut self.driver {
            driver.timer_elapsed(out);
        }
    }

    /// A press (`+0x40`, `0x5239d0`) or release (`+0x44`, `0x523f80`) of a
    /// stomp the block is assigned to. Stock calls it before the block's own
    /// stomp handling, which it skips when
    /// [`DriverKind::takes_footswitch`]. The writes go out at once.
    pub fn footswitch(&mut self, pressed: bool, out: &mut Vec<DspWrite>) {
        if let Some(driver) = &mut self.driver {
            if pressed {
                driver.footswitch_pressed(out);
            } else {
                driver.footswitch_released(out);
            }
        }
    }

    /// The controller's MIDI dispatch for this block: the driver takes the
    /// message when `+0x4c` is on and `+0x50` is `channel` (1..=16) or
    /// [`MIDI_OMNI`].
    pub fn midi(&mut self, channel: u8, message: DriverMidi, out: &mut Vec<DspWrite>) {
        if let Some(driver) = &mut self.driver
            && driver
                .midi_channel()
                .is_some_and(|listens| listens == i32::from(channel) || listens == MIDI_OMNI)
        {
            driver.midi(message, out);
        }
    }

    /// The driver's preset writes ([`LinkedEdit`]) since the last call, after
    /// a push, edit, recall, footswitch or MIDI message of this block.
    pub fn take_linked_edits(&mut self) -> Vec<LinkedEdit> {
        let mut edits = Vec::new();
        if let Some(driver) = &mut self.driver {
            driver.take_linked_edits(&mut edits);
        }
        edits
    }

    /// The driver's voice notes since the last call ([`Driver::take_voice_notes`]):
    /// `None` when it sent no voices.
    pub fn take_voice_notes(&mut self) -> Option<[Option<u8>; 2]> {
        self.driver.as_mut()?.take_voice_notes()
    }
}

#[cfg(test)]
pub(crate) mod test_factory {
    //! [`Block::build`] for driver tests: synthetic tables, no stock bytes.

    use super::drivers::media::test_media::TestMedia;
    use super::*;

    impl DriverTables {
        /// No driver payload ranges; zero NOTE tables, synthetic amp words
        /// and the default test media tables.
        pub(crate) fn synthetic() -> Self {
            Self {
                elf: ElfTables::empty(),
                notes: NoteTables::default(),
                amps: drivers::amps::tests::tables(),
                media: TestMedia::default().stock,
            }
        }
    }

    /// `Block::build` with `media` as the library and its stock tables.
    pub(crate) fn build_with(
        identity: ModelIdentity,
        catalog: &[CatalogParameter],
        media: &TestMedia,
        out: &mut Vec<DspWrite>,
    ) -> Result<Block, SetupError> {
        let tables = DriverTables {
            media: media.stock.clone(),
            ..DriverTables::synthetic()
        };
        let factory = Factory {
            tables: Some(&tables),
            library: media,
            mini: false,
        };
        Block::build(identity, catalog, &factory, out)
    }

    /// `Block::build` with the default test media and no catalog.
    pub(crate) fn build(
        identity: ModelIdentity,
        out: &mut Vec<DspWrite>,
    ) -> Result<Block, SetupError> {
        build_with(identity, &[], &TestMedia::default(), out)
    }
}

#[cfg(test)]
mod tests;
