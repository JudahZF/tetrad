use super::*;
use crate::responses::IgnoreReason;
use crate::test_support::{FakeClock, FakeMemory, SharedFake, reply};
use crate::transport::{MESSAGE_MEMORY_BYTES, RingLayout};
use std::cell::RefCell;
use std::rc::Rc;
use tetrad_dsp_protocol::packet::BulkMode;
use tetrad_dsp_protocol::requests::{BypassState, Request, RequestFrame};
use tetrad_dsp_protocol::responses::{ResponseType, encode};
use tetrad_dsp_protocol::{DeploymentTarget, ModuleSlot};

const CORE2: CoreId = CoreId::Dsp(DspCore::Core2);
pub(super) const CORE0: CoreId = CoreId::Dsp(DspCore::Core0);
pub(super) const ALL_RINGS: [CoreId; 5] = RECEIVE_ORDER;

pub(super) type Sent = Rc<RefCell<Vec<(CoreId, Vec<u8>)>>>;

/// Scripted DSP: consumes every pending request in ring order, records it,
/// and publishes whatever `answer` returns.
pub(super) fn dsp(
    sent: Sent,
    mut answer: impl FnMut(CoreId, &[u8]) -> Vec<(CoreId, Vec<u8>)> + 'static,
) -> impl FnMut(&mut FakeMemory, Duration) + 'static {
    move |memory, _| {
        for core in ALL_RINGS {
            let layout = RingLayout::outgoing(core);
            while memory.bytes[layout.consumer_offset] != memory.bytes[layout.producer_offset] {
                let slot = (memory.bytes[layout.consumer_offset] + 1) % layout.slot_count;
                let length = layout.length_offset + usize::from(slot) * 4;
                let length =
                    u32::from_le_bytes(memory.bytes[length..length + 4].try_into().unwrap());
                let payload = layout.payload_offset + usize::from(slot) * layout.slot_bytes;
                let bytes = memory.bytes[payload..payload + length as usize].to_vec();
                memory.bytes[layout.consumer_offset] = slot;
                for (ring, response) in answer(core, &bytes) {
                    reply(memory, ring, &response);
                }
                sent.borrow_mut().push((core, bytes));
            }
        }
    }
}

pub(super) fn session(
    peer: impl FnMut(&mut FakeMemory, Duration) + 'static,
) -> DspSession<SharedFake, FakeClock> {
    let window = SharedFake::new(MESSAGE_MEMORY_BYTES);
    let clock = FakeClock::new(window.clone(), peer);
    DspSession::new(DspTransport::new(window).unwrap(), clock)
}

#[test]
fn stock_wait_semantics_and_four_strike_crash_policy() {
    let frame = RequestFrame::new(Request::DdrTestA).unwrap();
    let policy = Request::DdrTestA.send_policy();
    // A silent DSP: each synchronous send publishes, then times out at 500 ms.
    let mut silent = session(|_, _| {});
    for strike in 1..=3 {
        let started = silent.clock().now;
        let error = silent
            .send(ControlPacket::Request(&frame), policy)
            .unwrap_err();
        assert!(matches!(
            error,
            SessionError::SendTimeout { published: true, disposition: SendDisposition::Timeout { consecutive, .. }, .. }
                if consecutive == strike
        ));
        assert_eq!(silent.clock().now - started, Duration::from_millis(500));
    }
    assert!(matches!(
        silent.send(ControlPacket::Request(&frame), policy),
        Err(SessionError::SendTimeout {
            disposition: SendDisposition::Crashed,
            ..
        })
    ));
    assert_eq!(
        silent.next_event(),
        Some((
            CORE0,
            Event::Crash(CrashCause::SendTimeouts { core: CORE0 })
        ))
    );
    let before = silent.transport().memory().0.borrow().bytes.clone();
    assert!(matches!(
        silent.send(ControlPacket::Request(&frame), policy),
        Err(SessionError::DeadCore(CORE0))
    ));
    assert_eq!(silent.transport().memory().0.borrow().bytes, before);

    // A consuming DSP completes the synchronous wait; an asynchronous send
    // never waits and reports publication only.
    let sent = Sent::default();
    let mut live = session(dsp(sent.clone(), |_, _| Vec::new()));
    let delivery = live.send(ControlPacket::Request(&frame), policy).unwrap();
    assert!(delivery.consumed);
    assert_eq!(
        sent.borrow().as_slice(),
        [(CORE0, frame.as_bytes().to_vec())]
    );
    let meter = RequestFrame::new(Request::MeterRequest {
        core: CORE0,
        slot: 3,
        word: 7,
    })
    .unwrap();
    let delivery = live
        .send(
            ControlPacket::Request(&meter),
            meter.request().send_policy(),
        )
        .unwrap();
    assert!(!delivery.consumed);
    assert_eq!(live.comms().link(CORE0), Default::default());
}

