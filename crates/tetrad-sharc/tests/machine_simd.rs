//! The machine on the forms a qcdsp WaveNet frame kernel needs: Type 1a dual moves with float
//! multifunction computes, DAG2, SIMD and broadcast loads, circular buffers and the VISA rule for
//! loop tails. Words are built field by field from the PRM figures, never by an encoder, and
//! stock words (raw hex from the stock core 1 blobs) must equal the same fields.
use tetrad_sharc::isa48::{MODE1_BDCST1, MODE1_BDCST9, MODE1_CBUFEN, MODE1_PEYEN};
use tetrad_sharc::machine::{
    AddressUnit, Bus, Machine, MachineError, MemoryMap, ProgramMemory, Stop,
};

const ISA: u32 = 0x9_0000;
const VISA: u32 = 0x1c_0000;
/// Word-space data. PM data accesses (DAG2) use the same address space.
const X: u32 = 0xb_0000;
const W: u32 = 0xc_0000;
const OUT: u32 = 0xd_0000;
/// Byte-space data, like a heap arena.
const BYTES: u32 = 0x24_a800;

/// One Type 1a access: D (store), I, M (DAG2 for PM: I8-I15, M8-M15) and the data register.
#[derive(Clone, Copy)]
struct Dual {
    store: bool,
    i: u64,
    m: u64,
    reg: u64,
}

const fn load(reg: u64, i: u64, m: u64) -> Dual {
    Dual {
        store: false,
        i,
        m,
        reg,
    }
}

const fn store(reg: u64, i: u64, m: u64) -> Dual {
    Dual {
        store: true,
        i,
        m,
        reg,
    }
}

/// Type 1a, PRM figure 14.03.1: 001 47:45, DMD 44, DMI 43:41, DMM 40:38, PMD 37, DMDREG 36:33,
/// PMI 32:30, PMM 29:27, PMDREG 26:23, compute 22:0.
fn type1a(dm: Dual, pm: Dual, compute: u64) -> u64 {
    0b001 << 45
        | u64::from(dm.store) << 44
        | dm.i << 41
        | dm.m << 38
        | u64::from(pm.store) << 37
        | dm.reg << 33
        | (pm.i - 8) << 30
        | (pm.m - 8) << 27
        | pm.reg << 23
        | compute
}

const MUL_ADD: u64 = 0b01_1000;
const MUL_SUB: u64 = 0b01_1001;

/// Float multiplier and ALU (PRM 21.16): 1 at 22, opcode 21:16, Fm 15:12, Fa 11:8, then the
/// inputs `[Fxm (F0-F3), Fym (F4-F7), Fxa (F8-F11), Fya (F12-F15)]` at 7:6, 5:4, 3:2, 1:0.
fn mul_alu(opcode: u64, fm: u64, fa: u64, [xm, ym, xa, ya]: [u64; 4]) -> u64 {
    1 << 22
        | opcode << 16
        | fm << 12
        | fa << 8
        | xm << 6
        | (ym - 4) << 4
        | (xa - 8) << 2
        | (ya - 12)
}

/// Multiplier and dual add/subtract (PRM 21.15): opcode 11 at 21:20, Fs at 19:16.
fn mul_dual(fs: u64, fm: u64, fa: u64, inputs: [u64; 4]) -> u64 {
    mul_alu(0b11_0000 | fs, fm, fa, inputs)
}

const FLOAT_ADD: u64 = 0x081;
const FLOAT_MUL: u64 = 0x130;

/// Single-function compute: CU and opcode 21:12, Rn 11:8, Rx 7:4, Ry 3:0.
fn single(opcode: u64, n: u64, x: u64, y: u64) -> u64 {
    opcode << 12 | n << 8 | x << 4 | y
}

/// Type 2a, COND = 31 (true) at 37:33.
fn type2a(compute: u64) -> u64 {
    0x01 << 40 | 31 << 33 | compute
}

