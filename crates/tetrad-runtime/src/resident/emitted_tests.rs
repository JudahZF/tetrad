use super::*;
use crate::gain_backend::{BuildError, GAIN_EXECUTION_STEPS, GainLayout};
use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::IsaProgram;
use tetrad_sharc::gain::GAIN_WORD_COUNT;
use tetrad_sharc::isa48::Instruction48;
use tetrad_sharc::register::{Dag1Index, DataReg};
use tetrad_sharc::simulator::UnmodeledSemantics;

const BUDGET: InstructionBudget = InstructionBudget::new(GAIN_EXECUTION_STEPS);

fn setup(gain: f32) -> (Resident, ReadyToken, ContextHandle) {
    let mut model = Resident::new();
    let ready = model.arm(Epoch(1), Tick(0)).unwrap();
    let context = model
        .create_context(ready, ContextSlot::First, gain)
        .unwrap();
    (model, ready, context)
}

fn backend(input: u32, output: u32) -> EmittedGainBackend {
    EmittedGainBackend::new(
        PmWordAddress24::new(0xff_fff7).unwrap(),
        GainLayout::new(DmWordAddress32::new(input), DmWordAddress32::new(output)).unwrap(),
    )
    .unwrap()
}

fn receive(
    model: &mut Resident,
    ready: ReadyToken,
    context: ContextHandle,
    slot: BlockSlot,
    input: &MonoBlock,
) -> BlockHandle {
    let completion = model.reserve_receive(ready, context, slot).unwrap();
    model.complete_receive(completion, input).unwrap()
}

#[test]
fn emitted_words_match_reference_bits_at_in_place_and_address_boundaries() {
    let input = std::array::from_fn(|frame| match frame % 8 {
        0 => 0.0,
        1 => -0.0,
        2 => 1.0,
        3 => -2.5,
        4 => f32::MIN_POSITIVE,
        5 => -f32::MIN_POSITIVE,
        6 => f32::from_bits(1),
        _ => -f32::from_bits(1),
    });
    for (dm_input, dm_output) in [(0, 0), (u32::MAX - 64, u32::MAX - 32), (32, 0)] {
        let backend = backend(dm_input, dm_output);
        assert_eq!(backend.code().word_count(), GAIN_WORD_COUNT);
        assert_eq!(backend.code().origin().get(), 0xff_fff7);
        let code_before = backend.code().clone();
        for gain in [-1.75, 2.0, 0.0, -0.0] {
            let (mut emitted, ready, context) = setup(gain);
            let block = receive(&mut emitted, ready, context, BlockSlot::First, &input);
            let token = emitted.begin_processing(block, Tick(10), 5).unwrap();
            assert_eq!(emitted.transmit_samples(block), Err(Error::NotReady));
            let receipt = emitted
                .finish_processing_emitted(token, Tick(15), &backend, BUDGET)
                .unwrap();
            assert_eq!(receipt.block, block);
            assert_eq!(receipt.execution.steps, GAIN_EXECUTION_STEPS);
            assert_eq!(receipt.execution.stores, BLOCK_FRAMES);
            let (mut reference, ready, context) = setup(gain);
            let reference_block = receive(&mut reference, ready, context, BlockSlot::First, &input);
            let token = reference
                .begin_processing(reference_block, Tick(10), 5)
                .unwrap();
            reference.finish_processing(token, Tick(15)).unwrap();
            assert_eq!(
                emitted.transmit_samples(block).unwrap().map(f32::to_bits),
                reference
                    .transmit_samples(reference_block)
                    .unwrap()
                    .map(f32::to_bits)
            );
            assert_eq!(
                emitted.blocks[0].input.map(f32::to_bits),
                input.map(f32::to_bits)
            );
            let tx = emitted.begin_transmit(block).unwrap();
            emitted.complete_transmit(tx).unwrap();
            assert_eq!(emitted.status().blocks, [Ownership::Free; 2]);
        }
        assert_eq!(backend.code(), &code_before);
    }
}