#[test]
fn full_ring_waits_for_the_stock_timeout_without_counting_as_published() {
    let frame = RequestFrame::new(Request::EnableAll {
        core: DspCore::Core0,
        value: 1,
    })
    .unwrap();
    let policy = frame.request().send_policy();
    let mut full = session(|_, _| {});
    for _ in 0..9 {
        full.send(ControlPacket::Request(&frame), policy).unwrap();
    }
    let started = full.clock().now;
    assert!(matches!(
        full.send(ControlPacket::Request(&frame), policy),
        Err(SessionError::SendTimeout {
            published: false,
            ..
        })
    ));
    assert_eq!(full.clock().now - started, Duration::from_millis(100));
}

#[test]
fn awaited_response_is_removed_and_other_events_stay_queued_in_order() {
    let mut session = session(|memory, now| {
        if now == RESPONSE_POLL_INTERVAL {
            let meter = [1_u32, 2, 3, 4].map(u32::to_le_bytes).concat();
            reply(memory, CORE0, &encode(ResponseType::Meter, 0, &meter));
            let start = [0_u32, 5, 1].map(u32::to_le_bytes).concat();
            reply(memory, CORE0, &encode(ResponseType::ModelStart, 0, &start));
        }
    });
    let started = session
        .await_event(
            "model start",
            Duration::from_millis(1),
            |_, event| match event {
                Event::ModelStart { slot, started, .. } => Some((*slot, *started)),
                _ => None,
            },
        )
        .unwrap();
    assert_eq!(started, (5, true));
    assert!(matches!(
        session.next_event(),
        Some((CORE0, Event::Meter(_)))
    ));
    assert_eq!(session.next_event(), None);
    assert!(matches!(
        session.await_event("nothing", Duration::from_millis(1), |_, _| Some(())),
        Err(SessionError::ResponseTimeout("nothing"))
    ));
}

/// Publication order from the window's write log: `(core, type, length)`.
fn published(session: &DspSession<SharedFake, FakeClock>) -> Vec<(u8, u8, usize)> {
    let transport = session.transport();
    let memory = transport.memory().0.borrow();
    let mut result = Vec::new();
    for event in &memory.events {
        let crate::test_support::Event::Bytes { offset, data } = event else {
            continue;
        };
        for core in ALL_RINGS {
            let layout = RingLayout::outgoing(core);
            let ring = layout.payload_offset
                ..layout.payload_offset + usize::from(layout.slot_count) * layout.slot_bytes;
            if ring.contains(offset)
                && (offset - layout.payload_offset).is_multiple_of(layout.slot_bytes)
            {
                result.push((core.wire(), data[0], data.len()));
            }
        }
    }
    result
}

fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn word_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn header(bytes: &[u8]) -> (u8, u8, u32) {
    (
        bytes[0],
        bytes[1],
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
    )
}

#[derive(Default)]
struct FakeSoc2 {
    images_at: Vec<u32>,
    finished: bool,
    /// Supplies both lengths; otherwise the session derives the second.
    pair: bool,
}

impl Soc2Boot for FakeSoc2 {
    type Error = std::io::Error;
    fn buffer_bytes(&mut self) -> Result<u32, Self::Error> {
        Ok(0x30_0000)
    }
    fn write_images(&mut self, physical_address: u32) -> Result<u32, Self::Error> {
        self.images_at.push(physical_address);
        Ok(0x1234)
    }
    fn write_image_pair(
        &mut self,
        physical_address: u32,
    ) -> Result<(u32, Option<Soc2ImageLengths>), Self::Error> {
        let first = self.write_images(physical_address)?;
        let lengths = self.pair.then_some(Soc2ImageLengths {
            first,
            second: 0x2000,
        });
        Ok((first, lengths))
    }
    fn finish(&mut self) -> Result<bool, Self::Error> {
        self.finished = true;
        Ok(true)
    }
}

