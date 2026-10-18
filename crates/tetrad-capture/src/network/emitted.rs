//! Owned scalar ISA48 leaf `adaa_clip_block32`: one ADAA clip neuron over 32 raw
//! preactivations.
//!
//! The quotient uses an OWNED reciprocal: integer seed `0x7EF311C7 - bits(m)` and
//! exactly seven explicit Newton steps `t=m*r; u=2-t; r=u*r`. This is a mathematical
//! substitution for stock `RECIPS` (seed ROM unknown), so results are NOT stock bit
//! parity. [`OwnedAdaaClip`] is the host model of the same steps; the existing
//! `reference::AdaaClip` (Rust division) is separate and unchanged.
//!
//! Execution uses the raw decoder under `run_scalar_normal_floats`: any subnormal or
//! nonfinite float operand or result is refused, never flushed. Host f32 stands in
//! for SHARC arithmetic (no 40-bit intermediates, rounding-mode or timing parity).
#![forbid(unsafe_code)]

use crate::reference::ADAA_EPSILON;
use tetrad_sharc::{
    address::{AddressError, DmWordAddress32, PmWordAddress24},
    emit::{BranchRelocation, EmitError, IsaProgram, Label, OwnedCode},
    isa48::{BranchKind, Condition, DmAccess, Instruction48, MODE1_RND32, MODE1_SCALAR_CLEAR},
    register::{Dag1Index, Dag1Length, Dag1Modify, DataReg, FloatReg, UniversalReg},
    simulator::{DmMemory, ScalarState, SimulationError, run_scalar_normal_floats},
};

pub const SAMPLES: u32 = 32;
/// Owned domain bound on `|preactivation|` (§4.4, P). Not a proof that every
/// intermediate stays normal; the strict runner checks each operation.
pub const PREACTIVATION_LIMIT: f32 = f32::from_bits(0x5f80_0000); // 2^64
const RECIPROCAL_MAGIC: u32 = 0x7ef3_11c7;
const NEWTON_STEPS: usize = 7;
/// Recovered A(x) cubic coefficient (Type 17a immediate bits, contracts §1).
const CUBIC: f32 = f32::from_bits(0xbdaa_aaab);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Address(#[from] AddressError),
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Simulation(#[from] SimulationError),
    #[error("ADAA input, output and state spans {first} and {second} overlap")]
    Overlap { first: usize, second: usize },
    #[error("preactivation {0:#010x} is nonfinite, subnormal or above 2^64")]
    Domain(u32),
    #[error("DM word {address:#x} = {bits:#010x} is outside the preactivation domain")]
    DomainAt { address: u32, bits: u32 },
    #[error("host model refused a subnormal or nonfinite intermediate (sample {sample:?})")]
    NonNormal { sample: Option<u32> },
    #[error("executed steps or store log differ from the compiled leaf")]
    Receipt,
}

/// A finite raw preactivation that is ±0 or normal with `|x| <= 2^64`.
/// Sign and bits (including -0) are preserved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preactivation(f32);

impl Preactivation {
    pub fn new(value: f32) -> Result<Self, Error> {
        if (value == 0.0 || value.is_normal()) && value.abs() <= PREACTIVATION_LIMIT {
            Ok(Self(value))
        } else {
            Err(Error::Domain(value.to_bits()))
        }
    }
    pub fn get(self) -> f32 {
        self.0
    }
}

/// Logical normal-word DM spans: input32, output32, state1 (`prev_raw`). Input and
/// output may be the exact same span; any other overlap is refused. Every one-past
/// endpoint fits u32. No physical address or alias map is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    input: DmWordAddress32,
    output: DmWordAddress32,
    state: DmWordAddress32,
}

impl Layout {
    pub fn new(
        input: DmWordAddress32,
        output: DmWordAddress32,
        state: DmWordAddress32,
    ) -> Result<Self, Error> {
        let layout = Self {
            input,
            output,
            state,
        };
        let spans = layout.spans();
        for (first, &(start, words)) in spans.iter().enumerate() {
            let end = start.checked_add(words)?;
            for (second, &(other, other_words)) in spans.iter().enumerate().skip(first + 1) {
                let exact_alias = (first, second) == (0, 1) && start == other;
                if !exact_alias && start < other.checked_add(other_words)? && other < end {
                    return Err(Error::Overlap { first, second });
                }
            }
        }
        Ok(layout)
    }
    pub fn input(self) -> DmWordAddress32 {
        self.input
    }
    pub fn output(self) -> DmWordAddress32 {
        self.output
    }
    pub fn state(self) -> DmWordAddress32 {
        self.state
    }
    /// Order: input32, output32, state1.
    pub fn spans(self) -> [(DmWordAddress32, u32); 3] {
        [
            (self.input, SAMPLES),
            (self.output, SAMPLES),
            (self.state, 1),
        ]
    }
}

