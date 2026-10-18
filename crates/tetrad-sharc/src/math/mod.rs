//! Owned ISA48 math leaves for control-rate parameter laws, each with an exact
//! host twin.
//!
//! # Leaf ABI (owned)
//!
//! - Argument in F4, result in F0. A leaf writes only its [`Leaf::clobbers`] registers, a subset of
//!   the CCES scratch set R0, R1, R2, R4, R8, R12. It touches no DAG register, no memory and no
//!   mode register. [`Leaf::LinearToDb`] and [`Leaf::Tan`] CALL one inner leaf, so they use one
//!   more PC stack entry.
//! - The caller owns the modes: RND32 set, TRUNCATE and ALUSAT clear (roadmap
//!   §2a: each owned routine sets them). Then FIX rounds to nearest even.
//! - Position independent: every JUMP and CALL is PC-relative, so a section
//!   that holds the leaves needs no relocation.
//! - Inputs are finite. Each leaf clamps its input to its domain (table), so an
//!   out-of-domain input gives the value at the nearest domain edge.
//!
//! # Twin contract
//!
//! Each twin (`exp2`, `log2`, …) does the operations of its leaf in the same
//! order: separate f32 multiply and add with round to nearest even (the emitted
//! subset has no fused operation), the same integer bit steps and the same
//! constants. `tests/math.rs` runs each leaf on [`crate::machine::Machine`] over
//! a dense sweep and compares the result bits with the twin.
//!
//! Denormal rule: inside the domain no operand or result is nonfinite.
//! Subnormal intermediates occur only for tiny inputs (z = r² when
//! |r| < 2^-63; a coefficient times f when |f| < 2^-112), and a later add to a
//! value at least 2^60 times larger absorbs them. So host f32 and the PRM rule
//! (round to 24 bits, then subnormal → signed zero) give the same bits, with
//! one known exception: `tan(2^-126)` is exactly 2^-126 − 2^-150 before the
//! last rounding; host f32 rounds it up to 2^-126, the PRM flushes it to zero.
//! Subnormal inputs are outside the domain (the PRM reads them as zero).
//! `tests/math.rs` runs the full sweep on the host-f32 machine and a modest
//! sweep under [`crate::machine::Arithmetic::Prm`] with the differences pinned.
//! [`prm`] has the exact PRM operations for twins that must hold at underflow.
//!
//! # Leaves
//!
//! Coefficients are owned minimax (Remez) fits, rounded to f32. The reciprocal
//! uses an integer seed and Newton steps, not `RECIPS` (its seed ROM is
//! unknown). Words count the leaf's own code. Cycles are executed words from
//! entry through RTS, without the caller's CALL (machine model: one cycle per
//! word); they include the inner leaf. The error is the maximum over every f32
//! input in the domain, in ulp of the exact f64 result, rounded up (the
//! ignored test `error_table` in `tests/math.rs` measures it).
//!
//! | Leaf | Domain (clamped) | Words | Cycles | Max error (ulp) |
//! | --- | --- | --- | --- | --- |
//! | `exp2` | \|x\| ≤ 126 | 27 | 27 | 1.23 |
//! | `db_to_linear` | \|dB\| ≤ 758.6 | 2 + `exp2` | 29 | 49.55; 12.29 for \|dB\| ≤ 120 |
//! | `log2` | x ≥ 2^-126 | 45 | 45 | 1.80 |
//! | `linear_to_db` | x ≥ 2^-126 | 4 + `log2` | 49 | 2.44 |
//! | `sin` | \|x\| ≤ 1024 | 54 | 39–42 | 2.42 |
//! | `cos` | \|x\| ≤ 1024 | 2 + `sin` | 40–43 | 2.38 |
//! | `tan` | \|x\| ≤ 1024 | 54 + `reciprocal` | 69–70 | 5.78 |
//! | `reciprocal` | 2^-125 ≤ \|x\| ≤ 2^125 | 20 | 20 | 1.99 |
//!
//! `sin`/`cos` take 3 more cycles on the cosine-polynomial quadrants. The
//! `db_to_linear` error comes from the one rounding of dB·log2(10)/20, which
//! grows with \|dB\|; it is far below audibility (12 ulp ≈ 1e-5 dB).
use crate::emit::{BranchRelocation, EmitError, IsaProgram, Label};
use crate::isa48::{BranchKind, Condition, Instruction48};
use crate::register::{DataReg, FloatReg, UniversalReg};

mod exp;
mod log;
pub mod prm;
mod reciprocal;
mod trig;