#[test]
fn emitted_boundary_freezes_updates_and_isolates_contexts_slots_and_scratch() {
    let backend = backend(96, 64);
    let (mut model, ready, a) = setup(2.0);
    let b = model
        .create_context(ready, ContextSlot::Second, -3.0)
        .unwrap();
    let first = receive(&mut model, ready, a, BlockSlot::First, &[1.0; BLOCK_FRAMES]);
    let second = receive(
        &mut model,
        ready,
        b,
        BlockSlot::Second,
        &[2.0; BLOCK_FRAMES],
    );
    model.update_gain(a, ParameterGeneration(2), 4.0).unwrap();
    let token = model.begin_processing(first, Tick(0), 0).unwrap();
    assert_eq!(token.parameter_generation(), ParameterGeneration(2));
    model.update_gain(a, ParameterGeneration(3), -2.0).unwrap();
    model.update_gain(a, ParameterGeneration(4), -5.0).unwrap();
    model
        .finish_processing_emitted(token, Tick(0), &backend, BUDGET)
        .unwrap();
    assert_eq!(model.transmit_samples(first).unwrap(), &[4.0; BLOCK_FRAMES]);
    let tx = model.begin_transmit(first).unwrap();
    let token = model.begin_processing(second, Tick(1), 0).unwrap();
    assert_eq!(token.parameter_generation(), ParameterGeneration(1));
    model
        .finish_processing_emitted(token, Tick(1), &backend, BUDGET)
        .unwrap();
    assert_eq!(
        model.transmit_samples(second).unwrap(),
        &[-6.0; BLOCK_FRAMES]
    );
    assert_eq!(model.transmit_samples(first).unwrap(), &[4.0; BLOCK_FRAMES]);
    assert_eq!(model.context_status(a).unwrap().active.gain, 4.0);
    assert_eq!(model.context_status(a).unwrap().pending.unwrap().gain, -5.0);
    model.complete_transmit(tx).unwrap();
    let third = receive(&mut model, ready, a, BlockSlot::First, &[3.0; BLOCK_FRAMES]);
    assert_ne!(third.generation(), first.generation());
    let token = model.begin_processing(third, Tick(2), 0).unwrap();
    assert_eq!(token.parameter_generation(), ParameterGeneration(4));
    model
        .finish_processing_emitted(token, Tick(2), &backend, BUDGET)
        .unwrap();
    assert_eq!(
        model.transmit_samples(third).unwrap(),
        &[-15.0; BLOCK_FRAMES]
    );
    let tx_b = model.begin_transmit(second).unwrap();
    model.complete_transmit(tx_b).unwrap();
    let new_b = model.reset_context(b, 0.5).unwrap();
    assert_ne!(new_b.generation(), b.generation());
    let fourth = receive(
        &mut model,
        ready,
        new_b,
        BlockSlot::Second,
        &[0.0; BLOCK_FRAMES],
    );
    let token = model.begin_processing(fourth, Tick(3), 0).unwrap();
    model
        .finish_processing_emitted(token, Tick(3), &backend, BUDGET)
        .unwrap();
    assert_eq!(
        model.transmit_samples(fourth).unwrap().map(f32::to_bits),
        [0; BLOCK_FRAMES]
    );
    assert_eq!(
        model.transmit_samples(third).unwrap(),
        &[-15.0; BLOCK_FRAMES]
    );
}

// Keep another slot held by a modeled DMA transfer through every finish fault.
fn held_and_processing(
    backend: &EmittedGainBackend,
    input: &MonoBlock,
) -> (
    Resident,
    ReadyToken,
    BlockHandle,
    Completion,
    ProcessingToken,
) {
    let (mut model, ready, context) = setup(2.0);
    let held = receive(
        &mut model,
        ready,
        context,
        BlockSlot::First,
        &[7.0; BLOCK_FRAMES],
    );
    let token = model.begin_processing(held, Tick(0), 1).unwrap();
    model
        .finish_processing_emitted(token, Tick(1), backend, BUDGET)
        .unwrap();
    let tx = model.begin_transmit(held).unwrap();
    let next = receive(&mut model, ready, context, BlockSlot::Second, input);
    let token = model.begin_processing(next, Tick(2), 2).unwrap();
    (model, ready, held, tx, token)
}

fn assert_atomic_fault(
    model: &mut Resident,
    held: BlockHandle,
    tx: Completion,
    token: ProcessingToken,
    error: Error,
) {
    assert_eq!(
        model.status().lifecycle,
        Lifecycle::Fault {
            epoch: Epoch(1),
            cause: error
        }
    );
    assert_eq!(
        model.status().blocks,
        [Ownership::DmaTransmit, Ownership::Processing]
    );
    assert_eq!(model.blocks[1].output.map(f32::to_bits), [0; BLOCK_FRAMES]);
    assert_eq!(model.blocks[1].processing, Some(token));
    assert_eq!(model.last_tick, Tick(2));
    assert_eq!(model.transmit_samples(held).unwrap(), &[14.0; BLOCK_FRAMES]);
    assert_eq!(model.transmit_samples(token.block()), Err(Error::NotReady));
    assert_eq!(model.complete_transmit(tx), Err(Error::NotReady));
    assert_eq!(model.begin_transmit(token.block()), Err(Error::NotReady));
    assert_eq!(model.fault_silence().unwrap(), &[0.0; BLOCK_FRAMES]);
    assert_ne!(
        model.fault_silence().unwrap().as_ptr(),
        model.transmit_samples(held).unwrap().as_ptr()
    );
}

