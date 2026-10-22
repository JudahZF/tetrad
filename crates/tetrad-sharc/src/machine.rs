//! Resident execution model: full DAG register file, PC/loop/status stacks,
//! hardware loops, interrupts and an injected data bus.
//!
//! The leaf [`crate::simulator`] keeps its narrow kernel contract. This model
//! adds what a resident runtime needs and decodes raw words independently of
//! the encoder. One step is one instruction; [`Machine::cycles`] (the EMUCLK
//! view) counts steps, so cycle numbers are an inferred 1-cycle-per-word model,
//! not SHARC+ pipeline timing. Effect latency, hazards, the instruction cache
//! and the BTB are not modeled. Execution errors leave earlier effects in place.
//!
//! The fetch address selects the decoder (PRM `.06.002`, `.06.004`): ISA48
//! words in normal-word space, VISA parcels in short-word space
//! ([`VISA_SPACE`]). Branches, returns and interrupts switch between them.
//!
//! Float computes use host `f32` by default. [`Arithmetic::Prm`] opts in to the
//! PRM rules of [`crate::simulator::run_scalar_prm`] on 40-bit data registers.
//!
//! MODE1 PEYEN selects SIMD (PRM `.08.35`): computes also run on PEy with the
//! complementary registers ([`Machine::s`]), and ureg transfers also move the
//! complementary register at the next normal word. BDCST1/BDCST9 broadcast
//! I1/I9 loads to both PEs (`.08.33`). With MODE1 CBUFEN, post-modify wraps
//! in the ring `[B, B + L)` (`.08.32`).
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ops::Range;

use crate::alu::{ComputeOp, compare_prm, evaluate, evaluate_prm};
use crate::emit::OwnedCode;
use crate::isa48::{
    MODE1_ALUSAT, MODE1_BDCST1, MODE1_BDCST9, MODE1_BR0, MODE1_BR8, MODE1_CBUFEN, MODE1_IRPTEN,
    MODE1_NESTM, MODE1_PEYEN, MODE1_RND32, MODE1_TRUNCATE,
};

/// MODE1 bits the model refuses: saturation, truncation and bit reversal.
const MODE1_UNMODELED: u32 = MODE1_TRUNCATE | MODE1_ALUSAT | MODE1_BR0 | MODE1_BR8;

pub const PC_STACK_DEPTH: usize = 30;
pub const LOOP_STACK_DEPTH: usize = 6;
pub const STATUS_STACK_DEPTH: usize = 15;
/// Stock core-1 images place 32 four-word vectors at PM `0x90000`. Interrupts
/// use it only when `SYSCTL.IIVT` is set.
pub const IVT_BASE: u32 = 0x9_0000;
/// Reset IVT (IIVT = 0): L2CTL ROM1 boot memory (PRM `SHARC-PLUS_shdbg_sysctl`,
/// `c_mem…09.32`). The model holds no ROM code, so delivery there stops with
/// [`MachineError::NoCode`].
pub const ROM_IVT_BASE: u32 = 0x50_0000;
pub const VECTOR_WORDS: u32 = 4;
/// Core `SYSCTL` (PRM `DBG_SYSCTL`) at DM `0x30024`. The PRM gives the fields,
/// not the address; stock core-1 init sets bit 2 at this address (**I**).
pub const SYSCTL: u32 = 0x3_0024;
/// `SYSCTL.IIVT`: vectors come from internal L1 at [`IVT_BASE`].
pub const SYSCTL_IIVT: u32 = 1 << 2;
/// First byte-addressed DM address: the L1 byte alias of normal word
/// `0x9_0000`. L1 byte aliases, L2 and system MMIO lie above it; word aliases
/// and core MMRs below. The PRM defers the map to the data sheet (`.09.05`);
/// this bound is from the SC58x map and the stock aliases (**I**).
pub const BYTE_SPACE: u32 = 0x24_0000;
/// Short-word code space. The PRM defers the map to the data sheet; these
/// ranges are stock (**D**): the L1 code-block span (ZenUI VA 0x5d8e7c, as
/// tetrad-dsp-host `CODE_BLOCKS`; PRM `.06.062` shows VISA code at `0x130000`)
/// and the L2 VISA alias loadsharc writes (loadsharc VA 0x16c4).
pub const VISA_SPACE: [Range<u32>; 2] = [0x12_0000..0x1d_0000, 0xba_0000..0xbc_0000];
/// STKYX ALU floating-point invalid sticky (PRM `SHARC-PLUS_regf_stkyx`).
pub const STKYX_AIS: u32 = 1 << 5;
/// STKYX multiplier floating-point invalid sticky.
pub const STKYX_MIS: u32 = 1 << 9;

/// Float semantics of computes, on both PEs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Arithmetic {
    /// Host `f32` on the 32-bit register view; the extension stays zero and
    /// no sticky flag is set (the resident and its pinned results use this).
    #[default]
    HostF32,
    /// PRM rules on 40-bit registers (`alu::evaluate_prm`): MODE1 RND32 selects
    /// 24-bit rounding (else the 32-bit extended format), denormal operands and
    /// post-rounded results become signed zero, NaN or invalid gives all ones,
    /// overflow gives ±infinity. Computes set [`STKYX_AIS`]/[`STKYX_MIS`] (in
    /// STKYY on PEy) on a NaN operand or an invalid operation (PRM 23.x, 26.2).
    Prm,
}

/// How the sequencer decodes a fetch address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionSpace {
    /// Normal-word address: one 48-bit ISA instruction per address.
    Isa,
    /// Short-word address: 16-, 32- or 48-bit instructions in 16-bit parcels.
    Visa,
}

impl InstructionSpace {
    pub fn of(pc: u32) -> Self {
        if VISA_SPACE.iter().any(|range| range.contains(&pc)) {
            Self::Visa
        } else {
            Self::Isa
        }
    }
    /// Whether every fetch address in `[base, end)` decodes in this space.
    fn holds(self, base: u32, end: u64) -> bool {
        let base = u64::from(base);
        let bounds = |r: &Range<u32>| (u64::from(r.start), u64::from(r.end));
        match self {
            Self::Isa => !VISA_SPACE
                .iter()
                .map(bounds)
                .any(|(s, e)| base < e && s < end),
            Self::Visa => VISA_SPACE
                .iter()
                .map(bounds)
                .any(|(s, e)| s <= base && end <= e),
        }
    }
}

