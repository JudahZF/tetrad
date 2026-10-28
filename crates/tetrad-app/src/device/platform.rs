//! Every injected backend of the on-device program. `linux.rs` implements it over tetrad-sys; tests
//! implement it over the crates' own fakes. Nothing in `device` touches a file, node or clock
//! except through this trait.
//!
//! Out of scope by stock design: the drvinit boot (`tetrad_watchdog::boot::BootBackend`, run by
//! init before ZenUI) and the hardware watchdog feeder (`HardwareWatchdog`, BusyBox `watchdog -t
//! 5`). ZenUI owns neither, so the device program does not either (o-lifecycle §3, §4).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tetrad_capture::session::{CaptureStorage, SharcMemory};
use tetrad_dsp_host::SharedMemory;
use tetrad_dsp_host::drain::DrainMode;
use tetrad_dsp_host::model::CoreWindows;
use tetrad_dsp_host::session::Clock;
use tetrad_hal::hal::Hal;
use tetrad_hal::platform::DisplayPanel;
use tetrad_loader::PhysAddr;
use tetrad_loader::exec::{CoreControl, PhysicalMemory};
use tetrad_loader::plan::Variant;
use tetrad_settings::SettingsStorage;
use tetrad_storage::media::MediaRoot;
use tetrad_sys::fs::FsSpace;
use tetrad_watchdog::power::PowerBackend;
use tetrad_watchdog::splash::SplashQueue;

use crate::device::services::ServiceNotice;

/// The whole platform. Long-lived backends are borrowed (`hal`, `settings`, `splash`, `power`,
/// `services`); boot-time resources are opened once, in stock order, and owned by the caller.
pub trait Platform {
    type Error: std::error::Error + Send + Sync + 'static;
    /// Device time and every DSP wait. Clones share one time source.
    type Clock: Clock + Clone;
    /// `/dev/mem` windows and `/dev/corectrl` for the SHARC boot (tetrad-sys
    /// `loader::BootHardware`).
    type BootBus: PhysicalMemory + CoreControl;
    /// One mapped DSP window: the ring window or a core-0/1 model window (tetrad-sys `mem::PhysicalWindow`).
    type Window: SharedMemory;
    /// Who drains the DSP receive rings between main-loop polls, as the stock uio workers
    /// (`0x8a4ec4`/`0x8a4d58`) do: `StockThread` on the device, `MainLoop` (polls and send waits
    /// only) where the window cannot leave its thread (`tetrad_dsp_host::drain`).
    type RingDrain: DrainMode<Self::Window>;
    type Hal: Hal;
    type Settings: SettingsStorage;
    type Splash: SplashQueue<Error: std::fmt::Display>;
    /// System power steps. Steps that need application state (`PowerStep::Dsp`,
    /// `MuteHeadphones`, `SurfaceLedsOff`, `Delay`) never reach it; the device runs them.
    type Power: PowerBackend<Error: std::fmt::Display>;
    type Services: ServiceBackends;

    fn clock(&self) -> Self::Clock;

    /// `/proc/cmdline` (libzenhal `getProcCmdlineValue` reads its first line).
    fn kernel_cmdline(&mut self) -> Result<String, Self::Error>;
    fn marker(&mut self, marker: Marker) -> Result<bool, Self::Error>;
    /// Creates a marker file. Only [`Marker::SharcsStarted`] is written by stock ZenUI.
    fn set_marker(&mut self, marker: Marker) -> Result<(), Self::Error>;
    /// `/opt/neuraldsp/boot_count.txt`; drives the NEW-badge expiry in `migrate_on_boot`.
    fn boot_count(&mut self) -> Result<u32, Self::Error>;

    fn splash(&mut self) -> &mut Self::Splash;
    fn power(&mut self) -> &mut Self::Power;
    fn settings(&mut self) -> &mut Self::Settings;
    fn storage(&self) -> StorageRoots;
    fn hal(&mut self) -> &mut Self::Hal;
    fn services(&mut self) -> &mut Self::Services;

