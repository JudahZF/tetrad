use super::*;
use crate::packet::{BulkMode, BulkRecord};

fn hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    digits
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn target(core: DspCore, slot: u32) -> DeploymentTarget {
    DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(slot).unwrap(),
    }
}

/// Independent literal vectors, one per recovered layout shape (VAs in the
/// variant docs). Unstored bytes are zero in new frames.
#[test]
fn new_frames_match_recovered_writer_layouts_and_policies() {
    let text = TextPrefix::try_from(&b"abc"[..]).unwrap();
    let blob = OpaqueBody::try_from(&[1_u8, 2][..]).unwrap();
    let cases = [
        (
            Request::DdrTestA,
            "54000000 00000000 10000000 00000000",
            0,
            (true, 500),
        ),
        (
            Request::PrepareSoc2,
            "53000000 00000000 10000000 00000000",
            4,
            (true, 1000),
        ),
        (
            Request::MasterVolumeChange {
                gain_bits: 0x3f80_0000,
            },
            "04010000 00000000 14000000 00000000 0000803f",
            1,
            (true, 100),
        ),
        (
            Request::SetInputGain {
                input: 2,
                gain_bits: 0x3f00_0000,
            },
            "05000200 00000000 14000000 00000000 0000003f",
            0,
            (true, 100),
        ),
        (
            Request::SwitchUsbIns {
                destination: CoreId::Dsp(DspCore::Core2),
                value_bits: 0x3f80_0000,
            },
            "39020000 00000000 14000000 00000000 0000803f",
            2,
            (false, 100),
        ),
        (
            Request::MasterMute {
                core: DspCore::Core1,
                value: 3,
            },
            "20010000 00000000 14000000 00000000 03000000",
            1,
            (true, 100),
        ),
        (
            Request::HeadphoneUsbFeedEnable { value: 1 },
            "33010100 00000000 10000000 00000000",
            1,
            (true, 100),
        ),
        (
            Request::LibUsbConnect { value: 0 },
            "5a040000 00000000 11000000 00000000 00",
            4,
            (true, 1000),
        ),
        (
            Request::ModelBypass {
                target: target(DspCore::Core1, 4),
                state: BypassState::BypassedFlagged,
            },
            "0c010400 00000000 14000000 00000000 00000040",
            1,
            (false, 100),
        ),
        (
            Request::EnableAll {
                core: DspCore::Core3,
                value: 0,
            },
            "1f030000 00000000 10000000 00000000",
            3,
            (false, 100),
        ),
        (
            Request::DeleteAllModels {
                core: DspCore::Core2,
            },
            "1e020000 00000000 10000000 00000000",
            2,
            (true, 500),
        ),
        (
            Request::BufferAllocate {
                core: DspCore::Core0,
                id: 5,
                bytes: 0x2864,
            },
            "0f000000 00000000 1c000000 00000000 05000000 02000000 64280000",
            0,
            (true, 100),
        ),
        (
            Request::RouteToSidechainBuffer {
                core: DspCore::Core2,
                same_core: true,
                channel: 1,
                source_slot: 7,
                source_core: 3,
            },
            "49020100 07000000 10000000 03000000",
            2,
            (false, 1000),
        ),
        (
            Request::ReceiveDataEnd {
                target: target(DspCore::Core0, 3),
                buffer: 2,
                bytes: 0x100,
            },
            "17000300 02000000 10000000 00010000",
            0,
            (true, 100),
        ),
        (
            Request::ModuleLatencyRequest {
                target: target(DspCore::Core1, 9),
            },
            "1b010900 00000000 10000000 00000000",
            1,
            (false, 1000),
        ),
        (
            Request::ParameterChange {
                target: target(DspCore::Core1, 3),
                index: 6,
                value_bits: 0x3f80_0000,
            },
            "02010300 06000000 14000000 00000000 0000803f",
            1,
            (false, 100),
        ),
        (
            Request::Text42(text),
            "42030000 00000000 13000000 00000000 616263",
            3,
            (false, 100),
        ),
        (
            Request::Blob51(blob),
            "51040000 00000000 12000000 00000000 0102",
            4,
            (true, 1000),
        ),
        // Cloud signing: send(…, 0, 100) to 3 and send(…, 1, 1000) to 4 (VAs 0x5151bc, 0x5154c8).
        (
            Request::SigningMaterial(text),
            "35030000 00000000 13000000 00000000 616263",
            3,
            (false, 100),
        ),
        (
            Request::ClientId(text),
            "4f040000 00000000 13000000 00000000 616263",
            4,
            (true, 1000),
        ),
    ];
    for (request, expected, core, (wait, timeout_ms)) in cases {
        let frame = RequestFrame::new(request.clone()).unwrap();
        assert_eq!(frame.as_bytes(), hex(expected), "{request:?}");
        assert_eq!(request.destination().wire(), core, "{request:?}");
        assert_eq!(request.send_policy(), SendPolicy { wait, timeout_ms });
        assert_eq!(RequestFrame::decode(frame.as_bytes()).unwrap(), frame);
    }
    // The single-edit float write and the packet codec agree byte for byte.
    let packet =
        crate::packet::ParameterChange::from_template([0; 20], target(DspCore::Core1, 3), 6, 1.0);
    let frame = RequestFrame::new(Request::ParameterChange {
        target: target(DspCore::Core1, 3),
        index: 6,
        value_bits: 0x3f80_0000,
    })
    .unwrap();
    assert_eq!(frame.as_bytes(), packet.as_bytes());
    // The signing requests and their tuner cover names write the same bytes.
    use crate::tuner_looper::{RequestFrame as TunerFrame, TunerLooperRequest as Tuner};
    for (request, cover) in [
        (Request::SigningMaterial(text), Tuner::TunerText35(text)),
        (Request::ClientId(text), Tuner::TunerText4f(text)),
    ] {
        let frame = RequestFrame::new(request).unwrap();
        assert_eq!(frame.as_bytes(), TunerFrame::new(cover).as_bytes());
    }
}

