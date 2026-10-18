//! Encodings follow the pinned manual fields and synthetic oracle in the report.
use crate::address::{DmWordAddress32, PcRelative24, PmWordAddress24};
use crate::register::{
    Dag1Index, Dag1Modify, Dag2Index, Dag2Modify, DataReg, FloatReg, UniversalReg,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EncodeError {
    #[error("instruction {0:#x} exceeds 48 bits")]
    WordRange(u64),
    #[error("unsupported backend feature: {0:?}")]
    Unsupported(UnsupportedFeature),
    #[error("unverified MODE1 mask {0:#x}")]
    Mode1Mask(u32),
    #[error("setting MODE1 mask {0:#x} is outside the supported scalar mode policy")]
    UnsupportedMode1Set(u32),
    #[error("unsupported {register} mask {mask:#x}")]
    SystemMask { register: &'static str, mask: u32 },
    #[error("STKYX stack-empty bits are read-only")]
    ReadOnlyStackStatus,
    #[error("{0:?} is read-only")]
    ReadOnlyRegister(UniversalReg),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedFeature {
    ProgramMemory,
    Visa,
    Simd,
    ParallelCompute,
    CrossIndexModify,
    IndirectCall,
    HardwareLoop,
    DelayedBranch,
    SystemRegister,
    StackPush,
    MixedStackPushPop,
    L1InstructionCache,
    L1DataCache,
    L1ProgramCache,
    BranchTargetBuffer,
    Toggle,
    DelayedReturn,
    LoopReturn,
    ResidentBoot,
    Dma,
    ContextAbi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionWord48(u64);

impl InstructionWord48 {
    pub const MAX: u64 = 0xffff_ffff_ffff;

    /// Read/inspect a logical word; the program builder has no raw-word insertion.
    pub const fn from_bits(bits: u64) -> Result<Self, EncodeError> {
        if bits <= Self::MAX {
            Ok(Self(bits))
        } else {
            Err(EncodeError::WordRange(bits))
        }
    }
    pub const fn bits(self) -> u64 {
        self.0
    }
    pub fn to_le_bytes(self) -> [u8; 6] {
        let b = self.0.to_le_bytes();
        [b[0], b[1], b[2], b[3], b[4], b[5]]
    }
    pub fn from_le_bytes(bytes: [u8; 6]) -> Self {
        Self(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], 0, 0,
        ]))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Always,
    Equal,
    NotEqual,
    TestFlag,
    NotTestFlag,
    /// ALU AN: after COMP/COMPU, `x < y` (signed/unsigned).
    LessThan,
    /// AN or AZ.
    LessOrEqual,
    GreaterOrEqual,
    GreaterThan,
}

impl Condition {
    pub(crate) const fn code(self) -> u64 {
        match self {
            Self::Always => 31,
            Self::Equal => 0,
            Self::NotEqual => 16,
            Self::TestFlag => 13,
            Self::NotTestFlag => 29,
            Self::LessThan => 1,
            Self::LessOrEqual => 2,
            Self::GreaterOrEqual => 17,
            Self::GreaterThan => 18,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmAccess {
    PreModify,
    PostModify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchKind {
    Jump,
    Call,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchTarget {
    Absolute(PmWordAddress24),
    Relative(PcRelative24),
}

pub const MODE1_IRPTEN: u32 = 1 << 12;
pub const MODE1_NESTM: u32 = 1 << 11;
pub const MODE1_CBUFEN: u32 = 1 << 24;
pub const MODE1_BDCST1: u32 = 1 << 23;
pub const MODE1_BDCST9: u32 = 1 << 22;
pub const MODE1_PEYEN: u32 = 1 << 21;
pub const MODE1_RND32: u32 = 1 << 16;
pub const MODE1_TRUNCATE: u32 = 1 << 15;
pub const MODE1_ALUSAT: u32 = 1 << 13;
pub const MODE1_BR0: u32 = 1 << 1;
pub const MODE1_BR8: u32 = 1;
pub const MODE1_SCALAR_CLEAR: u32 = MODE1_BDCST1
    | MODE1_BDCST9
    | MODE1_PEYEN
    | MODE1_TRUNCATE
    | MODE1_ALUSAT
    | MODE1_BR0
    | MODE1_BR8;
const MODE1_VERIFIED: u32 =
    MODE1_SCALAR_CLEAR | MODE1_RND32 | MODE1_CBUFEN | MODE1_IRPTEN | MODE1_NESTM;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode1Mask(u32);

impl Mode1Mask {
    pub const fn new(mask: u32) -> Result<Self, EncodeError> {
        if mask & !MODE1_VERIFIED == 0 {
            Ok(Self(mask))
        } else {
            Err(EncodeError::Mode1Mask(mask))
        }
    }
    pub const fn bits(self) -> u32 {
        self.0
    }
}

// Narrow named policies from SHARC-PLUS_regf_{mode1,mode2,mmask,imask,stkyx}.
// SYSREG selectors below are four-bit Type18 fields, not Type17 UREG codes.
pub const MODE2_CADIS: u32 = 1 << 4;
pub const MODE2_SNEN: u32 = 1;
pub const IMASK_SECI: u32 = 1 << 15;
pub const IMASK_RSTI: u32 = 1 << 1;
pub const IMASK_EMUI: u32 = 1;
// PRM IMASK R/W fields: 31:20, 15, 12:11, 8:3. Bits 1:0 are R/NW.
pub const IMASK_WRITABLE: u32 = 0xfff0_99f8;
pub const STKYX_LSEM: u32 = 1 << 26;
pub const STKYX_SSEM: u32 = 1 << 24;
pub const STKYX_PCEM: u32 = 1 << 22;

macro_rules! checked_mask {
    ($name:ident, $register:literal, $allowed:expr) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name(u32);
        impl $name {
            pub const fn new(mask: u32) -> Result<Self, EncodeError> {
                if mask & !($allowed) == 0 {
                    Ok(Self(mask))
                } else {
                    Err(EncodeError::SystemMask {
                        register: $register,
                        mask,
                    })
                }
            }
            pub const fn bits(self) -> u32 {
                self.0
            }
        }
    };
}

checked_mask!(Mode2Mask, "MODE2", MODE2_CADIS | MODE2_SNEN);
// MMASK clears selected MODE1 bits on status push. Do not clear RND32 or
// enable an inherited numerical mode through a generic immediate destination.
checked_mask!(
    Mmask,
    "MMASK",
    MODE1_SCALAR_CLEAR | MODE1_IRPTEN | MODE1_NESTM
);
checked_mask!(InterruptMask, "IMASK", IMASK_WRITABLE);
checked_mask!(
    StackEmptyMask,
    "STKYX",
    STKYX_LSEM | STKYX_SSEM | STKYX_PCEM
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOperation {
    Set,
    Clear,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemBitMask {
    Mode1(Mode1Mask),
    Mode2(Mode2Mask),
    Mmask(Mmask),
    Interrupt(InterruptMask),
    StackEmpty(StackEmptyMask),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackPops {
    Loop,
    Status,
    Pc,
    LoopStatus,
    LoopPc,
    StatusPc,
    All,
}

impl StackPops {
    const fn fields(self) -> u64 {
        match self {
            Self::Loop => 1 << 38,
            Self::Status => 1 << 36,
            Self::Pc => 1 << 34,
            Self::LoopStatus => (1 << 38) | (1 << 36),
            Self::LoopPc => (1 << 38) | (1 << 34),
            Self::StatusPc => (1 << 36) | (1 << 34),
            Self::All => (1 << 38) | (1 << 36) | (1 << 34),
        }
    }
}

fn system_bits(operation: BitOperation, operand: SystemBitMask) -> Result<u64, EncodeError> {
    let (selector, mask) = match operand {
        SystemBitMask::Mode1(mask) => {
            if operation == BitOperation::Set {
                if mask.bits() & MODE1_PEYEN != 0 {
                    return Err(EncodeError::Unsupported(UnsupportedFeature::Simd));
                }
                if mask.bits() & MODE1_SCALAR_CLEAR != 0 {
                    return Err(EncodeError::UnsupportedMode1Set(mask.bits()));
                }
            }
            (2, mask.bits())
        }
        SystemBitMask::Mmask(mask) => (3, mask.bits()),
        SystemBitMask::Mode2(mask) => (4, mask.bits()),
        SystemBitMask::Interrupt(mask) => (0xb, mask.bits()),
        SystemBitMask::StackEmpty(mask) => {
            if operation != BitOperation::Test {
                return Err(EncodeError::ReadOnlyStackStatus);
            }
            (8, mask.bits())
        }
    };
    let bop = match operation {
        BitOperation::Set => 0,
        BitOperation::Clear => 1,
        BitOperation::Test => 4,
    };
    Ok(0x1400_0000_0000 | (bop << 37) | (selector << 32) | u64::from(mask))
}

/// The words of [`C_RETURN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CReturnWord {
    /// Type 3a `I12 = DM(M7, I6)`: the saved return address − 1.
    LoadReturn,
    /// Type 9a `JUMP (M14, I12) (DB)`.
    Jump,
    /// `RFRAME`: `I7 = I6, I6 = DM(0, I6)`, in the first delay slot.
    Rframe,
}

/// The stock C return in ISA code (fortin_zuulgru_gate section 3 words
/// `0xdf..0xe2`): to the caller's CJUMP return literal + 1, in either space.
/// Needs the C run-time M7 = −1, M14 = 1.
pub const C_RETURN: [Instruction48; 4] = [
    Instruction48::CReturn(CReturnWord::LoadReturn),
    Instruction48::CReturn(CReturnWord::Jump),
    Instruction48::CReturn(CReturnWord::Rframe),
    Instruction48::Nop,
];

/// No PM, parallel compute, wide memory, arbitrary system registers or raw escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instruction48 {
    FloatMultiply {
        dst: FloatReg,
        lhs: FloatReg,
        rhs: FloatReg,
    },
    FloatAdd {
        dst: FloatReg,
        lhs: FloatReg,
        rhs: FloatReg,
    },
    FloatSubtract {
        dst: FloatReg,
        lhs: FloatReg,
        rhs: FloatReg,
    },
    FloatAbs {
        dst: FloatReg,
        src: FloatReg,
    },
    FloatMax {
        dst: FloatReg,
        lhs: FloatReg,
        rhs: FloatReg,
    },
    /// `dst = clip src by bound`.
    FloatClip {
        dst: FloatReg,
        src: FloatReg,
        bound: FloatReg,
    },
    /// `dst = magnitude copysign sign`.
    FloatCopySign {
        dst: FloatReg,
        magnitude: FloatReg,
        sign: FloatReg,
    },
    /// `comp(lhs, rhs)`: flags only, RN is encoded as zero.
    FloatCompare {
        lhs: FloatReg,
        rhs: FloatReg,
    },
    /// Fixed-point `dst = lhs - rhs`; wraps because ALUSAT is never set.
    IntegerSubtract {
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    },
    IntegerPass {
        dst: DataReg,
        src: DataReg,
    },
    IntegerDecrement {
        dst: DataReg,
        src: DataReg,
    },
    Immediate {
        dst: UniversalReg,
        value: u32,
    },
    RegisterMove {
        dst: UniversalReg,
        src: UniversalReg,
    },
    DmLoad {
        dst: DataReg,
        index: Dag1Index,
        modifier: Dag1Modify,
        access: DmAccess,
    },
    DmStore {
        src: DataReg,
        index: Dag1Index,
        modifier: Dag1Modify,
        access: DmAccess,
    },
    DmOffsetLoad {
        dst: DataReg,
        index: Dag1Index,
        offset: i32,
    },
    DmOffsetStore {
        src: DataReg,
        index: Dag1Index,
        offset: i32,
    },
    /// Type 19a `Ia = MODIFY(Ia, words)(NW)` (opcode `0x15`, W = 1). The DAG
    /// scales `words` by the pointer's space (PRM `.08.18`): ×4 for a
    /// byte-space pointer, ×1 in word space, so it always moves whole words.
    /// Stock: the C frame reserve `I7 = MODIFY(I7, -n)(NW)` (volume `0x034`).
    /// Self-index, DAG1. Caller owns the B/L contract.
    ModifyWords {
        index: Dag1Index,
        words: i32,
    },
    /// Type 19a `Ia = MODIFY(Ia, bytes)` (opcode `0x16`): never scaled. For a
    /// byte-space pointer `bytes` is a byte offset; in word space it counts
    /// words (both forms agree there). Stock: state byte offsets (volume
    /// `0x1b0`, noisegate `0x057`). Self-index, DAG1. Caller owns B/L.
    ModifyBytes {
        index: Dag1Index,
        bytes: i32,
    },
    DmDirectLoad {
        dst: DataReg,
        address: DmWordAddress32,
    },
    DmDirectStore {
        src: DataReg,
        address: DmWordAddress32,
    },
    MmaskImmediate(Mmask),
    SystemBit {
        operation: BitOperation,
        operand: SystemBitMask,
    },
    PopStacks {
        stacks: StackPops,
        flush_conflict_cache: bool,
    },
    FlushConflictCache,
    Mode1Set(Mode1Mask),
    Mode1Clear(Mode1Mask),
    Branch {
        kind: BranchKind,
        condition: Condition,
        target: BranchTarget,
    },
    /// Type9a non-delayed `JUMP/CALL (Mx, Iy)`: target `Iy + Mx`, DAG2 only.
    IndirectBranch {
        kind: BranchKind,
        condition: Condition,
        index: Dag2Index,
        modifier: Dag2Modify,
    },
    IntegerAdd {
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    },
    IntegerIncrement {
        dst: DataReg,
        src: DataReg,
    },
    /// `COMP(lhs, rhs)`: signed flags only.
    IntegerCompare {
        lhs: DataReg,
        rhs: DataReg,
    },
    /// `COMPU(lhs, rhs)`: unsigned flags only.
    IntegerCompareUnsigned {
        lhs: DataReg,
        rhs: DataReg,
    },
    And {
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    },
    Or {
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    },
    Xor {
        dst: DataReg,
        lhs: DataReg,
        rhs: DataReg,
    },
    /// `Fn = FLOAT Rx`.
    FloatFromInteger {
        dst: FloatReg,
        src: DataReg,
    },
    /// `Rn = FIX Fx`: rounds per MODE1 TRUNCATE (owned modes keep it clear).
    FixToInteger {
        dst: DataReg,
        src: FloatReg,
    },
    /// Type6a `Rn = LSHIFT Rx BY n`: positive left, negative right. The count
    /// is the 12-bit `{DATAEX, data8}` field; CCES sign-fills DATAEX (stock).
    /// Shifter flags are not ALU flags; conditions do not see the result.
    LogicalShift {
        dst: DataReg,
        src: DataReg,
        amount: i8,
    },
    /// Type14d `DM(addr32) = Rn (bw)`: store the low byte at a byte address.
    /// The PRM Group III syntax tables (`.16.01`, `.16.03`) list 14d with
    /// Addr = ISA; the "VISA" page title is template text on every "d" page.
    /// One hardware probe (ILOPI unmasked) must confirm it before device use;
    /// the fallback is Type 4d `DM(Ia, 0) = Rn (bw)` (ISA VISA, `.14.33`). The
    /// resident needs a byte store for the shared ring index bytes, where a
    /// 32-bit read-modify-write would race with the ARM bytes.
    DmDirectByteStore {
        src: DataReg,
        address: u32,
    },
    /// Type2a shifter `Rn = LSHIFT Rx BY Ry` (signed count in Ry's low byte).
    LogicalShiftBy {
        dst: DataReg,
        src: DataReg,
        amount: DataReg,
    },
    /// Type20a `PUSH STS`: MODE1/ASTATx/ASTATy onto the status stack, then
    /// MODE1 &= !MMASK.
    PushStatus,
    /// One word of the stock C return; emit all of [`C_RETURN`] in order.
    CReturn(CReturnWord),
    Idle,
    Rts,
    Rti,
    Nop,
    Unsupported(UnsupportedFeature),
}

impl Instruction48 {
    pub fn encode(self) -> Result<InstructionWord48, EncodeError> {
        let always = 31 << 33;
        let bits = match self {
            Self::FloatMultiply { dst, lhs, rhs } => {
                compute(1, 0x30, dst.index(), lhs.index(), rhs.index())
            }
            Self::FloatAdd { dst, lhs, rhs } => {
                compute(0, 0x81, dst.index(), lhs.index(), rhs.index())
            }
            Self::FloatSubtract { dst, lhs, rhs } => {
                compute(0, 0x82, dst.index(), lhs.index(), rhs.index())
            }
            Self::FloatAbs { dst, src } => compute(0, 0xb0, dst.index(), src.index(), 0),
            Self::FloatMax { dst, lhs, rhs } => {
                compute(0, 0xe2, dst.index(), lhs.index(), rhs.index())
            }
            Self::FloatClip { dst, src, bound } => {
                compute(0, 0xe3, dst.index(), src.index(), bound.index())
            }
            Self::FloatCopySign {
                dst,
                magnitude,
                sign,
            } => compute(0, 0xe0, dst.index(), magnitude.index(), sign.index()),
            Self::FloatCompare { lhs, rhs } => compute(0, 0x8a, 0, lhs.index(), rhs.index()),
            Self::IntegerSubtract { dst, lhs, rhs } => {
                compute(0, 0x02, dst.index(), lhs.index(), rhs.index())
            }
            Self::IntegerPass { dst, src } => compute(0, 0x21, dst.index(), src.index(), 0),
            Self::IntegerDecrement { dst, src } => compute(0, 0x2a, dst.index(), src.index(), 0),
            Self::Immediate { dst, value } => {
                writable(dst)?;
                0x0f00_0000_0000 | (u64::from(dst.code()) << 32) | u64::from(value)
            }
            Self::RegisterMove { dst, src } => {
                writable(dst)?;
                let source = u64::from(src.code());
                0x7000_0000_0000
                    | ((source >> 2) << 38)
                    | ((source & 2) << 31)
                    | ((source & 1) << 31)
                    | always
                    | (u64::from(dst.code()) << 23)
            }
            Self::DmLoad {
                dst,
                index,
                modifier,
                access,
            } => memory(dst, index, modifier, access, false),
            Self::DmStore {
                src,
                index,
                modifier,
                access,
            } => memory(src, index, modifier, access, true),
            Self::DmOffsetLoad { dst, index, offset } => offset_memory(dst, index, offset, false),
            Self::DmOffsetStore { src, index, offset } => offset_memory(src, index, offset, true),
            // Type 19a: SC 41:40 (01 scaled, 10 plain), W 39, G 38, IDIS 37:35 = 0.
            Self::ModifyWords { index, words } => {
                0x1580_0000_0000 | (u64::from(index.index()) << 32) | u64::from(words as u32)
            }
            Self::ModifyBytes { index, bytes } => {
                0x1600_0000_0000 | (u64::from(index.index()) << 32) | u64::from(bytes as u32)
            }
            Self::DmDirectLoad { dst, address } => {
                0x1000_0000_0000 | (u64::from(dst.index()) << 32) | u64::from(address.get())
            }
            Self::DmDirectStore { src, address } => {
                0x1100_0000_0000 | (u64::from(src.index()) << 32) | u64::from(address.get())
            }
            Self::MmaskImmediate(mask) => 0x0f73_0000_0000 | u64::from(mask.bits()),
            Self::SystemBit { operation, operand } => system_bits(operation, operand)?,
            Self::PopStacks {
                stacks,
                flush_conflict_cache,
            } => 0x1700_0000_0000 | stacks.fields() | (u64::from(flush_conflict_cache) << 33),
            Self::FlushConflictCache => 0x1702_0000_0000,
            Self::Mode1Set(mask) => system_bits(BitOperation::Set, SystemBitMask::Mode1(mask))?,
            Self::Mode1Clear(mask) => system_bits(BitOperation::Clear, SystemBitMask::Mode1(mask))?,
            Self::Branch {
                kind,
                condition,
                target,
            } => {
                let (relative, address) = match target {
                    BranchTarget::Absolute(a) => (false, a.get()),
                    BranchTarget::Relative(a) => (true, a.bits()),
                };
                0x0600_0000_0000
                    | (u64::from(relative) << 40)
                    | (u64::from(kind == BranchKind::Call) << 39)
                    | (condition.code() << 33)
                    | u64::from(address)
            }
            Self::IndirectBranch {
                kind,
                condition,
                index,
                modifier,
            } => {
                0x0800_0000_0000
                    | (u64::from(kind == BranchKind::Call) << 39)
                    | (condition.code() << 33)
                    | (u64::from(index.index() - 8) << 30)
                    | (u64::from(modifier.index() - 8) << 27)
            }
            Self::IntegerAdd { dst, lhs, rhs } => {
                compute(0, 0x01, dst.index(), lhs.index(), rhs.index())
            }
            Self::IntegerIncrement { dst, src } => compute(0, 0x29, dst.index(), src.index(), 0),
            Self::IntegerCompare { lhs, rhs } => compute(0, 0x0a, 0, lhs.index(), rhs.index()),
            Self::IntegerCompareUnsigned { lhs, rhs } => {
                compute(0, 0x0b, 0, lhs.index(), rhs.index())
            }
            Self::And { dst, lhs, rhs } => compute(0, 0x40, dst.index(), lhs.index(), rhs.index()),
            Self::Or { dst, lhs, rhs } => compute(0, 0x41, dst.index(), lhs.index(), rhs.index()),
            Self::Xor { dst, lhs, rhs } => compute(0, 0x42, dst.index(), lhs.index(), rhs.index()),
            Self::FloatFromInteger { dst, src } => compute(0, 0xca, dst.index(), src.index(), 0),
            Self::FixToInteger { dst, src } => compute(0, 0xc9, dst.index(), src.index(), 0),
            Self::LogicalShift { dst, src, amount } => {
                let count = u64::from(i16::from(amount) as u16 & 0xfff);
                0x023e_0000_0000
                    | ((count >> 8) << 27)
                    | ((count & 0xff) << 8)
                    | (u64::from(dst.index()) << 4)
                    | u64::from(src.index())
            }
            Self::LogicalShiftBy { dst, src, amount } => {
                compute(2, 0x00, dst.index(), src.index(), amount.index())
            }
            Self::DmDirectByteStore { src, address } => {
                0x1b00_0000_0000 | (u64::from(src.index()) << 32) | u64::from(address)
            }
            Self::PushStatus => 0x1720_0000_0000,
            // Type 3a U=0, I6, M7, COND=31, D=0, UREG=I12 (0x1c).
            Self::CReturn(CReturnWord::LoadReturn) => 0x4dfe_0e00_0000,
            // Type 9a JUMP, COND=31, PMI=I12-8, PMM=M14-8, J=1 (DB).
            Self::CReturn(CReturnWord::Jump) => 0x083f_3400_0000,
            Self::CReturn(CReturnWord::Rframe) => 0x1900_0000_0000,
            Self::Idle => 0x0080_0000_0000,
            Self::Rts => 0x0a3e_0000_0000,
            Self::Rti => 0x0b3e_0000_0000,
            Self::Nop => 0,
            Self::Unsupported(feature) => return Err(EncodeError::Unsupported(feature)),
        };
        InstructionWord48::from_bits(bits)
    }
}

fn writable(register: UniversalReg) -> Result<(), EncodeError> {
    match register {
        UniversalReg::System(system) if !system.writable() => {
            Err(EncodeError::ReadOnlyRegister(register))
        }
        _ => Ok(()),
    }
}

fn compute(unit: u64, opcode: u64, dst: u8, lhs: u8, rhs: u8) -> u64 {
    0x013e_0000_0000
        | (unit << 20)
        | (opcode << 12)
        | (u64::from(dst) << 8)
        | (u64::from(lhs) << 4)
        | u64::from(rhs)
}

fn memory(
    data: DataReg,
    index: Dag1Index,
    modifier: Dag1Modify,
    access: DmAccess,
    store: bool,
) -> u64 {
    0x403e_0000_0000
        | (u64::from(access == DmAccess::PostModify) << 44)
        | (u64::from(index.index()) << 41)
        | (u64::from(modifier.index()) << 38)
        | (u64::from(store) << 31)
        | (u64::from(data.index()) << 23)
}

fn offset_memory(data: DataReg, index: Dag1Index, offset: i32, store: bool) -> u64 {
    0xa000_0000_0000
        | (u64::from(index.index()) << 41)
        | (u64::from(store) << 40)
        | (u64::from(data.index()) << 32)
        | u64::from(offset as u32)
}
