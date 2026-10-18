use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use tetrad_dsp_protocol::packet::BulkMode;
use tetrad_dsp_protocol::requests::BypassState;
use tetrad_dsp_protocol::{CoreId, DeploymentTarget, DspCore, ModuleSlot};

use super::*;
use crate::model::{CoreWindows, ModelMemory};
use crate::session::{Clock, DspSession};
use crate::setup::test_factory::build;
use crate::test_support::{FakeClock, FakeMemory, SharedFake};
use crate::transport::{DspTransport, MESSAGE_MEMORY_BYTES, RingLayout};

fn target(core: DspCore, slot: u32) -> DeploymentTarget {
    DeploymentTarget {
        core,
        slot: ModuleSlot::try_from(slot).unwrap(),
    }
}

fn identity(model_id: u32, category: u32) -> ModelIdentity {
    ModelIdentity {
        target: target(DspCore::Core2, 4),
        model_id,
        category,
        fx_loop_send: None,
    }
}

fn float(value: f32) -> SetupValue {
    SetupValue::Float(value.to_bits())
}

fn parameter(target: DeploymentTarget, index: u32, value: f32) -> DspWrite {
    DspWrite::parameter(target, index, value)
}

/// Branches of the transcribed factory switch and its category overrides.
#[test]
fn factory_follows_stock_switch_and_category_overrides() {
    use DriverKind as K;
    let cases = [
        (1, 0, None),
        (30, 0, Some(K::Ts808Unified)),
        (6016, 6, Some(K::TapeDelay { range_end: 0x1a })),
        (6017, 6, Some(K::TapeDelay { range_end: 0x17 })),
        (7005, 7, None),
        (8003, 8, Some(K::Reverb)),
        (8004, 8, None),
        (8038, 8, Some(K::PcomSkews)),
        (12001, 12, Some(K::CabinetSimulator)),
        (32089, 32, Some(K::CabinetSimulator)),
        (32000, 32, Some(K::CabinetSimulatorStereo)),
        (3008, 3, Some(K::Parallax)),
        (13005, 13, Some(K::FxLoop)),
        (13007, 13, Some(K::FxReturn)),
        (27000, 27, Some(K::Looper)),
        (27001, 27, None),
    ];
    for (model, category, expected) in cases {
        assert_eq!(driver_kind(model, category), expected, "model {model}");
    }
}

/// Block push contract (VAs 0x5174a8, 0x517228): bypass first with the
/// sidechain router flag, then values by QVariant type; MUTE/BYPASS skipped.
#[test]
fn generic_push_sends_bypass_then_values_in_index_order() {
    let mut out = Vec::new();
    let mut block = build(identity(1, 0), &mut out).unwrap();
    assert!(out.is_empty());
    let router = target(DspCore::Core2, 9);
    let values = [
        float(0.25),
        SetupValue::Int(7),
        SetupValue::Meter,
        SetupValue::UInt(3),
        SetupValue::Text,
        SetupValue::Float(f32::NAN.to_bits() | 1),
        float(1.0),
        float(1.0),
    ];
    let bypass = BlockBypass {
        bypassed: true,
        muted: true,
    };
    block.push(&values, bypass, Some(router), &mut out).unwrap();
    let block_target = identity(1, 0).target;
    assert_eq!(
        out,
        [
            DspWrite::Bypass {
                target: block_target,
                state: BypassState::BypassedFlagged,
            },
            DspWrite::IntParameter {
                target: router,
                index: SIDECHAIN_ACTIVE_PARAMETER,
                value: 0,
            },
            parameter(block_target, 0, 0.25),
            DspWrite::IntParameter {
                target: block_target,
                index: 1,
                value: 7,
            },
            DspWrite::Parameter {
                target: block_target,
                index: 5,
                value_bits: f32::NAN.to_bits() | 1,
            },
        ]
    );
    let mut out = Vec::new();
    block
        .edit(3, SetupValue::UInt(3), ParamSetMode::Live, &mut out)
        .unwrap();
    block
        .edit(1, SetupValue::Int(7), ParamSetMode::Live, &mut out)
        .unwrap();
    assert_eq!(
        out,
        [DspWrite::IntParameter {
            target: block_target,
            index: 3,
            value: 3,
        }]
    );
}

