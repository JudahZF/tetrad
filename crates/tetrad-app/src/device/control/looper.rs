//! Looper (28) pushes: the values [`looper_pushes`] compares and the Looper messages it
//! builds. Moved from `control.rs` (round 37).

use tetrad_control::codecs::{
    ControlMessage, LooperMessage, LooperStatus, LooperUpdateType, MessageAction,
};

use tetrad_engine::Site;

use super::super::State;
use super::super::features::looper_slot;

/// The Looper values that [`looper_pushes`] compares: the DSP record and the controller's
/// state (`+0xb824`), one-shot play (`+0xb880`) and duplicate sync wait (`+0xb881`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LooperSeen {
    status: Option<tetrad_dsp_protocol::responses::LooperStatus>,
    state: u32,
    oneshot: bool,
    dupwait: bool,
    /// A Looper block was created (`+0xf058`) and its first record did not go out yet.
    awaiting_first: bool,
}

impl Default for LooperSeen {
    /// As `State::new`: no record, state 1 (empty).
    fn default() -> Self {
        Self {
            status: None,
            state: 1,
            oneshot: false,
            dupwait: false,
            awaiting_first: false,
        }
    }
}

/// The Looper (28) pushes of one device step, all UPDATE (LooperMessageSender `0x18f9d0`;
/// each sender returns while Control is not connected, `0x6e0444`), in order:
/// - a new DSP record (controller slot `0x513c94` → `0x18fa84`): `status`;
/// - a new controller state (`setLooperState` `0x513bbc` → `0x190908`): `state`;
/// - after a new record, `update_type` Meter (driver `0x55db24` → `0x511458(.., −2)` →
///   `0x18ff24`). Stock sends a second one when `events_waiting_for_quantize` changes.
///
/// Returns whether the delayed Buttons push restarts (`0x511458(.., −1)`): the driver calls
/// it when the record's state, armed, fixed duplicate cycles, waiting for cycle, reverse or
/// half speed word changes (`0x55db24`) and after a button parameter (`0x55e07c`). Inferred:
/// a button press shows here as a change of the controller state or flags.
pub(super) fn looper_pushes(seen: &mut LooperSeen, state: &State) -> (Vec<ControlMessage>, bool) {
    let playback = &state.looper_playback;
    let changed = state.looper.filter(|status| seen.status != Some(*status));
    // `0x513c94`: the first record after a block creation (`+0xf058`) goes out even when it
    // equals the copy. The flag clears when that record arrives (review r20 G8).
    let first = seen.awaiting_first && !playback.adopt_next_state;
    let new = LooperSeen {
        status: state.looper,
        state: playback.cached_state,
        oneshot: playback.oneshot,
        dupwait: playback.dupwait,
        awaiting_first: playback.adopt_next_state && changed.is_none(),
    };
    let old = std::mem::replace(seen, new);
    let mut buttons =
        (old.state, old.oneshot, old.dupwait) != (new.state, new.oneshot, new.dupwait);
    let record = changed.or(new.status.filter(|_| first));
    let mut pushes = Vec::new();
    if let Some(status) = &record {
        pushes.push(looper_update(LooperMessage {
            status: Some(looper_status(status)),
            ..Default::default()
        }));
    }
    if old.state != new.state {
        pushes.push(looper_update(LooperMessage {
            state: Some(new.state),
            ..Default::default()
        }));
    }
    if let Some(status) = record {
        pushes.push(looper_update(LooperMessage {
            update_type: Some(LooperUpdateType::Meter.into()),
            ..Default::default()
        }));
        let words = |status: &tetrad_dsp_protocol::responses::LooperStatus| {
            let flags = [
                status.armed,
                status.fixed_duplicate_cycles,
                status.waiting_for_cycle,
            ];
            (status.state, flags, status.in_reverse, status.half_speed)
        };
        buttons |= old.status.is_none_or(|old| words(&old) != words(&status));
    }
    (pushes, buttons)
}