/// Owned (QC, 2026-10-15): a core 0 that consumes the handshake and `0x12` but answers
/// nothing fails the boot with [`CORE0_SILENT`] after [`CORE0_ANSWER_WAIT`], not after the 30 s
/// slave-boot wait, so the caller can restart SOC1 within the start-up guard.
#[test]
fn a_silent_core_0_fails_the_boot_fast() {
    use super::ops::CORE0_ANSWER_WAIT;
    let sent = Sent::default();
    let mut silent = session(dsp(sent.clone(), |_, _| Vec::new()));
    let started = silent.clock().now;
    let error = silent.boot(&mut FakeSoc2::default(), true).unwrap_err();
    assert!(matches!(error, SessionError::ResponseTimeout(what) if what == CORE0_SILENT));
    let waited = silent.clock().now - started;
    assert!(
        waited >= CORE0_ANSWER_WAIT && waited < CORE0_ANSWER_WAIT * 2,
        "{waited:?}"
    );
    assert!(sent.borrow().iter().any(|(_, bytes)| bytes[0] == 0x12));
}

/// Stock order (VAs 0x89c848, 0x89b7d4, 0x89bdec, 0x89c0bc): handshake,
/// `0x12`, image copy at the `0x05` address, `0x13`, then `0x14` on a zero
/// `0x06`. A failed boot retries once, then marks SOC2 dead. Frames use the
/// tetrad-loader layout: `0x12` size at `+12`; `0x13` first length at `+12`,
/// second at `+4` (review R1, R2).
#[test]
fn boot_handshake_slave_boot_and_key_pulls_follow_stock_order() {
    let script = |boot_status: u32| {
        move |_: CoreId, bytes: &[u8]| match bytes[0] {
            0x57 => [
                (ResponseType::Vector24, 0x41),
                (ResponseType::Vector25, 0x42),
                (ResponseType::Vector27, 0x43),
            ]
            .map(|(kind, fill)| (CORE0, encode(kind, 0, &[fill; 16])))
            .to_vec(),
            0x12 => vec![(
                CORE0,
                encode(
                    ResponseType::SlaveBootBufferAddress,
                    0,
                    &words(&[0x9000_0000]),
                ),
            )],
            0x13 => vec![(
                CORE0,
                encode(ResponseType::SlaveBootResponse, 0, &words(&[boot_status])),
            )],
            _ => Vec::new(),
        }
    };
    let sent = Sent::default();
    let mut good = session(dsp(sent.clone(), script(0)));
    let mut soc2 = FakeSoc2 {
        pair: true,
        ..FakeSoc2::default()
    };
    assert_eq!(
        good.boot(&mut soc2, true).unwrap(),
        SlaveBootOutcome::Soc2Open
    );
    assert!(good.is_soc2_open() && soc2.finished);
    assert_eq!(soc2.images_at, [0x9000_0000]);
    let words_4_12 = |bytes: &[u8]| (bytes[0], word_at(bytes, 4), word_at(bytes, 12));
    let headers: Vec<_> = sent.borrow().iter().map(|(_, b)| words_4_12(b)).collect();
    assert_eq!(
        headers,
        [
            (0x54, 0, 0),
            (0x55, 0, 0),
            (0x57, 0, 0),
            (0x12, 0, 0x30_0000),
            (0x13, 0x2000, 0x1234),
            (0x14, 0, 0)
        ]
    );
    let keys = good.pull_boot_keys().unwrap();
    assert_eq!(keys.repo.unwrap().as_bytes(), [0x41; 16]);
    assert_eq!(keys.blob.as_ref().unwrap().as_bytes(), [0x42; 16]);
    assert_eq!(format!("{:?}", keys.blob), "Some(KeyMaterial(16 bytes))");
    assert_eq!(keys.third.unwrap().as_bytes(), [0x43; 16]);
    assert_eq!(good.pull_boot_keys().unwrap().repo, None);

    let sent = Sent::default();
    let mut failing = session(dsp(sent.clone(), script(7)));
    let mut soc2 = FakeSoc2::default();
    assert_eq!(
        failing.boot(&mut soc2, true).unwrap(),
        SlaveBootOutcome::Soc2Dead
    );
    assert_eq!(soc2.images_at.len(), 2);
    assert_eq!(
        sent.borrow()
            .iter()
            .filter(|(_, bytes)| bytes[0] == 0x12)
            .count(),
        2
    );
    // Derived second length: buffer size − align16(first).
    let boot = sent
        .borrow()
        .iter()
        .find(|(_, b)| b[0] == 0x13)
        .unwrap()
        .1
        .clone();
    assert_eq!(word_at(&boot, 4), 0x30_0000 - 0x1240);
    assert!(failing.comms().can_send(CORE0));
    for core in [CORE2, CoreId::Dsp(DspCore::Core3), CoreId::Control] {
        assert!(!failing.comms().can_send(core));
    }
}

