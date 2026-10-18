//! The leaf contract shared by the stock and resident adapters (mono by default).

use std::slice;

use qc_format::FormatError;
use tetrad_sharc::address::AddressError;
use tetrad_sharc::emit::{EmitError, IsaProgram, Label};
use tetrad_sharc::isa48::EncodeError;

pub const BLOCK_SAMPLES: usize = 32;
/// Static DM that one core's pool holds (`tetrad-engine` `CORE_TEMPLATE`).
pub const MAX_DM_BYTES: u32 = 59_392;

#[derive(Debug, thiserror::Error)]
pub enum BlockError {
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error(transparent)]
    Format(#[from] FormatError),
    #[error("invalid owned block: {0}")]
    Invalid(&'static str),
    #[error("ISA body changes when its origin moves or uses an absolute address")]
    PositionDependentBody,
    #[error("routine label {0:?} is missing")]
    MissingRoutine(Label),
    #[error("owned block size exceeds its address space")]
    SizeOverflow,
}

/// How a block reads one delivered value v in 0..=1. ZenUI sends every
/// parameter as a normalized `0x02` float: switches as 0/1, an n-step rotary
/// as k/(n − 1). Owned blocks install no `ctx[4]`, so the kernel drops `0x03`
/// integers, as for the five stock models (ABI record §3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParameterLaw {
    /// `minimum + (maximum − minimum)·v^(1/skew)`: the ZenUI display law
    /// (ZenUI `0x64b8f8`); skew 1 is linear.
    Range {
        minimum: f32,
        maximum: f32,
        skew: f32,
    },
    /// On when v > 0.5 (stock Volume CURVE, Gain PHASE INVERT).
    Switch,
    /// Choice `floor((n − 1)·v + 0.5)` of n names (stock Low-High Cut slopes).
    Choices(&'static [&'static str]),
    /// Not recovered; the reason names the open evidence.
    Unresolved(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParameterSpec {
    pub runtime_index: u32,
    pub name: &'static str,
    pub law: ParameterLaw,
}

/// Initial section-1 state. The shape selects the adapter (ABI record §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticLayout {
    /// Mono leaves: channel A's state, then channel B's. Equal sizes; the
    /// values can differ, for example a channel-role word for a pan law. The
    /// adapter calls every leaf for A, then for B.
    Channels([Vec<u32>; 2]),
    /// A stereo block (stock shape): one state. The adapter calls each leaf
    /// once, and process gets the `io` descriptor.
    Stereo(Vec<u32>),
}

/// Section-1 layout: the first state at byte 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceLayout {
    /// Words of each state (a channel state, or the one stereo state).
    pub channel_state_words: u32,
    /// Channel B's state; `None` for the one state of a stereo block.
    pub channel_b_offset_bytes: Option<u32>,
    /// Padded to 16 bytes. A stateless block still has one word per state.
    pub dm_bytes: u32,
}

impl StaticLayout {
    /// The states in DM order: A, then B, or the one stereo state.
    pub fn states(&self) -> &[Vec<u32>] {
        match self {
            Self::Channels(channels) => channels,
            Self::Stereo(state) => slice::from_ref(state),
        }
    }

    pub fn states_mut(&mut self) -> &mut [Vec<u32>] {
        match self {
            Self::Channels(channels) => channels,
            Self::Stereo(state) => slice::from_mut(state),
        }
    }

