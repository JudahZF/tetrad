use super::*;

#[test]
fn record_start_resets_reverse_then_oneshot_then_clock() {
    let (mut looper, mut playback, context) = fixture(1);
    playback.oneshot = true;
    playback.incoming.in_reverse = 17;
    assert_eq!(
        button(&mut looper, &mut playback, &context, 0, 1.0),
        [
            LooperWrite::RigParameter {
                index: 7,
                value_bits: 0.0_f32.to_bits()
            },
            param(8, 0.0),
            event(3),
            reset_clock(),
        ]
    );
    assert_eq!(playback.cached_state, 5);
    assert!(!playback.oneshot);
}

/// States 2/3/9/10 start overdub; 4/5 and 6 also act on release unless
/// P13 (`+0xbd`) or P14 (`+0xbe`) latches them.
#[test]
fn record_press_and_release_follow_armed_and_latch_flags() {
    for state in [2, 3, 9, 10] {
        let (mut looper, mut playback, context) = fixture(state);
        assert!(button(&mut looper, &mut playback, &context, 0, 0.5).is_empty());
        assert_eq!(
            button(&mut looper, &mut playback, &context, 0, 1.0),
            [event(4)]
        );
        assert_eq!(playback.cached_state, 6);
    }
    for state in [4, 5, 6] {
        for armed in [0, 19] {
            for latched in [false, true] {
                for pressed in [false, true] {
                    let (mut looper, mut playback, context) = fixture(state);
                    looper.last_status.armed = armed;
                    looper.record_latched = latched && state != 6;
                    looper.overdub_latched = latched && state == 6;
                    let value = if pressed { 1.0 } else { 0.5 };
                    let out = button(&mut looper, &mut playback, &context, 0, value);
                    let expected = match (state, pressed) {
                        (_, false) if latched => None,
                        (6, _) => Some(2),
                        _ if armed != 0 => Some(1),
                        (_, true) => Some(4),
                        _ => Some(2),
                    };
                    assert_eq!(out, Vec::from_iter(expected.map(event)));
                    let next = match expected {
                        Some(4) => 6,
                        Some(action) => action,
                        None => state,
                    };
                    assert_eq!(playback.cached_state, next);
                }
            }
        }
    }
}

#[test]
fn play_start_stop_and_active_states() {
    let (mut looper, mut playback, context) = fixture(1);
    playback.incoming.loop_length = 99; // The block copy, not the controller copy, gates start.
    assert!(button(&mut looper, &mut playback, &context, 1, 1.0).is_empty());
    looper.last_status.loop_length = 73;
    assert_eq!(
        button(&mut looper, &mut playback, &context, 1, 1.0),
        [event(2)]
    );
    assert_eq!(playback.cached_state, 3);

    let (mut looper, mut playback, context) = fixture(2);
    playback.oneshot = true;
    assert_eq!(
        button(&mut looper, &mut playback, &context, 1, 1.0),
        [event(1), param(8, 0.0)]
    );
    assert_eq!(playback.cached_state, 1);

    for state in [4, 5, 6, 8, 9, 10, 11] {
        let (mut looper, mut playback, context) = fixture(state);
        assert!(button(&mut looper, &mut playback, &context, 1, 0.5).is_empty());
        assert_eq!(
            button(&mut looper, &mut playback, &context, 1, 1.0),
            [event(2)]
        );
        assert_eq!(playback.cached_state, 2);
    }
}

#[test]
fn play_quantized_states_use_controller_mask_and_clock() {
    for state in [2, 3] {
        let (mut looper, mut playback, context) = fixture(state);
        playback.oneshot = true;
        playback.incoming.events_waiting_for_quantize = 4;
        assert_eq!(
            button(&mut looper, &mut playback, &context, 1, 1.0),
            [event(0x10), reset_clock()]
        );
        assert_eq!(playback.cached_state, 2);
        assert!(playback.oneshot);
    }
    let (mut looper, mut playback, context) = fixture(3);
    looper.last_status.events_waiting_for_quantize = 4;
    assert!(button(&mut looper, &mut playback, &context, 1, 1.0).is_empty());
    assert_eq!(playback.cached_state, 3);
}