#[test]
fn table_driver_sends_its_constructor_table_and_passes_values() {
    let mut out = Vec::new();
    let mut block = build(identity(1174, 1), &mut out).unwrap();
    assert_eq!(block.driver_kind(), Some(DriverKind::NollyAmp));
    let DspWrite::StockData {
        buffer, payload, ..
    } = out[0]
    else {
        panic!("expected a constructor table, got {out:?}");
    };
    assert_eq!((buffer, payload.len()), (0, 0xcaa4));
    out.clear();
    block
        .push(
            &[float(0.5), float(0.0), float(0.0)],
            BlockBypass::default(),
            None,
            &mut out,
        )
        .unwrap();
    assert_eq!(out[1], parameter(identity(1174, 1).target, 0, 0.5));
}

/// Proposal A: without the stock tables a driver that reads them is refused
/// with `MissingTables` (constructor tables, NOTE tables, amp words, media)
/// and nothing is appended; a model without a driver still builds.
#[test]
fn drivers_that_read_stock_tables_need_them() {
    let media = drivers::media::test_media::TestMedia::default();
    let bare = Factory {
        tables: None,
        library: &media,
        mini: false,
    };
    let mut out = Vec::new();
    for (model_id, category, driver) in [
        (12001, 12, DriverKind::CabinetSimulator),
        (7000, 7, DriverKind::TempoModulation),
        (1165, 1, DriverKind::FortinNameless),
        (30, 0, DriverKind::Ts808Unified),
    ] {
        assert_eq!(
            Block::build(identity(model_id, category), &[], &bare, &mut out).unwrap_err(),
            SetupError::MissingTables { model_id, driver }
        );
    }
    assert!(out.is_empty());
    let block = Block::build(identity(1, 0), &[], &bare, &mut out).unwrap();
    assert_eq!(block.driver_kind(), None);
}

#[test]
fn unrecovered_drivers_and_captures_are_refused() {
    let mut out = Vec::new();
    assert_eq!(
        build(identity(27000, 27), &mut out).unwrap_err(),
        SetupError::UnrecoveredPath {
            model_id: 27000,
            driver: DriverKind::Looper,
            va: 0x55e140,
            missing: "global Looper requests and playback state",
        }
    );
    assert_eq!(
        build(identity(13004, 13), &mut out).unwrap_err(),
        SetupError::MissingFxLoopSend { model_id: 13004 }
    );
    assert!(out.is_empty());
}

/// FxSendDriver: no `0x0c`; the bypass hook sends the stale pair first,
/// the after-push hook the converted values (40 dB range).
#[test]
fn fx_send_push_sends_gains_instead_of_values() {
    let mut out = Vec::new();
    let mut block = build(identity(13000, 13), &mut out).unwrap();
    let values = [float(1.0), float(0.5), float(0.0), float(0.0)];
    block
        .push(&values, BlockBypass::default(), None, &mut out)
        .unwrap();
    let at = identity(13000, 13).target;
    assert_eq!(
        out,
        [
            parameter(at, 3, 0.0),
            parameter(at, 5, 0.0),
            parameter(at, 3, 1.0),
            parameter(at, 5, 10_f32.powf(-1.0)),
        ]
    );
    out.clear();
    let bypassed = BlockBypass {
        bypassed: true,
        muted: false,
    };
    block.set_bypass(bypassed, None, &mut out);
    assert_eq!(out, [parameter(at, 3, 0.0), parameter(at, 5, 1.0)]);
}

