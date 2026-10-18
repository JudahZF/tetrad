use super::*;
use crate::test_support::{Access, Event, FakeMemory};
use tetrad_dsp_protocol::{
    CodecLimits, DeploymentTarget, ModuleSlot,
    fragment::{Fragmenter, MessageMetadata, Reassembler},
};

const CORES: [CoreId; 5] = [
    CoreId::Dsp(DspCore::Core0),
    CoreId::Dsp(DspCore::Core1),
    CoreId::Dsp(DspCore::Core2),
    CoreId::Dsp(DspCore::Core3),
    CoreId::Control,
];

#[test]
fn synchronous_flag_publication_waits_on_consumer_for_every_core_and_rejects_old_receipts() {
    use tetrad_dsp_protocol::controls::{ControlRequest, RequestFrame};
    use tetrad_dsp_protocol::synchronization::DiagnosticsRequestFrame;
    for (core, flag) in
        CORES
            .into_iter()
            .zip([None, None, Some(0x178d7), Some(0x178e1), Some(0x178c9)])
    {
        let mut transport = DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES)).unwrap();
        let diagnostic = DiagnosticsRequestFrame::new(match core {
            CoreId::Dsp(core) => core,
            CoreId::Control => DspCore::Core0,
        });
        let status = RequestFrame::new(ControlRequest::MetronomeStatusRequest).unwrap();
        let packet = if core == CoreId::Control {
            ControlPacket::Controls(&status)
        } else {
            ControlPacket::Diagnostics(&diagnostic)
        };
        let receipt = transport.send_control_with_wait_flag(packet, true).unwrap();
        assert_eq!(receipt.core, core);
        if let Some(flag) = flag {
            assert_eq!(transport.memory.bytes[flag], 1);
        }
        let before = transport.memory.bytes.clone();
        assert!(!transport.is_consumed(receipt).unwrap());
        assert_eq!(transport.memory.bytes, before);
        // The wait flag stays one. Completion is the peer consumer index.
        transport.memory.bytes[RingLayout::outgoing(core).consumer_offset] = receipt.slot;
        assert!(transport.is_consumed(receipt).unwrap());
        if let Some(flag) = flag {
            assert_eq!(transport.memory.bytes[flag], 1);
        }
        transport.send_control(packet).unwrap();
        assert_eq!(
            transport.is_consumed(receipt),
            Err(TransportError::ReceiptSuperseded)
        );
        let invalid = SendReceipt {
            core,
            slot: 255,
            byte_length: 16,
        };
        assert_eq!(
            transport.is_consumed(invalid),
            Err(TransportError::ReceiptSuperseded)
        );
    }
}

#[test]
fn typed_metronome_block_publishes_one_exact_generic_frame() {
    use tetrad_dsp_protocol::controls::MetronomeParameterBlock;
    let block = MetronomeParameterBlock::new(std::array::from_fn(|index| index as u32));
    let mut transport = DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES)).unwrap();
    let receipt = transport
        .send_control(ControlPacket::MetronomeBlock(&block))
        .unwrap();
    assert_eq!(
        receipt,
        SendReceipt {
            core: CoreId::Control,
            slot: 1,
            byte_length: 108
        }
    );
    let bytes = &transport.memory.bytes[0xb7e8..0xb854];
    assert_eq!(
        &bytes[..16],
        &[0x46, 4, 0, 0, 0, 0, 0, 0, 108, 0, 0, 0, 92, 0, 0, 0]
    );
    for (index, word) in bytes[16..].chunks_exact(4).enumerate() {
        assert_eq!(word, &(index as u32).to_le_bytes());
    }
}

