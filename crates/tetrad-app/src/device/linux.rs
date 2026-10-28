//! Linux platform (owner: 2B-linux) over tetrad-sys.
//!
//! Two layers:
//! - [`LinuxRoot`]: everything reached by a path under one root: markers, `boot_count.txt`, the
//!   settings INI files, firmware and DSP data files, `/dev/mem`, `/dev/corectrl`, GPIO 97 and
//!   `/dev/uio2`. The device uses `/`; tests use a temp dir.
//! - [`LinuxPlatform::open`]: the fixed libzenhal nodes, framebuffer, touch, MIDI UART, splash
//!   queue and power programs. Explicit hardware opt-in; tests never call it.
//!
//! Secrets: the kernel and `moduleReport.xml` secret (ZenUI `0x6abcac`: 57-byte seed + 9-byte
//! serial) is read at run time from the device's own ZenUI image and `/media/p4/qc_sn` and kept
//! only as a cipher in memory. The ModelRepo and blob keys come from the DSP (`dsp.rs`).
//!
//! Firmware: [`LinuxRoot::check_pinned_firmware`] identifies the stock build
//! (`tetrad_firmware::Firmware`); `main` selects it. Every later stock read checks its file
//! against the running build (`tetrad_firmware::running`).
//!
//! Logging: [`Platform::log`] writes to stderr and ignores errors. After
//! `PowerStep::StopLogPipelines` stderr is a pipe without a reader (review-b2-linux S1), so
//! nothing here uses `println!`/`eprintln!`.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use qc_format::crypto::NdspCipher;
use tetrad_dsp_host::drain::StockThread;
use tetrad_dsp_host::model::{CoreWindows, ModelError, ModelMemory};
use tetrad_dsp_host::session::{Clock, SystemClock};
use tetrad_dsp_host::setup::tables::{CIPHER_SEED, check_zenui_vas};
use tetrad_dsp_host::setup::{ElfTables, StockTables, TableError};
use tetrad_dsp_host::transport::{MESSAGE_MEMORY_BASE, MESSAGE_MEMORY_BYTES};
use tetrad_dsp_host::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::DspCore;
use tetrad_dsp_protocol::address::Address;
use tetrad_firmware::{Firmware, FirmwareError, Profile};
use tetrad_hal::hal::{Board, Hal, Query, Reading, StockHal};
use tetrad_hal::input::InputRecord;
use tetrad_hal::node::Node;
use tetrad_hal::platform::{DisplayPanel, Platform as HalPlatform, Product};
use tetrad_loader::PhysAddr;
use tetrad_loader::plan::Variant;
use tetrad_media::catalog::FACTORY_IR_ENTITIES_PATH;
use tetrad_media::factory::{
    DeviceCipher, DeviceSerial, FACTORY_IR_DIR, FactoryIrError, SERIAL_PATH,
};
use tetrad_services::http::CurlHttp;
use tetrad_services::system::{SystemFiles, SystemNet, SystemRunner};
use tetrad_services::wifi::{CONTROL_SOCKET, LinkError, SupplicantLink};
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_storage::media::MediaRoot;
use tetrad_sys::corectrl::CoreControlNode;
use tetrad_sys::display::Framebuffer;
use tetrad_sys::gpio::{SysfsGpio, UIO2_GATE_GPIO};
use tetrad_sys::hal::LinuxHal;
use tetrad_sys::input::{Frame, RecordNode};
use tetrad_sys::ioctl::IoctlNode;
use tetrad_sys::lifecycle::power::{LinuxPower, PowerPrograms};
use tetrad_sys::lifecycle::splash::PosixSplashQueue;
use tetrad_sys::loader::{BootHardware, L1};
use tetrad_sys::mem::{MapError, PhysicalWindow};
use tetrad_sys::paths;
use tetrad_sys::serial::MidiUart;
use tetrad_sys::supplicant::{self, Budget, Receipt, Transport};
use tetrad_sys::uio::UioDevice;
use zeroize::Zeroizing;

use super::bringup::FAULT_COUNT_ADDRESS;
use super::platform::{
    DspFile, DspWindows, Input, Marker, Platform, ServiceBackends, ServiceIo, SharcFirmware,
    Soc2Firmware, Soc2Pair, StorageRoots, TouchSample,
};
use super::services::{self, Lane, ServiceJob, ServiceNotice};
use super::{Notice, Subsystem};