/// FxLoopDriver: bypass mutes the send placement; TRAILS keeps the wet return.
#[test]
fn fx_loop_bypass_keeps_wet_return_only_with_trails() {
    let send = target(DspCore::Core3, 1);
    let mut out = Vec::new();
    let mut block = build(
        ModelIdentity {
            fx_loop_send: Some(send),
            ..identity(13008, 13)
        },
        &mut out,
    )
    .unwrap();
    let at = identity(13008, 13).target;
    let bypassed = BlockBypass {
        bypassed: true,
        muted: false,
    };
    out.clear();
    let values = [
        float(1.0),
        float(1.0),
        float(1.0),
        float(1.0),
        float(0.0),
        float(1.0),
    ];
    block.push(&values, bypassed, None, &mut out).unwrap();
    let wet = (f64::from(1.0_f32 * 52.0) - 37.74) * f64::from(0.05_f32);
    assert_eq!(
        out,
        [
            parameter(at, 9, 0.0),
            parameter(at, 7, 1.0),
            parameter(send, 3, 0.0),
            parameter(at, 9, 10_f32.powf(wet as f32)),
            parameter(at, 7, 1.0),
            parameter(send, 3, 0.0),
        ]
    );
    out.clear();
    block
        .edit(3, float(0.0), ParamSetMode::Live, &mut out)
        .unwrap();
    assert!(
        out.is_empty(),
        "TRAILS alone does not mark the driver dirty"
    );
    block
        .edit(2, float(0.5), ParamSetMode::Live, &mut out)
        .unwrap();
    assert_eq!(
        out,
        [
            parameter(at, 9, 0.0),
            parameter(at, 7, 1.0),
            parameter(send, 3, 0.0),
        ]
    );
}

/// F1: recall mode only stores FX values; `recalled` (VA 0x526cc0 → `+0x48`)
/// sends the new gains, with the +3 dB boost on the dry path too.
#[test]
fn fx_return_scene_recall_sends_gains_when_the_block_is_recalled() {
    let mut out = Vec::new();
    let mut block = build(identity(13002, 13), &mut out).unwrap();
    let values = [float(1.0), float(0.5), float(0.0), float(0.0)];
    block
        .push(&values, BlockBypass::default(), None, &mut out)
        .unwrap();
    out.clear();
    block
        .edit(1, float(1.0), ParamSetMode::Recall, &mut out)
        .unwrap();
    assert!(out.is_empty(), "recall mode stores the value only");
    block.recalled(&|_| None, &mut out);
    let at = identity(13002, 13).target;
    let gain = 10_f32.powf(1.0_f32.mul_add(52.0, -40.0) * 0.05);
    assert_eq!(
        out,
        [parameter(at, 9, gain * 1.412_537_5), parameter(at, 7, 0.0),]
    );
}

/// `0x51e4d8` sends MUTE/BYPASS as `0x0c`, so `edit` refuses them. The
/// sidechain SOURCE (index 1) goes through like any value: the caller
/// re-routes first (review w2c F3).
#[test]
fn edit_refuses_the_bypass_pair() {
    let mut out = Vec::new();
    let mut block = build(identity(5018, 5), &mut out).unwrap();
    let mut edit =
        |block: &mut Block, index| block.edit(index, float(1.0), ParamSetMode::Live, &mut out);
    assert_eq!(
        edit(&mut block, 0),
        Err(SetupError::EditBeforePush { model_id: 5018 })
    );
    let values = [float(0.5), float(0.5), float(0.0), float(0.0)];
    block
        .push(&values, BlockBypass::default(), None, &mut Vec::new())
        .unwrap();
    for index in [2, 3] {
        assert_eq!(
            edit(&mut block, index),
            Err(SetupError::BypassParameter {
                model_id: 5018,
                index
            })
        );
    }
    assert_eq!(
        edit(&mut block, 4),
        Err(SetupError::IndexOutOfRange {
            model_id: 5018,
            index: 4,
            count: 4
        })
    );
    assert_eq!(edit(&mut block, 1), Ok(()));
    assert_eq!(edit(&mut block, 0), Ok(()));
    let at = identity(5018, 5).target;
    assert_eq!(out, [parameter(at, 1, 1.0), parameter(at, 0, 1.0)]);
}

struct FakeTables;

impl StockTables for FakeTables {
    fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
        Ok(vec![0xa5; payload.len()])
    }
}

type Sent = Rc<RefCell<Vec<(CoreId, Vec<u8>)>>>;

const OUTGOING: [CoreId; 5] = [
    CoreId::Dsp(DspCore::Core0),
    CoreId::Dsp(DspCore::Core1),
    CoreId::Control,
    CoreId::Dsp(DspCore::Core2),
    CoreId::Dsp(DspCore::Core3),
];