#[test]
fn exhausted_raw_budgets_discard_partial_and_complete_scratch_output() {
    let backend = backend(0, 32);
    assert_eq!(
        InstructionBudget::new(usize::MAX).get(),
        GAIN_EXECUTION_STEPS
    );
    for steps in [0, 8, GAIN_EXECUTION_STEPS - 1] {
        let (mut model, _, held, tx, token) = held_and_processing(&backend, &[1.0; BLOCK_FRAMES]);
        let error = Error::RawExecution(RawExecutionError::StepBudget(steps));
        assert_eq!(
            model.finish_processing_emitted(
                token,
                Tick(4),
                &backend,
                InstructionBudget::new(steps)
            ),
            Err(error)
        );
        assert_atomic_fault(&mut model, held, tx, token, error);
    }
}

#[test]
fn unsupported_and_unmodeled_raw_words_fault_after_a_scratch_store() {
    for instruction in [
        Instruction48::ModifyWords {
            index: Dag1Index::new(0).unwrap(),
            words: 1,
        },
        Instruction48::Rti,
    ] {
        let mut backend = backend(0, 32);
        let (mut model, _, held, tx, token) = held_and_processing(&backend, &[1.0; BLOCK_FRAMES]);
        let origin = backend.code().origin();
        let mut program = IsaProgram::new(origin);
        program
            .push(Instruction48::DmDirectStore {
                src: DataReg::new(4).unwrap(),
                address: backend.layout().output(),
            })
            .unwrap();
        program.push(instruction).unwrap();
        backend.replace_code_for_test(program.finish().unwrap());
        let raw_error = if instruction == Instruction48::Rti {
            RawExecutionError::Unmodeled {
                pc: origin.get() + 1,
                feature: UnmodeledSemantics::InterruptReturn,
            }
        } else {
            RawExecutionError::UnsupportedWord {
                pc: origin.get() + 1,
                word: instruction.encode().unwrap().bits(),
            }
        };
        let error = Error::RawExecution(raw_error);
        assert_eq!(
            model.finish_processing_emitted(token, Tick(4), &backend, BUDGET),
            Err(error)
        );
        assert_atomic_fault(&mut model, held, tx, token, error);
    }
}

#[test]
fn emitted_nonfinite_overflow_keeps_held_output_and_frozen_boundary() {
    let backend = backend(64, 64);
    let mut input = [1.0; BLOCK_FRAMES];
    input[BLOCK_FRAMES - 1] = f32::MAX;
    let (mut model, _, held, tx, token) = held_and_processing(&backend, &input);
    let context = model.blocks[1].context.unwrap();
    model
        .update_gain(context, ParameterGeneration(2), 0.0)
        .unwrap();
    let error = Error::NonFiniteOutput(BLOCK_FRAMES - 1);
    assert_eq!(
        model.finish_processing_emitted(token, Tick(4), &backend, BUDGET),
        Err(error)
    );
    assert_atomic_fault(&mut model, held, tx, token, error);
    assert_eq!(model.contexts[0].parameters.unwrap().active.gain, 2.0);
    assert_eq!(
        model.contexts[0].parameters.unwrap().pending.unwrap().gain,
        0.0
    );
}

#[test]
fn emitted_finish_validates_ticks_and_tokens_before_raw_execution() {
    let backend = backend(0, 32);
    for case in 0..3 {
        let (mut model, _, held, tx, token) = held_and_processing(&backend, &[1.0; BLOCK_FRAMES]);
        let (submitted, now, error) = match case {
            0 => (
                token,
                Tick(5),
                Error::Deadline {
                    deadline: Tick(4),
                    now: Tick(5),
                },
            ),
            1 => (
                token,
                Tick(1),
                Error::ClockRollback {
                    previous: Tick(2),
                    now: Tick(1),
                },
            ),
            _ => (
                ProcessingToken {
                    parameter_generation: ParameterGeneration(2),
                    ..token
                },
                Tick(4),
                Error::ProcessingToken,
            ),
        };
        // A zero raw budget would fail if execution ran before these checks.
        assert_eq!(
            model.finish_processing_emitted(submitted, now, &backend, InstructionBudget::new(0)),
            Err(error)
        );
        assert_atomic_fault(&mut model, held, tx, token, error);
    }
}