/// One Type 3a access, PRM figure 14.13.1 and ACCESS table 14.14: U (post-modify), I, M (G = 1
/// for PM: I8-I15, M8-M15), D (store) and the ureg code.
#[derive(Clone, Copy)]
struct Single {
    post: bool,
    i: u64,
    m: u64,
    store: bool,
    ureg: u64,
}

/// Type 3a: 010 47:45, U 44, I 43:41, M 40:38, COND 37:33 (31), G 32, D 31, L 30 (0),
/// UREG 29:23, compute 22:0.
fn type3a(access: Single, compute: u64) -> u64 {
    let pm = access.i >= 8;
    0b010 << 45
        | u64::from(access.post) << 44
        | (access.i % 8) << 41
        | (access.m % 8) << 38
        | 31 << 33
        | u64::from(pm) << 32
        | u64::from(access.store) << 31
        | access.ureg << 23
        | compute
}

/// Type 12a immediate, PRM figure 15.20.1: 0x0c 47:40, count 39:24, mode 23 (1 = F1-active),
/// RELADDR 22:0 to the last body instruction.
fn type12a(count: u64, f1: bool, reladdr: u64) -> u64 {
    0x0c << 40 | count << 24 | u64::from(f1) << 23 | reladdr
}

/// Type 18a, PRM figure 17.02.1: 0x14 47:40, BOP 39:37 (0 SET, 1 CLR), SREG 35:32 (MODE1 = 2).
fn mode1(clear: bool, mask: u32) -> u64 {
    0x14 << 40 | u64::from(clear) << 37 | 2 << 32 | u64::from(mask)
}

/// 48-bit words as VISA parcels, high parcel first.
fn parcels(words: &[u64]) -> Vec<u16> {
    words
        .iter()
        .flat_map(|&w| [(w >> 32) as u16, (w >> 16) as u16, w as u16])
        .collect()
}

fn memory() -> MemoryMap {
    let mut memory = MemoryMap::default();
    for base in [X, W, OUT] {
        memory.add(base, AddressUnit::Word, 64).unwrap();
    }
    memory.add(BYTES, AddressUnit::Byte, 64).unwrap();
    memory
}

/// Normal word `index` of the array at `base`, in either address space.
fn at(base: u32, index: usize) -> u32 {
    base + AddressUnit::of(base).scale() * index as u32
}

fn fill(memory: &mut MemoryMap, base: u32, values: impl IntoIterator<Item = f32>) {
    for (index, value) in values.into_iter().enumerate() {
        memory.write(at(base, index), value.to_bits()).unwrap();
    }
}

/// Runs `program(unit)` from `machine` and `memory` as ISA48 words (unit 1) and as VISA parcels
/// (unit 3: loop offsets count parcels). Both runs must agree; returns the ISA run.
fn run(
    program: impl Fn(u64) -> Vec<u64>,
    machine: &Machine,
    memory: &MemoryMap,
) -> (Machine, MemoryMap) {
    let [isa, visa] = [(ISA, 1), (VISA, 3)].map(|(origin, unit)| {
        let words = program(unit);
        let mut code = ProgramMemory::default();
        if unit == 1 {
            code.load_words(origin, &words).unwrap();
        } else {
            code.load_visa(origin, &parcels(&words)).unwrap();
        }
        let (mut machine, mut memory) = (machine.clone(), memory.clone());
        machine.set_pc(origin);
        let end = origin + unit as u32 * words.len() as u32;
        assert_eq!(
            machine.run(&code, &mut memory, Some(end), 10_000),
            Ok(Stop::Breakpoint(end))
        );
        (machine, memory)
    });
    assert_eq!(
        (isa.0.r, isa.0.s, isa.0.i, isa.0.mode1),
        (visa.0.r, visa.0.s, visa.0.i, visa.0.mode1)
    );
    assert_eq!(isa.1, visa.1);
    isa
}

fn f(bits: u32) -> f32 {
    f32::from_bits(bits)
}