/// A DSP that consumes every published request and records it.
fn session(sent: Sent) -> DspSession<SharedFake, FakeClock> {
    let window = SharedFake::new(MESSAGE_MEMORY_BYTES);
    let clock = FakeClock::new(window.clone(), move |memory: &mut FakeMemory, _| {
        for core in OUTGOING {
            let layout = RingLayout::outgoing(core);
            while memory.bytes[layout.consumer_offset] != memory.bytes[layout.producer_offset] {
                let slot = (memory.bytes[layout.consumer_offset] + 1) % layout.slot_count;
                let at = layout.length_offset + usize::from(slot) * 4;
                let length = u32::from_le_bytes(memory.bytes[at..at + 4].try_into().unwrap());
                let payload = layout.payload_offset + usize::from(slot) * layout.slot_bytes;
                let bytes = memory.bytes[payload..payload + length as usize].to_vec();
                memory.bytes[layout.consumer_offset] = slot;
                sent.borrow_mut().push((core, bytes));
            }
        }
    });
    DspSession::new(DspTransport::new(window).unwrap(), clock)
}

/// Stock bulk mode: 0x03 and ReceiveData go out at once; 169 queued records
/// flush; the end flushes parameters, then bypass, sync and 0x3e.
#[test]
fn bulk_execution_follows_stock_queue_order() {
    let sent = Sent::default();
    let mut session = session(sent.clone());
    let window = |core| {
        ModelMemory::new(
            core,
            tetrad_dsp_protocol::address::Address::new(0x2e_0000),
            FakeMemory::new(0x100),
        )
    };
    let mut windows = CoreWindows::new(
        window(DspCore::Core0).unwrap(),
        window(DspCore::Core1).unwrap(),
    )
    .unwrap();
    let at = target(DspCore::Core2, 4);
    let constructors = [DspWrite::StockData {
        target: at,
        buffer: 0,
        payload: StockPayload::new(&[Segment { va: 0x1000, len: 8 }]),
    }];
    let mut push = vec![DspWrite::Bypass {
        target: at,
        state: BypassState::Engaged,
    }];
    push.extend((0..170).map(|index| parameter(at, index, 0.5)));
    push.push(DspWrite::IntParameter {
        target: at,
        index: 1,
        value: 2,
    });
    session
        .run_setup(
            &mut windows,
            &FakeTables,
            BulkMode::Value16,
            &constructors,
            &push,
        )
        .unwrap();
    session.clock_mut().sleep(Duration::from_micros(100));
    // Each ring is FIFO; the fake drains rings in its own order.
    let ring = |core: DspCore| -> Vec<u8> {
        let id = CoreId::Dsp(core);
        sent.borrow()
            .iter()
            .filter(|(ring, _)| *ring == id)
            .map(|(_, bytes)| bytes[0])
            .collect()
    };
    assert_eq!(ring(DspCore::Core2), [0x0b, 0x1d, 0x03, 0x1d, 0x3d, 0x0a]);
    assert_eq!(ring(DspCore::Core0), [0x0a, 0x3e]);
    assert_eq!(ring(DspCore::Core1), [0x0a]);
    assert_eq!(ring(DspCore::Core3), [0x0a]);
    let bulk = sent
        .borrow()
        .iter()
        .find(|(_, bytes)| bytes[0] == 0x1d)
        .unwrap()
        .1
        .clone();
    assert_eq!(bulk[4..8], 16_u32.to_le_bytes());
    let data = sent
        .borrow()
        .iter()
        .find(|(_, bytes)| bytes[0] == 0x0b)
        .unwrap()
        .1
        .clone();
    assert_eq!(data[data.len() - 8..], [0xa5; 8]);
}

#[test]
fn elf_tables_refuse_other_images() {
    assert_eq!(
        ElfTables::new(vec![0x7f, b'E', b'L', b'F']).unwrap_err(),
        TableError::WrongImage
    );
}

/// 4.1.0 reads every site at its own VA, so the site VAs in the driver files are the old
/// constants; [`tables::CIPHER_SEED`] is the old tetrad-app `SECRET_SEED`.
#[test]
fn v4_1_0_reads_every_site_at_its_old_va() {
    use tetrad_firmware::Firmware;
    assert_eq!(
        tables::CIPHER_SEED.segments(),
        [Segment {
            va: 0x0141_1478,
            len: 57
        }]
    );
    let vas = Firmware::V4_1_0.profile().unwrap().zenui.vas;
    for segment in tables::zenui_reads().flat_map(|payload| payload.segments().iter().copied()) {
        assert_eq!(vas.relocate(segment.va, segment.len), Some(segment.va));
    }
    for firmware in Firmware::ALL {
        if let Ok(profile) = firmware.profile() {
            assert_eq!(tables::check_zenui_vas(profile), Ok(()), "{firmware}");
        }
    }
}

