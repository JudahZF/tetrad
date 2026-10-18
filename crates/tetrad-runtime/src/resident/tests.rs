use super::*;

fn setup() -> (Resident, ReadyToken, ContextHandle) {
    let mut model = Resident::new();
    let ready = model.arm(Epoch::new(1), Tick::new(0)).unwrap();
    let context = model
        .create_context(ready, ContextSlot::First, 2.0)
        .unwrap();
    (model, ready, context)
}
fn receive(
    model: &mut Resident,
    ready: ReadyToken,
    context: ContextHandle,
    slot: BlockSlot,
    value: f32,
) -> BlockHandle {
    let event = model.reserve_receive(ready, context, slot).unwrap();
    model
        .complete_receive(event, &[value; BLOCK_FRAMES])
        .unwrap()
}
fn process(model: &mut Resident, block: BlockHandle, now: u64) -> BlockHandle {
    let token = model.begin_processing(block, Tick::new(now), 10).unwrap();
    model.finish_processing(token, Tick::new(now + 1)).unwrap()
}
fn assert_fault(model: &Resident, error: Error) {
    assert_eq!(
        model.status().lifecycle,
        Lifecycle::Fault {
            epoch: Epoch(1),
            cause: error
        }
    );
    assert_eq!(model.fault_silence(), Some(&[0.0; BLOCK_FRAMES]));
}

#[test]
fn complete_workflow_reuses_only_released_slot_with_new_generation() {
    let (mut model, ready, context) = setup();
    assert_eq!(CHANNELS, 1);
    assert_eq!(model.fault_silence(), None);
    let rx = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    assert_eq!(rx.sequence, Sequence(1));
    assert_eq!(
        model.status().blocks,
        [Ownership::DmaReceive, Ownership::Free]
    );
    let block = model.complete_receive(rx, &[3.0; BLOCK_FRAMES]).unwrap();
    assert_eq!(model.status().blocks[0], Ownership::DspReady);
    let token = model.begin_processing(block, Tick(5), 10).unwrap();
    assert_eq!(model.status().blocks[0], Ownership::Processing);
    assert_eq!(model.transmit_samples(block), Err(Error::NotReady));
    model.finish_processing(token, Tick(15)).unwrap(); // Deadline is inclusive.
    assert_eq!(model.status().blocks[0], Ownership::TransmitReady);
    assert_eq!(model.transmit_samples(block).unwrap(), &[6.0; BLOCK_FRAMES]);
    let tx = model.begin_transmit(block).unwrap();
    assert_eq!(model.status().blocks[0], Ownership::DmaTransmit);
    assert_eq!(model.reset_context(context, 1.0), Err(Error::ContextBusy));
    assert_eq!(model.destroy_context(context), Err(Error::ContextBusy));
    model.complete_transmit(tx).unwrap();
    assert_eq!(model.status().blocks[0], Ownership::Free);
    let rx2 = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    assert_eq!(rx2.sequence, Sequence(2));
    assert_eq!(rx2.block.generation, BlockGeneration(2));
    assert_ne!(rx2.block, block);
}

#[test]
fn contexts_are_isolated_and_pending_gain_activates_only_at_boundary() {
    let (mut model, ready, a) = setup();
    let b = model
        .create_context(ready, ContextSlot::Second, -1.0)
        .unwrap();
    let first = receive(&mut model, ready, a, BlockSlot::First, 4.0);
    let second = receive(&mut model, ready, b, BlockSlot::Second, 4.0);
    model.update_gain(a, ParameterGeneration(2), 3.0).unwrap();
    let status = model.context_status(a).unwrap();
    assert_eq!(status.active.gain, 2.0);
    assert_eq!(status.pending.unwrap().gain, 3.0);
    let token = model.begin_processing(first, Tick(0), 10).unwrap();
    assert_eq!(token.parameter_generation(), ParameterGeneration(2));
    assert_eq!(model.context_status(a).unwrap().pending, None);
    model.update_gain(a, ParameterGeneration(3), 5.0).unwrap();
    assert_eq!(model.context_status(a).unwrap().active.gain, 3.0);
    model.finish_processing(token, Tick(1)).unwrap();
    assert_eq!(
        model.transmit_samples(first).unwrap(),
        &[12.0; BLOCK_FRAMES]
    );
    process(&mut model, second, 2);
    assert_eq!(
        model.transmit_samples(second).unwrap(),
        &[-4.0; BLOCK_FRAMES]
    );
    assert_eq!(
        model.context_status(b).unwrap().active.generation,
        ParameterGeneration(1)
    );
    let tx = model.begin_transmit(first).unwrap();
    model.complete_transmit(tx).unwrap();
    let next_block = receive(&mut model, ready, a, BlockSlot::First, 4.0);
    process(&mut model, next_block, 4);
    assert_eq!(
        model.transmit_samples(next_block).unwrap(),
        &[20.0; BLOCK_FRAMES]
    );
    assert_eq!(
        model.context_status(a).unwrap().active.generation,
        ParameterGeneration(3)
    );
}

