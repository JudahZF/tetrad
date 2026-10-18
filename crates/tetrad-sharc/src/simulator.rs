//! Bounded mathematical simulator for a verified scalar, normal-word subset.
//! Decodes raw words, not disassembler text. The default entries use host f32, which
//! is not a SHARC numeric model: no 40-bit intermediates or denormal flush.
//! ABS/MAX/CLIP/COPYSIGN return all ones for a NaN input (manual); other float
//! NaN/Inf/subnormal values follow host f32. [`run_scalar_normal_floats`] refuses
//! those values instead. [`run_scalar_prm`] opts in to the PRM float rules.
use std::cmp::Ordering;

use crate::address::{AddressError, DmWordAddress32, PmWordAddress24};
use crate::alu::{ComputeOp, SIGN40, compare_prm, evaluate, evaluate_prm};
use crate::emit::OwnedCode;
use crate::gain::{GAIN_ABI, MAX_GAIN_SAMPLES};
use crate::isa48::{MODE1_RND32, MODE1_SCALAR_CLEAR};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SimulationError {
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error("gain sample count {0} exceeds 32")]
    Count(u32),
    #[error("gain must be finite")]
    NonFiniteGain,
    #[error("nonfinite floating-point result at PC {pc:#x}")]
    NonFiniteResult { pc: u32 },
    #[error("subnormal or nonfinite floating-point operand or result at PC {pc:#x}")]
    NonNormalFloat { pc: u32 },
    #[error("DM range starting at {address:#x}, length {words}, is outside the supplied memory")]
    MemoryBounds { address: u32, words: u32 },
    #[error("partial input/output overlap is unsupported")]
    PartialOverlap,
    #[error("scalar gain modes are not established (MODE1={0:#x})")]
    Modes(u32),
    #[error("DAG1 L{index} must be zero; got {length}")]
    CircularLength { index: usize, length: u32 },
    #[error("unsupported scalar simulation word {word:#014x} at PC {pc:#x}")]
    UnsupportedWord { pc: u32, word: u64 },
    #[error("PC {0} left the code without RTS")]
    MissingRts(i64),
    #[error("step budget {0} exhausted before RTS")]
    StepBudget(usize),
    #[error("normal-word index arithmetic overflow")]
    IndexOverflow,
    #[error("memory contains too many normal words")]
    MemorySize,
    #[error("unmodeled instruction semantics {feature:?} at PC {pc:#x}")]
    Unmodeled {
        pc: u32,
        feature: UnmodeledSemantics,
    },
    #[error("PC stack overflow at PC {0:#x} (capacity 30)")]
    PcStackOverflow(u32),
    #[error("RTS on an empty PC stack at PC {0:#x}")]
    PcStackUnderflow(u32),
    #[error("execution stopped with {0} outstanding CALL entries")]
    PcStackNotEmpty(usize),
    #[error("target PC {0} is outside linked code")]
    CodeTarget(i64),
}

/// Encoding support does not imply independently verified execution semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmodeledSemantics {
    InterruptReturn,
    StackMaintenance,
    ConflictCache,
    SystemRegisters,
    TestFlag,
}

pub const PC_STACK_CAPACITY: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleCount(u32);

