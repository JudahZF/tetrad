//! Math leaves: machine run equals the host twin bit for bit, the leaf ABI
//! holds, and the twin error vs f64 stays inside the documented table.
use tetrad_sharc::address::PmWordAddress24;
use tetrad_sharc::emit::{BranchRelocation, IsaProgram, OwnedCode};
use tetrad_sharc::isa48::{BranchKind, Condition, Instruction48};
use tetrad_sharc::machine::{Arithmetic, Machine, MemoryMap, ProgramMemory, Stop};
use tetrad_sharc::math::{self, Leaf};

/// Section 3 base of an owned blob (roadmap §2a).
const ORIGIN: u32 = 0xe_0000;

/// Cycles per leaf (min, max over all paths): pins the module doc table.
const CYCLES: [(Leaf, u64, u64); 8] = [
    (Leaf::Exp2, 27, 27),
    (Leaf::DbToLinear, 29, 29),
    (Leaf::Log2, 45, 45),
    (Leaf::LinearToDb, 49, 49),
    (Leaf::Sin, 39, 42),
    (Leaf::Cos, 40, 43),
    (Leaf::Tan, 69, 70),
    (Leaf::Reciprocal, 20, 20),
];

/// Max twin error in ulp of the exact result: pins the module doc table.
const MAX_ULP: [(Leaf, f64); 8] = [
    (Leaf::Exp2, 1.23),
    (Leaf::DbToLinear, 49.55),
    (Leaf::Log2, 1.80),
    (Leaf::LinearToDb, 2.44),
    (Leaf::Sin, 2.42),
    (Leaf::Cos, 2.38),
    (Leaf::Tan, 5.78),
    (Leaf::Reciprocal, 1.99),
];

/// One `CALL leaf; NOP` driver per leaf at the origin, then the leaves.
struct Harness {
    code: OwnedCode,
    program: ProgramMemory,
    drivers: Vec<Leaf>,
}

impl Harness {
    fn new(leaves: &[Leaf]) -> Self {
        let mut program = IsaProgram::new(PmWordAddress24::new(ORIGIN).unwrap());
        for &leaf in leaves {
            program
                .branch_to(
                    BranchKind::Call,
                    Condition::Always,
                    leaf.label(),
                    BranchRelocation::PcRel24,
                )
                .unwrap();
            program.push(Instruction48::Nop).unwrap();
        }
        math::emit(&mut program, leaves).unwrap();
        let code = program.finish().unwrap();
        let mut memory = ProgramMemory::default();
        memory.load(&code).unwrap();
        Self {
            code,
            program: memory,
            drivers: leaves.to_vec(),
        }
    }

    /// Result bits and leaf cycles. Checks the ABI: only clobbered registers
    /// change, no memory access (the bus has no regions), no mode or DAG
    /// change, no AIS/MIS sticky, the PC stack is empty again. `Machine::reset`
    /// has RND32 set, the kernel's MODE1 at callback entry.
    fn run(&self, leaf: Leaf, x: f32, arithmetic: Arithmetic) -> (u32, u64) {
        let index = self.drivers.iter().position(|&l| l == leaf).unwrap() as u32;
        let call = ORIGIN + 2 * index;
        let mut machine = Machine::reset();
        machine.arithmetic = arithmetic;
        for (n, r) in machine.r.iter_mut().enumerate() {
            *r = 0x5eed_0000 | n as u32;
        }
        machine.r[4] = x.to_bits();
        machine.set_pc(call);
        let before = machine.clone();
        let stop = machine
            .run(
                &self.program,
                &mut MemoryMap::default(),
                Some(call + 1),
                500,
            )
            .unwrap_or_else(|e| panic!("{leaf:?}({x:e}): {e}"));
        assert_eq!(stop, Stop::Breakpoint(call + 1));
        for n in 0..16 {
            if !leaf.clobbers().iter().any(|&s| s as usize == n) {
                assert_eq!(machine.r[n], before.r[n], "{leaf:?}({x:e}) changed R{n}");
            }
        }
        let system = |m: &Machine| (m.i, m.m, m.b, m.l, m.ustat, m.mode1, m.mode2, m.lcntr);
        assert_eq!(system(&machine), system(&before), "{leaf:?}({x:e})");
        assert_eq!(machine.stkyx, 0, "{leaf:?}({x:e}) raised AIS/MIS");
        assert_eq!(machine.pc_stack_depth(), 0);
        (machine.r[0], machine.cycles() - 1)
    }
}

fn grid(lo: f32, hi: f32, n: u32) -> impl Iterator<Item = f32> {
    (0..=n).map(move |i| lo + (hi - lo) * (i as f32 / n as f32))
}

/// Every `step`-th bit pattern in `lo..=hi` (positive floats, log spaced).
fn bit_stride(lo: f32, hi: f32, n: u32) -> impl Iterator<Item = f32> {
    let (lo, hi) = (lo.to_bits(), hi.to_bits());
    let step = ((hi - lo) / n).max(1);
    (lo..=hi).step_by(step as usize).map(f32::from_bits)
}