/// ZenWatchdog finds the UI by this comm name and reboots the unit without it (o-lifecycle).
pub const STOCK_COMM: &str = "ZenUI";
/// The comm name of this process ([`LinuxRoot::take_stock_comm`]).
const SELF_COMM: &str = "/proc/self/comm";
/// Exit code of `tetrad run` when [`LinuxRoot::check_pinned_firmware`] fails. Nothing was opened
/// or written, so the boot launcher (`scripts/launcher/tetrad-launch.sh`, `TD_WRONG_FIRMWARE`)
/// starts stock ZenUI in the same boot (docs/launcher.md).
pub const WRONG_FIRMWARE_EXIT: u8 = 4;
const CMDLINE: &str = "/proc/cmdline";
/// The stock ZenUI ELF: source of the secret seed. Keep the stock copy here when tetrad runs.
const ZENUI_IMAGE: &str = "/opt/neuraldsp/ZenUI";
const LOADSHARC: &str = "/usr/bin/loadsharc_SC589";
const SHARC_DIR: &str = "/opt/neuraldsp/sharc_binaries";
const KERNEL_CORE1: &str = "ZPUKernel_Core1.ldr";
const KERNEL_CORE2: &str = "ZPUKernel_Core2.ldr";
/// SOC2 pairs `[SPL, core 0]` (`0x8a5a74`). The Atma SPL is chosen by `0x49b45c`
/// (platform == ATMA), the same predicate as [`Variant::Atma`].
const SOC2_QC: [&str; 2] = ["SOC2_SPL_Core0.blwldr", "ZPU_SC589_SHARC_Core0.blwldr"];
const SOC2_ATMA: [&str; 2] = ["SOC2_SPL_Core0_Atma.blwldr", "ZPU_SC589_SHARC_Core0.blwldr"];
/// Only with `factory_image_flag`; tried first. Directory inferred (same as the normal pair).
const SOC2_FACTORY: [&str; 2] = ["SOC2_SPL_Emu_Core0.ldr", "SOC2_FACTORY.blwldr"];
const MODEL_REPO: &str = "/opt/neuraldsp/ModelRepo.xml";
const MODULE_REPORT: &str = "/opt/neuraldsp/blobs/moduleReport.xml";
const BLOBS: &str = "/opt/neuraldsp/blobs";
const BOOT_COUNT: &str = "/opt/neuraldsp/boot_count.txt";
/// Skips the diagnostics tick (`0x50ea74`, [`Marker::NoPoll`]).
const NOPOLL: &str = "/opt/neuraldsp/nopoll";
const POINTERCAL: &str = "/etc/pointercal";
/// Written by drvinit (`S80drvinit`); libzenhal reads it once at `CZendisplay::Open`.
const QC_DISPLAY: &str = "/tmp/qc_display";
/// Owned: binary HTTP bodies for curl (`CurlHttp::scratch`).
const HTTP_SCRATCH: &str = "/tmp/tetrad-http.body";
/// `externalTransfer` (`0x529d88`) addresses are core-window relative; the L1 window starts here.
const L1_WINDOW_OFFSET: u32 = 0x24_0000;
const TOUCH_DRAIN_LIMIT: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum LinuxError {
    #[error("not supported yet: {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{path}: {source}")]
    File {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("/dev/mem window: {0}")]
    Map(#[from] MapError),
    #[error("DSP window: {0}")]
    Memory(#[from] MemoryError),
    #[error("DSP core window: {0}")]
    Model(#[from] ModelError),
    #[error("stock ZenUI image: {0}")]
    StockImage(#[from] TableError),
    #[error("device serial: {0}")]
    Serial(#[from] FactoryIrError),
    #[error("{0} is not a known stock build")]
    WrongImage(&'static str),
    /// A known build that Tetrad cannot run yet (open profile slots), or a second build
    /// selection.
    #[error(transparent)]
    Firmware(FirmwareError),
    #[error("blob name {0:?} is not a plain file name")]
    BlobName(String),
    #[error("{0} is not open on this unit")]
    Unavailable(&'static str),
}

/// `stock` (an absolute device path) under `root`.
fn under(root: &Path, stock: impl AsRef<Path>) -> PathBuf {
    let stock = stock.as_ref();
    root.join(stock.strip_prefix("/").unwrap_or(stock))
}

fn marker_path(marker: Marker) -> &'static str {
    match marker {
        Marker::SharcsStarted => "/tmp/sharcs_started",
        Marker::CcesBoot => "/opt/neuraldsp/ccesboot",
        Marker::NoSlaveBoot => "/opt/neuraldsp/noslaveboot",
        Marker::FactoryImage => "/opt/neuraldsp/factory_image_flag",
        Marker::AtmaEmulation => HalPlatform::ATMA_EMULATION_FILE,
        Marker::EssEmulation => HalPlatform::ESS_EMULATION_FILE,
        Marker::NoPoll => NOPOLL,
    }
}

/// The kernel and `moduleReport.xml` cipher (ZenUI `0x6abcac`, `EVP_BytesToKey` SHA-1 × 10 over
/// seed ‖ serial; the NDSP scheme). `zenui_image` must be the ZenUI of the running build,
/// because the seed VA belongs to that build. The secret bytes never leave this function.
/// Both buffers are `Zeroizing` and the joined one has its final capacity, so no
/// reallocation frees an unzeroed copy (review r11 K2).
pub fn device_cipher(zenui_image: Vec<u8>, qc_sn: &[u8]) -> Result<NdspCipher, LinuxError> {
    let seed = Zeroizing::new(ElfTables::new(zenui_image)?.read(CIPHER_SEED)?);
    let serial = DeviceSerial::parse(qc_sn)?;
    let serial = serial.as_str().as_bytes();
    let mut secret = Zeroizing::new(Vec::with_capacity(seed.len() + serial.len()));
    secret.extend_from_slice(&seed);
    secret.extend_from_slice(serial);
    Ok(NdspCipher::derive(&secret))
}

/// The loadsharc of the running build must match before `StockBooter::parse`: the booter
/// table addresses belong to that build.
fn check_loadsharc(image: &[u8]) -> Result<(), LinuxError> {
    use sha2::{Digest, Sha256};
    let expected = tetrad_firmware::running()
        .firmware
        .identity()
        .loadsharc_sha256;
    if Sha256::digest(image).as_slice() == expected {
        Ok(())
    } else {
        Err(LinuxError::WrongImage("loadsharc_SC589"))
    }
}

/// Files and memory nodes under one root (`/` on the device).
pub struct LinuxRoot {
    root: PathBuf,
    /// Built once from the ZenUI image and the serial; memory only. The kernels, the media
    /// library, Cortex Control and capture saves share this one key (review-w6 F9).
    cipher: Option<DeviceCipher>,
}

impl LinuxRoot {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cipher: None,
        }
    }

    /// `stock` (an absolute device path) under this root.
    pub fn path(&self, stock: impl AsRef<Path>) -> PathBuf {
        under(&self.root, stock)
    }

    /// Names this process [`STOCK_COMM`]. ZenWatchdog restarts the unit when `pidof ZenUI`
    /// fails for about 3 s (o-lifecycle §2), so each on-unit command that runs while stock
    /// ZenUI is stopped (`run`, `select`, `bringup`) does this before any slow step.
    pub fn take_stock_comm(&self) -> io::Result<()> {
        fs::write(self.path(SELF_COMM), STOCK_COMM)
    }

    /// The start of `tetrad run` and `tetrad bringup`: [`Self::take_stock_comm`], then
    /// [`Self::check_pinned_firmware`], which hashes the 22 MB ZenUI image (about 3 to 6 s on
    /// the unit). The outer error is the rename; each caller treats the check in its own way.
    pub fn start_as_stock(&self) -> io::Result<Result<&'static Profile, LinuxError>> {
        self.take_stock_comm()?;
        Ok(self.check_pinned_firmware())
    }

    fn read(&self, stock: impl AsRef<Path>) -> Result<Vec<u8>, LinuxError> {
        let path = self.path(stock);
        fs::read(&path).map_err(|source| LinuxError::File { path, source })
    }

    /// The `tetrad run` preflight (gap census r48 C6): stock ZenUI and loadsharc must be one
    /// known build (`tetrad_firmware::Firmware::identify`) whose profile is complete and has a
    /// VA for every ZenUI read site, because the seed, table and booter addresses belong to
    /// it. A CorOS update changes both files. Returns the profile for `tetrad_firmware::select`.
    /// Reads only; the 22 MB ZenUI image is hashed in chunks.
    pub fn check_pinned_firmware(&self) -> Result<&'static Profile, LinuxError> {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let digest = |stock: &str| -> Result<Vec<u8>, LinuxError> {
            let path = self.path(stock);
            let mut hasher = Sha256::new();
            let mut chunk = vec![0; 1 << 16];
            let hashed = File::open(&path).and_then(|mut file| {
                loop {
                    match file.read(&mut chunk)? {
                        0 => return Ok(()),
                        read => hasher.update(&chunk[..read]),
                    }
                }
            });
            hashed.map_err(|source| LinuxError::File { path, source })?;
            Ok(hasher.finalize().to_vec())
        };
        let zenui = digest(ZENUI_IMAGE)?;
        let loadsharc = digest(LOADSHARC)?;
        let profile = Firmware::identify(&zenui, &loadsharc)
            .and_then(Firmware::profile)
            .map_err(|error| match error {
                FirmwareError::UnknownZenUi => LinuxError::WrongImage("ZenUI"),
                FirmwareError::LoadsharcMismatch(_) => LinuxError::WrongImage("loadsharc_SC589"),
                error => LinuxError::Firmware(error),
            })?;
        check_zenui_vas(profile)?;
        Ok(profile)
    }

    /// A marker is set when it is a regular file (`QFileInfo::isFile`, a-boot §2).
    pub fn marker(&self, marker: Marker) -> Result<bool, LinuxError> {
        let path = self.path(marker_path(marker));
        match fs::metadata(&path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(LinuxError::File { path, source }),
        }
    }

    /// `touch /tmp/sharcs_started` (`0x8a8420`). Stock ZenUI writes no other marker.
    pub fn set_marker(&self, marker: Marker) -> Result<(), LinuxError> {
        if marker != Marker::SharcsStarted {
            return Err(LinuxError::Unsupported(
                "stock ZenUI writes only /tmp/sharcs_started",
            ));
        }
        let path = self.path(marker_path(marker));
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map(drop)
            .map_err(|source| LinuxError::File { path, source })
    }

    /// Main `0x7dd68` increments the count (`0x004590a0`) and reads it back (`0x00458e54`):
    /// first line, decimal, 0 when missing or not a number. A failed write is ignored, as stock.
    pub fn boot_count(&self) -> u32 {
        let path = self.path(BOOT_COUNT);
        let previous = fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.lines().next()?.trim().parse::<i32>().ok())
            .unwrap_or(0);
        let next = previous.wrapping_add(1);
        let count = match fs::write(&path, next.to_string()) {
            Ok(()) => next,
            Err(_) => previous,
        };
        u32::try_from(count).unwrap_or(0)
    }

    pub fn settings(&self) -> LinuxSettings {
        LinuxSettings {
            root: self.root.clone(),
        }
    }

    pub fn storage(&self) -> StorageRoots {
        StorageRoots {
            user: MediaRoot::new(self.path("/media/p4")),
            factory: MediaRoot::factory(self.path("/opt/neuraldsp")),
            database: self.path("/media/p4/zenui.db"),
        }
    }

    fn cipher(&mut self) -> Result<&DeviceCipher, LinuxError> {
        let cipher = match self.cipher.take() {
            Some(cipher) => cipher,
            None => device_cipher(self.read(ZENUI_IMAGE)?, &self.read(SERIAL_PATH)?)?.into(),
        };
        Ok(self.cipher.insert(cipher))
    }

    /// loadsharc (checked), both kernels decrypted in memory (stock writes temp files,
    /// `0x8a8094`), and the SOC2 images.
    pub fn firmware(&mut self, variant: Variant) -> Result<SharcFirmware, LinuxError> {
        let loadsharc = self.read(LOADSHARC)?;
        check_loadsharc(&loadsharc)?;
        let mut core1 = self.read(Path::new(SHARC_DIR).join(KERNEL_CORE1))?;
        let mut core2 = self.read(Path::new(SHARC_DIR).join(KERNEL_CORE2))?;
        let cipher = self.cipher()?;
        cipher.apply(&mut core1);
        cipher.apply(&mut core2);
        Ok(SharcFirmware {
            loadsharc,
            core1,
            core2,
            soc2: self.soc2_firmware(variant)?,
        })
    }

    /// SOC2 images, copied verbatim (`0x89bae0`). With `factory_image_flag` the factory pair must
    /// read too: stock would stall the copy without it.
    pub fn soc2_firmware(&self, variant: Variant) -> Result<Soc2Firmware, LinuxError> {
        let pair = |[spl, core0]: [&str; 2]| -> Result<Soc2Pair, LinuxError> {
            Ok(Soc2Pair {
                spl: self.read(Path::new(SHARC_DIR).join(spl))?,
                core0: self.read(Path::new(SHARC_DIR).join(core0))?,
            })
        };
        let normal = match variant {
            Variant::QuadCortex => SOC2_QC,
            Variant::Atma => SOC2_ATMA,
        };
        let factory = if self.marker(Marker::FactoryImage)? {
            Some(pair(SOC2_FACTORY)?)
        } else {
            None
        };
        Ok(Soc2Firmware {
            normal: pair(normal)?,
            factory,
        })
    }

    /// Stock DSP data files. Only `moduleReport.xml` is decrypted here (seed + serial); the
    /// others need the keys the DSP sends.
    pub fn dsp_file(&mut self, file: DspFile<'_>) -> Result<Vec<u8>, LinuxError> {
        match file {
            DspFile::ModelRepo => self.read(MODEL_REPO),
            DspFile::ModuleReport => {
                let mut report = self.read(MODULE_REPORT)?;
                self.cipher()?.apply(&mut report);
                Ok(report)
            }
            DspFile::Blob(name) => {
                if name.is_empty() || name.contains(['/', '\0']) {
                    return Err(LinuxError::BlobName(name.to_owned()));
                }
                self.read(format!("{BLOBS}/core1/{name}_blob.bin"))
            }
            DspFile::Diffs(core) => {
                let dir = match core {
                    DspCore::Core0 => {
                        return Err(LinuxError::Unsupported(
                            "transport core 0 has no diffs.blob",
                        ));
                    }
                    DspCore::Core1 => "core2",
                    DspCore::Core2 => "core3",
                    DspCore::Core3 => "core4",
                };
                self.read(format!("{BLOBS}/{dir}/diffs.blob"))
            }
            DspFile::ZenUi => self.read(ZENUI_IMAGE),
            DspFile::FactoryIrCatalog => self.read(FACTORY_IR_ENTITIES_PATH),
            DspFile::FactoryIr(path) => {
                let name = path
                    .strip_prefix(FACTORY_IR_DIR)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .filter(|name| !name.is_empty() && !name.contains(['/', '\0']));
                match name {
                    Some(_) => self.read(path),
                    None => Err(LinuxError::BlobName(path.to_owned())),
                }
            }
        }
    }

    /// The factory IR and user IR metadata cipher: the kernel cipher itself (same seed and
    /// serial, n-media §1). The result shares the cached key.
    pub fn media_cipher(&mut self) -> Result<DeviceCipher, LinuxError> {
        self.cipher().cloned()
    }

    /// `/dev/mem` and `/dev/corectrl` for tetrad-loader, opened like `BootHardware::open`.
    pub fn boot_bus(&self) -> Result<BootHardware, LinuxError> {
        let path = self.path(paths::DEV_MEM);
        let mem = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_SYNC)
            .open(&path)
            .map_err(|source| LinuxError::File { path, source })?;
        let path = self.path(paths::CORECTRL);
        let corectrl =
            CoreControlNode::open(&path).map_err(|source| LinuxError::File { path, source })?;
        Ok(BootHardware::new(mem, corectrl)?)
    }

    /// Ring window `0x20088000`/`0x1791c`, zeroed after the map as stock does (b-linux §2), and
    /// the core 0/1 windows: `externalTransfer` (`0x529d88`) bases `0x28000000`/`0x28800000`,
    /// ranges `+0x240000..+0x3a0000`, which are exactly the loadsharc L1 windows. Sections
    /// outside L1 go to the SHARC DDR below Linux RAM (`capture::CAPTURE_WINDOWS[1]`), mapped
    /// at its physical address as stock does (0x529e64); S3 on the QC (2026-10-15) needed it
    /// for a section at `0x8000_0490`.
    pub fn dsp_windows(&self) -> Result<DspWindows<PhysicalWindow>, LinuxError> {
        let mem = self.path(paths::DEV_MEM);
        let mut messages =
            PhysicalWindow::open(&mem, MESSAGE_MEMORY_BASE.into(), MESSAGE_MEMORY_BYTES)?;
        messages.write(0, &vec![0; MESSAGE_MEMORY_BYTES])?;
        let window = |core, (base, len): (u32, u32)| -> Result<_, LinuxError> {
            let memory = PhysicalWindow::open(&mem, base.into(), len as usize)?;
            let (ddr, ddr_len) = super::capture::CAPTURE_WINDOWS[1];
            let external = PhysicalWindow::open(&mem, ddr.into(), ddr_len as usize)?;
            Ok(
                ModelMemory::new(core, Address::new(L1_WINDOW_OFFSET), memory)?
                    .with_external(Address::new(ddr), external)?,
            )
        };
        let cores = CoreWindows::new(
            window(DspCore::Core0, L1[0])?,
            window(DspCore::Core1, L1[1])?,
        )?;
        Ok(DspWindows { messages, cores })
    }

    /// The Neural Capture SHARC window over this root's `/dev/mem`.
    pub fn sharc_memory(&self) -> DevMemSharc {
        DevMemSharc {
            mem: self.path(paths::DEV_MEM),
        }
    }

    /// GPIO 97 (`0x8a7bbc`): export it when `value` is missing, then test byte 0 against `'1'`.
    pub fn uio2_gate(&self) -> io::Result<bool> {
        let gpio = SysfsGpio::new(self.path(paths::SYSFS_GPIO), UIO2_GATE_GPIO);
        gpio.export_input()?;
        gpio.read()
    }

    /// `/etc/pointercal` for the tslib `linear` stage. `None` when missing or malformed.
    fn calibration(&self) -> Option<Calibration> {
        Calibration::parse(&fs::read_to_string(self.path(POINTERCAL)).ok()?)
    }

    /// The backlight panel from `/tmp/qc_display`; `None` when missing or unknown.
    pub fn display_panel(&self) -> Option<DisplayPanel> {
        DisplayPanel::from_file(&fs::read(self.path(QC_DISPLAY)).ok()?)
    }
}

