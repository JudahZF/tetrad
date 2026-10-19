//! `LinuxRoot` against a temp-dir fake root: the stock file and flag rules, firmware selection
//! and the `/dev/mem` windows over a sparse file. No device node and no real system path is
//! touched; all bytes are invented.
#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use tetrad_app::device::linux::{LinuxError, LinuxRoot, device_cipher};
use tetrad_app::device::platform::{DspFile, Marker};
use tetrad_capture::session::SharcMemory;
use tetrad_dsp_host::SharedMemory;
use tetrad_dsp_protocol::address::Address;
use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot};
use tetrad_hal::platform::DisplayPanel;
use tetrad_loader::plan::Variant;
use tetrad_settings::{SettingsFileId, SettingsStorage};

/// A private fake root, removed on drop.
struct FakeRoot(PathBuf);

impl FakeRoot {
    fn new(test: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-app-device-linux-{test}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for dir in [
            "tmp",
            "dev",
            "media/p4",
            "usr/bin",
            "opt/neuraldsp/sharc_binaries",
            "opt/neuraldsp/blobs/core1",
            "opt/neuraldsp/blobs/core2",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self(root)
    }

    fn put(&self, stock: &str, bytes: &[u8]) {
        fs::write(self.at(stock), bytes).unwrap();
    }

    fn at(&self, stock: &str) -> PathBuf {
        self.0.join(stock.trim_start_matches('/'))
    }

    fn linux(&self) -> LinuxRoot {
        LinuxRoot::new(&self.0)
    }
}

impl Drop for FakeRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Markers are regular files (a directory is not one); ZenUI writes only `/tmp/sharcs_started`.
#[test]
fn markers_follow_the_stock_file_rules() {
    let fake = FakeRoot::new("markers");
    let root = fake.linux();
    assert!(!root.marker(Marker::SharcsStarted).unwrap());

    fs::create_dir_all(fake.at("/opt/neuraldsp/factory_image_flag")).unwrap();
    assert!(!root.marker(Marker::FactoryImage).unwrap());
    fake.put("/opt/neuraldsp/noslaveboot", b"");
    assert!(root.marker(Marker::NoSlaveBoot).unwrap());

    root.set_marker(Marker::SharcsStarted).unwrap();
    root.set_marker(Marker::SharcsStarted).unwrap();
    assert!(root.marker(Marker::SharcsStarted).unwrap());
    assert!(matches!(
        root.set_marker(Marker::CcesBoot),
        Err(LinuxError::Unsupported(_))
    ));
    assert!(!fake.at("/opt/neuraldsp/ccesboot").exists());
}

/// `/tmp/qc_display` names the backlight panel (`CZendisplay::Open` VA 0x8a38); missing is
/// stock `displayinfo_t` 0.
#[test]
fn display_panel_comes_from_qc_display() {
    let fake = FakeRoot::new("qc-display");
    let root = fake.linux();
    assert_eq!(root.display_panel(), None);
    fake.put("/tmp/qc_display", b"fvd\n");
    assert_eq!(root.display_panel(), Some(DisplayPanel::Fvd));
}

/// Main `0x7dd68`: increment, then read back; a missing or non-numeric file counts as 0.
#[test]
fn boot_count_increments_like_stock_main() {
    let fake = FakeRoot::new("boot-count");
    let root = fake.linux();
    assert_eq!(root.boot_count(), 1);
    assert_eq!(
        fs::read_to_string(fake.at("/opt/neuraldsp/boot_count.txt")).unwrap(),
        "1"
    );
    fake.put("/opt/neuraldsp/boot_count.txt", b" 41\nignored");
    assert_eq!(root.boot_count(), 42);
    fake.put("/opt/neuraldsp/boot_count.txt", b"many");
    assert_eq!(root.boot_count(), 1);
}

#[test]
fn settings_replace_whole_files_under_the_root() {
    let fake = FakeRoot::new("settings");
    let mut settings = fake.linux().settings();
    assert_eq!(settings.read(SettingsFileId::Zenjack).unwrap(), None);
    settings
        .write(SettingsFileId::Zenjack, b"[General]\na=1\n")
        .unwrap();
    settings
        .write(SettingsFileId::Zenjack, b"[General]\n")
        .unwrap();
    assert_eq!(
        fs::read(fake.at("/media/p4/ZenjackConfig.ini")).unwrap(),
        b"[General]\n"
    );
    assert_eq!(fs::read_dir(fake.at("/media/p4")).unwrap().count(), 1);
    settings.remove(SettingsFileId::Zenjack).unwrap();
    settings.remove(SettingsFileId::Zenjack).unwrap();
    assert_eq!(settings.read(SettingsFileId::Zenjack).unwrap(), None);
}

/// `0x8a5a74`: the Atma SPL only for the mini; the factory pair only with its flag, and then
/// it must exist.
#[test]
fn soc2_images_follow_variant_and_factory_flag() {
    let fake = FakeRoot::new("soc2");
    let root = fake.linux();
    let dir = "/opt/neuraldsp/sharc_binaries";
    for (name, byte) in [
        ("SOC2_SPL_Core0.blwldr", 1),
        ("SOC2_SPL_Core0_Atma.blwldr", 2),
        ("ZPU_SC589_SHARC_Core0.blwldr", 3),
        ("SOC2_SPL_Emu_Core0.ldr", 4),
    ] {
        fake.put(&format!("{dir}/{name}"), &[byte]);
    }
    let qc = root.soc2_firmware(Variant::QuadCortex).unwrap();
    assert_eq!((qc.normal.spl, qc.normal.core0), (vec![1], vec![3]));
    assert!(qc.factory.is_none());
    let atma = root.soc2_firmware(Variant::Atma).unwrap();
    assert_eq!(atma.normal.spl, vec![2]);

    fake.put("/opt/neuraldsp/factory_image_flag", b"");
    assert!(matches!(
        root.soc2_firmware(Variant::QuadCortex),
        Err(LinuxError::File { .. })
    ));
    fake.put(&format!("{dir}/SOC2_FACTORY.blwldr"), &[5]);
    let factory = root
        .soc2_firmware(Variant::QuadCortex)
        .unwrap()
        .factory
        .unwrap();
    assert_eq!((factory.spl, factory.core0), (vec![4], vec![5]));
}

/// Encrypted DSP files come back raw from their stock paths; blob names cannot leave `core1/`.
#[test]
fn dsp_files_resolve_stock_paths() {
    let fake = FakeRoot::new("dsp-files");
    let mut root = fake.linux();
    fake.put("/opt/neuraldsp/ModelRepo.xml", b"repo");
    fake.put("/opt/neuraldsp/blobs/core1/light_blob.bin", b"blob");
    fake.put("/opt/neuraldsp/blobs/core2/diffs.blob", b"diffs");
    assert_eq!(root.dsp_file(DspFile::ModelRepo).unwrap(), b"repo");
    assert_eq!(root.dsp_file(DspFile::Blob("light")).unwrap(), b"blob");
    assert_eq!(
        root.dsp_file(DspFile::Diffs(DspCore::Core1)).unwrap(),
        b"diffs"
    );
    assert!(matches!(
        root.dsp_file(DspFile::Blob("../core2/x")),
        Err(LinuxError::BlobName(_))
    ));
    assert!(matches!(
        root.dsp_file(DspFile::Diffs(DspCore::Core0)),
        Err(LinuxError::Unsupported(_))
    ));
}

/// The ring window is zeroed after the map; a core-1 model address lands at physical
/// `0x28800000 + address` (`externalTransfer` `0x529d88`). `/dev/mem` is a sparse file here.
#[test]
fn dsp_windows_map_the_ring_and_core_windows() {
    let fake = FakeRoot::new("windows");
    let mem = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(fake.at("/dev/mem"))
        .unwrap();
    mem.set_len(0x28ba_0000).unwrap();
    mem.write_all_at(&[0xff; 64], 0x2008_8000).unwrap();
    mem.write_all_at(&[0xff; 64], 0x2008_8000 + 0x1791c - 64)
        .unwrap();

    let mut windows = fake.linux().dsp_windows().unwrap();
    assert_eq!(windows.messages.len(), 0x1791c);
    let mut ring = vec![0xaa; 0x1791c];
    mem.read_exact_at(&mut ring, 0x2008_8000).unwrap();
    assert!(ring.iter().all(|&byte| byte == 0));

    let target = DeploymentTarget {
        core: DspCore::Core1,
        slot: ModuleSlot::try_from(0).unwrap(),
    };
    windows
        .cores
        .get_mut(DspCore::Core1)
        .unwrap()
        .load_model(target, Address::new(0x38_0000), &[1, 2, 3, 4])
        .unwrap();
    let mut code = [0; 4];
    mem.read_exact_at(&mut code, 0x2880_0000 + 0x38_0000)
        .unwrap();
    assert_eq!(code, [1, 2, 3, 4]);
}

/// Neural Capture V1 transfers (file → SHARC VA 0x333490, SHARC → file 0x337758) land at the
/// buffer's physical address through a page-aligned map; an unaligned address and an odd
/// length round trip byte for byte. Owned guard (review-w6 F13): a range outside SHARC L2 and
/// DDR is refused before any map; outside them only the fault count word can be read.
#[test]
fn sharc_window_round_trips_at_the_buffer_address() {
    let fake = FakeRoot::new("sharc");
    let mem = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(fake.at("/dev/mem"))
        .unwrap();
    mem.set_len(0x2840_0000).unwrap();
    let address = 0x2008_1235;
    let bytes: Vec<u8> = (0..0x2865u32).map(|i| (i * 7 + 1) as u8).collect();

    let mut window = fake.linux().sharc_memory();
    window.write(address, &bytes).unwrap();
    let mut stored = vec![0; bytes.len()];
    mem.read_exact_at(&mut stored, address.into()).unwrap();
    assert_eq!(stored, bytes);
    let mut back = vec![0; bytes.len()];
    window.read(address, &mut back).unwrap();
    assert_eq!(back, bytes);

    let refused = |result: std::io::Result<()>| {
        result.is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
    };
    // Below L2, and running past its end.
    assert!(refused(window.write(0x2000_1235, &[1])));
    assert!(refused(window.write(0x200b_fffe, &[1, 2, 3])));
    let fault = tetrad_app::device::bringup::FAULT_COUNT_ADDRESS;
    assert!(refused(window.write(fault, &[0; 4])));
    window.read(fault, &mut [0; 4]).unwrap();
    assert!(refused(window.read(fault, &mut [0; 8])));
}

/// Kernels are never decrypted or returned for a loadsharc that is not the pinned build.
#[test]
fn firmware_refuses_an_unpinned_loadsharc() {
    let fake = FakeRoot::new("loadsharc");
    fake.put("/usr/bin/loadsharc_SC589", b"\x7fELF invented");
    let error = fake.linux().firmware(Variant::QuadCortex).unwrap_err();
    assert!(
        matches!(
            error,
            LinuxError::Unsupported(_) | LinuxError::WrongImage(_)
        ),
        "{error}"
    );
}

/// The `run` preflight (C6): a missing or unpinned stock ZenUI fails it (exit 4 in `main`);
/// with the CorOS 4.1.0 rootfs extract next to the repo (`qc-dev`, not in git) it passes.
#[test]
fn pinned_firmware_check_needs_the_coros_4_1_0_images() {
    let fake = FakeRoot::new("pins");
    assert!(matches!(
        fake.linux().check_pinned_firmware(),
        Err(LinuxError::File { .. })
    ));
    fake.put("/opt/neuraldsp/ZenUI", b"\x7fELF invented");
    fake.put("/usr/bin/loadsharc_SC589", b"\x7fELF invented");
    assert!(matches!(
        fake.linux().check_pinned_firmware(),
        Err(LinuxError::WrongImage("ZenUI"))
    ));
    // With a copy of the CorOS root file system (docs/development.md, "Corpus tests"): the stock
    // pins hold.
    if let Some(dir) = std::env::var_os("QC_CORPUS_DIR") {
        LinuxRoot::new(Path::new(&dir).join("rootfs"))
            .check_pinned_firmware()
            .unwrap();
    }
}

/// `run` and `bringup` take the comm name `ZenUI` before the slow firmware hash: a stage that
/// hashed first was restarted by ZenWatchdog (CorOS 4.1.1, 2026-10-18). Here the ZenUI image is a
/// link to the comm file, so the hash can read it only when the rename came first; then the
/// check fails on the missing loadsharc, not on ZenUI.
#[test]
fn the_comm_name_comes_before_the_firmware_hash() {
    let fake = FakeRoot::new("comm");
    fs::create_dir_all(fake.at("/proc/self")).unwrap();
    std::os::unix::fs::symlink(fake.at("/proc/self/comm"), fake.at("/opt/neuraldsp/ZenUI"))
        .unwrap();
    let check = fake.linux().start_as_stock().unwrap();
    assert_eq!(fs::read(fake.at("/proc/self/comm")).unwrap(), b"ZenUI");
    match check {
        Err(LinuxError::File { path, .. }) => {
            assert_eq!(path, fake.at("/usr/bin/loadsharc_SC589"));
        }
        other => panic!("{other:?}"),
    }
}

/// The seed VA belongs to one ZenUI build; any other image is refused before a key exists.
#[test]
fn device_cipher_refuses_an_unpinned_zenui() {
    assert!(matches!(
        device_cipher(b"\x7fELF invented".to_vec(), b"A1B2C3D4E"),
        Err(LinuxError::StockImage(_))
    ));
}

/// Corpus: the seed read through the ELF tables at VA `0x1411478` plus a serial gives the same
/// keystream as tetrad-media's factory-IR cipher (file offset 21,009,528). Invented serial; no
/// key byte is printed.
#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
fn device_cipher_matches_the_factory_ir_cipher() {
    use tetrad_media::factory::{DeviceCipher, DeviceSerial, FactoryIrSeed};
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return;
    };
    let image = fs::read(Path::new(&dir).join("rootfs/opt/neuraldsp/ZenUI")).unwrap();
    let serial = b"A1B2C3D4E";
    let expected = DeviceCipher::seed_and_serial(
        &FactoryIrSeed::from_stock_zenui(&image).unwrap(),
        &DeviceSerial::parse(serial).unwrap(),
    );
    let mut ours = vec![0; 64];
    let mut theirs = vec![0; 64];
    device_cipher(image, serial)
        .unwrap()
        .apply_keystream(&mut ours);
    expected.apply(&mut theirs);
    assert!(ours == theirs, "keystreams differ");
}