#[test]
fn static_context_reservation_reset_destroy_and_parameter_errors_are_atomic() {
    let (mut model, ready, a) = setup();
    assert_eq!(
        model.create_context(ready, ContextSlot::First, 1.0),
        Err(Error::ContextReserved)
    );
    let original = model.context_status(a).unwrap();
    for gain in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            model.update_gain(a, ParameterGeneration(2), gain),
            Err(Error::NonFiniteGain)
        );
        assert_eq!(model.reset_context(a, gain), Err(Error::NonFiniteGain));
        assert_eq!(model.context_status(a).unwrap(), original);
    }
    assert_eq!(
        model.update_gain(a, ParameterGeneration(3), 4.0),
        Err(Error::ParameterGeneration {
            expected: ParameterGeneration(2),
            received: ParameterGeneration(3)
        })
    );
    model.update_gain(a, ParameterGeneration(2), 4.0).unwrap();
    assert_eq!(
        model.update_gain(a, ParameterGeneration(2), 7.0),
        Err(Error::ParameterGeneration {
            expected: ParameterGeneration(3),
            received: ParameterGeneration(2)
        })
    );
    model.update_gain(a, ParameterGeneration(3), 6.0).unwrap();
    let reset = model.reset_context(a, 0.5).unwrap();
    assert_eq!(reset.generation(), ContextGeneration(2));
    assert_eq!(model.context_status(a), Err(Error::StaleContext));
    assert_eq!(
        model.context_status(reset).unwrap(),
        ContextStatus {
            active: Parameters {
                generation: ParameterGeneration(1),
                gain: 0.5
            },
            pending: None
        }
    );
    model.destroy_context(reset).unwrap();
    assert_eq!(model.destroy_context(reset), Err(Error::StaleContext));
    let replacement = model
        .create_context(ready, ContextSlot::First, 1.0)
        .unwrap();
    assert_eq!(replacement.generation(), ContextGeneration(3));
    assert_eq!(
        model.update_gain(reset, ParameterGeneration(2), 4.0),
        Err(Error::StaleContext)
    );
    assert_eq!(
        model.status().lifecycle,
        Lifecycle::Ready { epoch: Epoch(1) }
    );
}

#[test]
fn receive_duplicate_missed_reordered_and_wrapped_sequences_do_not_commit() {
    for received in [0, 2, u64::MAX] {
        let (mut model, ready, context) = setup();
        let mut rx = model
            .reserve_receive(ready, context, BlockSlot::First)
            .unwrap();
        rx.sequence = Sequence(received);
        let error = Error::Sequence {
            direction: Direction::Receive,
            expected: Sequence(1),
            received: Sequence(received),
        };
        assert_eq!(model.complete_receive(rx, &[8.0; BLOCK_FRAMES]), Err(error));
        assert_fault(&model, error);
        assert_eq!(model.blocks[0].input, [0.0; BLOCK_FRAMES]);
        assert_eq!(model.status().blocks[0], Ownership::DmaReceive);
        assert_eq!(model.receive_complete, Sequence(1));
    }
    let (mut model, ready, context) = setup();
    let first = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    let second = model
        .reserve_receive(ready, context, BlockSlot::Second)
        .unwrap();
    assert!(matches!(
        model.complete_receive(second, &[4.0; BLOCK_FRAMES]),
        Err(Error::Sequence { .. })
    ));
    assert_eq!(model.status().blocks, [Ownership::DmaReceive; 2]);
    assert_eq!(
        model.complete_receive(first, &[4.0; BLOCK_FRAMES]),
        Err(Error::NotReady)
    );

    let (mut model, ready, context) = setup();
    let rx = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    model.complete_receive(rx, &[4.0; BLOCK_FRAMES]).unwrap();
    let error = Error::Sequence {
        direction: Direction::Receive,
        expected: Sequence(2),
        received: Sequence(1),
    };
    assert_eq!(model.complete_receive(rx, &[8.0; BLOCK_FRAMES]), Err(error));
    assert_fault(&model, error);
    assert_eq!(model.blocks[0].input, [4.0; BLOCK_FRAMES]);
    assert_eq!(model.status().blocks[0], Ownership::DspReady);
}