#[test]
fn stock_words_are_the_prm_fields() {
    assert_eq!(
        type1a(
            load(0, 4, 4),
            load(4, 13, 12),
            mul_alu(MUL_ADD, 12, 8, [0, 5, 8, 12])
        ),
        0x2901_6258_c810
    );
    assert_eq!(
        type1a(
            store(10, 7, 6),
            store(9, 12, 8),
            mul_dual(3, 1, 8, [0, 4, 9, 12])
        ),
        0x3fb5_04f3_1804
    );
    assert_eq!(type1a(load(0, 0, 4), store(0, 8, 12), 0), 0x2120_2000_0000);
    let post = Single {
        post: true,
        i: 2,
        m: 4,
        store: false,
        ureg: 0,
    };
    assert_eq!(type3a(post, single(FLOAT_MUL, 8, 0, 4)), 0x553e_0013_0804);
    assert_eq!(type12a(0x1f, true, 0x28), 0x0c00_1f80_0028);
    assert_eq!(mode1(false, MODE1_PEYEN), 0x1402_0020_0000);
    assert_eq!(mode1(true, MODE1_PEYEN), 0x1422_0020_0000);
    assert_eq!(mode1(false, MODE1_PEYEN | MODE1_BDCST9), 0x1402_0060_0000);
}

/// Stock Type 1a and 3a words run with parallel semantics: computes and stores read the
/// registers before the instruction's loads and results land, and both I registers post-modify.
#[test]
fn stock_dual_moves_and_multifunction_read_before_write() {
    // F12 = F0 * F5, F8 = F8 + F12, R0 = DM(I4, M4), R4 = PM(I13, M12).
    let mut machine = Machine::reset();
    let mut data = memory();
    [machine.r[0], machine.r[5]] = [2.0_f32.to_bits(), 3.0_f32.to_bits()];
    [machine.r[8], machine.r[12]] = [10.0_f32.to_bits(), 100.0_f32.to_bits()];
    (machine.i[4], machine.m[4], machine.i[13], machine.m[12]) = (X, 1, W + 5, -1);
    fill(&mut data, X, [5.0]);
    data.write(W + 5, 7.0_f32.to_bits()).unwrap();
    let (out, _) = run(|_| vec![0x2901_6258_c810], &machine, &data);
    assert_eq!(
        [out.r[12], out.r[8], out.r[0], out.r[4]].map(f),
        [6.0, 110.0, 5.0, 7.0]
    );
    assert_eq!((out.i[4], out.i[13]), (X + 1, W + 4));
    // Hand-encoded: F12 = F0 * F5, F8 = F8 - F12, R1 = DM(I4, M4), R5 = PM(I13, M12). The
    // multiply reads the F5 that the PM load replaces; dual-move loads may hit compute inputs.
    let word = type1a(
        load(1, 4, 4),
        load(5, 13, 12),
        mul_alu(MUL_SUB, 12, 8, [0, 5, 8, 12]),
    );
    let (out, _) = run(|_| vec![word], &machine, &data);
    assert_eq!(
        [out.r[12], out.r[8], out.r[1], out.r[5]].map(f),
        [6.0, -90.0, 5.0, 7.0]
    );
    assert!(out.flags.negative);

    // F1 = F0 * F4, F8 = F9 + F12, F3 = F9 - F12, DM(I7, M6) = R10, PM(I12, M8) = R9.
    let mut machine = Machine::reset();
    [machine.r[0], machine.r[4]] = [2.0_f32.to_bits(), 3.0_f32.to_bits()];
    [machine.r[9], machine.r[12]] = [10.0_f32.to_bits(), 4.0_f32.to_bits()];
    [machine.r[10], machine.r[3]] = [0x1111, 0x3333];
    (machine.i[7], machine.m[6], machine.i[12], machine.m[8]) = (X + 4, -1, W, 2);
    let (out, data) = run(|_| vec![0x3fb5_04f3_1804], &machine, &memory());
    assert_eq!([out.r[1], out.r[8], out.r[3]].map(f), [6.0, 14.0, 6.0]);
    assert!(!out.flags.negative, "AN: 14 < 0 OR 6 < 0");
    assert_eq!(
        (data.peek(X + 4), data.peek(W)),
        (Ok(0x1111), Ok(10.0_f32.to_bits()))
    );
    assert_eq!((out.i[7], out.i[12]), (X + 3, W + 2));

    // R0 = DM(I0, M4), PM(I8, M12) = R0: the store takes the old R0.
    let mut machine = Machine::reset();
    machine.r[0] = 0x77;
    (machine.i[0], machine.m[4], machine.i[8], machine.m[12]) = (X + 8, 1, W + 8, 1);
    let mut data = memory();
    data.write(X + 8, 0x99).unwrap();
    let (out, data) = run(|_| vec![0x2120_2000_0000], &machine, &data);
    assert_eq!((out.r[0], data.peek(W + 8)), (0x99, Ok(0x77)));

    // F8 = F0 * F4, R0 = DM(I2, M4): the multiply reads F0 before the load.
    let mut machine = Machine::reset();
    [machine.r[0], machine.r[4]] = [2.0_f32.to_bits(), 3.0_f32.to_bits()];
    (machine.i[2], machine.m[4]) = (X + 12, 1);
    let mut data = memory();
    fill(&mut data, X + 12, [9.0]);
    let (out, _) = run(|_| vec![0x553e_0013_0804], &machine, &data);
    assert_eq!([out.r[8], out.r[0]].map(f), [6.0, 9.0]);
    assert_eq!(out.i[2], X + 13);
}

