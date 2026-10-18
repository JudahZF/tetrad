use super::*;
use address::*;
use fragment::*;
use packet::*;
use ring::*;
use std::time::Duration;

fn target() -> DeploymentTarget {
    DeploymentTarget {
        core: DspCore::Core2,
        slot: ModuleSlot::try_from(29).unwrap(),
    }
}
fn metadata(core: CoreId) -> MessageMetadata {
    MessageMetadata {
        message_type: 0x21,
        core,
        slot: 17,
        argument: 0x8765_4321,
    }
}

#[test]
fn message_types_cover_the_extended_table_and_reject_unknown_bytes() {
    assert_eq!(std::mem::size_of::<MessageType>(), 1);
    for byte in 0..=0x5b {
        assert_eq!(MessageType::try_from(byte).unwrap() as u8, byte);
    }
    for byte in 0x5c..=u8::MAX {
        assert_eq!(MessageType::try_from(byte), Err(Error::MessageType(byte)));
    }
    // Independent values from the pointer table; the historic sentinel is not a bound.
    for (byte, expected) in [
        (0x00, MessageType::ProbablyEmptyBuffer),
        (0x02, MessageType::ParameterChange),
        (0x07, MessageType::LoadModel),
        (0x0b, MessageType::ReceiveData),
        (0x0d, MessageType::NumValidMessages),
        (0x0e, MessageType::MeterRequest),
        (0x17, MessageType::ReceiveDataEnd),
        (0x18, MessageType::UsbMidiOut),
        (0x1d, MessageType::BulkParameterChange),
        (0x27, MessageType::UsbHidTxToHost),
        (0x29, MessageType::LooperEvent),
        (0x2d, MessageType::MidiClockCountAndTempo),
        (0x34, MessageType::GlobalTunerInputPath),
        (0x38, MessageType::TunerFrequencyRequest),
        (0x44, MessageType::MetronomeParameter),
        (0x4b, MessageType::MetronomePreRollStatusRequest),
        (0x4f, MessageType::TunerCalibrate),
        (0x57, MessageType::DataiMessage),
        (0x5a, MessageType::LibUsbConnect),
        (0x5b, MessageType::DummyMessage),
    ] {
        assert_eq!(MessageType::try_from(byte).unwrap(), expected);
        assert_eq!(expected as u8, byte);
    }
    assert_eq!(PARAMETER_CHANGE_TYPE, 2);
    assert_eq!(BULK_PARAMETER_CHANGE_TYPE, 0x1d);
    assert_eq!(deployment::LOAD_MODEL_TYPE, 7);
}

#[test]
fn opaque_unknown_fragment_types_remain_wire_compatible() {
    let metadata = MessageMetadata {
        message_type: 0xff,
        ..metadata(CoreId::Control)
    };
    let wire = Fragmenter::new(metadata, &[1, 2, 3], CodecLimits::default())
        .unwrap()
        .next()
        .unwrap()
        .to_bytes();
    assert_eq!(wire[0], 0xff);
    let decoded = Fragment::decode(&wire, CodecLimits::default()).unwrap();
    assert_eq!(decoded.metadata(), metadata);
    assert_eq!(decoded.to_bytes(), wire);
    assert_eq!(
        MessageType::try_from(decoded.metadata().message_type),
        Err(Error::MessageType(0xff))
    );
}

#[test]
fn typed_units_alignment_and_inclusive_relocation() {
    assert_eq!(
        Address::<Parcels16>::new(3).byte_offset().unwrap().value(),
        6
    );
    assert_eq!(
        Address::<Words48>::new(3).byte_offset().unwrap().value(),
        18
    );
    assert!(Address::<Words48>::from_byte_offset(Address::new(7)).is_err());
    assert!(Address::<Words48>::new(u32::MAX).byte_offset().is_err());
    let relocation = Relocation::new(
        Address::<Parcels16>::new(100),
        Address::new(110),
        Address::new(2),
    )
    .unwrap();
    for (before, after) in [(99, 99), (100, 2), (105, 7), (110, 12), (111, 111)] {
        assert_eq!(
            relocation.apply(Address::new(before)).unwrap().value(),
            after
        );
    }
    assert!(Relocation::<Bytes>::new(Address::new(2), Address::new(1), Address::new(0)).is_err());
    assert!(
        Relocation::<Bytes>::new(Address::new(0), Address::new(1), Address::new(u32::MAX)).is_err()
    );
    assert!(DspCore::try_from(4).is_err());
    assert_eq!(CoreId::try_from(4).unwrap(), CoreId::Control);
    assert!(CoreId::try_from(5).is_err());
    assert!(ModuleSlot::try_from(30).is_err());
}