#[test]
fn emitted_recovery_requires_quiescence_and_rejects_stale_epoch_and_reused_slot() {
    let backend = backend(0, 32);
    let (mut model, ready, held, tx, old) = held_and_processing(&backend, &[1.0; BLOCK_FRAMES]);
    model
        .finish_processing_emitted(old, Tick(4), &backend, InstructionBudget::new(0))
        .unwrap_err();
    assert_eq!(model.arm(Epoch(2), Tick(4)), Err(Error::QuiescenceRequired));
    assert_eq!(model.complete_transmit(tx), Err(Error::NotReady));
    assert_eq!(model.transmit_samples(held).unwrap(), &[14.0; BLOCK_FRAMES]);
    model.acknowledge_quiescence(ready).unwrap();
    assert!(matches!(
        model.arm(Epoch(1), Tick(4)),
        Err(Error::EpochNotFresh { .. })
    ));
    let fresh = model.arm(Epoch(2), Tick(4)).unwrap();
    let context = model
        .create_context(fresh, ContextSlot::First, -1.0)
        .unwrap();
    let block = receive(
        &mut model,
        fresh,
        context,
        BlockSlot::Second,
        &[3.0; BLOCK_FRAMES],
    );
    let current = model.begin_processing(block, Tick(4), 0).unwrap();
    let error = Error::StaleEpoch {
        expected: Epoch(2),
        received: Epoch(1),
    };
    assert_eq!(
        model.finish_processing_emitted(old, Tick(4), &backend, InstructionBudget::new(0)),
        Err(error)
    );
    assert_eq!(model.blocks[1].processing, Some(current));
    assert_eq!(model.blocks[1].output.map(f32::to_bits), [0; BLOCK_FRAMES]);
    assert_eq!(model.acknowledge_quiescence(ready), Err(error));
    model.acknowledge_quiescence(fresh).unwrap();
    let fresh = model.arm(Epoch(3), Tick(4)).unwrap();
    let context = model
        .create_context(fresh, ContextSlot::First, -1.0)
        .unwrap();
    let block = receive(
        &mut model,
        fresh,
        context,
        BlockSlot::First,
        &[3.0; BLOCK_FRAMES],
    );
    let old = model.begin_processing(block, Tick(4), 0).unwrap();
    model
        .finish_processing_emitted(old, Tick(4), &backend, BUDGET)
        .unwrap();
    assert_eq!(
        model.transmit_samples(block).unwrap(),
        &[-3.0; BLOCK_FRAMES]
    );
    let tx = model.begin_transmit(block).unwrap();
    model.complete_transmit(tx).unwrap();
    let block = receive(
        &mut model,
        fresh,
        context,
        BlockSlot::First,
        &[9.0; BLOCK_FRAMES],
    );
    let current = model.begin_processing(block, Tick(4), 0).unwrap();
    assert_eq!(
        model.finish_processing_emitted(old, Tick(4), &backend, BUDGET),
        Err(Error::StaleBlock)
    );
    assert_eq!(model.blocks[0].processing, Some(current));
    assert_eq!(model.blocks[0].output.map(f32::to_bits), [0; BLOCK_FRAMES]);
}

#[test]
fn logical_layout_rejects_partial_overlap_gaps_and_one_past_overflow() {
    for (input, output) in [(0, 1), (1, 0), (0, 33), (64, 0)] {
        assert_eq!(
            GainLayout::new(DmWordAddress32::new(input), DmWordAddress32::new(output)),
            Err(BuildError::BufferLayout)
        );
    }
    assert!(matches!(
        GainLayout::new(
            DmWordAddress32::new(u32::MAX - 31),
            DmWordAddress32::new(u32::MAX - 31)
        ),
        Err(BuildError::Address(_))
    ));
    let layout = GainLayout::new(DmWordAddress32::new(0), DmWordAddress32::new(32)).unwrap();
    assert!(matches!(
        EmittedGainBackend::new(PmWordAddress24::new(0xff_fff8).unwrap(), layout),
        Err(BuildError::Emit(_))
    ));
}