#[test]
fn typed_control_requests_publish_the_complete_packet_without_an_extra_fragment_header() {
    use tetrad_dsp_protocol::controls::{ControlRequest, RequestFrame};
    use tetrad_dsp_protocol::hid::HidReportFrame;
    use tetrad_dsp_protocol::tuner_looper::{RequestFrame as TunerFrame, TunerLooperRequest};

    let tuner = TunerFrame::new(TunerLooperRequest::GlobalTunerInputPath { port: 7 });
    let control = RequestFrame::new(ControlRequest::MetronomeDestination {
        value_bits: 19_f32.to_bits(),
    })
    .unwrap();
    let hid = HidReportFrame::from_template([0xa5; 16], &[0x5a; 128]);
    let mut transport = DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES)).unwrap();
    for (packet, core, offset, bytes) in [
        (
            ControlPacket::TunerLooper(&tuner),
            CoreId::Dsp(DspCore::Core0),
            0xf98,
            tuner.as_bytes(),
        ),
        (
            ControlPacket::Controls(&control),
            CoreId::Dsp(DspCore::Core1),
            0x6778,
            control.as_bytes(),
        ),
        (
            ControlPacket::Hid(&hid),
            CoreId::Control,
            0xb7e8,
            hid.as_bytes().as_slice(),
        ),
    ] {
        let receipt = transport.send_control(packet).unwrap();
        assert_eq!(receipt.core, core);
        assert_eq!(receipt.slot, 1);
        assert_eq!(receipt.byte_length, bytes.len());
        assert_eq!(
            &transport.memory().bytes[offset..offset + bytes.len()],
            bytes
        );
    }
    assert_eq!(
        &transport.memory().bytes[0xb7e8..0xb7f8],
        &[
            0x27, 4, 0, 0xa5, 0, 0, 0, 0, 0x90, 0, 0, 0, 0xa5, 0xa5, 0xa5, 0xa5
        ]
    );
}

fn fragments(core: CoreId, payload: &[u8]) -> Fragmenter<'_> {
    Fragmenter::new(
        MessageMetadata {
            message_type: 6,
            core,
            slot: 29,
            argument: 0x1234_5678,
        },
        payload,
        CodecLimits::default(),
    )
    .unwrap()
}

#[test]
fn recovered_offsets_next_slot_and_publication_order_for_every_outgoing_ring() {
    // Independent offset vectors from VA 0x54e0ac, not derived from RingLayout.
    let expected = [
        (0x784, 0x798, 0x176d8, None),
        (0x788, 0x5f78, 0x17738, None),
        (0x790, 0xc718, 0x17808, Some(0x178d6)),
        (0x794, 0x11ef8, 0x17868, Some(0x178e0)),
        (0x78c, 0xb758, 0x17798, Some(0x178c8)),
    ];
    for (core, (producer, payload, length, flag)) in CORES.into_iter().zip(expected) {
        let mut memory = FakeMemory::new(MESSAGE_MEMORY_BYTES);
        memory.bytes.fill(0xa5);
        memory.bytes[producer..producer + 2].fill(0);
        let before = memory.bytes.clone();
        let mut transport = DspTransport::new(memory).unwrap();
        assert_eq!(
            transport.memory().bytes,
            before,
            "construction must not clear memory"
        );
        let fragment = fragments(core, &[9, 8, 7]).next().unwrap();
        let wire = fragment.to_bytes();
        let receipt = transport.try_send_fragment(fragment).unwrap();
        assert_eq!(
            receipt,
            SendReceipt {
                core,
                slot: 1,
                byte_length: wire.len()
            }
        );
        let capacity = if core == CoreId::Control { 144 } else { 2048 };
        let mut expected_bytes = before;
        expected_bytes[payload + capacity..payload + capacity + wire.len()].copy_from_slice(&wire);
        expected_bytes[length + 4..length + 8].copy_from_slice(&(wire.len() as u32).to_le_bytes());
        expected_bytes[producer] = 1;
        let mut events = vec![
            Event::Acquire,
            Event::Bytes {
                offset: payload + capacity,
                data: wire,
            },
            Event::Barrier,
            Event::Word {
                offset: length + 4,
                value: 19,
            },
        ];
        if let Some(flag) = flag {
            expected_bytes[flag + 1] = 0;
            events.push(Event::Bytes {
                offset: flag + 1,
                data: vec![0],
            });
        }
        events.extend([
            Event::Barrier,
            Event::Bytes {
                offset: producer,
                data: vec![1],
            },
        ]);
        assert_eq!(transport.memory().bytes, expected_bytes);
        assert_eq!(transport.memory().events, events);
    }
}

