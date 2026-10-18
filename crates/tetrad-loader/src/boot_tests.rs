//! Boot plan, executor and SOC2 contracts on invented bytes. Addresses are the recovered
//! stock constants; no stock firmware or booter bytes are embedded.
use std::time::Duration;

use crate::blw::{self, Blw};
use crate::booter::{L2_BASE, Placement, Region, Source, StockBooter, Swap};
use crate::exec::{
    AccessError, ArgumentOutcome, BootError, Phase, PhysicalMemory, Soc1Error, StepError,
    StreamEnd, execute, execute_soc1, word,
};
use crate::fake::{Access, Memory, RecordingBus, SimSharc};
use crate::intent::{CoreAction, LoaderCore};
use crate::plan::{BOOT_ROM_IDLE, BootPlan, RESTART_QUIESCE, Start, Step, Variant, plan_soc1};
use crate::soc2::{SlaveBootKind, Soc2Boot, Soc2Images, Soc2Outcome};
use crate::{Error, Limits, PhysAddr, RawLdr};

const SEGMENT_VA: u32 = 0x12000;
const SEGMENT_OFFSET: u32 = 0x1000;
const FILE_END_VA: u32 = 0x17cc0;
const MEMORY_END_VA: u32 = 0x17d10;

/// Invented loadsharc-shaped ELF: one PT_LOAD whose file data ends at `FILE_END_VA`
/// and whose zero fill runs on, like the stock `.data`/`.bss`.
struct FakeLoadsharc(Vec<u8>);

impl FakeLoadsharc {
    fn new() -> Self {
        let mut elf = vec![0; (SEGMENT_OFFSET + FILE_END_VA - SEGMENT_VA) as usize];
        elf[..8].copy_from_slice(b"\x7fELF\x01\x01\x01\x00");
        let mut put = |at: usize, bytes: &[u8]| elf[at..at + bytes.len()].copy_from_slice(bytes);
        put(0x10, &2_u16.to_le_bytes());
        put(0x12, &40_u16.to_le_bytes());
        put(0x1c, &0x34_u32.to_le_bytes());
        put(0x2a, &32_u16.to_le_bytes());
        put(0x2c, &1_u16.to_le_bytes());
        for (index, value) in [
            1,
            SEGMENT_OFFSET,
            SEGMENT_VA,
            SEGMENT_VA,
            FILE_END_VA - SEGMENT_VA,
            MEMORY_END_VA - SEGMENT_VA,
        ]
        .into_iter()
        .enumerate()
        {
            put(0x34 + 4 * index, &value.to_le_bytes());
        }
        Self(elf)
    }
    fn put(&mut self, va: u32, bytes: &[u8]) {
        let at = (SEGMENT_OFFSET + va - SEGMENT_VA) as usize;
        self.0[at..at + bytes.len()].copy_from_slice(bytes);
    }
    /// Stock config words for one core plus `(target, source VA, size)` entries.
    fn table(&mut self, config: u32, mailbox: u32, app_start: u32, sections: &[(u32, u32, u32)]) {
        let words = [
            mailbox,
            mailbox + 0xa74,
            mailbox + 0xa70,
            mailbox + 0xa6c,
            mailbox + 0xa68,
            app_start,
            sections.len() as u32,
        ];
        let entries = sections
            .iter()
            .flat_map(|&(target, source, size)| [target, source, size]);
        let bytes: Vec<u8> = words
            .into_iter()
            .chain(entries)
            .flat_map(u32::to_le_bytes)
            .collect();
        self.put(config, &bytes);
    }
    /// Core 1 and core 2 tables with one section of every stock placement kind.
    fn stock_shaped() -> Self {
        let mut elf = Self::new();
        elf.put(0x13000, b"SharcBooter_Core1.dxe\0");
        elf.put(0x13100, b"SharcBooter_Core2.dxe\0");
        elf.put(0x13200, &[1, 2, 3, 4, 5, 6, 7, 8]);
        elf.put(0x13300, &[0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6]);
        for (config, mailbox, app_start, name) in [
            (0x123c0, 0x200b_c330, 0x00bb_d810, 0x13000),
            (0x15040, 0x200a_e330, 0x00bb_6810, 0x13100),
        ] {
            elf.table(
                config,
                mailbox,
                app_start,
                &[
                    (0x0009_0000, 0x13400, 0x300),
                    (0x200b_b004, 0x17ce4, 4),
                    (0x0802_ec00, 0x13200, 5),
                    (0x200b_b008, name, 22),
                    (app_start, 0x13300, 6),
                ],
            );
        }
        elf
    }
}

