//! Structural facts of the local stock boot inputs. Reads files in `QC_CORPUS_DIR` (README,
//! "Corpus tests"); nothing is copied into the repository. Run with `cargo test -p
//! tetrad-loader -- --ignored`.
use std::fs;
use std::path::PathBuf;

use tetrad_loader::blw::{self, Blw};
use tetrad_loader::booter::{Source, StockBooter};
use tetrad_loader::exec::{execute, execute_soc1};
use tetrad_loader::fake::{RecordingBus, SimSharc};
use tetrad_loader::intent::LoaderCore;
use tetrad_loader::plan::{Start, Variant, plan_soc1};
use tetrad_loader::soc2::{Soc2Boot, Soc2Images};
use tetrad_loader::{Error, Limits, PhysAddr, RawLdr};

/// The files of `$QC_CORPUS_DIR`, or `None` with a note when the variable is not set.
fn corpus() -> Option<PathBuf> {
    let dir = std::env::var_os("QC_CORPUS_DIR").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("skipped: set QC_CORPUS_DIR");
    }
    dir
}

fn read(dir: &std::path::Path, relative: &str) -> Vec<u8> {
    let path = dir.join(relative);
    fs::read(&path)
        .unwrap_or_else(|error| panic!("missing corpus file {}: {error}", path.display()))
}

#[test]
#[ignore = "reads stock firmware in $QC_CORPUS_DIR"]
fn stock_boot_inputs_build_and_run_a_full_plan() {
    let Some(dir) = corpus() else { return };
    let elf = read(&dir, "rootfs/usr/bin/loadsharc_SC589");
    let core1_bytes = read(&dir, "plain/ZPUKernel_Core1.ldr");
    let core2_bytes = read(&dir, "plain/ZPUKernel_Core2.ldr");
    let core1 = RawLdr::parse(&core1_bytes, Limits::default()).unwrap();
    let core2 = RawLdr::parse(&core2_bytes, Limits::default()).unwrap();
    assert_eq!((core1.blocks().len(), core2.blocks().len()), (143, 136));
    let layouts = [
        core1.stream_layout().unwrap(),
        core2.stream_layout().unwrap(),
    ];
    assert_eq!(
        layouts.map(|layout| (layout.entry, layout.final_offset)),
        [(0x0016_1894, 171_376), (0x0016_1aca, 161_372)]
    );
    // The shipped kernels are encrypted; ZenUI decrypts them before loadsharc runs.
    let encrypted = read(
        &dir,
        "rootfs/opt/neuraldsp/sharc_binaries/ZPUKernel_Core1.ldr",
    );
    assert!(matches!(
        RawLdr::parse(&encrypted, Limits::default()),
        Err(Error::Signature { .. } | Error::Checksum(_))
    ));

    let booter = StockBooter::parse(&elf).unwrap();
    for (image, app_start, buffer) in [
        (&booter.core1, 0x00bb_d810, 0x200b_c330),
        (&booter.core2, 0x00bb_6810, 0x200a_e330),
    ] {
        assert_eq!(image.sections.len(), 8);
        let skipped: Vec<_> = image
            .sections
            .iter()
            .filter(|section| section.placement.is_none())
            .map(|section| section.target)
            .collect();
        assert_eq!(skipped, [0x0009_00a8, 0x0009_0000]);
        let zero_filled = image
            .sections
            .iter()
            .filter(|section| {
                matches!(
                    section.placement.map(|placement| placement.source),
                    Some(Source::Zeros(_))
                )
            })
            .count();
        assert_eq!(zero_filled, 3);
        assert_eq!(
            (image.app_start, image.stream.buffer),
            (app_start, PhysAddr(buffer))
        );
        assert_eq!(image.stream.capacity, 0xa68);
    }

    let plan = plan_soc1(&booter, &core1, &core2, Variant::QuadCortex, Start::Restart).unwrap();
    assert_eq!(plan.quiesce.steps.len(), 46 + 2);
    assert_eq!((plan.core2.steps.len(), plan.core1.steps.len()), (13, 13));
    let peers = vec![
        SimSharc::new(&booter.core2, 0x0024_0000),
        SimSharc::new(&booter.core1, 0x0024_0000),
    ];
    let mut bus = RecordingBus::new(peers);
    let report = execute_soc1(&plan, &mut bus).unwrap();
    assert!(report.failures.is_empty());
    assert_eq!(bus.peer[0].received, core2_bytes);
    assert_eq!(bus.peer[1].received, core1_bytes);
    assert!(
        bus.peer
            .iter()
            .all(|sim| sim.arguments.as_deref() == Some(&b"QC"[..]))
    );
    assert!(
        report
            .boot
            .streams
            .iter()
            .all(|stream| stream.unconsumed == 0)
    );
    assert_eq!(
        report
            .boot
            .streams
            .iter()
            .map(|stream| stream.core)
            .collect::<Vec<_>>(),
        [LoaderCore::Core2, LoaderCore::Core1]
    );
}

#[test]
#[ignore = "reads stock firmware in $QC_CORPUS_DIR"]
fn stock_soc2_images_are_intact_blw_and_size_like_zenui() {
    let Some(dir) = corpus() else { return };
    let sharc = dir.join("rootfs/opt/neuraldsp/sharc_binaries");
    let spl = read(&sharc, "SOC2_SPL_Core0.blwldr");
    let spl_atma = read(&sharc, "SOC2_SPL_Core0_Atma.blwldr");
    let core0 = read(&sharc, "ZPU_SC589_SHARC_Core0.blwldr");
    let wrappers = [&spl, &spl_atma, &core0].map(|bytes| Blw::parse(bytes).unwrap());
    for wrapper in &wrappers {
        assert_eq!(
            wrapper.payload_len() as usize,
            wrapper.as_bytes().len() - blw::HEADER_BYTES
        );
        assert_eq!(wrapper.flags(), 1);
        assert_eq!(wrapper.key_id(), wrappers[0].key_id());
    }
    assert_ne!(wrappers[0].nonce(), wrappers[2].nonce());

    let normal = Soc2Images::new(&spl, &core0).unwrap();
    let atma = Soc2Images::new(&spl_atma, &core0).unwrap();
    assert_eq!(
        (normal.total_len(), atma.total_len()),
        (15_280 + 481_124, 15_936 + 481_124)
    );
    let mut boot = Soc2Boot::new(normal, None);
    assert_eq!(boot.begin().unwrap().argument, 496_404);
    let (copy, request) = boot.on_buffer_address(PhysAddr(0x8800_0000)).unwrap();
    assert_eq!(request.argument, 15_268);
    let mut bus = RecordingBus::new(());
    execute(&copy, &mut bus).unwrap();
    assert_eq!(
        bus.memory.bytes(PhysAddr(0x8800_0000 + 15_280), 4),
        b"BLw\x01"
    );
}