/// Type 2a also takes a multifunction compute (stock `013e00598c70`, 295 sites).
#[test]
fn type2a_runs_a_multifunction_compute() {
    let word = type2a(mul_alu(MUL_SUB, 8, 12, [1, 7, 8, 12]));
    assert_eq!(word, 0x013e_0059_8c70);
    let mut machine = Machine::reset();
    [machine.r[1], machine.r[7]] = [2.0_f32.to_bits(), 3.0_f32.to_bits()];
    [machine.r[8], machine.r[12]] = [10.0_f32.to_bits(), 4.0_f32.to_bits()];
    let (out, _) = run(|_| vec![word], &machine, &memory());
    assert_eq!(
        [out.r[8], out.r[12]].map(f),
        [6.0, 6.0],
        "F8 = 2 * 3, F12 = 10 - 4"
    );
}

/// The pipelined dot product a code generator emits: prime the loads, one Type 1a per product
/// (`F12 = F0 * F4, F8 = F8 + F12` with the next DM and PM loads) in an F1-active counter loop,
/// then the last add. `mode` is set before and cleared after.
fn dot_product(unit: u64, count: u64, dm: Dual, pm: Dual, mode: u32) -> Vec<u64> {
    let mut words = vec![mode1(false, mode)];
    words.extend([
        type1a(dm, pm, 0),
        type12a(count, true, unit),
        type1a(dm, pm, mul_alu(MUL_ADD, 12, 8, [0, 4, 8, 12])),
        type2a(single(FLOAT_ADD, 8, 8, 12)),
        mode1(true, mode),
    ]);
    words
}

/// The machine's order: `acc + previous product`, as host f32.
fn reference(x: &[f32], w: &[f32]) -> f32 {
    let (mut acc, mut product) = (0.0_f32, 0.0_f32);
    for (x, w) in x.iter().zip(w) {
        acc += product;
        product = x * w;
    }
    acc + product
}

fn signal(seed: f32, count: usize) -> Vec<f32> {
    (0..count)
        .map(|k| (k as f32 * 0.37 + seed).sin() * (1.0 + seed))
        .collect()
}

fn interleave(a: &[f32], b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).flat_map(|(a, b)| [*a, *b]).collect()
}