/// FIRST|IGNORE, one literal of `payload` bytes, one fill, FINAL.
fn kernel(signature: u8, payload: usize) -> Vec<u8> {
    let header = |flags: u16, target: u32, count: u32, argument: u32| {
        let mut raw = [0_u8; 16];
        raw[..2].copy_from_slice(&flags.to_le_bytes());
        raw[3] = signature;
        raw[4..8].copy_from_slice(&target.to_le_bytes());
        raw[8..12].copy_from_slice(&count.to_le_bytes());
        raw[12..].copy_from_slice(&argument.to_le_bytes());
        raw[2] = raw.iter().fold(0, |xor, byte| xor ^ byte);
        raw
    };
    let final_offset = (16 + 16 + payload + 16) as u32;
    let mut bytes = header(0x5001, 0x0016_1894, 0, final_offset).to_vec();
    bytes.extend(header(0x0001, 0x2824_0000, payload as u32, 0));
    bytes.extend((0..payload).map(|index| index as u8));
    bytes.extend(header(0x0101, 0x2825_0000, 0x40, 0));
    bytes.extend(header(0x8001, 0x0016_1894, 0, 0));
    bytes
}

#[test]
fn stream_layout_recovers_entry_and_rejects_broken_frames() {
    let bytes = kernel(0xac, 32);
    let layout = RawLdr::parse(&bytes, Limits::default())
        .unwrap()
        .stream_layout()
        .unwrap();
    assert_eq!((layout.entry, layout.final_offset), (0x0016_1894, 80));
    let mutate = |at: usize, xor: u8| {
        let mut bad = bytes.clone();
        bad[at] ^= xor;
        bad[(at & !15) + 2] ^= xor;
        RawLdr::parse(&bad, Limits::default())
            .unwrap()
            .stream_layout()
    };
    // Wrong FINAL offset, FINAL flag lost, FIRST flag inside the stream.
    for (at, xor) in [(12, 0x10), (81, 0x80), (17, 0x40)] {
        assert!(
            matches!(mutate(at, xor), Err(Error::Layout(_))),
            "byte {at}"
        );
    }
}

#[test]
fn blw_frame_checks_length_alignment_and_reserved_bytes() {
    let mut file = vec![0_u8; blw::HEADER_BYTES + 32];
    file[..4].copy_from_slice(&blw::MAGIC);
    file[0x04..0x3c].fill(0x5a);
    file[0x6c..0x70].copy_from_slice(&32_u32.to_be_bytes());
    file[0x70..0x74].copy_from_slice(&1_u32.to_be_bytes());
    let wrapper = Blw::parse(&file).unwrap();
    assert_eq!((wrapper.payload_len(), wrapper.flags()), (32, 1));
    assert_eq!(wrapper.signature().len(), 56);
    assert_eq!(wrapper.as_bytes().len(), file.len());
    assert_eq!(
        RawLdr::parse(&file, Limits::default()).unwrap_err(),
        Error::UnsupportedWrapper
    );
    let broken = |edit: fn(&mut Vec<u8>)| {
        let mut bad = file.clone();
        edit(&mut bad);
        Blw::parse(&bad).is_err()
    };
    assert!(broken(|bytes| bytes.truncate(blw::HEADER_BYTES + 16)));
    assert!(broken(|bytes| bytes.extend([0; 8])));
    assert!(broken(|bytes| bytes[0x80] = 1));
    assert!(broken(|bytes| bytes[3] = 2));
}