#[test]
fn undo_captures_controller_count_and_blocks_repeat_operations() {
    for state in 0..=12 {
        let (mut looper, mut playback, context) = fixture(state);
        playback.incoming.undo_count = 37;
        looper.last_status.undo_count = 21;
        assert!(button(&mut looper, &mut playback, &context, 2, 0.5).is_empty());
        assert!(!looper.operation_pending);
        assert_eq!(
            button(&mut looper, &mut playback, &context, 2, 1.0),
            [event(5)]
        );
        assert_eq!(looper.undo_count, 37);
        assert!(looper.operation_pending);
        assert_eq!(
            playback.cached_state,
            match state {
                3 | 4 | 5 | 8 | 11 => 1,
                6 | 9 | 10 => 2,
                _ => state,
            }
        );
        assert!(button(&mut looper, &mut playback, &context, 2, 1.0).is_empty());
    }
}

#[test]
fn duplicate_start_uses_sync_mode_except_from_state_one() {
    for state in [1, 2, 4, 6, 10] {
        for sync in [false, true] {
            let (mut looper, mut playback, context) = fixture(state);
            looper.duplicate_sync = sync;
            looper.duplicate_again = true;
            playback.oneshot = true;
            playback.dupwait = true;
            assert!(button(&mut looper, &mut playback, &context, 3, 0.5).is_empty());
            assert!(playback.oneshot);
            let waiting = sync && state != 1;
            assert_eq!(
                button(&mut looper, &mut playback, &context, 3, 1.0),
                [param(8, 0.0), event(if waiting { 9 } else { 6 })]
            );
            assert_eq!(playback.cached_state, if state == 1 { 8 } else { 9 });
            assert_eq!(playback.dupwait, waiting);
            assert!(!playback.oneshot);
            assert!(!looper.duplicate_again);
        }
    }
}

/// State 8 stops, state 9 restores the incoming state or duplicates again,
/// and other states only clear one-shot.
#[test]
fn duplicate_pending_states_and_unhandled_states() {
    for (incoming, action, next) in [(6, 4, 6), (10, 7, 10), (4, 3, 4), (91, 2, 2)] {
        let (mut looper, mut playback, context) = fixture(9);
        playback.incoming.state = incoming;
        playback.dupwait = true;
        assert_eq!(
            button(&mut looper, &mut playback, &context, 3, 1.0),
            [event(action)]
        );
        assert_eq!(playback.cached_state, next);
        assert!(!looper.duplicate_again);
    }
    let (mut looper, mut playback, context) = fixture(9);
    assert_eq!(
        button(&mut looper, &mut playback, &context, 3, 1.0),
        [event(6)]
    );
    assert_eq!(playback.cached_state, 9);
    assert!(looper.duplicate_again);
    for state in [0, 3, 8, 11] {
        let (mut looper, mut playback, context) = fixture(state);
        playback.oneshot = true;
        let mut expected = vec![param(8, 0.0)];
        expected.extend((state == 8).then(|| event(1)));
        assert_eq!(
            button(&mut looper, &mut playback, &context, 3, 1.0),
            expected
        );
        assert_eq!(playback.cached_state, if state == 8 { 1 } else { state });
    }
}

#[test]
fn oneshot_toggle_uses_loop_length_and_duplicate_incoming_state() {
    for oneshot in [false, true] {
        let (mut looper, mut playback, context) = fixture(9);
        playback.oneshot = oneshot;
        assert_eq!(
            button(&mut looper, &mut playback, &context, 4, 1.0),
            [event(8)]
        );
        assert_eq!(playback.cached_state, 2);
        assert_eq!(playback.oneshot, oneshot);
    }
    for state in [1, 2, 3, 4, 6, 10, 91] {
        for incoming in [2, 9] {
            for length in [0, 53] {
                for oneshot in [false, true] {
                    let (mut looper, mut playback, context) = fixture(state);
                    playback.incoming.state = incoming;
                    playback.oneshot = oneshot;
                    looper.last_status.loop_length = length;
                    let action = match state {
                        2 if incoming == 9 => Some(8),
                        1..=3 if !oneshot && length != 0 => Some(2),
                        6 | 10 if incoming == 9 => Some(8),
                        _ => None,
                    };
                    let mut expected = vec![param(8, if oneshot { 0.0 } else { 1.0 })];
                    expected.extend(action.map(event));
                    assert_eq!(
                        button(&mut looper, &mut playback, &context, 4, 1.0),
                        expected
                    );
                    assert_eq!(
                        playback.cached_state,
                        if action.is_some() { 2 } else { state }
                    );
                    assert_eq!(playback.oneshot, !oneshot);
                }
            }
        }
    }
}