/// Data bus seen by the core. Addresses are the raw 32-bit DM address of the
/// access (normal-word aliases for L1 words, byte addresses for system space).
pub trait Bus {
    fn read(&mut self, address: u32) -> Result<u32, BusError>;
    fn write(&mut self, address: u32, value: u32) -> Result<(), BusError>;
    /// One byte at a byte address. Lanes are little-endian within a word.
    fn write_byte(&mut self, address: u32, value: u8) -> Result<(), BusError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BusError {
    #[error("no memory or device at {0:#x}")]
    Unmapped(u32),
    #[error("word access at byte address {0:#x} is not 4-byte aligned")]
    Misaligned(u32),
    #[error("byte access at {0:#x} targets a word-addressed region")]
    ByteInWordRegion(u32),
    #[error("device at {address:#x} refused the access: {reason}")]
    Device { address: u32, reason: &'static str },
}

/// How a region's addresses count 32-bit words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressUnit {
    /// One address per word (L1 normal-word alias).
    Word,
    /// Four addresses per word (L2, L1 byte alias, system space).
    Byte,
}

impl AddressUnit {
    /// The space a DM address belongs to; DAG scaling follows the pointer.
    pub const fn of(address: u32) -> Self {
        if address >= BYTE_SPACE {
            Self::Byte
        } else {
            Self::Word
        }
    }
    /// Addresses per normal word. PRM `.08.18`: normal-word loads and stores
    /// through a byte-space pointer scale the offset or modifier by this.
    pub const fn scale(self) -> u32 {
        match self {
            Self::Word => 1,
            Self::Byte => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Region {
    base: u32,
    unit: AddressUnit,
    words: Vec<u32>,
}

impl Region {
    fn end(&self) -> u64 {
        u64::from(self.base) + self.words.len() as u64 * u64::from(self.unit.scale())
    }
    fn contains(&self, address: u32) -> bool {
        address >= self.base && u64::from(address) < self.end()
    }
    fn word_index(&self, address: u32) -> Result<usize, BusError> {
        let offset = address - self.base;
        match self.unit {
            AddressUnit::Word => Ok(offset as usize),
            AddressUnit::Byte if offset.is_multiple_of(4) => Ok((offset / 4) as usize),
            AddressUnit::Byte => Err(BusError::Misaligned(address)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegionError {
    #[error("region at {0:#x} overlaps an existing region or the 32-bit space")]
    Overlap(u32),
    /// The unit must match [`AddressUnit::of`] over the whole region, so the
    /// bus and the DAG scaling agree.
    #[error("region at {0:#x} crosses or contradicts the byte-space boundary")]
    Space(u32),
}

/// Plain RAM regions. Devices wrap this map in their own [`Bus`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryMap {
    regions: Vec<Region>,
}

impl MemoryMap {
    pub fn add(&mut self, base: u32, unit: AddressUnit, words: usize) -> Result<(), RegionError> {
        let region = Region {
            base,
            unit,
            words: vec![0; words],
        };
        let overlaps = self
            .regions
            .iter()
            .any(|r| u64::from(base) < r.end() && u64::from(r.base) < region.end());
        if region.end() > 1 << 32 || overlaps {
            return Err(RegionError::Overlap(base));
        }
        let space = match unit {
            AddressUnit::Word => region.end() <= u64::from(BYTE_SPACE),
            AddressUnit::Byte => base >= BYTE_SPACE,
        };
        if !space {
            return Err(RegionError::Space(base));
        }
        self.regions.push(region);
        Ok(())
    }
    fn region(&self, address: u32) -> Result<&Region, BusError> {
        self.regions
            .iter()
            .find(|r| r.contains(address))
            .ok_or(BusError::Unmapped(address))
    }
    fn region_mut(&mut self, address: u32) -> Result<&mut Region, BusError> {
        self.regions
            .iter_mut()
            .find(|r| r.contains(address))
            .ok_or(BusError::Unmapped(address))
    }
    pub fn contains(&self, address: u32) -> bool {
        self.region(address).is_ok()
    }
    /// Copy `bytes` (LE words) into a byte-addressed region; for loaders and hosts.
    pub fn load_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), BusError> {
        for (offset, byte) in bytes.iter().enumerate() {
            self.write_byte(address.wrapping_add(offset as u32), *byte)?;
        }
        Ok(())
    }
    pub fn read_bytes(&self, address: u32, count: usize) -> Result<Vec<u8>, BusError> {
        (0..count)
            .map(|offset| {
                let at = address.wrapping_add(offset as u32);
                let word = self.peek(at & !3)?;
                Ok(word.to_le_bytes()[(at & 3) as usize])
            })
            .collect()
    }
    pub fn peek(&self, address: u32) -> Result<u32, BusError> {
        let region = self.region(address)?;
        Ok(region.words[region.word_index(address)?])
    }
}

impl Bus for MemoryMap {
    fn read(&mut self, address: u32) -> Result<u32, BusError> {
        self.peek(address)
    }
    fn write(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        let region = self.region_mut(address)?;
        let index = region.word_index(address)?;
        region.words[index] = value;
        Ok(())
    }
    fn write_byte(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        let region = self.region_mut(address)?;
        if region.unit == AddressUnit::Word {
            return Err(BusError::ByteInWordRegion(address));
        }
        let index = region.word_index(address & !3)?;
        let mut bytes = region.words[index].to_le_bytes();
        bytes[(address & 3) as usize] = value;
        region.words[index] = u32::from_le_bytes(bytes);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProgramError {
    #[error("code at {0:#x} overlaps a loaded section")]
    Overlap(u32),
    /// Code linked into the other space would decode as the wrong set.
    #[error("code at {base:#x} is not wholly in {space:?} space")]
    Space { base: u32, space: InstructionSpace },
}

/// Linked code sections: ISA48 words by normal-word address, VISA parcels by
/// short-word address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramMemory {
    sections: BTreeMap<u32, Vec<u64>>,
    visa: BTreeMap<u32, Vec<u16>>,
}

impl ProgramMemory {
    pub fn load(&mut self, code: &OwnedCode) -> Result<(), ProgramError> {
        let words: Vec<u64> = code.words().iter().map(|w| w.bits()).collect();
        self.load_words(code.origin().get(), &words)
    }
    /// Load raw 48-bit ISA words at a normal-word address.
    pub fn load_words(&mut self, origin: u32, words: &[u64]) -> Result<(), ProgramError> {
        insert(
            &mut self.sections,
            origin,
            words.to_vec(),
            InstructionSpace::Isa,
        )
    }
    /// Load section-2 style parcels (execution order) at a short-word address.
    pub fn load_visa(&mut self, origin: u32, parcels: &[u16]) -> Result<(), ProgramError> {
        insert(
            &mut self.visa,
            origin,
            parcels.to_vec(),
            InstructionSpace::Visa,
        )
    }
    pub fn fetch(&self, pc: u32) -> Option<u64> {
        fetch(&self.sections, pc)
    }
    fn fetch_parcel(&self, address: u32) -> Option<u16> {
        fetch(&self.visa, address)
    }
}

fn insert<T>(
    sections: &mut BTreeMap<u32, Vec<T>>,
    base: u32,
    units: Vec<T>,
    space: InstructionSpace,
) -> Result<(), ProgramError> {
    let end = u64::from(base) + units.len() as u64;
    if !space.holds(base, end) {
        return Err(ProgramError::Space { base, space });
    }
    if sections.iter().any(|(&start, loaded)| {
        u64::from(base) < u64::from(start) + loaded.len() as u64 && u64::from(start) < end
    }) {
        return Err(ProgramError::Overlap(base));
    }
    sections.insert(base, units);
    Ok(())
}

fn fetch<T: Copy>(sections: &BTreeMap<u32, Vec<T>>, address: u32) -> Option<T> {
    let (&start, units) = sections.range(..=address).next_back()?;
    units.get((address - start) as usize).copied()
}

/// Core interrupt latch bits (IRPTL/IMASK/IMASKP bit index = vector index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct InterruptBit(u8);

impl InterruptBit {
    pub const RESET: Self = Self(1);
    /// System event controller interrupt (`IMASK_SECI`, bit 15).
    pub const SECI: Self = Self(15);

    pub const fn new(bit: u8) -> Option<Self> {
        if bit < 32 { Some(Self(bit)) } else { None }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
    pub const fn vector(self) -> u32 {
        IVT_BASE + VECTOR_WORDS * self.0 as u32
    }
}

/// Arithmetic status. Only the fields below are modeled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    pub zero: bool,
    pub negative: bool,
    pub carry: bool,
    pub overflow: bool,
    /// BIT TST result (TF): all selected bits set.
    pub test: bool,
}

/// Counter loops only (`UNTIL LCE`); condition-terminated DO is not decoded.
/// `start` is the first body address, `end` the address of the last
/// instruction (its first parcel in VISA space).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoopFrame {
    start: u32,
    end: u32,
    count: u32,
}

/// PRM `.06.016`: ASTATx and ASTATy are pushed in SISD and SIMD alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatusFrame {
    flags: Flags,
    flags_y: Flags,
    mode1: u32,
}

/// A taken `(DB)` branch: `slots` instructions still run before it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DelayedBranch {
    target: u32,
    slots: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MachineError {
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error("no code at PC {0:#x}")]
    NoCode(u32),
    #[error("unsupported word {word:#014x} at PC {pc:#x}")]
    UnsupportedWord { pc: u32, word: u64 },
    #[error("PC stack overflow at PC {0:#x}")]
    PcStackOverflow(u32),
    #[error("PC stack underflow at PC {0:#x}")]
    PcStackUnderflow(u32),
    #[error("loop stack overflow at PC {0:#x}")]
    LoopStackOverflow(u32),
    #[error("loop stack underflow at PC {0:#x}")]
    LoopStackUnderflow(u32),
    #[error("status stack overflow at PC {0:#x}")]
    StatusStackOverflow(u32),
    #[error("status stack underflow at PC {0:#x}")]
    StatusStackUnderflow(u32),
    /// PRM: LCNTR = 0 runs 2^32 iterations. The model refuses it instead.
    #[error("loop counter is zero at DO, PC {0:#x}")]
    ZeroLoopCount(u32),
    #[error("branch at PC {0:#x} is in the last three instructions of a loop")]
    BranchInLoopTail(u32),
    /// PRM `.06.062`: the last eleven instructions of a loop body (all of a
    /// shorter body) must be 48-bit. Raised when VISA code fetches a 16- or
    /// 32-bit instruction there.
    #[error("short VISA instruction at PC {0:#x} is in the last eleven instructions of a loop")]
    VisaLoopTail(u32),
    /// PRM `.06.026`: no branch, return or DO in the two delay slots.
    #[error("branch at PC {0:#x} is in the delay slot of a delayed branch")]
    BranchInDelaySlot(u32),
    #[error("nested loop at PC {0:#x} shares its end address with the enclosing loop")]
    SharedLoopEnd(u32),
    #[error("RTI at PC {0:#x} without an active interrupt")]
    RtiWithoutInterrupt(u32),
    /// PRM `.08.32`: the (scaled) length is below 2^31 and the modifier is
    /// smaller than the length, so one wrap lands inside the ring.
    #[error("circular buffer I{index} at PC {pc:#x}: L or |M| out of range")]
    CircularBuffer { pc: u32, index: usize },
    #[error("unmodeled MODE1 {0:#x}: saturation, truncation and bit reversal are refused")]
    Modes(u32),
    #[error("index register arithmetic overflow at PC {0:#x}")]
    IndexOverflow(u32),
    #[error("step budget {0} exhausted")]
    StepBudget(u64),
}

/// Why a run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// IDLE executed and no enabled interrupt is pending.
    Idle,
    /// The caller's PC breakpoint was reached (before executing it).
    Breakpoint(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub r: [u32; 16],
    pub i: [u32; 16],
    pub m: [i32; 16],
    pub b: [u32; 16],
    pub l: [u32; 16],
    pub ustat: [u32; 4],
    pub mode1: u32,
    pub mode2: u32,
    pub mmask: u32,
    pub imask: u32,
    pub irptl: u32,
    pub imaskp: u32,
    pub lcntr: u32,
    /// Core `SYSCTL` at DM [`SYSCTL`]; only IIVT changes behaviour.
    pub sysctl: u32,
    pub flags: Flags,
    pub arithmetic: Arithmetic,
    /// Bits 7:0 of the 40-bit data registers; `r[n]` holds bits 39:8 (PRM 4.06).
    /// Only [`Arithmetic::Prm`] computes and data register moves set them.
    pub extension: [u8; 16],
    /// STKYX: only [`STKYX_AIS`] and [`STKYX_MIS`] are modeled. Sticky until
    /// the caller clears them, as the stock kernel does around each process.
    pub stkyx: u32,
    /// PEy data registers S0-S15 (ureg `0x50`-`0x5f`), the complements of
    /// `r` (PRM `.04.08`), with their own extension bytes.
    pub s: [u32; 16],
    pub s_extension: [u8; 16],
    /// ASTATy: SIMD computes set it; SIMD branches AND their condition over
    /// both PEs (PRM `.06.084`).
    pub flags_y: Flags,
    /// STKYY: the PEy [`STKYX_AIS`]/[`STKYX_MIS`] stickies.
    pub stkyy: u32,
    pc: u32,
    pc_stack: Vec<u32>,
    loop_stack: Vec<LoopFrame>,
    status_stack: Vec<StatusFrame>,
    /// Interrupt frames: (bit, status pushed).
    active: Vec<(u8, bool)>,
    delayed: Option<DelayedBranch>,
    cycles: u64,
    idle: bool,
}

impl Machine {
    /// Reset state: PC at the internal reset vector, interrupts disabled, RND32
    /// set, IIVT clear (callers that model a boot set the entry with `set_pc`).
    pub fn reset() -> Self {
        Self {
            r: [0; 16],
            i: [0; 16],
            m: [0; 16],
            b: [0; 16],
            l: [0; 16],
            ustat: [0; 4],
            mode1: MODE1_RND32,
            mode2: 0,
            mmask: 0,
            imask: 0,
            irptl: 0,
            imaskp: 0,
            lcntr: 0,
            sysctl: 0,
            flags: Flags::default(),
            arithmetic: Arithmetic::HostF32,
            extension: [0; 16],
            stkyx: 0,
            s: [0; 16],
            s_extension: [0; 16],
            flags_y: Flags::default(),
            stkyy: 0,
            pc: InterruptBit::RESET.vector(),
            pc_stack: Vec::new(),
            loop_stack: Vec::new(),
            status_stack: Vec::new(),
            active: Vec::new(),
            delayed: None,
            cycles: 0,
            idle: false,
        }
    }
    pub fn pc(&self) -> u32 {
        self.pc
    }
    /// Also drops a pending delayed branch.
    pub fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
        self.delayed = None;
        self.idle = false;
    }
    pub fn cycles(&self) -> u64 {
        self.cycles
    }
    pub fn is_idle(&self) -> bool {
        self.idle
    }
    pub fn pc_stack_depth(&self) -> usize {
        self.pc_stack.len()
    }
    pub fn loop_stack_depth(&self) -> usize {
        self.loop_stack.len()
    }
    pub fn status_stack_depth(&self) -> usize {
        self.status_stack.len()
    }
    /// Latch an interrupt (peripheral or software). Delivery follows IMASK/IRPTEN.
    pub fn raise(&mut self, interrupt: InterruptBit) {
        self.irptl |= 1 << interrupt.0;
    }

    /// Run until IDLE with nothing deliverable, a breakpoint or the budget.
    pub fn run(
        &mut self,
        program: &ProgramMemory,
        bus: &mut impl Bus,
        breakpoint: Option<u32>,
        budget: u64,
    ) -> Result<Stop, MachineError> {
        for _ in 0..budget {
            if breakpoint == Some(self.pc) && !self.idle {
                return Ok(Stop::Breakpoint(self.pc));
            }
            if self.deliver_interrupt()? {
                continue;
            }
            if self.idle {
                return Ok(Stop::Idle);
            }
            self.step(program, bus)?;
        }
        Err(MachineError::StepBudget(budget))
    }

    /// A delayed branch and its two slots are atomic (PRM `.06.026`).
    fn pending(&self) -> Option<u8> {
        if self.mode1 & MODE1_IRPTEN == 0 || self.delayed.is_some() {
            return None;
        }
        let candidates = self.irptl & self.imask;
        if candidates == 0 {
            return None;
        }
        let bit = candidates.trailing_zeros() as u8;
        let allowed = if self.mode1 & MODE1_NESTM != 0 {
            self.imaskp == 0 || bit < self.imaskp.trailing_zeros() as u8
        } else {
            self.imaskp == 0
        };
        allowed.then_some(bit)
    }

    /// Interrupt entry: return PC and status are pushed, MMASK clears MODE1 bits,
    /// the latch moves to IMASKP. Status push for every vector is inferred.
    fn deliver_interrupt(&mut self) -> Result<bool, MachineError> {
        let Some(bit) = self.pending() else {
            return Ok(false);
        };
        self.push_pc(self.pc)?;
        if self.status_stack.len() == STATUS_STACK_DEPTH {
            return Err(MachineError::StatusStackOverflow(self.pc));
        }
        self.status_stack.push(self.status());
        self.mode1 &= !self.mmask;
        self.irptl &= !(1 << bit);
        self.imaskp |= 1 << bit;
        self.active.push((bit, true));
        let base = if self.sysctl & SYSCTL_IIVT != 0 {
            IVT_BASE
        } else {
            ROM_IVT_BASE
        };
        self.pc = base + VECTOR_WORDS * u32::from(bit);
        self.idle = false;
        self.cycles += 1;
        Ok(true)
    }

    fn push_pc(&mut self, value: u32) -> Result<(), MachineError> {
        if self.pc_stack.len() == PC_STACK_DEPTH {
            return Err(MachineError::PcStackOverflow(self.pc));
        }
        self.pc_stack.push(value);
        Ok(())
    }
    fn pop_pc(&mut self) -> Result<u32, MachineError> {
        self.pc_stack
            .pop()
            .ok_or(MachineError::PcStackUnderflow(self.pc))
    }

    /// Execute one instruction (no interrupt delivery).
    pub fn step(
        &mut self,
        program: &ProgramMemory,
        bus: &mut impl Bus,
    ) -> Result<(), MachineError> {
        if self.mode1 & MODE1_UNMODELED != 0 {
            return Err(MachineError::Modes(self.mode1));
        }
        let pc = self.pc;
        let space = InstructionSpace::of(pc);
        let (op, length) = match space {
            InstructionSpace::Isa => {
                let word = program.fetch(pc).ok_or(MachineError::NoCode(pc))?;
                (
                    decode(word).ok_or(MachineError::UnsupportedWord { pc, word })?,
                    1,
                )
            }
            InstructionSpace::Visa => decode_visa(program, pc)?,
        };
        // The 48-bit loop tail (PRM `.06.062`) starts in the 31 parcels that
        // end at the loop end; a shorter instruction there breaks it.
        if space == InstructionSpace::Visa
            && length < 3
            && self
                .loop_stack
                .iter()
                .any(|frame| frame.start <= pc && pc <= frame.end && frame.end - pc <= 30)
        {
            return Err(MachineError::VisaLoopTail(pc));
        }
        let branch = matches!(op, Op::Branch { .. } | Op::Return { .. } | Op::CJump { .. });
        // PRM: no JUMP/CALL/RTS in the last three instructions of a loop
        // (48-bit, so three parcels each in VISA space).
        let tail = if space == InstructionSpace::Visa {
            6
        } else {
            2
        };
        if branch
            && self
                .loop_stack
                .last()
                .is_some_and(|frame| pc <= frame.end && pc + tail >= frame.end)
        {
            return Err(MachineError::BranchInLoopTail(pc));
        }
        if self.delayed.is_some() && (branch || matches!(op, Op::Do { .. })) {
            return Err(MachineError::BranchInDelaySlot(pc));
        }
        let pending = self.delayed.take();
        self.cycles += 1;
        let mut next = pc.wrapping_add(length);
        match op {
            Op::Nop => {}
            Op::Idle => {
                self.idle = true;
            }
            Op::Compute(compute) => {
                let effects = self.compute(Some(compute));
                self.commit(effects);
            }
            Op::Immediate { ureg, value } => {
                self.write_ureg(ureg, value);
                if let Some(complement) = complement(ureg).filter(|_| self.simd()) {
                    self.write_ureg(complement, value);
                }
            }
            Op::Move { dst, src } => self.copy(dst, src),
            Op::Transfer { compute, accesses } => self.transfer(bus, compute, accesses)?,
            Op::ImmediateStore {
                index,
                modifier,
                value,
            } => {
                let original = self.i[index];
                self.i[index] = self.post_modify(index, self.m[modifier])?;
                self.store(bus, original, value)?;
                // PRM 16.12: PEy writes the same data at the next word.
                if self.simd() {
                    let next = scaled(original, 1).ok_or(MachineError::IndexOverflow(pc))?;
                    self.store(bus, next, value)?;
                }
            }
            // Pre-modify: no update, no wrap (PRM `.08.14`, `.08.30`).
            Op::OffsetMemory {
                data,
                index,
                offset,
                store,
            } => {
                let address =
                    scaled(self.i[index], offset).ok_or(MachineError::IndexOverflow(pc))?;
                if store {
                    self.store_ureg(bus, data, address, Some(index))?;
                } else {
                    self.load_ureg(bus, data, address, Some(index))?;
                }
            }
            // PRM `.08.18`: the NW form scales like a normal-word offset; the
            // plain form adds raw addresses. MODIFY wraps a ring whatever
            // CBUFEN says (`.08.16`).
            Op::Modify {
                index,
                offset,
                words,
            } => self.i[index] = self.modified(index, offset, words, true)?,
            Op::Direct {
                data,
                address,
                store,
            } => {
                if store {
                    self.store_ureg(bus, data as u8, address, None)?;
                } else {
                    self.load_ureg(bus, data as u8, address, None)?;
                }
            }
            // PRM `.06.090`: in byte space the PEy byte is 4 addresses on.
            Op::ByteStore { data, address } => {
                bus.write_byte(address, self.r[data] as u8)?;
                if self.simd() {
                    let next = address
                        .checked_add(4)
                        .ok_or(MachineError::IndexOverflow(pc))?;
                    bus.write_byte(next, self.s[data] as u8)?;
                }
            }
            Op::SystemBit {
                operation,
                sysreg,
                mask,
            } => self.system_bit(operation, sysreg, mask),
            Op::Stacks { push, pop } => self.stacks(push, pop)?,
            Op::FlushCache => {}
            // PRM `.17.21`: I7 = I6 and I6 = DM(0, I6), both from the old I6.
            Op::Rframe => {
                let frame = self.i[6];
                self.i[6] = self.load(bus, frame)?;
                self.i[7] = frame;
            }
            Op::Branch {
                call,
                delayed,
                condition,
                target,
            } => {
                if self.condition(condition) {
                    // VISA relative targets count parcels from the branch.
                    let target = match target {
                        Target::Absolute(address) => address,
                        Target::Relative(delta) => pc.wrapping_add_signed(delta),
                        Target::Indirect { index, modifier } => {
                            self.i[index].wrapping_add_signed(self.m[modifier])
                        }
                    };
                    if call {
                        self.push_pc(next)?;
                    }
                    if delayed {
                        self.delayed = Some(DelayedBranch { target, slots: 2 });
                    } else {
                        next = target;
                    }
                }
            }
            // Type 25a (ABI record §4): the two delay slots push R2 and the
            // return literal into the new frame. `R2 = I6` is a ureg move.
            Op::CJump { target } => {
                self.copy(2, 0x16);
                self.i[6] = self.i[7];
                self.delayed = Some(DelayedBranch { target, slots: 2 });
            }
            Op::Return { interrupt } => {
                next = self.pop_pc()?;
                if interrupt {
                    let (bit, status) = self
                        .active
                        .pop()
                        .ok_or(MachineError::RtiWithoutInterrupt(pc))?;
                    if status {
                        let frame = self
                            .status_stack
                            .pop()
                            .ok_or(MachineError::StatusStackUnderflow(pc))?;
                        self.restore(frame);
                    }
                    self.imaskp &= !(1 << bit);
                }
            }
            Op::Do { end, count } => {
                self.lcntr = match count {
                    LoopSource::Immediate(value) => value,
                    LoopSource::Register(register) => self.r[register],
                };
                if self.lcntr == 0 {
                    return Err(MachineError::ZeroLoopCount(pc));
                }
                if self.loop_stack.len() == LOOP_STACK_DEPTH {
                    return Err(MachineError::LoopStackOverflow(pc));
                }
                let end = pc.wrapping_add_signed(end);
                if self.loop_stack.last().is_some_and(|frame| frame.end == end) {
                    return Err(MachineError::SharedLoopEnd(pc));
                }
                self.push_pc(next)?;
                self.loop_stack.push(LoopFrame {
                    start: next,
                    end,
                    count: self.lcntr,
                });
            }
        }
        match pending {
            Some(DelayedBranch { target, slots: 1 }) => next = target,
            Some(branch) => {
                self.delayed = Some(DelayedBranch {
                    slots: branch.slots - 1,
                    ..branch
                });
            }
            None => {}
        }
        if let Some(frame) = self.loop_stack.last().copied()
            && frame.end == pc
        {
            if frame.count == 1 {
                self.loop_stack.pop();
                self.pop_pc()?;
            } else {
                self.loop_stack.last_mut().expect("frame").count -= 1;
                next = *self.pc_stack.last().expect("loop top");
            }
        }
        self.pc = next;
        Ok(())
    }

    fn simd(&self) -> bool {
        self.mode1 & MODE1_PEYEN != 0
    }

    /// Types 1 and 3: an optional compute with one or two DAG accesses, all
    /// reading the registers as they were before the instruction (stores and
    /// computes first, then loads; the decoders refuse two writes to one
    /// register). Post-modify updates I last.
    fn transfer(
        &mut self,
        bus: &mut impl Bus,
        compute: Option<Compute>,
        accesses: [Option<Access>; 2],
    ) -> Result<(), MachineError> {
        let mut planned = [None; 2];
        for (plan, access) in planned.iter_mut().zip(accesses) {
            if let Some(access) = access {
                let modifier = self.m[access.modifier];
                let address = if access.post {
                    self.i[access.index]
                } else {
                    scaled(self.i[access.index], modifier)
                        .ok_or(MachineError::IndexOverflow(self.pc))?
                };
                let updated = if access.post {
                    Some(self.post_modify(access.index, modifier)?)
                } else {
                    None
                };
                *plan = Some((access, address, updated));
            }
        }
        let effects = self.compute(compute);
        for &(access, address, _) in planned.iter().flatten().filter(|p| p.0.store) {
            self.store_ureg(bus, access.data, address, Some(access.index))?;
        }
        self.commit(effects);
        for &(access, address, _) in planned.iter().flatten().filter(|p| !p.0.store) {
            self.load_ureg(bus, access.data, address, Some(access.index))?;
        }
        for (access, _, updated) in planned.into_iter().flatten() {
            if let Some(updated) = updated {
                self.i[access.index] = updated;
            }
        }
        Ok(())
    }

    /// I after a post-modify access: wrapped when CBUFEN is set.
    fn post_modify(&self, index: usize, modifier: i32) -> Result<u32, MachineError> {
        self.modified(index, modifier, true, self.mode1 & MODE1_CBUFEN != 0)
    }

    /// `I + M`, wrapped into the ring `[B, B + L)` when `ring` and L != 0
    /// (PRM `.08.32`): once down by L when M >= 0 and the sum reaches B + L,
    /// once up by L when M < 0 and the sum falls below B. With `words`, a
    /// byte-space pointer scales M and L by 4 (normal-word units, `.08.18`).
    fn modified(
        &self,
        index: usize,
        modifier: i32,
        words: bool,
        ring: bool,
    ) -> Result<u32, MachineError> {
        let pointer = self.i[index];
        let unit = if words {
            i64::from(AddressUnit::of(pointer).scale())
        } else {
            1
        };
        let step = i64::from(modifier) * unit;
        let length = i64::from(self.l[index]) * unit;
        let mut next = i64::from(pointer) + step;
        if ring && length != 0 {
            if length > i64::from(i32::MAX) || step.abs() >= length {
                return Err(MachineError::CircularBuffer { pc: self.pc, index });
            }
            let base = i64::from(self.b[index]);
            if step >= 0 && next >= base + length {
                next -= length;
            } else if step < 0 && next < base {
                next += length;
            }
        }
        u32::try_from(next).map_err(|_| MachineError::IndexOverflow(self.pc))
    }

    /// The PEy half of a normal-word ureg transfer at `address` (PRM `.08.33`,
    /// `.08.36`, `.06.090`). A load through I1 with BDCST1 or I9 with BDCST9
    /// into a data register also loads its complement from the same address,
    /// in SISD too. Otherwise SIMD moves the complementary ureg at the next
    /// normal word: `address + 1` in word space, `+ 4` in byte space. The
    /// implicit address does not wrap in a ring (`.08.31`).
    fn implicit(
        &self,
        ureg: u8,
        address: u32,
        index: Option<usize>,
        store: bool,
    ) -> Result<Option<(u8, u32)>, MachineError> {
        let Some(complement) = complement(ureg) else {
            return Ok(None);
        };
        let broadcast = match index {
            Some(1) => MODE1_BDCST1,
            Some(9) => MODE1_BDCST9,
            _ => 0,
        };
        if !store && is_data(ureg) && self.mode1 & broadcast != 0 {
            return Ok(Some((complement, address)));
        }
        if !self.simd() {
            return Ok(None);
        }
        let next = scaled(address, 1).ok_or(MachineError::IndexOverflow(self.pc))?;
        Ok(Some((complement, next)))
    }

    fn store_ureg(
        &mut self,
        bus: &mut impl Bus,
        ureg: u8,
        address: u32,
        index: Option<usize>,
    ) -> Result<(), MachineError> {
        let implicit = self.implicit(ureg, address, index, true)?;
        let value = self.read_ureg(ureg);
        let implicit = implicit.map(|(complement, at)| (at, self.read_ureg(complement)));
        self.store(bus, address, value)?;
        if let Some((at, value)) = implicit {
            self.store(bus, at, value)?;
        }
        Ok(())
    }

    fn load_ureg(
        &mut self,
        bus: &mut impl Bus,
        ureg: u8,
        address: u32,
        index: Option<usize>,
    ) -> Result<(), MachineError> {
        let implicit = self.implicit(ureg, address, index, false)?;
        let value = self.load(bus, address)?;
        let implicit = implicit
            .map(|(complement, at)| self.load(bus, at).map(|value| (complement, value)))
            .transpose()?;
        self.write_ureg(ureg, value);
        if let Some((complement, value)) = implicit {
            self.write_ureg(complement, value);
        }
        Ok(())
    }

    /// `dst = src`. In SIMD a destination with a complement also takes the
    /// complement of the source, or the source itself when it has none
    /// (`R5 = I8` loads R5 and S5; `I8 = R5` only I8; PRM `.08.36`,
    /// `.06.084`). Data register to data register moves carry all 40 bits (I:
    /// PRM 4.10 swaps and PX transfers are 40-bit; DAG transfers use 39:8).
    fn copy(&mut self, dst: u8, src: u8) {
        let read = |m: &Self, code: u8| (m.read_ureg(code), m.extension_of(code));
        let explicit = read(self, src);
        let implicit = complement(dst)
            .filter(|_| self.simd())
            .map(|cdst| (cdst, read(self, complement(src).unwrap_or(src))));
        for (dst, (value, extension)) in std::iter::once((dst, explicit)).chain(implicit) {
            self.write_ureg(dst, value);
            if let (Some(extension), Some(slot)) = (extension, self.extension_mut(dst)) {
                *slot = extension;
            }
        }
    }

    fn extension_of(&self, code: u8) -> Option<u8> {
        let n = usize::from(code & 15);
        match code >> 4 {
            0 => Some(self.extension[n]),
            5 => Some(self.s_extension[n]),
            _ => None,
        }
    }
    fn extension_mut(&mut self, code: u8) -> Option<&mut u8> {
        let n = usize::from(code & 15);
        match code >> 4 {
            0 => Some(&mut self.extension[n]),
            5 => Some(&mut self.s_extension[n]),
            _ => None,
        }
    }

    /// Normal-word data access; the core-internal `SYSCTL` never reaches the bus.
    fn load(&self, bus: &mut impl Bus, address: u32) -> Result<u32, BusError> {
        if address == SYSCTL {
            return Ok(self.sysctl);
        }
        bus.read(address)
    }
    fn store(&mut self, bus: &mut impl Bus, address: u32, value: u32) -> Result<(), BusError> {
        if address == SYSCTL {
            self.sysctl = value;
            return Ok(());
        }
        bus.write(address, value)
    }

    /// The effects of `compute` on PEx and, in SIMD, PEy (PRM `.06.086`),
    /// from the registers before the instruction writes.
    fn compute(&self, compute: Option<Compute>) -> [Option<Effects>; 2] {
        let Some(compute) = compute else {
            return [None, None];
        };
        [
            Some(self.effects(compute, false)),
            self.simd().then(|| self.effects(compute, true)),
        ]
    }

    fn commit(&mut self, effects: [Option<Effects>; 2]) {
        for (y, effects) in [false, true].into_iter().zip(effects) {
            let Some(effects) = effects else { continue };
            let (registers, extension, flags, sticky) = if y {
                (
                    &mut self.s,
                    &mut self.s_extension,
                    &mut self.flags_y,
                    &mut self.stkyy,
                )
            } else {
                (
                    &mut self.r,
                    &mut self.extension,
                    &mut self.flags,
                    &mut self.stkyx,
                )
            };
            for (dst, bits) in effects.writes.into_iter().flatten() {
                registers[dst] = (bits >> 8) as u32;
                extension[dst] = bits as u8;
            }
            effects.flags.apply(flags);
            *sticky |= effects.sticky;
        }
    }

    /// One PE's data registers: PEx (`R`) or PEy (`S`).
    fn bank(&self, y: bool) -> (&[u32; 16], &[u8; 16]) {
        if y {
            (&self.s, &self.s_extension)
        } else {
            (&self.r, &self.extension)
        }
    }

    fn effects(&self, compute: Compute, y: bool) -> Effects {
        let (registers, _) = self.bank(y);
        // Shifter status (SZ/SV/SS) is separate from the ALU flags; a shift
        // result is a 32-bit write.
        let shift = |dst: usize, value: u32| Effects {
            writes: [Some((dst, u64::from(value) << 8)), None, None],
            ..Effects::default()
        };
        match compute {
            Compute::Alu { op, dst, lhs, rhs } => {
                let (result, flags, sticky) = self.operate(y, op, lhs, rhs);
                Effects {
                    writes: [result.map(|bits| (dst, bits)), None, None],
                    flags,
                    sticky,
                }
            }
            Compute::Shift { dst, src, amount } => {
                shift(dst, logical_shift(registers[src], amount))
            }
            Compute::ShiftBy { dst, src, amount } => shift(
                dst,
                logical_shift(registers[src], registers[amount] as u8 as i8),
            ),
            // PRM `.05.45`: each part works as its single function; dual
            // add/subtract ORs the ALU flags. The multiplier sets no ALU flag.
            Compute::Multi {
                product,
                xm,
                ym,
                xa,
                ya,
                sum,
                difference,
            } => {
                let (result, _, mut sticky) = self.operate(y, ComputeOp::FloatMultiply, xm, ym);
                let mut writes = [result.map(|bits| (product, bits)), None, None];
                let mut flags = FlagUpdate::None;
                let alu = [
                    (ComputeOp::FloatAdd, sum),
                    (ComputeOp::FloatSubtract, difference),
                ];
                for (slot, (op, dst)) in writes[1..].iter_mut().zip(alu) {
                    let Some(dst) = dst else { continue };
                    let (result, update, alu_sticky) = self.operate(y, op, xa, ya);
                    *slot = result.map(|bits| (dst, bits));
                    flags = flags.or(update);
                    sticky |= alu_sticky;
                }
                Effects {
                    writes,
                    flags,
                    sticky,
                }
            }
        }
    }

    /// One ALU or multiplier operation on PE `y`: the 40-bit result (`None`
    /// for compares), its flag update and the stickies it raises.
    fn operate(
        &self,
        y: bool,
        op: ComputeOp,
        lhs: usize,
        rhs: usize,
    ) -> (Option<u64>, FlagUpdate, u32) {
        let (registers, extension) = self.bank(y);
        let (x, y) = (registers[lhs], registers[rhs]);
        let mut sticky = 0;
        // 40-bit result, and the float order (x vs y for COMP, else the result
        // vs zero) that sets AZ/AN. `None` for NaN or a non-float result.
        let (result, order) = match self.arithmetic {
            Arithmetic::HostF32 => {
                let result = evaluate(op, x, y).map(|bits| u64::from(bits) << 8);
                let float = |bits: u32| f32::from_bits(bits);
                let order = match op {
                    ComputeOp::FloatCompare => float(x).partial_cmp(&float(y)),
                    _ => result
                        .filter(|_| op.float_result())
                        .and_then(|bits| float((bits >> 8) as u32).partial_cmp(&0.0)),
                };
                (result, order)
            }
            Arithmetic::Prm => {
                let rnd32 = self.mode1 & MODE1_RND32 != 0;
                let wide = |n: usize| u64::from(registers[n]) << 8 | u64::from(extension[n]);
                let (x, y) = (wide(lhs), wide(rhs));
                let result = evaluate_prm(op, x, y, rnd32);
                if invalid(op, x, y, rnd32, result) {
                    sticky = match op {
                        ComputeOp::FloatMultiply => STKYX_MIS,
                        _ => STKYX_AIS,
                    };
                }
                let order = match op {
                    ComputeOp::FloatCompare => compare_prm(x, y, rnd32),
                    _ => result
                        .filter(|_| op.float_result())
                        .and_then(|bits| compare_prm(bits, 0, false)),
                };
                (result, order)
            }
        };
        let flags = match op {
            // The multiplier does not update ALU flags.
            ComputeOp::FloatMultiply => FlagUpdate::None,
            ComputeOp::IntegerCompare => FlagUpdate::ZeroNegative {
                zero: x == y,
                negative: (x as i32) < (y as i32),
            },
            ComputeOp::IntegerCompareUnsigned => FlagUpdate::ZeroNegative {
                zero: x == y,
                negative: x < y,
            },
            // Float results and COMP (both read floats).
            _ if op.float_result() => FlagUpdate::ZeroNegative {
                zero: order == Some(Ordering::Equal),
                negative: order == Some(Ordering::Less),
            },
            _ => {
                let bits = (result.expect("non-compare result") >> 8) as u32;
                let (carry, overflow) = match op {
                    ComputeOp::IntegerAdd => add_flags(x, y),
                    ComputeOp::IntegerIncrement => add_flags(x, 1),
                    ComputeOp::IntegerSubtract => sub_flags(x, y),
                    ComputeOp::IntegerDecrement => sub_flags(x, 1),
                    _ => (false, false),
                };
                FlagUpdate::Integer {
                    zero: bits == 0,
                    negative: bits & (1 << 31) != 0,
                    carry,
                    overflow,
                }
            }
        };
        (result, flags, sticky)
    }

    /// PRM `.06.084`: in SIMD a branch condition is the AND over both PEs.
    fn condition(&self, condition: u8) -> bool {
        holds(self.flags, condition) && (!self.simd() || holds(self.flags_y, condition))
    }

    fn system_bit(&mut self, operation: BitOp, sysreg: SysReg, mask: u32) {
        let register = match sysreg {
            SysReg::Mode1 => &mut self.mode1,
            SysReg::Mmask => &mut self.mmask,
            SysReg::Mode2 => &mut self.mode2,
            SysReg::Imask => &mut self.imask,
            SysReg::Irptl => &mut self.irptl,
            SysReg::Stky => {
                let empty = (u32::from(self.loop_stack.is_empty()) << 26)
                    | (u32::from(self.status_stack.is_empty()) << 24)
                    | (u32::from(self.pc_stack.is_empty()) << 22);
                self.set_test(empty & mask == mask);
                return;
            }
        };
        match operation {
            BitOp::Set => *register |= mask,
            BitOp::Clear => *register &= !mask,
            BitOp::Test => {
                let all = *register & mask == mask;
                self.set_test(all);
            }
        }
    }
    /// BIT TST is a sequencer operation; both ASTATs get its result (I), so
    /// a SIMD branch on TF sees it.
    fn set_test(&mut self, value: bool) {
        self.flags.test = value;
        self.flags_y.test = value;
    }

    fn status(&self) -> StatusFrame {
        StatusFrame {
            flags: self.flags,
            flags_y: self.flags_y,
            mode1: self.mode1,
        }
    }
    fn restore(&mut self, frame: StatusFrame) {
        self.flags = frame.flags;
        self.flags_y = frame.flags_y;
        self.mode1 = frame.mode1;
    }

    fn stacks(&mut self, push: StackSet, pop: StackSet) -> Result<(), MachineError> {
        let pc = self.pc;
        if push.status {
            if self.status_stack.len() == STATUS_STACK_DEPTH {
                return Err(MachineError::StatusStackOverflow(pc));
            }
            self.status_stack.push(self.status());
            self.mode1 &= !self.mmask;
        }
        if pop.status {
            let frame = self
                .status_stack
                .pop()
                .ok_or(MachineError::StatusStackUnderflow(pc))?;
            self.restore(frame);
        }
        if pop.loops {
            self.loop_stack
                .pop()
                .ok_or(MachineError::LoopStackUnderflow(pc))?;
        }
        if pop.pc {
            self.pop_pc()?;
        }
        Ok(())
    }

    fn read_ureg(&self, code: u8) -> u32 {
        let index = usize::from(code & 15);
        match code >> 4 {
            0 => self.r[index],
            1 => self.i[index],
            2 => self.m[index] as u32,
            3 => self.l[index],
            4 => self.b[index],
            5 => self.s[index],
            _ => match SystemUreg::from_code(code).expect("decoder restricts uregs") {
                SystemUreg::Lcntr => self.lcntr,
                SystemUreg::CurLcntr => self.loop_stack.last().map_or(0, |f| f.count),
                SystemUreg::Emuclk => self.cycles as u32,
                SystemUreg::Ustat(n) => self.ustat[n],
                SystemUreg::Mode1 => self.mode1,
                SystemUreg::Mode2 => self.mode2,
                SystemUreg::Mmask => self.mmask,
                SystemUreg::Imask => self.imask,
                SystemUreg::Irptl => self.irptl,
                SystemUreg::Imaskp => self.imaskp,
            },
        }
    }

    fn write_ureg(&mut self, code: u8, value: u32) {
        let index = usize::from(code & 15);
        match code >> 4 {
            // A 32-bit write clears the eight LSBs (PRM 4.06).
            0 => {
                self.r[index] = value;
                self.extension[index] = 0;
            }
            1 => self.i[index] = value,
            2 => self.m[index] = value as i32,
            3 => self.l[index] = value,
            4 => {
                self.b[index] = value;
                self.i[index] = value;
            }
            5 => {
                self.s[index] = value;
                self.s_extension[index] = 0;
            }
            _ => match SystemUreg::from_code(code).expect("decoder restricts uregs") {
                SystemUreg::Lcntr => self.lcntr = value,
                // Read-only views: the decoder rejects writes to these.
                SystemUreg::CurLcntr | SystemUreg::Emuclk => {}
                SystemUreg::Ustat(n) => self.ustat[n] = value,
                SystemUreg::Mode1 => self.mode1 = value,
                SystemUreg::Mode2 => self.mode2 = value,
                SystemUreg::Mmask => self.mmask = value,
                SystemUreg::Imask => self.imask = value,
                SystemUreg::Irptl => self.irptl = value,
                SystemUreg::Imaskp => self.imaskp = value,
            },
        }
    }
}

/// `pointer + offset`, with the offset scaled for a byte-space pointer
/// (normal-word access, PRM `.08.18`). `None` on 32-bit overflow.
fn scaled(pointer: u32, offset: i32) -> Option<u32> {
    let scale = AddressUnit::of(pointer).scale() as i32;
    pointer.checked_add_signed(offset.checked_mul(scale)?)
}

/// PRM AI (ALU) / MI (multiplier): a NaN float operand after the operand rules,
/// infinity − infinity or 0 · infinity (an all-ones result from non-NaN
/// operands), or `FIX` of ±infinity (ALUSAT clear, PRM 23.15). Overflow is AV.
fn invalid(op: ComputeOp, x: u64, y: u64, rnd32: bool, result: Option<u64>) -> bool {
    let nan = |bits: u64, rnd32: bool| compare_prm(bits, bits, rnd32).is_none();
    let mut sources = [x, y].into_iter().take(op.float_sources());
    if sources.any(|bits| nan(bits, rnd32)) {
        return true;
    }
    match op {
        ComputeOp::FloatAdd | ComputeOp::FloatSubtract | ComputeOp::FloatMultiply => {
            result.is_some_and(|bits| nan(bits, false))
        }
        // Biased exponent 255 (bits 38:31) with no NaN fraction: ±infinity.
        ComputeOp::FixToInteger => (x >> 31) & 0xff == 0xff,
        _ => false,
    }
}

fn logical_shift(x: u32, amount: i8) -> u32 {
    if amount >= 0 {
        x.checked_shl(amount as u32).unwrap_or(0)
    } else {
        x.checked_shr(u32::from(amount.unsigned_abs())).unwrap_or(0)
    }
}

fn add_flags(x: u32, y: u32) -> (bool, bool) {
    let (_, carry) = x.overflowing_add(y);
    let (_, overflow) = (x as i32).overflowing_add(y as i32);
    (carry, overflow)
}

/// SHARC AC on subtraction is NOT borrow.
fn sub_flags(x: u32, y: u32) -> (bool, bool) {
    let (_, borrow) = x.overflowing_sub(y);
    let (_, overflow) = (x as i32).overflowing_sub(y as i32);
    (!borrow, overflow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Absolute(u32),
    Relative(i32),
    Indirect { index: usize, modifier: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BitOp {
    Set,
    Clear,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SysReg {
    Mode1,
    Mmask,
    Mode2,
    Imask,
    Irptl,
    Stky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct StackSet {
    loops: bool,
    status: bool,
    pc: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemUreg {
    Lcntr,
    CurLcntr,
    Emuclk,
    Ustat(usize),
    Mode1,
    Mode2,
    Mmask,
    Imask,
    Irptl,
    Imaskp,
}

impl SystemUreg {
    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0x67 => Self::CurLcntr,
            0x68 => Self::Lcntr,
            0x69 => Self::Emuclk,
            0x70..=0x71 => Self::Ustat((code - 0x70) as usize),
            0x72 => Self::Mode1,
            0x73 => Self::Mmask,
            0x74 => Self::Mode2,
            0x7a => Self::Irptl,
            0x7b => Self::Imask,
            0x7c => Self::Imaskp,
            _ => return None,
        })
    }
    const fn writable(self) -> bool {
        !matches!(self, Self::CurLcntr | Self::Emuclk)
    }
}

/// R, I, M, L, B of both DAGs, S (PEy), plus the modeled system registers.
const fn ureg(code: u8, write: bool) -> Option<u8> {
    match code >> 4 {
        0..=5 => Some(code),
        _ => match SystemUreg::from_code(code) {
            Some(system) if !write || system.writable() => Some(code),
            _ => None,
        },
    }
}

/// The SIMD complement of a modeled ureg (PRM `.06.084`): Rn and Sn, USTAT1
/// and USTAT2. Other uregs have none; SIMD moves them on PEx only.
const fn complement(code: u8) -> Option<u8> {
    match code {
        0x00..=0x0f => Some(code + 0x50),
        0x50..=0x5f => Some(code - 0x50),
        0x70 => Some(0x71),
        0x71 => Some(0x70),
        _ => None,
    }
}

/// R or S. Broadcast loads reach only these (PRM `.08.33`).
const fn is_data(code: u8) -> bool {
    matches!(code >> 4, 0 | 5)
}

/// An I, M, L or B register of DAG2 (`dag2`) or DAG1.
const fn in_dag(code: u8, dag2: bool) -> bool {
    matches!(code >> 4, 1..=4) && (code & 15 >= 8) == dag2
}

fn condition_code(word: u64) -> Option<u8> {
    let code = ((word >> 33) & 31) as u8;
    matches!(code, 0..=2 | 13 | 16..=18 | 29 | 31).then_some(code)
}

fn holds(f: Flags, condition: u8) -> bool {
    match condition {
        0 => f.zero,
        1 => f.negative && !f.zero,
        2 => f.negative || f.zero,
        3 => f.carry,
        13 => f.test,
        16 => !f.zero,
        17 => !f.negative || f.zero,
        18 => !f.negative && !f.zero,
        19 => !f.carry,
        29 => !f.test,
        31 => true,
        _ => unreachable!("decoder restricts conditions"),
    }
}

/// The ALU flags one compute writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FlagUpdate {
    #[default]
    None,
    /// Float results and compares: AZ and AN only.
    ZeroNegative { zero: bool, negative: bool },
    /// Fixed-point results.
    Integer {
        zero: bool,
        negative: bool,
        carry: bool,
        overflow: bool,
    },
}

impl FlagUpdate {
    fn apply(self, flags: &mut Flags) {
        match self {
            Self::None => {}
            Self::ZeroNegative { zero, negative } => {
                (flags.zero, flags.negative) = (zero, negative);
            }
            Self::Integer {
                zero,
                negative,
                carry,
                overflow,
            } => {
                (flags.zero, flags.negative) = (zero, negative);
                (flags.carry, flags.overflow) = (carry, overflow);
            }
        }
    }
    /// PRM `.05.45`: dual add/subtract ORs the ALU flags of its two parts.
    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, update) | (update, Self::None) => update,
            (
                Self::ZeroNegative { zero, negative },
                Self::ZeroNegative {
                    zero: zero2,
                    negative: negative2,
                },
            ) => Self::ZeroNegative {
                zero: zero || zero2,
                negative: negative || negative2,
            },
            _ => unreachable!("multifunction ALU parts are float"),
        }
    }
}

/// What a compute does on one PE: 40-bit register writes, flags, stickies.
#[derive(Debug, Clone, Copy, Default)]
struct Effects {
    writes: [Option<(usize, u64)>; 3],
    flags: FlagUpdate,
    sticky: u32,
}

/// A compute field (PRM 21.x) or a Type 6a shift. Registers are 0-15 of the
/// PE that runs it (R on PEx, S on PEy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compute {
    Alu {
        op: ComputeOp,
        dst: usize,
        lhs: usize,
        rhs: usize,
    },
    /// Type 6a `Rn = LSHIFT Rx BY data8`.
    Shift { dst: usize, src: usize, amount: i8 },
    /// `Rn = LSHIFT Rx BY Ry`.
    ShiftBy {
        dst: usize,
        src: usize,
        amount: usize,
    },
    /// Float multifunction (PRM 21.15, 21.16): `Fm = Fxm * Fym` with
    /// `Fa = Fxa + Fya` and/or `Fs = Fxa - Fya`.
    Multi {
        product: usize,
        xm: usize,
        ym: usize,
        xa: usize,
        ya: usize,
        sum: Option<usize>,
        difference: Option<usize>,
    },
}

impl Compute {
    fn destinations(self) -> [Option<usize>; 3] {
        match self {
            Self::Alu {
                op:
                    ComputeOp::FloatCompare
                    | ComputeOp::IntegerCompare
                    | ComputeOp::IntegerCompareUnsigned,
                ..
            } => [None; 3],
            Self::Alu { dst, .. } | Self::Shift { dst, .. } | Self::ShiftBy { dst, .. } => {
                [Some(dst), None, None]
            }
            Self::Multi {
                product,
                sum,
                difference,
                ..
            } => [Some(product), sum, difference],
        }
    }
}

/// One DAG access of Types 1 and 3. `data` is a ureg code; `index` and
/// `modifier` are 0-15 (DAG2 is 8-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Access {
    data: u8,
    index: usize,
    modifier: usize,
    post: bool,
    store: bool,
}