#[test]
fn exact_parameter_layout_preserves_unspecified_bytes_and_float_bits() {
    let packet = ParameterChange::from_template(
        [0xa5; 20],
        target(),
        0x1234_5678,
        f32::from_bits(0x7fc0_0021),
    );
    assert_eq!(
        packet.as_bytes(),
        &[
            2, 2, 29, 0xa5, 0x78, 0x56, 0x34, 0x12, 20, 0, 0, 0, 0xa5, 0xa5, 0xa5, 0xa5, 0x21, 0,
            0xc0, 0x7f
        ]
    );
    let decoded = ParameterChange::decode(packet.as_bytes()).unwrap();
    assert_eq!(decoded, packet);
    assert_eq!(decoded.target(), target());
    assert_eq!(decoded.parameter_index(), 0x1234_5678);
    assert_eq!(decoded.value().to_bits(), 0x7fc0_0021);
    for offset in [0, 1, 2, 8] {
        let mut corrupt = *packet.as_bytes();
        corrupt[offset] = 0xff;
        assert!(ParameterChange::decode(&corrupt).is_err());
    }
    assert!(ParameterChange::decode(&packet.as_bytes()[..19]).is_err());
}

#[test]
fn bulk_verified_records_lengths_unknown_mode_and_limits() {
    let records = [BulkRecord {
        slot: target().slot,
        parameter_index: 0x1234_5678,
        value_bits: 0xffc0_1234,
    }];
    let bytes = encode_bulk(
        [0xa5; 16],
        DspCore::Core3,
        BulkMode::Value16,
        &records,
        CodecLimits::default(),
    )
    .unwrap();
    assert_eq!(
        &bytes[..16],
        &[0x1d, 3, 0, 0xa5, 16, 0, 0, 0, 28, 0, 0, 0, 1, 0, 0, 0]
    );
    assert_eq!(
        &bytes[16..],
        &[29, 0, 0, 0, 0x78, 0x56, 0x34, 0x12, 0x34, 0x12, 0xc0, 0xff]
    );
    let decoded = BulkParameterChange::decode(&bytes, CodecLimits::default()).unwrap();
    assert_eq!(decoded.records().collect::<Vec<_>>(), records);
    assert_eq!(decoded.core(), DspCore::Core3);
    assert_eq!(decoded.mode_word(), 16);
    let mut opaque = bytes.clone();
    opaque[4..8].copy_from_slice(&0xabcdef01_u32.to_le_bytes());
    assert_eq!(
        BulkParameterChange::decode(&opaque, CodecLimits::default())
            .unwrap()
            .as_bytes(),
        opaque
    );
    for offset in [0, 1, 8, 12, 16] {
        let mut corrupt = bytes.clone();
        corrupt[offset] = 0xff;
        assert!(BulkParameterChange::decode(&corrupt, CodecLimits::default()).is_err());
    }
    assert!(BulkParameterChange::decode(&bytes[..27], CodecLimits::default()).is_err());
    let limits = CodecLimits {
        max_message_bytes: 27,
        max_records: 1,
    };
    assert!(encode_bulk([0; 16], DspCore::Core0, BulkMode::Value8, &records, limits).is_err());
    assert!(BulkParameterChange::decode(&bytes, limits).is_err());
    let limits = CodecLimits {
        max_message_bytes: 28,
        max_records: 0,
    };
    assert!(BulkParameterChange::decode(&bytes, limits).is_err());
    let empty = encode_bulk(
        [0; 16],
        DspCore::Core0,
        BulkMode::Value8,
        &[],
        CodecLimits::default(),
    )
    .unwrap();
    assert_eq!(
        BulkParameterChange::decode(&empty, CodecLimits::default())
            .unwrap()
            .records()
            .len(),
        0
    );
}