#[test]
fn transmit_order_and_duplicates_preserve_buffers_without_releasing() {
    let (mut model, ready, context) = setup();
    let a = receive(&mut model, ready, context, BlockSlot::First, 1.0);
    let b = receive(&mut model, ready, context, BlockSlot::Second, 2.0);
    process(&mut model, a, 0);
    process(&mut model, b, 2);
    let first = model.begin_transmit(a).unwrap();
    let second = model.begin_transmit(b).unwrap();
    let error = Error::Sequence {
        direction: Direction::Transmit,
        expected: Sequence(1),
        received: Sequence(2),
    };
    assert_eq!(model.complete_transmit(second), Err(error));
    assert_fault(&model, error);
    assert_eq!(model.status().blocks, [Ownership::DmaTransmit; 2]);
    assert_eq!(model.transmit_samples(a).unwrap(), &[2.0; BLOCK_FRAMES]);
    assert_eq!(model.transmit_samples(b).unwrap(), &[4.0; BLOCK_FRAMES]);
    assert_eq!(model.complete_transmit(first), Err(Error::NotReady));

    let (mut model, ready, context) = setup();
    let a = receive(&mut model, ready, context, BlockSlot::First, 1.0);
    process(&mut model, a, 0);
    let tx = model.begin_transmit(a).unwrap();
    model.complete_transmit(tx).unwrap();
    let error = Error::Sequence {
        direction: Direction::Transmit,
        expected: Sequence(2),
        received: Sequence(1),
    };
    assert_eq!(model.complete_transmit(tx), Err(error));
    assert_fault(&model, error);
    assert_eq!(
        model.reserve_receive(ready, context, BlockSlot::First),
        Err(Error::NotReady)
    );
}

#[test]
fn stale_epochs_and_generations_fault_before_accepting_data() {
    for stale_epoch in [false, true] {
        let (mut model, ready, context) = setup();
        let mut rx = model
            .reserve_receive(ready, context, BlockSlot::First)
            .unwrap();
        let expected = if stale_epoch {
            rx.epoch = Epoch(0);
            Error::StaleEpoch {
                expected: Epoch(1),
                received: Epoch(0),
            }
        } else {
            rx.block.generation = BlockGeneration(0);
            Error::StaleBlock
        };
        assert_eq!(
            model.complete_receive(rx, &[8.0; BLOCK_FRAMES]),
            Err(expected)
        );
        assert_fault(&model, expected);
        assert_eq!(model.blocks[0].input, [0.0; BLOCK_FRAMES]);
    }
    let (mut model, ready, context) = setup();
    model.destroy_context(context).unwrap();
    model
        .create_context(ready, ContextSlot::First, 3.0)
        .unwrap();
    assert_eq!(
        model.reserve_receive(ready, context, BlockSlot::First),
        Err(Error::StaleContext)
    );
    assert_fault(&model, Error::StaleContext);
    assert_eq!(model.status().blocks, [Ownership::Free; 2]);
}

#[test]
fn overrun_and_invalid_completion_do_not_change_existing_ownership() {
    let (mut model, ready, context) = setup();
    let rx = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    assert_eq!(
        model.reserve_receive(ready, context, BlockSlot::First),
        Err(Error::Overrun)
    );
    assert_fault(&model, Error::Overrun);
    assert_eq!(model.blocks[0].generation, rx.block.generation);
    assert_eq!(model.status().blocks[0], Ownership::DmaReceive);

    let (mut model, ready, context) = setup();
    let rx = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    let error = Error::Ownership {
        expected: Ownership::DmaTransmit,
        actual: Ownership::DmaReceive,
    };
    assert_eq!(
        model.complete_transmit(Completion {
            direction: Direction::Transmit,
            ..rx
        }),
        Err(error)
    );
    assert_fault(&model, error);
    assert_eq!(model.transmit_complete, Sequence(1));

    // Receive/transmit sequences can be equal for the same block generation.
    // The old receive record still cannot free a transmit-owned buffer.
    let (mut model, ready, context) = setup();
    let rx = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    let block = model.complete_receive(rx, &[3.0; BLOCK_FRAMES]).unwrap();
    process(&mut model, block, 0);
    let tx = model.begin_transmit(block).unwrap();
    assert_eq!(rx.sequence, tx.sequence);
    let error = Error::CompletionDirection {
        expected: Direction::Transmit,
        received: Direction::Receive,
    };
    assert_eq!(model.complete_transmit(rx), Err(error));
    assert_fault(&model, error);
    assert_eq!(model.status().blocks[0], Ownership::DmaTransmit);
    assert_eq!(model.transmit_samples(block).unwrap(), &[6.0; BLOCK_FRAMES]);
}