    /// Decrypted SHARC kernels, the loadsharc ELF and the SOC2 images. The platform holds the
    /// kernel secret (ZenUI `0x6abcac`, seed + serial) and checks the loadsharc SHA-256 of the
    /// running build (`tetrad_firmware::running`); neither key nor plaintext is ever stored.
    fn firmware(&mut self, variant: Variant) -> Result<SharcFirmware, Self::Error>;
    fn boot_bus(&mut self) -> Result<Self::BootBus, Self::Error>;
    /// Maps the DSP-given SOC2 DDR buffer before the image copy (tetrad-sys `map_soc2_buffer`).
    fn map_soc2_buffer(
        &mut self,
        bus: &mut Self::BootBus,
        buffer: PhysAddr,
        bytes: u32,
    ) -> Result<(), Self::Error>;
    /// After `0x14` and the 1 s settle: GPIO 97 probe and SOC2 ring open (`0x89c0bc`,
    /// `0x8a7bbc`). Returns whether SOC2 comms opened.
    fn open_soc2(&mut self) -> Result<bool, Self::Error>;
    /// Ring window `0x20088000`/`0x1791c` and the core 0/1 model windows. Called once, after
    /// the SOC1 boot.
    fn dsp_windows(&mut self) -> Result<DspWindows<Self::Window>, Self::Error>;
    /// Bring-up drill (`--drill-core0`): `true` once, and the DSP boot then reports core 0
    /// silent after the handshake.
    fn drill_core0_silence(&mut self) -> bool {
        false
    }
    /// One stock DSP data file. Encrypted files stay encrypted; the device decrypts them with
    /// keys the DSP sends after the handshake.
    fn dsp_file(&mut self, file: DspFile<'_>) -> Result<Vec<u8>, Self::Error>;

    /// Moves ready platform input into `out`. Never blocks.
    fn poll_input(&mut self, out: &mut Vec<Input>) -> Result<(), Self::Error>;
    /// One DIN MIDI message, one `write` (`CMIDI::MessageSyncPost` `0x1db60`).
    fn midi_write(&mut self, message: &[u8]) -> Result<(), Self::Error>;
    /// One 800×480 frame, packed RGB8 rows (the `tetrad_ui::Frame::rgb` layout).
    fn present(&mut self, rgb: &[u8]) -> Result<(), Self::Error>;
    /// [`Self::present`] when only `rows` (top first) can differ from the last frame this
    /// platform presented. A display may skip the other rows; by default every row goes.
    fn present_rows(&mut self, rgb: &[u8], rows: std::ops::Range<u32>) -> Result<(), Self::Error> {
        let _ = rows;
        self.present(rgb)
    }
    /// Log sink. Must ignore write errors: after `PowerStep::StopLogPipelines` stderr is a
    /// pipe without a reader (review-b2-linux S1).
    fn log(&mut self, notice: &crate::device::Notice);

    /// One value of the hardware random source (stock `std::random_device`), for the Neural
    /// Capture trainer seed (`TrainerSeed::from_random_device`). `None`: no source, no V1
    /// capture.
    fn random_u32(&mut self) -> Option<u32> {
        None
    }

    /// Storage for the Neural Capture stimulus and capture files (`CapturePaths::stock`).
    /// `None`: no V1 capture. Device code never opens these files itself.
    fn capture_storage(&mut self) -> Option<Box<dyn CaptureStorage>> {
        None
    }

    /// SHARC memory for Neural Capture V1 transfers at DSP buffer addresses: file → SHARC
    /// (VA 0x333490) and SHARC → file (0x337758) each open `/dev/mem` `O_RDWR|O_SYNC` and map
    /// the page-aligned range around the buffer. `None`: V1 transfers fail as `Unsupported`.
    fn sharc_memory(&mut self) -> Option<Box<dyn SharcMemory>> {
        None
    }

    /// The device cipher for factory IR images and user IR metadata (ZenUI seed + serial,
    /// `tetrad_media::factory::DeviceCipher`). `None`: cabs and IR Loaders load the default IR.
    fn media_cipher(&mut self) -> Option<tetrad_media::factory::DeviceCipher> {
        None
    }

    /// UTC wall time for preset save dates (stock `QDateTime::currentDateTimeUtc`). `None`:
    /// saves are refused.
    fn wall_clock(&mut self) -> Option<SystemTime> {
        None
    }

    /// The panel named in `/tmp/qc_display` (`CZendisplay::Open` VA 0x8a38), which sets the
    /// backlight divisor ([`DisplayPanel::backlight`]). `None` is stock `displayinfo_t` 0:
    /// the screen brightness is sent undivided.
    fn display_panel(&mut self) -> Option<DisplayPanel> {
        None
    }

    /// Whether the stock boot splash (`/opt/neuraldsp/splashScreen/zensplash`) still runs.
    fn splash_running(&mut self) -> bool {
        false
    }

    /// The size and free space of the user storage `/media/p4` (stock `statvfs64`,
    /// `0x210f6c`), for GeneralSettings 17, 18 and 36. `None`: no reading; Cortex Control then
    /// reads 0, as stock after a failed `statvfs64`.
    fn disk_space(&mut self) -> Option<FsSpace> {
        None
    }

    /// Another program drew on the display: the next [`Self::present`] writes every row.
    fn forget_display(&mut self) {}

    /// Starts an extension program ([`crate::device::app`]). Where programs do not run, it
    /// says so.
    fn start_app(
        &mut self,
        launch: &crate::device::app::AppLaunch,
    ) -> Result<Box<dyn crate::device::app::AppProcess>, String> {
        let _ = launch;
        Err("programs do not run here".into())
    }

    /// Extension runner processes; `None` where extensions do not run.
    fn extensions(&mut self) -> Option<&mut dyn crate::device::extensions::ExtensionBackend> {
        None
    }

    /// The Remote HTTP server; `None` where Remote does not run.
    fn remote(&mut self) -> Option<&mut dyn crate::device::remote::RemoteBackend> {
        None
    }
}

/// tetrad-services backends, borrowed together by `services.rs`.
pub trait ServiceBackends {
    type Runner: tetrad_services::exec::CommandRunner;
    type Files: tetrad_services::exec::Files;
    type Net: tetrad_services::exec::NetIo;
    type Http: tetrad_services::http::HttpClient;
    type Supplicant: tetrad_services::wifi::SupplicantLink;
    fn runner(&mut self) -> &mut Self::Runner;
    fn files(&mut self) -> &mut Self::Files;
    fn net(&mut self) -> &mut Self::Net;
    fn http(&mut self) -> &mut Self::Http;
    fn supplicant(&mut self) -> &mut Self::Supplicant;