#[test]
fn decode_retains_unstored_bytes_and_rejects_altered_writer_constants() {
    // ModelBypass stores +0..+2, +8 and the body; +3, +4..7 and +12..15 are stale.
    let stale = hex("0c010400 a5a5a5a5 14000000 5a5a5a5a 0000803f");
    let mut with_marker = stale.clone();
    with_marker[3] = 0x77;
    let frame = RequestFrame::decode(&with_marker).unwrap();
    assert_eq!(frame.as_bytes(), with_marker);
    assert_eq!(
        frame.request(),
        Request::ModelBypass {
            target: target(DspCore::Core1, 4),
            state: BypassState::Bypassed,
        }
    );
    // LimiterBypass stores +2 = 0 but not +4; MasterVolume stores both.
    let limiter = hex("19010000 12345678 14000000 00000000 01000000");
    assert!(RequestFrame::decode(&limiter).is_ok());
    let mut selector = limiter.clone();
    selector[2] = 1;
    assert!(RequestFrame::decode(&selector).is_err());
    let volume = hex("04010000 12345678 14000000 00000000 0000803f");
    assert!(RequestFrame::decode(&volume).is_err());
    // Bypass bodies other than 0.0, 1.0 and 2.0, short frames and a wrong
    // `+8` total are refused.
    let mut odd = stale.clone();
    odd[16..20].copy_from_slice(&0.5_f32.to_bits().to_le_bytes());
    assert!(RequestFrame::decode(&odd).is_err());
    assert!(RequestFrame::decode(&stale[..19]).is_err());
    let mut total = stale;
    total[8] = 0x15;
    assert!(RequestFrame::decode(&total).is_err());
    // Recovered caller shapes only.
    assert!(
        RequestFrame::new(Request::MasterMute {
            core: DspCore::Core2,
            value: 1
        })
        .is_err()
    );
    assert!(
        RequestFrame::new(Request::SwitchUsbIns {
            destination: CoreId::Dsp(DspCore::Core1),
            value_bits: 0
        })
        .is_err()
    );
    assert!(OpaqueBody::try_from(&[0_u8; OPAQUE_BODY_MAX + 1][..]).is_err());
}

/// Activation codes and licence data travel in `OpaqueBody`; it and the frame that copies it
/// must keep their zeroing `Drop` (review-w6 F10).
#[test]
fn opaque_bodies_and_frames_are_zeroed_on_drop() {
    assert!(std::mem::needs_drop::<OpaqueBody>());
    assert!(std::mem::needs_drop::<RequestFrame>());
}

#[test]
fn bulk_frames_round_trip_and_fit_one_dsp_slot() {
    let slot = |index| ModuleSlot::try_from(index).unwrap();
    let start = BulkFrame::new(BulkRequest::Start {
        core: DspCore::Core1,
        records: vec![StartRecord {
            model: 16006,
            slot: slot(2),
            layout_word: 0x0102,
            block_word: -1,
        }],
    })
    .unwrap();
    assert_eq!(
        start.as_bytes(),
        hex("1c010000 00000000 1c000000 01000000 863e0000 0200 0201 ffffffff")
    );
    let bypass = BulkFrame::new(BulkRequest::Bypass {
        core: DspCore::Core0,
        records: vec![BypassRecord {
            slot: slot(29),
            state: BypassState::Bypassed,
        }],
    })
    .unwrap();
    assert_eq!(
        bypass.as_bytes(),
        hex("3d000000 00000000 18000000 01000000 1d000000 0000803f")
    );
    let parameters = BulkFrame::new(BulkRequest::Parameters {
        core: DspCore::Core3,
        mode: BulkMode::Value16,
        records: vec![
            BulkRecord {
                slot: slot(1),
                parameter_index: 2,
                value_bits: 0x7fc0_0001,
            };
            BULK_PARAMETER_RECORDS_MAX
        ],
    })
    .unwrap();
    assert_eq!(parameters.as_bytes().len(), 16 + 169 * 12);
    for frame in [&start, &bypass, &parameters] {
        assert_eq!(&BulkFrame::decode(frame.as_bytes()).unwrap(), frame);
    }
    // Bypass `+4` is unstored and retained; Start requires zero there.
    let mut stale = bypass.as_bytes().to_vec();
    stale[4] = 0xa5;
    assert_eq!(BulkFrame::decode(&stale).unwrap().as_bytes(), stale);
    let mut word = start.as_bytes().to_vec();
    word[4] = 1;
    assert!(BulkFrame::decode(&word).is_err());
    let mut count = bypass.as_bytes().to_vec();
    count[12] = 2;
    assert!(BulkFrame::decode(&count).is_err());
    assert!(
        BulkFrame::new(BulkRequest::Start {
            core: DspCore::Core0,
            records: vec![
                StartRecord {
                    model: 1,
                    slot: slot(0),
                    layout_word: 0,
                    block_word: 0,
                };
                BULK_START_RECORDS_MAX + 1
            ],
        })
        .is_err()
    );
    assert!(
        BulkFrame::new(BulkRequest::Parameters {
            core: DspCore::Core0,
            mode: BulkMode::Value8,
            records: vec![
                BulkRecord {
                    slot: slot(0),
                    parameter_index: 0,
                    value_bits: 0,
                };
                BULK_PARAMETER_RECORDS_MAX + 1
            ],
        })
        .is_err()
    );
}