macro_rules! reg {
    ($class:ident, $index:literal) => {
        match $class::new($index) {
            Ok(register) => register,
            Err(_) => panic!("fixed ABI register outside class"),
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Abi {
    pub clobbered_indices: [Dag1Index; 2],
    pub clobbered_modifiers: [Dag1Modify; 1],
    pub zero_lengths: [Dag1Length; 2],
    pub clobbers_all_data_registers: bool,
    pub clobbers_arithmetic_flags: bool,
    pub required_mode1_set: u32,
    pub required_mode1_clear: u32,
}

/// The code sets I0=input, I1=output, M0=1 and the R8 counter itself; it reads no
/// caller register. L0/L1 must be zero. R0..R15 and AZ are clobbered.
pub const ABI: Abi = Abi {
    clobbered_indices: [reg!(Dag1Index, 0), reg!(Dag1Index, 1)],
    clobbered_modifiers: [reg!(Dag1Modify, 0)],
    zero_lengths: [reg!(Dag1Length, 0), reg!(Dag1Length, 1)],
    clobbers_all_data_registers: true,
    clobbers_arithmetic_flags: true,
    required_mode1_set: MODE1_RND32,
    required_mode1_clear: MODE1_SCALAR_CLEAR,
};

/// Executed instruction counts of one call (including RTS); not processor cycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepPlan {
    /// Entry, exit and RTS.
    pub fixed: usize,
    /// Per sample on either path.
    pub common: usize,
    pub reciprocal: usize,
    pub midpoint: usize,
}

impl StepPlan {
    /// Returns None for a count above 32 or overflow in the public plan fields.
    pub fn steps(self, reciprocal_samples: u32) -> Option<usize> {
        let midpoint_samples = SAMPLES.checked_sub(reciprocal_samples)? as usize;
        self.fixed
            .checked_add((SAMPLES as usize).checked_mul(self.common)?)?
            .checked_add((reciprocal_samples as usize).checked_mul(self.reciprocal)?)?
            .checked_add(midpoint_samples.checked_mul(self.midpoint)?)
    }
}

/// Data-dependent result of one committed call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    pub steps: usize,
    pub stores: usize,
    pub reciprocal_samples: u32,
}

/// Code, placement and step plan form one immutable compilation result. The code
/// is built once and reused for every block; state lives in caller DM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledAdaaClip {
    code: OwnedCode,
    layout: Layout,
    plan: StepPlan,
}

/// Counts pushed words per section so the step plan comes from the emitted code.
struct Emitter {
    program: IsaProgram,
    count: usize,
}

impl Emitter {
    fn push(&mut self, instruction: Instruction48) -> Result<(), EmitError> {
        self.program.push(instruction)?;
        self.count += 1;
        Ok(())
    }
    fn branch(&mut self, condition: Condition, label: &Label) -> Result<(), EmitError> {
        self.program.branch_to(
            BranchKind::Jump,
            condition,
            label.clone(),
            BranchRelocation::PcRel24,
        )?;
        self.count += 1;
        Ok(())
    }
    fn section(&mut self) -> usize {
        std::mem::take(&mut self.count)
    }
    fn immediate(&mut self, dst: FloatReg, value: f32) -> Result<(), EmitError> {
        self.push(Instruction48::Immediate {
            dst: UniversalReg::Data(dst.into()),
            value: value.to_bits(),
        })
    }
}

/// `float!(FloatMultiply, dst, lhs, rhs)` builds a three-register float compute.
macro_rules! float {
    ($variant:ident, $dst:expr, $lhs:expr, $rhs:expr) => {
        Instruction48::$variant {
            dst: $dst,
            lhs: $lhs,
            rhs: $rhs,
        }
    };
}

