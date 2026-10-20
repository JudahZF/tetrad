//! Explicit application commands: the on-device `run` program and offline file tools.
#![forbid(unsafe_code)]

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
#[cfg(target_os = "linux")]
use tetrad_app::device::bringup::{PowerMode, Stage};
use tetrad_app::event_loop::{
    CommandBackend, EventLoop, FrameSink, InputSource, LoopEvent, OfflineBackend, RunStop,
};
use tetrad_app::library_backend::{LibraryBackend, LibraryBackendError, PresetBinding};
use tetrad_app::looper::{Looper, LooperCommand, LooperLimits, MAX_BLOCK_FRAMES, StereoFrame};
use tetrad_app::offline::{FileError, decode_f32, read_limited, write_new};
use tetrad_app::replay::{MAX_TRACE_BYTES, MAX_TRACE_EVENTS, ReplayEvent, TraceError, parse_trace};
use tetrad_app::session::{OfflineSession, SessionError};
use tetrad_app::tuner::{Detection, MAX_FRAME_LEN, ReferencePitch, Tuner, TunerConfig, TunerError};
use tetrad_ui::{Control, InputEvent, SceneIndex, Screen, UiState};

const MAX_PRESET_BYTES: usize = 16 * 1024 * 1024;
const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;
const MAX_BINDINGS_BYTES: usize = 1024 * 1024;

#[derive(Parser)]
#[command(
    name = "tetrad",
    version,
    about = "Owned QC app: the on-device program and offline UI, library and audio tools."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render an owned 800x480 PPM view; a preset supplies metadata only.
    Render {
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        preset: Option<PathBuf>,
        /// Offline view scene, 1 through 8; does not activate DSP.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=8))]
        scene: u8,
        #[arg(long, value_enum, default_value_t = View::Grid)]
        screen: View,
        /// Opt-in conservative grid projection. Owned defaults assume true means bypassed.
        #[arg(long, value_enum, requires = "preset")]
        grid_policy: Option<GridPolicy>,
    },
    /// Replay explicit JSONL UI events into local state and export the final view.
    Replay {
        #[arg(long)]
        preset: PathBuf,
        #[arg(long)]
        trace: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=8))]
        scene: u8,
        #[arg(long, value_enum)]
        grid_policy: Option<GridPolicy>,
        #[arg(long, default_value_t = MAX_TRACE_EVENTS as u32, value_parser = clap::value_parser!(u32).range(1..=MAX_TRACE_EVENTS as i64))]
        max_events: u32,
    },
    /// Replay library selection and local edits; never save or activate DSP.
    LibraryReplay {
        #[arg(long)]
        database: PathBuf,
        /// JSON array of explicit ui_handle, product_id and title bindings.
        #[arg(long)]
        bindings: PathBuf,
        #[arg(long)]
        initial_handle: u64,
        /// Caller-selected opaque type, not a recovered stock enum.
        #[arg(long)]
        preset_type: i64,
        #[arg(long)]
        trace: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=8))]
        scene: u8,
        #[arg(long, value_enum)]
        grid_policy: Option<GridPolicy>,
        #[arg(long, default_value_t = MAX_TRACE_EVENTS as u32, value_parser = clap::value_parser!(u32).range(1..=MAX_TRACE_EVENTS as i64))]
        max_events: u32,
    },
    /// Edit existing scene labels/colors through the dialog; missing entries refuse.
    SceneEdit {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        scene: u8,
        #[arg(long, required_unless_present = "color")]
        label: Option<String>,
        /// Opaque stored color, not an RGB value; zero is omitted by stock confirmation.
        #[arg(long, required_unless_present = "label")]
        color: Option<u32>,
    },
    /// Save scene edits in an existing owned SQLite library, never a stock database.
    LibrarySceneEdit {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        id: String,
        /// Caller-selected opaque product type; not a recovered stock enum.
        #[arg(long)]
        preset_type: i64,
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=8))]
        scene: u8,
        #[arg(long, required_unless_present = "color")]
        label: Option<String>,
        #[arg(long, required_unless_present = "label")]
        color: Option<u32>,
    },
    /// Detect one bounded raw LE-f32 mono frame with an owned host algorithm.
    Tune {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        sample_rate: u32,
        #[arg(long, default_value_t = 440.0)]
        reference_hz: f32,
        #[arg(long, default_value_t = 25.0)]
        min_hz: f32,
        #[arg(long, default_value_t = 2000.0)]
        max_hz: f32,
    },
    /// On-device program: boot the stock SHARC firmware and run the QC until a power action, then
    /// the stock teardown. Real hardware access; use `bringup` for first live tests. Owned blocks
    /// are not offered here: they run only in `bringup` until proof H1 passes. Exits 4 before it
    /// opens anything when the stock firmware is not a known CorOS build with a complete profile
    /// (`tetrad_firmware`). CorOS updates run through `tetrad-update.sh` (docs/updates.md).
    #[cfg(target_os = "linux")]
    Run(RunArgs),
    /// Boot selector (docs/launcher.md): CorOS or Tetrad on the screen, the saved default after 5
    /// s. Exits 10 for CorOS or 11 for Tetrad; any other status is a failure and the boot launcher
    /// starts the saved choice.
    #[cfg(target_os = "linux")]
    Select {
        /// The saved choice: the default, replaced after a touch picks the other UI.
        #[arg(long, default_value = tetrad_app::boot_select::CHOICE_FILE)]
        file: PathBuf,
    },
    /// On-device bring-up run: one stage under a time limit, then the test exit (mute, headphones
    /// 0, exit; ZenWatchdog restarts stock). Every risky option is off unless given.
    #[cfg(target_os = "linux")]
    Bringup(BringupArgs),
    /// Internal: one extension's JavaScript runner, started by the device program with a
    /// module file. Speaks the extension protocol on stdin and stdout.
    #[command(hide = true)]
    ExtensionRunner {
        #[arg(long)]
        module: PathBuf,
    },
    /// Record raw LE-f32 stereo, optionally overdub once, export one tape cycle.
    Loop {
        #[arg(long)]
        record: PathBuf,
        #[arg(long)]
        overdub: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
}