const COUNT: usize = 16;

/// SISD: activations through DM/DAG1 (I0), weights through PM/DAG2 (I8).
#[test]
fn sisd_dot_product_reads_weights_through_dag2() {
    let (x, w) = (signal(0.1, COUNT + 1), signal(0.7, COUNT + 1));
    let mut data = memory();
    fill(&mut data, X, x.iter().copied());
    fill(&mut data, W, w.iter().copied());
    let mut machine = Machine::reset();
    (machine.i[0], machine.m[0], machine.i[8], machine.m[8]) = (X, 1, W, 1);
    let program = |unit| dot_product(unit, COUNT as u64, load(0, 0, 0), load(4, 8, 8), 0);
    let (out, _) = run(program, &machine, &data);
    assert_eq!(f(out.r[8]), reference(&x[..COUNT], &w[..COUNT]));
    assert_eq!(
        (out.i[0], out.i[8]),
        (X + COUNT as u32 + 1, W + COUNT as u32 + 1)
    );
}

/// SIMD: two outputs per instruction. Each PE reads its own interleaved activation and weight
/// (PEy at I + 1 normal word: +1 in word space, +4 in byte space), and the closing Type 2a add
/// runs on both PEs.
#[test]
fn simd_dot_product_computes_two_outputs_per_instruction() {
    let (a, b) = (signal(0.1, COUNT + 1), signal(0.2, COUNT + 1));
    let (u, v) = (signal(0.7, COUNT + 1), signal(0.9, COUNT + 1));
    for activations in [X, BYTES] {
        let mut data = memory();
        fill(&mut data, activations, interleave(&a, &b));
        fill(&mut data, W, interleave(&u, &v));
        let mut machine = Machine::reset();
        (machine.i[0], machine.m[0], machine.i[8], machine.m[8]) = (activations, 2, W, 2);
        let program = |unit| {
            dot_product(
                unit,
                COUNT as u64,
                load(0, 0, 0),
                load(4, 8, 8),
                MODE1_PEYEN,
            )
        };
        let (out, _) = run(program, &machine, &data);
        assert_eq!(f(out.r[8]), reference(&a[..COUNT], &u[..COUNT]));
        assert_eq!(f(out.s[8]), reference(&b[..COUNT], &v[..COUNT]));
        assert_eq!(out.mode1 & MODE1_PEYEN, 0);
        assert_eq!(out.i[0], at(activations, 2 * (COUNT + 1)));
    }
}

/// Broadcast: BDCST1 gives both PEs the same input sample through I1 while DAG2 reads
/// interleaved weights (two output channels of one input); BDCST9 gives both PEs the same weight
/// through I9 while DAG1 reads interleaved activations.
#[test]
fn broadcast_dot_products_share_one_operand() {
    let (x, w) = (signal(0.3, COUNT + 1), signal(0.5, COUNT + 1));
    let (u, v) = (signal(0.7, COUNT + 1), signal(0.9, COUNT + 1));
    let (a, b) = (signal(0.1, COUNT + 1), signal(0.2, COUNT + 1));

    let mut data = memory();
    fill(&mut data, BYTES, x.iter().copied());
    fill(&mut data, W, interleave(&u, &v));
    let mut machine = Machine::reset();
    (machine.i[1], machine.m[1], machine.i[8], machine.m[8]) = (BYTES, 1, W, 2);
    let mode = MODE1_PEYEN | MODE1_BDCST1;
    let program = |unit| dot_product(unit, COUNT as u64, load(0, 1, 1), load(4, 8, 8), mode);
    let (out, _) = run(program, &machine, &data);
    assert_eq!(f(out.r[8]), reference(&x[..COUNT], &u[..COUNT]));
    assert_eq!(f(out.s[8]), reference(&x[..COUNT], &v[..COUNT]));

    let mut data = memory();
    fill(&mut data, X, interleave(&a, &b));
    fill(&mut data, W, w.iter().copied());
    let mut machine = Machine::reset();
    (machine.i[0], machine.m[0], machine.i[9], machine.m[9]) = (X, 2, W, 1);
    let mode = MODE1_PEYEN | MODE1_BDCST9;
    let program = |unit| dot_product(unit, COUNT as u64, load(0, 0, 0), load(4, 9, 9), mode);
    let (out, _) = run(program, &machine, &data);
    assert_eq!(f(out.r[8]), reference(&a[..COUNT], &w[..COUNT]));
    assert_eq!(f(out.s[8]), reference(&b[..COUNT], &w[..COUNT]));
}