impl CompiledAdaaClip {
    /// Registers: F0=x, F1=p, F2=A(p), F3=A(x), F4=2.0, F5=eps, F6=1.0, F7=0.5,
    /// R8=counter, F9..F15 temporaries (k, 0.25 and MAGIC by immediate at use).
    ///
    /// ```text
    /// entry: M0=1; I0=input; I1=output; R8=32; R1=DM(state); constants; F2=A(F1)
    /// loop:  F0=DM(I0,M0); F3=A(F0); F12=F0-F1; F13=max(abs F12, eps)
    ///        comp(F13,eps); IF EQ JUMP midpoint
    ///        F9=1.0 copysign F12; F10=F3-F2; F9=F10*F9
    ///        R14=MAGIC; R14=R14-R13; 7 x (F15=F13*F14; F15=2.0-F15; F14=F15*F14)
    ///        F11=F9*F14; JUMP store
    /// mid:   F9=(F0+F1)*0.5; F9=clip F9 by 2.0; F11=F9*(1.0-abs(F9)*0.25)
    /// store: DM(I1,M0)=F11; R1=PASS R0; R2=PASS R3; R8=R8-1; IF NE JUMP loop
    /// exit:  DM(state)=R1; RTS
    /// ```
    /// A(v) = ((((k*b)+0.5)*b)-1)*b + |v| with b = clip |v| by 2.0 (stock order).
    pub fn compile(origin: PmWordAddress24, layout: Layout) -> Result<Self, Error> {
        let f: [FloatReg; 16] =
            std::array::from_fn(|index| FloatReg::new(index as u8).expect("fixed F0..F15 set"));
        let r = f.map(DataReg::from);
        let [i0, i1] = ABI.clobbered_indices;
        let [m0] = ABI.clobbered_modifiers;
        let (two, eps, one, half) = (f[4], f[5], f[6], f[7]);
        let counter = r[8];
        let loop_label = Label::new("adaa_clip.loop");
        let midpoint_label = Label::new("adaa_clip.midpoint");
        let store_label = Label::new("adaa_clip.store");

        let mut program = IsaProgram::new(origin);
        program.define(Label::new("adaa_clip.entry"))?;
        let mut e = Emitter { program, count: 0 };
        let emit_antiderivative = |e: &mut Emitter, dst: FloatReg, src: FloatReg| {
            let (magnitude, bounded, t) = (f[9], f[10], f[11]);
            e.push(Instruction48::FloatAbs {
                dst: magnitude,
                src,
            })?;
            e.push(Instruction48::FloatClip {
                dst: bounded,
                src: magnitude,
                bound: two,
            })?;
            e.immediate(t, CUBIC)?;
            e.push(float!(FloatMultiply, t, t, bounded))?;
            e.push(float!(FloatAdd, t, t, half))?;
            e.push(float!(FloatMultiply, t, t, bounded))?;
            e.push(float!(FloatSubtract, t, t, one))?;
            e.push(float!(FloatMultiply, t, t, bounded))?;
            e.push(float!(FloatAdd, dst, t, magnitude))
        };

        for (dst, value) in [
            (UniversalReg::Modify(m0), 1),
            (UniversalReg::Index(i0), layout.input.get()),
            (UniversalReg::Index(i1), layout.output.get()),
            (UniversalReg::Data(counter), SAMPLES),
        ] {
            e.push(Instruction48::Immediate { dst, value })?;
        }
        e.push(Instruction48::DmDirectLoad {
            dst: r[1],
            address: layout.state,
        })?;
        for (dst, value) in [(two, 2.0), (eps, ADAA_EPSILON), (one, 1.0), (half, 0.5)] {
            e.immediate(dst, value)?;
        }
        emit_antiderivative(&mut e, f[2], f[1])?;
        let entry = e.section();

        e.program.define(loop_label.clone())?;
        e.push(Instruction48::DmLoad {
            dst: r[0],
            index: i0,
            modifier: m0,
            access: DmAccess::PostModify,
        })?;
        emit_antiderivative(&mut e, f[3], f[0])?;
        e.push(float!(FloatSubtract, f[12], f[0], f[1]))?;
        e.push(Instruction48::FloatAbs {
            dst: f[13],
            src: f[12],
        })?;
        e.push(Instruction48::FloatMax {
            dst: f[13],
            lhs: f[13],
            rhs: eps,
        })?;
        // AZ from comp must reach the branch unchanged: nothing in between.
        e.push(Instruction48::FloatCompare {
            lhs: f[13],
            rhs: eps,
        })?;
        e.branch(Condition::Equal, &midpoint_label)?;
        let common_head = e.section();

        e.push(Instruction48::FloatCopySign {
            dst: f[9],
            magnitude: one,
            sign: f[12],
        })?;
        e.push(float!(FloatSubtract, f[10], f[3], f[2]))?;
        e.push(float!(FloatMultiply, f[9], f[10], f[9]))?;
        e.push(Instruction48::Immediate {
            dst: UniversalReg::Data(r[14]),
            value: RECIPROCAL_MAGIC,
        })?;
        e.push(Instruction48::IntegerSubtract {
            dst: r[14],
            lhs: r[14],
            rhs: r[13],
        })?;
        for _ in 0..NEWTON_STEPS {
            e.push(float!(FloatMultiply, f[15], f[13], f[14]))?;
            e.push(float!(FloatSubtract, f[15], two, f[15]))?;
            e.push(float!(FloatMultiply, f[14], f[15], f[14]))?;
        }
        e.push(float!(FloatMultiply, f[11], f[9], f[14]))?;
        e.branch(Condition::Always, &store_label)?;
        let reciprocal = e.section();

        e.program.define(midpoint_label)?;
        e.push(float!(FloatAdd, f[9], f[0], f[1]))?;
        e.push(float!(FloatMultiply, f[9], f[9], half))?;
        e.push(Instruction48::FloatClip {
            dst: f[9],
            src: f[9],
            bound: two,
        })?;
        e.push(Instruction48::FloatAbs {
            dst: f[10],
            src: f[9],
        })?;
        e.immediate(f[15], 0.25)?;
        e.push(float!(FloatMultiply, f[10], f[10], f[15]))?;
        e.push(float!(FloatSubtract, f[10], one, f[10]))?;
        e.push(float!(FloatMultiply, f[11], f[9], f[10]))?;
        let midpoint = e.section();

        e.program.define(store_label)?;
        e.push(Instruction48::DmStore {
            src: r[11],
            index: i1,
            modifier: m0,
            access: DmAccess::PostModify,
        })?;
        // Raw-bit moves keep -0 and every payload bit of p and A(p).
        e.push(Instruction48::IntegerPass {
            dst: r[1],
            src: r[0],
        })?;
        e.push(Instruction48::IntegerPass {
            dst: r[2],
            src: r[3],
        })?;
        e.push(Instruction48::IntegerDecrement {
            dst: counter,
            src: counter,
        })?;
        e.branch(Condition::NotEqual, &loop_label)?;
        let common_tail = e.section();

        e.program.define(Label::new("adaa_clip.exit"))?;
        e.push(Instruction48::DmDirectStore {
            src: r[1],
            address: layout.state,
        })?;
        e.push(Instruction48::Rts)?;
        let exit = e.section();

        Ok(Self {
            code: e.program.finish()?,
            layout,
            plan: StepPlan {
                fixed: entry + exit,
                common: common_head + common_tail,
                reciprocal,
                midpoint,
            },
        })
    }