    /// Every backend at once, for calls that need two (a supplicant event can run a command).
    /// The default lends nothing; the services bridge then reports `Unsupported`.
    fn lend(&mut self) -> Option<ServiceIo<'_, Self>> {
        None
    }

    /// Runs blocking service work. The default runs it inline and returns the result; a
    /// platform with a worker returns `None` and delivers the result through
    /// [`Platform::poll_input`] as [`Input::Service`].
    fn run_job(&mut self, job: crate::device::services::ServiceJob) -> Option<ServiceNotice>
    where
        Self: Sized,
    {
        Some(crate::device::services::run_job(self, job))
    }
}

/// All [`ServiceBackends`] borrowed together.
pub struct ServiceIo<'a, B: ServiceBackends + ?Sized> {
    pub runner: &'a mut B::Runner,
    pub files: &'a mut B::Files,
    pub net: &'a mut B::Net,
    pub http: &'a mut B::Http,
    pub supplicant: &'a mut B::Supplicant,
}

/// Marker files ZenUI tests at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `/tmp/sharcs_started`: a later boot is a restart (quiesce first, `0x8a8420`).
    SharcsStarted,
    /// `/opt/neuraldsp/ccesboot`: debugger start without a load. Not supported.
    CcesBoot,
    /// `/opt/neuraldsp/noslaveboot`: skip the SOC2 boot (`0x89c848`).
    NoSlaveBoot,
    /// `/opt/neuraldsp/factory_image_flag`: try the factory SOC2 pair first.
    FactoryImage,
    /// `/media/p4/atma_emulation` (`tetrad_hal::platform::Platform::ATMA_EMULATION_FILE`).
    AtmaEmulation,
    /// `/media/p4/ess_emulation`.
    EssEmulation,
    /// `/opt/neuraldsp/nopoll`: skip the DSP diagnostics poll (`0x50ea74`, tested each tick).
    NoPoll,
}

/// Host locations of the stock storage.
#[derive(Debug, Clone)]
pub struct StorageRoots {
    /// `/media/p4`.
    pub user: MediaRoot,
    /// `/opt/neuraldsp` (Factory Library, `factory.db`).
    pub factory: MediaRoot,
    /// `/media/p4/zenui.db`.
    pub database: PathBuf,
}