#[cfg(target_os = "linux")]
#[derive(clap::Args)]
struct RunArgs {
    /// Remove this file when start-up ends with the DSP booted and no DSP fault: the boot
    /// launcher's count of unconfirmed starts (`scripts/launcher/tetrad-launch.sh`).
    #[arg(long, value_name = "FILE")]
    started: Option<PathBuf>,
    /// Print the umask and the stock firmware check, then exit (0, or 4 for firmware that is
    /// not pinned) without opening the platform.
    #[arg(long)]
    check: bool,
}

#[cfg(target_os = "linux")]
#[derive(clap::Args)]
struct BringupArgs {
    #[arg(long, value_enum)]
    stage: Stage,
    /// Seconds from the platform open to the test exit.
    #[arg(long, value_parser = clap::value_parser!(u64).range(10..=7200))]
    time_limit: u64,
    /// Log folder: summary, run log, trace and would-write settings.
    #[arg(long)]
    log_dir: PathBuf,
    /// A `.pb` device path deployed instead of LastPreset (stage deploy or full).
    #[arg(long)]
    preset: Option<String>,
    /// Write settings, library and presets to the real files (stage full).
    #[arg(long)]
    persist: bool,
    /// Wi-Fi, internet probe, NTP and the services UI (stage full).
    #[arg(long)]
    services: bool,
    /// Hold every 0x20 unmute muted.
    #[arg(long)]
    keep_muted: bool,
    /// Drill: the first DSP boot reports core 0 silent once, so the SOC1 retry runs.
    #[arg(long)]
    drill_core0: bool,
    /// After start-up send 0x20 values 1, 0, 1, 3, 0, 1, five seconds each.
    #[arg(long)]
    mute_probe: bool,
    /// Let 48 V phantom power reach the inputs (stage full; nothing plugged in).
    #[arg(long)]
    allow_phantom: bool,
    /// `test`: every power action ends in the test exit; `stock`: the stock teardown.
    #[arg(long, value_enum, default_value_t = PowerMode::Test)]
    power: PowerMode,
    /// Trace DSP ring traffic, model-window writes and HAL commands.
    #[arg(long)]
    trace: bool,
    /// Trace HAL, touch and MIDI input and DIN MIDI output.
    #[arg(long)]
    log_input: bool,
    /// Deploy this stock model as its owned block (repeatable); needs `--allow-owned`. Only
    /// DSP core 0 runs it; on another core the stock model stays.
    #[arg(long = "owned", value_name = "MODEL")]
    owned: Vec<tetrad_catalog::owned::OwnedModel>,
    /// Let `--owned` blocks deploy in this bring-up run.
    #[arg(long)]
    allow_owned: bool,
    /// Let cloud calls go over Cortex Control when it is connected and online (stage full,
    /// needs `--services`). Off: Wi-Fi only.
    #[arg(long)]
    control_cloud: bool,
    /// Validate the options and exit 0 without opening the platform (`tetrad-stage.sh` runs this
    /// before it stops stock ZenUI).
    #[arg(long)]
    check: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum View {
    Grid,
    Gig,
    Presets,
}

#[derive(Clone, Copy, ValueEnum)]
enum GridPolicy {
    /// Refuse missing scalars and unproved audio bypass polarity.
    Strict,
    /// Use protobuf scalar defaults and owned true-is-bypassed display policy.
    OwnedDefaults,
}

impl From<GridPolicy> for tetrad_engine::ImportPolicy {
    fn from(policy: GridPolicy) -> Self {
        match policy {
            GridPolicy::Strict => Self::STRICT,
            GridPolicy::OwnedDefaults => Self {
                absent: tetrad_engine::AbsentScalars::Proto3Default,
                bypass: tetrad_engine::BypassPolarity::StoredTrueBypasses,
            },
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePresetBinding {
    ui_handle: u64,
    product_id: String,
    title: String,
}

struct TraceSource(std::vec::IntoIter<ReplayEvent>);

impl InputSource for TraceSource {
    type Error = std::convert::Infallible;

    fn next_event(&mut self) -> Result<Option<LoopEvent>, Self::Error> {
        Ok(self.0.next().map(|event| match event {
            ReplayEvent::Input(input) => LoopEvent::Input(input),
            ReplayEvent::Quit => LoopEvent::Quit,
        }))
    }
}

// Keep only the latest presented frame. A refused/limited replay creates no file.
struct LastFrame(tetrad_ui::Frame);

impl FrameSink for LastFrame {
    type Error = std::convert::Infallible;

    fn present(&mut self, frame: &tetrad_ui::Frame) -> Result<(), Self::Error> {
        self.0 = frame.clone();
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Ui(#[from] tetrad_ui::UiError),
    #[error(transparent)]
    Trace(#[from] TraceError),
    #[error("replay exceeds its {0}-event limit; no output was created")]
    ReplayLimit(u32),
    #[error(transparent)]
    Library(#[from] tetrad_library::Error),
    #[error(transparent)]
    LibraryBackend(#[from] LibraryBackendError),
    #[error("binding manifest: {0}")]
    BindingManifest(#[source] serde_json::Error),
    #[error(transparent)]
    Tuner(#[from] TunerError),
    #[error(transparent)]
    Looper(#[from] tetrad_app::looper::LooperError),
    #[error("audio input must contain complete nonempty stereo frames")]
    StereoFrames,
    #[error("overdub must contain exactly one recorded loop cycle")]
    OverdubLength,
    #[error("zero color is not emitted by scene-dialog confirmation; use the direct preset editor")]
    ZeroColor,
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Platform(#[from] tetrad_app::device::linux::LinuxError),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Startup(#[from] tetrad_app::device::StartupError),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Device(#[from] tetrad_app::device::DeviceError),
    #[cfg(target_os = "linux")]
    #[error(transparent)]
    Options(#[from] tetrad_app::device::bringup::OptionsError),
    #[cfg(target_os = "linux")]
    #[error("boot selector: {0}")]
    Select(#[source] std::io::Error),
    #[cfg(target_os = "linux")]
    #[error("bring-up log: {0}")]
    BringupLog(#[source] std::io::Error),
    #[error("output: {0}")]
    Output(#[from] std::io::Error),
    #[error("JSON output: {0}")]
    Json(#[from] serde_json::Error),
}

fn read_trace(path: &Path, max_events: u32) -> Result<Vec<ReplayEvent>, Error> {
    let events = parse_trace(&read_limited(path, MAX_TRACE_BYTES)?)?;
    let consumed = events
        .iter()
        .position(|event| matches!(event, ReplayEvent::Quit))
        .map_or(events.len(), |index| index + 1);
    if consumed > max_events as usize {
        return Err(Error::ReplayLimit(max_events));
    }
    Ok(events)
}

struct ReplayOutput<B> {
    event_loop: EventLoop<B>,
    frame: tetrad_ui::Frame,
    events: usize,
    stop: &'static str,
}

fn run_replay<B>(
    backend: B,
    events: Vec<ReplayEvent>,
    max_events: u32,
) -> Result<ReplayOutput<B>, Error>
where
    B: CommandBackend,
    Error: From<B::Error>,
{
    let mut event_loop = EventLoop::new(UiState::new(), backend)?;
    let mut sink = LastFrame(event_loop.ui().render());
    let mut source = TraceSource(events.into_iter());
    // read_trace admits the full bounded input first. One spare iteration lets
    // an exact-length trace observe exhaustion without consuming another event.
    let report = event_loop.run(&mut source, &mut sink, max_events as usize + 1);
    let stop = match report.stop {
        RunStop::Quit => "quit",
        RunStop::InputExhausted => "input_exhausted",
        RunStop::LimitReached => return Err(Error::ReplayLimit(max_events)),
        RunStop::UiRejected(error) => return Err(error.into()),
        RunStop::BackendRefused(error) => return Err(error.into()),
        RunStop::InputFailed(error) => match error {},
        RunStop::PresentationFailed { error, .. } => match error {},
    };
    // Terminal stops cancel incomplete gestures after the last presented frame.
    let frame = event_loop.ui().render();
    Ok(ReplayOutput {
        event_loop,
        frame,
        events: report.events,
        stop,
    })
}

fn run(command: Command) -> Result<ExitCode, Error> {
    match command {
        Command::Render {
            output,
            preset,
            scene,
            screen,
            grid_policy,
        } => {
            let scene = SceneIndex::new(scene - 1)?;
            let mut ui = UiState::new();
            if let Some(path) = preset {
                let bytes = read_limited(&path, MAX_PRESET_BYTES)?;
                let session = OfflineSession::from_bytes(&bytes, scene)?;
                let snapshot = if let Some(policy) = grid_policy {
                    session.grid_snapshot(policy.into())?
                } else {
                    session.snapshot()?
                };
                ui.set_snapshot(snapshot)?;
            } else {
                let snapshot = tetrad_ui::UiSnapshot {
                    selected_scene: scene,
                    ..Default::default()
                };
                ui.set_snapshot(snapshot)?;
            }
            ui.handle_input(InputEvent::Activate(Control::Navigate(match screen {
                View::Grid => Screen::Grid,
                View::Gig => Screen::Gig,
                View::Presets => Screen::Presets,
            })));
            let frame = ui.render();
            write_new(&output, |file| frame.write_ppm(file))?;
            print_json(
                json!({"scope":"owned_offline_view", "width":frame.width(), "height":frame.height(), "output":output.display().to_string(), "grid_projected":grid_policy.is_some(), "dsp_loaded":false}),
            )?;
        }
        Command::Replay {
            preset,
            trace,
            output,
            scene,
            grid_policy,
            max_events,
        } => {
            let events = read_trace(&trace, max_events)?;
            let bytes = read_limited(&preset, MAX_PRESET_BYTES)?;
            let session = OfflineSession::from_bytes(&bytes, SceneIndex::new(scene - 1)?)?;
            let backend =
                OfflineBackend::new(session, grid_policy.map(tetrad_engine::ImportPolicy::from))?;
            let replay = run_replay(backend, events, max_events)?;
            let snapshot = replay.event_loop.ui().snapshot();
            write_new(&output, |file| replay.frame.write_ppm(file))?;
            print_json(json!({
                "scope":"owned_injected_ui_replay", "output":output.display().to_string(),
                "events":replay.events, "stop":replay.stop, "changed":snapshot.dirty,
                "selected_scene":snapshot.selected_scene.get() + 1,
                "selected_label":snapshot.scenes[usize::from(snapshot.selected_scene.get())].label,
                "grid_projected":grid_policy.is_some(), "dsp_loaded":false, "source_written":false,
            }))?;
        }
        Command::LibraryReplay {
            database,
            bindings,
            initial_handle,
            preset_type,
            trace,
            output,
            scene,
            grid_policy,
            max_events,
        } => {
            let events = read_trace(&trace, max_events)?;
            let wire: Vec<WirePresetBinding> =
                serde_json::from_slice(&read_limited(&bindings, MAX_BINDINGS_BYTES)?)
                    .map_err(Error::BindingManifest)?;
            if wire.is_empty() || wire.len() > tetrad_ui::MAX_PRESETS {
                return Err(LibraryBackendError::BindingCount.into());
            }
            let bindings = wire
                .into_iter()
                .map(|binding| PresetBinding {
                    ui_handle: binding.ui_handle,
                    product_id: binding.product_id,
                    title: binding.title,
                })
                .collect();
            let library = tetrad_library::PresetLibrary::open_owned(
                &database,
                preset_type,
                tetrad_library::Limits::default(),
                tetrad_library::Access::ReadOnly,
            )?;
            let backend = LibraryBackend::new(
                library,
                bindings,
                initial_handle,
                SceneIndex::new(scene - 1)?,
                grid_policy.map(tetrad_engine::ImportPolicy::from),
            )?;
            let replay = run_replay(backend, events, max_events)?;
            let snapshot = replay.event_loop.ui().snapshot();
            let provenance = replay.event_loop.backend().provenance();
            write_new(&output, |file| replay.frame.write_ppm(file))?;
            print_json(json!({
                "scope":"owned_library_ui_replay", "output":output.display().to_string(),
                "events":replay.events, "stop":replay.stop, "changed":snapshot.dirty,
                "selected_scene":snapshot.selected_scene.get() + 1,
                "selected_label":snapshot.scenes[usize::from(snapshot.selected_scene.get())].label,
                "selected_handle":provenance.ui_handle, "verified_product_id":provenance.product_id,
                "product_read_only":provenance.read_only, "preset_id":snapshot.preset_id,
                "grid_projected":grid_policy.is_some(), "dsp_loaded":false, "database_written":false,
            }))?;
        }
        Command::SceneEdit {
            input,
            output,
            scene,
            label,
            color,
        } => {
            if color == Some(0) {
                return Err(Error::ZeroColor);
            }
            let scene = SceneIndex::new(scene - 1)?;
            let bytes = read_limited(&input, MAX_PRESET_BYTES)?;
            let mut session = OfflineSession::from_bytes(&bytes, scene)?;
            let snapshot = edit_scene(&mut session, label, color)?;
            let exported = session.export();
            write_new(&output, |file| file.write_all(&exported))?;
            print_json(
                json!({"scope":"owned_lossless_local_edit", "output":output.display().to_string(), "bytes":exported.len(), "changed":snapshot.dirty, "device_written":false}),
            )?;
        }
        Command::LibrarySceneEdit {
            database,
            id,
            preset_type,
            scene,
            label,
            color,
        } => {
            if color == Some(0) {
                return Err(Error::ZeroColor);
            }
            let scene = SceneIndex::new(scene - 1)?;
            let mut library = tetrad_library::PresetLibrary::open_owned(
                &database,
                preset_type,
                tetrad_library::Limits::default(),
                tetrad_library::Access::ReadWrite,
            )?;
            let mut stored = library.load(&id)?;
            let mut session = OfflineSession::from_bytes(&stored.document().to_bytes(), scene)?;
            edit_scene(&mut session, label, color)?;
            let exported = session.export();
            stored.edit(|document| {
                *document = tetrad_preset::PresetDocument::parse(
                    &exported,
                    tetrad_preset::Limits::default(),
                )?;
                Ok(())
            })?;
            let outcomes = library.save(std::slice::from_mut(&mut stored))?;
            let changed = outcomes[0] == tetrad_library::SaveOutcome::Updated;
            print_json(
                json!({"scope":"owned_library_scene_save", "database":database.display().to_string(), "id":id, "changed":changed, "device_written":false}),
            )?;
        }
        Command::Tune {
            input,
            sample_rate,
            reference_hz,
            min_hz,
            max_hz,
        } => {
            let tuner = Tuner::new(TunerConfig {
                sample_rate,
                min_hz,
                max_hz,
            })?;
            let reference = ReferencePitch::new(reference_hz)?;
            let bytes = read_limited(&input, MAX_FRAME_LEN * 4)?;
            let samples = decode_f32(&bytes)?;
            let result = match tuner.detect(&samples)? {
                Detection::Silent => json!({"detection":"silent"}),
                Detection::Unpitched => json!({"detection":"unpitched"}),
                Detection::Pitched(pitch) => {
                    let reading = reference.reading(pitch.hz)?;
                    json!({"detection":"pitched", "hz":pitch.hz, "clarity":pitch.clarity, "note":reading.note.to_string(), "midi":reading.note.midi, "cents":reading.cents})
                }
            };
            print_json(
                json!({"scope":"owned_host_tuner", "sample_rate":sample_rate, "reference_hz":reference.hz(), "samples":samples.len(), "result":result}),
            )?;
        }
        #[cfg(target_os = "linux")]
        Command::Run(args) => return run_device(args),
        #[cfg(target_os = "linux")]
        Command::Select { file } => {
            use tetrad_app::boot_select::{exit_code, run};
            let choice = run(&file).map_err(Error::Select)?;
            return Ok(ExitCode::from(exit_code(choice)));
        }
        #[cfg(target_os = "linux")]
        Command::Bringup(args) => return bringup(args),
        Command::ExtensionRunner { module } => return Ok(extension_runner(&module)),
        Command::Loop {
            record,
            overdub,
            output,
        } => {
            let recording = stereo(&decode_f32(&read_limited(&record, MAX_AUDIO_BYTES)?)?)?;
            let overdub = overdub
                .map(|path| -> Result<_, Error> {
                    stereo(&decode_f32(&read_limited(&path, MAX_AUDIO_BYTES)?)?)
                })
                .transpose()?;
            if overdub
                .as_ref()
                .is_some_and(|frames| frames.len() != recording.len())
            {
                return Err(Error::OverdubLength);
            }
            let mut looper = Looper::new(LooperLimits {
                max_loop_frames: recording.len(),
                max_block_frames: MAX_BLOCK_FRAMES,
            })?;
            looper.command(LooperCommand::Record)?;
            let mut scratch = vec![[0.0; 2]; recording.len().min(MAX_BLOCK_FRAMES)];
            for block in recording.chunks(MAX_BLOCK_FRAMES) {
                looper.process_block(block, &mut scratch[..block.len()])?;
            }
            if let Some(frames) = overdub {
                looper.command(LooperCommand::Overdub)?;
                for block in frames.chunks(MAX_BLOCK_FRAMES) {
                    looper.process_block(block, &mut scratch[..block.len()])?;
                }
            }
            looper.command(LooperCommand::Stop)?;
            write_new(&output, |file| {
                let mut writer = BufWriter::new(file);
                for frame in looper.loop_frames() {
                    for sample in frame {
                        writer.write_all(&sample.to_le_bytes())?;
                    }
                }
                writer.flush()
            })?;
            print_json(
                json!({"scope":"owned_host_loop", "output":output.display().to_string(), "frames":looper.loop_frames().len(), "channels":2, "format":"raw_le_f32", "sample_rate":null}),
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Gap census r48 A2: init starts stock ZenUI with umask 022, so stock files are 0644 and
/// folders 0755. Tetrad writes with default modes, so the mask decides; an SSH shell has 077.
/// Returns the text for `--check`: the mask now in force and the caller's. The QC kernel's
/// `/proc/self/status` has no `Umask:` line (Linux 4.7+), so a second `umask` reads it back.
#[cfg(target_os = "linux")]
fn stock_umask() -> String {
    use tetrad_sys::lifecycle::set_umask;
    let caller = set_umask(0o022);
    let now = set_umask(0o022);
    format!("umask {now:04o} (caller {caller:04o})")
}

/// `tetrad run`. The firmware check comes first, after the comm name `ZenUI`
/// ([`LinuxRoot::start_as_stock`]): ZenWatchdog reboots the unit when `pidof ZenUI` fails for
/// 3 s (o-lifecycle §2), and hashing the 22 MB image takes about 3 to 6 s on the unit. The
/// check selects the running build (`tetrad_firmware::select`) before any stock read. A failed
/// check exits
/// [`WRONG_FIRMWARE_EXIT`] with nothing opened, so the boot launcher can start stock ZenUI in
/// the same boot.
///
/// [`LinuxRoot::start_as_stock`]: tetrad_app::device::linux::LinuxRoot::start_as_stock
/// [`WRONG_FIRMWARE_EXIT`]: tetrad_app::device::linux::WRONG_FIRMWARE_EXIT
#[cfg(target_os = "linux")]
fn run_device(args: RunArgs) -> Result<ExitCode, Error> {
    use tetrad_app::device::bringup::RunOptions;
    use tetrad_app::device::linux::{LinuxError, LinuxPlatform, LinuxRoot, WRONG_FIRMWARE_EXIT};

    let umask = stock_umask();
    let root = LinuxRoot::new("/");
    // `--check` runs while stock ZenUI still runs: it keeps its own name.
    let pinned = if args.check {
        root.check_pinned_firmware()
    } else {
        root.start_as_stock()?
    }
    .and_then(|profile| {
        tetrad_firmware::select(profile)
            .map(|()| profile)
            .map_err(LinuxError::Firmware)
    });
    if args.check || pinned.is_err() {
        let firmware = match &pinned {
            Ok(profile) => format!("stock firmware is the supported {} build", profile.firmware),
            Err(error) => format!("{error}; stock ZenUI must run"),
        };
        let _ = writeln!(std::io::stderr().lock(), "tetrad run: {umask}; {firmware}");
        return Ok(pinned.map_or(ExitCode::from(WRONG_FIRMWARE_EXIT), |_| ExitCode::SUCCESS));
    }
    // Prints nothing from here: after the power teardown stderr may have no reader. The
    // device logs through `Platform::log`, which ignores write errors.
    let mut platform = LinuxPlatform::open()?;
    platform.enable_extensions()?;
    let device = tetrad_app::device::Device::start_with(platform, RunOptions::STOCK)?;
    if let Some(started) = args.started
        && device.dsp().is_some()
        && device.state().dsp_fault.is_none()
    {
        // A failed remove leaves the start unconfirmed: the launcher counts it.
        let _ = std::fs::remove_file(started);
    }
    device.run()?;
    Ok(ExitCode::SUCCESS)
}

/// P1–P4: options, the guard thread, the decorated platform, start-up and the bring-up loop.
/// Prints nothing on success: the summary and run log in `--log-dir` hold the result.
/// `--check` prints the validated options and the umask and stops before the platform opens.
#[cfg(target_os = "linux")]
fn bringup(args: BringupArgs) -> Result<ExitCode, Error> {
    use tetrad_app::device::bringup::{Bringup, Heartbeat, Launch, RunOptions};
    use tetrad_app::device::linux::{LinuxError, LinuxPlatform, LinuxRoot};
    use tetrad_app::device::{Device, Platform};
    use tetrad_dsp_host::session::Clock;

    let umask = stock_umask();
    let limit = std::time::Duration::from_secs(args.time_limit);
    let options = RunOptions {
        preset: args.preset,
        persist: args.persist,
        services: args.services,
        keep_muted: args.keep_muted,
        mute_probe: args.mute_probe,
        allow_phantom: args.allow_phantom,
        power: args.power,
        trace: args.trace,
        log_input: args.log_input,
        owned: args.owned.into_iter().collect(),
        allow_owned: args.allow_owned,
        control_cloud: args.control_cloud,
        drill_core0: args.drill_core0,
        ..RunOptions::bringup(args.stage, limit, args.log_dir.clone())
    };
    options.validate()?;
    if args.check {
        writeln!(
            std::io::stdout().lock(),
            "tetrad bringup: options valid: {options}; {umask}"
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    // The comm name comes before the firmware hash, as in `tetrad run`: the stage has stopped
    // stock ZenUI, and ZenWatchdog restarts the unit after about 3 s without a ZenUI.
    // A known complete build runs as itself. Else the default build stays, and its stock reads
    // refuse the files as before (bring-up is a drill, not the boot path).
    if let Ok(profile) = LinuxRoot::new("/").start_as_stock()? {
        tetrad_firmware::select(profile).map_err(LinuxError::Firmware)?;
    }
    let mut platform = LinuxPlatform::open()?;
    if !options.services {
        platform.skip_supplicant_monitor();
    }
    let clock = platform.clock();
    let heartbeat = Heartbeat::default();
    heartbeat.beat(clock.now());
    spawn_guard(clock, heartbeat.clone(), limit, args.log_dir)?;
    let launch = Launch {
        stored_boot_count: stored_boot_count(),
        binary_sha256: binary_sha256(),
        heartbeat,
        host: Box::new(OsHost),
    };
    let platform = Bringup::new(platform, &options, launch).map_err(Error::BringupLog)?;
    let exit = Device::bringup(platform, options)?;
    if let Some(Err(error)) = &exit.power {
        let _ = writeln!(
            std::io::stderr().lock(),
            "tetrad: stock power path: {error}"
        );
    }
    Ok(ExitCode::from(exit.code()))
}

/// P3: aborts the process when the loop heartbeat stalls or the run overruns its limit
/// (`Heartbeat::verdict`); ZenWatchdog then restarts stock (R1). Nothing can mute here: the
/// device belongs to the stalled thread.
#[cfg(target_os = "linux")]
fn spawn_guard(
    clock: tetrad_dsp_host::session::SystemClock,
    heartbeat: tetrad_app::device::bringup::Heartbeat,
    limit: std::time::Duration,
    log_dir: PathBuf,
) -> Result<(), Error> {
    use tetrad_dsp_host::session::Clock;
    std::thread::Builder::new()
        .name("tetrad-guard".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if let Some(stall) = heartbeat.verdict(clock.now(), limit) {
                    let line = format!(
                        "GUARD abort at {:.3} s: {stall}\n",
                        clock.now().as_secs_f64()
                    );
                    let _ = std::fs::write(log_dir.join("guard.log"), &line);
                    let _ = std::io::stderr().lock().write_all(line.as_bytes());
                    std::process::abort();
                }
            }
        })
        .map_err(Error::BringupLog)?;
    Ok(())
}

/// `/opt/neuraldsp/boot_count.txt` without the increment `LinuxPlatform::boot_count` writes.
#[cfg(target_os = "linux")]
fn stored_boot_count() -> u32 {
    std::fs::read_to_string("/opt/neuraldsp/boot_count.txt")
        .ok()
        .and_then(|text| text.lines().next()?.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn binary_sha256() -> Option<String> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(std::fs::read("/proc/self/exe").ok()?);
    Some(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(target_os = "linux")]
struct OsHost;

#[cfg(target_os = "linux")]
impl tetrad_app::device::bringup::Host for OsHost {
    fn stop_requested(&mut self) -> bool {
        Path::new(tetrad_app::device::bringup::STOP_FILE).exists()
    }

    fn rss_kib(&mut self) -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))?;
        line.trim().strip_suffix("kB")?.trim().parse().ok()
    }
}

fn edit_scene(
    session: &mut OfflineSession,
    label: Option<String>,
    color: Option<u32>,
) -> Result<tetrad_ui::UiSnapshot, Error> {
    let mut ui = UiState::new();
    ui.set_snapshot(session.snapshot()?)?;
    // Scene names are edited from Gig View (EDIT SCENE), as on stock.
    ui.show(tetrad_ui::Screen::Gig);
    ui.handle_input(InputEvent::Activate(Control::EditScenes));
    if let Some(label) = label {
        let refusals = ui.handle_input(InputEvent::TextChanged(label));
        session.apply(&refusals)?;
    }
    if let Some(raw_color) = color {
        ui.handle_input(InputEvent::DraftColor(raw_color));
    }
    let commands = ui.handle_input(InputEvent::Activate(Control::ConfirmScenes));
    Ok(session.apply(&commands)?)
}

fn stereo(samples: &[f32]) -> Result<Vec<StereoFrame>, Error> {
    if samples.is_empty() || !samples.len().is_multiple_of(2) {
        return Err(Error::StereoFrames);
    }
    Ok(samples
        .chunks_exact(2)
        .map(|samples| [samples[0], samples[1]])
        .collect())
}

fn print_json(value: serde_json::Value) -> Result<(), Error> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    writeln!(stdout)?;
    Ok(())
}

/// Largest extension module the runner loads.
const MAX_MODULE_BYTES: usize = 1024 * 1024;
/// Runner address-space limit: the mapped `tetrad` image, its libraries, two thread stacks and
/// the QuickJS heap. Trial value; measured on the unit in gate A.
#[cfg(target_os = "linux")]
const RUNNER_ADDRESS_SPACE: usize = 128 * 1024 * 1024;

/// The child process of one extension (`tetrad_extensions::runner`). Errors go to stderr,
/// which the device program inherits into its log; the host sees the exit.
fn extension_runner(module: &Path) -> ExitCode {
    let fail = |message: String| {
        let _ = writeln!(
            std::io::stderr().lock(),
            "tetrad extension-runner: {message}"
        );
        ExitCode::FAILURE
    };
    // The module is read first: the confined runner (user `nobody`) cannot read the store.
    let source = match read_limited(module, MAX_MODULE_BYTES).map(String::from_utf8) {
        Ok(Ok(source)) => source,
        Ok(Err(_)) => return fail("module is not UTF-8".into()),
        Err(error) => return fail(error.to_string()),
    };
    #[cfg(target_os = "linux")]
    if let Err(error) = tetrad_sys::process::confine_extension_runner(RUNNER_ADDRESS_SPACE) {
        return fail(format!("confine: {error}"));
    }
    let input = std::io::BufReader::new(std::io::stdin());
    let limits = tetrad_extensions::runner::Limits {
        #[cfg(target_os = "linux")]
        slice_clock: tetrad_sys::process::thread_cpu_time,
        ..tetrad_extensions::runner::Limits::default()
    };
    match tetrad_extensions::runner::run(&source, input, &mut std::io::stdout().lock(), limits) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error.to_string()),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(code) => code,
        Err(error) => {
            // Never `eprintln!`: it panics when stderr is a pipe without a reader.
            let _ = writeln!(std::io::stderr().lock(), "tetrad: {error}");
            ExitCode::FAILURE
        }
    }
}