/// VAs 0x517010, 0x511084, 0x511188: a queue reaching 169 records flushes
/// every core; bypass flush is followed by `syncAllCores` and `0x3e`.
#[test]
fn bulk_scene_update_uses_stock_queue_flush_and_sync_order() {
    let target = |core, slot| DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(slot).unwrap(),
    };
    let update = |core, index| ParameterUpdate {
        target: target(core, 1),
        index,
        value_bits: 0,
    };
    let mut parameters = vec![update(DspCore::Core2, 0), update(DspCore::Core2, 1)];
    parameters.extend((0..170).map(|index| update(DspCore::Core0, index)));
    let bypasses = [BypassUpdate {
        target: target(DspCore::Core1, 4),
        state: BypassState::Bypassed,
    }];
    let mut session = session(dsp(Sent::default(), |_, _| Vec::new()));
    session.open_soc2();
    session
        .apply_bulk(BulkMode::Value16, &parameters, &bypasses)
        .unwrap();
    assert_eq!(
        published(&session),
        [
            (0, 0x1d, 16 + 169 * 12),
            (2, 0x1d, 16 + 2 * 12),
            (0, 0x1d, 16 + 12),
            (1, 0x3d, 16 + 8),
            (0, 0x0a, 16),
            (1, 0x0a, 16),
            (2, 0x0a, 16),
            (3, 0x0a, 16),
            (0, 0x3e, 16),
        ]
    );
}

/// VAs 0x582570, 0x515890, 0x89c680: `0x1e`, then a `0x1c` acknowledgement
/// selected by SOC and word 0. A missing acknowledgement marks the core dead.
#[test]
fn delete_all_waits_for_run_list_ack_and_crashes_a_silent_core() {
    let sent = Sent::default();
    let mut session = session(dsp(sent.clone(), |core, bytes| match (core, bytes[0]) {
        // Word 1 on SOC1 names core 1, so it must not satisfy core 0's wait.
        (CoreId::Dsp(DspCore::Core0), 0x1e) => [1, 0]
            .map(|word| {
                (
                    CORE0,
                    encode(ResponseType::Word1c, 0, &words(&[word, 0, 1])),
                )
            })
            .to_vec(),
        _ => Vec::new(),
    }));
    session.open_soc2();
    assert!(matches!(
        session.delete_all_models(&[DspCore::Core0, DspCore::Core2]),
        Err(SessionError::RunListTimeout(DspCore::Core2))
    ));
    assert_eq!(
        sent.borrow()
            .iter()
            .map(|(core, bytes)| (core.wire(), bytes[0]))
            .collect::<Vec<_>>(),
        [(0, 0x1e), (2, 0x1e)]
    );
    assert!(session.comms().can_send(CORE0));
    assert!(!session.comms().can_send(CORE2));
    let events: Vec<_> = std::iter::from_fn(|| session.next_event()).collect();
    assert_eq!(
        events,
        [
            (CORE0, Event::Word1c(1)),
            (
                CORE2,
                Event::Crash(CrashCause::RunListTimeout {
                    core: DspCore::Core2
                })
            )
        ]
    );
}