/// Decrypted boot images. The loadsharc ELF is the device's own copy (the booter tables).
#[derive(Debug, Clone)]
pub struct SharcFirmware {
    pub loadsharc: Vec<u8>,
    /// `ZPUKernel_Core1.ldr`, decrypted.
    pub core1: Vec<u8>,
    /// `ZPUKernel_Core2.ldr`, decrypted.
    pub core2: Vec<u8>,
    pub soc2: Soc2Firmware,
}

/// SOC2 images, copied verbatim (`0x89bae0`). Selection: `0x8a5a74`, `0x8a66d8`.
#[derive(Debug, Clone)]
pub struct Soc2Firmware {
    pub normal: Soc2Pair,
    /// Only when [`Marker::FactoryImage`] exists; tried first.
    pub factory: Option<Soc2Pair>,
}

#[derive(Debug, Clone)]
pub struct Soc2Pair {
    pub spl: Vec<u8>,
    pub core0: Vec<u8>,
}

pub struct DspWindows<W> {
    pub messages: W,
    pub cores: CoreWindows<W>,
}

/// Stock DSP data files under `/opt/neuraldsp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DspFile<'a> {
    /// `ModelRepo.xml`, encrypted with the `0x24` key.
    ModelRepo,
    /// `moduleReport.xml`, decrypted by the platform (seed + serial secret).
    ModuleReport,
    /// `blobs/core1/<name>_blob.bin`, encrypted with the `0x25` key.
    Blob(&'a str),
    /// `blobs/core{2,3,4}/diffs.blob` for transport cores 1..=3.
    Diffs(tetrad_dsp_protocol::DspCore),
    /// `/opt/neuraldsp/ZenUI`, the stock image: driver, NOTE and media tables
    /// (`Dsp::load_tables`, pinned by SHA-256).
    ZenUi,
    /// `/opt/neuraldsp/factory_ir_entities.pb` (`tetrad_media::catalog`).
    FactoryIrCatalog,
    /// One factory IR image as the catalog names it (under `impulse_responses`), still
    /// encrypted.
    FactoryIr(&'a str),
}

/// Platform input, stamped with the read time.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// One DIN byte (`CMIDI::_Rx` stamps every byte; i-midi wave-2 contract).
    MidiByte {
        byte: u8,
        at: Duration,
    },
    Touch(TouchSample),
    /// A service worker finished or changed state.
    Service(ServiceNotice),
    /// Extension runner output.
    Extension(crate::device::extensions::ExtensionInput),
    /// A request of the Remote page (`crate::device::remote`).
    Remote(tetrad_remote::Call),
    /// The user or the system asked for a power action.
    Power(crate::device::PowerAction),
}

/// One touch controller sample in display pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchSample {
    pub x: u16,
    pub y: u16,
    pub pressed: bool,
}

/// Drops each touch move that a later move in the same batch replaces: a pressed sample with a
/// pressed sample before and after it. Each touch input costs a UI sync (6–17 ms on the QC), so
/// a drag that queued during a slow pass cost one sync per sample (r53: 64 ms at the first I/O
/// open). Presses, releases and the last move of each contact stay; tetrad-ui keeps no velocity.
pub fn drop_replaced_moves(input: &mut Vec<Input>) {
    let pressed: Vec<bool> = (input.iter())
        .filter_map(|input| match input {
            Input::Touch(sample) => Some(sample.pressed),
            _ => None,
        })
        .collect();
    let mut index = 0;
    input.retain(|input| {
        if !matches!(input, Input::Touch(_)) {
            return true;
        }
        index += 1;
        let replaced = index >= 2 && pressed[index - 2] && pressed[index - 1];
        !(replaced && pressed.get(index) == Some(&true))
    });
}

#[cfg(test)]
mod tests {
    use super::{Input, TouchSample, drop_replaced_moves};

    #[test]
    fn only_moves_between_two_pressed_samples_drop() {
        let touch = |x, pressed| Input::Touch(TouchSample { x, y: 0, pressed });
        let midi = Input::MidiByte {
            byte: 0xf8,
            at: std::time::Duration::ZERO,
        };
        let mut input = vec![
            touch(1, true),
            touch(2, true),
            midi.clone(),
            touch(3, true),
            touch(4, true),
            touch(5, false),
            touch(6, true),
            touch(7, true),
        ];
        drop_replaced_moves(&mut input);
        let expected = vec![
            touch(1, true),
            midi,
            touch(4, true),
            touch(5, false),
            touch(6, true),
            touch(7, true),
        ];
        assert_eq!(input, expected);
    }
}