/// Each 4.1.1 read site was found one by one by the RE (`tetrad-firmware/data/v4_1_1_sites.tsv`),
/// not only through the span relocations of the 4.1.1 profile.
#[test]
fn every_v4_1_1_site_has_an_re_result() {
    let sites: Vec<(u32, u32)> = include_str!("../../../tetrad-firmware/data/v4_1_1_sites.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let hex = |field: &str| u32::from_str_radix(&field[2..], 16).unwrap();
            let mut fields = line.split('\t');
            (hex(fields.next().unwrap()), hex(fields.next().unwrap()))
        })
        .collect();
    for segment in tables::zenui_reads().flat_map(|payload| payload.segments().iter().copied()) {
        assert!(sites.contains(&(segment.va, segment.len)), "{segment:x?}");
    }
}

/// A relocated build reads each site at its own VA, also from a kept table set; a site
/// without a VA fails instead of reading other bytes.
#[test]
fn relocated_builds_read_each_site_at_its_own_va() {
    use tetrad_firmware::{Relocation, ZenUiVas};
    const MOVED: &[Relocation] = &[Relocation {
        from: 0x1000,
        len: 8,
        to: 0x2004,
    }];
    const SITE: StockPayload = StockPayload::new(&[Segment { va: 0x1002, len: 4 }]);
    const OUTSIDE: StockPayload = StockPayload::new(&[Segment { va: 0x1006, len: 4 }]);
    let tables = ElfTables::flat(0x2000, (0..16).collect(), ZenUiVas::Relocated(MOVED));
    assert_eq!(tables.read(SITE), Ok(vec![6, 7, 8, 9]));
    assert_eq!(
        tables.read(OUTSIDE),
        Err(TableError::Unrelocated { va: 0x1006, len: 4 })
    );
    let kept = tables.keep([SITE]).unwrap();
    assert_eq!(kept.read(SITE), Ok(vec![6, 7, 8, 9]));
}

/// The table readers read only sites of [`tables::zenui_reads`], so
/// [`tables::check_zenui_vas`] covers every read of `DriverTables::load` and the looper.
#[test]
fn table_readers_read_only_listed_sites() {
    struct Recording(RefCell<Vec<Segment>>);
    impl StockTables for Recording {
        fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
            self.0.borrow_mut().extend(payload.segments());
            Ok(vec![0; payload.len()])
        }
    }
    let recording = Recording(RefCell::default());
    NoteTables::read(&recording).unwrap();
    AmpTables::read(&recording).unwrap();
    MediaStock::read(&recording).unwrap();
    drivers::looper::LooperTables::read(&recording).unwrap();
    let listed: Vec<Segment> = tables::zenui_reads()
        .flat_map(|payload| payload.segments().iter().copied())
        .collect();
    let read = recording.0.take();
    assert!(!read.is_empty());
    for segment in read {
        assert!(listed.contains(&segment), "{segment:x?}");
    }
}

pub(super) fn delay_identity(model_id: u32) -> ModelIdentity {
    ModelIdentity {
        target: target(DspCore::Core2, 4),
        model_id,
        category: 6,
        fx_loop_send: None,
    }
}

/// One delay hook call: its pass flag and its `(index, value)` writes.
pub(super) fn delay_hook(
    driver: &mut dyn drivers::Driver,
    index: u32,
    value: f32,
    mode: ParamSetMode,
) -> (bool, Vec<(u32, f32)>) {
    let mut out = Vec::new();
    let pass = driver.update_parameter(index, value, mode, &mut out);
    let writes = out
        .iter()
        .map(|write| match *write {
            DspWrite::Parameter {
                index, value_bits, ..
            } => (index, f32::from_bits(value_bits)),
            ref other => panic!("unexpected {other:?}"),
        })
        .collect();
    (pass, writes)
}