/// Dense in-domain sweep plus clamped inputs, edges and anchors.
fn sweep(leaf: Leaf, n: u32) -> Vec<f32> {
    let tiny = [
        0.0,
        -0.0,
        f32::MIN_POSITIVE,
        2f32.powi(-70),
        -2f32.powi(-120),
    ];
    let mut x: Vec<f32> = match leaf {
        Leaf::Exp2 => grid(-130.0, 130.0, n)
            .chain([126.0, -126.0, -125.5, 125.5, 0.5, -0.5, 1.0])
            .collect(),
        Leaf::DbToLinear => grid(-800.0, 800.0, n)
            .chain(grid(-120.0, 20.0, n / 4))
            .chain([0.0, 6.0, -6.0, 12.0, -60.0])
            .collect(),
        Leaf::Log2 | Leaf::LinearToDb => bit_stride(f32::MIN_POSITIVE, f32::MAX, n)
            .chain(grid(0.5, 2.0, n / 4))
            .chain([1.0, 2.0, 0.5, f32::MAX, -1.0, 2f32.sqrt(), 0.5f32.sqrt()])
            .collect(),
        Leaf::Sin | Leaf::Cos | Leaf::Tan => grid(-1100.0, 1100.0, n)
            .chain(grid(-7.0, 7.0, n / 2))
            .chain((-8..=8).flat_map(|k| {
                let near = k as f32 * std::f32::consts::FRAC_PI_2;
                [near, f32::from_bits(near.to_bits() + 1), 1024.0, -1024.0]
            }))
            .collect(),
        Leaf::Reciprocal => bit_stride(f32::MIN_POSITIVE, f32::MAX, n)
            .flat_map(|v| [v, -v])
            .chain([1.0, -1.0, 3.0, math::RECIPROCAL_MAX, math::RECIPROCAL_MIN])
            .collect(),
    };
    x.extend(tiny);
    x
}

#[test]
fn leaves_equal_host_twins_bit_for_bit() {
    let harness = Harness::new(&Leaf::ALL);
    for (leaf, min, max) in CYCLES {
        let mut seen = (u64::MAX, 0);
        for x in sweep(leaf, 1 << 15) {
            let (bits, cycles) = harness.run(leaf, x, Arithmetic::HostF32);
            assert_eq!(
                bits,
                leaf.host(x).to_bits(),
                "{leaf:?}({x:e} = {:#010x})",
                x.to_bits()
            );
            seen = (seen.0.min(cycles), seen.1.max(cycles));
        }
        assert_eq!(seen, (min, max), "{leaf:?} cycles");
    }
}

/// Host f32 vs PRM differences: (leaf, input, machine bits, twin bits). The
/// twins use host gradual underflow; the PRM rounds to 24 bits first and then
/// flushes. `tan(2^-126)`: s·(1/c) is exactly 2^-126 − 2^-150, which host f32
/// rounds up to 2^-126 and the PRM keeps below 2^-126, so it flushes to zero.
/// `sin(2^-149)`: a subnormal input (outside the domain) reads as zero.
const PRM_DIFFERENCES: [(Leaf, f32, u32, u32); 2] = [
    (Leaf::Tan, f32::MIN_POSITIVE, 0, 0x0080_0000),
    (Leaf::Sin, f32::from_bits(1), 0, 1),
];

/// Under the kernel's rules (RND32 = 1, denormals flushed) the leaves equal
/// their twins on a modest sweep, except the pinned underflow cases. Subnormal
/// inputs are outside the domain and read as signed zero, so they are skipped.
#[test]
fn leaves_equal_host_twins_under_prm_rules_except_at_underflow() {
    let harness = Harness::new(&Leaf::ALL);
    for (leaf, x, machine, twin) in PRM_DIFFERENCES {
        assert_eq!(
            harness.run(leaf, x, Arithmetic::Prm).0,
            machine,
            "{leaf:?}({x:e})"
        );
        assert_eq!(leaf.host(x).to_bits(), twin, "{leaf:?}({x:e}) twin");
    }
    for leaf in Leaf::ALL {
        let known = |x: f32| PRM_DIFFERENCES.iter().any(|d| (d.0, d.1) == (leaf, x));
        for x in sweep(leaf, 1 << 10)
            .into_iter()
            .filter(|&x| !x.is_subnormal() && !known(x))
        {
            assert_eq!(
                harness.run(leaf, x, Arithmetic::Prm).0,
                leaf.host(x).to_bits(),
                "{leaf:?}({x:e} = {:#010x})",
                x.to_bits()
            );
        }
    }
}