/// Half Speed (P2) and Reverse (P6) skip the `+0xc8` gate and send on press only.
#[test]
fn half_speed_and_reverse_skip_pending_and_ignore_release() {
    for (index, parameter) in [(5, 2), (7, 6)] {
        let (mut looper, mut playback, context) = fixture(91);
        looper.operation_pending = true;
        for value in [0.0, 0.5, f32::NAN] {
            assert!(button(&mut looper, &mut playback, &context, index, value).is_empty());
        }
        assert_eq!(
            button(&mut looper, &mut playback, &context, index, 1.0),
            [param(parameter, 1.0)]
        );
        assert_eq!(playback.cached_state, 91);
    }
}

#[test]
fn punch_start_stop_and_release_use_armed_and_punch_mode() {
    for (state, next) in [(1, 11), (2, 10), (6, 10)] {
        let (mut looper, mut playback, context) = fixture(state);
        assert!(button(&mut looper, &mut playback, &context, 6, 0.5).is_empty());
        assert_eq!(
            button(&mut looper, &mut playback, &context, 6, 1.0),
            [event(7)]
        );
        assert_eq!(playback.cached_state, next);
    }
    for state in [10, 11] {
        for armed in [0, 29] {
            for latched in [false, true] {
                for value in [0.5, 1.0] {
                    let (mut looper, mut playback, context) = fixture(state);
                    looper.last_status.armed = armed;
                    looper.punch_latched = latched;
                    let out = button(&mut looper, &mut playback, &context, 6, value);
                    if value <= 0.5 && latched {
                        assert!(out.is_empty());
                        assert_eq!(playback.cached_state, state);
                    } else {
                        let next = if armed != 0 { 1 } else { 2 };
                        assert_eq!(out, [event(next)]);
                        assert_eq!(playback.cached_state, next);
                    }
                }
            }
        }
    }
}

#[test]
fn only_source_zero_runs_buttons_and_pending_blocks_six_actions() {
    for index in 0..=7 {
        let (mut looper, mut playback, context) = fixture(1);
        playback.oneshot = true;
        let before = playback;
        for mode in [ParamSetMode::Push, ParamSetMode::Recall] {
            let mut out = Vec::new();
            looper
                .set_parameter(index, 1.0, mode, Some(&mut playback), &context, &mut out)
                .unwrap();
            assert!(out.is_empty());
            assert_eq!(playback, before);
        }
        if !matches!(index, 5 | 7) {
            looper.operation_pending = true;
            assert!(button(&mut looper, &mut playback, &context, index, 1.0).is_empty());
            assert_eq!(playback, before);
        }
    }
}

/// 0x49a754 gates the P13–16 flags for every source; P15 clears dupwait
/// whenever the kept `+0xc0` is off.
#[test]
fn parameter_mode_gates_retain_flags_and_p15_clears_dupwait() {
    let (mut looper, mut playback, mut context) = fixture(1);
    let flags = |looper: &Looper| {
        [
            looper.record_latched,
            looper.overdub_latched,
            looper.duplicate_sync,
            looper.punch_latched,
        ]
    };
    let mut out = Vec::new();
    for (allowed, value, mode, expected) in [
        (true, 1.0, ParamSetMode::Push, true),
        (false, 0.0, ParamSetMode::Recall, true),
        (true, 0.5, ParamSetMode::Live, false),
    ] {
        context.mode_changes_allowed = Some(allowed);
        playback.dupwait = true;
        for index in 13..=16 {
            looper
                .set_parameter(index, value, mode, Some(&mut playback), &context, &mut out)
                .unwrap();
        }
        assert_eq!(flags(&looper), [expected; 4]);
        assert_eq!(playback.dupwait, expected);
    }
    playback.dupwait = true;
    context.mode_changes_allowed = Some(false);
    looper
        .set_parameter(
            15,
            1.0,
            ParamSetMode::Live,
            Some(&mut playback),
            &context,
            &mut out,
        )
        .unwrap();
    assert!(!playback.dupwait);
    assert!(out.is_empty());
}