/// The planned convolution kernel: the input history is a ring on I1 (byte space, CBUFEN) whose
/// samples BDCST1 broadcasts to both PEs, while DAG2 reads the interleaved weights of two output
/// channels. Starting mid-ring, the reads wrap.
#[test]
fn broadcast_kernel_reads_a_history_ring() {
    const LENGTH: usize = COUNT + 1;
    let history = signal(0.4, LENGTH);
    let (u, v) = (signal(0.7, LENGTH), signal(0.9, LENGTH));
    let mut data = memory();
    fill(&mut data, BYTES, history.iter().copied());
    fill(&mut data, W, interleave(&u, &v));
    let mut machine = Machine::reset();
    machine.mode1 |= MODE1_CBUFEN;
    (machine.b[1], machine.l[1]) = (BYTES, LENGTH as u32);
    (machine.i[1], machine.m[1], machine.i[8], machine.m[8]) = (at(BYTES, 5), 1, W, 2);
    let mode = MODE1_PEYEN | MODE1_BDCST1;
    let program = |unit| dot_product(unit, COUNT as u64, load(0, 1, 1), load(4, 8, 8), mode);
    let (out, _) = run(program, &machine, &data);
    let x: Vec<f32> = (0..COUNT).map(|k| history[(5 + k) % LENGTH]).collect();
    assert_eq!(f(out.r[8]), reference(&x, &u[..COUNT]));
    assert_eq!(f(out.s[8]), reference(&x, &v[..COUNT]));
    assert_eq!(out.i[1], at(BYTES, 5), "a full turn of the ring");
}

/// Type 3a with G = 1 (`PM(Ic, Md)`): SISD, SIMD (complementary register at I + 1, S2 as the
/// explicit register moves R2 implicitly) and BDCST9 in SISD (both banks from one address).
#[test]
fn type3a_pm_transfers_follow_simd_and_broadcast() {
    let load_r1 = type3a(
        Single {
            post: true,
            i: 9,
            m: 10,
            store: false,
            ureg: 1,
        },
        0,
    );
    let store_s2 = type3a(
        Single {
            post: true,
            i: 10,
            m: 10,
            store: true,
            ureg: 0x52,
        },
        0,
    );
    let mut data = memory();
    for word in 0..4 {
        data.write(W + word, 0x100 + word).unwrap();
    }
    let mut machine = Machine::reset();
    (machine.i[9], machine.i[10], machine.m[10]) = (W, OUT, 3);
    (machine.r[2], machine.s[2]) = (0x22, 0x52);
    for (mode, s1, out1) in [
        (0, 0, 0),
        (MODE1_PEYEN, 0x101, 0x22),
        (MODE1_BDCST9, 0x100, 0),
    ] {
        machine.mode1 = mode;
        let (out, data) = run(|_| vec![load_r1, store_s2], &machine, &data);
        assert_eq!((out.r[1], out.s[1]), (0x100, s1), "mode {mode:#x}");
        assert_eq!((data.peek(OUT), data.peek(OUT + 1)), (Ok(0x52), Ok(out1)));
        assert_eq!((out.i[9], out.i[10]), (W + 3, OUT + 3));
    }
    // A post-modify store may not source its own DAG (UREGXDAG2): `PM(I8, M8) = I9`.
    let own_dag = type3a(
        Single {
            post: true,
            i: 8,
            m: 8,
            store: true,
            ureg: 0x19,
        },
        0,
    );
    let mut program = ProgramMemory::default();
    program.load_words(ISA, &[own_dag]).unwrap();
    let mut machine = Machine::reset();
    machine.set_pc(ISA);
    assert_eq!(
        machine.step(&program, &mut memory()),
        Err(MachineError::UnsupportedWord {
            pc: ISA,
            word: own_dag
        })
    );
}