    pub fn code(&self) -> &OwnedCode {
        &self.code
    }
    pub fn layout(&self) -> Layout {
        self.layout
    }
    pub fn step_plan(&self) -> StepPlan {
        self.plan
    }

    /// Atomic offline call. Validates modes, L0/L1, ranges and every input/state
    /// word up front, then runs the raw words on clones under the normal-only float
    /// policy. Commits registers, DM and the write log only if the run returns, the
    /// write log is exactly output[0..32] then state, and the step count matches a
    /// whole number of reciprocal samples. Any error leaves both untouched.
    pub fn simulate_atomic(
        &self,
        state: &mut ScalarState,
        memory: &mut DmMemory,
        step_budget: usize,
    ) -> Result<Receipt, Error> {
        if state.mode1 & ABI.required_mode1_set != ABI.required_mode1_set
            || state.mode1 & ABI.required_mode1_clear != 0
        {
            return Err(SimulationError::Modes(state.mode1).into());
        }
        for length in ABI.zero_lengths {
            let index = usize::from(length.index());
            if state.lengths[index] != 0 {
                return Err(SimulationError::CircularLength {
                    index,
                    length: state.lengths[index],
                }
                .into());
            }
        }
        for (start, words) in self.layout.spans() {
            memory.validate_range(start, words)?;
        }
        let input = (0..SAMPLES).map(|offset| self.layout.input.checked_add(offset));
        for address in input.chain([Ok(self.layout.state)]) {
            let address = address?;
            let bits = memory.read(address)?;
            Preactivation::new(f32::from_bits(bits)).map_err(|_| Error::DomainAt {
                address: address.get(),
                bits,
            })?;
        }

        let mut next_state = state.clone();
        let mut next_memory = memory.clone();
        let prior_writes = next_memory.writes().len();
        let outcome =
            run_scalar_normal_floats(&self.code, &mut next_state, &mut next_memory, step_budget)?;
        let expected_writes = (0..SAMPLES)
            .map(|offset| self.layout.output.checked_add(offset))
            .chain([Ok(self.layout.state)])
            .collect::<Result<Vec<_>, _>>()?;
        let reciprocal_samples = (0..=SAMPLES)
            .find(|&count| self.plan.steps(count) == Some(outcome.steps))
            .ok_or(Error::Receipt)?;
        if next_memory.writes()[prior_writes..] != expected_writes[..]
            || outcome.stores != expected_writes.len()
        {
            return Err(Error::Receipt);
        }
        *state = next_state;
        *memory = next_memory;
        Ok(Receipt {
            steps: outcome.steps,
            stores: outcome.stores,
            reciprocal_samples,
        })
    }
}