#[test]
fn booter_sections_follow_loadsharc_placement_rules() {
    let elf = FakeLoadsharc::stock_shaped();
    let booter = StockBooter::parse(&elf.0).unwrap();
    let core1 = &booter.core1;
    assert_eq!(core1.app_start, 0x00bb_d810);
    assert_eq!(core1.stream.buffer, PhysAddr(0x200b_c330));
    assert_eq!(core1.stream.sharc_to_arm, PhysAddr(0x200b_cd98));
    assert_eq!(core1.stream.capacity, 0xa68);
    assert_eq!(booter.core2.map.app.sharc_to_arm, PhysAddr(0x2008_2000));
    let placements: Vec<_> = core1
        .sections
        .iter()
        .map(|section| section.placement)
        .collect();
    assert_eq!(placements[0], None, "core-local L1 targets are skipped");
    assert_eq!(
        placements[1],
        Some(Placement {
            address: PhysAddr(0x200b_b004),
            swap: Swap::None,
            source: Source::Zeros(4)
        })
    );
    // Normal word ×4 and widened to whole words; VISA ×2 + 0x1e940000.
    assert_eq!(
        placements[2],
        Some(Placement {
            address: PhysAddr(0x200b_b000),
            swap: Swap::Words,
            source: Source::Bytes(&[1, 2, 3, 4, 5, 6, 7, 8])
        })
    );
    let visa = placements[4].unwrap();
    assert_eq!(
        (visa.address, visa.swap),
        (PhysAddr(0x200b_b020), Swap::Halfwords)
    );

    let mut isa = FakeLoadsharc::stock_shaped();
    isa.put(0x123c0 + 0x1c + 12 * 2, &0x005a_0000_u32.to_le_bytes());
    assert!(matches!(StockBooter::parse(&isa.0), Err(Error::Booter(_))));
    let mut unnamed = FakeLoadsharc::stock_shaped();
    unnamed.put(0x13100, b"SharcBooter_CoreX");
    assert!(matches!(
        StockBooter::parse(&unnamed.0),
        Err(Error::Booter(_))
    ));
    assert!(StockBooter::parse(&elf.0[..0x1000]).is_err());
}

#[test]
fn restart_plan_matches_zenui_and_loadsharc_order() {
    let elf = FakeLoadsharc::stock_shaped();
    let booter = StockBooter::parse(&elf.0).unwrap();
    let (bytes1, bytes2) = (kernel(0xac, 8), kernel(0xab, 8));
    let core1 = RawLdr::parse(&bytes1, Limits::default()).unwrap();
    let core2 = RawLdr::parse(&bytes2, Limits::default()).unwrap();
    let plan = plan_soc1(&booter, &core1, &core2, Variant::Atma, Start::Restart).unwrap();
    let quiesce = &plan.quiesce.steps[..RESTART_QUIESCE.len()];
    assert_eq!(quiesce, RESTART_QUIESCE);
    let writes = |value: u32| {
        quiesce
            .iter()
            .filter(|step| matches!(step, Step::Write32 { value: v, .. } if *v == value))
            .count()
    };
    assert_eq!((writes(0), writes(0x0038_0000)), (42, 1));
    let core = |core, action| Step::Core { core, action };
    let per_core = |id: LoaderCore| {
        let booter = booter.image(id);
        let image = match id {
            LoaderCore::Core1 => &core1,
            LoaderCore::Core2 => &core2,
        };
        let mut steps = vec![
            core(id, CoreAction::Stop),
            core(LoaderCore::Core1, CoreAction::SetEntry(BOOT_ROM_IDLE)),
            core(id, CoreAction::Start),
        ];
        steps.extend(booter.sections[1..].iter().map(|section| {
            let placement = section.placement.unwrap();
            Step::Upload {
                address: placement.address,
                swap: placement.swap,
                source: placement.source,
            }
        }));
        steps.extend([
            core(id, CoreAction::SetEntry(booter.app_start)),
            core(id, CoreAction::Start),
            Step::Stream {
                core: id,
                mailbox: booter.stream,
                image: image.as_bytes(),
            },
            Step::Arguments {
                core: id,
                mailbox: booter.map.app,
                arguments: b"Atma",
            },
        ]);
        steps
    };
    let mut quiesce = RESTART_QUIESCE.to_vec();
    quiesce.extend([
        core(LoaderCore::Core1, CoreAction::Stop),
        core(LoaderCore::Core2, CoreAction::Stop),
    ]);
    assert_eq!(plan.quiesce.steps, quiesce);
    assert_eq!(plan.core2.steps, per_core(LoaderCore::Core2));
    assert_eq!(plan.core1.steps, per_core(LoaderCore::Core1));
    assert_eq!(
        plan_soc1(&booter, &core2, &core1, Variant::QuadCortex, Start::First).unwrap_err(),
        Error::CoreMismatch
    );
}