/// VAs 0x51965c, 0x54e710: core 0 data goes straight to the published
/// receive buffer, then `0x17`; core 2 data and sections use ring fragments.
#[test]
fn model_data_and_sections_use_direct_copy_on_soc1_cores_and_fragments_elsewhere() {
    use crate::model::{CoreWindows, ModelMemory};
    use tetrad_dsp_protocol::address::Address;
    let window = |core| ModelMemory::new(core, Address::new(0x2e_0000), FakeMemory::new(0x2000));
    let mut windows = CoreWindows::new(
        window(DspCore::Core0).unwrap(),
        window(DspCore::Core1).unwrap(),
    )
    .unwrap();
    let target = |core| DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(3).unwrap(),
    };
    let sent = Sent::default();
    let mut session = session(dsp(sent.clone(), |_, _| Vec::new()));
    session.open_soc2();
    {
        // Core 0 table: slot 3, buffer 2 -> entry (3 * 4 + 2) * 4.
        let transport = session.transport();
        let mut memory = transport.memory().0.borrow_mut();
        memory.bytes[0x4 + 56..0x4 + 60].copy_from_slice(&0x2e_0100_u32.to_le_bytes());
        memory.bytes[0x1e4 + 56..0x1e4 + 60].copy_from_slice(&8_u32.to_le_bytes());
    }
    assert!(matches!(
        session.send_model_data(&mut windows, target(DspCore::Core0), 2, &[1; 9]),
        Err(SessionError::Refused(_))
    ));
    assert!(sent.borrow().is_empty());
    session
        .send_model_data(&mut windows, target(DspCore::Core0), 2, &[7; 8])
        .unwrap();
    let core0_bytes = |windows: &mut CoreWindows<FakeMemory>| -> [u8; 8] {
        let core0 = windows.get_mut(DspCore::Core0).unwrap();
        core0.memory().bytes[0x100..0x108].try_into().unwrap()
    };
    assert_eq!(core0_bytes(&mut windows), [7; 8]);
    assert_eq!(
        sent.borrow()[0].1,
        words(&[0x0003_0017, 2, 16, 8]),
        "ReceiveDataEnd: type, core, slot, buffer, total, bytes"
    );
    // The Capture 2 clear (VA 0x5661ec): no copy, then `0x17` with 0 bytes (B7 on the QC: it
    // was refused as an empty model copy, so a V2 capture block never deployed).
    session
        .send_model_data(&mut windows, target(DspCore::Core0), 2, &[])
        .unwrap();
    assert_eq!(sent.borrow()[1].1, words(&[0x0003_0017, 2, 16, 0]));
    assert_eq!(core0_bytes(&mut windows), [7; 8], "nothing copied");
    session
        .send_model_data(&mut windows, target(DspCore::Core2), 1, &[5; 3000])
        .unwrap();
    session
        .load_section(&mut windows, target(DspCore::Core2), 4, 0x38_0000, &[6; 20])
        .unwrap();
    session
        .load_section(&mut windows, target(DspCore::Core1), 4, 0x2e_0010, &[6; 4])
        .unwrap();
    assert_eq!(
        windows.get_mut(DspCore::Core1).unwrap().memory().bytes[0x10..0x14],
        [6; 4]
    );
    assert_eq!(
        published(&session)[2..],
        [
            (2, 0x0b, 16 + 2032),
            (2, 0x0b, 16 + 968),
            (2, 0x07, 16 + 20)
        ]
    );
    // The section is still unconsumed: read it from the core 2 ring slot.
    let transport = session.transport();
    let memory = transport.memory().0.borrow();
    let layout = RingLayout::outgoing(CORE2);
    let slot = usize::from(memory.bytes[layout.producer_offset]);
    let load = &memory.bytes[layout.payload_offset + slot * layout.slot_bytes..][..36];
    assert_eq!(header(load), (0x07, 2, 0x38_0000));
    assert_eq!(load[2], 4);
}

