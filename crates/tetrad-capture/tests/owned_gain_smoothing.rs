use tetrad_capture::gain::emitted::{CompiledGainSmoother, Error, Layout, SAMPLES};
use tetrad_capture::reference::{GAIN_SMOOTHING, GainSmoother, gain_target};
use tetrad_sharc::address::{AddressError, DmWordAddress32, PmWordAddress24};
use tetrad_sharc::isa48::{MODE1_CBUFEN, MODE1_RND32, MODE1_TRUNCATE};
use tetrad_sharc::register::{DataReg, FloatReg, UniversalReg};
use tetrad_sharc::simulator::{DmMemory, ScalarState, SimulationError};
use tetrad_sharc::{EmitError, Instruction48};

const BASE: u32 = 0x4000;
const N: usize = SAMPLES as usize;

fn dm(address: u32) -> DmWordAddress32 {
    DmWordAddress32::new(address)
}

/// input 0x4000, output 0x4020, current 0x4040, target 0x4041; canaries elsewhere.
fn compiled(origin: u32, base: u32) -> CompiledGainSmoother {
    let layout = Layout::new(dm(base), dm(base + 32), dm(base + 64), dm(base + 65)).unwrap();
    CompiledGainSmoother::compile(PmWordAddress24::new(origin).unwrap(), layout).unwrap()
}

fn memory(words: usize) -> DmMemory {
    let canaries = (0..words as u32).map(|i| 0x7fc0_0000 | i).collect(); // NaN canaries.
    DmMemory::new(dm(BASE), canaries).unwrap()
}

fn install(
    memory: &mut DmMemory,
    smoother: &CompiledGainSmoother,
    inputs: &[f32; N],
    gains: [f32; 2],
) {
    let [(input, _), _, (current, _), (target, _)] = smoother.layout().spans();
    for (offset, value) in inputs.iter().enumerate() {
        memory
            .write(input.checked_add(offset as u32).unwrap(), value.to_bits())
            .unwrap();
    }
    memory.write(current, gains[0].to_bits()).unwrap();
    memory.write(target, gains[1].to_bits()).unwrap();
}

fn read_outputs(memory: &DmMemory, smoother: &CompiledGainSmoother) -> [u32; N] {
    let output = smoother.layout().spans()[1].0;
    std::array::from_fn(|i| memory.read(output.checked_add(i as u32).unwrap()).unwrap())
}

/// Deterministic finite vectors including zero/subnormal exponents and both signs.
/// Exponent field stays below 0xfe so that gains below 2 cannot overflow.
fn vectors(seed: u32) -> [f32; N] {
    let mut x = seed;
    std::array::from_fn(|_| {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let exponent = (x >> 8) % 0xfe;
        f32::from_bits((x & 0x8000_0000) | (exponent << 23) | (x.rotate_left(7) & 0x7f_ffff))
    })
}