#[test]
fn parameter_packet_keeps_opaque_bytes_and_exceptional_float_bits() {
    let target = DeploymentTarget {
        core: DspCore::Core1,
        slot: ModuleSlot::try_from(7).unwrap(),
    };
    let packet = ParameterChange::from_template(
        [0xa5; 20],
        target,
        0x1020_3040,
        f32::from_bits(0x7fc0_1234),
    );
    let mut transport = DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES)).unwrap();
    transport.send_parameter(&packet).unwrap();
    let published = &transport.memory().bytes[0x6778..0x678c];
    assert_eq!(published, packet.as_bytes());
    assert_eq!(published[3], 0xa5);
    assert_eq!(&published[12..16], &[0xa5; 4]);
    assert_eq!(&published[16..20], &0x7fc0_1234_u32.to_le_bytes());
    assert_eq!(transport.memory().bytes[0x788], 1);
    assert_eq!(transport.memory().bytes[0x784], 0);
}

#[test]
fn full_ring_refuses_without_writes_then_wraps_after_consumer_progress() {
    for core in CORES {
        let layout = RingLayout::outgoing(core);
        let mut transport = DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES)).unwrap();
        let fragment = fragments(core, &[]).next().unwrap();
        for slot in 1..layout.slot_count {
            assert_eq!(transport.try_send_fragment(fragment).unwrap().slot, slot);
        }
        let bytes = transport.memory().bytes.clone();
        let events = transport.memory().events.len();
        assert_eq!(
            transport.try_send_fragment(fragment),
            Err(TransportError::RingFull(core))
        );
        assert_eq!(transport.memory().bytes, bytes);
        assert_eq!(transport.memory().events.len(), events);
        // Synthetic DSP consumes one published slot; the next host slot wraps to 0.
        transport.memory.bytes[layout.consumer_offset] = 1;
        assert_eq!(transport.try_send_fragment(fragment).unwrap().slot, 0);
        assert_eq!(transport.memory().bytes[layout.producer_offset], 0);
    }
}

#[test]
fn multi_fragment_publication_preserves_protocol_and_requires_explicit_resume() {
    let core = CoreId::Control;
    let payload: Vec<u8> = (0..1900).map(|index| index as u8).collect();
    let mut transport = DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES)).unwrap();
    let layout = RingLayout::outgoing(core);
    let mut reassembler = Reassembler::new(CodecLimits::default());
    let mut result = None;
    for (index, fragment) in fragments(core, &payload).enumerate() {
        if index == 13 {
            assert_eq!(
                transport.try_send_fragment(fragment),
                Err(TransportError::RingFull(core))
            );
            // Already-published fragments stay committed. Explicit peer progress permits resume.
            transport.memory.bytes[layout.consumer_offset] = 1;
        } else if index > 13 {
            transport.memory.bytes[layout.consumer_offset] += 1;
        }
        let receipt = transport.try_send_fragment(fragment).unwrap();
        let offset = layout.payload_offset + usize::from(receipt.slot) * layout.slot_bytes;
        let wire = &transport.memory().bytes[offset..offset + receipt.byte_length];
        result = reassembler
            .push(Fragment::decode(wire, CodecLimits::default()).unwrap())
            .unwrap();
    }
    assert_eq!(result.unwrap().payload, payload);
    reassembler.finish().unwrap();
}