/// Requested leaves pull in what they enter, and nothing else.
#[test]
fn emit_adds_required_leaves_only() {
    let harness = Harness::new(&[Leaf::DbToLinear, Leaf::Tan]);
    let symbols = harness.code.symbols();
    for leaf in [Leaf::Exp2, Leaf::Reciprocal] {
        assert!(symbols.contains_key(&leaf.label()), "{leaf:?} missing");
    }
    for leaf in [Leaf::Log2, Leaf::LinearToDb, Leaf::Sin, Leaf::Cos] {
        assert!(!symbols.contains_key(&leaf.label()), "{leaf:?} emitted");
    }
    // Two drivers, then the doc table words of db_to_linear, exp2, tan, reciprocal.
    assert_eq!(harness.code.word_count(), 2 * 2 + 2 + 27 + 54 + 20);
    for (leaf, x) in [(Leaf::DbToLinear, -6.0), (Leaf::Tan, 2.0)] {
        assert_eq!(
            harness.run(leaf, x, Arithmetic::HostF32).0,
            leaf.host(x).to_bits()
        );
    }
}

/// Exact values that parameter laws rely on (0 dB is unity gain).
#[test]
fn exact_anchors() {
    assert_eq!(math::db_to_linear(0.0), 1.0);
    assert_eq!(math::linear_to_db(1.0), 0.0);
    for k in -126..=126 {
        assert_eq!(math::exp2(k as f32), 2f32.powi(k), "exp2({k})");
        assert_eq!(math::log2(2f32.powi(k)), k as f32, "log2(2^{k})");
    }
    assert_eq!(math::log2(0.0), -126.0);
    assert_eq!(
        (math::sin(0.0), math::cos(0.0), math::tan(0.0)),
        (0.0, 1.0, 0.0)
    );
}

fn exact(leaf: Leaf, x: f32) -> f64 {
    let x = f64::from(x);
    match leaf {
        Leaf::Exp2 => x.exp2(),
        Leaf::DbToLinear => 10f64.powf(x / 20.0),
        Leaf::Log2 => x.log2(),
        Leaf::LinearToDb => 20.0 * x.log10(),
        Leaf::Sin => x.sin(),
        Leaf::Cos => x.cos(),
        Leaf::Tan => x.tan(),
        Leaf::Reciprocal => 1.0 / x,
    }
}

/// |y − exact| in units of the f32 ulp at the exact value.
fn ulp_error(y: f32, exact: f64) -> f64 {
    if exact == 0.0 {
        return if y == 0.0 { 0.0 } else { f64::INFINITY };
    }
    let exponent = exact.abs().log2().floor().max(-126.0);
    (f64::from(y) - exact).abs() / (exponent - 23.0).exp2()
}

/// Inputs inside the domain, where the leaf approximates the exact function.
fn in_domain(leaf: Leaf, x: f32) -> bool {
    let normal = x == 0.0 || x.is_normal();
    normal
        && match leaf {
            Leaf::Exp2 => x.abs() <= math::EXP2_LIMIT,
            Leaf::DbToLinear => x.abs() <= 758.0,
            Leaf::Log2 | Leaf::LinearToDb => x >= f32::MIN_POSITIVE,
            Leaf::Sin | Leaf::Cos | Leaf::Tan => x.abs() <= math::TRIG_LIMIT,
            Leaf::Reciprocal => (math::RECIPROCAL_MIN..=math::RECIPROCAL_MAX).contains(&x.abs()),
        }
}

fn max_ulp(leaf: Leaf, inputs: impl Iterator<Item = f32>) -> (f64, f32) {
    inputs
        .filter(|&x| in_domain(leaf, x))
        .map(|x| (ulp_error(leaf.host(x), exact(leaf, x)), x))
        .fold((0.0, 0.0), |a, b| if b.0 > a.0 { b } else { a })
}

#[test]
fn twin_error_within_documented_bounds() {
    for (leaf, bound) in MAX_ULP {
        let (error, at) = max_ulp(leaf, sweep(leaf, 1 << 17).into_iter());
        assert!(error <= bound, "{leaf:?}: {error:.3} ulp at {at:e}");
    }
}

/// Exhaustive over every f32 in each domain (and |dB| ≤ 120 for
/// `db_to_linear`); prints the doc table column.
/// `cargo test -p tetrad-sharc --release --test math -- --ignored --nocapture`
#[test]
#[ignore = "exhaustive audit, minutes in release"]
fn error_table() {
    let rows: Vec<(Leaf, f32)> = Leaf::ALL
        .iter()
        .map(|&leaf| (leaf, f32::INFINITY))
        .chain([(Leaf::DbToLinear, 120.0)])
        .collect();
    for batch in rows.chunks(3) {
        std::thread::scope(|scope| {
            for &(leaf, limit) in batch {
                scope.spawn(move || {
                    let positive = (0..0x7f80_0000).map(f32::from_bits);
                    let all = positive.clone().chain(positive.map(|x| -x));
                    let (error, at) = max_ulp(leaf, all.filter(|x| x.abs() <= limit));
                    println!("{leaf:?} |x| <= {limit}: {error:.3} ulp at {at:e}");
                });
            }
        });
    }
}
