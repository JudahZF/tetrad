//! Injected media for the cab, IR Loader and capture drivers: the app's IR and capture library
//! ([`MediaLibrary`]) and the stock tables those drivers embed ([`MediaStock`], read by site VA
//! from the stock ZenUI).
//!
//! [`Block::build`] builds these drivers; [`Block::set_text`] is the string
//! path (`ir selector`, `IR PATH`, `file_name`) that stock handles in its IR
//! and capture managers.

use std::sync::Arc;

use tetrad_media::block::{REVERB_TABLE_BYTES, ReverbTable};
use tetrad_media::cab::{CAB_IR_WORDS, CabIr, PETRUCCI_ROOM_IRS, StockCabData};
use tetrad_media::ir::IrSamples;

use super::super::tables::{Segment, StockPayload, StockTables, TableError};
use super::super::{Block, DspWrite, ParamSetMode, SetupError};

/// Model 14000 (`nc_player`, V1 captures) has no stock driver.
pub const CAPTURE_V1_MODEL: u32 = 14000;

/// What a cab `ir selector` names (stock IR manager VA `0x3289c4`).
#[derive(Debug, Clone)]
pub enum CabIrs {
    /// A factory entity: its catalog tag and every file, decrypted, in
    /// catalog order (`0x3284fc`).
    Factory { tag: String, irs: Vec<Arc<CabIr>> },
    /// A user IR product (`0x327688`), loaded with tag `Single`.
    Custom(Arc<CabIr>),
    /// Neither: stock logs "Cannot open IR" and loads the built-in default
    /// IR (`0x51284c`).
    Missing,
}

/// A capture library record as the DSP takes it (record `+0x78`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureData {
    /// Record version other than `"2"` (`0x51cef8`).
    V1(Arc<[u8]>),
    /// Version `"2"`: the `nc2_player` blob (identity LE ‖ data, `0x5128f4`).
    V2(Arc<[u8]>),
}

/// The app's media library. Every byte it returns is already decrypted and
/// decoded; key material stays with the app.
pub trait MediaLibrary {
    /// ModelRepo `plugin_id` of a model (artist packs). Cab drivers test it
    /// for their own block (VA `0x2ed194`: model `+0xf8 != -1`).
    fn plugin_id(&self, model_id: u32) -> Option<u32>;
    /// Resolves a cab `ir selector` value (factory entity or user IR id).
    fn cab_irs(&self, selector: &str) -> CabIrs;
    /// A user IR product by id for the IR Loader (`0x5599a0`). The words are
    /// the product's `rawdata`; [`IrSamples`] keeps at most 1,024.
    fn ir_product(&self, product_id: &str) -> Option<IrSamples>;
    /// A capture library record by key (`0x6f6484`). The key is the block's
    /// `file_name` cut to 64 UTF-16 units (`QString::left(64)`, VA `0x517cac`).
    fn capture(&self, key: &str) -> Option<CaptureData>;
}

/// What the cab and IR Loader constructors read: the stock tables and the
/// app's library.
pub(super) struct Media<'a> {
    pub stock: &'a MediaStock,
    pub library: &'a dyn MediaLibrary,
}

macro_rules! segment {
    ($va:expr, $len:expr) => {
        StockPayload::new(&[Segment { va: $va, len: $len }])
    };
}

const IR_BYTES: u32 = (CAB_IR_WORDS * 4) as u32;
const TABLE_BYTES: u32 = REVERB_TABLE_BYTES as u32;
/// Selector ctor default node (VA `0x56e9e8`).
const DEFAULT_IR: StockPayload = segment!(0x00f5_5780, IR_BYTES);
/// Petrucci room IRs (VA `0x553ed8`).
const PETRUCCI_ROOMS: StockPayload = segment!(0x00a4_fc00, IR_BYTES * PETRUCCI_ROOM_IRS as u32);
/// Mono cab table, buffer 2 (VA `0x554070`).
const MONO_TABLE: StockPayload = segment!(0x00a5_2c00, TABLE_BYTES);
/// Stereo cab table, buffer 2 (VA `0x5567f0`).
const STEREO_TABLE: StockPayload = segment!(0x00a5_2f10, TABLE_BYTES);
/// IR Loader table, buffer 2 (VA `0x559ed4`).
const IR_LOADER_TABLE: StockPayload = segment!(0x00a5_39b8, TABLE_BYTES);
/// John Mayer cab room SIZE rows (VA `0x554644`): SIZE, PRE DELAY ms, REV
/// LOW PASS Hz, three entries each.
const ROOM_SIZES: [StockPayload; 3] = [
    segment!(0x00a5_2dc0, 12),
    segment!(0x00a5_2dd0, 12),
    segment!(0x00a5_2de0, 12),
];
/// IR Loader SIZE values (VA `0x5596a8` case 20).
const IR_LOADER_SIZES: StockPayload = segment!(0x00a5_3900, 12);

