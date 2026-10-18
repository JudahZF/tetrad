use std::time::{Duration, Instant};

use tetrad_dsp_protocol::responses::{ResponseType, encode};
use tetrad_dsp_protocol::{CoreId, DspCore};

use super::*;
use crate::responses::Event;
use crate::session::{DspSession, SystemClock};
use crate::test_support::{FakeClock, SharedFake, SyncFake, reply, unread};
use crate::transport::MESSAGE_MEMORY_BYTES;

const CORE0: CoreId = CoreId::Dsp(DspCore::Core0);

/// SOC2 USB MIDI (`0x07` on Control) carrying `sequence` in its first word.
fn midi(sequence: u32) -> Vec<u8> {
    let body = [sequence, 0, 0].map(u32::to_le_bytes).concat();
    encode(ResponseType::MidiFromType07, 0, &body)
}

/// A SOC1 meter on core 0 carrying `sequence` in its value word.
fn meter(sequence: u32) -> Vec<u8> {
    let body = [0, 0, 0, sequence].map(u32::to_le_bytes).concat();
    encode(ResponseType::Meter, 0, &body)
}

/// The sequence numbers the session decoded, per ring, in decode order.
fn sequences<M: SharedMemory, C: crate::session::Clock>(
    session: &mut DspSession<M, C>,
) -> (Vec<u32>, Vec<u32>) {
    let (mut control, mut core0) = (Vec::new(), Vec::new());
    while let Some((ring, event)) = session.next_event() {
        match (ring, event) {
            (CoreId::Control, Event::MidiReceived { record, .. }) => control.push(record.words[0]),
            (CORE0, Event::Meter(record)) => core0.push(record.value_bits),
            other => panic!("unexpected {other:?}"),
        }
    }
    (control, core0)
}

/// A main loop that does not poll while the DSP sends 101 Control records and 51 core 0
/// records, seven times a ring's capacity. A drain every five records (the thread's tick)
/// keeps each ring at five unread or fewer. One poll then decodes every record in ring order;
/// the last record, still in its ring, comes after the drained ones.
#[test]
fn drains_during_a_main_loop_stall_keep_rings_low_and_order_intact() {
    let window = SharedFake::new(MESSAGE_MEMORY_BYTES);
    let clock = FakeClock::new(window.clone(), |_, _| {});
    let mut session = DspSession::new(DspTransport::new(window.clone()).unwrap(), clock);
    session.open_soc2();
    let mut fullest = 0;
    for sequence in 0..=100 {
        let mut memory = window.0.borrow_mut();
        reply(&mut memory, CoreId::Control, &midi(sequence));
        if sequence % 2 == 0 {
            reply(&mut memory, CORE0, &meter(sequence / 2));
        }
        fullest = fullest.max(unread(&memory, CoreId::Control));
        drop(memory);
        if sequence % 5 == 4 {
            session.rings().drain().unwrap();
        }
    }
    assert_eq!(fullest, 5);
    assert_eq!(unread(&window.0.borrow(), CoreId::Control), 1);
    assert_eq!(session.poll().unwrap(), 152);
    let (control, core0) = sequences(&mut session);
    assert_eq!(control, (0..=100).collect::<Vec<_>>());
    assert_eq!(core0, (0..=50).collect::<Vec<_>>());
    assert_eq!(session.rings().pending(), 0);
}

/// The stock thread drains without any session poll: three bursts of ten records each fit the
/// ring (13 usable slots), but thirty would not. Dropping the session joins the thread, so no reader is left on
/// the rings for a later session (the SOC1 retry).
#[test]
fn the_drain_thread_empties_rings_while_the_session_does_not_poll() {
    let window = SyncFake::new(MESSAGE_MEMORY_BYTES);
    let mut session = DspSession::new(
        DspTransport::new(window.clone()).unwrap(),
        SystemClock::new(),
    );
    session.open_soc2();
    session.start_drain::<StockThread>().unwrap();
    for burst in 0..3 {
        for sequence in burst * 10..burst * 10 + 10 {
            reply(&mut window.lock(), CoreId::Control, &midi(sequence));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while unread(&window.lock(), CoreId::Control) > 0 {
            assert!(Instant::now() < deadline, "the drain thread did not run");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(session.poll().unwrap(), 30);
    assert_eq!(sequences(&mut session).0, (0..30).collect::<Vec<_>>());

    drop(session);
    reply(&mut window.lock(), CoreId::Control, &midi(30));
    std::thread::sleep(DRAIN_PERIOD * 4);
    assert_eq!(unread(&window.lock(), CoreId::Control), 1);
}