/// Host model of the emitted leaf: the same operations, order and owned reciprocal
/// in f32, refusing every subnormal or nonfinite result like the strict runner.
/// It is the owned contract, not stock RECIPS parity and not `reference::AdaaClip`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OwnedAdaaClip {
    previous: Preactivation,
}

/// Output of one host block, with the number of samples that took the quotient path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostBlock {
    pub output: [f32; SAMPLES as usize],
    pub reciprocal_samples: u32,
}

fn normal(value: f32) -> Option<f32> {
    (value == 0.0 || value.is_normal()).then_some(value)
}

/// Manual CLIP (23.26): `|x| < |bound| ? x : |bound|` with the sign of x.
fn clip_by(value: f32, bound: f32) -> f32 {
    if value.abs() < bound.abs() {
        value
    } else {
        bound.abs().copysign(value)
    }
}

fn antiderivative(value: f32) -> Option<f32> {
    let magnitude = value.abs();
    let bounded = clip_by(magnitude, 2.0);
    let mut t = normal(CUBIC * bounded)?;
    t = normal(t + 0.5)?;
    t = normal(t * bounded)?;
    t = normal(t - 1.0)?;
    t = normal(t * bounded)?;
    normal(t + magnitude)
}

fn owned_reciprocal(m: f32) -> Option<f32> {
    let mut r = normal(f32::from_bits(RECIPROCAL_MAGIC.wrapping_sub(m.to_bits())))?;
    for _ in 0..NEWTON_STEPS {
        let t = normal(m * r)?;
        let u = normal(2.0 - t)?;
        r = normal(u * r)?;
    }
    Some(r)
}

impl OwnedAdaaClip {
    pub fn new(previous: Preactivation) -> Self {
        Self { previous }
    }
    pub fn previous(self) -> Preactivation {
        self.previous
    }

    /// Atomic: history advances to the last sample only if every sample succeeds.
    pub fn process_block(
        &mut self,
        input: &[Preactivation; SAMPLES as usize],
    ) -> Result<HostBlock, Error> {
        let mut p = self.previous.get();
        let mut a_p = antiderivative(p).ok_or(Error::NonNormal { sample: None })?;
        let mut output = [0.0; SAMPLES as usize];
        let mut reciprocal_samples = 0;
        for (index, (x, y)) in input.iter().zip(&mut output).enumerate() {
            let x = x.get();
            let refused = Error::NonNormal {
                sample: Some(index as u32),
            };
            let sample = || {
                let a_x = antiderivative(x)?;
                let d = normal(x - p)?;
                // max(|d|, eps) never sees ±0 against eps, so `f32::max` is exact here.
                let m = d.abs().max(ADAA_EPSILON);
                let value = if m == ADAA_EPSILON {
                    let c = clip_by(normal(normal(x + p)? * 0.5)?, 2.0);
                    let scaled = normal(c.abs() * 0.25)?;
                    normal(c * normal(1.0 - scaled)?)?
                } else {
                    let s = 1.0f32.copysign(d);
                    let n = normal(normal(a_x - a_p)? * s)?;
                    normal(n * owned_reciprocal(m)?)?
                };
                Some((value, a_x, m != ADAA_EPSILON))
            };
            let (value, a_x, quotient) = sample().ok_or(refused)?;
            *y = value;
            reciprocal_samples += u32::from(quotient);
            (p, a_p) = (x, a_x);
        }
        self.previous = Preactivation(p);
        Ok(HostBlock {
            output,
            reciprocal_samples,
        })
    }
}