#[test]
fn executor_streams_every_byte_and_places_arguments_deterministically() {
    let elf = FakeLoadsharc::stock_shaped();
    let booter = StockBooter::parse(&elf.0).unwrap();
    // A payload above the 0xa68 buffer forces a split request.
    let (bytes1, bytes2) = (kernel(0xac, 3000), kernel(0xab, 40));
    let core1 = RawLdr::parse(&bytes1, Limits::default()).unwrap();
    let core2 = RawLdr::parse(&bytes2, Limits::default()).unwrap();
    let plan = plan_soc1(&booter, &core1, &core2, Variant::QuadCortex, Start::First).unwrap();
    let run = || {
        let peers = vec![
            SimSharc::new(&booter.core2, 0x0024_0100),
            SimSharc::new(&booter.core1, 0x200b_0000),
        ];
        let mut bus = RecordingBus::new(peers);
        let report = execute_soc1(&plan, &mut bus).unwrap();
        (report, bus)
    };
    let (report, bus) = run();
    assert!(report.failures.is_empty());
    let [sim2, sim1] = [&bus.peer[0], &bus.peer[1]];
    assert_eq!(
        (sim1.received.as_slice(), sim2.received.as_slice()),
        (&bytes1[..], &bytes2[..])
    );
    assert_eq!(sim1.arguments.as_deref(), Some(&b"QC"[..]));
    assert_eq!(
        report.boot.streams[1].chunks, 6,
        "FIRST, literal header, 0xa68 + 0x150 payload, fill, FINAL"
    );
    assert_eq!(
        (
            report.boot.streams[1].end,
            report.boot.streams[1].unconsumed
        ),
        (StreamEnd::ZeroRequest, 0)
    );
    let outcomes: Vec<_> = report
        .boot
        .arguments
        .iter()
        .map(|argument| argument.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [
            ArgumentOutcome::Placed {
                address: PhysAddr(0x28a4_0100),
                region: Region::L1
            },
            ArgumentOutcome::Placed {
                address: PhysAddr(0x200b_0000),
                region: Region::L2
            },
        ]
    );
    // Swapped booter sections land as the stock byte loops leave them; .bss source is zero.
    assert_eq!(
        bus.memory.bytes(PhysAddr(0x200b_b000), 8),
        [4, 3, 2, 1, 8, 7, 6, 5]
    );
    assert_eq!(
        bus.memory.bytes(PhysAddr(0x200b_b020), 6),
        [0xa2, 0xa1, 0xa4, 0xa3, 0xa6, 0xa5]
    );
    let mailbox = booter.core2.stream;
    let first_exchange: Vec<_> = bus
        .log
        .iter()
        .filter(|access| matches!(access, Access::Write32 { address, .. } if *address == mailbox.arm_to_sharc))
        .take(5)
        .copied()
        .collect();
    let out = |value| Access::Write32 {
        address: mailbox.arm_to_sharc,
        value,
    };
    assert_eq!(
        first_exchange,
        [
            out(0xdead),
            out(0x5555),
            out(0xbbbb),
            out(0x6666),
            out(0x5555)
        ]
    );
    assert_eq!(
        run().1.log,
        bus.log,
        "same inputs give the same access sequence"
    );
}

#[test]
fn soc1_still_loads_core1_after_a_handled_core2_failure() {
    let elf = FakeLoadsharc::stock_shaped();
    let booter = StockBooter::parse(&elf.0).unwrap();
    let (bytes1, bytes2) = (kernel(0xac, 40), kernel(0xab, 40));
    let core1 = RawLdr::parse(&bytes1, Limits::default()).unwrap();
    let core2 = RawLdr::parse(&bytes2, Limits::default()).unwrap();
    let plan = plan_soc1(&booter, &core1, &core2, Variant::QuadCortex, Start::First).unwrap();
    let peers = vec![
        // Outside L1 and L2: a handled argument-region error on core 2.
        SimSharc::new(&booter.core2, 0x1000_0000),
        SimSharc::new(&booter.core1, 0x200b_0000),
    ];
    let mut bus = RecordingBus::new(peers);
    let report = execute_soc1(&plan, &mut bus).unwrap();
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].core, LoaderCore::Core2);
    assert!(matches!(
        report.failures[0].error.error,
        StepError::ArgumentRegion(0x1000_0000)
    ));
    assert_eq!(bus.peer[1].received, &bytes1[..]);
    assert_eq!(bus.peer[1].arguments.as_deref(), Some(&b"QC"[..]));
    assert_eq!(report.boot.streams.len(), 1);
}