impl SampleCount {
    pub const fn new(count: u32) -> Result<Self, SimulationError> {
        if count <= MAX_GAIN_SAMPLES {
            Ok(Self(count))
        } else {
            Err(SimulationError::Count(count))
        }
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainInvocation {
    input: DmWordAddress32,
    output: DmWordAddress32,
    count: SampleCount,
    gain: f32,
}

impl GainInvocation {
    pub fn new(
        input: DmWordAddress32,
        output: DmWordAddress32,
        count: SampleCount,
        gain: f32,
    ) -> Result<Self, SimulationError> {
        if !gain.is_finite() {
            return Err(SimulationError::NonFiniteGain);
        }
        Ok(Self {
            input,
            output,
            count,
            gain,
        })
    }
    pub fn input(self) -> DmWordAddress32 {
        self.input
    }
    pub fn output(self) -> DmWordAddress32 {
        self.output
    }
    pub fn count(self) -> SampleCount {
        self.count
    }
    pub fn gain(self) -> f32 {
        self.gain
    }

    pub fn validate(self, memory: &DmMemory) -> Result<(), SimulationError> {
        let count = self.count.get();
        memory.validate_range(self.input, count)?;
        memory.validate_range(self.output, count)?;
        // Require representable one-past indices, even at the top of DM.
        let input_end = self.input.checked_add(count)?;
        let output_end = self.output.checked_add(count)?;
        if self.input != self.output && self.input < output_end && self.output < input_end {
            return Err(SimulationError::PartialOverlap);
        }
        Ok(())
    }
}

/// One caller-supplied normal-word DM region; no hardware or host-pointer aliasing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmMemory {
    base: DmWordAddress32,
    words: Vec<u32>,
    writes: Vec<DmWordAddress32>,
}

impl DmMemory {
    pub fn new(base: DmWordAddress32, words: Vec<u32>) -> Result<Self, SimulationError> {
        let size = u32::try_from(words.len()).map_err(|_| SimulationError::MemorySize)?;
        // u64 exclusive end permits a region containing address u32::MAX.
        if u64::from(base.get()) + u64::from(size) > u64::from(u32::MAX) + 1 {
            return Err(SimulationError::Address(AddressError::DmOverflow));
        }
        Ok(Self {
            base,
            words,
            writes: Vec::new(),
        })
    }
    pub fn base(&self) -> DmWordAddress32 {
        self.base
    }
    pub fn words(&self) -> &[u32] {
        &self.words
    }
    pub fn writes(&self) -> &[DmWordAddress32] {
        &self.writes
    }
    pub fn read(&self, address: DmWordAddress32) -> Result<u32, SimulationError> {
        self.validate_range(address, 1)?;
        Ok(self.words[(address.get() - self.base.get()) as usize])
    }
    pub fn write(&mut self, address: DmWordAddress32, value: u32) -> Result<(), SimulationError> {
        self.validate_range(address, 1)?;
        self.words[(address.get() - self.base.get()) as usize] = value;
        self.writes.push(address);
        Ok(())
    }
    pub fn validate_range(
        &self,
        address: DmWordAddress32,
        words: u32,
    ) -> Result<(), SimulationError> {
        let start = u64::from(address.get());
        let base = u64::from(self.base.get());
        let end = start + u64::from(words);
        if start < base || end > base + self.words.len() as u64 {
            Err(SimulationError::MemoryBounds {
                address: address.get(),
                words,
            })
        } else {
            Ok(())
        }
    }
}

/// Registers are raw 32-bit storage. F and R are views of the same data array.
/// [`run_scalar_prm`] keeps bits 7:0 of the 40-bit registers in a separate array.
/// Only ALU zero is modeled; AN, AS, AI, AV, AC and CACC are outside this model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarState {
    pub data: [u32; 16],
    pub indices: [u32; 8],
    pub modifiers: [i32; 8],
    pub bases: [u32; 8],
    pub lengths: [u32; 8],
    pub mode1: u32,
    pub alu_zero: bool,
}