#[test]
fn emitted_blocks_match_separate_reference_across_explicit_caller_updates() {
    let smoother = compiled(0x2000, BASE);
    let mut state = ScalarState::default();
    let mut mem = memory(128);
    let mut reference = GainSmoother::from_state(0.25, 2.0).unwrap();
    let mut gains = [0.25f32, 2.0];
    let mut fused_differs = false;
    for block in 0..4u32 {
        let mut inputs = vectors(block * 0x9e37 + 1);
        match block {
            // Caller-explicit target update; current carries across the block boundary.
            1 => {
                reference.set_linear_target(0.5).unwrap();
                gains[1] = 0.5;
            }
            // Caller-explicit state: subnormal current/target, signed zeros and MAX.
            2 => {
                reference = GainSmoother::from_state(f32::from_bits(1), f32::from_bits(3)).unwrap();
                gains = [f32::from_bits(1), f32::from_bits(3)];
                inputs[..4].copy_from_slice(&[-0.0, 0.0, f32::MAX, -f32::MAX]);
            }
            3 => {
                reference = GainSmoother::from_state(-0.0, 0.0).unwrap();
                gains = [-0.0, 0.0];
            }
            _ => {}
        }
        install(&mut mem, &smoother, &inputs, gains);
        let current_before = reference.current();
        let mut expected = [0.0f32; N];
        reference.process_chunk(&inputs, &mut expected).unwrap();
        let mut probe = current_before;
        for _ in 0..N {
            let target_product = (1.0 - GAIN_SMOOTHING) * reference.target();
            let separate = GAIN_SMOOTHING * probe + target_product;
            fused_differs |=
                GAIN_SMOOTHING.mul_add(probe, target_product).to_bits() != separate.to_bits();
            probe = separate;
        }
        let writes_before = mem.writes().len();
        let outcome = smoother
            .simulate_atomic(&mut state, &mut mem, smoother.max_steps())
            .unwrap();
        assert_eq!((outcome.steps, outcome.stores), (230, 33));
        assert_eq!(&mem.writes()[writes_before..], smoother.publications());
        assert_eq!(
            read_outputs(&mem, &smoother),
            expected.map(f32::to_bits),
            "block {block}"
        );
        let [_, _, (current, _), (target, _)] = smoother.layout().spans();
        assert_eq!(mem.read(current).unwrap(), reference.current().to_bits());
        assert_eq!(mem.read(target).unwrap(), gains[1].to_bits());
        gains[0] = reference.current();
        if block == 3 {
            // +0 gain times -0.0 input keeps the IEEE sign; -0 current becomes +0.
            assert!(read_outputs(&mem, &smoother).contains(&0x8000_0000));
            assert_eq!(reference.current().to_bits(), 0);
        }
    }
    assert!(
        fused_differs,
        "vectors must distinguish fused from separate rounding"
    );
}

#[test]
fn immutable_words_top_pm_dm_bounds_and_register_preservation() {
    let smoother = compiled(0x2000, BASE);
    let code = smoother.code();
    assert_eq!(
        (code.word_count(), code.isa48_le_bytes().len()),
        (230, 1380)
    );
    assert_eq!(smoother.publications().len(), 33);
    let r = |index| DataReg::new(index).unwrap();
    let f = |index| FloatReg::new(index).unwrap();
    let head = [
        Instruction48::Immediate {
            dst: UniversalReg::Data(r(1)),
            value: 0x3f73_3333,
        },
        Instruction48::Immediate {
            dst: UniversalReg::Data(r(2)),
            value: 0x3f80_0000,
        },
        Instruction48::DmDirectLoad {
            dst: r(3),
            address: dm(BASE + 64),
        },
        Instruction48::DmDirectLoad {
            dst: r(4),
            address: dm(BASE + 65),
        },
        Instruction48::FloatSubtract {
            dst: f(5),
            lhs: f(2),
            rhs: f(1),
        },
        Instruction48::FloatMultiply {
            dst: f(6),
            lhs: f(1),
            rhs: f(3),
        },
        Instruction48::FloatMultiply {
            dst: f(7),
            lhs: f(5),
            rhs: f(4),
        },
        Instruction48::FloatAdd {
            dst: f(3),
            lhs: f(6),
            rhs: f(7),
        },
        Instruction48::DmDirectLoad {
            dst: r(0),
            address: dm(BASE),
        },
        Instruction48::FloatMultiply {
            dst: f(0),
            lhs: f(3),
            rhs: f(0),
        },
        Instruction48::DmDirectStore {
            src: r(0),
            address: dm(BASE + 32),
        },
    ];
    let tail = [
        Instruction48::DmDirectStore {
            src: r(0),
            address: dm(BASE + 63),
        },
        Instruction48::DmDirectStore {
            src: r(3),
            address: dm(BASE + 64),
        },
        Instruction48::Rts,
    ];
    for (word, instruction) in code.words().iter().zip(head) {
        assert_eq!(*word, instruction.encode().unwrap());
    }
    for (word, instruction) in code.words()[227..].iter().zip(tail) {
        assert_eq!(*word, instruction.encode().unwrap());
    }
    // Each sample repeats the same 7 arithmetic/transfer shapes at advancing addresses.
    for sample in 1..N {
        assert_eq!(code.words()[4 + 7 * sample..][..4], code.words()[4..8]);
    }

    let latest = 0xff_ffff - 229;
    assert_eq!(compiled(latest, BASE).code().words(), code.words());
    let layout = smoother.layout();
    assert!(matches!(
        CompiledGainSmoother::compile(PmWordAddress24::new(latest + 1).unwrap(), layout),
        Err(Error::Emit(EmitError::ProgramBounds { .. }))
    ));
    let spans = [(0, 32), (100, 32), (200, 1), (300, 1)];
    for first in 0..4 {
        for second in first + 1..4 {
            let mut starts = spans.map(|(start, _)| dm(start));
            starts[second] = dm(spans[first].0 + spans[first].1 - 1);
            assert_eq!(
                Layout::new(starts[0], starts[1], starts[2], starts[3]),
                Err(Error::Overlap { first, second })
            );
        }
    }
    let overflow = Err(Error::Address(AddressError::DmOverflow));
    assert_eq!(
        Layout::new(dm(u32::MAX - 31), dm(0), dm(64), dm(65)),
        overflow
    );
    assert_eq!(Layout::new(dm(0), dm(32), dm(64), dm(u32::MAX)), overflow);

    // Top-of-DM placement: every one-past endpoint is exactly representable.
    let top = u32::MAX - 66;
    let top_layout = Layout::new(dm(u32::MAX - 32), dm(top + 2), dm(top + 1), dm(top)).unwrap();
    let top_code =
        CompiledGainSmoother::compile(PmWordAddress24::new(latest).unwrap(), top_layout).unwrap();
    let mut top_memory = DmMemory::new(dm(top), vec![0x7fc0_0000; 67]).unwrap();
    install(&mut top_memory, &top_code, &vectors(7), [0.5, 1.5]);
    let mut state = ScalarState {
        data: std::array::from_fn(|i| 0xa500_0000 | i as u32),
        indices: [11; 8],
        modifiers: [-3; 8],
        bases: [13; 8],
        lengths: [17; 8],
        mode1: MODE1_RND32 | MODE1_CBUFEN,
        alu_zero: true,
    };
    let before = state.clone();
    top_code
        .simulate_atomic(&mut state, &mut top_memory, top_code.max_steps())
        .unwrap();
    assert_eq!(state.data[8..], before.data[8..]);
    assert_eq!(
        (
            state.indices,
            state.modifiers,
            state.bases,
            state.lengths,
            state.mode1
        ),
        (
            before.indices,
            before.modifiers,
            before.bases,
            before.lengths,
            before.mode1
        )
    );
    assert_eq!(top_memory.read(dm(u32::MAX)).unwrap(), 0x7fc0_0000);
}