#[test]
fn incoming_offsets_copy_before_release_and_wrap_for_every_ring() {
    let expected = [
        (0x786, 0x5798, 0x17700),
        (0x78a, 0xaf78, 0x17760),
        (0x792, 0x11718, 0x17830),
        (0x796, 0x16ef8, 0x17890),
        (0x78e, 0xbf38, 0x177d0),
    ];
    for (core, (producer, payload, length)) in CORES.into_iter().zip(expected) {
        let mut memory = FakeMemory::new(MESSAGE_MEMORY_BYTES);
        memory.bytes[producer + 1] = 13;
        memory.bytes[payload..payload + 3].copy_from_slice(&[3, 2, 1]);
        memory.bytes[length..length + 4].copy_from_slice(&3_u32.to_le_bytes());
        let before = memory.bytes.clone();
        let mut transport = DspTransport::new(memory).unwrap();
        assert_eq!(transport.try_receive(core).unwrap(), Some(vec![3, 2, 1]));
        let mut expected = before;
        expected[producer + 1] = 0;
        assert_eq!(transport.memory().bytes, expected);
        assert_eq!(
            transport.memory().events,
            [
                Event::Acquire,
                Event::Acquire,
                Event::Barrier,
                Event::Bytes {
                    offset: producer + 1,
                    data: vec![0]
                }
            ]
        );
        assert_eq!(transport.try_receive(core).unwrap(), None);
        transport.memory.bytes[producer] = 1;
        transport.memory.events.clear();
        let before = transport.memory().bytes.clone();
        for _ in 0..2 {
            assert_eq!(transport.try_receive(core).unwrap(), None);
            assert_eq!(transport.memory().bytes, before);
        }
        assert_eq!(transport.memory().events, [Event::Acquire, Event::Acquire]);
        // The peer finishes publishing the same slot after an early producer update.
        transport.memory.bytes[payload + 144..payload + 147].copy_from_slice(&[9, 8, 7]);
        transport.memory.bytes[length + 4..length + 8].copy_from_slice(&3_u32.to_le_bytes());
        assert_eq!(transport.try_receive(core).unwrap(), Some(vec![9, 8, 7]));
        assert_eq!(transport.memory().bytes[producer + 1], 1);
        assert_eq!(transport.try_receive(core).unwrap(), None);
    }
}

#[test]
fn peer_observations_precede_slot_access_and_payload_copy_precedes_ack() {
    let core = CoreId::Control;
    let mut memory = FakeMemory::new(MESSAGE_MEMORY_BYTES);
    memory.bytes[0x78e] = 1;
    memory.bytes[0x177d4..0x177d8].copy_from_slice(&1_u32.to_le_bytes());
    memory.bytes[0xbfc8] = 0xa5;
    let mut transport = DspTransport::new(memory).unwrap();
    assert_eq!(transport.try_receive(core).unwrap(), Some(vec![0xa5]));
    assert_eq!(
        *transport.memory().accesses.borrow(),
        [
            Access::Read(0x78e),
            Access::Acquire,
            Access::Read(0x177d4),
            Access::Acquire,
            Access::Read(0xbfc8),
            Access::Publish,
            Access::Write(0x78f),
        ]
    );
    transport.memory.accesses.get_mut().clear();
    transport
        .try_send_fragment(fragments(core, &[]).next().unwrap())
        .unwrap();
    assert_eq!(
        *transport.memory().accesses.borrow(),
        [
            Access::Read(0x78c),
            Access::Acquire,
            Access::Write(0xb7e8),
            Access::Publish,
            Access::Write(0x1779c),
            Access::Write(0x178c9),
            Access::Publish,
            Access::Write(0x78c),
        ]
    );
}

#[test]
fn malformed_incoming_length_or_failed_read_does_not_release_slot() {
    let core = CoreId::Dsp(DspCore::Core0);
    let layout = RingLayout::incoming(core);
    let mut memory = FakeMemory::new(MESSAGE_MEMORY_BYTES);
    memory.bytes[layout.producer_offset] = 1;
    memory.bytes[layout.length_offset + 4..layout.length_offset + 8]
        .copy_from_slice(&145_u32.to_le_bytes());
    let mut transport = DspTransport::new(memory).unwrap();
    assert_eq!(
        transport.try_receive(core),
        Err(TransportError::InvalidLength {
            slot: 1,
            bytes: 145,
            capacity: 144
        })
    );
    assert_eq!(transport.memory().events, [Event::Acquire]);
    assert_eq!(transport.memory().bytes[layout.consumer_offset], 0);
    transport.memory.bytes[layout.length_offset + 4..layout.length_offset + 8]
        .copy_from_slice(&144_u32.to_le_bytes());
    transport.memory.fail_read_offset = Some(layout.payload_offset + 144);
    assert!(matches!(
        transport.try_receive(core),
        Err(TransportError::Memory(_))
    ));
    assert!(!transport.is_faulted());
    assert_eq!(
        transport.memory().events,
        [Event::Acquire, Event::Acquire, Event::Acquire]
    );
    assert_eq!(transport.memory().bytes[layout.consumer_offset], 0);
    transport.memory.fail_read_offset = None;
    assert_eq!(transport.try_receive(core).unwrap(), Some(vec![0; 144]));
}