#[test]
fn midi_clock_start_uses_both_caller_settings() {
    for enabled in [false, true] {
        for inhibited in [false, true] {
            let (mut looper, mut playback, mut context) = fixture(1);
            context.midi_clock_start = Some(LooperMidiClockStart { enabled, inhibited });
            assert_eq!(
                button(&mut looper, &mut playback, &context, 19, 1.0),
                [param(9, if enabled && !inhibited { 1.0 } else { 0.0 })]
            );
        }
    }
    let (mut looper, mut playback, context) = fixture(1);
    assert_eq!(
        button(&mut looper, &mut playback, &context, 19, f32::NAN),
        [param(9, 0.0)]
    );
}

/// The reset needs metronome slot 4 == ±0.0 (NaN blocks), both MIDI gates
/// clear and a zero P20 index; a nonzero index skips the external check.
#[test]
fn clock_check_uses_preroll_index_and_each_external_gate() {
    let open = fixture(1).2.clock.unwrap();
    for (clock, reset) in [
        (open, true),
        (
            LooperClock {
                metronome_slot_4: -0.0,
                ..open
            },
            true,
        ),
        (
            LooperClock {
                metronome_slot_4: f32::NAN,
                ..open
            },
            false,
        ),
        (
            LooperClock {
                midi_gate_f514: true,
                ..open
            },
            false,
        ),
        (
            LooperClock {
                midi_gate_fbb4: true,
                ..open
            },
            false,
        ),
    ] {
        let (mut looper, mut playback, mut context) = fixture(1);
        context.clock = Some(clock);
        let mut expected = vec![event(3)];
        expected.extend(reset.then(reset_clock));
        assert_eq!(
            button(&mut looper, &mut playback, &context, 0, 1.0),
            expected
        );
    }
    let (mut looper, mut playback, mut context) = fixture(1);
    looper.tables.pre_roll[1] = 0.0; // Synthetic table value: the index controls +0xd5.
    assert_eq!(
        button(&mut looper, &mut playback, &context, 20, 1.0 / 3.0),
        [param(10, 0.0)]
    );
    context.clock = None;
    assert_eq!(
        button(&mut looper, &mut playback, &context, 0, 1.0),
        [event(3)]
    );
}

#[test]
fn missing_context_is_an_error_before_any_state_or_output_change() {
    for index in [0, 13, 14, 15, 16, 19] {
        let (mut looper, mut playback, _) = fixture(1);
        playback.oneshot = true;
        playback.dupwait = true;
        let before = playback;
        let mut out = vec![event(91)];
        assert!(matches!(
            looper.set_parameter(
                index,
                1.0,
                ParamSetMode::Live,
                Some(&mut playback),
                &LooperContext::default(),
                &mut out
            ),
            Err(SetupError::UnrecoveredPath { .. })
        ));
        assert_eq!(playback, before);
        assert_eq!(out, [event(91)]);
        assert!(!looper.duplicate_sync && !looper.record_latched);
    }
}

/// Regression: 0x55e140 tests every release as `!(v > 0.5)`, so NaN is
/// a release for each button, not a press.
#[test]
fn nan_is_a_release_for_every_button() {
    for (index, state) in [(0, 1), (0, 2), (1, 2), (2, 3), (3, 1), (4, 1), (6, 2)] {
        let (mut looper, mut playback, context) = fixture(state);
        playback.oneshot = true;
        let before = playback;
        assert!(button(&mut looper, &mut playback, &context, index, f32::NAN).is_empty());
        assert_eq!(playback, before);
        assert!(!looper.operation_pending);
    }
    let (mut looper, mut playback, context) = fixture(4);
    assert_eq!(
        button(&mut looper, &mut playback, &context, 0, f32::NAN),
        [event(2)]
    );
}