/// VAs 0x585464, 0x879100: placed sections go to the core window (core 0)
/// or ring fragments (core 2), then `0x1c` records with the relocated
/// entries, then `syncAllCores`. A redeploy deletes and waits first.
#[test]
fn deploy_loads_sections_starts_models_and_deletes_before_redeploy() {
    use crate::model::{CoreWindows, ModelMemory};
    use qc_format::blob::{LinkRange, ModuleBlob, Section, SectionBytes};
    use tetrad_dsp_protocol::address::Address;
    let blob = |code: usize| ModuleBlob {
        contents: vec![SectionBytes {
            section: Section::Code,
            bytes: vec![0xaa; code],
        }],
        relocations: Vec::new(),
        ranges: vec![LinkRange::for_contents(Section::Code, code).unwrap()],
        entry: Section::Code.base() + 1,
    };
    let slot = |model_id, code| {
        Some(SlotModel {
            model_id,
            blob: (code > 0).then(|| blob(code)),
            layout_word: 7,
            block_word: -1,
        })
    };
    let window = |core| ModelMemory::new(core, Address::new(0x38_0000), FakeMemory::new(0x100));
    let mut windows = CoreWindows::new(
        window(DspCore::Core0).unwrap(),
        window(DspCore::Core1).unwrap(),
    )
    .unwrap();
    let sent = Sent::default();
    let mut session = session(dsp(sent.clone(), |core, bytes| match bytes[0] {
        0x1e => vec![(core, encode(ResponseType::Word1c, 0, &words(&[0, 0, 1])))],
        _ => Vec::new(),
    }));
    session.open_soc2();
    let cores = vec![
        (DspCore::Core0, vec![slot(16006, 4), None, slot(22004, 0)]),
        (DspCore::Core2, vec![slot(16006, 6)]),
    ];
    session.deploy(&mut windows, &cores).unwrap();
    // Slot 0 code at parcel 0x1c0000 -> byte 0x380000; entry parcel + 1.
    let core0 = windows.get_mut(DspCore::Core0).unwrap();
    assert_eq!(core0.memory().bytes[..4], [0xaa; 4]);
    // Records: {entry, slot u16 | layout u16 << 16, block word}.
    let records = words(&[
        0x1c_0001,
        0x0007_0000,
        u32::MAX,
        22004,
        0x0007_0002,
        u32::MAX,
    ]);
    let start0 = sent
        .borrow()
        .iter()
        .find(|(core, bytes)| *core == CORE0 && bytes[0] == 0x1c)
        .unwrap()
        .1
        .clone();
    assert_eq!(start0[16..], records);
    assert_eq!(
        published(&session),
        [
            (2, 0x07, 16 + 6),
            (0, 0x1c, 16 + 2 * 12),
            (2, 0x1c, 16 + 12),
            (0, 0x0a, 16),
            (1, 0x0a, 16),
            (2, 0x0a, 16),
            (3, 0x0a, 16),
        ]
    );
    // Every core that held models is cleared, listed or not (VA 0x582570).
    session
        .deploy(&mut windows, &[(DspCore::Core0, vec![slot(16006, 4)])])
        .unwrap();
    assert_eq!(
        published(&session)[7..10],
        [(0, 0x1e, 16), (2, 0x1e, 16), (0, 0x1c, 16 + 12)]
    );
}