#[test]
fn deadline_rollback_and_overflow_leave_output_and_parameters_unpublished() {
    for case in 0..2 {
        let (mut model, ready, context) = setup();
        let block = receive(&mut model, ready, context, BlockSlot::First, 4.0);
        let token = model.begin_processing(block, Tick(10), 5).unwrap();
        let (now, error) = match case {
            0 => (
                Tick(16),
                Error::Deadline {
                    deadline: Tick(15),
                    now: Tick(16),
                },
            ),
            _ => (
                Tick(9),
                Error::ClockRollback {
                    previous: Tick(10),
                    now: Tick(9),
                },
            ),
        };
        assert_eq!(model.finish_processing(token, now), Err(error));
        assert_fault(&model, error);
        assert_eq!(model.blocks[0].output, [0.0; BLOCK_FRAMES]);
        assert_eq!(model.status().blocks[0], Ownership::Processing);
        assert_eq!(model.transmit_samples(block), Err(Error::NotReady));
    }
    let (mut model, ready, context) = setup();
    let first = receive(&mut model, ready, context, BlockSlot::First, 1.0);
    process(&mut model, first, 10);
    let second = receive(&mut model, ready, context, BlockSlot::Second, 1.0);
    model
        .update_gain(context, ParameterGeneration(2), 3.0)
        .unwrap();
    let error = Error::ClockRollback {
        previous: Tick(11),
        now: Tick(10),
    };
    assert_eq!(model.begin_processing(second, Tick(10), 5), Err(error));
    assert_fault(&model, error);
    assert_eq!(model.status().blocks[1], Ownership::DspReady);
    assert_eq!(model.contexts[0].parameters.unwrap().active.gain, 2.0);

    let (mut model, ready, context) = setup();
    let block = receive(&mut model, ready, context, BlockSlot::First, 1.0);
    model
        .update_gain(context, ParameterGeneration(2), 3.0)
        .unwrap();
    assert_eq!(
        model.begin_processing(block, Tick(u64::MAX), 1),
        Err(Error::DeadlineOverflow)
    );
    assert_fault(&model, Error::DeadlineOverflow);
    assert_eq!(model.status().blocks[0], Ownership::DspReady);
    assert_eq!(model.contexts[0].parameters.unwrap().active.gain, 2.0);
    assert_eq!(
        model.contexts[0].parameters.unwrap().pending.unwrap().gain,
        3.0
    );
    assert_eq!(model.last_tick, Tick(0));
}

#[test]
fn nonfinite_input_and_multiply_overflow_are_atomic_faults() {
    for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let (mut model, ready, context) = setup();
        let rx = model
            .reserve_receive(ready, context, BlockSlot::First)
            .unwrap();
        let mut input = [1.0; BLOCK_FRAMES];
        input[31] = sample;
        assert_eq!(
            model.complete_receive(rx, &input),
            Err(Error::NonFiniteInput(31))
        );
        assert_fault(&model, Error::NonFiniteInput(31));
        assert_eq!(model.blocks[0].input, [0.0; BLOCK_FRAMES]);
    }
    let (mut model, ready, context) = setup();
    let rx = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    let mut input = [1.0; BLOCK_FRAMES];
    input[31] = f32::MAX;
    let block = model.complete_receive(rx, &input).unwrap();
    let token = model.begin_processing(block, Tick(0), 1).unwrap();
    assert_eq!(
        model.finish_processing(token, Tick(1)),
        Err(Error::NonFiniteOutput(31))
    );
    assert_fault(&model, Error::NonFiniteOutput(31));
    assert_eq!(model.blocks[0].output, [0.0; BLOCK_FRAMES]);
}