#[test]
fn soc1_stops_on_a_failed_access_before_core1() {
    let elf = FakeLoadsharc::stock_shaped();
    let booter = StockBooter::parse(&elf.0).unwrap();
    let (bytes1, bytes2) = (kernel(0xac, 40), kernel(0xab, 40));
    let core1 = RawLdr::parse(&bytes1, Limits::default()).unwrap();
    let core2 = RawLdr::parse(&bytes2, Limits::default()).unwrap();
    let plan = plan_soc1(&booter, &core1, &core2, Variant::QuadCortex, Start::First).unwrap();
    let peers = vec![
        SimSharc::new(&booter.core2, 0x200b_0000),
        SimSharc::new(&booter.core1, 0x200b_0000),
    ];
    let mut bus = RecordingBus::new(peers);
    // Core 2 places its sections at 0x200b_b000 and above; that part is unmapped.
    bus.windows.push(0x2008_0000..0x200b_0000);
    let error = execute_soc1(&plan, &mut bus).unwrap_err();
    assert!(matches!(
        error,
        Soc1Error::Access {
            core: LoaderCore::Core2,
            error: BootError {
                error: StepError::Access(AccessError::Unmapped(_)),
                ..
            }
        }
    ));
    assert!(bus.peer[1].received.is_empty());
}