#[test]
fn fragments_empty_max_and_multi_boundary_roundtrips_all_cores() {
    for core in (0..=4).map(|n| CoreId::try_from(n).unwrap()) {
        let maximum = core.fragment_payload_max();
        for length in [
            0,
            1,
            maximum - 1,
            maximum,
            maximum + 1,
            maximum * 2,
            maximum * 2 + 1,
            maximum * 3,
        ] {
            let payload: Vec<_> = (0..length).map(|n| n as u8).collect();
            let fragments: Vec<_> =
                Fragmenter::new(metadata(core), &payload, CodecLimits::default())
                    .unwrap()
                    .collect();
            let expected_count = length.div_ceil(maximum).max(1);
            assert_eq!(fragments.len(), expected_count);
            let mut assembler = Reassembler::new(CodecLimits::default());
            let mut completed = None;
            for (index, fragment) in fragments.iter().copied().enumerate() {
                let expected = if expected_count == 1 {
                    Marker::Single
                } else if index == 0 {
                    Marker::First
                } else if index == expected_count - 1 {
                    Marker::Last
                } else {
                    Marker::Middle
                };
                assert_eq!(fragment.marker(), expected);
                let bytes = fragment.to_bytes();
                assert_eq!(bytes.len(), 16 + fragment.payload().len());
                assert_eq!(bytes[3], expected as u8);
                let decoded = Fragment::decode(&bytes, CodecLimits::default()).unwrap();
                let output = assembler.push(decoded).unwrap();
                if output.is_some() {
                    assert_eq!(index, expected_count - 1);
                    completed = output;
                }
            }
            assembler.finish().unwrap();
            assert_eq!(
                completed.unwrap(),
                ReassembledMessage {
                    metadata: metadata(core),
                    payload
                }
            );
        }
    }
}

#[test]
fn fragments_reject_size_sequence_identity_and_special_paths() {
    let core = CoreId::Control;
    let payload = vec![7; 257];
    let fragments: Vec<_> = Fragmenter::new(metadata(core), &payload, CodecLimits::default())
        .unwrap()
        .collect();
    let mut assembler = Reassembler::new(CodecLimits::default());
    assert!(assembler.push(fragments[1]).is_err());
    assembler.push(fragments[0]).unwrap();
    assert!(assembler.finish().is_err());
    assert!(assembler.push(fragments[0]).is_err()); // Abort, then allow fresh input.
    assembler.push(fragments[0]).unwrap();
    let mut corrupt = fragments[1].to_bytes();
    corrupt[2] += 1;
    assert!(
        assembler
            .push(Fragment::decode(&corrupt, CodecLimits::default()).unwrap())
            .is_err()
    );
    assembler.finish().unwrap();
    assembler.push(fragments[0]).unwrap();
    assert!(assembler.push(fragments[2]).is_err()); // Early last.
    let bytes = fragments[0].to_bytes();
    for offset in [1, 3, 8, 15] {
        let mut corrupt = bytes.clone();
        corrupt[offset] = 0xff;
        assert!(Fragment::decode(&corrupt, CodecLimits::default()).is_err());
    }
    assert!(Fragment::decode(&bytes[..15], CodecLimits::default()).is_err());
    assert!(
        Fragmenter::new(
            metadata(core),
            &payload,
            CodecLimits {
                max_message_bytes: 256,
                max_records: 0
            }
        )
        .is_err()
    );
    assert!(
        Fragment::decode(
            &bytes,
            CodecLimits {
                max_message_bytes: 256,
                max_records: 0
            }
        )
        .is_err()
    );
    let tiny = Fragmenter::new(metadata(CoreId::Control), &[], CodecLimits::default())
        .unwrap()
        .next()
        .unwrap()
        .to_bytes();
    let mut oversized = tiny;
    oversized.resize(145, 0);
    oversized[8..12].copy_from_slice(&145_u32.to_le_bytes());
    oversized[12..16].copy_from_slice(&129_u32.to_le_bytes());
    assert!(Fragment::decode(&oversized, CodecLimits::default()).is_err());
    let mut load = metadata(CoreId::Dsp(DspCore::Core0));
    load.message_type = 7;
    assert_eq!(
        Fragmenter::new(load, &[], CodecLimits::default()).unwrap_err(),
        Error::DirectMemoryRequired
    );
    let plan = deployment::plan_load_model(target(), 20);
    assert_eq!(
        plan.required_path,
        deployment::RequiredLoadPath::GenericRing
    );
    let plan = deployment::plan_load_model(
        DeploymentTarget {
            core: DspCore::Core1,
            ..target()
        },
        20,
    );
    assert_eq!(
        plan.required_path,
        deployment::RequiredLoadPath::DirectMemory
    );
    for core in (0..=4).map(|value| CoreId::try_from(value).unwrap()) {
        load.core = core;
        for message_type in [MessageType::LoadModel, MessageType::ReceiveData] {
            load.message_type = message_type as u8;
            if matches!(core, CoreId::Dsp(DspCore::Core0 | DspCore::Core1)) {
                assert_eq!(
                    Fragmenter::new(load, &[], CodecLimits::default()).unwrap_err(),
                    Error::DirectMemoryRequired
                );
            } else {
                let wire = Fragmenter::new(load, &[], CodecLimits::default())
                    .unwrap()
                    .next()
                    .unwrap()
                    .to_bytes();
                assert_eq!(wire[0], message_type as u8);
                assert_eq!(
                    Fragment::decode(&wire, CodecLimits::default())
                        .unwrap()
                        .metadata(),
                    load
                );
            }
        }
    }
}

