use super::*;

fn reconcile(
    looper: &mut Looper,
    playback: &mut LooperPlayback,
    context: &LooperContext,
    status: LooperStatus,
) -> Vec<LooperWrite> {
    let mut out = Vec::new();
    looper.status(&status, playback, context, &mut out).unwrap();
    assert_eq!(looper.last_status, status);
    assert_eq!(playback.incoming, status);
    out
}

fn with_state(mut status: LooperStatus, state: u32) -> LooperStatus {
    status.state = state;
    status
}

/// First 0x55db24 rules: a changed block state moves pending cached states.
#[test]
fn state_change_moves_pending_cached_states() {
    for (cached, incoming, next, events) in [
        (5, 1, 1, vec![]),
        (3, 2, 2, vec![]),
        (3, 1, 1, vec![event(1)]),
        (11, 1, 1, vec![event(1)]),
        (8, 1, 1, vec![event(1)]),
        (11, 10, 10, vec![]),
        (5, 4, 4, vec![]),
        (6, 1, 6, vec![]),
    ] {
        let (mut looper, mut playback, context) = fixture(cached);
        let status = with_state(playback.incoming, incoming);
        assert_eq!(
            reconcile(&mut looper, &mut playback, &context, status),
            events
        );
        assert_eq!(playback.cached_state, next);
    }
}

#[test]
fn armed_falling_resets_clock_unless_preroll_or_an_external_gate_blocks_it() {
    for pre_roll in [false, true] {
        for metronome_slot_4 in [0.0, 7.0] {
            let (mut looper, mut playback, mut context) = fixture(1);
            looper.last_status.armed = 1;
            looper.pre_roll = pre_roll;
            context.clock.as_mut().unwrap().metronome_slot_4 = metronome_slot_4;
            playback.incoming.armed = 1;
            let status = LooperStatus {
                armed: 0,
                ..playback.incoming
            };
            assert_eq!(
                reconcile(&mut looper, &mut playback, &context, status),
                if !pre_roll && metronome_slot_4 == 0.0 {
                    vec![reset_clock()]
                } else {
                    vec![]
                },
            );
        }
    }
}

#[test]
fn dupwait_is_cleared_only_when_previous_block_state_was_nine() {
    for old in [4, 9] {
        let (mut looper, mut playback, context) = fixture(9);
        looper.last_status.state = old;
        playback.dupwait = true;
        let status = with_state(playback.incoming, 6);
        assert!(reconcile(&mut looper, &mut playback, &context, status).is_empty());
        assert_eq!(playback.dupwait, old != 9);
        assert_eq!(playback.cached_state, 9);
    }
}

#[test]
fn oneshot_stopped_requires_all_gates_and_clears_parameter_eight() {
    for state in [1, 2, 3, 4, 5, 6, 8, 10, 11] {
        let (mut looper, mut playback, context) = fixture(state);
        // Same state but changed record: the first state-change rules do not run.
        looper.last_status.state = 1;
        playback.oneshot = true;
        let mut status = with_state(playback.incoming, 1);
        status.one_shot_stopped = 27;
        let stop = matches!(state, 2 | 3 | 6 | 10);
        assert_eq!(
            reconcile(&mut looper, &mut playback, &context, status),
            if stop { vec![param(8, 0.0)] } else { vec![] }
        );
        assert_eq!(playback.oneshot, !stop);
        assert_eq!(playback.cached_state, if stop { 1 } else { state });
    }
    for (oneshot, stopped, waiting, incoming) in [
        (false, 1, 0, 1),
        (true, 0, 0, 1),
        (true, 1, 4, 1),
        (true, 1, 0, 2),
    ] {
        let (mut looper, mut playback, context) = fixture(6);
        playback.oneshot = oneshot;
        let mut status = with_state(playback.incoming, incoming);
        status.one_shot_stopped = stopped;
        status.events_waiting_for_quantize = waiting;
        assert!(reconcile(&mut looper, &mut playback, &context, status).is_empty());
        assert_eq!(playback.oneshot, oneshot);
        assert_eq!(playback.cached_state, 6);
    }
}

#[test]
fn duplicate_state_nine_to_two_clears_duplicate_again_after_dupwait() {
    for waiting in [false, true] {
        let (mut looper, mut playback, context) = fixture(9);
        looper.last_status.state = 9;
        looper.duplicate_again = true;
        playback.dupwait = waiting;
        let status = with_state(playback.incoming, 2);
        assert!(reconcile(&mut looper, &mut playback, &context, status).is_empty());
        assert_eq!(playback.cached_state, 2);
        assert!(!playback.dupwait);
        assert!(!looper.duplicate_again);
    }
}

/// Cached 8 → incoming 9, and cached 4 with block 4 → incoming 2, adopt `n`.
#[test]
fn duplicate_and_record_completion_adopt_the_incoming_state() {
    for (cached, old, incoming, next) in [(8, 0, 9, 9), (4, 4, 2, 2), (4, 6, 2, 4)] {
        let (mut looper, mut playback, context) = fixture(cached);
        looper.last_status.state = old;
        let status = with_state(playback.incoming, incoming);
        assert!(reconcile(&mut looper, &mut playback, &context, status).is_empty());
        assert_eq!(playback.cached_state, next);
    }
}