/// `R0 = R1 + R2, S0 = DM(I0, M0)` writes two registers in SISD; in SIMD the compute also writes
/// S0, a double write the PRM does not define.
#[test]
fn type3a_sn_load_beside_an_rn_compute_is_sisd_only() {
    let word = type3a(
        Single {
            post: true,
            i: 0,
            m: 0,
            store: false,
            ureg: 0x50,
        },
        single(FLOAT_ADD, 0, 1, 2),
    );
    let mut data = memory();
    data.write(X, 0x1234).unwrap();
    let mut machine = Machine::reset();
    (machine.i[0], machine.r[0]) = (X, 0x77);
    let (out, _) = run(|_| vec![word], &machine, &data);
    assert_eq!((out.r[0], out.s[0]), (0, 0x1234));

    let mut program = ProgramMemory::default();
    program.load_words(ISA, &[word]).unwrap();
    machine.mode1 = MODE1_PEYEN;
    machine.set_pc(ISA);
    assert_eq!(
        machine.step(&program, &mut data),
        Err(MachineError::SimdDoubleWrite(ISA))
    );
}

/// DAG2 in Types 15a (`R3 = PM(5, I11)`, figure 16.10.1: G 44), 16a (`PM(I12, M13) = data32`,
/// figure 16.12.1: G 37) and 19a (`MODIFY(I14, -2) (NW)`, figure 17.04.1: G 38).
#[test]
fn dag2_offset_immediate_and_modify_forms() {
    let offset_load = 0b101 << 45 | 1 << 44 | 3 << 41 | 3 << 32 | 5;
    let immediate_store = 0b1001 << 44 | 4 << 41 | 5 << 38 | 1 << 37 | 0x1234;
    let modify = 0b00_0101 << 42 | 0b01 << 40 | 1 << 39 | 1 << 38 | 6 << 32 | 0xffff_fffe;
    let mut data = memory();
    data.write(W + 5, 0xabcd).unwrap();
    let mut machine = Machine::reset();
    (machine.i[11], machine.i[12], machine.m[13], machine.i[14]) = (W, OUT, 2, BYTES + 16);
    let (out, data) = run(
        |_| vec![offset_load, immediate_store, modify],
        &machine,
        &data,
    );
    assert_eq!((out.r[3], out.i[11]), (0xabcd, W));
    assert_eq!((data.peek(OUT), out.i[12]), (Ok(0x1234), OUT + 2));
    assert_eq!(out.i[14], BYTES + 8, "NW modify scales by 4 in byte space");
}

/// A ring of five words holding 0..5. `I1` (or `I9`) starts at `B + 3` and seven post-modify
/// loads by `step` each store the value read to OUT. Loads through DM with I1/M1 or through PM
/// with I9/M9.
fn ring_program(unit: u64, dag2: bool) -> Vec<u64> {
    let (i, m) = if dag2 { (9, 9) } else { (1, 1) };
    let read = Single {
        post: true,
        i,
        m,
        store: false,
        ureg: 0,
    };
    let write = Single {
        post: true,
        i: 2,
        m: 2,
        store: true,
        ureg: 0,
    };
    vec![
        type12a(7, true, 2 * unit),
        type3a(read, 0),
        type3a(write, 0),
    ]
}