#[test]
fn bad_inputs_and_late_failures_preserve_entire_transaction() {
    let smoother = compiled(0x2000, BASE);
    let [(input, _), (output, _), (current, _), (target, _)] = smoother.layout().spans();
    let mut clean = memory(128);
    install(&mut clean, &smoother, &vectors(3), [0.25, 2.0]);
    clean.write(dm(BASE + 100), 0x1234_5678).unwrap(); // Prior caller log entry.
    let clean_state = ScalarState {
        data: [0x55; 16],
        ..ScalarState::default()
    };

    let last = input.checked_add(31).unwrap();
    let late_overflow = output.checked_add(31).unwrap();
    // (MODE1, words written over the clean fixture, step budget, expected error).
    let mut late = vec![(current, 4.0), (target, 4.0)];
    late.extend((0..SAMPLES).map(|i| (input.checked_add(i).unwrap(), 1.0)));
    late.push((last, f32::MAX)); // Gain 4 overflows on the last sample only.
    let mut cases = vec![
        (0, vec![], 230, Error::Modes(0)),
        (
            MODE1_RND32 | MODE1_TRUNCATE,
            vec![],
            230,
            Error::Modes(MODE1_RND32 | MODE1_TRUNCATE),
        ),
        (
            MODE1_RND32,
            vec![],
            229,
            Error::Simulation(SimulationError::StepBudget(229)),
        ),
        (
            MODE1_RND32,
            late,
            230,
            Error::NonFiniteMemory {
                address: late_overflow,
            },
        ),
    ];
    for (address, value) in [
        (last, f32::NAN),
        (last, f32::INFINITY),
        (current, f32::NAN),
        (current, f32::NEG_INFINITY),
        (target, f32::INFINITY),
        (target, f32::NAN),
    ] {
        cases.push((
            MODE1_RND32,
            vec![(address, value)],
            230,
            Error::NonFiniteMemory { address },
        ));
    }
    for address in [current, target] {
        cases.push((
            MODE1_RND32,
            vec![(address, -1.0)],
            230,
            Error::NegativeGain { address },
        ));
    }
    for (mode1, writes, budget, error) in cases {
        let mut state = ScalarState {
            mode1,
            ..clean_state.clone()
        };
        let mut mem = clean.clone();
        for (address, value) in writes {
            mem.write(address, value.to_bits()).unwrap();
        }
        let (state_before, mem_before) = (state.clone(), mem.clone());
        assert_eq!(
            smoother.simulate_atomic(&mut state, &mut mem, budget),
            Err(error.clone())
        );
        assert_eq!((state, mem), (state_before, mem_before), "{error:?}");
    }
    // The reference rejects the same late overflow and also keeps its state.
    let mut reference = GainSmoother::from_state(4.0, 4.0).unwrap();
    let mut inputs = [1.0; N];
    inputs[31] = f32::MAX;
    assert!(reference.process_chunk(&inputs, &mut [0.0; N]).is_err());
    assert_eq!(reference.current(), 4.0);

    // A missing target word fails before staging.
    let mut short = DmMemory::new(dm(BASE), clean.words()[..65].to_vec()).unwrap();
    let short_before = short.clone();
    let mut state = clean_state.clone();
    assert_eq!(
        smoother.simulate_atomic(&mut state, &mut short, 230),
        Err(Error::Simulation(SimulationError::MemoryBounds {
            address: target.get(),
            words: 1
        }))
    );
    assert_eq!((state, short), (clean_state, short_before));
}