#[test]
fn ring_capacity_publication_and_completion_survive_wrap() {
    for kind in [
        RingKind::Outgoing(CoreId::Dsp(DspCore::Core0)),
        RingKind::Outgoing(CoreId::Control),
        RingKind::Incoming,
    ] {
        let mut ring = Ring::new(kind);
        let mut tokens = Vec::new();
        let payload = vec![0xa5; kind.slot_bytes()];
        for index in 0..kind.slot_count() - 1 {
            let published = ring.publish(&payload).unwrap();
            assert_eq!(
                published.log,
                [
                    PublicationEvent::Payload {
                        slot: index,
                        bytes: payload.len()
                    },
                    PublicationEvent::Length {
                        slot: index,
                        length_le: (payload.len() as u32).to_le_bytes()
                    },
                    PublicationEvent::Producer { index: index + 1 }
                ]
            );
            assert!(!ring.is_complete(&published.token).unwrap());
            tokens.push(published.token);
        }
        assert!(ring.is_full());
        let before = ring.image().clone();
        assert_eq!(ring.publish(&[]).unwrap_err(), Error::RingFull);
        assert_eq!(ring.image(), &before);
        for token in &tokens {
            assert_eq!(ring.consume().unwrap(), Some(payload.clone()));
            assert!(ring.is_complete(token).unwrap());
        }
        assert!(ring.is_empty());
        assert_eq!(ring.consume().unwrap(), None);
        // Cross many complete index wraps; stale tokens must not become pending.
        for _ in 0..100 {
            let p = ring.publish(&[]).unwrap();
            assert!(!ring.is_complete(&p.token).unwrap());
            assert_eq!(ring.consume().unwrap(), Some(vec![]));
            assert!(ring.is_complete(&p.token).unwrap());
            for old in &tokens {
                assert!(ring.is_complete(old).unwrap());
            }
        }
        let other = Ring::new(kind);
        assert_eq!(other.is_complete(&tokens[0]), Err(Error::ForeignToken));
        assert!(ring.publish(&vec![0; kind.slot_bytes() + 1]).is_err());
    }
}

#[test]
fn ring_import_rejects_corrupt_indices_lengths_and_slot_layout() {
    let kind = RingKind::Incoming;
    let image = Ring::new(kind).image().clone();
    let mut corrupt = image.clone();
    corrupt.producer = 14;
    assert!(Ring::from_image(kind, corrupt).is_err());
    let mut corrupt = image.clone();
    corrupt.consumer = 255;
    assert!(Ring::from_image(kind, corrupt).is_err());
    let mut corrupt = image.clone();
    corrupt.slots.pop();
    assert!(Ring::from_image(kind, corrupt).is_err());
    let mut corrupt = image.clone();
    corrupt.slots[0].payload.pop();
    assert!(Ring::from_image(kind, corrupt).is_err());
    let mut corrupt = image.clone();
    corrupt.slots[0].length_le = u32::MAX.to_le_bytes();
    assert!(Ring::from_image(kind, corrupt).is_err());
    let mut wrapped = image;
    wrapped.consumer = 13;
    wrapped.producer = 1;
    wrapped.slots[13].payload[0] = 9;
    wrapped.slots[13].length_le = 1_u32.to_le_bytes();
    wrapped.slots[0].payload[0] = 8;
    wrapped.slots[0].length_le = 1_u32.to_le_bytes();
    let mut ring = Ring::from_image(kind, wrapped).unwrap();
    assert_eq!(ring.consume().unwrap(), Some(vec![9]));
    assert_eq!(ring.consume().unwrap(), Some(vec![8]));
    assert!(ring.is_empty());
}

#[test]
fn completion_wait_timeout_boundaries_and_injected_clock() {
    let mut wait = CompletionWait::new(1);
    assert_eq!(
        wait.observe(Duration::ZERO, false).unwrap(),
        WaitDecision::Sleep(Duration::from_micros(100))
    );
    assert_eq!(
        wait.observe(Duration::from_micros(999), false).unwrap(),
        WaitDecision::Sleep(Duration::from_micros(1))
    );
    assert_eq!(
        wait.observe(Duration::from_millis(1), false).unwrap(),
        WaitDecision::TimedOut
    );
    assert!(wait.observe(Duration::ZERO, true).is_err());
    assert_eq!(
        CompletionWait::new(0)
            .observe(Duration::ZERO, true)
            .unwrap(),
        WaitDecision::Complete
    );
    assert_eq!(
        CompletionWait::new(0)
            .observe(Duration::ZERO, false)
            .unwrap(),
        WaitDecision::TimedOut
    );
}