/// Neural Capture V1 transfers at DSP buffer addresses. Like stock (file → SHARC VA
/// 0x333490, SHARC → file 0x337758) every transfer opens `/dev/mem` `O_RDWR|O_SYNC`, maps the
/// page-aligned range around the buffer, copies and unmaps. The copy uses the window's
/// aligned volatile accesses, not stock `memcpy` (tetrad-sys `mem`).
///
/// Owned guard (review-w6 F13; stock maps any address): a range must lie inside SHARC L2 or
/// the DDR window ([`TRANSFER_WINDOWS`]). The one read outside them is the bring-up fault
/// count word ([`FAULT_COUNT_ADDRESS`], 4 bytes). Every read maps `O_RDONLY` and `PROT_READ`
/// (tetrad-sys `ReadOnlyWindow`), so the fault watch can never write.
pub struct DevMemSharc {
    mem: PathBuf,
}

/// `(base, len)` of the physical ranges a DSP-supplied transfer address may use: SHARC L2
/// (tetrad-sys `loader::L2`) and the SC589 DDR window of SOC2 buffers (tetrad-loader `SOC2_DDR`).
pub const TRANSFER_WINDOWS: [(u32, u32); 2] =
    [tetrad_sys::loader::L2, tetrad_loader::soc2::SOC2_DDR];

/// The F13 range check of one transfer.
fn check_transfer(address: u32, len: usize, write: bool) -> io::Result<()> {
    let end = u64::from(address) + len as u64;
    let inside = TRANSFER_WINDOWS
        .iter()
        .any(|&(base, size)| address >= base && end <= u64::from(base) + u64::from(size));
    let fault_word = !write && address == FAULT_COUNT_ADDRESS && len == 4;
    if inside || fault_word {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("SHARC range {address:#x} + {len:#x} is outside L2 and DDR"),
        ))
    }
}

fn map_error(error: MapError) -> io::Error {
    match error {
        MapError::Io(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidInput, other),
    }
}

impl tetrad_capture::session::SharcMemory for DevMemSharc {
    fn write(&mut self, address: u32, bytes: &[u8]) -> io::Result<()> {
        check_transfer(address, bytes.len(), true)?;
        PhysicalWindow::open(&self.mem, address.into(), bytes.len())
            .map_err(map_error)?
            .write(0, bytes)
            .map_err(io::Error::other)
    }

    fn read(&mut self, address: u32, out: &mut [u8]) -> io::Result<()> {
        check_transfer(address, out.len(), false)?;
        PhysicalWindow::open_read_only(&self.mem, address.into(), out.len())
            .map_err(map_error)?
            .read(0, out)
            .map_err(io::Error::other)
    }
}

/// The four stock INI files under `/media/p4`.
pub struct LinuxSettings {
    root: PathBuf,
}

impl LinuxSettings {
    fn path(&self, file: SettingsFileId) -> PathBuf {
        under(&self.root, file.path())
    }
}

impl SettingsStorage for LinuxSettings {
    fn read(&self, file: SettingsFileId) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(file)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Whole-file replace through a temp file and `rename`, then `fsync` of the file and its
    /// directory. Owned: stock follows `QSettings::sync` with a global `sync()`.
    fn write(&mut self, file: SettingsFileId, bytes: &[u8]) -> io::Result<()> {
        let path = self.path(file);
        let temp = path.with_extension("ini.tmp");
        let mut out = File::create(&temp)?;
        out.write_all(bytes)?;
        out.sync_all()?;
        fs::rename(&temp, &path)?;
        File::open(path.parent().unwrap_or(&self.root))?.sync_all()
    }