#[test]
fn construction_zero_state_normalized_target_and_instances_are_isolated() {
    let first = compiled(0x2000, BASE);
    let second = compiled(0x3000, BASE + 66);
    assert_eq!(first.code().words(), compiled(0x2000, BASE).code().words());
    let mut mem = memory(140);
    let mut state = ScalarState::default();
    // Construction-zero smoother: outputs are signed zeros and current stays zero.
    let mut zero = GainSmoother::default();
    install(
        &mut mem,
        &first,
        &vectors(11),
        [zero.current(), zero.target()],
    );
    let mut expected = [0.0f32; N];
    zero.process_chunk(&vectors(11), &mut expected).unwrap();
    first.simulate_atomic(&mut state, &mut mem, 230).unwrap();
    assert_eq!(read_outputs(&mem, &first), expected.map(f32::to_bits));
    assert!(expected.iter().all(|value| *value == 0.0));

    // Host gain law selects the linear target; the compiled code only reads DM.
    zero.set_normalized_target(0.25).unwrap();
    assert_eq!(zero.target(), gain_target(0.25).unwrap());
    let mut other = GainSmoother::from_state(1.0, 0.125).unwrap();
    install(
        &mut mem,
        &first,
        &vectors(12),
        [zero.current(), zero.target()],
    );
    install(
        &mut mem,
        &second,
        &vectors(13),
        [other.current(), other.target()],
    );
    for round in 0..2 {
        let mut a = [0.0f32; N];
        let mut b = [0.0f32; N];
        zero.process_chunk(&vectors(12), &mut a).unwrap();
        other.process_chunk(&vectors(13), &mut b).unwrap();
        first.simulate_atomic(&mut state, &mut mem, 230).unwrap();
        let first_outputs = read_outputs(&mem, &first);
        second.simulate_atomic(&mut state, &mut mem, 230).unwrap();
        assert_eq!(read_outputs(&mem, &first), first_outputs, "round {round}");
        assert_eq!(first_outputs, a.map(f32::to_bits));
        assert_eq!(read_outputs(&mem, &second), b.map(f32::to_bits));
        let current = |code: &CompiledGainSmoother| mem.read(code.layout().spans()[2].0).unwrap();
        assert_eq!(
            (current(&first), current(&second)),
            (zero.current().to_bits(), other.current().to_bits())
        );
    }
}