/// Every delay factory arm has a build path. These constructors read no
/// stock tables and have empty scene/tempo hooks (`0xed550/5bc/5e4`).
#[test]
fn every_delay_model_builds_without_stock_tables() {
    let media = drivers::media::test_media::TestMedia::default();
    let factory = Factory {
        tables: None,
        library: &media,
        mini: false,
    };
    let mut out = Vec::new();
    for (model_id, parameter_count, sync_and_notes) in [
        (6004, 24, &[6, 7][..]),
        (6005, 18, &[4, 5]),
        (6007, 22, &[5, 6]),
        (6011, 21, &[5, 6]),
        (6012, 25, &[1, 2, 6, 7]),
        (6014, 25, &[1, 2, 7]),
        (6016, 27, &[6, 7]),
        (6017, 24, &[5, 6]),
        (6018, 28, &[6, 7]),
        (6019, 28, &[6, 7]),
        (6020, 25, &[5, 6]),
        (6021, 25, &[5, 6]),
        (6022, 22, &[5, 6]),
        (6023, 22, &[5, 6]),
        (6024, 22, &[5, 6]),
        (6027, 17, &[2, 5, 6]),
        (6031, 32, &[1, 9, 16, 23, 28]),
        (6032, 23, &[5, 6]),
    ] {
        let identity = delay_identity(model_id);
        let mut block = Block::build(identity, &[], &factory, &mut out).unwrap();
        let kind = driver_kind(model_id, 6).expect("delay driver class");
        assert_eq!(block.driver_kind(), Some(kind), "model {model_id}");
        assert!(out.is_empty(), "model {model_id} sends no constructor data");
        let values = vec![float(0.5); parameter_count + 2];
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        for &index in sync_and_notes {
            assert!(out.contains(&parameter(identity.target, index, 0.5)));
        }
        for &index in sync_and_notes {
            out.clear();
            block
                .edit(index, float(0.25), ParamSetMode::Recall, &mut out)
                .unwrap();
            assert_eq!(out, [parameter(identity.target, index, 0.25)]);
        }
        out.clear();
        block.tempo_changed(90, &mut out);
        block.recalled(&|_| None, &mut out);
        assert!(
            out.is_empty(),
            "model {model_id} has no constructor/tempo/scene writes"
        );
    }
}

/// r14-misc and r15: every non-delay census class builds through the
/// factory. Without the stock tables (no or a foreign
/// `/opt/neuraldsp/ZenUI`) every driver that reads or uploads them is
/// refused at build, so the app mutes one block (review r14 D1); tables
/// without a driver's range name the read error (D4). Parametric-4 needs
/// its catalog.
#[test]
fn r14_misc_models_build_or_name_their_gap() {
    use DriverKind as K;
    let builds = [
        (26005, 26, K::Glitch),
        (5017, 5, K::SslBusComp),
        (5022, 5, K::SslBusComp),
        (5023, 5, K::SslBusComp),
        (5030, 5, K::SslBusComp),
        (5029, 5, K::HorizonClarityComp),
        (18001, 18, K::Transpose),
        (18010, 18, K::Transpose),
        (18019, 18, K::MishaOctaver),
        (18009, 18, K::GojiraWow),
        (7015, 7, K::OptimisedAdvancedChorus),
        (26, 0, K::Odr1),
        (45, 0, K::KeeleyKatana),
        (47, 0, K::RabeaFuzz),
        (26001, 26, K::Freeze),
        (18007, 18, K::Minivoicer),
        (30001, 30, K::MonosynthMidi),
    ];
    for (model_id, category, kind) in builds {
        let block = build(identity(model_id, category), &mut Vec::new()).unwrap();
        assert_eq!(block.driver_kind(), Some(kind), "model {model_id}");
    }
    let media = drivers::media::test_media::TestMedia::default();
    let bare = Factory {
        tables: None,
        library: &media,
        mini: false,
    };
    let row_readers = [
        (18018, K::MishaLaser),
        (7014, K::OptimisedAdvancedFlanger),
        (46, K::JmOverdrive),
        (7021, K::MxrFlanger),
        (7042, K::MxrFlanger),
        (7046, K::MishaThor),
        (18012, K::Multivoicer),
    ];
    let uploaders = [
        (5029, K::HorizonClarityComp),
        (47, K::RabeaFuzz),
        (26, K::Odr1),
        (45, K::KeeleyKatana),
    ];
    for (model_id, driver) in row_readers.into_iter().chain(uploaders) {
        let mut out = Vec::new();
        let refused = Block::build(identity(model_id, 7), &[], &bare, &mut out).unwrap_err();
        assert_eq!(refused, SetupError::MissingTables { model_id, driver });
        assert!(out.is_empty(), "model {model_id}");
    }
    for (model_id, driver) in row_readers {
        assert!(matches!(
            build(identity(model_id, 7), &mut Vec::new()).unwrap_err(),
            SetupError::StockTable { model_id: m, driver: d, source: TableError::Unmapped { .. } }
                if m == model_id && d == driver
        ));
    }
    assert_eq!(
        build(identity(4008, 4), &mut Vec::new()).unwrap_err(),
        SetupError::MissingCatalog {
            model_id: 4008,
            index: 2
        }
    );
}