/// M5 (VAs 0x5e62a0, 0x878aa0, 0x879100): one placement and one copy per
/// blob-list entry; slots that share an entry start at the same address.
/// Loads go round-robin over the cores and type 7 `+2` is the blob index.
/// A list that does not fit names its core and sends nothing.
#[test]
fn shared_blob_loads_copy_once_and_round_robin_over_cores() {
    use crate::model::{CoreWindows, ModelMemory};
    use qc_format::blob::{LinkRange, ModuleBlob, Section, SectionBytes};
    use tetrad_dsp_protocol::address::Address;
    let blob = |section: Section, bytes: usize| ModuleBlob {
        contents: vec![SectionBytes {
            section,
            bytes: vec![0xaa; bytes],
        }],
        relocations: Vec::new(),
        ranges: vec![LinkRange::for_contents(section, bytes).unwrap()],
        entry: section.base() + 1,
    };
    let load = |bytes| BlobLoad {
        model_id: 16006,
        blob: Some(blob(Section::Code, bytes)),
    };
    let slot = |model_id, blob| {
        Some(RunSlot {
            model_id,
            blob,
            layout_word: 0,
            block_word: -1,
        })
    };
    let window = |core| ModelMemory::new(core, Address::new(0x38_0000), FakeMemory::new(0x100));
    let mut windows = CoreWindows::new(
        window(DspCore::Core0).unwrap(),
        window(DspCore::Core1).unwrap(),
    )
    .unwrap();
    let sent = Sent::default();
    let mut session = session(dsp(sent.clone(), |_, _| Vec::new()));
    session.open_soc2();
    let cores = [
        CoreLoad {
            core: DspCore::Core3,
            blobs: vec![load(6)],
            slots: vec![slot(16006, Some(0))],
        },
        CoreLoad {
            core: DspCore::Core2,
            blobs: vec![load(4), load(8)],
            slots: vec![
                slot(16006, Some(0)),
                slot(16006, Some(0)),
                slot(22000, None),
                slot(16006, Some(1)),
            ],
        },
    ];
    session.deploy_loads(&mut windows, &cores).unwrap();
    let order: Vec<_> = published(&session)
        .into_iter()
        .filter(|&(_, kind, _)| kind == 0x07)
        .collect();
    assert_eq!(order, [(2, 7, 16 + 4), (3, 7, 16 + 6), (2, 7, 16 + 8)]);
    let indices: Vec<_> = sent
        .borrow()
        .iter()
        .filter(|(_, bytes)| bytes[0] == 0x07)
        .map(|(core, bytes)| (core.wire(), bytes[2]))
        .collect();
    assert_eq!(indices, [(2, 0), (2, 1), (3, 0)]);
    let start = sent
        .borrow()
        .iter()
        .find(|(core, bytes)| core.wire() == 2 && bytes[0] == 0x1c)
        .unwrap()
        .1
        .clone();
    let entries: Vec<_> = (0..4)
        .map(|record| word_at(&start, 16 + 12 * record))
        .collect();
    assert_eq!(entries, [0x1c_0001, 0x1c_0001, 22000, 0x1c_0003]);

    let sent_before = sent.borrow().len();
    let big = CoreLoad {
        core: DspCore::Core3,
        blobs: vec![BlobLoad {
            model_id: 16006,
            blob: Some(blob(Section::Dm, 0xe801)),
        }],
        slots: vec![slot(16006, Some(0))],
    };
    let error = session.deploy_loads(&mut windows, &[big]).unwrap_err();
    assert!(matches!(
        error,
        SessionError::WontFit {
            core: DspCore::Core3,
            section: Section::Dm
        }
    ));
    session.poll().unwrap();
    assert_eq!(sent.borrow().len(), sent_before);
}

/// The sidechain step's writes: in bulk mode a route request goes out at
/// once while parameters wait for the flush; `SyncAllCores` sends `0x0a` to
/// every core and then waits.
#[test]
fn executor_sends_requests_at_once_and_syncs_with_a_settle() {
    use crate::model::{CoreWindows, ModelMemory};
    use crate::setup::{DspWrite, StockPayload, StockTables, TableError};
    struct NoTables;
    impl StockTables for NoTables {
        fn read(&self, _: StockPayload) -> Result<Vec<u8>, TableError> {
            unreachable!("no stock data in this test")
        }
    }
    use tetrad_dsp_protocol::address::Address;
    let window = |core| ModelMemory::new(core, Address::new(0x38_0000), FakeMemory::new(0x10));
    let mut windows = CoreWindows::new(
        window(DspCore::Core0).unwrap(),
        window(DspCore::Core1).unwrap(),
    )
    .unwrap();
    let sent = Sent::default();
    let mut session = session(dsp(sent.clone(), |_, _| Vec::new()));
    session.open_soc2();
    let target = DeploymentTarget {
        core: DspCore::Core0,
        slot: ModuleSlot::try_from(3).unwrap(),
    };
    let route = Request::RouteToSidechainBuffer {
        core: DspCore::Core1,
        same_core: true,
        channel: 1,
        source_slot: 2,
        source_core: 0,
    };
    let writes = [
        DspWrite::parameter(target, 8, 0.5),
        DspWrite::Request(route),
        DspWrite::SyncAllCores {
            settle: Duration::from_millis(25),
        },
    ];
    let started = session.clock().now();
    session
        .execute(&mut windows, &NoTables, Some(BulkMode::Value16), &writes)
        .unwrap();
    assert!(session.clock().now() - started >= Duration::from_millis(25));
    session.clock_mut().sleep(Duration::from_millis(1));
    let kinds: Vec<_> = sent
        .borrow()
        .iter()
        .map(|(core, b)| (core.wire(), b[0]))
        .collect();
    assert_eq!(
        kinds,
        [
            (1, 0x49),
            (0, 0x0a),
            (1, 0x0a),
            (2, 0x0a),
            (3, 0x0a),
            (0, 0x1d)
        ]
    );
}