#[test]
fn fault_silence_is_separate_and_rearm_needs_ack_and_strictly_fresh_epoch() {
    let (mut model, ready, context) = setup();
    let block = receive(&mut model, ready, context, BlockSlot::First, 7.0);
    process(&mut model, block, 0);
    let tx = model.begin_transmit(block).unwrap();
    let other = receive(&mut model, ready, context, BlockSlot::Second, 2.0);
    let token = model.begin_processing(other, Tick(2), 1).unwrap();
    let error = Error::Deadline {
        deadline: Tick(3),
        now: Tick(4),
    };
    assert_eq!(model.finish_processing(token, Tick(4)), Err(error));
    assert_fault(&model, error);
    let held = model.transmit_samples(block).unwrap();
    let silence = model.fault_silence().unwrap();
    assert_ne!(held.as_ptr(), silence.as_ptr());
    assert_eq!(held, &[14.0; BLOCK_FRAMES]);
    assert_eq!(model.begin_transmit(block), Err(Error::NotReady));
    assert_eq!(model.complete_transmit(tx), Err(Error::NotReady));
    assert_eq!(model.arm(Epoch(2), Tick(5)), Err(Error::QuiescenceRequired));
    assert_eq!(
        model.acknowledge_quiescence(ReadyToken { epoch: Epoch(0) }),
        Err(Error::StaleEpoch {
            expected: Epoch(1),
            received: Epoch(0)
        })
    );
    assert_eq!(model.status().blocks[0], Ownership::DmaTransmit);
    model.acknowledge_quiescence(ready).unwrap();
    assert_eq!(model.status().blocks, [Ownership::Free; 2]);
    assert_eq!(model.status().contexts, [false; 2]);
    assert_eq!(model.acknowledge_quiescence(ready), Err(Error::NotReady));
    assert_eq!(
        model.arm(Epoch(1), Tick(5)),
        Err(Error::EpochNotFresh {
            previous: Epoch(1),
            proposed: Epoch(1)
        })
    );
    let fresh = model.arm(Epoch(2), Tick(5)).unwrap();
    let new_context = model
        .create_context(fresh, ContextSlot::First, 1.0)
        .unwrap();
    assert_ne!(new_context.generation(), context.generation());
    assert!(matches!(
        model.context_status(context),
        Err(Error::StaleEpoch { .. })
    ));
    assert!(matches!(
        model.create_context(ready, ContextSlot::Second, 1.0),
        Err(Error::StaleEpoch { .. })
    ));
    assert!(matches!(
        model.finish_processing(token, Tick(6)),
        Err(Error::StaleEpoch { .. })
    ));
    assert_eq!(
        model.status().lifecycle,
        Lifecycle::Fault {
            epoch: Epoch(2),
            cause: Error::StaleEpoch {
                expected: Epoch(2),
                received: Epoch(1)
            }
        }
    );
    // An old ack cannot release resources from the new epoch.
    assert!(matches!(
        model.acknowledge_quiescence(ready),
        Err(Error::StaleEpoch { .. })
    ));
}

#[test]
fn same_epoch_stale_block_cannot_complete_a_reused_slot() {
    let (mut model, ready, context) = setup();
    let old = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    let block = model.complete_receive(old, &[1.0; BLOCK_FRAMES]).unwrap();
    process(&mut model, block, 0);
    let tx = model.begin_transmit(block).unwrap();
    model.complete_transmit(tx).unwrap();
    let current = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    let replay = Completion {
        sequence: current.sequence,
        ..old
    };
    assert_eq!(
        model.complete_receive(replay, &[8.0; BLOCK_FRAMES]),
        Err(Error::StaleBlock)
    );
    assert_fault(&model, Error::StaleBlock);
    assert_eq!(model.blocks[0].generation, current.block.generation());
    assert_eq!(model.blocks[0].input, [0.0; BLOCK_FRAMES]);
}

#[test]
fn processing_token_replay_and_concurrent_processing_are_faults() {
    for separate_context in [false, true] {
        let (mut model, ready, context) = setup();
        let second_context = if separate_context {
            model
                .create_context(ready, ContextSlot::Second, 1.0)
                .unwrap()
        } else {
            context
        };
        let a = receive(&mut model, ready, context, BlockSlot::First, 1.0);
        let b = receive(&mut model, ready, second_context, BlockSlot::Second, 2.0);
        model.begin_processing(a, Tick(0), 10).unwrap();
        assert_eq!(
            model.begin_processing(b, Tick(0), 10),
            Err(Error::ProcessorBusy)
        );
        assert_fault(&model, Error::ProcessorBusy);
        assert_eq!(
            model.status().blocks,
            [Ownership::Processing, Ownership::DspReady]
        );
    }

    let (mut model, ready, context) = setup();
    let a = receive(&mut model, ready, context, BlockSlot::First, 1.0);
    let token = model.begin_processing(a, Tick(0), 1).unwrap();
    model.finish_processing(token, Tick(1)).unwrap();
    let error = Error::Ownership {
        expected: Ownership::Processing,
        actual: Ownership::TransmitReady,
    };
    assert_eq!(model.finish_processing(token, Tick(1)), Err(error));
    assert_fault(&model, error);
    assert_eq!(model.blocks[0].output, [2.0; BLOCK_FRAMES]);
    assert_eq!(model.transmit_samples(a), Err(Error::NotReady));
}