#[test]
fn invalid_indices_short_window_and_special_paths_refuse_without_writes() {
    assert!(matches!(
        DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES - 1)),
        Err(TransportError::Memory(MemoryError::OutOfBounds { .. }))
    ));
    for core in CORES {
        let mut transport = DspTransport::new(FakeMemory::new(MESSAGE_MEMORY_BYTES)).unwrap();
        let fragment = fragments(core, &[]).next().unwrap();
        for layout in [RingLayout::outgoing(core), RingLayout::incoming(core)] {
            for offset in [layout.producer_offset, layout.consumer_offset] {
                transport.memory.bytes[offset] = layout.slot_count;
                let error = if layout.slot_bytes == 2048 || layout.producer_offset == 0x78c {
                    transport.try_send_fragment(fragment).unwrap_err()
                } else {
                    transport.try_receive(core).unwrap_err()
                };
                assert_eq!(
                    error,
                    TransportError::InvalidIndex {
                        offset,
                        value: layout.slot_count,
                        slot_count: layout.slot_count
                    }
                );
                transport.memory.bytes[offset] = 0;
            }
        }
        let direct = matches!(core, CoreId::Dsp(DspCore::Core0 | DspCore::Core1));
        for message_type in [7, 11] {
            // Cores 0/1 take these as direct copies; the ring must refuse them.
            let mut wire = fragment.to_bytes();
            wire[0] = message_type;
            let special = Fragment::decode(&wire, CodecLimits::default()).unwrap();
            if direct {
                assert_eq!(
                    transport.try_send_fragment(special),
                    Err(TransportError::SpecialMessageType(message_type))
                );
            } else {
                transport.try_send_fragment(special).unwrap();
            }
        }
        assert_eq!(transport.memory().events.is_empty(), direct);
    }
}

#[test]
fn any_failed_publication_write_faults_even_if_backend_already_mutated() {
    let core = CoreId::Dsp(DspCore::Core2);
    let fragment = fragments(core, &[1]).next().unwrap();
    for fail_write in 0..4 {
        for mutate_on_failure in [false, true] {
            let mut memory = FakeMemory::new(MESSAGE_MEMORY_BYTES);
            memory.fail_write = Some(fail_write);
            memory.mutate_on_failure = mutate_on_failure;
            let mut transport = DspTransport::new(memory).unwrap();
            assert!(matches!(
                transport.try_send_fragment(fragment),
                Err(TransportError::Memory(_))
            ));
            assert!(transport.is_faulted());
            let events = transport.memory().events.len();
            assert_eq!(
                transport.try_send_fragment(fragment),
                Err(TransportError::Faulted)
            );
            assert_eq!(transport.try_receive(core), Err(TransportError::Faulted));
            assert_eq!(transport.memory().events.len(), events);
            assert_eq!(
                transport.memory().bytes[0x790],
                u8::from(fail_write == 3 && mutate_on_failure)
            );
        }
    }
}

#[test]
fn failed_consumer_release_never_reports_a_successful_receive() {
    let core = CoreId::Control;
    for mutate_on_failure in [false, true] {
        let mut memory = FakeMemory::new(MESSAGE_MEMORY_BYTES);
        memory.bytes[0x78e] = 1;
        memory.bytes[0x177d4..0x177d8].copy_from_slice(&1_u32.to_le_bytes());
        memory.fail_write = Some(0);
        memory.mutate_on_failure = mutate_on_failure;
        let mut transport = DspTransport::new(memory).unwrap();
        assert!(matches!(
            transport.try_receive(core),
            Err(TransportError::Memory(_))
        ));
        assert!(transport.is_faulted());
        assert_eq!(transport.try_receive(core), Err(TransportError::Faulted));
        assert_eq!(transport.memory().bytes[0x78f], u8::from(mutate_on_failure));
    }
}
