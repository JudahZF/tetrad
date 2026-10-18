//! IR Loader blocks: model layouts and typed DSP transfer plans.
//!
//! `zenUI::IRLoaderDriver` (ctor VA 0x55a138) keeps one 4,100-byte record per
//! IR (1,024 words and a count). The ctor sends the configuration (argument 3)
//! and, on non-Lite models, the reverb table (argument 2). `loadIR(path, channel)`
//! (VA 0x5599a0) resolves the `IR PATH` product id through the library; a found
//! product copies `min(count, 1024)` words, a missing one stores count 0. Only a
//! loaded channel is marked dirty, and a timer then sends each dirty record (VA
//! 0x559654 → 0x51965c). The ctor default record is never sent. Plans here are
//! inert: `tetrad-dsp-protocol` owns framing and `tetrad-engine` owns core/slot.

use crate::ir::{IrChannel, IrSamples, IrTransferPlan};

/// DSP message type for IR words and IR configuration (`ReceiveData`).
pub const RECEIVE_DATA_MESSAGE: u8 = 0x0b;
/// Generic-header argument of the IR configuration message (VA 0x55989c).
pub const IR_CONFIG_ARGUMENT: u8 = 3;
/// Generic-header argument of the reverb table message (VA 0x559ed4).
pub const REVERB_TABLE_ARGUMENT: u8 = 2;
/// Reverb table length: two contiguous rodata copies of 0x100 and 0xc0 bytes.
pub const REVERB_TABLE_BYTES: usize = 0x1c0;
/// `IR PATH` string parameters (product ids) for slot A and slot B.
pub const IR_PATH_PARAMS: [u32; 2] = [2, 10];
/// Hidden `IR NAME` display parameters for slot A and slot B.
pub const IR_NAME_PARAMS: [u32; 2] = [22, 23];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IrLoaderModel {
    SingleMono = 29001,
    SingleStereo = 29002,
    DualMono = 29003,
    DualStereo = 29004,
    SingleMonoLite = 29005,
    SingleStereoLite = 29006,
    DualMonoLite = 29007,
    DualStereoLite = 29008,
}

impl IrLoaderModel {
    pub const ALL: [Self; 8] = [
        Self::SingleMono,
        Self::SingleStereo,
        Self::DualMono,
        Self::DualStereo,
        Self::SingleMonoLite,
        Self::SingleStereoLite,
        Self::DualMonoLite,
        Self::DualStereoLite,
    ];

    pub fn from_model_id(id: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|model| *model as u32 == id)
    }

    pub fn model_id(self) -> u32 {
        self as u32
    }

    /// Two IR records for Dual models (VA 0x5597e4: `id - 29003` in mask 0x33).
    pub fn ir_count(self) -> u32 {
        match self {
            Self::DualMono | Self::DualStereo | Self::DualMonoLite | Self::DualStereoLite => 2,
            _ => 1,
        }
    }

    /// Output channels for (ST) models (VA 0x55989c: `id - 29002` in mask 0x55).
    pub fn output_channels(self) -> u32 {
        match self {
            Self::SingleStereo
            | Self::DualStereo
            | Self::SingleStereoLite
            | Self::DualStereoLite => 2,
            _ => 1,
        }
    }

    /// Lite models use the `irloaderlite` blob, hide the room reverb and skip
    /// the reverb table (ctor flag `id - 29005 > 3`).
    pub fn is_lite(self) -> bool {
        self.model_id() >= Self::SingleMonoLite.model_id()
    }

    /// Setter VA 0x559db4: the slot A path loads channel 0; the slot B path
    /// loads channel 1 only on Dual models. Every other parameter is not an IR load.
    pub fn channel_for_param(self, param: u32) -> Option<IrChannel> {
        if param == IR_PATH_PARAMS[0] {
            Some(IrChannel::Channel0)
        } else if param == IR_PATH_PARAMS[1] && self.ir_count() == 2 {
            Some(IrChannel::Channel1)
        } else {
            None
        }
    }

    pub fn config(self) -> IrLoaderConfig {
        IrLoaderConfig {
            ir_count: self.ir_count(),
            output_channels: self.output_channels(),
        }
    }
}

/// 8-byte configuration body sent with argument [`IR_CONFIG_ARGUMENT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrLoaderConfig {
    pub ir_count: u32,
    pub output_channels: u32,
}

impl IrLoaderConfig {
    pub fn payload(self) -> [u8; 8] {
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&self.ir_count.to_le_bytes());
        bytes[4..].copy_from_slice(&self.output_channels.to_le_bytes());
        bytes
    }
}

/// One `ReceiveData` (0x0b) body for a block: generic-header argument (IR
/// channel, table or configuration address) and payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataTransfer {
    pub argument: u8,
    pub payload: Vec<u8>,
}

impl From<&IrTransferPlan> for DataTransfer {
    fn from(plan: &IrTransferPlan) -> Self {
        Self {
            argument: plan.argument(),
            payload: plan.payload().to_vec(),
        }
    }
}