/// Every media table, for [`super::super::tables::zenui_reads`]. [`MediaStock::read`]
/// reads them from the full image, so they are not in [`super::stock_payloads`].
pub(crate) fn payloads() -> impl Iterator<Item = StockPayload> {
    [
        DEFAULT_IR,
        PETRUCCI_ROOMS,
        MONO_TABLE,
        STEREO_TABLE,
        IR_LOADER_TABLE,
        IR_LOADER_SIZES,
    ]
    .into_iter()
    .chain(ROOM_SIZES)
}

/// Stock tables of the media drivers. No bytes live in the repository.
#[derive(Clone)]
pub struct MediaStock {
    pub cab: Arc<StockCabData>,
    pub ir_loader_table: ReverbTable,
    /// `[row][size]`: SIZE, PRE DELAY (ms), REV LOW PASS (Hz).
    pub room_sizes: [[f32; 3]; 3],
    pub ir_loader_sizes: [f32; 3],
}

impl std::fmt::Debug for MediaStock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MediaStock(<stock data>)")
    }
}

fn words(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes
        .chunks_exact(4)
        .map(|word| f32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

fn cab_ir(bytes: &[u8]) -> Arc<CabIr> {
    let mut ir = [0.0; CAB_IR_WORDS];
    for (slot, word) in ir.iter_mut().zip(words(bytes)) {
        *slot = word;
    }
    Arc::new(ir)
}

fn three(bytes: &[u8]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (slot, word) in out.iter_mut().zip(words(bytes)) {
        *slot = word;
    }
    out
}

impl MediaStock {
    /// Reads every table through `tables` (for the device, [`ElfTables`] over
    /// `/opt/neuraldsp/ZenUI`, which checks the running build by SHA-256).
    ///
    /// [`ElfTables`]: super::super::ElfTables
    pub fn read(tables: &impl StockTables) -> Result<Self, TableError> {
        let table = |payload| {
            tables.read(payload).map(|bytes| {
                ReverbTable::new(&bytes).expect("the segment is REVERB_TABLE_BYTES long")
            })
        };
        let rooms = tables.read(PETRUCCI_ROOMS)?;
        let room = |i: usize| cab_ir(&rooms[i * IR_BYTES as usize..]);
        let cab = StockCabData {
            default_ir: cab_ir(&tables.read(DEFAULT_IR)?),
            petrucci_rooms: [room(0), room(1), room(2)],
            mono_reverb: table(MONO_TABLE)?,
            stereo_reverb: table(STEREO_TABLE)?,
        };
        let mut room_sizes = [[0.0; 3]; 3];
        for (row, payload) in room_sizes.iter_mut().zip(ROOM_SIZES) {
            *row = three(&tables.read(payload)?);
        }
        Ok(Self {
            cab: Arc::new(cab),
            ir_loader_table: table(IR_LOADER_TABLE)?,
            room_sizes,
            ir_loader_sizes: three(&tables.read(IR_LOADER_SIZES)?),
        })
    }
}

/// A computed `0x0b` ReceiveData write.
pub(super) fn data(
    target: tetrad_dsp_protocol::DeploymentTarget,
    buffer: u8,
    payload: Vec<u8>,
) -> DspWrite {
    DspWrite::Data {
        target,
        buffer,
        payload: payload.into(),
    }
}

/// The media drivers' single-shot `QTimer`: no interval is set, so it fires
/// on the next event-loop turn (0 ms). `active` is stock's timer-id test.
#[derive(Debug, Default)]
pub(super) struct ZeroTimer {
    active: bool,
    started: bool,
}

impl ZeroTimer {
    pub(super) fn start(&mut self) {
        self.active = true;
        self.started = true;
    }

    pub(super) fn is_active(&self) -> bool {
        self.active
    }

    pub(super) fn fired(&mut self) {
        self.active = false;
    }

    pub(super) fn take(&mut self) -> Option<std::time::Duration> {
        std::mem::take(&mut self.started).then_some(std::time::Duration::ZERO)
    }
}

impl Block {
    /// One string value (QVariant type 10) through the driver's text hook
    /// (`+0x34`) and, for cabs and captures, the stock IR and capture
    /// managers. `mode` is [`ParamSetMode::Push`] for the block's strings
    /// before [`Block::push`], [`ParamSetMode::Recall`] inside a scene
    /// recall (before [`Block::recalled`]) and [`ParamSetMode::Live`] for
    /// edits. Blocks without a driver ignore strings.
    pub fn set_text(
        &mut self,
        index: u32,
        text: &str,
        mode: ParamSetMode,
        library: &dyn MediaLibrary,
        out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        match &mut self.driver {
            Some(driver) => driver.update_text(index, text, mode, library, out),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_media {
    //! Synthetic media for driver tests: no stock bytes.

    use std::collections::HashMap;

    use tetrad_media::ir::{IrInputFormat, IrLimits, decode};

    use super::*;

    pub fn samples(words: &[f32]) -> IrSamples {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let limits = IrLimits {
            max_asset_bytes: 1 << 16,
            max_chunks: 0,
        };
        decode(&bytes, IrInputFormat::RawLeFloat32, limits).unwrap()
    }

    /// An IR with one tap of `gain` at word `at`.
    pub fn impulse(at: usize, gain: f32) -> Arc<CabIr> {
        let mut ir = [0.0; CAB_IR_WORDS];
        ir[at] = gain;
        Arc::new(ir)
    }

    #[derive(Debug)]
    pub struct TestMedia {
        pub stock: MediaStock,
        pub plugins: HashMap<u32, u32>,
        pub cabs: HashMap<String, CabIrs>,
        pub products: HashMap<String, IrSamples>,
        pub captures: HashMap<String, CaptureData>,
    }

    impl Default for TestMedia {
        fn default() -> Self {
            let table = |fill: u8| ReverbTable::new(&[fill; REVERB_TABLE_BYTES]).unwrap();
            let cab = StockCabData {
                default_ir: impulse(0, 0.5),
                petrucci_rooms: [impulse(1, 1.0), impulse(2, 1.0), impulse(3, 1.0)],
                mono_reverb: table(0x11),
                stereo_reverb: table(0x22),
            };
            Self {
                stock: MediaStock {
                    cab: Arc::new(cab),
                    ir_loader_table: table(0x33),
                    room_sizes: [
                        [0.0, 1.0, 2.0],
                        [10.0, 50.0, 90.0],
                        [1000.0, 8000.0, 16000.0],
                    ],
                    ir_loader_sizes: [0.1, 0.5, 1.0],
                },
                plugins: HashMap::new(),
                cabs: HashMap::new(),
                products: HashMap::new(),
                captures: HashMap::new(),
            }
        }
    }

    impl MediaLibrary for TestMedia {
        fn plugin_id(&self, model_id: u32) -> Option<u32> {
            self.plugins.get(&model_id).copied()
        }
        fn cab_irs(&self, selector: &str) -> CabIrs {
            self.cabs.get(selector).cloned().unwrap_or(CabIrs::Missing)
        }
        fn ir_product(&self, product_id: &str) -> Option<IrSamples> {
            self.products.get(product_id).cloned()
        }
        fn capture(&self, key: &str) -> Option<CaptureData> {
            self.captures.get(key).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use tetrad_media::block::ReverbTable;
    use tetrad_media::cab::StockCabData;

    use super::*;
    use crate::setup::ElfTables;

    /// The VA reads match tetrad-media's file-offset slices of the same pinned
    /// image. Comparisons never print stock bytes.
    #[test]
    #[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
    fn stock_tables_resolve_in_the_pinned_zenui() {
        let Some(path) = crate::corpus("rootfs/opt/neuraldsp/ZenUI") else {
            return;
        };
        let image = std::fs::read(path).unwrap();
        let by_offset = StockCabData::from_stock_zenui(&image).unwrap();
        let ir_loader_table = ReverbTable::from_stock_zenui(&image).unwrap();
        let stock = MediaStock::read(&ElfTables::new(image).unwrap()).unwrap();
        assert!(stock.cab.default_ir == by_offset.default_ir);
        assert!(stock.cab.petrucci_rooms == by_offset.petrucci_rooms);
        assert!(stock.cab.mono_reverb == by_offset.mono_reverb);
        assert!(stock.cab.stereo_reverb == by_offset.stereo_reverb);
        assert!(stock.ir_loader_table == ir_loader_table);
        let rows = stock.room_sizes.iter().chain([&stock.ir_loader_sizes]);
        assert!(rows.flatten().all(|v| v.is_finite()));
    }
}
