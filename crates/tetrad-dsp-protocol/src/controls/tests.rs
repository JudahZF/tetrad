use super::*;

fn vectors() -> Vec<(ControlRequest, Vec<u8>)> {
    use ControlRequest as R;
    vec![
        (
            R::MetronomeParameter {
                destination: 4,
                parameter: 7,
                value_bits: 0x3fc00000,
            },
            vec![
                0x44, 4, 0, 0, 7, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xc0, 0x3f,
            ],
        ),
        (
            R::MetronomeStatusRequest,
            vec![0x45, 4, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            R::MetronomeDestination {
                value_bits: 0x41980000,
            },
            vec![
                0x46, 1, 0, 0, 9, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x98, 0x41,
            ],
        ),
        (
            R::MetronomeSound,
            vec![
                0x47, 4, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x3f,
            ],
        ),
        (
            R::EnableUsbMidiClock { value: 1 },
            vec![0x22, 4, 0, 0, 0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 1],
        ),
        (
            R::EnableInternalMidiClock { value: 0x12345678 },
            vec![
                0x23, 4, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x78, 0x56, 0x34, 0x12,
            ],
        ),
        (
            R::ResetInternalMidiClock { value: 2 },
            vec![
                0x24, 4, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0,
            ],
        ),
        (
            R::EnableMidiClockOut { value: 3 },
            vec![
                0x25, 4, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0,
            ],
        ),
        (
            R::EnableMidiClockIn { value: 0xfe },
            vec![0x26, 4, 0, 0, 0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0xfe],
        ),
        (
            R::MidiClockCountAndTempo {
                tempo_bits: 0x42f00000,
                count: 0x12345678,
            },
            vec![
                0x2d, 4, 0, 0, 0, 0, 0, 0, 24, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0, 0x42, 0x78, 0x56,
                0x34, 0x12,
            ],
        ),
        (
            R::MidiClockStart,
            vec![0x2e, 4, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0],
        ),
        (
            R::UsbMidiOut {
                delivery: UsbMidiDelivery::Immediate,
                event: UsbMidiEvent {
                    code: 15,
                    status: 0xf8,
                    data: [0, 0],
                },
            },
            vec![
                0x18, 4, 0, 0, 0, 0, 0, 0, 28, 0, 0, 0, 0, 0, 0, 0, 15, 0, 0xf8, 0, 0, 0, 0, 0, 0,
                0, 0, 0,
            ],
        ),
        (
            R::UsbMidiOut {
                delivery: UsbMidiDelivery::Tagged(0x12345678),
                event: UsbMidiEvent {
                    code: 11,
                    status: 0xb2,
                    data: [7, 127],
                },
            },
            vec![
                0x18, 4, 1, 0, 0x78, 0x56, 0x34, 0x12, 28, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0xb2, 7,
                127, 0, 0, 0, 0, 0, 0, 0,
            ],
        ),
        (
            R::InputGateSource {
                destination: 0,
                gate: 2,
                word: 0x12345678,
                source: 3,
            },
            vec![
                0x2f, 0, 2, 0, 0x78, 0x56, 0x34, 0x12, 20, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0,
            ],
        ),
        (
            R::InputGateEnabled {
                destination: 1,
                value: 1,
            },
            vec![
                0x30, 1, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
            ],
        ),
        (
            R::InputGateParameter {
                destination: 0,
                gate: 2,
                parameter: 3,
                value_bits: 0xc1200000,
            },
            vec![
                0x31, 0, 2, 0, 3, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x20, 0xc1,
            ],
        ),
        (
            R::InputGateInputGain {
                destination: 1,
                gate: 3,
                value_bits: 0x80000000,
            },
            vec![
                0x32, 1, 3, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80,
            ],
        ),
        (
            R::OutputEqualizerEnable { value: 1 },
            vec![
                0x3b, 1, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
            ],
        ),
        (
            R::OutputEqualizerParameter {
                output: 2,
                parameter: 27,
                value_bits: 0x7fc01234,
            },
            vec![
                0x3c, 1, 2, 0, 27, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0x34, 0x12, 0xc0, 0x7f,
            ],
        ),
    ]
}

#[test]
fn independent_wire_vectors_cover_every_writer() {
    for (request, bytes) in vectors() {
        assert_eq!(RequestFrame::new(request).unwrap().as_bytes(), bytes);
        let decoded = RequestFrame::decode(&bytes).unwrap();
        assert_eq!(decoded.request(), request);
        assert_eq!(decoded.as_bytes(), bytes);
    }
}

#[test]
fn rejects_truncation_trailing_data_and_false_length_words() {
    for (_, bytes) in vectors() {
        for length in 0..bytes.len() {
            let mut short = bytes[..length].to_vec();
            if length >= 16 {
                short[8..12].copy_from_slice(&(length as u32).to_le_bytes());
            }
            assert!(RequestFrame::decode(&short).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        let length = extra.len() as u32;
        extra[8..12].copy_from_slice(&length.to_le_bytes());
        assert!(RequestFrame::decode(&extra).is_err());
        let mut wrong = bytes;
        wrong[8] ^= 1;
        assert!(RequestFrame::decode(&wrong).is_err());
    }
}

#[test]
fn retains_unstored_bytes_but_rejects_writer_constant_changes() {
    for (_, mut bytes) in vectors() {
        bytes[3] = 0xa5;
        bytes[12..16].copy_from_slice(&[1, 2, 3, 4]);
        if [0x2d, 0x2e].contains(&bytes[0]) {
            bytes[2] = 0x82;
        }
        if [0x2d, 0x2e, 0x30, 0x45].contains(&bytes[0]) {
            bytes[4..8].copy_from_slice(&[5, 6, 7, 8]);
        }
        if bytes[0] == 0x18 {
            bytes[21..28].copy_from_slice(&[9, 10, 11, 12, 13, 14, 15]);
        }
        assert_eq!(RequestFrame::decode(&bytes).unwrap().as_bytes(), bytes);
    }
    for (index, offset) in [(1, 2), (2, 4), (3, 16), (4, 4), (11, 17), (11, 4), (17, 1)] {
        let mut bytes = vectors()[index].1.clone();
        bytes[offset] ^= 1;
        assert!(RequestFrame::decode(&bytes).is_err());
    }
}

#[test]
fn signed_parameter_bounds_and_raw_float_bits() {
    for (kind, maximum) in [(0, 3), (1, 27)] {
        for parameter in [i32::MIN, -1, 0, maximum, maximum + 1] {
            let request = if kind == 0 {
                ControlRequest::InputGateParameter {
                    destination: 0,
                    gate: 0,
                    parameter,
                    value_bits: 0xffc01234,
                }
            } else {
                ControlRequest::OutputEqualizerParameter {
                    output: 0,
                    parameter,
                    value_bits: 0xffc01234,
                }
            };
            let encoded = RequestFrame::new(request);
            assert_eq!(encoded.is_ok(), parameter <= maximum);
            if let Ok(encoded) = encoded {
                assert_eq!(
                    RequestFrame::decode(encoded.as_bytes()).unwrap().request(),
                    request
                );
            }
        }
    }
    for event in [
        UsbMidiEvent {
            code: 1,
            status: 0x90,
            data: [0, 0],
        },
        UsbMidiEvent {
            code: 15,
            status: 0xf8,
            data: [1, 0],
        },
    ] {
        assert!(
            RequestFrame::new(ControlRequest::UsbMidiOut {
                delivery: UsbMidiDelivery::Immediate,
                event
            })
            .is_err()
        );
    }
}

#[test]
fn tempo_change_is_a_sixteen_byte_header_only_request() {
    // VA 0x875a48: type 0x16, destination 4, selector 0, LE i32 BPM at +4, 16 at +8.
    let frame = RequestFrame::new(ControlRequest::TempoChange { bpm: 120 }).unwrap();
    assert_eq!(
        frame.as_bytes(),
        [0x16, 4, 0, 0, 120, 0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0]
    );
    for bpm in [i32::MIN, -1, 0, 40, 240, i32::MAX] {
        let request = ControlRequest::TempoChange { bpm };
        let bytes = RequestFrame::new(request).unwrap();
        assert_eq!(bytes.as_bytes().len(), 16);
        assert_eq!(&bytes.as_bytes()[4..8], bpm.to_le_bytes());
        assert_eq!(
            RequestFrame::decode(bytes.as_bytes()).unwrap().request(),
            request
        );
    }
    let mut retained = [
        0x16, 4, 0, 0xa5, 0xff, 0xff, 0xff, 0xff, 16, 0, 0, 0, 1, 2, 3, 4,
    ];
    let decoded = RequestFrame::decode(&retained).unwrap();
    assert_eq!(decoded.request(), ControlRequest::TempoChange { bpm: -1 });
    assert_eq!(decoded.as_bytes(), retained);
    retained[2] = 1;
    assert!(RequestFrame::decode(&retained).is_err());
    let mut long = retained.to_vec();
    long[2] = 0;
    long.extend_from_slice(&[0; 4]);
    long[8] = 20;
    assert!(RequestFrame::decode(&long).is_err());
}

#[test]
fn metronome_block_is_one_single_fragment_of_raw_slot_bits() {
    let values: [u32; METRONOME_PARAMETER_COUNT] =
        std::array::from_fn(|index| 0x4000_0000 | (index as u32) << 8 | 0x7f);
    let block = MetronomeParameterBlock::new(values);
    let frame = block.to_frame();
    assert_eq!(frame.len(), 108);
    // VA 0x51965c unfragmented path: 0x46, core 4, slot 0, marker 0, argument 0,
    // frame length 108 at +8, payload length 92 at +12.
    assert_eq!(
        &frame[..16],
        [0x46, 4, 0, 0, 0, 0, 0, 0, 108, 0, 0, 0, 92, 0, 0, 0]
    );
    assert_eq!(&frame[16..20], [0x7f, 0, 0, 0x40]);
    assert_eq!(&frame[104..108], [0x7f, 0x16, 0, 0x40]);
    assert_eq!(frame[16 + METRONOME_ENABLE_INDEX * 4 + 1], 4);
    assert_eq!(
        MetronomeParameterBlock::decode_frame(&frame).unwrap(),
        block
    );
    assert_eq!(
        MetronomeParameterBlock::from_payload(&block.payload()).unwrap(),
        block
    );
    assert!(MetronomeParameterBlock::from_payload(&block.payload()[..88]).is_err());

    // NaN and negative-zero bit patterns survive untouched.
    let odd = MetronomeParameterBlock::new([0xffc0_1234; METRONOME_PARAMETER_COUNT]);
    assert_eq!(
        MetronomeParameterBlock::decode_frame(&odd.to_frame())
            .unwrap()
            .values_bits(),
        &[0xffc0_1234; METRONOME_PARAMETER_COUNT]
    );

    // Any header deviation is refused: wrong type, wrong core, slot, marker, argument.
    for (offset, value) in [
        (0, 0x44),
        (1, 1),
        (2, 1),
        (3, 1),
        (4, 1),
        (8, 109),
        (12, 88),
    ] {
        let mut wrong = frame.clone();
        wrong[offset] = value;
        assert!(
            MetronomeParameterBlock::decode_frame(&wrong).is_err(),
            "offset {offset}"
        );
    }
    assert!(MetronomeParameterBlock::decode_frame(&frame[..107]).is_err());
    let mut extra = frame.clone();
    extra.push(0);
    assert!(MetronomeParameterBlock::decode_frame(&extra).is_err());
}

#[test]
fn sound_bundle_layout_fragments_and_refusals() {
    let limits = CodecLimits::default();
    let sounds: Vec<Vec<u8>> = (0..METRONOME_SOUND_COUNT)
        .map(|index| vec![index as u8; 2 * (index + 1)])
        .collect();
    let bundle = MetronomeSoundBundle::new(sounds.clone(), limits).unwrap();
    let payload = bundle.to_payload();
    let data: usize = (1..=METRONOME_SOUND_COUNT).map(|n| 2 * n).sum();
    // Stock assertion: total == sum(sizes) + 4 * (count + 1).
    assert_eq!(payload.len(), data + 4 * (METRONOME_SOUND_COUNT + 1));
    assert_eq!(payload.len(), bundle.payload_len());
    assert_eq!(&payload[..4], [18, 0, 0, 0]);
    // Length words are byte length >> 1.
    assert_eq!(&payload[4..8], [1, 0, 0, 0]);
    assert_eq!(&payload[72..76], [18, 0, 0, 0]);
    assert_eq!(&payload[76..78], [0, 0]);
    assert_eq!(&payload[78..82], [1, 1, 1, 1]);
    assert_eq!(payload[payload.len() - 1], 17);
    assert_eq!(
        MetronomeSoundBundle::from_payload(&payload, limits).unwrap(),
        bundle
    );

    // Small bundle: one Single fragment to core 2 with the generic header.
    let fragments = bundle.fragments(limits).unwrap();
    let frames: Vec<_> = fragments.iter().collect();
    assert_eq!(frames.len(), 1);
    let bytes = frames[0].to_bytes();
    assert_eq!(bytes[0], 0x47);
    assert_eq!(bytes[1], 2);
    assert_eq!(bytes[3], 0);
    assert_eq!(bytes[16..], payload[..]);

    // Large bundle: 2032-byte fragments, first/middle/last markers, lossless.
    let big: Vec<Vec<u8>> = (0..METRONOME_SOUND_COUNT)
        .map(|index| vec![index as u8; 300])
        .collect();
    let big = MetronomeSoundBundle::new(big, limits).unwrap();
    let fragments = big.fragments(limits).unwrap();
    let frames: Vec<_> = fragments.iter().collect();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].marker(), Marker::First);
    assert_eq!(frames[1].marker(), Marker::Middle);
    assert_eq!(frames[2].marker(), Marker::Last);
    assert_eq!(frames[0].payload().len(), 2032);
    let mut reassembler = crate::fragment::Reassembler::new(limits);
    let mut done = None;
    for frame in &frames {
        done = reassembler.push(*frame).unwrap();
    }
    assert_eq!(done.unwrap().payload, big.to_payload());

    // Refusals: count, parity, limit, length table disagreement, trailing bytes.
    assert!(MetronomeSoundBundle::new(sounds[..17].to_vec(), limits).is_err());
    let mut odd = sounds.clone();
    odd[3].push(0);
    assert!(MetronomeSoundBundle::new(odd, limits).is_err());
    let tight = CodecLimits {
        max_message_bytes: payload.len() - 1,
        ..limits
    };
    assert!(MetronomeSoundBundle::new(sounds, tight).is_err());
    let mut count = payload.clone();
    count[0] = 17;
    assert!(MetronomeSoundBundle::from_payload(&count, limits).is_err());
    let mut length = payload.clone();
    length[4] = 2;
    assert!(MetronomeSoundBundle::from_payload(&length, limits).is_err());
    let mut trailing = payload.clone();
    trailing.push(0);
    assert!(MetronomeSoundBundle::from_payload(&trailing, limits).is_err());
    assert!(MetronomeSoundBundle::from_payload(&payload[..75], limits).is_err());
}