fn ring_machine(base: u32, dag2: bool, step: i32, mode1: u32) -> Machine {
    let mut machine = Machine::reset();
    let index = if dag2 { 9 } else { 1 };
    (machine.b[index], machine.l[index]) = (base, 5);
    (machine.i[index], machine.m[index]) = (at(base, 3), step);
    (machine.i[2], machine.m[2]) = (OUT, 1);
    machine.mode1 |= mode1;
    machine
}

/// PRM `.08.32`: with CBUFEN, post-modify wraps by L (scaled by 4 for a byte-space pointer)
/// for positive and negative modifiers, on DAG1 and DAG2.
#[test]
fn circular_buffers_wrap_post_modify_in_word_and_byte_space() {
    for (base, dag2, step) in [
        (X, false, 1),
        (X, false, -2),
        (BYTES, false, 1),
        (BYTES, false, -2),
        (BYTES, true, -2),
        (W, true, 1),
    ] {
        let mut data = memory();
        for k in 0..5 {
            data.write(at(base, k), k as u32).unwrap();
        }
        let machine = ring_machine(base, dag2, step, MODE1_CBUFEN);
        let (out, data) = run(|unit| ring_program(unit, dag2), &machine, &data);
        let mut index = 3_i32;
        for n in 0..7 {
            assert_eq!(data.peek(OUT + n), Ok(index as u32), "{base:#x} by {step}");
            index = (index + step).rem_euclid(5);
        }
        let ring = if dag2 { 9 } else { 1 };
        assert_eq!(out.i[ring], at(base, index as usize));
    }
}

/// CBUFEN clear: post-modify ignores B and L, but MODIFY still wraps (PRM `.08.16`). An |M|
/// of at least L is refused.
#[test]
fn ring_wrap_needs_cbufen_except_for_modify_and_a_short_modifier() {
    let machine = ring_machine(X, false, 1, 0);
    let (out, _) = run(|unit| ring_program(unit, false), &machine, &memory());
    assert_eq!(out.i[1], X + 10);

    // MODIFY(I1, 4) (NW): B + 3 + 4 wraps to B + 2.
    let modify = 0x15 << 40 | 1 << 39 | 1 << 32 | 4;
    let (out, _) = run(|_| vec![modify], &ring_machine(X, false, 1, 0), &memory());
    assert_eq!(out.i[1], X + 2);

    let mut program = ProgramMemory::default();
    program.load_words(ISA, &ring_program(1, false)).unwrap();
    let mut machine = ring_machine(X, false, 5, MODE1_CBUFEN);
    machine.set_pc(ISA);
    assert_eq!(
        machine.run(&program, &mut memory(), None, 10),
        Err(MachineError::CircularBuffer {
            pc: ISA + 1,
            index: 1
        })
    );
}

/// PRM `.06.062`: the last eleven instructions of a loop body must be 48-bit. Twelve
/// instructions with a 16-bit NOP first run; a 16-bit NOP sixth from the end is refused.
#[test]
fn visa_loops_need_48_bit_tails() {
    const NOP16: u16 = 0x0001;
    let nops = |count: usize| vec![0_u16; 3 * count];
    let program = |body: Vec<u16>, end: u64| {
        let mut parcels = parcels(&[type12a(2, true, end)]);
        parcels.extend(body);
        parcels
    };
    let valid = program([vec![NOP16], nops(11)].concat(), 3 + 1 + 30);
    let invalid = program([nops(5), vec![NOP16], nops(5)].concat(), 3 + 16 + 12);
    for (parcels, expected) in [
        (valid, Ok(Stop::Breakpoint(VISA + 3 + 34))),
        (invalid, Err(MachineError::VisaLoopTail(VISA + 3 + 15))),
    ] {
        let mut code = ProgramMemory::default();
        code.load_visa(VISA, &parcels).unwrap();
        let mut machine = Machine::reset();
        machine.set_pc(VISA);
        let end = VISA + parcels.len() as u32;
        assert_eq!(machine.run(&code, &mut memory(), Some(end), 100), expected);
    }
}