fn single_step(step: Step<'_>) -> BootPlan<'_> {
    BootPlan { steps: vec![step] }
}

#[test]
fn every_wait_is_bounded_with_the_stock_timeout_and_poll() {
    let elf = FakeLoadsharc::stock_shaped();
    let booter = StockBooter::parse(&elf.0).unwrap();
    let stream = single_step(Step::Stream {
        core: LoaderCore::Core1,
        mailbox: booter.core1.stream,
        image: &[],
    });
    let mut silent = RecordingBus::new(());
    silent.read_cost = Duration::from_millis(100);
    let error = execute(&stream, &mut silent).unwrap_err();
    assert_eq!(
        error,
        BootError {
            step: 0,
            error: StepError::Timeout {
                phase: Phase::BooterReady,
                address: booter.core1.stream.sharc_to_arm,
                last: 0
            }
        }
    );
    assert!(silent.now() > Duration::from_secs(9) && silent.now() <= Duration::from_millis(9100));

    let arguments = single_step(Step::Arguments {
        core: LoaderCore::Core1,
        mailbox: booter.core1.map.app,
        arguments: b"QC",
    });
    let mut idle = RecordingBus::new(());
    idle.read_cost = Duration::ZERO;
    let error = execute(&arguments, &mut idle).unwrap_err().error;
    assert!(matches!(
        error,
        StepError::Timeout {
            phase: Phase::AppStart,
            ..
        }
    ));
    let polls = idle
        .log
        .iter()
        .filter(|access| **access == Access::Delay(Duration::from_millis(500)))
        .count();
    assert_eq!(
        polls, 19,
        "check, then sleep 500 ms, until more than 9 s elapsed"
    );
}

#[test]
fn sharc_requests_outside_the_contract_are_typed_errors() {
    let elf = FakeLoadsharc::stock_shaped();
    let booter = StockBooter::parse(&elf.0).unwrap();
    let mailbox = booter.core1.stream;
    let image = [7_u8; 16];
    let stream_with = |requested: u32| {
        let peer = move |memory: &mut Memory, access: &Access| match *access {
            Access::Write32 {
                address,
                value: word::ARM_READY,
            } if address == mailbox.arm_to_sharc => {
                memory.write_u32(mailbox.sharc_to_arm, word::BOOTER_READY)
            }
            Access::Write32 {
                address,
                value: word::REQUEST,
            } if address == mailbox.arm_to_sharc => {
                memory.write_u32(mailbox.to_arm_count, requested);
                memory.write_u32(mailbox.sharc_to_arm, word::GRANT);
            }
            _ => {}
        };
        let mut bus = RecordingBus::new(peer);
        let result = execute(
            &single_step(Step::Stream {
                core: LoaderCore::Core1,
                mailbox,
                image: &image,
            }),
            &mut bus,
        );
        (result, bus.log.last().copied())
    };
    assert_eq!(
        stream_with(0xa69).0.unwrap_err().error,
        StepError::Overrun {
            requested: 0xa69,
            capacity: 0xa68
        }
    );
    assert_eq!(
        stream_with(17).0.unwrap_err().error,
        StepError::Exhausted {
            requested: 17,
            remaining: 16
        }
    );
    let (stopped, last) = stream_with(word::STOP_COUNT);
    assert_eq!(stopped.unwrap().streams[0].end, StreamEnd::StopRequest);
    assert_eq!(
        last,
        Some(Access::Write32 {
            address: mailbox.arm_to_sharc,
            value: word::DONE
        })
    );

    let app = booter.core1.map.app;
    let arguments_with = |answer: u32, buffer: u32| {
        let peer = move |memory: &mut Memory, access: &Access| {
            if *access
                == (Access::Write32 {
                    address: app.arm_to_sharc,
                    value: word::REQUEST,
                })
            {
                memory.write_u32(app.data_to_arm, buffer);
                memory.write_u32(app.sharc_to_arm, answer);
            }
        };
        let mut bus = RecordingBus::new(peer);
        bus.memory.write_u32(app.sharc_to_arm, word::REQUEST);
        let plan = single_step(Step::Arguments {
            core: LoaderCore::Core1,
            mailbox: app,
            arguments: b"QC",
        });
        (
            execute(&plan, &mut bus),
            bus.memory.read_u32(app.arm_to_sharc),
        )
    };
    let (refused, reply) = arguments_with(word::ARM_READY, 0);
    assert_eq!(
        refused.unwrap().arguments[0].outcome,
        ArgumentOutcome::Refused
    );
    assert_eq!(reply, word::REFUSED);
    assert_eq!(
        arguments_with(word::GRANT, 0x10).0.unwrap_err().error,
        StepError::ArgumentRegion(0x10)
    );
    assert_eq!(
        arguments_with(word::GRANT, L2_BASE + 0x3_ffff)
            .0
            .unwrap_err()
            .error,
        StepError::ArgumentBounds(PhysAddr(L2_BASE + 0x3_ffff))
    );
}

#[test]
fn soc2_handshake_places_images_and_follows_the_retry_rule() {
    let (spl, core0) = ([0x11_u8; 21], [0x22_u8; 5]);
    let images = Soc2Images::new(&spl, &core0).unwrap();
    assert_eq!((images.total_len(), images.spl_len()), (32 + 5, 21));
    let mut boot = Soc2Boot::new(images, None);
    let request = boot.begin().unwrap();
    assert_eq!(
        request.encode(),
        [0x12, 0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 37, 0, 0, 0]
    );
    assert_eq!(request.timeout(), Duration::from_millis(2000));
    let (copy, boot_request) = boot.on_buffer_address(PhysAddr(0x8100_0000)).unwrap();
    assert_eq!(
        (
            boot_request.kind,
            boot_request.argument,
            boot_request.timeout()
        ),
        (SlaveBootKind::Boot, 21, Duration::from_secs(4))
    );
    assert_eq!(
        boot_request.encode(),
        [0x13, 0, 0, 0, 5, 0, 0, 0, 16, 0, 0, 0, 21, 0, 0, 0]
    );
    let mut bus = RecordingBus::new(());
    execute(&copy, &mut bus).unwrap();
    assert_eq!(bus.memory.bytes(PhysAddr(0x8100_0000), 21), spl);
    assert_eq!(bus.memory.bytes(PhysAddr(0x8100_0020), 5), core0);
    let Soc2Outcome::Booted { finished } = boot.on_status(0).unwrap() else {
        panic!("expected success")
    };
    assert_eq!(finished.encode()[0], 0x14);
    assert_eq!(finished.timeout(), Duration::from_secs(20));
    assert!(
        boot.on_status(0).is_err(),
        "status after completion is unexpected"
    );

    let factory = Soc2Images::new(&spl, &spl).unwrap();
    let mut boot = Soc2Boot::new(images, Some(factory));
    assert_eq!(boot.begin().unwrap().argument, factory.total_len());
    assert!(
        boot.on_status(1).is_err(),
        "status before the buffer address is unexpected"
    );
    boot.on_buffer_address(PhysAddr(0x8100_0000)).unwrap();
    assert_eq!(
        boot.on_status(3).unwrap(),
        Soc2Outcome::Retry(crate::soc2::SlaveBootRequest {
            kind: SlaveBootKind::BufferMemAddr,
            argument: images.total_len(),
            core0_len: 0,
        })
    );
    boot.on_buffer_address(PhysAddr(0x8100_0000)).unwrap();
    assert_eq!(boot.on_status(3).unwrap(), Soc2Outcome::Dead);
    assert!(Soc2Images::new(&[], &core0).is_err());
    assert!(Blw::parse(b"BLw\x01broken").is_err());
    assert!(Soc2Images::new(b"BLw\x01broken", &core0).is_ok());
}