#[test]
fn counters_exhaust_without_wrapping_or_committing() {
    for transmit in [false, true] {
        let (mut model, ready, context) = setup();
        if transmit {
            let block = receive(&mut model, ready, context, BlockSlot::First, 1.0);
            process(&mut model, block, 0);
            model.transmit_issue = Sequence(u64::MAX);
            assert_eq!(
                model.begin_transmit(block),
                Err(Error::Exhausted("transmit sequence"))
            );
            assert_eq!(model.status().blocks[0], Ownership::TransmitReady);
        } else {
            model.receive_issue = Sequence(u64::MAX);
            assert_eq!(
                model.reserve_receive(ready, context, BlockSlot::First),
                Err(Error::Exhausted("receive sequence"))
            );
            assert_eq!(model.status().blocks[0], Ownership::Free);
        }
    }
    let (mut model, ready, context) = setup();
    model.receive_complete = Sequence(u64::MAX);
    let mut rx = model
        .reserve_receive(ready, context, BlockSlot::First)
        .unwrap();
    rx.sequence = Sequence(u64::MAX);
    assert_eq!(
        model.complete_receive(rx, &[1.0; BLOCK_FRAMES]),
        Err(Error::Exhausted("completion sequence"))
    );
    assert_eq!(model.blocks[0].input, [0.0; BLOCK_FRAMES]);

    let (mut model, ready, context) = setup();
    model.contexts[0]
        .parameters
        .as_mut()
        .unwrap()
        .active
        .generation = ParameterGeneration(u64::MAX);
    assert_eq!(
        model.update_gain(context, ParameterGeneration(0), 3.0),
        Err(Error::Exhausted("parameter generation"))
    );
    model.contexts[0].generation = ContextGeneration(u64::MAX);
    let exhausted = ContextHandle {
        generation: ContextGeneration(u64::MAX),
        ..context
    };
    assert_eq!(
        model.reset_context(exhausted, 1.0),
        Err(Error::Exhausted("context generation"))
    );
    model.destroy_context(exhausted).unwrap();
    assert_eq!(
        model.create_context(ready, ContextSlot::First, 1.0),
        Err(Error::Exhausted("context generation"))
    );
    let other = model
        .create_context(ready, ContextSlot::Second, 1.0)
        .unwrap();
    model.blocks[0].generation = BlockGeneration(u64::MAX);
    assert_eq!(
        model.reserve_receive(ready, other, BlockSlot::First),
        Err(Error::Exhausted("block generation"))
    );
    assert_eq!(model.status().blocks[0], Ownership::Free);
}

#[test]
fn startup_and_physical_gates_are_explicit_and_do_not_change_state() {
    let mut model = Resident::new();
    assert_eq!(
        model.arm(Epoch(0), Tick(0)),
        Err(Error::EpochNotFresh {
            previous: Epoch(0),
            proposed: Epoch(0)
        })
    );
    let ready = model.arm(Epoch(1), Tick(10)).unwrap();
    model.acknowledge_quiescence(ready).unwrap();
    assert_eq!(
        model.arm(Epoch(2), Tick(9)),
        Err(Error::ClockRollback {
            previous: Tick(10),
            now: Tick(9)
        })
    );
    assert_eq!(model.last_epoch, Epoch(1));
    assert_eq!(model.status().lifecycle, Lifecycle::Quiescent);
    for operation in [
        PhysicalOperation::Backend,
        PhysicalOperation::Start,
        PhysicalOperation::Load,
        PhysicalOperation::Audio,
    ] {
        assert_eq!(
            model.physical(operation),
            Err(Error::PhysicalUnavailable(operation))
        );
    }
    assert_eq!(PHYSICAL_GATES.len(), 6);
}