/// A Looper push: `message` as UPDATE.
pub(super) fn looper_update(message: LooperMessage) -> ControlMessage {
    ControlMessage::Looper(LooperMessage {
        action: Some(MessageAction::Update.into()),
        ..message
    })
}

/// The 22 record words as `LooperStatus` (`0x18fa84`, READ `0x18bad8`). The fields are
/// proto3 implicit, so a zero word is left out (in the stock S7 capture, records 1765 and 1776
/// carry only state, free samples, armed and the two NaN displacements); a float counts as zero
/// only with all bits clear.
pub(super) fn looper_status(status: &tetrad_dsp_protocol::responses::LooperStatus) -> LooperStatus {
    let word = |value: u32| (value != 0).then_some(value);
    let float = |bits: u32| (bits != 0).then(|| f32::from_bits(bits));
    LooperStatus {
        state: word(status.state),
        progress: float(status.progress_bits),
        undo_progress: float(status.undo_progress_bits),
        duplicate_cycle: word(status.duplicate_cycle),
        num_duplicate_cycles: word(status.num_duplicate_cycles),
        one_shot_stopped: word(status.one_shot_stopped),
        redo_available: word(status.redo_available),
        loop_length: word(status.loop_length),
        free_samples: word(status.free_samples),
        in_reverse: word(status.in_reverse),
        one_shot: word(status.one_shot),
        half_speed: word(status.half_speed),
        fixed_duplicate_cycles: word(status.fixed_duplicate_cycles),
        armed: word(status.armed),
        waiting_for_cycle: word(status.waiting_for_cycle),
        undo_count: word(status.undo_count),
        max_write_displacement: float(status.max_write_displacement_bits),
        min_write_displacement: float(status.min_write_displacement_bits),
        events_waiting_for_quantize: word(status.events_waiting_for_quantize),
        current_clock: word(status.current_clock),
        transition: word(status.transition),
        action: word(status.action),
    }
}

/// Looper X parameter 18 (QUANTIZE): the driver write (`0x55e140` case `0x12`) maps it to
/// table `0xa553c8` = [0, 24, 48, …, 192, 384] at `trunc(v · 9 + 0.5)` (clamped to 9) and
/// stores `0 < entry` as the controller flag `+0xb882` (`0x513df0`).
const QUANTIZE: usize = 18;

/// The controller quantize flag `+0xb882` for the deployed Looper X ([`looper_slot`]) at the
/// playing scene; `None` without a Looper X. Inferred: stock keeps the flag of the last
/// driver write, which is the current value after every deploy and recall.
pub(in crate::device) fn quantize_enabled(state: &State) -> Option<bool> {
    let slot = looper_slot(state)?;
    let current = state.current.as_ref()?;
    let block = current.deployment.layout().instance(Site::Slot(slot))?;
    let value = block.params.get(QUANTIZE)?.at(current.deployment.scene());
    Some(value * 9.0 + 0.5 >= 1.0)
}

/// The Looper pushes of a Looper X driver write in a deploy or a scene recall
/// (`0x55e140` with a forced mode): `quantize_enabled` (`0x513df0` → `0x190c4c`), then
/// `update_type` Meter twice (`0x511458(.., −2)`, stock `[1649]`/`[1650]` and
/// `[1751]`/`[1752]`; inferred: the second from parameter 22, `0x55e07c` case `0x16`). The
/// same writes restart the Buttons timer (`0x511458(.., −1)`); [`super::Control::notify`]
/// does that when the quantize push goes out. Empty without a Looper X.
pub(super) fn looper_writes(state: &State) -> Vec<ControlMessage> {
    let Some(quantize) = quantize_enabled(state) else {
        return Vec::new();
    };
    let meter = || {
        looper_update(LooperMessage {
            update_type: Some(LooperUpdateType::Meter.into()),
            ..Default::default()
        })
    };
    vec![
        looper_update(LooperMessage {
            quantize_enabled: Some(quantize),
            ..Default::default()
        }),
        meter(),
        meter(),
    ]
}