pub use exp::{EXP2_LIMIT, db_to_linear, exp2};
pub use log::{linear_to_db, log2};
pub use reciprocal::{RECIPROCAL_MAX, RECIPROCAL_MIN, reciprocal};
pub use trig::{TRIG_LIMIT, cos, sin, tan};

/// CCES C run-time scratch registers. The leaf builder takes only these, so a leaf cannot write a
/// callee-saved register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scratch {
    R0 = 0,
    R1 = 1,
    R2 = 2,
    R4 = 4,
    R8 = 8,
    R12 = 12,
}

impl From<Scratch> for DataReg {
    fn from(value: Scratch) -> Self {
        DataReg::new(value as u8).expect("scratch registers are data registers")
    }
}

impl From<Scratch> for FloatReg {
    fn from(value: Scratch) -> Self {
        DataReg::from(value).into()
    }
}

/// Argument register of every leaf (F4).
pub const ARGUMENT: Scratch = Scratch::R4;
/// Result register of every leaf (F0).
pub const RESULT: Scratch = Scratch::R0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Leaf {
    Exp2,
    DbToLinear,
    Log2,
    LinearToDb,
    Sin,
    Cos,
    Tan,
    Reciprocal,
}

impl Leaf {
    /// Emission order. `DbToLinear` falls through into `Exp2`, so it comes
    /// directly before it.
    pub const ALL: [Self; 8] = [
        Self::DbToLinear,
        Self::Exp2,
        Self::LinearToDb,
        Self::Log2,
        Self::Sin,
        Self::Cos,
        Self::Tan,
        Self::Reciprocal,
    ];

    /// The CALL target. [`emit`] defines it.
    pub fn label(self) -> Label {
        Label::new(match self {
            Self::Exp2 => "math.exp2",
            Self::DbToLinear => "math.db_to_linear",
            Self::Log2 => "math.log2",
            Self::LinearToDb => "math.linear_to_db",
            Self::Sin => "math.sin",
            Self::Cos => "math.cos",
            Self::Tan => "math.tan",
            Self::Reciprocal => "math.reciprocal",
        })
    }

    /// The leaf whose code this leaf enters (fall-through, JUMP or CALL).
    pub const fn requires(self) -> Option<Self> {
        match self {
            Self::DbToLinear => Some(Self::Exp2),
            Self::LinearToDb => Some(Self::Log2),
            Self::Cos => Some(Self::Sin),
            Self::Tan => Some(Self::Reciprocal),
            Self::Exp2 | Self::Log2 | Self::Sin | Self::Reciprocal => None,
        }
    }

    /// Registers the leaf may change. Every other register keeps its value.
    pub const fn clobbers(self) -> &'static [Scratch] {
        use Scratch::*;
        match self {
            Self::Exp2 | Self::DbToLinear | Self::Reciprocal => &[R0, R2, R4, R8],
            Self::Log2 | Self::LinearToDb => &[R0, R1, R2, R8],
            Self::Sin | Self::Cos | Self::Tan => &[R0, R1, R2, R4, R8, R12],
        }
    }

    /// The host twin.
    pub fn host(self, x: f32) -> f32 {
        match self {
            Self::Exp2 => exp2(x),
            Self::DbToLinear => db_to_linear(x),
            Self::Log2 => log2(x),
            Self::LinearToDb => linear_to_db(x),
            Self::Sin => sin(x),
            Self::Cos => cos(x),
            Self::Tan => tan(x),
            Self::Reciprocal => reciprocal(x),
        }
    }
}

/// Append `leaves` and the leaves they require, each once, in [`Leaf::ALL`]
/// order. Call a leaf with `CALL` to [`Leaf::label`]. Emit once per program:
/// a second call fails with [`EmitError::DuplicateLabel`].
pub fn emit(program: &mut IsaProgram, leaves: &[Leaf]) -> Result<(), EmitError> {
    let mut needed = [false; Leaf::ALL.len()];
    for &leaf in leaves {
        let mut next = Some(leaf);
        while let Some(leaf) = next {
            needed[leaf as usize] = true;
            next = leaf.requires();
        }
    }
    let mut asm = Asm(program);
    for leaf in Leaf::ALL.into_iter().filter(|&leaf| needed[leaf as usize]) {
        match leaf {
            Leaf::Exp2 => exp::emit_exp2(&mut asm)?,
            Leaf::DbToLinear => exp::emit_db_to_linear(&mut asm)?,
            Leaf::Log2 => log::emit_log2(&mut asm)?,
            Leaf::LinearToDb => log::emit_linear_to_db(&mut asm)?,
            Leaf::Sin => trig::emit_sin(&mut asm)?,
            Leaf::Cos => trig::emit_cos(&mut asm)?,
            Leaf::Tan => trig::emit_tan(&mut asm)?,
            Leaf::Reciprocal => reciprocal::emit_reciprocal(&mut asm)?,
        }
    }
    Ok(())
}