/// A 448-byte stock table (IR Loader and cab "reverb" data), injected from the
/// device's own ZenUI. Its contents are opaque.
#[derive(Clone, PartialEq, Eq)]
pub struct ReverbTable(Box<[u8; REVERB_TABLE_BYTES]>);

impl ReverbTable {
    pub fn new(bytes: &[u8]) -> Result<Self, IrBlockError> {
        let table: [u8; REVERB_TABLE_BYTES] = bytes
            .try_into()
            .map_err(|_| IrBlockError::ReverbTableSize(bytes.len()))?;
        Ok(Self(Box::new(table)))
    }

    /// Slices the IR Loader table from the stock ZenUI image of the running build
    /// (`MediaOffsets::reverb_table`; 4.1.0 VA 0xa539b8). The bytes are stock data and are
    /// never embedded. This does not verify the build.
    pub fn from_stock_zenui(image: &[u8]) -> Result<Self, IrBlockError> {
        Self::from_stock_zenui_at(image, tetrad_firmware::running().zenui.media.reverb_table)
    }

    pub fn from_stock_zenui_at(image: &[u8], offset: usize) -> Result<Self, IrBlockError> {
        image
            .get(offset..offset + REVERB_TABLE_BYTES)
            .ok_or(IrBlockError::ZenUiTooShort(image.len()))
            .and_then(Self::new)
    }

    pub fn payload(&self) -> &[u8] {
        &self.0[..]
    }

    pub fn transfer(&self, argument: u8) -> DataTransfer {
        DataTransfer {
            argument,
            payload: self.payload().to_vec(),
        }
    }
}

impl std::fmt::Debug for ReverbTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReverbTable(448 stock bytes)")
    }
}

/// What a slot holds. `Default` is the ctor record, which stock never sends.
/// `Missing` is the count-zero record for an unknown or deleted product id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrSlotContent<'a> {
    Default,
    Loaded(&'a IrSamples),
    Missing,
}

/// The full DSP load for one IR Loader block in stock order: configuration,
/// the reverb table on non-Lite models, then one `ReceiveData` per loaded record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrLoaderLoadPlan {
    pub model: IrLoaderModel,
    pub config: IrLoaderConfig,
    pub reverb_table: Option<ReverbTable>,
    pub transfers: Vec<IrTransferPlan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IrBlockError {
    #[error("IR Loader model {model:?} has {expected} IR slots, got {actual}")]
    SlotCount {
        model: IrLoaderModel,
        expected: usize,
        actual: usize,
    },
    #[error("non-Lite IR Loader model {0:?} needs the stock reverb table")]
    MissingReverbTable(IrLoaderModel),
    #[error("reverb table must be {REVERB_TABLE_BYTES} bytes, got {0}")]
    ReverbTableSize(usize),
    #[error("ZenUI image has {0} bytes; too short for the reverb table")]
    ZenUiTooShort(usize),
}

impl IrLoaderLoadPlan {
    /// `slots[i]` is channel `i`. Default slots send nothing. Missing slots send
    /// an empty payload, which matches the stock count-zero record; its audio
    /// effect is unverified. Lite models ignore `reverb_table`; others require it.
    pub fn new(
        model: IrLoaderModel,
        reverb_table: Option<&ReverbTable>,
        slots: &[IrSlotContent<'_>],
    ) -> Result<Self, IrBlockError> {
        if slots.len() != model.ir_count() as usize {
            return Err(IrBlockError::SlotCount {
                model,
                expected: model.ir_count() as usize,
                actual: slots.len(),
            });
        }
        let reverb_table = match (model.is_lite(), reverb_table) {
            (true, _) => None,
            (false, Some(table)) => Some(table.clone()),
            (false, None) => return Err(IrBlockError::MissingReverbTable(model)),
        };
        let transfers = slots
            .iter()
            .zip([IrChannel::Channel0, IrChannel::Channel1])
            .filter_map(|(slot, channel)| match slot {
                IrSlotContent::Default => None,
                IrSlotContent::Loaded(samples) => Some(IrTransferPlan::new(samples, channel)),
                IrSlotContent::Missing => Some(IrTransferPlan::empty(channel)),
            })
            .collect();
        Ok(Self {
            model,
            config: model.config(),
            reverb_table,
            transfers,
        })
    }

    /// Every message body in stock send order.
    pub fn messages(&self) -> Vec<DataTransfer> {
        let config = DataTransfer {
            argument: IR_CONFIG_ARGUMENT,
            payload: self.config.payload().to_vec(),
        };
        std::iter::once(config)
            .chain(
                self.reverb_table
                    .iter()
                    .map(|table| table.transfer(REVERB_TABLE_ARGUMENT)),
            )
            .chain(self.transfers.iter().map(DataTransfer::from))
            .collect()
    }
}
