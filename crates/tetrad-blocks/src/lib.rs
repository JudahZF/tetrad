//! Owned DSP blocks for the SHARC: each block is one ISA48 body wrapped as a stock-ABI module blob
//! (and later as a resident kernel).
#![forbid(unsafe_code)]

pub mod blob;
pub mod block;
pub mod blocks;
pub mod dsp;
pub mod verification;

pub use blob::{BuiltModule, Callback, LinkedModule, ModuleStats, SectionAddresses};
pub use block::{
    BLOCK_SAMPLES, BlockError, HostReference, InstanceLayout, MAX_DM_BYTES, OwnedBlock,
    ParameterLaw, ParameterSpec, RoutineLabels, StaticLayout,
};