/// One instruction's register writes are distinct: compute results and data
/// register loads, by register number (SIMD writes both banks). The PRM does
/// not define a double write, so the decoders refuse it (**I**).
fn distinct(compute: Option<Compute>, accesses: [Option<Access>; 2]) -> bool {
    let loads = accesses
        .into_iter()
        .flatten()
        .filter(|access| !access.store && is_data(access.data))
        .map(|access| usize::from(access.data & 15));
    let results = compute.map_or([None; 3], Compute::destinations);
    let mut seen = 0_u16;
    results.into_iter().flatten().chain(loads).all(|n| {
        let fresh = seen & (1 << n) == 0;
        seen |= 1 << n;
        fresh
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Nop,
    Idle,
    Compute(Compute),
    Immediate {
        ureg: u8,
        value: u32,
    },
    Move {
        dst: u8,
        src: u8,
    },
    /// Types 1 and 3: an optional compute with one or two DAG accesses.
    Transfer {
        compute: Option<Compute>,
        accesses: [Option<Access>; 2],
    },
    OffsetMemory {
        data: u8,
        index: usize,
        offset: i32,
        store: bool,
    },
    /// Type 16a `DM(Ia, Mb) = imm32` / `PM(Ic, Md) = imm32`, post-modify.
    ImmediateStore {
        index: usize,
        modifier: usize,
        value: u32,
    },
    /// `words`: the NW form (scaled by the pointer's space), else plain.
    Modify {
        index: usize,
        offset: i32,
        words: bool,
    },
    Direct {
        data: usize,
        address: u32,
        store: bool,
    },
    ByteStore {
        data: usize,
        address: u32,
    },
    SystemBit {
        operation: BitOp,
        sysreg: SysReg,
        mask: u32,
    },
    Stacks {
        push: StackSet,
        pop: StackSet,
    },
    FlushCache,
    Rframe,
    /// Decoders never produce a delayed CALL (its return address is not modeled).
    Branch {
        call: bool,
        delayed: bool,
        condition: u8,
        target: Target,
    },
    Return {
        interrupt: bool,
    },
    /// Type 25a `CJUMP addr24 (DB)`.
    CJump {
        target: u32,
    },
    Do {
        end: i32,
        count: LoopSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopSource {
    Immediate(u32),
    Register(usize),
}

/// Raw-word decoder, independent of the encoder. Unlisted bits must be zero.
fn decode(word: u64) -> Option<Op> {
    const ALWAYS: u64 = 31 << 33;
    match word {
        0 => return Some(Op::Nop),
        0x0080_0000_0000 => return Some(Op::Idle),
        0x0a3e_0000_0000 => return Some(Op::Return { interrupt: false }),
        0x0b3e_0000_0000 => return Some(Op::Return { interrupt: true }),
        0x1702_0000_0000 => return Some(Op::FlushCache),
        0x1900_0000_0000 => return Some(Op::Rframe),
        0x1720_0000_0000 => {
            return Some(Op::Stacks {
                push: StackSet {
                    status: true,
                    ..StackSet::default()
                },
                pop: StackSet::default(),
            });
        }
        _ => {}
    }
    let opcode = word >> 40;
    match opcode {
        // Type2a compute, COND=31; the 23-bit field holds a single or a multifunction compute
        // (stock `013e00598c70` = `F8=F1*F7, F12=F8-F12`).
        0x01 if word & !0x7f_ffff == 0x013e_0000_0000 => decode_compute(word).map(Op::Compute),
        // Type6a shifter immediate, LSHIFT only; DATAEX must sign-fill data8.
        0x02 if word & !(0xf << 27 | 0xffff) == 0x023e_0000_0000 => {
            let amount = (word >> 8) as u8 as i8;
            let dataex = (word >> 27) & 0xf;
            (dataex == if amount < 0 { 0xf } else { 0 }).then_some(Op::Compute(Compute::Shift {
                dst: ((word >> 4) & 15) as usize,
                src: (word & 15) as usize,
                amount,
            }))
        }
        // Type8a: A (bit 38) is (LA), loop abort, which is not modeled.
        0x06 if word & (1 << 38 | 0x3f << 24) == 0 => Some(Op::Branch {
            call: word & (1 << 39) != 0,
            delayed: false,
            condition: condition_code(word)?,
            target: if word & (1 << 40) != 0 {
                Target::Relative(((word as u32 & 0xff_ffff) << 8) as i32 >> 8)
            } else {
                Target::Absolute(word as u32 & 0xff_ffff)
            },
        }),
        0x07 if word & (1 << 38 | 0x3f << 24) == 0 => Some(Op::Branch {
            call: word & (1 << 39) != 0,
            delayed: false,
            condition: condition_code(word)?,
            target: Target::Relative(((word as u32 & 0xff_ffff) << 8) as i32 >> 8),
        }),
        // Type9a indirect, DAG2; J (bit 26) = (DB) for JUMP only.
        0x08 if word & 0x03ff_ffff == 0
            && word & (1 << 38) == 0
            && word & (1 << 39 | 1 << 26) != 1 << 39 | 1 << 26 =>
        {
            Some(Op::Branch {
                call: word & (1 << 39) != 0,
                delayed: word & (1 << 26) != 0,
                condition: condition_code(word)?,
                target: Target::Indirect {
                    index: 8 + ((word >> 30) & 7) as usize,
                    modifier: 8 + ((word >> 27) & 7) as usize,
                },
            })
        }
        // Type12a counter loops: immediate (0x0c) or data register (0x0d).
        // Mode (bit 23) picks F1- or E2-active execution (PRM `.06.048`),
        // which runs the same loop; RELADDR is 22:0.
        0x0c if (word >> 24) & 0xffff != 0 => Some(Op::Do {
            end: (word & 0x7f_ffff) as i32,
            count: LoopSource::Immediate(((word >> 24) & 0xffff) as u32),
        }),
        0x0d if word & 0xff80_ff00_0000 == 0x0d00_0000_0000 && (word >> 32) & 0x7f < 16 => {
            Some(Op::Do {
                end: (word & 0x7f_ffff) as i32,
                count: LoopSource::Register(((word >> 32) & 15) as usize),
            })
        }
        0x0f => Some(Op::Immediate {
            ureg: ureg(((word >> 32) & 0xff) as u8, true)?,
            value: word as u32,
        }),
        // Type14a direct DM, R0-R15 only.
        0x10 | 0x11 if word & (0xf0 << 32) == 0 => Some(Op::Direct {
            data: ((word >> 32) & 15) as usize,
            address: word as u32,
            store: opcode == 0x11,
        }),
        // Type14d byte store `DM(addr32) = Rn (bw)`: D=1, EX=L=W=X=0.
        0x1b if word & (0xf0 << 32) == 0 => Some(Op::ByteStore {
            data: ((word >> 32) & 15) as usize,
            address: word as u32,
        }),
        0x14 => {
            let operation = match (word >> 37) & 7 {
                0 => BitOp::Set,
                1 => BitOp::Clear,
                4 => BitOp::Test,
                _ => return None,
            };
            let sysreg = match (word >> 32) & 31 {
                2 => SysReg::Mode1,
                3 => SysReg::Mmask,
                4 => SysReg::Mode2,
                8 if operation == BitOp::Test => SysReg::Stky,
                0xa => SysReg::Irptl,
                0xb => SysReg::Imask,
                _ => return None,
            };
            Some(Op::SystemBit {
                operation,
                sysreg,
                mask: word as u32,
            })
        }
        // Type 19a self-index MODIFY (W 39, G 38, IDIS 37:35, IS 34:32):
        // `0x15` with W = 1 is the NW form, `0x16` with W = 0 the plain form.
        // G selects DAG2. SW (`0x15`, W = 0) and cross-index are refused.
        0x15 | 0x16 if (word >> 35) & 0x17 == u64::from(opcode == 0x15) << 4 => Some(Op::Modify {
            index: 8 * ((word >> 38) & 1) as usize + ((word >> 32) & 7) as usize,
            offset: word as u32 as i32,
            words: opcode == 0x15,
        }),
        // Type25a `CJUMP addr24 (DB)`; the non-delayed form is refused.
        0x18 if word & !0xff_ffff == 0x1804_0000_0000 => Some(Op::CJump {
            target: word as u32 & 0xff_ffff,
        }),
        0x17 => {
            let pops = (1 << 38) | (1 << 36) | (1 << 34);
            if word & !(pops | (1 << 33)) != 0x1700_0000_0000 || word & pops == 0 {
                return None;
            }
            Some(Op::Stacks {
                push: StackSet::default(),
                pop: StackSet {
                    loops: word & (1 << 38) != 0,
                    status: word & (1 << 36) != 0,
                    pc: word & (1 << 34) != 0,
                },
            })
        }
        0x20..=0x3f => decode_dual(word),
        // Type3a compute with DM(Ia, Mb) or PM(Ic, Md) <-> ureg; COND=31, L=0.
        0x40..=0x5f if word & ALWAYS == ALWAYS && word & (1 << 30) == 0 => {
            let field = word & 0x7f_ffff;
            let compute = if field == 0 {
                None
            } else {
                Some(decode_compute(field)?)
            };
            let accesses = [Some(dm_transfer(word >> 16)?), None];
            distinct(compute, accesses).then_some(Op::Transfer { compute, accesses })
        }
        // Type5a ureg move, COND=31.
        0x70..=0x77 if word & ALWAYS == ALWAYS && word & 0x407f_ffff == 0 => {
            let source = (((word >> 38) & 0x1f) << 2) | ((word >> 31) & 3);
            Some(Op::Move {
                dst: ureg(((word >> 23) & 0x7f) as u8, true)?,
                src: ureg(source as u8, false)?,
            })
        }
        // Type16a `DM(Ia, Mb) = imm32`: G 37 selects DAG2, bits 36:32 zero.
        0x90..=0x9f if word & (0x1f << 32) == 0 => {
            let base = 8 * ((word >> 37) & 1) as usize;
            Some(Op::ImmediateStore {
                index: base + ((word >> 41) & 7) as usize,
                modifier: base + ((word >> 38) & 7) as usize,
                value: word as u32,
            })
        }
        // Type15a normal-word universal-register transfer; G 44 selects DAG2.
        0xa0..=0xbf if word & (1 << 39) == 0 => {
            let (store, dag2) = (word & (1 << 40) != 0, word & (1 << 44) != 0);
            Some(Op::OffsetMemory {
                data: offset_ureg(((word >> 32) & 0x7f) as u8, store, dag2)?,
                index: 8 * usize::from(dag2) + ((word >> 41) & 7) as usize,
                offset: word as u32 as i32,
                store,
            })
        }
        _ => None,
    }
}

/// Offset loads may target the access's DAG; stores must source another DAG
/// or a data register.
fn offset_ureg(code: u8, store: bool, dag2: bool) -> Option<u8> {
    if store && in_dag(code, dag2) {
        return None;
    }
    ureg(code, !store)
}

/// Type 3a/3b ACCESS (PRM 14.14, 14.16) from the Type 3b field layout (Type
/// 3a's upper 32 bits): U 28, I 27:25, M 24:22, G 16, D 15, UREG 13:7. G
/// selects `PM(Ic, Md)` on DAG2 (I8-I15, M8-M15). Post-modify and stores
/// need a ureg outside that DAG (UREGXDAG1, UREGXDAG2).
fn dm_transfer(fields: u64) -> Option<Access> {
    let store = fields & (1 << 15) != 0;
    let post = fields & (1 << 28) != 0;
    let dag2 = fields & (1 << 16) != 0;
    let code = ((fields >> 7) & 0x7f) as u8;
    if in_dag(code, dag2) && (store || post) {
        return None;
    }
    let base = 8 * usize::from(dag2);
    Some(Access {
        data: ureg(code, !store)?,
        index: base + ((fields >> 25) & 7) as usize,
        modifier: base + ((fields >> 22) & 7) as usize,
        post,
        store,
    })
}

/// Type 1a (PRM 14.02, figure 14.03.1) accesses: DMD 44, DMI 43:41, DMM
/// 40:38, DMDREG 36:33 through DAG1; PMD 37, PMI 32:30, PMM 29:27, PMDREG
/// 26:23 through DAG2. Both post-modify; D = 1 stores.
fn dual_accesses(word: u64) -> [Option<Access>; 2] {
    let field = |shift: u32, mask: u64| ((word >> shift) & mask) as usize;
    let access = |store: u32, index: u32, modifier: u32, data: u32, base: usize| Access {
        data: field(data, 15) as u8,
        index: base + field(index, 7),
        modifier: base + field(modifier, 7),
        post: true,
        store: field(store, 1) == 1,
    };
    [
        Some(access(44, 41, 38, 33, 0)),
        Some(access(37, 30, 27, 23, 8)),
    ]
}

/// Type 1a: compute 22:0 (0 for none) with a DM and a PM access. Compute
/// 22:16 = `0x3f` is the Type 1b marker, so the 48-bit reading refuses it.
fn decode_dual(word: u64) -> Option<Op> {
    let field = word & 0x7f_ffff;
    if field >> 16 == 0x3f {
        return None;
    }
    let compute = if field == 0 {
        None
    } else {
        Some(decode_compute(field)?)
    };
    let accesses = dual_accesses(word);
    distinct(compute, accesses).then_some(Op::Transfer { compute, accesses })
}

/// VISA decoder for the shim forms (PRM "b"/"c" figures). Other short forms
/// fall through to the 48-bit reading; their markers (checked for Types 1b,
/// 2b, 2c, 3b, 3c, 5b, 7b, 9b, 11c, 15b, 16b, 17b, 21c, 22c, 25c) make
/// [`decode`] refuse it. Returns the op and its length in parcels.
fn decode_visa(program: &ProgramMemory, pc: u32) -> Result<(Op, u32), MachineError> {
    let parcel = |n: u32| {
        let address = pc.wrapping_add(n);
        program
            .fetch_parcel(address)
            .ok_or(MachineError::NoCode(address))
    };
    let first = parcel(0)?;
    let short = match first {
        // Type 21c NOP, Type 25c RFRAME.
        0x0001 => Some(Op::Nop),
        0x1901 => Some(Op::Rframe),
        // Type 3c normal-word post-modify, PEx data registers.
        _ if first & 0xf010 == 0x9010 => Some(Op::Transfer {
            compute: None,
            accesses: [
                Some(Access {
                    data: (first & 15) as u8,
                    index: ((first >> 9) & 7) as usize,
                    modifier: ((first >> 6) & 7) as usize,
                    post: true,
                    store: first & (1 << 5) != 0,
                }),
                None,
            ],
        }),
        // Type 11c, unconditional, J=LR=0; X (bit 8) selects RTI.
        0x0abe | 0x0bbe => Some(Op::Return {
            interrupt: first & 0x100 != 0,
        }),
        _ => None,
    };
    if let Some(op) = short {
        return Ok((op, 1));
    }
    let word32 = u32::from(first) << 16 | u32::from(parcel(1)?);
    if let Some(op) = decode_visa32(word32) {
        return Ok((op, 2));
    }
    let word = u64::from(word32) << 16 | u64::from(parcel(2)?);
    decode(word)
        .map(|op| (op, 3))
        .ok_or(MachineError::UnsupportedWord { pc, word })
}

fn decode_visa32(word: u32) -> Option<Op> {
    let fields = u64::from(word);
    // Type 5b transfer: source split over 26:22, 16:15; COND=31.
    if word & 0xf83e_407f == 0x703e_003f {
        let source = (((word >> 22) & 31) << 2) | ((word >> 15) & 3);
        return Some(Op::Move {
            dst: ureg(((word >> 7) & 127) as u8, true)?,
            src: ureg(source as u8, false)?,
        });
    }
    // Type 15b normal-word universal-register transfer, L=0; G 21 is DAG2.
    if word & 0xf0df_c000 == 0x9008_0000 {
        let (store, dag2) = (word & (1 << 24) != 0, word & (1 << 21) != 0);
        return Some(Op::OffsetMemory {
            data: offset_ureg(((word >> 7) & 0x7f) as u8, store, dag2)?,
            index: 8 * usize::from(dag2) + ((word >> 25) & 7) as usize,
            offset: ((word << 25) as i32) >> 25,
            store,
        });
    }
    // Type 3b, COND=31, L=0, marker 01111, W=X=1 (normal word); G is free.
    if word & 0xe03e_407f == 0x403e_003f {
        let accesses = [Some(dm_transfer(fields)?), None];
        return Some(Op::Transfer {
            compute: None,
            accesses,
        });
    }
    // Type 1b: Type 1a's upper 32 bits, marker 0111111 (PRM figure 14.07.1).
    if word >> 29 == 1 && word & 0x7f == 0x3f {
        let accesses = dual_accesses(fields << 16);
        return distinct(None, accesses).then_some(Op::Transfer {
            compute: None,
            accesses,
        });
    }
    // Type 9b `JUMP (Md, Ic)`, REL=B=A=CI=0, J = (DB). Fields sit 16 bits
    // below Type 9a, so COND is read through the 48-bit position.
    if word & 0xffc0_03ff == 0x0800_003f {
        return Some(Op::Branch {
            call: false,
            delayed: word & (1 << 10) != 0,
            condition: condition_code(fields << 16)?,
            target: Target::Indirect {
                index: 8 + ((word >> 14) & 7) as usize,
                modifier: 8 + ((word >> 11) & 7) as usize,
            },
        });
    }
    None
}

/// Compute field (PRM 21.x). Bit 22 clear: single function, CU and opcode
/// 21:12, Rn 11:8, Rx 7:4, Ry 3:0. Bit 22 set: float multifunction.
fn decode_compute(field: u64) -> Option<Compute> {
    if field & (1 << 22) != 0 {
        return decode_multifunction(field);
    }
    let dst = ((field >> 8) & 15) as usize;
    let lhs = ((field >> 4) & 15) as usize;
    let rhs = (field & 15) as usize;
    if (field >> 12) & 0x3ff == 0x200 {
        return Some(Compute::ShiftBy {
            dst,
            src: lhs,
            amount: rhs,
        });
    }
    let op = match ((field >> 12) & 0x3ff, dst, rhs) {
        (0x130, ..) => ComputeOp::FloatMultiply,
        (0x081, ..) => ComputeOp::FloatAdd,
        (0x082, ..) => ComputeOp::FloatSubtract,
        (0x0b0, _, 0) => ComputeOp::FloatAbs,
        (0x0a1, _, 0) => ComputeOp::FloatPass,
        (0x0a2, _, 0) => ComputeOp::FloatNegate,
        (0x0e1, ..) => ComputeOp::FloatMin,
        (0x0e2, ..) => ComputeOp::FloatMax,
        (0x0e3, ..) => ComputeOp::FloatClip,
        (0x0e0, ..) => ComputeOp::FloatCopySign,
        (0x08a, 0, _) => ComputeOp::FloatCompare,
        (0x021, _, 0) => ComputeOp::IntegerPass,
        (0x02a, _, 0) => ComputeOp::IntegerDecrement,
        (0x002, ..) => ComputeOp::IntegerSubtract,
        (0x001, ..) => ComputeOp::IntegerAdd,
        (0x029, _, 0) => ComputeOp::IntegerIncrement,
        (0x00a, 0, _) => ComputeOp::IntegerCompare,
        (0x00b, 0, _) => ComputeOp::IntegerCompareUnsigned,
        (0x040, ..) => ComputeOp::And,
        (0x041, ..) => ComputeOp::Or,
        (0x042, ..) => ComputeOp::Xor,
        (0x0ca, _, 0) => ComputeOp::FloatFromInteger,
        (0x0c9, _, 0) => ComputeOp::FixToInteger,
        _ => return None,
    };
    Some(Compute::Alu { op, dst, lhs, rhs })
}

/// Float multifunction (PRM 21.15, 21.16): Fm 15:12, Fa 11:8, Fxm 7:6
/// (F0-F3), Fym 5:4 (F4-F7), Fxa 3:2 (F8-F11), Fya 1:0 (F12-F15). Opcode
/// 21:16 `011000` adds, `011001` subtracts; 21:20 = `11` is the dual
/// add/subtract with Fs at 19:16. Other multifunction forms are refused.
fn decode_multifunction(field: u64) -> Option<Compute> {
    let register = |shift: u32, mask: u64| ((field >> shift) & mask) as usize;
    let fa = register(8, 15);
    let (sum, difference) = match (field >> 16) & 0x3f {
        0b01_1000 => (Some(fa), None),
        0b01_1001 => (None, Some(fa)),
        0b11_0000..=0b11_1111 => (Some(fa), Some(register(16, 15))),
        _ => return None,
    };
    let compute = Compute::Multi {
        product: register(12, 15),
        xm: register(6, 3),
        ym: 4 + register(4, 3),
        xa: 8 + register(2, 3),
        ya: 12 + register(0, 3),
        sum,
        difference,
    };
    distinct(Some(compute), [None, None]).then_some(compute)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPE1A: u64 = 0b001 << 45;
    /// `F0 = F0 * F4` (CU and opcode 21:12, Rn 11:8, Rx 7:4, Ry 3:0).
    const MULTIPLY_F0: u64 = 0x130 << 12 | 4;

    /// The PRM leaves a double write undefined, so a code generator bug that loads and computes
    /// into one register, or loads one register twice, is refused rather than guessed.
    #[test]
    fn decoders_refuse_two_writes_to_one_register() {
        // R0 = DM(I0, M0), R0 = PM(I8, M8).
        assert_eq!(decode(TYPE1A), None);
        // F0 = F0 * F4, R0 = DM(I0, M0), R1 = PM(I8, M8).
        assert_eq!(decode(TYPE1A | 1 << 23 | MULTIPLY_F0), None);
        // F0 = F0 * F4, R1 = DM(I0, M0), R2 = PM(I8, M8) is fine.
        assert!(decode(TYPE1A | 1 << 33 | 2 << 23 | MULTIPLY_F0).is_some());
        // F2 = F0 * F4, F2 = F8 + F12: Fm = Fa.
        assert_eq!(
            decode_compute(1 << 22 | 0b01_1000 << 16 | 2 << 12 | 2 << 8),
            None
        );
        // Type 3a F0 = F0 * F4, R0 = DM(I0, M0) (U = 1, COND = 31).
        assert_eq!(decode(0b010 << 45 | 1 << 44 | 31 << 33 | MULTIPLY_F0), None);
    }

    /// Type 1b is Type 1a's upper half with compute 22:16 = 0x3f (PRM figure 14.07.1): the
    /// 48-bit reading refuses that marker and the 32-bit reading is the bare dual move.
    #[test]
    fn type1b_marker_is_not_a_compute() {
        // R0 = DM(I0, M0), R1 = PM(I8, M8).
        let type1b: u32 = 0b001 << 29 | 1 << 7 | 0x3f;
        assert_eq!(decode(u64::from(type1b) << 16), None);
        let access = |data, index, modifier| {
            Some(Access {
                data,
                index,
                modifier,
                post: true,
                store: false,
            })
        };
        assert_eq!(
            decode_visa32(type1b),
            Some(Op::Transfer {
                compute: None,
                accesses: [access(0, 0, 0), access(1, 8, 8)],
            })
        );
    }
}