    pub fn materialize(&self) -> Result<(InstanceLayout, Vec<u8>), BlockError> {
        let states = self.states();
        let words = states[0].len();
        if states.iter().any(|state| state.len() != words) {
            return Err(BlockError::Invalid("channel states must have equal sizes"));
        }
        let words = u32::try_from(words).map_err(|_| BlockError::SizeOverflow)?;
        let stride = words
            .max(1)
            .checked_mul(4)
            .ok_or(BlockError::SizeOverflow)?;
        let dm_bytes = stride
            .checked_mul(states.len() as u32)
            .and_then(|size| size.checked_add(15))
            .ok_or(BlockError::SizeOverflow)?
            & !15;
        if dm_bytes > MAX_DM_BYTES {
            return Err(BlockError::Invalid(
                "static DM state exceeds one core's pool",
            ));
        }
        let mut data = vec![0; dm_bytes as usize];
        for (k, state) in states.iter().enumerate() {
            for (index, value) in state.iter().enumerate() {
                let start = k * stride as usize + 4 * index;
                data[start..start + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        let layout = InstanceLayout {
            channel_state_words: words,
            channel_b_offset_bytes: matches!(self, Self::Channels(_)).then_some(stride),
            dm_bytes,
        };
        Ok((layout, data))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineLabels {
    pub process: Label,
    pub set_parameter: Label,
    pub reset: Label,
}

/// One owned block: an ISA48 body with three leaf routines.
///
/// Leaf ABI (adapter A calls each leaf once per state of the
/// [`StaticLayout`]: for channel A, then B, or once for a stereo state):
/// - R4 = I4 = the state. Under adapter A it is a byte-space DM address; use
///   normal-word DAG accesses so offsets scale with the pointer.
/// - process: R8 = the channel's 32 samples, in place. A stereo process gets
///   R8 = io, whose two pointers name A and B.
/// - set-parameter: R8 = runtime index,
///   F12 = value, finite and in 0..=1 (adapter A drops other bits; stock does
///   not clamp).
/// - reset: R8 = the kernel's flag (1). Fault recovery calls it with R8 = 0
///   after the NaN guard fired, so it must then clear all history. A leaf
///   that always clears all history can ignore R8.
/// - Modes are the kernel's at callback entry (ABI record §3): RND32 = 1,
///   CBUFEN = 1, PEYEN/ALUSAT/TRUNCATE/BDCST/BR = 0. With CBUFEN set every L
///   register must stay 0. Do not write MODE1, with one exception (the stock
///   cascades: k1 `0x162c50`/`0x162ce3`, Graphic-9 `0x991`/`0x9bf`): a leaf
///   may clear RND32 inside its body (`BIT CLR MODE1 RND32; NOP`) for 40-bit
///   computes. It must set RND32 again (`BIT SET MODE1 RND32; NOP`) before it
///   calls a routine or returns, so callees and later callbacks see the entry
///   state. An interrupt inside the bracket also does: the C dispatcher (k1
///   `0x16957b..0x169587`) sets RND32 for its handlers. Never `PUSH STS`
///   (the kernel MMASK clears IRPTEN).
/// - Scratch (CCES): R0–R2, R4, R8, R12, I4, I12, I13, M4, M12, B4, B12, B13,
///   LCNTR. Preserve every other register, including I5 and USTAT.
/// - Never raise AIS or MIS (NaN operand, ∞ − ∞, 0·∞, FIX of ∞): the kernel
///   then zeroes the output and disables the slot.
/// - PC-relative branches and calls only, and no absolute DM address: section 3
///   is copied without relocation records.
pub trait OwnedBlock {
    /// The deployment ID. Stock-ID replacement is the caller's explicit choice.
    fn model_id(&self) -> u32;
    fn parameters(&self) -> &'static [ParameterSpec];
    /// State size and initial values for this instance; no heap or kernel
    /// pointers. [`StaticLayout::Stereo`] makes the block a stereo block.
    fn static_layout(&self) -> StaticLayout;
    /// Emit each leaf once. The labels must name instructions in `program`.
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError>;
    /// A conservative bound for one 32-sample process leaf, in modeled steps.
    fn process_cycles(&self) -> u64;
}

/// Rust twin of the leaves (roadmap V1). Do the emitted float computes in the
/// same order with `tetrad_sharc::math::prm` (`mul`, `add`, `sub`: kernel modes,
/// flush to zero, no fused multiply-add). Inside an RND32 bracket use
/// `prm::Precision::Extended` on `prm::Float40` registers: a 32-bit load
/// clears the extension and a store keeps the upper 32 bits. Exact
/// operations (moves, sign bits, compares) read operands through
/// `prm::operand`. Under RND32 a `FLOAT` result keeps 32 significand bits,
/// but its next use truncates to 24. State words are raw register bits. V2
/// calls the twins as the adapter calls the leaves: once per state.
pub trait HostReference: OwnedBlock {
    /// Twin of a mono process leaf: one channel state and its samples.
    fn process(&self, state: &mut [u32], samples: &mut [f32; BLOCK_SAMPLES]);
    /// Twin of a stereo process leaf ([`StaticLayout::Stereo`]): the one
    /// state and both channels. The default runs [`Self::process`] for A,
    /// then B, on that state.
    fn process_stereo(&self, state: &mut [u32], samples: &mut [[f32; BLOCK_SAMPLES]; 2]) {
        for channel in samples {
            self.process(state, channel);
        }
    }
    /// `value` is finite and in 0..=1.
    fn set_parameter(&self, state: &mut [u32], index: u32, value: f32);
    /// The kernel reset (leaf R8 ≠ 0).
    fn reset(&self, state: &mut [u32]);
    /// Fault recovery (leaf R8 = 0). The default is [`Self::reset`].
    fn recover(&self, state: &mut [u32]) {
        self.reset(state);
    }
}