/// W1 (`0x8a4c30`): the uio2 clock thread arms the node three times, sends exactly one `0xF8`
/// per interrupt after its re-arm, and stops and releases the UART on drop. A socket pair
/// stands in for `/dev/uio2` and the UART.
#[test]
fn uio2_clock_out_sends_one_timing_clock_per_interrupt() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;
    use tetrad_app::device::linux::ClockOut;
    use tetrad_sys::uio::UioDevice;

    let (node, mut peer) = UnixStream::pair().unwrap();
    let (uart, mut line) = UnixStream::pair().unwrap();
    for end in [&peer, &line] {
        end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    }
    let clock = ClockOut::spawn(UioDevice::from_fd(node.into()), uart).unwrap();
    let mut words = [0; 12];
    peer.read_exact(&mut words).unwrap();
    assert_eq!(words, [1, 0, 0, 0].repeat(3).as_slice());

    peer.write_all(&7_u32.to_ne_bytes()).unwrap();
    let mut rearm = [0; 4];
    peer.read_exact(&mut rearm).unwrap();
    assert_eq!(rearm, [1, 0, 0, 0]);
    let mut byte = [0; 1];
    line.read_exact(&mut byte).unwrap();
    assert_eq!(byte, [0xf8]);

    drop(clock);
    let mut rest = Vec::new();
    line.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "{rest:x?}");
}