#[test]
fn undo_pending_clears_only_after_unsigned_count_increases() {
    for count in [8, 9, 10, u32::MAX] {
        for state in [1, 6] {
            let (mut looper, mut playback, context) = fixture(state);
            looper.operation_pending = true;
            looper.undo_count = 9;
            let mut status = with_state(playback.incoming, 1);
            status.undo_count = count;
            assert!(reconcile(&mut looper, &mut playback, &context, status).is_empty());
            assert_eq!(looper.operation_pending, count <= 9);
            assert_eq!(playback.cached_state, if count > 9 { 1 } else { state });
        }
    }
}

/// 0x513c94 drops a record equal to the controller copy; 0x55db24 then
/// dedupes against the block copy by raw bits.
#[test]
fn unchanged_status_dedupes_in_controller_then_block() {
    let (mut looper, mut playback, context) = fixture(3);
    let mut words = std::array::from_fn(|i| 100 + i as u32);
    words[0] = 1;
    words[1] = f32::NAN.to_bits();
    let status = LooperStatus::from_words(words);
    assert_eq!(
        reconcile(&mut looper, &mut playback, &context, status),
        [event(1)]
    );
    assert_eq!(looper.last_status.to_words(), words);
    playback.cached_state = 3;
    assert!(reconcile(&mut looper, &mut playback, &context, status).is_empty());
    // A new block sees the controller's copy: no signal, so no reconcile.
    let (mut fresh, _, _) = fixture(3);
    let mut out = Vec::new();
    fresh
        .status(&status, &mut playback, &context, &mut out)
        .unwrap();
    assert!(out.is_empty());
    assert_eq!(fresh.last_status, LooperStatus::from_words([0; 22]));
    let mut changed = status;
    changed.progress_bits = (-0.0_f32).to_bits();
    assert!(reconcile(&mut looper, &mut playback, &context, changed).is_empty());
    assert_eq!(playback.cached_state, 3); // Same incoming state skips state-change rules.
}

/// After block creation (`+0xf058`), the next record reaches the new block
/// even when the controller copy matches, then sets the cached state.
#[test]
fn first_status_after_block_creation_reaches_the_block_and_sets_cached_state() {
    let (mut looper, mut playback, context) = fixture(3);
    let status = with_state(playback.incoming, 1);
    playback.incoming = status;
    playback.adopt_next_state = true;
    assert_eq!(
        reconcile(&mut looper, &mut playback, &context, status),
        [event(1)]
    );
    assert!(!playback.adopt_next_state);
    assert_eq!(playback.cached_state, 1);
    let (mut looper, mut playback, context) = fixture(6);
    playback.adopt_next_state = true;
    let status = with_state(playback.incoming, 4);
    assert!(reconcile(&mut looper, &mut playback, &context, status).is_empty());
    assert_eq!(playback.cached_state, 4);
}

#[test]
fn missing_status_clock_context_leaves_both_status_copies_unchanged() {
    let (mut looper, mut playback, _) = fixture(3);
    looper.last_status.armed = 1;
    let before = playback;
    let old = looper.last_status;
    let incoming = with_state(playback.incoming, 1);
    let mut out = vec![event(91)];
    assert!(matches!(
        looper.status(
            &incoming,
            &mut playback,
            &LooperContext::default(),
            &mut out
        ),
        Err(SetupError::UnrecoveredPath { va: 0x5138dc, .. })
    ));
    assert_eq!(looper.last_status, old);
    assert_eq!(playback, before);
    assert_eq!(out, [event(91)]);
}

#[test]
fn local_teardown_sends_one_event_and_keeps_controller_state() {
    for state in 0..=12 {
        let (looper, mut playback, _) = fixture(state);
        playback.oneshot = true;
        playback.dupwait = true;
        let before = playback;
        let mut out = Vec::new();
        looper.teardown(TeardownMode::Local, &mut playback, &mut out);
        assert_eq!(
            out,
            match state {
                4 | 6 | 7 | 9 | 10 => vec![event(2)],
                5 | 8 | 11 => vec![event(1)],
                _ => vec![],
            }
        );
        assert_eq!(playback, before);
    }
}

#[test]
fn global_teardown_disconnects_both_paths_then_stops_and_resets_state() {
    for state in 0..=12 {
        let (looper, mut playback, _) = fixture(state);
        playback.oneshot = true;
        playback.dupwait = true;
        let mut out = Vec::new();
        looper.teardown(TeardownMode::Global, &mut playback, &mut out);
        let mut expected: Vec<_> = [0, 1]
            .map(|destination| {
                LooperWrite::Request(TunerLooperRequest::GlobalLooperInsertPath {
                    destination,
                    path: u32::MAX,
                })
            })
            .into();
        expected.extend(match state {
            2 | 3 | 5 | 8 | 11 => vec![event(1)],
            4 | 6 | 7 | 9 | 10 => vec![event(2), event(1)],
            _ => vec![],
        });
        assert_eq!(out, expected);
        assert_eq!(playback.cached_state, 1);
        assert!(playback.oneshot && playback.dupwait);
    }
}