    fn remove(&mut self, file: SettingsFileId) -> io::Result<()> {
        match fs::remove_file(self.path(file)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// tslib `linear`: `x' = (a2 + a0·x + a1·y) / a6`, `y' = (a5 + a3·x + a4·y) / a6`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Calibration([i32; 7]);

impl Default for Calibration {
    fn default() -> Self {
        Self([1, 0, 0, 0, 1, 0, 1])
    }
}

impl Calibration {
    /// The first seven integers of `/etc/pointercal`; later fields (resolution, rotation) are ignored.
    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_ascii_whitespace().map(str::parse::<i32>);
        let mut a = [0; 7];
        for slot in &mut a {
            *slot = words.next()?.ok()?;
        }
        (a[6] != 0).then_some(Self(a))
    }

    fn apply(self, x: i32, y: i32) -> (i64, i64) {
        let a = self.0.map(i64::from);
        let (x, y) = (i64::from(x), i64::from(y));
        (
            (a[2] + a[0] * x + a[1] * y) / a[6],
            (a[5] + a[3] * x + a[4] * y) / a[6],
        )
    }
}

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const SYN_MT_REPORT: u16 = 2;
const SYN_DROPPED: u16 = 3;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_PRESSURE: u16 = 0x18;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
/// `/etc/ts.conf`: `module invert x0=800 y0=480`, `module pthres pmin=1`.
const TS_INVERT: (i32, i32) = (800, 480);
const TS_PMIN: i32 = 1;

/// The stock tslib chain (`/etc/ts.conf`: `input`, `pthres`, `dejitter`, `invert`, `linear`)
/// over raw evdev records. Inferred: ZenUI reads touch through tslib (`QWS_MOUSE_PROTO=tslib`).
/// Owned: `dejitter delta=100` is not applied.
#[derive(Debug, Clone, Default)]
struct TouchDecoder {
    calibration: Calibration,
    x: i32,
    y: i32,
    pressure: i32,
    changed: bool,
    /// Type A multi-touch (no `BTN_TOUCH` or pressure, as on the ILI2130): a frame ends its
    /// contacts with `SYN_MT_REPORT`; a frame with a position or tracking id is a press, one
    /// with none is a release. The frame flags are cleared at each `SYN_REPORT`.
    frame_mt: bool,
    frame_contact: bool,
    /// The device sent `BTN_TOUCH` or pressure once, so the type A fallback is off for good.
    /// Per frame is not enough: the input core drops an unchanged `BTN_TOUCH`, so a hover move
    /// after a lift would read as a press. A tracking id does not count: type A drivers send no
    /// id -1, a lifted contact is just not reported.
    reports_contact: bool,
    /// After `SYN_DROPPED`, the records up to the next `SYN_REPORT` are a partial frame: its
    /// explicit events still count, but it gives no type A press or release.
    dropped: bool,
}

impl TouchDecoder {
    fn new(calibration: Calibration) -> Self {
        Self {
            calibration,
            ..Self::default()
        }
    }

    /// One evdev record. A changed contact becomes one sample at `SYN_REPORT`.
    fn push(&mut self, record: &InputRecord) -> Option<TouchSample> {
        let value = record.value;
        match (record.event_type, record.code) {
            (EV_ABS, ABS_X | ABS_MT_POSITION_X) => {
                self.x = value;
                self.frame_contact = true;
            }
            (EV_ABS, ABS_Y | ABS_MT_POSITION_Y) => {
                self.y = value;
                self.frame_contact = true;
            }
            (EV_ABS, ABS_PRESSURE) | (EV_KEY, BTN_TOUCH) => {
                self.pressure = value;
                self.reports_contact = true;
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                self.pressure = i32::from(value >= 0);
                self.frame_contact |= value >= 0;
            }
            (EV_SYN, SYN_MT_REPORT) => {
                self.frame_mt = true;
                return None;
            }
            (EV_SYN, SYN_DROPPED) => {
                self.dropped = true;
                return None;
            }
            (EV_SYN, SYN_REPORT) => {
                let dropped = std::mem::take(&mut self.dropped);
                if self.frame_mt && !self.reports_contact && !dropped {
                    let pressure = i32::from(self.frame_contact);
                    self.changed |= pressure != self.pressure;
                    self.pressure = pressure;
                }
                self.frame_mt = false;
                self.frame_contact = false;
                return std::mem::take(&mut self.changed).then(|| self.sample());
            }
            _ => return None,
        }
        self.changed = true;
        None
    }

    fn sample(&self) -> TouchSample {
        let (x, y) = self
            .calibration
            .apply(TS_INVERT.0 - self.x, TS_INVERT.1 - self.y);
        let pixel = |value: i64, size: u32| value.clamp(0, i64::from(size) - 1) as u16;
        TouchSample {
            x: pixel(x, tetrad_ui::WIDTH),
            y: pixel(y, tetrad_ui::HEIGHT),
            pressed: self.pressure >= TS_PMIN,
        }
    }
}

/// The touch screen through the stock tslib chain (`TouchDecoder`). `run` and the boot
/// selector (`crate::boot_select`) read it.
pub struct Touch {
    node: RecordNode,
    decoder: TouchDecoder,
    frames: Vec<Frame>,
}

impl Touch {
    /// Opens `/dev/input/event0` with this root's `/etc/pointercal`. When that file is missing
    /// or malformed, touch is uncalibrated and `failures` says so.
    pub fn open(root: &LinuxRoot, failures: &mut Vec<String>) -> io::Result<Self> {
        let calibration = root.calibration().unwrap_or_else(|| {
            failures.push(format!(
                "{POINTERCAL} missing or malformed; touch is uncalibrated"
            ));
            Calibration::default()
        });
        Ok(Self {
            node: RecordNode::open(Path::new(paths::TOUCH_EVENTS))?,
            decoder: TouchDecoder::new(calibration),
            frames: Vec::new(),
        })
    }

    /// Appends one [`Input::Touch`] per changed contact read since the last poll.
    pub fn poll(&mut self, out: &mut Vec<Input>) -> io::Result<()> {
        self.node.drain(&mut self.frames, TOUCH_DRAIN_LIMIT)?;
        for frame in self.frames.drain(..) {
            let record = InputRecord::decode(&frame).expect("a frame is one evdev record");
            out.extend(self.decoder.push(&record).map(Input::Touch));
        }
        Ok(())
    }
}

type MidiRead = io::Result<(Vec<u8>, Duration)>;

/// Owned: pause after a MIDI read error before the next read (the stock `VTIME`).
const MIDI_RETRY: Duration = Duration::from_millis(100);

/// MIDI DIN UART. Writes run on the caller's thread; reads run on a worker, as stock
/// `CMIDI::_Rx` does (blocking reads with the 100 ms `VTIME`). The worker owns a duplicate
/// descriptor and stamps each read with the device clock. A read error is reported once per
/// run of errors and the worker retries after [`MIDI_RETRY`]; it ends at the first send after
/// the port is dropped.
struct MidiPort {
    uart: MidiUart,
    received: Receiver<MidiRead>,
}

impl MidiPort {
    fn open(clock: SystemClock) -> io::Result<Self> {
        let uart = MidiUart::open(Path::new(paths::MIDI_UART))?;
        let mut reader = MidiUart::from_fd(uart.as_fd().try_clone_to_owned()?)?;
        let (sender, received) = mpsc::channel::<MidiRead>();
        std::thread::Builder::new()
            .name("midi-rx".into())
            .spawn(move || {
                let mut buffer = [0; 64];
                let mut failing = false;
                loop {
                    let read = match reader.read_available(&mut buffer) {
                        Ok(None) => continue,
                        Ok(Some(count)) => Ok((buffer[..count.get()].to_vec(), clock.now())),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(error) => Err(error),
                    };
                    let failed = read.is_err();
                    if !(failed && failing) && sender.send(read).is_err() {
                        break;
                    }
                    failing = failed;
                    if failed {
                        std::thread::sleep(MIDI_RETRY);
                    }
                }
            })?;
        Ok(Self { uart, received })
    }
}

/// Stock uio2 thread poll timeout (`mov r2,#45` at `0x8a4cb0`).
const UIO2_POLL: Duration = Duration::from_millis(45);
/// LE `1` to `irqcontrol`: unmask the uio2 interrupt.
const UIO_ARM: [u8; 4] = 1_u32.to_le_bytes();

/// DIN MIDI clock out (`0x8a4c30` → `0x3326c4` type 4 → `CMIDI::MessageSyncPost`): one `0xF8`
/// per uio2 interrupt, written on its own thread to a duplicate UART descriptor, as stock does.
/// It never enters the event loop, which blocks in DSP waits. The SHARC paces the ticks; the
/// ARM has no clock-out setting gate. Drop stops the thread and joins it (≤ 45 ms) before the
/// node and the descriptor close.
pub struct ClockOut {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ClockOut {
    /// Starts `uio2-clock`: the three stock arm writes (two in the thread, one in the opener at
    /// `0x8a7df4`), then per tick a 45 ms poll and, on an interrupt, count read → re-arm →
    /// `0xF8`. One count jump sends one byte. Stock ignores every result; here the first error
    /// of a run is logged to stderr. Owned: a failed poll or read sends nothing and sleeps the
    /// interval, so a broken node cannot spin.
    pub fn spawn(node: UioDevice, mut uart: impl Write + Send + 'static) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("uio2-clock".into())
            .spawn({
                let stop = Arc::clone(&stop);
                move || clock_out(&node, &mut uart, &stop)
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ClockOut {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn clock_out(node: &UioDevice, uart: &mut impl Write, stop: &AtomicBool) {
    let timing_clock = tetrad_midi::message::Realtime::TimingClock.byte();
    let mut failing = false;
    let mut report = |error: Option<String>| {
        if let Some(error) = &error
            && !failing
        {
            let _ = writeln!(io::stderr().lock(), "uio2 clock out: {error}");
        }
        failing = error.is_some();
    };
    for _ in 0..3 {
        report(
            node.write_control(UIO_ARM)
                .err()
                .map(|error| error.to_string()),
        );
    }
    while !stop.load(Ordering::Acquire) {
        let error = match node.poll(UIO2_POLL) {
            Ok(false) => continue,
            Ok(true) => {
                let read = node.read_count();
                let rearm = node.write_control(UIO_ARM).err();
                match read {
                    Ok(_) => {
                        let sent = uart.write_all(&[timing_clock]).err();
                        report(
                            rearm
                                .map(|error| error.to_string())
                                .or(sent.map(|error| error.to_string())),
                        );
                        continue;
                    }
                    Err(error) => error.to_string(),
                }
            }
            Err(error) => error.to_string(),
        };
        report(Some(error));
        std::thread::sleep(UIO2_POLL);
    }
}

/// `wpa_supplicant` links as stock opens them (ZenUI `0x0088f974`): a control connection for
/// requests (`+0x90`) and a monitor connection that sends `ATTACH` (`0x000ebd60`) and receives
/// the `CTRL-EVENT-*` lines (`+0x94`, read by `0x0088f8f4`); the supplicant sends events only
/// to attached clients. Each is one datagram socket at the stock local path
/// `/tmp/wpa_ctrl_<pid>-<n>` (`0x000eb8b0`), opened on first use. A failed request drops the
/// control connection; a receive error drops the monitor. A failed monitor open is retried
/// after `MONITOR_RETRY` (5 s). Oversized events are dropped.
pub struct LinuxSupplicant {
    dir: PathBuf,
    remote: PathBuf,
    control: Option<Transport>,
    monitor: Option<Transport>,
    /// No monitor open before this time.
    monitor_retry: Option<Instant>,
    connections: u32,
    events: Vec<String>,
}

impl Default for LinuxSupplicant {
    fn default() -> Self {
        Self::new(PathBuf::from("/tmp"), PathBuf::from(CONTROL_SOCKET))
    }
}

/// Owned limits: stock reads replies into 1023 bytes and truncates; scan results need more.
const SUPPLICANT_BUDGET: Budget = Budget {
    reply_capacity: 16 * 1024,
    event_capacity: 1024,
    request_timeout: Duration::from_secs(10),
    max_events_per_request: 64,
};
/// Owned: the monitor runs one request, `ATTACH`, from the event loop; a short wait bounds the
/// stall when the supplicant hangs.
const MONITOR_BUDGET: Budget = Budget {
    request_timeout: Duration::from_secs(1),
    ..SUPPLICANT_BUDGET
};
/// Owned: wait between monitor open attempts (before Wi-Fi bring-up the socket is missing).
const MONITOR_RETRY: Duration = Duration::from_secs(5);

fn event_text(receipt: Receipt) -> Option<String> {
    match receipt {
        Receipt::Complete(payload) => {
            Some(String::from_utf8_lossy(payload.as_bytes()).into_owned())
        }
        Receipt::Oversized { .. } => None,
    }
}

impl SupplicantLink for LinuxSupplicant {
    fn request(&mut self, command: &str) -> Result<String, LinkError> {
        let transport = match self.control.take() {
            Some(transport) => transport,
            None => self
                .open(SUPPLICANT_BUDGET)
                .map_err(|error| LinkError::Transport(error.to_string()))?,
        };
        let transport = self.control.insert(transport);
        match transport.request(command.as_bytes()) {
            Ok(response) => {
                self.events
                    .extend(response.events.into_iter().filter_map(event_text));
                Ok(String::from_utf8_lossy(response.reply.as_bytes()).into_owned())
            }
            Err(error) => {
                if let Some(transport) = self.control.take() {
                    // The local path is in /tmp; a failed unlink there does not block the next path.
                    let _ = transport.close();
                }
                Err(match error {
                    supplicant::Error::Timeout => LinkError::Timeout,
                    other => LinkError::Transport(other.to_string()),
                })
            }
        }
    }
}

impl LinuxSupplicant {
    fn new(dir: PathBuf, remote: PathBuf) -> Self {
        Self {
            dir,
            remote,
            control: None,
            monitor: None,
            monitor_retry: None,
            connections: 0,
            events: Vec::new(),
        }
    }

    fn open(&mut self, budget: Budget) -> Result<Transport, supplicant::Error> {
        let local = supplicant::stock_local_path(&self.dir, self.connections);
        self.connections = self.connections.wrapping_add(1);
        Transport::open(&local, &self.remote, budget)
    }

    /// A monitor connection with `ATTACH` acknowledged; events queued before the
    /// acknowledgement are kept.
    fn open_monitor(&mut self) -> Result<Transport, supplicant::Error> {
        let mut monitor = self.open(MONITOR_BUDGET)?;
        match monitor.attach() {
            Ok(response) => {
                self.events
                    .extend(response.events.into_iter().filter_map(event_text));
                Ok(monitor)
            }
            Err(error) => {
                let _ = monitor.close();
                Err(error)
            }
        }
    }

    /// Moves monitor events into the event list without waiting, opening and attaching the
    /// monitor first when it is missing.
    fn poll_events(&mut self) {
        if self.monitor.is_none() && self.monitor_retry.is_none_or(|at| Instant::now() >= at) {
            match self.open_monitor() {
                Ok(monitor) => {
                    self.monitor = Some(monitor);
                    self.monitor_retry = None;
                }
                Err(_) => self.monitor_retry = Some(Instant::now() + MONITOR_RETRY),
            }
        }
        let Some(monitor) = &mut self.monitor else {
            return;
        };
        for _ in 0..MONITOR_BUDGET.max_events_per_request {
            match monitor.try_receive() {
                Ok(Some(supplicant::Received::Event(receipt))) => {
                    self.events.extend(event_text(receipt));
                }
                // Oversized events and stray replies (no request is outstanding) are dropped.
                Ok(Some(_)) => {}
                Ok(None) => return,
                Err(_) => {
                    if let Some(monitor) = self.monitor.take() {
                        let _ = monitor.close();
                    }
                    return;
                }
            }
        }
    }
}

/// Runs the blocking service jobs of one [`Lane`] on a worker thread, so they never block the
/// event loop (k-services K13). Results come back through [`Platform::poll_input`].
struct ServiceWorker {
    jobs: Sender<ServiceJob>,
    results: Receiver<ServiceNotice>,
    /// Sent and not answered yet, in order (the worker runs jobs one at a time).
    pending: VecDeque<ServiceJob>,
}

impl ServiceWorker {
    /// `make` runs on the new thread and builds the job runner there, with its own backends.
    fn spawn<R>(lane: Lane, make: impl FnOnce() -> R + Send + 'static) -> io::Result<Self>
    where
        R: FnMut(ServiceJob) -> ServiceNotice,
    {
        let (jobs, inbox) = mpsc::channel::<ServiceJob>();
        let (outbox, results) = mpsc::channel();
        std::thread::Builder::new()
            .name(format!("tetrad-svc-{lane:?}").to_ascii_lowercase())
            .spawn(move || {
                let mut run = make();
                for job in inbox {
                    if outbox.send(run(job)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs,
            results,
            pending: VecDeque::new(),
        })
    }

    /// A worker over the real backends.
    fn linux(lane: Lane) -> io::Result<Self> {
        Self::spawn(lane, || {
            let mut backends = LinuxServices::new();
            move |job| services::run_job(&mut backends, job)
        })
    }

    /// Moves finished results to `out`. When the worker died (a panic), every job it never
    /// answered reports [`ServiceJob::lost`] and this returns `false`.
    fn drain(&mut self, out: &mut Vec<Input>) -> bool {
        loop {
            match self.results.try_recv() {
                Ok(notice) => {
                    self.pending.pop_front();
                    out.push(Input::Service(notice));
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => {
                    out.extend(self.pending.drain(..).map(|job| Input::Service(job.lost())));
                    return false;
                }
            }
        }
    }
}

/// One worker per [`Lane`], each started on its lane's first job: an update download or a
/// backup on the bulk lane never delays a lease, a probe or a cloud call (review r14 S-2).
#[derive(Default)]
struct Workers([Option<ServiceWorker>; Lane::ALL.len()]);

impl Workers {
    /// Hands `job` to its lane's worker, started with `start` when the lane has none. The job
    /// comes back when no worker runs.
    fn send(
        &mut self,
        job: ServiceJob,
        start: impl FnOnce(Lane) -> io::Result<ServiceWorker>,
    ) -> Result<(), ServiceJob> {
        let lane = job.lane();
        let slot = &mut self.0[lane as usize];
        if slot.is_none() {
            *slot = start(lane).ok();
        }
        let Some(worker) = slot else {
            return Err(job);
        };
        match worker.jobs.send(job.clone()) {
            Ok(()) => {
                worker.pending.push_back(job);
                Ok(())
            }
            Err(mpsc::SendError(job)) => Err(job),
        }
    }

    /// Moves every lane's results to `out`. A dead worker reports its unanswered jobs and is
    /// dropped; its lane's next job starts a new one.
    fn drain(&mut self, out: &mut Vec<Input>) {
        for slot in &mut self.0 {
            if slot.as_mut().is_some_and(|worker| !worker.drain(out)) {
                *slot = None;
            }
        }
    }
}

/// The extensions worker's curl body scratch, apart from the services worker's.
const EXTENSION_HTTP_SCRATCH: &str = "/tmp/tetrad-extension-http";

pub struct LinuxServices {
    runner: SystemRunner,
    files: SystemFiles,
    net: SystemNet,
    http: CurlHttp<SystemRunner, SystemFiles>,
    supplicant: LinuxSupplicant,
    workers: Workers,
}

impl LinuxServices {
    fn new() -> Self {
        Self {
            runner: SystemRunner::default(),
            files: SystemFiles,
            net: SystemNet,
            http: CurlHttp {
                runner: SystemRunner::default(),
                files: SystemFiles,
                scratch: PathBuf::from(HTTP_SCRATCH),
            },
            supplicant: LinuxSupplicant::default(),
            workers: Workers::default(),
        }
    }
}

impl ServiceBackends for LinuxServices {
    type Runner = SystemRunner;
    type Files = SystemFiles;
    type Net = SystemNet;
    type Http = CurlHttp<SystemRunner, SystemFiles>;
    type Supplicant = LinuxSupplicant;
    fn runner(&mut self) -> &mut SystemRunner {
        &mut self.runner
    }
    fn files(&mut self) -> &mut SystemFiles {
        &mut self.files
    }
    fn net(&mut self) -> &mut SystemNet {
        &mut self.net
    }
    fn http(&mut self) -> &mut Self::Http {
        &mut self.http
    }
    fn supplicant(&mut self) -> &mut LinuxSupplicant {
        &mut self.supplicant
    }

    fn lend(&mut self) -> Option<ServiceIo<'_, Self>> {
        Some(ServiceIo {
            runner: &mut self.runner,
            files: &mut self.files,
            net: &mut self.net,
            http: &mut self.http,
            supplicant: &mut self.supplicant,
        })
    }

    /// Hands the job to its lane's worker; its result arrives through `poll_input`. When no
    /// worker can run, the job runs inline so it is never lost. A dead worker stays until
    /// `poll_input` has reported its unanswered jobs; the lane's next job starts a new one.
    fn run_job(&mut self, job: ServiceJob) -> Option<ServiceNotice> {
        match self.workers.send(job, ServiceWorker::linux) {
            Ok(()) => None,
            Err(job) => Some(services::run_job(self, job)),
        }
    }
}

/// libzenhal opens its nodes before ZenUI picks the board platform, and the platform needs two
/// revision reads (`0x004962ac`). So: every node but the codec and the UART, then the
/// detection (`device::identify` rule), then `modprobe` of the platform's codec module as
/// ZenUI does (`0x87782c`), then the codec node when `has_ess_codec`. The UART is
/// [`MidiPort`]'s.
fn open_hal(
    root: &LinuxRoot,
    product: &Product,
    failures: &mut Vec<String>,
) -> Result<(StockHal<LinuxHal>, HalPlatform), LinuxError> {
    let mut attach = |driver: &mut LinuxHal, node: Node| {
        if let Err(error) = driver.attach(node, None) {
            failures.push(format!("HAL node {node:?}: {error}"));
        }
    };
    let mut driver = LinuxHal::default();
    for node in Node::all().filter(|node| !matches!(node, Node::Codec | Node::MidiUart)) {
        attach(&mut driver, node);
    }
    let mut hal = StockHal::open(driver, product);
    let mut revision = |board, substitute| match hal.read(Query::Revision(board)) {
        Ok(Reading::Word(word)) => word,
        _ => substitute,
    };
    let ess = product.is_qc_with_ess(revision(Board::Zenjack, 4), revision(Board::Zencoder, 3));
    let platform = HalPlatform::detect(
        product,
        ess,
        root.marker(Marker::AtmaEmulation)?,
        root.marker(Marker::EssEmulation)?,
    );
    let codec = load_codec_module(platform.codec_module());
    let mut driver = hal.into_driver();
    if platform.has_ess_codec() {
        attach(&mut driver, Node::Codec);
    }
    failures.extend(codec.err());
    Ok((StockHal::open(driver, product), platform))
}

/// `modprobe <module>`, as ZenUI's `popen("modprobe … 2>&1")`. A module that is already loaded
/// (stock ran first, or a restart) is a success. A failure is a platform notice: the UI runs
/// on, with silent analog inputs.
fn load_codec_module(module: &str) -> Result<(), String> {
    let output = std::process::Command::new("modprobe")
        .arg(module)
        .output()
        .map_err(|error| format!("modprobe {module}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "modprobe {module}: {} {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Keeps an opened node, or records why it did not open.
fn opened<T>(failures: &mut Vec<String>, what: &str, result: io::Result<T>) -> Option<T> {
    result
        .map_err(|error| failures.push(format!("{what}: {error}")))
        .ok()
}

pub struct LinuxPlatform {
    root: LinuxRoot,
    hal_platform: HalPlatform,
    hal: StockHal<LinuxHal>,
    settings: LinuxSettings,
    splash: PosixSplashQueue,
    power: LinuxPower,
    services: LinuxServices,
    clock: SystemClock,
    /// Only when `uses_midi_uart` (`0x49c14c`; the mini routes MIDI through the DSP).
    midi: Option<MidiPort>,
    touch: Option<Touch>,
    framebuffer: Option<Framebuffer>,
    display_panel: Option<DisplayPanel>,
    /// The uio2 DIN clock-out thread, started by [`Platform::open_soc2`].
    clock_out: Option<ClockOut>,
    /// `poll_input` opens and attaches the supplicant monitor (off for bring-up without
    /// `--services`).
    supplicant_monitor: bool,
    /// Extension runner processes (`tetrad extension-runner`); `None` until
    /// [`Self::enable_extensions`].
    extensions: Option<crate::device::extensions::LocalBackend>,
    /// The Remote HTTP server; `None` until [`Self::enable_remote`].
    remote: Option<LinuxRemote>,
    /// The BLE host; `None` until [`Self::enable_bluetooth`].
    bluetooth: Option<LinuxBluetooth>,
}

impl LinuxPlatform {
    /// Bring-up without `--services` (review B2): the supplicant monitor is never opened, so
    /// no `ATTACH` is sent and no Wi-Fi event reaches the device.
    pub fn skip_supplicant_monitor(&mut self) {
        self.supplicant_monitor = false;
    }

    /// Runs extensions as child processes of this binary. Only the `run` program does this;
    /// bring-up never starts extension code.
    pub fn enable_extensions(&mut self) -> Result<(), LinuxError> {
        let program = std::env::current_exe()?;
        let root = self
            .root
            .path("/media/p4")
            .join(crate::device::extension_jobs::STORE_DIR);
        let fetch = crate::device::extension_jobs::HttpFetch(CurlHttp {
            runner: SystemRunner::default(),
            files: SystemFiles,
            scratch: PathBuf::from(EXTENSION_HTTP_SCRATCH),
        });
        self.extensions = Some(
            crate::device::extensions::LocalBackend::processes(program).with_worker(root, fetch),
        );
        Ok(())
    }

    /// Lets the Remote page run (its port opens when the user turns Remote on). Only the `run`
    /// program does this; bring-up never opens a listening port.
    pub fn enable_remote(&mut self) {
        self.remote = Some(LinuxRemote::default());
    }

    /// Lets Bluetooth run (the controller starts when the user turns Bluetooth on). Only the
    /// `run` program does this; bring-up never starts the radio.
    pub fn enable_bluetooth(&mut self) {
        self.bluetooth = Some(LinuxBluetooth::default());
    }

    /// Explicit hardware opt-in. Sets the comm name `ZenUI`, then opens the HAL, MIDI UART,
    /// touch, framebuffer, splash queue and power programs. Only the rename, `/proc/cmdline` and
    /// the emulation markers can fail it; a node that does not open is logged and its feature
    /// answers [`LinuxError::Unavailable`].
    pub fn open() -> Result<Self, LinuxError> {
        let root = LinuxRoot::new("/");
        root.take_stock_comm()?;
        let clock = SystemClock::new();
        let mut failures = Vec::new();
        let product = Product::from_cmdline(&fs::read_to_string(root.path(CMDLINE))?);
        let (hal, hal_platform) = open_hal(&root, &product, &mut failures)?;
        let midi = if hal_platform.uses_midi_uart() {
            opened(&mut failures, "MIDI UART", MidiPort::open(clock))
        } else {
            None
        };
        let touch = Touch::open(&root, &mut failures);
        let touch = opened(&mut failures, "touch", touch);
        let framebuffer = opened(
            &mut failures,
            "framebuffer",
            Framebuffer::open(Path::new(paths::FRAMEBUFFER)),
        );
        let power = match opened(
            &mut failures,
            "power soundcard node",
            IoctlNode::open(Node::Soundcard),
        ) {
            Some(node) => LinuxPower::with_soundcard(PowerPrograms::STOCK, node),
            None => LinuxPower::new(PowerPrograms::STOCK),
        };
        let mut platform = Self {
            settings: root.settings(),
            display_panel: root.display_panel(),
            root,
            hal_platform,
            hal,
            splash: PosixSplashQueue::stock(),
            power,
            services: LinuxServices::new(),
            clock,
            midi,
            touch,
            framebuffer,
            clock_out: None,
            supplicant_monitor: true,
            extensions: None,
            remote: None,
            bluetooth: None,
        };
        for detail in failures {
            platform.note(detail);
        }
        Ok(platform)
    }

    fn note(&mut self, detail: String) {
        let notice = Notice {
            at: self.clock.now(),
            subsystem: Subsystem::Platform,
            detail,
        };
        self.log(&notice);
    }
}

impl Platform for LinuxPlatform {
    type Error = LinuxError;
    type Clock = SystemClock;
    type BootBus = BootHardware;
    type Window = PhysicalWindow;
    type RingDrain = StockThread;
    type Hal = StockHal<LinuxHal>;
    type Settings = LinuxSettings;
    type Splash = PosixSplashQueue;
    type Power = LinuxPower;
    type Services = LinuxServices;

    fn clock(&self) -> SystemClock {
        self.clock
    }
    fn kernel_cmdline(&mut self) -> Result<String, LinuxError> {
        Ok(fs::read_to_string(self.root.path(CMDLINE))?)
    }
    fn marker(&mut self, marker: Marker) -> Result<bool, LinuxError> {
        self.root.marker(marker)
    }
    fn set_marker(&mut self, marker: Marker) -> Result<(), LinuxError> {
        self.root.set_marker(marker)
    }
    fn boot_count(&mut self) -> Result<u32, LinuxError> {
        Ok(self.root.boot_count())
    }
    fn splash(&mut self) -> &mut PosixSplashQueue {
        &mut self.splash
    }
    fn power(&mut self) -> &mut LinuxPower {
        &mut self.power
    }
    fn settings(&mut self) -> &mut LinuxSettings {
        &mut self.settings
    }
    fn storage(&self) -> StorageRoots {
        self.root.storage()
    }
    fn hal(&mut self) -> &mut StockHal<LinuxHal> {
        &mut self.hal
    }
    fn services(&mut self) -> &mut LinuxServices {
        &mut self.services
    }
    fn firmware(&mut self, variant: Variant) -> Result<SharcFirmware, LinuxError> {
        self.root.firmware(variant)
    }
    fn boot_bus(&mut self) -> Result<BootHardware, LinuxError> {
        self.root.boot_bus()
    }
    fn map_soc2_buffer(
        &mut self,
        bus: &mut BootHardware,
        buffer: PhysAddr,
        bytes: u32,
    ) -> Result<(), LinuxError> {
        Ok(bus.map_soc2_buffer(buffer, bytes)?)
    }
    /// `0x8a7bbc`: the SOC2 flag `DAT_015630b0`. The mini reports it set and never opens
    /// `/dev/uio2`. A full-size QC returns GPIO 97 (a failed read is low, as stock) and, when
    /// high, starts the DIN clock out ([`ClockOut`]) on the MIDI UART. A failed uio2 open is
    /// logged and does not change the flag; without a UART nothing can be sent.
    fn open_soc2(&mut self) -> Result<bool, LinuxError> {
        if !self.hal_platform.is_qc() {
            return Ok(true);
        }
        let high = self.root.uio2_gate().unwrap_or_else(|error| {
            self.note(format!("GPIO {UIO2_GATE_GPIO} read as low: {error}"));
            false
        });
        // A second open stops the earlier thread first.
        self.clock_out = None;
        if high && let Some(midi) = &self.midi {
            let started = UioDevice::open(&self.root.path(paths::UIO[2])).and_then(|node| {
                let uart = File::from(midi.uart.as_fd().try_clone_to_owned()?);
                ClockOut::spawn(node, uart)
            });
            match started {
                Ok(clock_out) => self.clock_out = Some(clock_out),
                Err(error) => self.note(format!("uio2 clock out not started: {error}")),
            }
        }
        Ok(high)
    }
    fn dsp_windows(&mut self) -> Result<DspWindows<PhysicalWindow>, LinuxError> {
        self.root.dsp_windows()
    }
    fn dsp_file(&mut self, file: DspFile<'_>) -> Result<Vec<u8>, LinuxError> {
        self.root.dsp_file(file)
    }
    /// Polls every source; one failing source does not starve the others. The first error is
    /// returned after the pass.
    fn poll_input(&mut self, out: &mut Vec<Input>) -> Result<(), LinuxError> {
        let mut first = None;
        if let Some(midi) = &self.midi {
            while let Ok(read) = midi.received.try_recv() {
                match read {
                    Ok((bytes, at)) => {
                        out.extend(bytes.into_iter().map(|byte| Input::MidiByte { byte, at }));
                    }
                    Err(error) => {
                        first.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(touch) = &mut self.touch
            && let Err(error) = touch.poll(out)
        {
            first.get_or_insert(error);
        }
        self.services.workers.drain(out);
        if let Some(extensions) = &mut self.extensions {
            extensions.drain(out);
        }
        if let Some(server) = self
            .remote
            .as_mut()
            .and_then(|remote| remote.server.as_mut())
        {
            let mut calls = Vec::new();
            server.drain(&mut calls);
            out.extend(calls.into_iter().map(Input::Remote));
        }
        if let Some(host) = (self.bluetooth.as_mut()).and_then(|b| b.host.as_mut()) {
            out.extend(host.events().into_iter().map(Input::Bluetooth));
        }
        if self.supplicant_monitor {
            self.services.supplicant.poll_events();
        }
        out.extend(
            self.services
                .supplicant
                .events
                .drain(..)
                .map(|line| Input::Service(ServiceNotice::Supplicant(line))),
        );
        first.map_or(Ok(()), |error| Err(error.into()))
    }
    fn midi_write(&mut self, message: &[u8]) -> Result<(), LinuxError> {
        let midi = self
            .midi
            .as_mut()
            .ok_or(LinuxError::Unavailable("MIDI UART"))?;
        if midi.uart.write(message)? != message.len() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "short MIDI write").into());
        }
        Ok(())
    }
    fn present(&mut self, rgb: &[u8]) -> Result<(), LinuxError> {
        self.present_rows(rgb, 0..tetrad_ui::HEIGHT)
    }
    fn present_rows(&mut self, rgb: &[u8], rows: std::ops::Range<u32>) -> Result<(), LinuxError> {
        let framebuffer = self
            .framebuffer
            .as_mut()
            .ok_or(LinuxError::Unavailable("framebuffer"))?;
        Ok(framebuffer.present_rgb8_rows(tetrad_ui::WIDTH, tetrad_ui::HEIGHT, rgb, rows)?)
    }
    /// One stderr line, which stock's `tee` keeps in the boot log. With a set clock it starts
    /// with the time of day, so a report's time finds its lines.
    fn log(&mut self, notice: &Notice) {
        let when = super::support::set_clock(self.wall_clock())
            .map_or(String::new(), |seconds| super::support::utc(seconds) + " ");
        let _ = writeln!(
            io::stderr().lock(),
            "{when}[{:?}] {:?}: {}",
            notice.at,
            notice.subsystem,
            notice.detail
        );
    }
    /// `/dev/urandom`, as glibc's `std::random_device` reads it.
    fn random_u32(&mut self) -> Option<u32> {
        let mut bytes = [0; 4];
        io::Read::read_exact(&mut File::open("/dev/urandom").ok()?, &mut bytes).ok()?;
        Some(u32::from_ne_bytes(bytes))
    }
    fn capture_storage(&mut self) -> Option<Box<dyn tetrad_capture::session::CaptureStorage>> {
        Some(Box::new(tetrad_capture::session::FsStorage))
    }
    fn sharc_memory(&mut self) -> Option<Box<dyn tetrad_capture::session::SharcMemory>> {
        Some(Box::new(self.root.sharc_memory()))
    }
    fn media_cipher(&mut self) -> Option<DeviceCipher> {
        match self.root.media_cipher() {
            Ok(cipher) => Some(cipher),
            Err(error) => {
                self.log(&Notice {
                    at: self.clock().now(),
                    subsystem: Subsystem::Dsp,
                    detail: format!("media cipher: {error}"),
                });
                None
            }
        }
    }
    fn wall_clock(&mut self) -> Option<SystemTime> {
        Some(SystemTime::now())
    }
    fn display_panel(&mut self) -> Option<DisplayPanel> {
        self.display_panel
    }
    fn splash_running(&mut self) -> bool {
        let Ok(entries) = fs::read_dir("/proc") else {
            return false;
        };
        entries.flatten().any(|entry| {
            fs::read(entry.path().join("comm")).is_ok_and(|comm| comm == b"zensplash\n")
        })
    }
    fn disk_space(&mut self) -> Option<tetrad_sys::fs::FsSpace> {
        tetrad_sys::fs::statvfs(&self.root.path("/media/p4")).ok()
    }
    fn start_app(
        &mut self,
        launch: &crate::device::app::AppLaunch,
    ) -> Result<Box<dyn crate::device::app::AppProcess>, String> {
        let program = launch.program.display();
        fs::create_dir_all(&launch.dir).map_err(|e| format!("{}: {e}", launch.dir.display()))?;
        let mut command = std::process::Command::new(&launch.program);
        command
            .args(&launch.args)
            .current_dir(&launch.dir)
            .stdin(std::process::Stdio::null());
        let child =
            tetrad_sys::process::spawn_app(&mut command).map_err(|e| format!("{program}: {e}"))?;
        Ok(Box::new(LinuxApp(child)))
    }
    fn extensions(&mut self) -> Option<&mut dyn crate::device::extensions::ExtensionBackend> {
        self.extensions
            .as_mut()
            .map(|backend| backend as &mut dyn crate::device::extensions::ExtensionBackend)
    }

    fn remote(&mut self) -> Option<&mut dyn crate::device::remote::RemoteBackend> {
        self.remote
            .as_mut()
            .map(|backend| backend as &mut dyn crate::device::remote::RemoteBackend)
    }

    fn bluetooth(&mut self) -> Option<&mut dyn crate::device::bluetooth::BluetoothBackend> {
        self.bluetooth
            .as_mut()
            .map(|backend| backend as &mut dyn crate::device::bluetooth::BluetoothBackend)
    }

    fn forget_display(&mut self) {
        if let Some(framebuffer) = &mut self.framebuffer {
            framebuffer.forget_shown();
        }
    }
}

/// A running extension program. Its output goes to the device log, as the runners' does.
struct LinuxApp(std::process::Child);

impl crate::device::app::AppProcess for LinuxApp {
    fn exited(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(status.to_string()),
            Err(error) => Some(error.to_string()),
        }
    }
    fn stop(&mut self) {
        let _ = self.0.kill();
    }
}

impl Drop for LinuxApp {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(event_type: u16, code: u16, value: i32) -> InputRecord {
        InputRecord {
            timestamp: tetrad_hal::input::ArmTimestamp {
                seconds: 0,
                microseconds: 0,
            },
            event_type,
            code,
            value,
        }
    }

    /// Stock `/etc/ts.conf` order: `invert` runs before `linear`, then the result is clamped to
    /// the 800×480 display. `pthres pmin=1` decides `pressed`.
    #[test]
    fn touch_inverts_then_calibrates_and_reports_release() {
        // Invented calibration: x' = (10 + 2x) / 2, y' = (-4 + 2y) / 2. Ignored trailing fields.
        let calibration = Calibration::parse("2 0 10 0 2 -4 2 800 480 0").unwrap();
        let mut touch = TouchDecoder::new(calibration);
        assert_eq!(touch.push(&record(EV_SYN, SYN_REPORT, 0)), None);
        for (event_type, code, value) in [(EV_ABS, ABS_X, 700), (EV_ABS, ABS_Y, 400)] {
            assert_eq!(touch.push(&record(event_type, code, value)), None);
        }
        assert_eq!(touch.push(&record(EV_KEY, BTN_TOUCH, 1)), None);
        let pressed = TouchSample {
            x: 105,
            y: 78,
            pressed: true,
        };
        assert_eq!(touch.push(&record(EV_SYN, SYN_REPORT, 0)), Some(pressed));
        assert_eq!(touch.push(&record(EV_SYN, SYN_REPORT, 0)), None);

        // Multi-touch release and clamping: raw 900 inverts to -100, then clamps to 0.
        touch.push(&record(EV_ABS, ABS_MT_POSITION_X, 900));
        touch.push(&record(EV_ABS, ABS_MT_TRACKING_ID, -1));
        let released = touch.push(&record(EV_SYN, SYN_REPORT, 0)).unwrap();
        assert_eq!((released.x, released.pressed), (0, false));

        assert_eq!(Calibration::parse("1 0 0 0 1 0 0"), None, "zero divisor");
        assert_eq!(Calibration::parse("1 0 0"), None, "too short");
    }

    /// The ILI2130 (ili213x driver) sends only `ABS_MT_POSITION_X/Y`, `SYN_MT_REPORT` and
    /// `SYN_REPORT`: a position is a press, an empty frame is the release.
    #[test]
    fn type_a_multitouch_without_pressure_presses_and_releases() {
        let mut touch = TouchDecoder::new(Calibration::default());
        let frame = |touch: &mut TouchDecoder, position: Option<(i32, i32)>| {
            if let Some((x, y)) = position {
                touch.push(&record(EV_ABS, ABS_MT_POSITION_X, x));
                touch.push(&record(EV_ABS, ABS_MT_POSITION_Y, y));
            }
            assert_eq!(touch.push(&record(EV_SYN, SYN_MT_REPORT, 0)), None);
            touch.push(&record(EV_SYN, SYN_REPORT, 0))
        };
        let down = frame(&mut touch, Some((700, 400))).unwrap();
        assert_eq!((down.x, down.y, down.pressed), (100, 80, true));
        let moved = frame(&mut touch, Some((690, 400))).unwrap();
        assert_eq!((moved.x, moved.pressed), (110, true));
        // An evdev overflow mid-drag: the partial frame after `SYN_DROPPED` is no release.
        touch.push(&record(EV_SYN, SYN_DROPPED, 0));
        assert_eq!(frame(&mut touch, None), None, "partial frame");
        let up = frame(&mut touch, None).unwrap();
        assert!(!up.pressed);
        assert_eq!(frame(&mut touch, None), None, "no repeated release");
    }

    /// A type A screen with `BTN_TOUCH` and hover: the input core drops the second
    /// `BTN_TOUCH 0`, so a hover move carries only a position and must stay released.
    #[test]
    fn type_a_multitouch_with_btn_touch_ignores_hover_moves() {
        let mut touch = TouchDecoder::new(Calibration::default());
        let frame = |touch: &mut TouchDecoder, btn_touch: Option<i32>, x: i32| {
            if let Some(value) = btn_touch {
                touch.push(&record(EV_KEY, BTN_TOUCH, value));
            }
            touch.push(&record(EV_ABS, ABS_MT_POSITION_X, x));
            touch.push(&record(EV_ABS, ABS_MT_POSITION_Y, 400));
            touch.push(&record(EV_SYN, SYN_MT_REPORT, 0));
            touch.push(&record(EV_SYN, SYN_REPORT, 0))
        };
        assert!(frame(&mut touch, Some(1), 700).unwrap().pressed);
        // An evdev overflow before the lift: the partial frame's `BTN_TOUCH 0` still releases.
        touch.push(&record(EV_SYN, SYN_DROPPED, 0));
        assert!(!frame(&mut touch, Some(0), 700).unwrap().pressed);
        assert!(!frame(&mut touch, None, 690).unwrap().pressed, "hover move");
    }

    /// Type A with tracking ids sends no id -1: the release is a frame without the contact.
    #[test]
    fn type_a_multitouch_with_tracking_id_releases() {
        let mut touch = TouchDecoder::new(Calibration::default());
        touch.push(&record(EV_ABS, ABS_MT_TRACKING_ID, 3));
        touch.push(&record(EV_ABS, ABS_MT_POSITION_X, 700));
        touch.push(&record(EV_ABS, ABS_MT_POSITION_Y, 400));
        touch.push(&record(EV_SYN, SYN_MT_REPORT, 0));
        assert!(touch.push(&record(EV_SYN, SYN_REPORT, 0)).unwrap().pressed);
        touch.push(&record(EV_SYN, SYN_MT_REPORT, 0));
        assert!(!touch.push(&record(EV_SYN, SYN_REPORT, 0)).unwrap().pressed);
    }

    /// Stock `0x0088f974`: events come on a second, attached connection (the supplicant sends
    /// `CTRL-EVENT-*` only to attached clients); requests use the control connection.
    #[test]
    fn supplicant_events_arrive_on_an_attached_monitor() {
        use std::os::unix::net::UnixDatagram;
        const CONNECTED: &str = "<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:01";
        let dir = std::env::temp_dir().join(format!("tetrad-app-wpa-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let remote = dir.join("wlan0");
        let server = UnixDatagram::bind(&remote).unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let peer = std::thread::spawn(move || {
            let mut bytes = [0; 64];
            let (len, monitor) = server.recv_from(&mut bytes).unwrap();
            assert_eq!(&bytes[..len], b"ATTACH");
            let monitor = monitor.as_pathname().unwrap().to_owned();
            server.send_to(b"OK\n", &monitor).unwrap();
            server.send_to(CONNECTED.as_bytes(), &monitor).unwrap();
            let (len, control) = server.recv_from(&mut bytes).unwrap();
            assert_eq!(&bytes[..len], b"PING");
            let control = control.as_pathname().unwrap();
            assert_ne!(control, monitor);
            server.send_to(b"PONG\n", control).unwrap();
        });

        let mut link = LinuxSupplicant::new(dir.clone(), remote);
        let deadline = Instant::now() + Duration::from_secs(5);
        while link.events.is_empty() && Instant::now() < deadline {
            link.poll_events();
        }
        assert_eq!(link.events, [CONNECTED]);
        assert_eq!(link.request("PING").unwrap(), "PONG\n");
        peer.join().unwrap();
        drop(link);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Review r14 S-2: a bulk job that runs for minutes does not hold up the other lanes. The
    /// bulk worker here blocks until released; a clock job sent after it answers first.
    #[test]
    fn a_long_bulk_job_does_not_block_other_lanes() {
        let (release, held) = mpsc::channel::<()>();
        let held = std::sync::Mutex::new(Some(held));
        let mut workers = Workers::default();
        let start = |lane: Lane| match lane {
            Lane::Bulk => {
                let held = held.lock().unwrap().take().unwrap();
                ServiceWorker::spawn(lane, move || {
                    move |job: ServiceJob| {
                        let _ = held.recv();
                        job.lost()
                    }
                })
            }
            _ => ServiceWorker::spawn(lane, || |job: ServiceJob| job.lost()),
        };
        workers.send(ServiceJob::Install, start).unwrap();
        let Some(crate::device::Effect::Service(probe)) =
            services::Services::new().set_clock_if_unset(1)
        else {
            panic!("a clock job");
        };
        assert_eq!(probe.lane(), Lane::System);
        workers.send(probe, start).unwrap();

        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while out.is_empty() && Instant::now() < deadline {
            workers.drain(&mut out);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            out.as_slice(),
            [Input::Service(ServiceNotice::Internet(_))]
        ));
        release.send(()).unwrap();
        while out.len() < 2 && Instant::now() < deadline {
            workers.drain(&mut out);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            out.as_slice(),
            [_, Input::Service(ServiceNotice::Install(Err(_)))]
        ));
    }

    /// Review w2c-io R11: a worker that dies (a panic) still answers every job it took, so the
    /// updater never stays in `Updating`.
    #[test]
    fn dead_worker_reports_its_unanswered_jobs() {
        let (jobs, _inbox) = mpsc::channel();
        let (_, results) = mpsc::channel();
        let mut worker = ServiceWorker {
            jobs,
            results,
            pending: VecDeque::from([ServiceJob::Install]),
        };
        let mut out = Vec::new();
        assert!(!worker.drain(&mut out));
        assert_eq!(
            out,
            [Input::Service(ServiceNotice::Install(Err(
                services::JobError::WorkerStopped
            )))]
        );
    }
}

/// The Remote server on all interfaces, port [`tetrad_remote::PORT`]; `server` is `Some`
/// while Remote is on.
#[derive(Default)]
pub struct LinuxRemote {
    server: Option<tetrad_remote::Server>,
}

/// The BLE host thread of tetrad-sys (`tetrad_sys::ble`).
#[derive(Default)]
pub struct LinuxBluetooth {
    host: Option<tetrad_sys::ble::Bluetooth>,
}

impl crate::device::bluetooth::BluetoothBackend for LinuxBluetooth {
    fn start(
        &mut self,
        config: tetrad_ble::Config,
        bonds: Vec<tetrad_ble::Bond>,
    ) -> Result<(), String> {
        // A new start ends the old thread first: one host owns the UART.
        self.host = None;
        let host = tetrad_sys::ble::Bluetooth::start(config, bonds)
            .map_err(|error| format!("Bluetooth thread: {error}"))?;
        self.host = Some(host);
        Ok(())
    }

    fn stop(&mut self) {
        self.host = None;
    }

    fn send(&mut self, command: tetrad_ble::Command) {
        if let Some(host) = &mut self.host {
            host.send(command);
        }
    }
}

impl crate::device::remote::RemoteBackend for LinuxRemote {
    fn start(&mut self) -> Result<(), String> {
        use std::net::{Ipv4Addr, SocketAddr};
        if self.server.is_some() {
            return Ok(());
        }
        let port = tetrad_remote::PORT;
        let server = tetrad_remote::Server::start(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
            .map_err(|error| match error.kind() {
                io::ErrorKind::AddrInUse => format!("Port {port} is in use"),
                _ => format!("Port {port}: {error}"),
            })?;
        self.server = Some(server);
        Ok(())
    }

    fn stop(&mut self) {
        self.server = None;
    }

    fn reply(&mut self, id: u64, reply: tetrad_remote::Reply) {
        if let Some(server) = &mut self.server {
            server.reply(id, reply);
        }
    }

    fn new_pin(&mut self) -> Result<String, String> {
        use std::io::Read;
        let mut bytes = [0; 4];
        File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|error| format!("no random PIN: {error}"))?;
        Ok(format!("{:04}", u32::from_le_bytes(bytes) % 10_000))
    }

    #[cfg(feature = "dev")]
    fn nonce(&mut self) -> Result<String, String> {
        use std::io::Read;
        let mut bytes = [0; 16];
        File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut bytes))
            .map_err(|error| format!("no random challenge: {error}"))?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
}