/// The r14-misc VAs resolve in the pinned ZenUI through the long-lived
/// driver table set, and the row-reading drivers build from it. Nothing is
/// printed or saved.
#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
fn r14_misc_tables_resolve_from_the_pinned_image() {
    let Some(path) = crate::corpus("rootfs/opt/neuraldsp/ZenUI") else {
        return;
    };
    let tables = DriverTables::load(std::fs::read(path).unwrap()).unwrap();
    let media = drivers::media::test_media::TestMedia::default();
    let factory = Factory {
        tables: Some(&tables),
        library: &media,
        mini: false,
    };
    for (model_id, category) in [
        (18018, 18),
        (7014, 7),
        (46, 0),
        (5029, 5),
        (26, 0),
        (45, 0),
        (47, 0),
        (7042, 7),
        (7046, 7),
        (18012, 18),
    ] {
        let mut out = Vec::new();
        let mut block =
            Block::build(identity(model_id, category), &[], &factory, &mut out).unwrap();
        let values = vec![float(1.0); 23];
        block
            .push(&values, BlockBypass::default(), None, &mut out)
            .unwrap();
        for write in &out {
            if let DspWrite::StockData { payload, .. } = write {
                assert_eq!(tables.read(*payload).unwrap().len(), payload.len());
            }
        }
    }
}

/// `Block` routes the r15 hooks: a MIDI note reaches the driver only while its `+0x4c` is
/// on and on its `+0x50` channel or omni (`0x514270`); a stomp edge reaches `+0x40`; the
/// driver's preset writes come back once.
#[test]
fn block_routes_midi_by_channel_and_stomp_edges() {
    let at = identity(18007, 18).target;
    let mut out = Vec::new();
    let mut mini = build(identity(18007, 18), &mut out).unwrap();
    let mut values = vec![float(0.0); 25];
    values[15] = float(1.0);
    values[22] = float(0.5);
    mini.push(&values, BlockBypass::default(), None, &mut out)
        .unwrap();
    let note = |note| DriverMidi::Note {
        note,
        velocity: 100,
        on: true,
    };
    mini.midi(8, note(61), &mut out);
    mini.midi(9, note(60), &mut out);
    assert!(mini.take_timer().is_some());
    out.clear();
    mini.timer_elapsed(&mut out);
    assert_eq!(
        out[..2],
        [parameter(at, 5, 1.0), parameter(at, 18, 60.0 / 127.0)]
    );
    mini.edit(22, float(1.0), ParamSetMode::Live, &mut out)
        .unwrap();
    mini.midi(3, note(64), &mut out);
    out.clear();
    mini.timer_elapsed(&mut out);
    assert_eq!(
        out[1],
        parameter(at, 18, 64.0 / 127.0),
        "channel 17 takes all"
    );

    let mut freeze = build(identity(26001, 26), &mut out).unwrap();
    freeze
        .push(&[float(0.0); 4], BlockBypass::default(), None, &mut out)
        .unwrap();
    out.clear();
    freeze.footswitch(true, &mut out);
    assert_eq!(out, [parameter(at, 0, 1.0)]);
    let edit = LinkedEdit {
        index: 0,
        value: 1.0,
        echo: true,
    };
    assert_eq!(freeze.take_linked_edits(), [edit]);
    assert!(freeze.take_linked_edits().is_empty());
}