impl Default for ScalarState {
    fn default() -> Self {
        Self {
            data: [0; 16],
            indices: [0; 8],
            modifiers: [0; 8],
            bases: [0; 8],
            lengths: [0; 8],
            mode1: MODE1_RND32,
            alu_zero: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimulationOutcome {
    pub steps: usize,
    pub stores: usize,
}

/// Checks caller ranges/modes before installing the leaf ABI, then decodes code.
/// An execution error can leave partial register/memory changes; there is no rollback.
pub fn simulate_gain(
    code: &OwnedCode,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    invocation: GainInvocation,
    step_budget: usize,
) -> Result<SimulationOutcome, SimulationError> {
    invocation.validate(memory)?;
    if state.mode1 & MODE1_RND32 == 0 || state.mode1 & MODE1_SCALAR_CLEAR != 0 {
        return Err(SimulationError::Modes(state.mode1));
    }
    for index in GAIN_ABI.zero_lengths {
        let index = usize::from(index.index());
        if state.lengths[index] != 0 {
            return Err(SimulationError::CircularLength {
                index,
                length: state.lengths[index],
            });
        }
    }
    state.indices[0] = invocation.input.get();
    state.indices[1] = invocation.output.get();
    state.data[4] = invocation.gain.to_bits();
    state.data[8] = invocation.count.get();
    run_scalar(code, state, memory, step_budget)
}

/// Per-entry float semantics. The checks inspect only the float view of compute
/// sources and results; `Prm` replaces host f32 with [`evaluate_prm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FloatPolicy {
    Host,
    FiniteResults,
    NormalOnly,
    Prm,
}

/// One Type 2a compute: `dst = operation(lhs, rhs)` on data registers.
#[derive(Debug, Clone, Copy)]
struct Compute {
    operation: ComputeOp,
    dst: usize,
    lhs: usize,
    rhs: usize,
}

#[derive(Debug)]
enum Decoded {
    Nop,
    Rts,
    Unmodeled(UnmodeledSemantics),
    DirectMemory {
        data: usize,
        address: u32,
        store: bool,
    },
    Immediate {
        register: u8,
        value: u32,
    },
    Compute(Compute),
    Memory {
        data: usize,
        index: usize,
        modifier: usize,
        post: bool,
        store: bool,
    },
    Branch {
        call: bool,
        condition: u8,
        relative: bool,
        target: u32,
    },
}

// Field extraction is independent of the emitter. The Type 3a modifier is 40:38,
// excluding U at 44; no existing decoder or its text output is consumed.
fn decode(word: u64) -> Option<Decoded> {
    match word {
        0 => return Some(Decoded::Nop),
        0x0a3e_0000_0000 => return Some(Decoded::Rts),
        0x0b3e_0000_0000 => return Some(Decoded::Unmodeled(UnmodeledSemantics::InterruptReturn)),
        _ => {}
    }
    let direct_fields = (1 << 40) | (15 << 32) | 0xffff_ffff;
    if word & !direct_fields == 0x1000_0000_0000 {
        return Some(Decoded::DirectMemory {
            data: ((word >> 32) & 15) as usize,
            address: word as u32,
            store: word & (1 << 40) != 0,
        });
    }
    let maintenance_fields = (1 << 38) | (1 << 36) | (1 << 34) | (1 << 33);
    if word & !maintenance_fields == 0x1700_0000_0000 && word & maintenance_fields != 0 {
        return Some(Decoded::Unmodeled(
            if word & ((1 << 38) | (1 << 36) | (1 << 34)) != 0 {
                UnmodeledSemantics::StackMaintenance
            } else {
                UnmodeledSemantics::ConflictCache
            },
        ));
    }
    let system_fields = (7 << 37) | (15 << 32) | 0xffff_ffff;
    if word & !system_fields == 0x1400_0000_0000 || word & 0xffff_0000_0000 == 0x0f73_0000_0000 {
        return Some(Decoded::Unmodeled(UnmodeledSemantics::SystemRegisters));
    }
    if word & 0xff80_0000_0000 == 0x0f00_0000_0000 {
        let register = ((word >> 32) & 0x7f) as u8;
        if register <= 15 || matches!(register >> 4, 1..=4) && register & 0xf <= 7 {
            return Some(Decoded::Immediate {
                register,
                value: word as u32,
            });
        }
    }
    // Type 2a, COND=31, MF=0. Unused RN/RY fields must be zero.
    if word & !0x003f_ffff == 0x013e_0000_0000 {
        let dst = ((word >> 8) & 15) as usize;
        let lhs = ((word >> 4) & 15) as usize;
        let rhs = (word & 15) as usize;
        let operation = match ((word >> 12) & 0x3ff, dst, rhs) {
            (0x130, ..) => Some(ComputeOp::FloatMultiply),
            (0x081, ..) => Some(ComputeOp::FloatAdd),
            (0x082, ..) => Some(ComputeOp::FloatSubtract),
            (0x0b0, _, 0) => Some(ComputeOp::FloatAbs),
            (0x0e2, ..) => Some(ComputeOp::FloatMax),
            (0x0e3, ..) => Some(ComputeOp::FloatClip),
            (0x0e0, ..) => Some(ComputeOp::FloatCopySign),
            (0x08a, 0, _) => Some(ComputeOp::FloatCompare),
            (0x021, _, 0) => Some(ComputeOp::IntegerPass),
            (0x02a, _, 0) => Some(ComputeOp::IntegerDecrement),
            (0x002, ..) => Some(ComputeOp::IntegerSubtract),
            (0x0ca, _, 0) => Some(ComputeOp::FloatFromInteger),
            (0x0c9, _, 0) => Some(ComputeOp::FixToInteger),
            _ => None,
        };
        if let Some(operation) = operation {
            return Some(Decoded::Compute(Compute {
                operation,
                dst,
                lhs,
                rhs,
            }));
        }
    }
    let memory_fields = (1 << 44) | (7 << 41) | (7 << 38) | (1 << 31) | (15 << 23);
    if word & !memory_fields == 0x403e_0000_0000 {
        return Some(Decoded::Memory {
            data: ((word >> 23) & 15) as usize,
            index: ((word >> 41) & 7) as usize,
            modifier: ((word >> 38) & 7) as usize,
            post: word & (1 << 44) != 0,
            store: word & (1 << 31) != 0,
        });
    }
    let branch_fields = (1 << 40) | (1 << 39) | (31 << 33) | 0xff_ffff;
    if word & !branch_fields == 0x0600_0000_0000 {
        let condition = ((word >> 33) & 31) as u8;
        if matches!(condition, 13 | 29) {
            return Some(Decoded::Unmodeled(UnmodeledSemantics::TestFlag));
        }
        if matches!(condition, 0 | 16 | 31) {
            return Some(Decoded::Branch {
                call: word & (1 << 39) != 0,
                condition,
                relative: word & (1 << 40) != 0,
                target: word as u32 & 0xff_ffff,
            });
        }
    }
    None
}

/// Execute a leaf invocation with a host return on RTS at depth zero.
/// Nested direct CALL/RTS uses a bounded architectural PC stack. No interrupts,
/// status/loop stacks, cache effects, effect latency or timing are modeled.
pub fn run_scalar(
    code: &OwnedCode,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    step_budget: usize,
) -> Result<SimulationOutcome, SimulationError> {
    execute(
        code,
        state,
        &mut [0; 16],
        memory,
        step_budget,
        None,
        FloatPolicy::Host,
    )
}

// Same raw decoder and execution path, with per-operation finite-result checks.
// The caller owns staging/rollback; this internal entry itself is not atomic.
pub(crate) fn run_scalar_checked_floats(
    code: &OwnedCode,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    step_budget: usize,
) -> Result<SimulationOutcome, SimulationError> {
    execute(
        code,
        state,
        &mut [0; 16],
        memory,
        step_budget,
        None,
        FloatPolicy::FiniteResults,
    )
}

/// [`run_scalar`] for kernels whose contract is normal-range host f32. Every float
/// source and float result of a compute must be zero or normal; otherwise
/// [`SimulationError::NonNormalFloat`] is returned before that compute writes its
/// register or ALU zero. Loads, moves and integer computes are not checked. Earlier
/// effects remain: clone state and memory, and commit only on success.
pub fn run_scalar_normal_floats(
    code: &OwnedCode,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    step_budget: usize,
) -> Result<SimulationOutcome, SimulationError> {
    execute(
        code,
        state,
        &mut [0; 16],
        memory,
        step_budget,
        None,
        FloatPolicy::NormalOnly,
    )
}

/// [`run_scalar`] under the PRM float rules, on 40-bit data registers:
/// `state.data[n]` is bits 39:8 of register n and `extension[n]` bits 7:0.
/// - MODE1 RND32 set rounds float results to 24 significand bits; clear keeps the
///   reset default, 40-bit extended format (32 bits). FLOAT always keeps 40 bits.
/// - Denormal operands and post-rounded denormal results become signed zero.
/// - A NaN operand or an invalid operation returns all ones; overflow returns
///   ±infinity.
/// - Loads, immediates and fixed-point results clear the extension; stores write
///   bits 39:8 (PRM 4.06).
///
/// Earlier effects remain on error, as in [`run_scalar`].
pub fn run_scalar_prm(
    code: &OwnedCode,
    state: &mut ScalarState,
    extension: &mut [u8; 16],
    memory: &mut DmMemory,
    step_budget: usize,
) -> Result<SimulationOutcome, SimulationError> {
    execute(
        code,
        state,
        extension,
        memory,
        step_budget,
        None,
        FloatPolicy::Prm,
    )
}

/// Run from origin with an initially empty, fixed 30-entry PC stack. Stop before
/// the checked linked stop instruction, only at depth zero. Empty RTS is an error.
/// This is an offline control-flow harness, not a boot or warm-entry model.
pub fn run_linked(
    code: &OwnedCode,
    state: &mut ScalarState,
    memory: &mut DmMemory,
    stop: PmWordAddress24,
    step_budget: usize,
) -> Result<SimulationOutcome, SimulationError> {
    check_target(code, i64::from(stop.get()))?;
    execute(
        code,
        state,
        &mut [0; 16],
        memory,
        step_budget,
        Some(stop.get()),
        FloatPolicy::Host,
    )
}

fn check_target(code: &OwnedCode, target: i64) -> Result<(), SimulationError> {
    let offset = target - i64::from(code.origin().get());
    if offset < 0 || offset as u64 >= code.word_count() as u64 {
        Err(SimulationError::CodeTarget(target))
    } else {
        Ok(())
    }
}

fn execute(
    code: &OwnedCode,
    state: &mut ScalarState,
    extension: &mut [u8; 16],
    memory: &mut DmMemory,
    step_budget: usize,
    stop: Option<u32>,
    policy: FloatPolicy,
) -> Result<SimulationOutcome, SimulationError> {
    // Host f32 needs RND32; the PRM model also runs the 40-bit default.
    let rnd32_required = policy != FloatPolicy::Prm;
    if (rnd32_required && state.mode1 & MODE1_RND32 == 0) || state.mode1 & MODE1_SCALAR_CLEAR != 0 {
        return Err(SimulationError::Modes(state.mode1));
    }
    let origin = i64::from(code.origin().get());
    let mut pc = origin;
    let initial_writes = memory.writes.len();
    let mut pc_stack = [0i64; PC_STACK_CAPACITY];
    let mut depth = 0;
    for step in 0..=step_budget {
        if stop.map(i64::from) == Some(pc) {
            if depth != 0 {
                return Err(SimulationError::PcStackNotEmpty(depth));
            }
            return Ok(SimulationOutcome {
                steps: step,
                stores: memory.writes.len() - initial_writes,
            });
        }
        if step == step_budget {
            return Err(SimulationError::StepBudget(step_budget));
        }
        let offset = usize::try_from(pc - origin).map_err(|_| SimulationError::MissingRts(pc))?;
        let word = code
            .words()
            .get(offset)
            .ok_or(SimulationError::MissingRts(pc))?
            .bits();
        let instruction = decode(word).ok_or(SimulationError::UnsupportedWord {
            pc: pc as u32,
            word,
        })?;
        let mut next_pc = pc + 1;
        match instruction {
            Decoded::Nop => {}
            Decoded::Unmodeled(feature) => {
                return Err(SimulationError::Unmodeled {
                    pc: pc as u32,
                    feature,
                });
            }
            Decoded::Rts => {
                if depth == 0 {
                    if stop.is_some() {
                        return Err(SimulationError::PcStackUnderflow(pc as u32));
                    }
                    return Ok(SimulationOutcome {
                        steps: step + 1,
                        stores: memory.writes.len() - initial_writes,
                    });
                }
                depth -= 1;
                next_pc = pc_stack[depth];
                check_target(code, next_pc)?;
            }
            Decoded::DirectMemory {
                data,
                address,
                store,
            } => {
                let address = DmWordAddress32::new(address);
                if store {
                    memory.write(address, state.data[data])?;
                } else {
                    state.data[data] = memory.read(address)?;
                    extension[data] = 0;
                }
            }
            Decoded::Immediate { register, value } => {
                let index = usize::from(register & 15);
                match register >> 4 {
                    0 => {
                        state.data[index] = value;
                        extension[index] = 0;
                    }
                    1 => state.indices[index] = value,
                    2 => state.modifiers[index] = value as i32,
                    3 => state.lengths[index] = value,
                    4 => {
                        state.bases[index] = value;
                        state.indices[index] = value;
                    }
                    _ => unreachable!("decoder restricts universal register classes"),
                }
            }
            Decoded::Compute(instruction) => {
                compute(state, extension, instruction, policy, pc as u32)?
            }
            Decoded::Memory {
                data,
                index,
                modifier,
                post,
                store,
            } => {
                if state.lengths[index] != 0 {
                    return Err(SimulationError::CircularLength {
                        index,
                        length: state.lengths[index],
                    });
                }
                let original = state.indices[index];
                let updated = original
                    .checked_add_signed(state.modifiers[modifier])
                    .ok_or(SimulationError::IndexOverflow)?;
                let address = DmWordAddress32::new(if post { original } else { updated });
                if store {
                    memory.write(address, state.data[data])?;
                } else {
                    state.data[data] = memory.read(address)?;
                    extension[data] = 0;
                }
                if post {
                    state.indices[index] = updated;
                }
            }
            Decoded::Branch {
                call,
                condition,
                relative,
                target,
            } => {
                let taken = match condition {
                    0 => state.alu_zero,
                    16 => !state.alu_zero,
                    31 => true,
                    _ => unreachable!("decoder restricts branch conditions"),
                };
                if taken {
                    let target_pc = if relative {
                        // Sign-extend the raw 24-bit word displacement, relative to branch PC.
                        let displacement = ((target << 8) as i32) >> 8;
                        pc + i64::from(displacement)
                    } else {
                        i64::from(target)
                    };
                    check_target(code, target_pc)?;
                    if call {
                        if depth == PC_STACK_CAPACITY {
                            return Err(SimulationError::PcStackOverflow(pc as u32));
                        }
                        // The non-delayed architectural return address is PC+1.
                        check_target(code, pc + 1)?;
                        pc_stack[depth] = pc + 1;
                        depth += 1;
                    }
                    next_pc = target_pc;
                }
            }
        }
        pc = next_pc;
    }
    Err(SimulationError::StepBudget(step_budget))
}

/// One Type 2a compute. Policy checks run before the register or ALU zero changes.
fn compute(
    state: &mut ScalarState,
    extension: &mut [u8; 16],
    Compute {
        operation,
        dst,
        lhs,
        rhs,
    }: Compute,
    policy: FloatPolicy,
    pc: u32,
) -> Result<(), SimulationError> {
    if policy == FloatPolicy::Prm {
        // Read both 40-bit sources first: RN may alias RX or RY.
        let register = |n: usize| (u64::from(state.data[n]) << 8) | u64::from(extension[n]);
        let (x, y) = (register(lhs), register(rhs));
        let rnd32 = state.mode1 & MODE1_RND32 != 0;
        let result = evaluate_prm(operation, x, y, rnd32);
        // PRM 23.02: AZ also marks a post-rounded denormal, which is now ±zero.
        state.alu_zero = match operation {
            ComputeOp::FloatMultiply => state.alu_zero,
            ComputeOp::FloatCompare => compare_prm(x, y, rnd32) == Some(Ordering::Equal),
            _ if operation.float_result() => result.is_some_and(|bits| bits & !SIGN40 == 0),
            _ => result.is_some_and(|bits| bits >> 8 == 0),
        };
        if let Some(bits) = result {
            state.data[dst] = (bits >> 8) as u32;
            extension[dst] = bits as u8;
        }
        return Ok(());
    }
    // Read both sources first: RN may alias RX or RY.
    let (x, y) = (state.data[lhs], state.data[rhs]);
    let (fx, fy) = (f32::from_bits(x), f32::from_bits(y));
    let result = evaluate(operation, x, y);
    let float_result = result.filter(|_| operation.float_result());
    match policy {
        FloatPolicy::Host | FloatPolicy::Prm => {}
        FloatPolicy::FiniteResults => {
            if float_result.is_some_and(|bits| !f32::from_bits(bits).is_finite()) {
                return Err(SimulationError::NonFiniteResult { pc });
            }
        }
        FloatPolicy::NormalOnly => {
            let mut checked = [x, y]
                .into_iter()
                .take(operation.float_sources())
                .chain(float_result)
                .map(f32::from_bits);
            if checked.any(|value| value != 0.0 && !value.is_normal()) {
                return Err(SimulationError::NonNormalFloat { pc });
            }
        }
    }
    // AZ: compare equality, float ±0 or fixed-point all zeros. MUL has no ALU flag.
    state.alu_zero = match operation {
        ComputeOp::FloatMultiply => state.alu_zero,
        ComputeOp::FloatCompare => fx == fy,
        _ => match float_result {
            Some(bits) => f32::from_bits(bits) == 0.0,
            None => result == Some(0),
        },
    };
    if let Some(bits) = result {
        state.data[dst] = bits;
        extension[dst] = 0;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Compute, ComputeOp, Decoded, FloatPolicy, ScalarState, UnmodeledSemantics, compute, decode,
    };

    // Manual-derived Type 2a words (RN=3, RX=5, RY=9 unless noted), written as raw
    // literals so these checks do not depend on the encoder.
    const ABS_F3_F9: u64 = 0x013e000b0390;
    const MAX_F3: u64 = 0x013e000e2359;
    const CLIP_F3: u64 = 0x013e000e3359;
    const COPYSIGN_F3: u64 = 0x013e000e0359;
    const COMP_F5_F9: u64 = 0x013e0008a059;
    const SUB_R3: u64 = 0x013e00002359;

    /// Decode a raw word and run it under the host policy with R0 = R3 = sentinel.
    fn run_raw(word: u64, x: u32, y: u32) -> ScalarState {
        let Some(Decoded::Compute(instruction)) = decode(word) else {
            panic!("{word:#014x} is not a supported compute");
        };
        let mut state = ScalarState::default();
        state.data[0] = 0xdead_beef;
        state.data[3] = 0xdead_beef;
        state.data[5] = x;
        state.data[9] = y;
        state.alu_zero = !state.alu_zero;
        compute(&mut state, &mut [0; 16], instruction, FloatPolicy::Host, 0).unwrap();
        state
    }

    #[test]
    fn raw_neural_alu_decoder_requires_canonical_type2a_fields() {
        for (word, operation) in [
            (ABS_F3_F9, ComputeOp::FloatAbs),
            (MAX_F3, ComputeOp::FloatMax),
            (CLIP_F3, ComputeOp::FloatClip),
            (COPYSIGN_F3, ComputeOp::FloatCopySign),
            (COMP_F5_F9, ComputeOp::FloatCompare),
            (SUB_R3, ComputeOp::IntegerSubtract),
        ] {
            assert!(
                matches!(decode(word), Some(Decoded::Compute(Compute { operation: o, .. })) if o == operation),
                "{word:#014x}"
            );
        }
        for word in [
            0x013e0008a159, // comp with RN=1
            0x013e000b0391, // abs with RY=1
            0x013e004e2359, // MF=1 (multifunction)
            0x013e008e2359, // bit 23 outside the short compute field
            0x013e001e2359, // CU=1 with the ALU max opcode
            0x0120000e2359, // IF NE max: conditional compute
            0x010000082359, // IF EQ sub (selas-verified, still unsupported)
            0x013e000c4390, // recips: seed ROM is unknown
        ] {
            assert!(decode(word).is_none(), "{word:#014x}");
        }
    }

    #[test]
    fn raw_neural_alu_words_follow_manual_semantics() {
        const N0: u32 = 0x8000_0000;
        const ONE: u32 = 0x3f80_0000;
        const TWO: u32 = 0x4000_0000;
        const THREE: u32 = 0x4040_0000;
        const ONE_HALF: u32 = 0x3fc0_0000;
        const EPS: u32 = 0x38d1_b717;
        const NAN: u32 = 0x7fc0_0000;
        let neg = |bits: u32| bits | N0;
        // (word, RX/F5, RY/F9, R3 result, AZ). ABS reads its source from F9.
        for (word, x, y, result, zero) in [
            (ABS_F3_F9, 0, N0, 0, true),
            (ABS_F3_F9, 0, neg(THREE), THREE, false),
            (ABS_F3_F9, 0, NAN, u32::MAX, false),
            (MAX_F3, 0, N0, 0, true),
            (MAX_F3, N0, 0, 0, true),
            (MAX_F3, N0, N0, N0, true),
            (MAX_F3, neg(ONE), neg(TWO), neg(ONE), false),
            (MAX_F3, EPS - 1, EPS, EPS, false),
            (MAX_F3, ONE, NAN, u32::MAX, false),
            (CLIP_F3, THREE, TWO, TWO, false),
            (CLIP_F3, neg(THREE), TWO, neg(TWO), false),
            (CLIP_F3, neg(THREE), neg(TWO), neg(TWO), false),
            (CLIP_F3, TWO, neg(TWO), TWO, false),
            (CLIP_F3, neg(TWO), TWO, neg(TWO), false),
            (CLIP_F3, ONE_HALF, TWO, ONE_HALF, false),
            (CLIP_F3, N0, 0, N0, true),
            (CLIP_F3, NAN, TWO, u32::MAX, false),
            (COPYSIGN_F3, ONE, N0, neg(ONE), false),
            (COPYSIGN_F3, neg(TWO), 0, TWO, false),
            (COPYSIGN_F3, 0, neg(ONE), N0, true),
            (SUB_R3, 0, 1, u32::MAX, false),
            (SUB_R3, N0, 1, 0x7fff_ffff, false),
            (SUB_R3, 5, 5, 0, true),
            (SUB_R3, 0x7ef3_11c7, EPS, 0x4621_5ab0, false),
            (SUB_R3, u32::MAX, NAN, 0x803f_ffff, false),
        ] {
            let state = run_raw(word, x, y);
            assert_eq!(
                (state.data[3], state.alu_zero),
                (result, zero),
                "{word:#014x} {x:#x} {y:#x}"
            );
        }
        for (x, y, zero) in [(0, N0, true), (EPS, EPS + 1, false), (EPS, EPS, true)] {
            let state = run_raw(COMP_F5_F9, x, y);
            assert_eq!(state.alu_zero, zero, "{x:#x} {y:#x}");
            assert_eq!((state.data[0], state.data[3]), (0xdead_beef, 0xdead_beef));
        }
        // RN aliasing a source: every source is read before the write.
        for (word, x, y, dst, result) in [
            (0x013e000b0990, 0, neg(THREE), 9, THREE), // f9 = abs f9
            (0x013e000e2559, neg(ONE), TWO, 5, TWO),   // f5 = max(f5, f9)
            (0x013e000e3959, THREE, neg(TWO), 9, TWO), // f9 = clip f5 by f9
            (0x013e00002555, 7, 0, 5, 0),              // r5 = r5 - r5
        ] {
            assert_eq!(run_raw(word, x, y).data[dst], result, "{word:#014x}");
        }
    }

    #[test]
    fn raw_decoder_distinguishes_rti_and_defers_wrong_system_selectors() {
        assert!(matches!(decode(0x0a3e00000000), Some(Decoded::Rts)));
        assert!(matches!(
            decode(0x0b3e00000000),
            Some(Decoded::Unmodeled(UnmodeledSemantics::InterruptReturn))
        ));
        assert!(decode(0x0abe00000000).is_none()); // Oracle's wrong X=39.
        assert!(matches!(
            decode(0x0f7300200000),
            Some(Decoded::Unmodeled(UnmodeledSemantics::SystemRegisters))
        ));
        // Invented direct system writes stay outside the decoder's execution
        // boundary, including the oracle's incorrect legacy MMASK selector.
        for word in [
            0x0f7c00200000,
            0x0f7200010000,
            0x0f7400000010,
            0x0f7b00000000,
            0x0f7600000001,
            0x0f7800000000,
            0x0f6400123456,
            0x0f6500000000,
        ] {
            assert!(decode(word).is_none(), "{word:#014x}");
        }
    }

    #[test]
    fn raw_direct_dm_decoder_rejects_pm_lw_and_system_ureg_fields() {
        assert!(matches!(
            decode(0x100712345678),
            Some(Decoded::DirectMemory {
                data: 7,
                address: 0x12345678,
                store: false,
            })
        ));
        assert!(matches!(
            decode(0x110712345678),
            Some(Decoded::DirectMemory {
                data: 7,
                address: 0x12345678,
                store: true,
            })
        ));
        for word in [
            0x120712345678,
            0x108712345678,
            0x101012345678,
            0x107312345678,
            0x0b3e04000000,
            0x0a3e01000000,
            0x170080000000,
            0x170020000000,
            0x170018000000,
        ] {
            assert!(decode(word).is_none(), "{word:#014x}");
        }
    }
}