/// Review R4: a malformed response is acknowledged and queued as ignored
/// (stock dispatchers have no malformed state); the wait goes on.
#[test]
fn malformed_response_is_ignored_and_the_wait_continues() {
    let mut session = session(|memory, now| {
        if now == RESPONSE_POLL_INTERVAL {
            // A key vector with a length word larger than its body.
            reply(memory, CORE0, &[0x24, 0, 0, 0, 0xff, 0xff, 0, 0]);
            reply(
                memory,
                CORE0,
                &encode(ResponseType::Word1c, 0, &words(&[0, 0, 1])),
            );
        }
    });
    let word = session
        .await_event(
            "run list",
            Duration::from_millis(1),
            |_, event| match event {
                Event::Word1c(word) => Some(*word),
                _ => None,
            },
        )
        .unwrap();
    assert_eq!(word, 0);
    assert_eq!(
        session.next_event(),
        Some((
            CORE0,
            Event::Ignored {
                response_type: 0x24,
                reason: IgnoreReason::Malformed,
            }
        ))
    );
}

/// Review R7, R8: on a full ring, `0x2d` reports success like stock and a
/// meter request through `request` counts no crash strike (VAs 0x89c6fc,
/// 0x89b474).
#[test]
fn midi_clock_and_meter_sends_never_count_strikes() {
    let fill = RequestFrame::new(Request::EnableAll {
        core: DspCore::Core0,
        value: 1,
    })
    .unwrap();
    let mut full = session(|_, _| {});
    for _ in 0..9 {
        full.send(ControlPacket::Request(&fill), fill.request().send_policy())
            .unwrap();
    }
    let meter = Request::MeterRequest {
        core: CORE0,
        slot: 3,
        word: 7,
    };
    assert!(matches!(
        full.request(meter),
        Err(SessionError::NoFreeSlot { .. })
    ));
    assert_eq!(full.comms().link(CORE0), Default::default());
    // `0x2d` goes to the control core: fill its 14-slot ring first.
    let clock = tetrad_dsp_protocol::controls::RequestFrame::new(
        tetrad_dsp_protocol::controls::ControlRequest::MidiClockCountAndTempo {
            tempo_bits: 0,
            count: 1,
        },
    )
    .unwrap();
    let policy = SendPolicy::new(false, 100);
    full.open_soc2();
    for _ in 0..13 {
        full.send(ControlPacket::Controls(&clock), policy).unwrap();
    }
    let delivery = full.send(ControlPacket::Controls(&clock), policy).unwrap();
    assert_eq!(delivery.receipt, None);
}

#[test]
fn sync_all_cores_skips_a_dead_core_quietly() {
    let sent = Sent::default();
    let mut live = session(dsp(sent.clone(), |_, _| Vec::new()));
    live.mark_dead(&[CORE2]);
    assert!(live.sync_all_cores().is_ok());
    let cores: Vec<_> = sent.borrow().iter().map(|(core, _)| *core).collect();
    assert_eq!(cores.len(), 3);
    assert!(!cores.contains(&CORE2));
}

/// S2 on the QC (2026-10-15): a peer with unread messages in its outgoing ring stops consuming
/// (Control held 14 through start-up and four `wait` sends timed out). A waiting send drains
/// the receive rings, so the peer consumes and the drained message stays queued.
#[test]
fn waiting_send_drains_receive_rings_so_a_blocked_peer_consumes() {
    use tetrad_dsp_protocol::controls::{ControlRequest, RequestFrame as ControlFrame};
    let mut staged = false;
    let mut session = session(move |memory, _| {
        let incoming = RingLayout::incoming(CoreId::Control);
        if !staged {
            reply(memory, CoreId::Control, &[0x19; 8]);
            staged = true;
        } else if memory.bytes[incoming.consumer_offset] == memory.bytes[incoming.producer_offset] {
            let outgoing = RingLayout::outgoing(CoreId::Control);
            memory.bytes[outgoing.consumer_offset] = memory.bytes[outgoing.producer_offset];
        }
    });
    session.open_soc2();
    let frame = ControlFrame::new(ControlRequest::EnableUsbMidiClock { value: 1 }).unwrap();
    let delivery = session
        .send(ControlPacket::Controls(&frame), SendPolicy::new(true, 1000))
        .unwrap();
    assert!(delivery.consumed);
    assert_eq!(session.queued_events().count(), 1);
}