/// Two-source ALU operations on scratch registers.
#[derive(Debug, Clone, Copy)]
enum Op {
    FMul,
    FAdd,
    FSub,
    FMax,
    FClip,
    FCopySign,
    IAdd,
    ISub,
    And,
    Xor,
}

/// Leaf code builder over scratch registers only.
struct Asm<'a>(&'a mut IsaProgram);

impl Asm<'_> {
    fn push(&mut self, instruction: Instruction48) -> Result<(), EmitError> {
        self.0.push(instruction).map(drop)
    }

    fn label(&mut self, label: Label) -> Result<(), EmitError> {
        self.0.define(label)
    }

    /// `Rn = imm32`.
    fn set(&mut self, dst: Scratch, bits: u32) -> Result<(), EmitError> {
        self.push(Instruction48::Immediate {
            dst: UniversalReg::Data(dst.into()),
            value: bits,
        })
    }

    fn op(&mut self, op: Op, dst: Scratch, lhs: Scratch, rhs: Scratch) -> Result<(), EmitError> {
        let (fd, fl, fr) = (dst.into(), lhs.into(), rhs.into());
        let (d, l, r) = (dst.into(), lhs.into(), rhs.into());
        self.push(match op {
            Op::FMul => Instruction48::FloatMultiply {
                dst: fd,
                lhs: fl,
                rhs: fr,
            },
            Op::FAdd => Instruction48::FloatAdd {
                dst: fd,
                lhs: fl,
                rhs: fr,
            },
            Op::FSub => Instruction48::FloatSubtract {
                dst: fd,
                lhs: fl,
                rhs: fr,
            },
            Op::FMax => Instruction48::FloatMax {
                dst: fd,
                lhs: fl,
                rhs: fr,
            },
            Op::FClip => Instruction48::FloatClip {
                dst: fd,
                src: fl,
                bound: fr,
            },
            Op::FCopySign => Instruction48::FloatCopySign {
                dst: fd,
                magnitude: fl,
                sign: fr,
            },
            Op::IAdd => Instruction48::IntegerAdd {
                dst: d,
                lhs: l,
                rhs: r,
            },
            Op::ISub => Instruction48::IntegerSubtract {
                dst: d,
                lhs: l,
                rhs: r,
            },
            Op::And => Instruction48::And {
                dst: d,
                lhs: l,
                rhs: r,
            },
            Op::Xor => Instruction48::Xor {
                dst: d,
                lhs: l,
                rhs: r,
            },
        })
    }

    /// `acc = horner(coefficients, x)` with R2 as the coefficient register;
    /// the twin is [`horner`].
    fn horner(&mut self, acc: Scratch, x: Scratch, coefficients: &[f32]) -> Result<(), EmitError> {
        let (&last, rest) = coefficients
            .split_last()
            .expect("a polynomial has coefficients");
        self.set(acc, last.to_bits())?;
        for c in rest.iter().rev() {
            self.op(Op::FMul, acc, acc, x)?;
            self.set(Scratch::R2, c.to_bits())?;
            self.op(Op::FAdd, acc, acc, Scratch::R2)?;
        }
        Ok(())
    }

    fn jump(&mut self, condition: Condition, label: Label) -> Result<(), EmitError> {
        self.0
            .branch_to(
                BranchKind::Jump,
                condition,
                label,
                BranchRelocation::PcRel24,
            )
            .map(drop)
    }

    fn call(&mut self, leaf: Leaf) -> Result<(), EmitError> {
        self.0
            .branch_to(
                BranchKind::Call,
                Condition::Always,
                leaf.label(),
                BranchRelocation::PcRel24,
            )
            .map(drop)
    }
}

/// `c[0] + x·(c[1] + x·(…))`, one rounding per multiply and per add.
fn horner(coefficients: &[f32], x: f32) -> f32 {
    let (&last, rest) = coefficients
        .split_last()
        .expect("a polynomial has coefficients");
    rest.iter().rev().fold(last, |acc, &c| acc * x + c)
}

/// ISA `CLIP x BY bound` for finite x and a positive bound.
fn clip(x: f32, bound: f32) -> f32 {
    if x.abs() < bound {
        x
    } else {
        bound.copysign(x)
    }
}
