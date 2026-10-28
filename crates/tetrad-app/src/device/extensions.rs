//! Extensions bridge: runner sessions, host calls and events for installed extensions.
//!
//! The host owns every authority. A runner only proposes calls; this bridge checks the
//! session, scope, capability and preset context of each one, then turns accepted edits into
//! ordinary device [`Action`]s through [`Effect::ExtensionAction`], so native scene, dirty and
//! echo rules apply. Native edits come back to subscribed extensions as
//! [`Event::ControlChanged`]; extension edits do not. Extensions do not run mappings: they
//! declare triggers, and each accepted firing goes to the device as a [`FiredTrigger`] in
//! [`Outcome::triggers`].
//!
//! Lifecycle per installation: disabled → starting (read/register-only activation) → running →
//! faulted or disabled. MIDI channel reservations come from a committed activation and stay
//! while the installation is enabled, also when it faults, so a dead runner never lets its
//! channel fall through to native actions. Messages on a reserved channel without a running
//! session are dropped.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

use serde_json::{Value as Json, json};
use tetrad_engine::{Column, Row, Site as EngineSite, Slot};
use tetrad_extensions::host::RunnerOutput;
use tetrad_extensions::protocol::{
    AssetText, Binding, BlockPublish, BuildOutcome, CallResult, ControlChange, ControlKind,
    ErrorCode, Event, ExtensionInfo, HostMessage, LogLevel, MAX_TRIGGER_LABEL, MAX_TRIGGERS,
    MidiEvent, PROTOCOL_VERSION, PUBLISH_BLOCK, Pair, Registrations, RunnerMessage, Site,
    TriggerSpec,
};
use tetrad_midi::{Channel, MidiMessage, U7};

use super::actions::Action;
use super::extension_jobs::{AssetFile, AssetScan, Loaded};
pub use super::extension_jobs::{ExtensionJob, JobResult};
use super::{Effect, State, Subsystem};
use tetrad_extensions::catalog::{Catalog, PublicKey};
use tetrad_extensions::manifest::{BlockSpec, Capability, SettingKind};
use tetrad_extensions::repository::SourceId;
use tetrad_extensions::store::{InstallKey, Source};
use tetrad_ui::{
    AvailableExtension, ExtensionBlock, ExtensionButton, ExtensionCommand, ExtensionSetting,
    ExtensionSettingInput, ExtensionSettingValue, ExtensionSource, ExtensionState, ExtensionsView,
    InstalledExtension, PendingTrust,
};

/// Log lines kept per installation for the detail page.
const LOG_LINES: usize = 50;
/// Status lines an extension may show, and their length.
const STATUS_LINES: usize = 16;
const STATUS_CHARS: usize = 120;
/// Events in flight to one runner before continuous CC input coalesces to the latest value.
const IN_FLIGHT: usize = 32;
/// Events waiting in the host for one runner; ordered input over this faults the session.
const BACKLOG: usize = 256;
/// Bytes of one installation's key/value storage, as JSON.
pub const MAX_STORAGE: usize = 256 * 1024;
/// Keys of one installation's key/value storage.
const MAX_STORAGE_KEY: usize = 128;
/// Longest source of a trigger firing, such as `cc:20`.
const MAX_TRIGGER_SOURCE: usize = 64;

/// Starts, feeds and stops runner processes. Runner output comes back through
/// [`super::Platform::poll_input`] as [`super::platform::Input::Extension`].
pub trait ExtensionBackend {
    /// Starts the runner of `key` on its module file and returns its run number. Output of an
    /// older run of the same key is stale.
    fn start(&mut self, key: &str, module: &std::path::Path) -> Result<u64, String>;
    fn send(&mut self, key: &str, message: &HostMessage) -> Result<(), String>;
    /// Kills the runner; its exit still arrives as [`RunnerOutput::Exited`].
    fn stop(&mut self, key: &str);
    /// Queues store or repository work; its result arrives as [`ExtensionInput::Job`].
    fn submit(&mut self, job: ExtensionJob);
}

/// Platform input for this bridge.
#[derive(Debug, Clone, PartialEq)]
pub enum ExtensionInput {
    Runner {
        key: String,
        run: u64,
        output: RunnerOutput,
    },
    Job(JobResult),
}

enum Runner {
    Process(tetrad_extensions::host::RunnerProcess),
    Thread(tetrad_extensions::host::RunnerThread),
}

/// The standard backend: one `tetrad extension-runner` child process per extension
/// (`program` is the running binary), or a thread runner for tests.
pub struct LocalBackend {
    program: Option<PathBuf>,
    runners: HashMap<String, Runner>,
    next_run: u64,
    jobs: Option<std::sync::mpsc::Sender<ExtensionJob>>,
    /// Bounded: a runner that floods output blocks its own reader thread (and so its own
    /// stdout) instead of growing the device program's memory.
    sender: std::sync::mpsc::SyncSender<ExtensionInput>,
    received: std::sync::mpsc::Receiver<ExtensionInput>,
}

impl LocalBackend {
    /// Child processes of `program extension-runner --module FILE`.
    pub fn processes(program: PathBuf) -> Self {
        Self::new(Some(program))
    }

    /// In-process thread runners (tests).
    pub fn threads() -> Self {
        Self::new(None)
    }

    fn new(program: Option<PathBuf>) -> Self {
        let (sender, received) = std::sync::mpsc::sync_channel(INPUT_QUEUE);
        Self {
            program,
            runners: HashMap::new(),
            next_run: 0,
            jobs: None,
            sender,
            received,
        }
    }

    /// Runs store and repository jobs on a worker thread over the store at `root`.
    pub fn with_worker<F>(mut self, root: PathBuf, mut fetch: F) -> Self
    where
        F: tetrad_extensions::repository::Fetch + Send + 'static,
    {
        let (jobs, inbox) = std::sync::mpsc::channel::<ExtensionJob>();
        let results = self.sender.clone();
        std::thread::spawn(move || {
            let store = tetrad_extensions::store::Store::open(&root);
            while let Ok(first) = inbox.recv() {
                // A burst of `storage.set` (one per CC of a pedal sweep) becomes one write.
                if matches!(first, ExtensionJob::SaveStorage { .. }) {
                    std::thread::sleep(STORAGE_HOLD);
                }
                let batch: Vec<ExtensionJob> =
                    std::iter::once(first).chain(inbox.try_iter()).collect();
                // A later save of the same storage makes an earlier one pointless: it is
                // answered without a write, so a flood of `storage.set` costs one write.
                let superseded: Vec<bool> = (0..batch.len())
                    .map(|i| match &batch[i] {
                        ExtensionJob::SaveStorage { key, .. } => batch[i + 1..].iter().any(
                            |later| matches!(later, ExtensionJob::SaveStorage { key: k, .. } if k == key),
                        ),
                        _ => false,
                    })
                    .collect();
                for (job, superseded) in batch.into_iter().zip(superseded) {
                    let result = match &store {
                        _ if superseded => JobResult::Saved(Ok(())),
                        Ok(store) => super::extension_jobs::run(store, &mut fetch, job),
                        Err(error) => JobResult::Saved(Err(format!("extension store: {error}"))),
                    };
                    if results.send(ExtensionInput::Job(result)).is_err() {
                        return;
                    }
                }
            }
        });
        self.jobs = Some(jobs);
        self
    }

    /// Moves runner output to `out`.
    pub fn drain(&mut self, out: &mut Vec<super::platform::Input>) {
        // A bounded share per device pass: a flooding runner delays only itself.
        let inputs = self.received.try_iter().take(MAX_DRAIN);
        out.extend(inputs.map(super::platform::Input::Extension));
    }
}

impl ExtensionBackend for LocalBackend {
    fn start(&mut self, key: &str, module: &std::path::Path) -> Result<u64, String> {
        self.stop(key);
        self.next_run += 1;
        let run = self.next_run;
        let sender = self.sender.clone();
        let owner = key.to_owned();
        let sink = move |output| {
            let _ = sender.send(ExtensionInput::Runner {
                key: owner.clone(),
                run,
                output,
            });
        };
        let runner = match &self.program {
            Some(program) => {
                let mut command = std::process::Command::new(program);
                command.arg("extension-runner").arg("--module").arg(module);
                Runner::Process(
                    tetrad_extensions::host::RunnerProcess::spawn(command, sink)
                        .map_err(|e| e.to_string())?,
                )
            }
            None => {
                let source = std::fs::read_to_string(module).map_err(|e| e.to_string())?;
                let limits = tetrad_extensions::runner::Limits::default();
                Runner::Thread(
                    tetrad_extensions::host::RunnerThread::spawn(source, limits, sink)
                        .map_err(|e| e.to_string())?,
                )
            }
        };
        self.runners.insert(key.to_owned(), runner);
        Ok(run)
    }

    fn send(&mut self, key: &str, message: &HostMessage) -> Result<(), String> {
        match self.runners.get_mut(key) {
            Some(Runner::Process(runner)) => runner.send(message).map_err(|e| e.to_string()),
            Some(Runner::Thread(runner)) => runner.send(message).map_err(|e| e.to_string()),
            None => Err("the runner is not started".into()),
        }
    }

    fn stop(&mut self, key: &str) {
        match self.runners.remove(key) {
            // Dropping a process runner kills and reaps it.
            Some(Runner::Process(runner)) => drop(runner),
            Some(Runner::Thread(mut runner)) => runner.kill(),
            None => {}
        }
    }

    fn submit(&mut self, job: ExtensionJob) {
        let sent = self
            .jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok());
        if !sent {
            let refused = JobResult::Saved(Err("no extension worker".into()));
            // The device thread is the only receiver: never block it on its own queue.
            let _ = self.sender.try_send(ExtensionInput::Job(refused));
        }
    }
}

/// What an installation may do; from its manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub midi_receive: bool,
    pub midi_send: bool,
    pub controls_read: bool,
    pub controls_edit: bool,
    pub storage: bool,
    pub triggers: bool,
    pub blocks: bool,
    pub usb: bool,
}

/// One installed extension as the bridge runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Installation {
    /// `owner/repo:extension.id`.
    pub key: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub source: String,
    /// `None`: a data-only package (themes or DSP blocks), which has no runner.
    pub module: Option<PathBuf>,
    pub capabilities: Capabilities,
    /// The DSP blocks of the manifest.
    pub blocks: Vec<BlockSpec>,
    /// The IR of the static blocks, by block ID.
    pub block_irs: BTreeMap<String, qcdsp::Block>,
    /// IR the extension built from assets in an earlier run, by block and variant.
    pub generated: BTreeMap<(String, String), qcdsp::Block>,
    pub config: Json,
    pub storage: BTreeMap<String, Json>,
    pub enabled: bool,
}

/// An installation that builds blocks from asset files: what Remote shows for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetKind {
    pub key: String,
    pub id: String,
    pub name: String,
    /// What the files are, such as "NAM model".
    pub label: String,
    /// The extensions of the files, with the dot.
    pub extensions: Vec<String>,
}

impl Installation {
    /// The store key; `None` only for a key that was not made by the store.
    fn install_key(&self) -> Option<InstallKey> {
        self.key.parse().ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Disabled,
    Starting,
    Running,
    Faulted(String),
}

struct Session {
    /// The backend run number; output of other runs is stale.
    run: u64,
    /// Scopes the runner may still use, with the preset generation they were opened in.
    scopes: HashMap<u64, u64>,
    /// Events sent and not yet answered with `Done`.
    in_flight: usize,
    backlog: VecDeque<Event>,
    /// USB request IDs of [`Event::Usb`] events in the backlog, in order.
    usb_waiting: VecDeque<u32>,
    /// The USB request ID of each open [`Event::Usb`] scope.
    usb_scopes: HashMap<u64, u32>,
}

impl Session {
    /// Opens `scope` for `event` (it takes the next waiting USB request when it is one).
    fn open(&mut self, scope: u64, generation: u64, event: &Event) {
        self.scopes.insert(scope, generation);
        self.in_flight += 1;
        if matches!(event, Event::Usb { .. })
            && let Some(request) = self.usb_waiting.pop_front()
        {
            self.usb_scopes.insert(scope, request);
        }
    }

    /// Error replies for every USB request the session can no longer answer.
    fn orphans(self, reason: &str) -> impl Iterator<Item = UsbReply> {
        (self.usb_scopes.into_values().chain(self.usb_waiting)).map(move |request| UsbReply {
            request,
            result: Err(format!("the extension stopped: {reason}")),
        })
    }
}

struct Entry {
    installation: Installation,
    state: RunState,
    session: Option<Session>,
    /// Committed by the last successful activation; kept while enabled.
    registrations: Registrations,
    status_lines: Vec<String>,
    log: VecDeque<String>,
    /// Generated block IR by block and variant: the cache, then each new build.
    generated: BTreeMap<(String, String), qcdsp::Block>,
    /// Why the last build of a block and variant failed.
    build_errors: BTreeMap<(String, String), String>,
    /// Asset files that wait for a [`Event::BuildBlock`]; one build runs at a time.
    builds: VecDeque<AssetFile>,
    /// The scope and asset of the build that runs.
    building: Option<(u64, AssetFile)>,
    /// Asset blocks for which the last scan found no file.
    no_assets: Vec<String>,
    /// Builds that stopped the session (a fault, a time limit), with the asset text they had.
    /// Such an asset is not built again until its file changes, so one bad file cannot fault
    /// the extension at every start.
    stopped_builds: BTreeMap<(String, String), String>,
}

impl Entry {
    fn new(installation: Installation) -> Self {
        Self {
            generated: installation.generated.clone(),
            installation,
            state: RunState::Disabled,
            session: None,
            registrations: Registrations::default(),
            status_lines: Vec::new(),
            log: VecDeque::new(),
            build_errors: BTreeMap::new(),
            builds: VecDeque::new(),
            building: None,
            no_assets: Vec::new(),
            stopped_builds: BTreeMap::new(),
        }
    }

    /// The session ended: no build runs, and the queue goes (the next activation scans again).
    /// When a `fault` ended it, a build that was running is recorded as stopped.
    fn end_session(&mut self, fault: Option<&str>) {
        if let (Some((_, file)), Some(reason)) = (self.building.take(), fault) {
            let id = (file.block, file.variant);
            self.stopped_builds.insert(id.clone(), file.text);
            self.build_errors
                .insert(id, format!("the build stopped the extension: {reason}"));
        }
        self.builds.clear();
    }
}

/// Runner outputs and job results waiting for the device.
const INPUT_QUEUE: usize = 1024;
/// How long the store worker holds a storage save to collect later ones.
const STORAGE_HOLD: std::time::Duration = std::time::Duration::from_millis(300);
/// Runner outputs and job results the device takes in one pass.
const MAX_DRAIN: usize = 64;
/// Command buttons one extension may register.
const MAX_COMMANDS: usize = 16;

/// Work the device does for the bridge after a call.
#[derive(Debug, Default)]
pub struct Outcome {
    pub effects: Vec<Effect>,
    /// Storage of these installations changed and should be saved.
    pub storage_changed: Vec<String>,
    /// Reservations changed: the MIDI controller needs [`Extensions::reserved_channels`].
    pub reservations_changed: bool,
    /// Triggers the extensions fired (`scope.triggers.fire`), for the mappings bridge.
    pub triggers: Vec<FiredTrigger>,
    /// The community blocks may have changed: the device installs
    /// [`Extensions::community_sources`] again.
    pub community_changed: bool,
    /// Store work a host call asked for; the bridge submits it before it returns.
    jobs: Vec<ExtensionJob>,
    /// Answers to USB messages ([`Extensions::usb_message`]), for the USB link.
    pub usb_replies: Vec<UsbReply>,
}

/// The answer of an extension to one USB message: its handler's return value or its error.
#[derive(Debug, Clone, PartialEq)]
pub struct UsbReply {
    pub request: u32,
    pub result: Result<Json, String>,
}

/// One firing of an extension trigger.
#[derive(Debug, Clone, PartialEq)]
pub struct FiredTrigger {
    /// Installation key, `owner/repo:extension.id`.
    pub key: String,
    /// The trigger id the extension declared.
    pub trigger: String,
    /// What fired inside the trigger, such as `cc:20`.
    pub source: String,
    /// Display text of the source, such as "CC 20".
    pub label: String,
    /// 0..=1.
    pub value: f32,
    /// A continuous source (a CC); otherwise a press (a note, a PC).
    pub continuous: bool,
    /// A press source: pressed (`true`) or released. Firings without the field are presses.
    pub pressed: bool,
}

#[derive(Default)]
pub struct Extensions {
    slots: BTreeMap<String, Entry>,
    /// Preset generation: a preset load makes every older scope stale.
    generation: u64,
    next_scope: u64,
    repositories: Repositories,
    /// Set by every device step that reaches this bridge; [`Self::view`] is built again only
    /// then, not on every pass.
    pub(in crate::device) view_stale: bool,
}

/// The store and repository model behind the Extensions pages.
#[derive(Default)]
struct Repositories {
    loaded: bool,
    load_requested: bool,
    sources: Vec<Source>,
    catalogs: HashMap<SourceId, Catalog>,
    source_status: HashMap<SourceId, String>,
    pending_trust: Option<(SourceId, PublicKey)>,
    /// Installed records by key; the source of truth for [`Installation`]s.
    records: BTreeMap<String, Loaded>,
    jobs: usize,
    status: Option<String>,
}

impl Repositories {
    /// The labels of the sources other than `source` whose installation of `id` is on: with an
    /// installation from `source` on too, each adds the same blocks, themes and handlers twice.
    fn enabled_elsewhere(&self, source: &SourceId, id: &str) -> Vec<String> {
        (self.records.values())
            .map(|record| &record.installed)
            .filter(|installed| installed.enabled && installed.key.id == id)
            .filter(|installed| &installed.key.source != source)
            .map(|installed| self.label(&installed.key.source))
            .collect()
    }

    /// "Default" for the default repository, as the pages name it; any other its `owner/repo`.
    fn label(&self, source: &SourceId) -> String {
        let default = (self.sources.iter()).any(|s| s.default && &s.source == source);
        if default {
            "Default".into()
        } else {
            source.to_string()
        }
    }
}

impl Extensions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the known installations (after a store load or an install). Running sessions of
    /// installations that are gone or changed are stopped.
    pub fn set_installations(
        &mut self,
        installations: Vec<Installation>,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let incoming: BTreeMap<_, _> = installations
            .into_iter()
            .map(|installation| (installation.key.clone(), installation))
            .collect();
        let stale: Vec<String> = (self.slots.iter())
            .filter(|(key, slot)| {
                incoming.get(*key).is_none_or(|new| {
                    new.version != slot.installation.version
                        || new.module != slot.installation.module
                })
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.stop(&key, backend, &mut outcome);
            // A removed slot's channels go back to native MIDI even when it was running.
            let slot = self.slots.remove(&key);
            if slot.is_some_and(|slot| !slot.registrations.midi_channels.is_empty()) {
                outcome.reservations_changed = true;
            }
        }
        for (key, installation) in incoming {
            match self.slots.get_mut(&key) {
                Some(slot) => {
                    let enabled = installation.enabled;
                    slot.installation = installation;
                    if !enabled {
                        self.stop(&key, backend, &mut outcome);
                    }
                }
                None => {
                    self.slots.insert(key, Entry::new(installation));
                }
            }
        }
        outcome.community_changed = true;
        let keys: Vec<String> = self.slots.keys().cloned().collect();
        for key in keys {
            let slot = &self.slots[&key];
            if slot.installation.enabled && slot.state == RunState::Disabled {
                self.start(&key, backend, &mut outcome);
            }
        }
        outcome
    }

    /// The enabled installations that build DSP blocks from asset files (NAM models, for example),
    /// in key order: where Remote shows and takes their files. A disabled extension is not shown.
    pub fn asset_kinds(&self) -> Vec<AssetKind> {
        self.slots
            .values()
            .filter(|slot| slot.installation.enabled)
            .filter_map(|slot| {
                let installation = &slot.installation;
                let specs: Vec<_> = (installation.blocks.iter())
                    .filter(|block| block.generated)
                    .filter_map(|block| block.asset.as_ref())
                    .collect();
                let label = specs.first()?.label.clone();
                let mut extensions: Vec<String> = Vec::new();
                for extension in specs.iter().flat_map(|spec| spec.extensions.iter()) {
                    if !extensions.contains(extension) {
                        extensions.push(extension.clone());
                    }
                }
                Some(AssetKind {
                    key: installation.key.clone(),
                    id: installation.id.clone(),
                    name: installation.name.clone(),
                    label,
                    extensions,
                })
            })
            .collect()
    }

    /// What became of the asset file with the block variant `variant`: `("ready", "")` for a
    /// built block, `("failed", reason)` for a failed build, else `("waiting", "")`.
    pub fn asset_state(&self, key: &str, variant: &str) -> (&'static str, String) {
        let Some(slot) = self.slots.get(key) else {
            return ("waiting", String::new());
        };
        if let Some(error) = slot
            .build_errors
            .iter()
            .find_map(|((_, v), error)| (v == variant).then(|| error.clone()))
        {
            return ("failed", error);
        }
        if slot.generated.keys().any(|(_, v)| v == variant) {
            return ("ready", String::new());
        }
        ("waiting", String::new())
    }

    /// Reads the asset folder of `key` again, so a file that Remote stored or removed shows up
    /// (or goes) without a restart. Nothing happens while the extension does not run.
    pub fn rescan_assets(&mut self, key: &str, backend: &mut dyn ExtensionBackend) {
        self.scan_assets(key, backend);
    }

    pub fn installation(&self, key: &str) -> Option<&Installation> {
        self.slots.get(key).map(|slot| &slot.installation)
    }

    pub fn state(&self, key: &str) -> Option<&RunState> {
        self.slots.get(key).map(|slot| &slot.state)
    }

    pub fn registrations(&self, key: &str) -> Option<&Registrations> {
        self.slots.get(key).map(|slot| &slot.registrations)
    }

    pub fn status_lines(&self, key: &str) -> &[String] {
        self.slots.get(key).map_or(&[], |slot| &slot.status_lines)
    }

    pub fn log(&self, key: &str) -> Vec<String> {
        (self.slots.get(key)).map_or_else(Vec::new, |slot| slot.log.iter().cloned().collect())
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.slots.keys()
    }

    /// Channels (bit `n` = channel index `n`) reserved by enabled installations.
    pub fn reserved_channels(&self) -> u16 {
        // Only a running extension holds its channels; a stopped or faulted one gives them
        // back to native MIDI.
        (self.slots.values())
            .filter(|slot| slot.installation.enabled && slot.state == RunState::Running)
            .flat_map(|slot| &slot.registrations.midi_channels)
            .fold(0, |mask, &channel| mask | 1 << (channel - 1))
    }

    /// The QC's own MIDI channel changed: an extension that holds the new channel faults, so
    /// native MIDI gets it back.
    pub fn native_channel_changed(
        &mut self,
        native: Option<u8>,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let Some(native) = native else {
            return outcome;
        };
        let holders: Vec<String> = (self.slots.iter())
            .filter(|(_, slot)| {
                slot.state == RunState::Running
                    && slot.registrations.midi_channels.contains(&native)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in holders {
            let reason =
                format!("MIDI channel {native} is now the QC's own MIDI channel; choose another");
            self.fault(&key, reason, backend, &mut outcome);
        }
        outcome
    }

    /// Starts a new session for an enabled installation (also RETRY after a fault).
    pub fn start(&mut self, key: &str, backend: &mut dyn ExtensionBackend, outcome: &mut Outcome) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        if slot.session.is_some() || !slot.installation.enabled {
            return;
        }
        let Some(module) = slot.installation.module.clone() else {
            // Themes or DSP blocks only: nothing runs; they follow the enabled switch.
            slot.state = RunState::Running;
            return;
        };
        let activate = HostMessage::Activate {
            protocol: PROTOCOL_VERSION,
            extension: ExtensionInfo {
                id: slot.installation.id.clone(),
                version: slot.installation.version.clone(),
            },
            config: slot.installation.config.clone(),
        };
        let started = backend
            .start(key, &module)
            .and_then(|run| backend.send(key, &activate).map(|()| run));
        match started {
            Ok(run) => {
                slot.state = RunState::Starting;
                slot.session = Some(Session {
                    run,
                    scopes: HashMap::new(),
                    in_flight: 0,
                    backlog: VecDeque::new(),
                    usb_waiting: VecDeque::new(),
                    usb_scopes: HashMap::new(),
                });
            }
            Err(error) => {
                backend.stop(key);
                let reason = format!("start failed: {error}");
                push_log(&mut slot.log, &reason);
                slot.state = RunState::Faulted(reason);
                outcome
                    .effects
                    .push(notice(format!("{key}: start failed: {error}")));
            }
        }
    }

    /// Stops the session; reservations stay while the installation is enabled.
    pub fn stop(&mut self, key: &str, backend: &mut dyn ExtensionBackend, outcome: &mut Outcome) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        if let Some(session) = slot.session.take() {
            let _ = backend.send(key, &HostMessage::Stop);
            backend.stop(key);
            outcome.usb_replies.extend(session.orphans("disabled"));
        }
        slot.end_session(None);
        if !slot.installation.enabled {
            if !slot.registrations.midi_channels.is_empty() {
                outcome.reservations_changed = true;
            }
            slot.registrations = Registrations::default();
            slot.status_lines.clear();
            slot.state = RunState::Disabled;
        } else if slot.state != RunState::Disabled && !matches!(slot.state, RunState::Faulted(_)) {
            slot.state = RunState::Disabled;
        }
    }

    pub fn set_enabled(
        &mut self,
        key: &str,
        enabled: bool,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let Some(slot) = self.slots.get_mut(key) else {
            return outcome;
        };
        slot.installation.enabled = enabled;
        // DSP blocks follow the enabled switch.
        outcome.community_changed = !slot.installation.blocks.is_empty();
        if enabled {
            if matches!(slot.state, RunState::Faulted(_)) {
                slot.state = RunState::Disabled;
            }
            self.start(key, backend, &mut outcome);
        } else {
            self.stop(key, backend, &mut outcome);
        }
        outcome
    }

    /// RETRY: a new session for a faulted installation.
    pub fn retry(&mut self, key: &str, backend: &mut dyn ExtensionBackend) -> Outcome {
        let mut outcome = Outcome::default();
        if let Some(slot) = self.slots.get_mut(key) {
            slot.session = None;
            slot.end_session(None);
            slot.state = RunState::Disabled;
        }
        backend.stop(key);
        self.start(key, backend, &mut outcome);
        outcome
    }

    /// New configuration: the session restarts with it (no seamless reconfiguration in v1).
    pub fn set_config(
        &mut self,
        key: &str,
        config: Json,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let Some(slot) = self.slots.get_mut(key) else {
            return outcome;
        };
        slot.installation.config = config;
        if slot.installation.enabled {
            self.stop(key, backend, &mut outcome);
            if let Some(slot) = self.slots.get_mut(key) {
                slot.state = RunState::Disabled;
            }
            self.start(key, backend, &mut outcome);
        }
        outcome
    }

    /// A command button.
    pub fn run_command(
        &mut self,
        key: &str,
        command: &str,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let known = self
            .slots
            .get(key)
            .is_some_and(|slot| slot.registrations.commands.iter().any(|c| c == command));
        if known {
            self.deliver(
                key,
                Event::Command { id: command.into() },
                backend,
                &mut outcome,
            );
        }
        outcome
    }

    /// A message from a computer over USB for the extension with manifest ID or key `id`. Its
    /// handler's answer comes back in [`Outcome::usb_replies`] under `request`.
    pub fn usb_message(
        &mut self,
        id: &str,
        request: u32,
        message: Json,
        backend: &mut dyn ExtensionBackend,
    ) -> Result<Outcome, String> {
        let (key, slot) = (self.slots.iter_mut())
            .find(|(key, slot)| *key == id || slot.installation.id == id)
            .ok_or_else(|| format!("no extension {id} is installed"))?;
        if !slot.installation.capabilities.usb {
            return Err(format!("{id} does not have the usb capability"));
        }
        let session = (slot.session.as_mut())
            .filter(|_| slot.state == RunState::Running)
            .ok_or_else(|| format!("{id} is not running"))?;
        if !slot.registrations.usb {
            return Err(format!("{id} has no USB handler"));
        }
        // A message over the runner frame limit would fault the extension when it is sent: it
        // is refused here, and the extension keeps running.
        let event = Event::Usb { message };
        let frame = HostMessage::Event {
            scope: u64::MAX,
            event: event.clone(),
        };
        tetrad_extensions::protocol::encode(&frame)
            .map_err(|error| format!("the message is too large for an extension: {error}"))?;
        session.usb_waiting.push_back(request);
        let key = key.clone();
        let mut outcome = Outcome::default();
        self.deliver(&key, event, backend, &mut outcome);
        Ok(outcome)
    }

    /// A message on a reserved channel. Without a running owner it is dropped.
    pub fn on_midi(
        &mut self,
        message: &MidiMessage,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let Some(event) = midi_event(message) else {
            return outcome;
        };
        let channel = match event {
            MidiEvent::ControlChange { channel, .. }
            | MidiEvent::ProgramChange { channel, .. }
            | MidiEvent::NoteOn { channel, .. }
            | MidiEvent::NoteOff { channel, .. } => channel,
        };
        let owner = (self.slots.iter())
            .find(|(_, slot)| {
                slot.installation.enabled
                    && slot.installation.capabilities.midi_receive
                    && slot.registrations.midi_channels.contains(&channel)
            })
            .map(|(key, _)| key.clone());
        if let Some(key) = owner {
            self.deliver(&key, Event::Midi { message: event }, backend, &mut outcome);
        }
        outcome
    }

    /// A preset load: every open scope becomes stale and running sessions hear about it.
    pub fn preset_loaded(&mut self, backend: &mut dyn ExtensionBackend) -> Outcome {
        self.generation += 1;
        let mut outcome = Outcome::default();
        let keys: Vec<String> = self.slots.keys().cloned().collect();
        for key in keys {
            if self.slots[&key].state == RunState::Running {
                self.deliver(&key, Event::PresetLoaded, backend, &mut outcome);
            }
        }
        outcome
    }

    /// A native edit (never one an extension made).
    pub fn control_changed(
        &mut self,
        change: ControlChange,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let keys: Vec<String> = (self.slots.iter())
            .filter(|(_, slot)| {
                slot.registrations.control_changes && slot.installation.capabilities.controls_read
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.deliver(
                &key,
                Event::ControlChanged {
                    change: change.clone(),
                },
                backend,
                &mut outcome,
            );
        }
        outcome
    }

    /// Sends `event` in a new scope, or holds it while too many events are in flight. Held
    /// continuous CC input keeps only its latest value; other input stays ordered.
    fn deliver(
        &mut self,
        key: &str,
        event: Event,
        backend: &mut dyn ExtensionBackend,
        outcome: &mut Outcome,
    ) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        if slot.state != RunState::Running {
            return;
        }
        let Some(session) = slot.session.as_mut() else {
            return;
        };
        if session.in_flight >= IN_FLIGHT || !session.backlog.is_empty() {
            if let Event::Midi {
                message: MidiEvent::ControlChange { channel, controller, .. },
            } = event
                && let Some(held) = session.backlog.iter_mut().find(|held| {
                    matches!(held, Event::Midi { message: MidiEvent::ControlChange { channel: c, controller: n, .. } }
                        if *c == channel && *n == controller)
                })
            {
                *held = event;
                return;
            }
            if session.backlog.len() >= BACKLOG {
                self.fault(key, "MIDI input overflow".into(), backend, outcome);
                return;
            }
            session.backlog.push_back(event);
            return;
        }
        self.next_scope += 1;
        let scope = self.next_scope;
        session.open(scope, self.generation, &event);
        if let Err(error) = backend.send(key, &HostMessage::Event { scope, event }) {
            self.fault(key, format!("send failed: {error}"), backend, outcome);
        }
    }

    fn fault(
        &mut self,
        key: &str,
        reason: String,
        backend: &mut dyn ExtensionBackend,
        outcome: &mut Outcome,
    ) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        if let Some(session) = slot.session.take() {
            backend.stop(key);
            outcome.usb_replies.extend(session.orphans(&reason));
        }
        slot.end_session(Some(&reason));
        push_log(&mut slot.log, &format!("fault: {reason}"));
        slot.state = RunState::Faulted(reason.clone());
        if !slot.registrations.midi_channels.is_empty() {
            outcome.reservations_changed = true;
        }
        outcome
            .effects
            .push(notice(format!("{key} stopped: {reason}")));
    }

    /// Runner output from the platform.
    pub fn on_input(
        &mut self,
        input: ExtensionInput,
        state: &State,
        native_channel: Option<u8>,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        let (key, run, output) = match input {
            ExtensionInput::Runner { key, run, output } => (key, run, output),
            ExtensionInput::Job(result) => return self.on_job(result, backend),
        };
        let Some(slot) = self.slots.get_mut(&key) else {
            return outcome;
        };
        if slot
            .session
            .as_ref()
            .is_none_or(|session| session.run != run)
        {
            // Output of a session that was stopped or replaced.
            return outcome;
        }
        match output {
            RunnerOutput::Exited(reason) => {
                self.fault(&key, reason, backend, &mut outcome);
            }
            RunnerOutput::Message(message) => {
                self.on_message(&key, message, state, native_channel, backend, &mut outcome)
            }
        }
        self.submit_jobs(&mut outcome, backend);
        outcome
    }

    fn submit_jobs(&mut self, outcome: &mut Outcome, backend: &mut dyn ExtensionBackend) {
        for job in std::mem::take(&mut outcome.jobs) {
            self.submit(job, backend);
        }
    }

    fn on_message(
        &mut self,
        key: &str,
        message: RunnerMessage,
        state: &State,
        native_channel: Option<u8>,
        backend: &mut dyn ExtensionBackend,
        outcome: &mut Outcome,
    ) {
        match message {
            RunnerMessage::Activated { registrations } => {
                // Registration happens once, during activation; the runner is not trusted.
                if self.slots[key].state != RunState::Starting {
                    self.fault(key, "activated twice".into(), backend, outcome);
                    return;
                }
                if let Err(reason) = self.check_registrations(key, &registrations, native_channel) {
                    self.fault(key, reason, backend, outcome);
                    return;
                }
                let slot = self.slots.get_mut(key).expect("checked");
                if slot.registrations.midi_channels != registrations.midi_channels {
                    outcome.reservations_changed = true;
                }
                slot.registrations = registrations;
                slot.state = RunState::Running;
                self.deliver(key, Event::Running, backend, outcome);
                self.scan_assets(key, backend);
            }
            RunnerMessage::Request {
                id,
                scope,
                method,
                params,
            } => {
                let result = self.call(key, scope, &method, params, state, outcome);
                if let Err(error) = backend.send(key, &HostMessage::Response { id, result }) {
                    self.fault(key, format!("send failed: {error}"), backend, outcome);
                }
            }
            RunnerMessage::Done {
                scope,
                error,
                result,
            } => {
                let slot = self.slots.get_mut(key).expect("checked");
                if let Some(error) = &error
                    && !is_expected_cancellation(error)
                {
                    push_log(&mut slot.log, &format!("handler error: {error}"));
                }
                let Some(session) = slot.session.as_mut() else {
                    return;
                };
                if session.scopes.remove(&scope).is_some() {
                    session.in_flight = session.in_flight.saturating_sub(1);
                }
                if slot.building.as_ref().is_some_and(|(at, _)| *at == scope) {
                    slot.building = None;
                }
                if let Some(request) = session.usb_scopes.remove(&scope) {
                    let result = match error {
                        Some(error) => Err(error),
                        None => Ok(result.unwrap_or(Json::Null)),
                    };
                    outcome.usb_replies.push(UsbReply { request, result });
                }
                while session.in_flight < IN_FLIGHT {
                    let Some(event) = session.backlog.pop_front() else {
                        break;
                    };
                    self.next_scope += 1;
                    let scope = self.next_scope;
                    session.open(scope, self.generation, &event);
                    if let Err(error) = backend.send(key, &HostMessage::Event { scope, event }) {
                        self.fault(key, format!("send failed: {error}"), backend, outcome);
                        return;
                    }
                }
                self.next_build(key, backend, outcome);
            }
            RunnerMessage::Log { level, text } => {
                let slot = self.slots.get_mut(key).expect("checked");
                let line = match level {
                    LogLevel::Info => text,
                    LogLevel::Warn => format!("warning: {text}"),
                    LogLevel::Error => format!("error: {text}"),
                };
                push_log(&mut slot.log, &line);
            }
            RunnerMessage::Fault { message } => self.fault(key, message, backend, outcome),
        }
    }

    /// Channel claims must be in range, requested in the manifest, free of other enabled
    /// installations' claims and not the fixed native channel. Triggers need their capability
    /// and must follow the declaration rules.
    fn check_registrations(
        &self,
        key: &str,
        registrations: &Registrations,
        native_channel: Option<u8>,
    ) -> Result<(), String> {
        let slot = &self.slots[key];
        if registrations.commands.len() > MAX_COMMANDS {
            return Err(format!("registers more than {MAX_COMMANDS} commands"));
        }
        if !registrations.midi_channels.is_empty() && !slot.installation.capabilities.midi_receive {
            return Err("reserves MIDI channels without the midiReceive capability".into());
        }
        if registrations.control_changes && !slot.installation.capabilities.controls_read {
            return Err("watches controls without the controlsRead capability".into());
        }
        if !registrations.blocks.is_empty() && !slot.installation.capabilities.blocks {
            return Err("provides DSP blocks without the blocks capability".into());
        }
        for (i, block) in registrations.blocks.iter().enumerate() {
            let generated =
                (slot.installation.blocks.iter()).any(|b| &b.id == block && b.generated);
            if !generated || registrations.blocks[..i].contains(block) {
                return Err(format!(
                    "provides block {block:?}, which is not a generated block of the manifest or \
                     is provided twice"
                ));
            }
        }
        if registrations.usb && !slot.installation.capabilities.usb {
            return Err("answers USB messages without the usb capability".into());
        }
        let triggers = &registrations.triggers;
        if !triggers.is_empty() && !slot.installation.capabilities.triggers {
            return Err("declares triggers without the triggers capability".into());
        }
        if triggers.len() > MAX_TRIGGERS {
            return Err(format!("declares more than {MAX_TRIGGERS} triggers"));
        }
        for (i, trigger) in triggers.iter().enumerate() {
            if !trigger.is_valid() || triggers[..i].iter().any(|t| t.id == trigger.id) {
                return Err(format!(
                    "trigger {:?} is not valid or not unique",
                    trigger.id
                ));
            }
        }
        for &channel in &registrations.midi_channels {
            if !(1..=16).contains(&channel) {
                return Err(format!("MIDI channel {channel} is out of range"));
            }
            if native_channel == Some(channel) {
                return Err(format!(
                    "MIDI channel {channel} is the QC's own MIDI channel; choose another"
                ));
            }
            let taken = (self.slots.iter()).find(|(other, other_slot)| {
                *other != key
                    && other_slot.installation.enabled
                    && other_slot.registrations.midi_channels.contains(&channel)
            });
            if let Some((other, _)) = taken {
                return Err(format!("MIDI channel {channel} is reserved by {other}"));
            }
        }
        Ok(())
    }

    /// One host call. Device-scoped reads work without a scope; every write needs an open
    /// scope from the current preset generation.
    fn call(
        &mut self,
        key: &str,
        scope: Option<u64>,
        method: &str,
        params: Json,
        state: &State,
        outcome: &mut Outcome,
    ) -> CallResult {
        let generation = self.generation;
        let slot = self.slots.get_mut(key).expect("checked");
        if scope.is_none() && slot.state == RunState::Running {
            // Unscoped calls outside activation have no handler deadline, so a detached chain
            // of them could keep the device loop busy without end.
            return CallResult::error(
                ErrorCode::NotRunning,
                "after activation, calls need a handler scope",
            );
        }
        let capabilities = slot.installation.capabilities;
        let scope_generation = match scope {
            None => None,
            Some(scope) => match slot.session.as_ref().and_then(|s| s.scopes.get(&scope)) {
                Some(&opened) => Some(opened),
                None => {
                    return CallResult::error(ErrorCode::ScopeClosed, "the handler has finished");
                }
            },
        };
        let write = || -> Result<(), CallResult> {
            match scope_generation {
                None => Err(CallResult::error(
                    ErrorCode::NotRunning,
                    "writes need a handler scope",
                )),
                Some(opened) if opened != generation => Err(CallResult::error(
                    ErrorCode::StaleContext,
                    "a different preset is loaded",
                )),
                Some(_) => Ok(()),
            }
        };
        let deny = |what: &str| {
            CallResult::error(
                ErrorCode::PermissionDenied,
                format!("the manifest does not request {what}"),
            )
        };
        match method {
            "controls.list" => {
                if !capabilities.controls_read {
                    return deny("controlsRead");
                }
                CallResult::Ok(controls_list(state))
            }
            "controls.set" | "controls.setBypass" | "controls.toggleBypass" => {
                if !capabilities.controls_edit {
                    return deny("controlsEdit");
                }
                if let Err(refused) = write() {
                    return refused;
                }
                let Some(binding) = params
                    .get("binding")
                    .and_then(|b| serde_json::from_value::<Binding>(b.clone()).ok())
                else {
                    return CallResult::error(ErrorCode::InvalidParams, "binding is not valid");
                };
                match edit(method, &binding, &params, state) {
                    Ok(action) => {
                        outcome.effects.push(Effect::ExtensionAction(action));
                        CallResult::Ok(Json::Null)
                    }
                    Err(refused) => refused,
                }
            }
            "midi.send" => {
                if !capabilities.midi_send {
                    return deny("midiSend");
                }
                if let Err(refused) = write() {
                    return refused;
                }
                match serde_json::from_value::<MidiEvent>(params)
                    .ok()
                    .and_then(midi_message)
                {
                    Some(
                        message @ (MidiMessage::ControlChange { .. }
                        | MidiMessage::ProgramChange { .. }),
                    ) => {
                        outcome.effects.push(Effect::ExtensionMidiOut(message));
                        CallResult::Ok(Json::Null)
                    }
                    Some(_) => CallResult::error(
                        ErrorCode::Unsupported,
                        "only CC and PC output is supported",
                    ),
                    None => CallResult::error(ErrorCode::InvalidParams, "not a valid MIDI message"),
                }
            }
            "storage.get" | "storage.set" | "storage.delete" => {
                if !capabilities.storage {
                    return deny("storage");
                }
                let Some(name) = params.get("key").and_then(Json::as_str) else {
                    return CallResult::error(ErrorCode::InvalidParams, "key must be a string");
                };
                if name.is_empty() || name.len() > MAX_STORAGE_KEY {
                    return CallResult::error(
                        ErrorCode::InvalidParams,
                        "key length must be 1..=128",
                    );
                }
                let storage = &mut slot.installation.storage;
                match method {
                    "storage.get" => {
                        CallResult::Ok(storage.get(name).cloned().unwrap_or(Json::Null))
                    }
                    _ => {
                        if let Err(refused) = write() {
                            return refused;
                        }
                        let previous = if method == "storage.set" {
                            let value = params.get("value").cloned().unwrap_or(Json::Null);
                            storage.insert(name.to_owned(), value)
                        } else {
                            storage.remove(name)
                        };
                        if serde_json::to_vec(storage).map_or(usize::MAX, |b| b.len()) > MAX_STORAGE
                        {
                            match previous {
                                Some(previous) => storage.insert(name.to_owned(), previous),
                                None => storage.remove(name),
                            };
                            return CallResult::error(
                                ErrorCode::LimitExceeded,
                                "storage is full (256 KiB)",
                            );
                        }
                        outcome.storage_changed.push(key.to_owned());
                        CallResult::Ok(Json::Null)
                    }
                }
            }
            "ui.setStatus" => {
                if let Err(refused) = write() {
                    return refused;
                }
                let Some(lines) = params.get("lines").and_then(Json::as_array) else {
                    return CallResult::error(
                        ErrorCode::InvalidParams,
                        "lines must be an array of strings",
                    );
                };
                slot.status_lines = (lines.iter())
                    .take(STATUS_LINES)
                    .map(|line| match line {
                        Json::String(text) => text.chars().take(STATUS_CHARS).collect(),
                        other => other.to_string().chars().take(STATUS_CHARS).collect(),
                    })
                    .collect();
                CallResult::Ok(Json::Null)
            }
            "triggers.fire" => {
                if !capabilities.triggers {
                    return deny("triggers");
                }
                if let Err(refused) = write() {
                    return refused;
                }
                match fired_trigger(key, &slot.registrations.triggers, &params) {
                    Ok(fired) => {
                        outcome.triggers.push(fired);
                        CallResult::Ok(Json::Null)
                    }
                    Err(why) => CallResult::error(ErrorCode::InvalidParams, why),
                }
            }
            PUBLISH_BLOCK => {
                if !capabilities.blocks {
                    return deny("blocks");
                }
                let publish: BlockPublish = match serde_json::from_value(params) {
                    Ok(publish) => publish,
                    Err(error) => {
                        return CallResult::error(ErrorCode::InvalidParams, error.to_string());
                    }
                };
                let building = (slot.building.as_ref()).is_some_and(|(at, file)| {
                    Some(*at) == scope
                        && file.block == publish.block
                        && file.variant == publish.variant
                });
                if !building {
                    return CallResult::error(
                        ErrorCode::InvalidParams,
                        "blocks.publish answers the build of its own handler only",
                    );
                }
                let id = (publish.block.clone(), publish.variant.clone());
                let ir = match publish.outcome {
                    BuildOutcome::Error(error) => Err(error),
                    BuildOutcome::Ir(ir) => serde_json::from_value::<qcdsp::Block>(ir)
                        .map_err(|error| error.to_string())
                        .and_then(|ir| ir.validate().map(|()| ir).map_err(|e| e.to_string())),
                };
                match ir {
                    Ok(ir) => {
                        slot.build_errors.remove(&id);
                        if slot.generated.get(&id) != Some(&ir) {
                            slot.generated.insert(id.clone(), ir.clone());
                            outcome.community_changed = true;
                            if let Some(key) = slot.installation.install_key() {
                                outcome.jobs.push(ExtensionJob::SaveGenerated {
                                    key,
                                    block: id.0,
                                    variant: id.1,
                                    ir: Box::new(ir),
                                });
                            }
                        }
                    }
                    Err(error) => {
                        push_log(
                            &mut slot.log,
                            &format!("block {} ({}) not built: {error}", id.0, id.1),
                        );
                        slot.build_errors.insert(id, error);
                    }
                }
                CallResult::Ok(Json::Null)
            }
            _ => CallResult::error(ErrorCode::UnknownMethod, format!("no host method {method}")),
        }
    }

    /// Asks the store worker for the asset files of the generated blocks of `key`.
    fn scan_assets(&mut self, key: &str, backend: &mut dyn ExtensionBackend) {
        let Some(slot) = self.slots.get(key) else {
            return;
        };
        let blocks: Vec<(String, Vec<String>)> = (slot.installation.blocks.iter())
            .filter(|block| block.generated && slot.registrations.blocks.contains(&block.id))
            .map(|block| {
                let extensions = (block.asset.iter())
                    .flat_map(|asset| asset.extensions.iter().cloned())
                    .collect();
                (block.id.clone(), extensions)
            })
            .collect();
        if blocks.is_empty() {
            return;
        }
        let Some(install) = slot.installation.install_key() else {
            return;
        };
        let job = ExtensionJob::ScanAssets {
            key: install,
            id: slot.installation.id.clone(),
            blocks,
        };
        self.submit(job, backend);
    }

    /// The assets of `key`'s generated blocks: each file is queued for a build, and cached IR of
    /// a file that is gone is removed (only after a complete scan). A file whose build stopped
    /// the extension is not built again until its text changes. A generated block of a
    /// registered provider without an asset spec builds once, as variant `default`.
    fn on_assets(
        &mut self,
        key: &str,
        scan: AssetScan,
        backend: &mut dyn ExtensionBackend,
        outcome: &mut Outcome,
    ) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        for problem in &scan.problems {
            push_log(&mut slot.log, &format!("assets: {problem}"));
        }
        let mut builds: VecDeque<AssetFile> = scan.files.into();
        for block in &slot.installation.blocks {
            if block.generated
                && block.asset.is_none()
                && slot.registrations.blocks.contains(&block.id)
            {
                builds.push_back(AssetFile {
                    block: block.id.clone(),
                    variant: "default".into(),
                    name: String::new(),
                    text: String::new(),
                });
            }
        }
        let mut present = scan.present;
        present.extend(builds.iter().map(|f| (f.block.clone(), f.variant.clone())));
        // A file that changed gets another chance; one that did not stays out.
        slot.stopped_builds.retain(|id, text| {
            builds
                .iter()
                .any(|f| (&f.block, &f.variant) == (&id.0, &id.1) && &f.text == text)
        });
        builds.retain(|f| {
            !slot
                .stopped_builds
                .contains_key(&(f.block.clone(), f.variant.clone()))
        });
        if scan.complete {
            let gone: Vec<(String, String)> = (slot.generated.keys())
                .filter(|id| !present.contains(id))
                .cloned()
                .collect();
            for (block, variant) in gone {
                slot.generated.remove(&(block.clone(), variant.clone()));
                outcome.community_changed = true;
                if let Some(key) = slot.installation.install_key() {
                    outcome.jobs.push(ExtensionJob::RemoveGenerated {
                        key,
                        block,
                        variant,
                    });
                }
            }
            slot.build_errors.retain(|id, _| present.contains(id));
        }
        slot.no_assets = (slot.installation.blocks.iter())
            .filter(|block| block.asset.is_some() && !present.iter().any(|(id, _)| id == &block.id))
            .map(|block| block.id.clone())
            .collect();
        slot.builds = builds;
        self.next_build(key, backend, outcome);
    }

    /// Sends the next queued build when the session is idle. A build goes out only when it
    /// can be sent at once, so the bridge knows its scope.
    fn next_build(&mut self, key: &str, backend: &mut dyn ExtensionBackend, outcome: &mut Outcome) {
        let Some(slot) = self.slots.get_mut(key) else {
            return;
        };
        let idle = slot
            .session
            .as_ref()
            .is_some_and(|session| session.in_flight < IN_FLIGHT && session.backlog.is_empty());
        if slot.building.is_some() || slot.state != RunState::Running || !idle {
            return;
        }
        let Some(file) = slot.builds.pop_front() else {
            return;
        };
        let event = Event::BuildBlock {
            block: file.block.clone(),
            variant: file.variant.clone(),
            asset: AssetText {
                name: file.name.clone(),
                text: file.text.clone(),
            },
        };
        // `deliver` sends at once (the session is idle), in the next scope.
        slot.building = Some((self.next_scope + 1, file));
        self.deliver(key, event, backend, outcome);
    }

    /// Every community block of the enabled installations: static blocks, and blocks built from
    /// assets (their name is the block name and the variant, such as "NAM Plexi").
    pub fn community_sources(&self) -> Vec<super::community::Source> {
        let mut sources = Vec::new();
        for (key, slot) in &self.slots {
            let installation = &slot.installation;
            if !installation.enabled {
                continue;
            }
            for block in &installation.blocks {
                let source =
                    |key: String, name: String, ir: &qcdsp::Block| super::community::Source {
                        key,
                        name,
                        category: block.category,
                        placeholder: block.placeholder,
                        ir: ir.clone(),
                    };
                if !block.generated {
                    if let Some(ir) = installation.block_irs.get(&block.id) {
                        sources.push(source(
                            format!("{key}/{}", block.id),
                            block.name.clone(),
                            ir,
                        ));
                    }
                    continue;
                }
                for ((id, variant), ir) in &slot.generated {
                    if id == &block.id {
                        let name: String = format!("{} {variant}", block.name)
                            .chars()
                            .take(MAX_BLOCK_NAME)
                            .collect();
                        sources.push(source(format!("{key}/{id}#{variant}"), name, ir));
                    }
                }
            }
        }
        sources
    }
}

/// The longest name of a community block in the picker.
const MAX_BLOCK_NAME: usize = 32;

/// The DSP block rows of one installation's page.
fn block_rows(
    key: &str,
    slot: Option<&Entry>,
    blocks: &[BlockSpec],
    community: &super::community::Installed,
) -> Vec<ExtensionBlock> {
    let category = |block: &BlockSpec| category_name(block.category);
    let ready = |status: &super::community::Status, block: &BlockSpec| match &status.model {
        Some(_) => ExtensionBlock {
            name: status.name.clone(),
            detail: format!(
                "{} · {}% DSP",
                category(block),
                status.cpu.unwrap_or_default()
            ),
            ready: true,
        },
        None => ExtensionBlock {
            name: status.name.clone(),
            detail: status.error.clone().unwrap_or_else(|| "not built".into()),
            ready: false,
        },
    };
    let mut rows = Vec::new();
    for block in blocks {
        if !block.generated {
            let status = community.status(&format!("{key}/{}", block.id));
            rows.push(status.map_or_else(
                || ExtensionBlock {
                    name: block.name.clone(),
                    detail: "Off".into(),
                    ready: false,
                },
                |status| ready(status, block),
            ));
            continue;
        }
        let Some(slot) = slot else { continue };
        for (id, variant) in slot.generated.keys().filter(|(id, _)| id == &block.id) {
            if let Some(status) = community.status(&format!("{key}/{id}#{variant}")) {
                rows.push(ready(status, block));
            }
        }
        for ((id, variant), error) in &slot.build_errors {
            if id == &block.id {
                rows.push(ExtensionBlock {
                    name: format!("{} {variant}", block.name),
                    detail: error.clone(),
                    ready: false,
                });
            }
        }
        if slot.no_assets.contains(&block.id)
            && let Some(asset) = &block.asset
        {
            rows.push(ExtensionBlock {
                name: block.name.clone(),
                detail: format!(
                    "Copy {} files ({}) to /media/p4/tetrad/{}/{}",
                    asset.label,
                    asset.extensions.join(", "),
                    super::extension_jobs::ASSETS_DIR,
                    slot.installation.id
                ),
                ready: false,
            });
        }
    }
    rows
}

/// The stock picker name of a block category.
fn category_name(category: qcdsp::Category) -> &'static str {
    use qcdsp::Category::*;
    match category {
        GuitarOverdrive => "Guitar Overdrive",
        GuitarAmplifier => "Guitar Amplifier",
        BassAmplifier => "Bass Amplifier",
        BassOverdrive => "Bass Overdrive",
        Equalizer => "Equalizer",
        Compressor => "Compressor",
        Delay => "Delay",
        Modulation => "Modulation",
        Reverb => "Reverb",
        Wah => "Wah",
        Utility => "Utility",
        Pitch => "Pitch",
        Filter => "Filter",
        Synth => "Synth",
    }
}

/// Checks the params of one `triggers.fire` call from installation `key`.
fn fired_trigger(
    key: &str,
    declared: &[TriggerSpec],
    params: &Json,
) -> Result<FiredTrigger, &'static str> {
    let text = |name| params.get(name).and_then(Json::as_str);
    let trigger = text("trigger").ok_or("trigger must be a string")?;
    if !declared.iter().any(|t| t.id == trigger) {
        return Err("the trigger is not declared");
    }
    let source = text("source")
        .filter(|s| (1..=MAX_TRIGGER_SOURCE).contains(&s.chars().count()))
        .ok_or("source must be 1..=64 characters")?;
    let label = text("label")
        .filter(|l| l.chars().count() <= MAX_TRIGGER_LABEL)
        .ok_or("label must be at most 48 characters")?;
    // JSON numbers are finite; a value too large for f32 clamps to 1.
    let value = (params.get("value").and_then(Json::as_f64)).ok_or("value must be a number")?;
    let continuous =
        (params.get("continuous").and_then(Json::as_bool)).ok_or("continuous must be a boolean")?;
    let pressed = match params.get("pressed") {
        None | Some(Json::Null) => true,
        Some(pressed) => pressed.as_bool().ok_or("pressed must be a boolean")?,
    };
    Ok(FiredTrigger {
        key: key.to_owned(),
        trigger: trigger.to_owned(),
        source: source.to_owned(),
        label: label.to_owned(),
        value: (value as f32).clamp(0.0, 1.0),
        continuous,
        pressed,
    })
}

fn notice(text: String) -> Effect {
    Effect::Notice(Subsystem::Extensions, text)
}

fn push_log(log: &mut VecDeque<String>, line: &str) {
    if log.len() >= LOG_LINES {
        log.pop_front();
    }
    log.push_back(line.chars().take(400).collect());
}

/// `StaleContext` and `ScopeClosed` end handlers on purpose; they are not worth a log line.
fn is_expected_cancellation(error: &str) -> bool {
    error.contains("a different preset is loaded") || error.contains("the handler has finished")
}

fn midi_event(message: &MidiMessage) -> Option<MidiEvent> {
    Some(match *message {
        MidiMessage::ControlChange {
            channel,
            controller,
            value,
        } => MidiEvent::ControlChange {
            channel: channel.number(),
            controller: controller.get(),
            value: value.get(),
        },
        MidiMessage::ProgramChange { channel, program } => MidiEvent::ProgramChange {
            channel: channel.number(),
            program: program.get(),
        },
        MidiMessage::NoteOn {
            channel,
            key,
            velocity,
        } => MidiEvent::NoteOn {
            channel: channel.number(),
            key: key.get(),
            velocity: velocity.get(),
        },
        MidiMessage::NoteOff {
            channel,
            key,
            velocity,
        } => MidiEvent::NoteOff {
            channel: channel.number(),
            key: key.get(),
            velocity: velocity.get(),
        },
        _ => return None,
    })
}

fn midi_message(event: MidiEvent) -> Option<MidiMessage> {
    let channel = |number: u8| Channel::from_number(u32::from(number));
    let data = U7::new;
    Some(match event {
        MidiEvent::ControlChange {
            channel: c,
            controller,
            value,
        } => MidiMessage::ControlChange {
            channel: channel(c)?,
            controller: data(controller)?,
            value: data(value)?,
        },
        MidiEvent::ProgramChange {
            channel: c,
            program,
        } => MidiMessage::ProgramChange {
            channel: channel(c)?,
            program: data(program)?,
        },
        MidiEvent::NoteOn {
            channel: c,
            key,
            velocity,
        } => MidiMessage::NoteOn {
            channel: channel(c)?,
            key: data(key)?,
            velocity: data(velocity)?,
        },
        MidiEvent::NoteOff {
            channel: c,
            key,
            velocity,
        } => MidiMessage::NoteOff {
            channel: channel(c)?,
            key: data(key)?,
            velocity: data(velocity)?,
        },
    })
}

pub fn engine_site(site: Site) -> Option<EngineSite> {
    let row = |index: u8| Row::new(index).ok();
    Some(match site {
        Site::Slot { row: r, column } => EngineSite::Slot(Slot {
            row: row(r)?,
            column: Column::new(column).ok()?,
        }),
        Site::Input { row: r } => EngineSite::InputControl(row(r)?),
        Site::Output { row: r } => EngineSite::OutputControl(row(r)?),
        Site::Splitter { pair } => EngineSite::Splitter(engine_pair(pair)),
        Site::Mixer { pair } => EngineSite::Mixer(engine_pair(pair)),
    })
}

fn engine_pair(pair: Pair) -> tetrad_engine::Pair {
    match pair {
        Pair::Upper => tetrad_engine::Pair::Upper,
        Pair::Lower => tetrad_engine::Pair::Lower,
    }
}

pub fn protocol_site(site: EngineSite) -> Site {
    let pair = |pair| match pair {
        tetrad_engine::Pair::Upper => Pair::Upper,
        tetrad_engine::Pair::Lower => Pair::Lower,
    };
    match site {
        EngineSite::Slot(slot) => Site::Slot {
            row: slot.row.index() as u8,
            column: slot.column.index() as u8,
        },
        EngineSite::InputControl(row) => Site::Input {
            row: row.index() as u8,
        },
        EngineSite::OutputControl(row) => Site::Output {
            row: row.index() as u8,
        },
        EngineSite::Splitter(p) => Site::Splitter { pair: pair(p) },
        EngineSite::Mixer(p) => Site::Mixer { pair: pair(p) },
    }
}

/// Every deployed block of the loaded preset with its editable numeric parameters.
fn controls_list(state: &State) -> Json {
    let (Some(current), Some(specs)) = (&*state.current, &state.specs) else {
        return json!([]);
    };
    let layout = current.deployment.layout();
    let scene = current.deployment.scene();
    let mut sites = Vec::new();
    for row in 0..tetrad_engine::ROWS as u8 {
        let Ok(r) = Row::new(row) else { continue };
        sites.push(EngineSite::InputControl(r));
        for column in 0..tetrad_engine::COLUMNS as u8 {
            if let Ok(column) = Column::new(column) {
                sites.push(EngineSite::Slot(Slot { row: r, column }));
            }
        }
        sites.push(EngineSite::OutputControl(r));
    }
    for pair in tetrad_engine::Pair::ALL {
        sites.push(EngineSite::Splitter(pair));
        sites.push(EngineSite::Mixer(pair));
    }
    let blocks: Vec<Json> = (sites.into_iter())
        .filter_map(|site| {
            let instance = layout.instance(site)?;
            let model = specs.model(instance.model.0);
            let parameters: Vec<Json> = (instance.params.iter().enumerate())
                .filter_map(|(index, value)| {
                    let spec = model?.parameter(index as u32)?;
                    if spec.kind == tetrad_catalog::ParameterType::String || spec.scale.disabled {
                        return None;
                    }
                    Some(json!({ "index": index, "name": spec.name, "value": value.at(scene) }))
                })
                .collect();
            let mut block = serde_json::to_value(protocol_site(site)).ok()?;
            let fields = block.as_object_mut()?;
            fields.insert("model".into(), json!(instance.model.0));
            fields.insert("name".into(), json!(model.map_or("", |m| m.name.as_str())));
            fields.insert("bypassed".into(), json!(instance.bypass.at(scene)));
            fields.insert("parameters".into(), json!(parameters));
            Some(block)
        })
        .collect();
    Json::Array(blocks)
}

/// Resolves `binding` against the loaded preset and builds the native edit.
fn edit(
    method: &str,
    binding: &Binding,
    params: &Json,
    state: &State,
) -> Result<Action, CallResult> {
    let unresolved = |why: &str| CallResult::error(ErrorCode::Unresolved, why.to_owned());
    let (Some(current), Some(specs)) = (&*state.current, &state.specs) else {
        return Err(unresolved("no preset is loaded"));
    };
    let site = engine_site(binding.site).ok_or_else(|| unresolved("no such site"))?;
    let instance = (current.deployment.layout().instance(site))
        .ok_or_else(|| unresolved("the site is empty"))?;
    if instance.model.0 != binding.model {
        return Err(unresolved("a different block is at the site"));
    }
    let scene = current.deployment.scene();
    match (method, binding.control) {
        ("controls.set", ControlKind::Parameter { index }) => {
            let spec = (specs.model(binding.model).and_then(|m| m.parameter(index)))
                .ok_or_else(|| unresolved("the block has no such parameter"))?;
            if spec.kind == tetrad_catalog::ParameterType::String || spec.scale.disabled {
                return Err(CallResult::error(
                    ErrorCode::Unsupported,
                    "this parameter cannot be set",
                ));
            }
            let value = params.get("value").and_then(Json::as_f64).map(|v| v as f32);
            match value {
                Some(value) if value.is_finite() && (0.0..=1.0).contains(&value) => {
                    Ok(Action::SetParameter { site, index, value })
                }
                _ => Err(CallResult::error(
                    ErrorCode::InvalidParams,
                    "value must be a number 0..1",
                )),
            }
        }
        ("controls.setBypass" | "controls.toggleBypass", ControlKind::Bypass) => {
            if !matches!(site, EngineSite::Slot(_)) {
                return Err(CallResult::error(
                    ErrorCode::Unsupported,
                    "only grid blocks can be bypassed",
                ));
            }
            let bypassed = if method == "controls.toggleBypass" {
                !instance.bypass.at(scene)
            } else {
                params
                    .get("bypassed")
                    .and_then(Json::as_bool)
                    .ok_or_else(|| {
                        CallResult::error(ErrorCode::InvalidParams, "bypassed must be a boolean")
                    })?
            };
            Ok(Action::SetBypass { site, bypassed })
        }
        _ => Err(CallResult::error(
            ErrorCode::InvalidParams,
            "the binding's control does not fit this call",
        )),
    }
}

/// The binding of a native edit, for [`Event::ControlChanged`].
pub fn native_change(action: &Action, state: &State) -> Option<ControlChange> {
    let current = state.current.as_ref()?;
    let (site, control, value) = match *action {
        Action::SetParameter { site, index, value } => {
            (site, ControlKind::Parameter { index }, value)
        }
        Action::SetBypass { site, bypassed } | Action::MomentaryBypass { site, bypassed } => {
            (site, ControlKind::Bypass, f32::from(u8::from(bypassed)))
        }
        // Touch edits arrive wrapped for undo.
        Action::Undoable(_, ref action) => return native_change(action, state),
        _ => return None,
    };
    let instance = current.deployment.layout().instance(site)?;
    Some(ControlChange {
        binding: Binding {
            site: protocol_site(site),
            model: instance.model.0,
            control,
        },
        value,
    })
}

impl Extensions {
    /// Loads the store once, after the first preset is up, so extension work never delays
    /// start-up.
    pub fn boot(&mut self, backend: &mut dyn ExtensionBackend) -> Outcome {
        if !self.repositories.load_requested {
            self.repositories.load_requested = true;
            self.submit(ExtensionJob::Load, backend);
        }
        Outcome::default()
    }

    fn submit(&mut self, job: ExtensionJob, backend: &mut dyn ExtensionBackend) {
        self.repositories.jobs += 1;
        backend.submit(job);
    }

    fn sync_installations(&mut self, backend: &mut dyn ExtensionBackend) -> Outcome {
        let installations = self
            .repositories
            .records
            .values()
            .map(installation)
            .collect();
        self.set_installations(installations, backend)
    }

    /// Saves one installation's storage after an extension changed it.
    pub fn save_storage(&mut self, key: &str, backend: &mut dyn ExtensionBackend) {
        let (Some(slot), Ok(install_key)) = (self.slots.get(key), key.parse::<InstallKey>()) else {
            return;
        };
        let storage: serde_json::Map<String, Json> =
            slot.installation.storage.clone().into_iter().collect();
        if let Some(record) = self.repositories.records.get_mut(key) {
            record.storage = storage.clone();
        }
        self.submit(
            ExtensionJob::SaveStorage {
                key: install_key,
                storage,
            },
            backend,
        );
    }

    fn on_job(&mut self, result: JobResult, backend: &mut dyn ExtensionBackend) -> Outcome {
        let repositories = &mut self.repositories;
        repositories.jobs = repositories.jobs.saturating_sub(1);
        let mut effects = Vec::new();
        let outcome = match result {
            JobResult::Loaded {
                sources,
                installed,
                catalogs,
                problems,
            } => {
                repositories.loaded = true;
                repositories.sources = sources;
                for (source, catalog) in catalogs {
                    repositories
                        .source_status
                        .insert(source.clone(), "Saved catalog".into());
                    repositories.catalogs.insert(source, catalog);
                }
                repositories.records = (installed.into_iter())
                    .map(|loaded| (loaded.installed.key.to_string(), loaded))
                    .collect();
                effects.extend(
                    problems
                        .into_iter()
                        .map(|problem| notice(format!("store: {problem}"))),
                );
                let outcome = self.sync_installations(backend);
                self.refresh(backend);
                outcome
            }
            JobResult::Refreshed(results) => {
                for (source, result) in results {
                    match result {
                        Ok(catalog) => {
                            repositories
                                .source_status
                                .insert(source.clone(), "Up to date".into());
                            repositories.catalogs.insert(source, catalog);
                        }
                        Err(error) => {
                            repositories
                                .source_status
                                .insert(source, format!("Refresh failed: {error}"));
                        }
                    }
                }
                repositories.status = None;
                Outcome::default()
            }
            JobResult::Key(source, Ok(key)) => {
                repositories.status = None;
                repositories.pending_trust = Some((source, *key));
                Outcome::default()
            }
            JobResult::Key(source, Err(error)) => {
                repositories.status = Some(format!("{source}: {error}"));
                Outcome::default()
            }
            JobResult::Assets { key, scan } => {
                let mut outcome = Outcome::default();
                self.on_assets(&key.to_string(), scan, backend, &mut outcome);
                self.submit_jobs(&mut outcome, backend);
                outcome
            }
            JobResult::Installed(Ok(loaded)) => {
                let manifest = &loaded.installed.current.manifest;
                let key = &loaded.installed.key;
                let installed = format!("{} {}", manifest.name, manifest.version);
                // The warning leads: a long name is cut at the end of the line.
                // An update of a copy that is off keeps it off: no conflict.
                let duplicates = match loaded.installed.enabled {
                    true => repositories.enabled_elsewhere(&key.source, &key.id),
                    false => Vec::new(),
                };
                repositories.status = Some(if duplicates.is_empty() {
                    format!("Installed {installed}")
                } else {
                    let from = duplicates.join(", ");
                    format!("Also installed from {from}: keep one on ({installed})")
                });
                repositories
                    .records
                    .insert(loaded.installed.key.to_string(), *loaded);
                self.sync_installations(backend)
            }
            JobResult::Removed(key, Ok(())) => {
                repositories.records.remove(&key.to_string());
                repositories.status = Some(format!("Removed {key}"));
                self.sync_installations(backend)
            }
            JobResult::Installed(Err(error))
            | JobResult::Removed(_, Err(error))
            | JobResult::Saved(Err(error)) => {
                repositories.status = Some(error.clone());
                effects.push(notice(error));
                Outcome::default()
            }
            JobResult::Saved(Ok(())) => Outcome::default(),
        };
        let mut outcome = outcome;
        outcome.effects.extend(effects);
        outcome
    }

    fn refresh(&mut self, backend: &mut dyn ExtensionBackend) {
        let sources: Vec<Source> = (self.repositories.sources.iter())
            .filter(|s| s.enabled)
            .cloned()
            .collect();
        if !sources.is_empty() {
            self.repositories.status = Some("Refreshing repositories…".into());
            self.submit(ExtensionJob::Refresh(sources), backend);
        }
    }

    fn save_sources(&mut self, backend: &mut dyn ExtensionBackend) {
        let sources = self.repositories.sources.clone();
        self.submit(ExtensionJob::SaveSources(sources), backend);
    }

    /// A request from the Extensions pages. Unknown keys and sources are ignored: the page
    /// only offers what the last view showed.
    pub fn on_ui(
        &mut self,
        command: ExtensionCommand,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        if !self.repositories.loaded {
            self.repositories.status = Some("Extensions are still loading".into());
            return outcome;
        }
        match command {
            ExtensionCommand::Refresh => self.refresh(backend),
            ExtensionCommand::Install { source, id } => {
                let Ok(source) = source.parse::<SourceId>() else {
                    return outcome;
                };
                self.install(&source, &id, backend);
            }
            ExtensionCommand::Update { key } => {
                if let Ok(key) = key.parse::<InstallKey>() {
                    self.install(&key.source, &key.id, backend);
                }
            }
            ExtensionCommand::SetEnabled { key, enabled } => {
                let Ok(install_key) = key.parse::<InstallKey>() else {
                    return outcome;
                };
                if let Some(record) = self.repositories.records.get_mut(&key) {
                    record.installed.enabled = enabled;
                    outcome = self.set_enabled(&key, enabled, backend);
                    self.submit(
                        ExtensionJob::SetEnabled {
                            key: install_key,
                            enabled,
                        },
                        backend,
                    );
                }
            }
            ExtensionCommand::Remove { key } => {
                let Ok(install_key) = key.parse::<InstallKey>() else {
                    return outcome;
                };
                if let Some(slot) = self.slots.get_mut(&key) {
                    slot.installation.enabled = false;
                }
                self.stop(&key, backend, &mut outcome);
                self.repositories.status = Some(format!("Removing {key}…"));
                self.submit(ExtensionJob::Remove(install_key), backend);
            }
            ExtensionCommand::Retry { key } => outcome = self.retry(&key, backend),
            ExtensionCommand::Run { key, command } => {
                outcome = self.run_command(&key, &command, backend)
            }
            ExtensionCommand::SetSetting {
                key,
                setting,
                value,
            } => {
                outcome = self.set_setting(&key, &setting, value, backend);
            }
            ExtensionCommand::AddSource { repository } => {
                match repository.trim().parse::<SourceId>() {
                    Ok(source) if self.repositories.sources.iter().any(|s| s.source == source) => {
                        self.repositories.status = Some(format!("{source} is already added"));
                    }
                    Ok(source) => {
                        self.repositories.status = Some(format!("Fetching the key of {source}…"));
                        self.submit(ExtensionJob::FetchKey(source), backend);
                    }
                    Err(error) => self.repositories.status = Some(error.to_string()),
                }
            }
            ExtensionCommand::TrustSource { repository, trust } => {
                let pending = self.repositories.pending_trust.take();
                if let Some((source, key)) = pending.filter(|(s, _)| s.to_string() == repository)
                    && trust
                    && !self.repositories.sources.iter().any(|s| s.source == source)
                {
                    let added = Source {
                        source,
                        key,
                        default: false,
                        enabled: true,
                    };
                    self.repositories.sources.push(added.clone());
                    self.save_sources(backend);
                    self.repositories.status = Some(format!("Refreshing {}…", added.source));
                    self.submit(ExtensionJob::Refresh(vec![added]), backend);
                }
            }
            ExtensionCommand::SetSourceEnabled {
                repository,
                enabled,
            } => {
                if let Some(source) = self
                    .repositories
                    .sources
                    .iter_mut()
                    .find(|s| s.source.to_string() == repository)
                {
                    source.enabled = enabled;
                    self.save_sources(backend);
                }
            }
            ExtensionCommand::RemoveSource { repository } => {
                let before = self.repositories.sources.len();
                self.repositories
                    .sources
                    .retain(|s| s.default || s.source.to_string() != repository);
                if self.repositories.sources.len() != before {
                    if let Ok(source) = repository.parse::<SourceId>() {
                        self.repositories.catalogs.remove(&source);
                        self.repositories.source_status.remove(&source);
                    }
                    self.save_sources(backend);
                }
            }
        }
        outcome
    }

    /// Installs or updates to the newest supported version in the source's verified catalog.
    fn install(&mut self, source: &SourceId, id: &str, backend: &mut dyn ExtensionBackend) {
        let found = (self.repositories.sources.iter())
            .find(|s| &s.source == source && s.enabled)
            .cloned()
            .zip(
                self.repositories
                    .catalogs
                    .get(source)
                    .and_then(|c| c.latest(id))
                    .cloned(),
            );
        let Some((source, entry)) = found else {
            self.repositories.status = Some(format!("{id} is not in an enabled repository"));
            return;
        };
        self.repositories.status = Some(format!("Installing {} {}…", entry.name, entry.version));
        let (source, entry) = (Box::new(source), Box::new(entry));
        self.submit(ExtensionJob::Install { source, entry }, backend);
    }

    fn set_setting(
        &mut self,
        key: &str,
        setting: &str,
        value: ExtensionSettingInput,
        backend: &mut dyn ExtensionBackend,
    ) -> Outcome {
        let (Some(record), Ok(install_key)) = (
            self.repositories.records.get(key),
            key.parse::<InstallKey>(),
        ) else {
            return Outcome::default();
        };
        let manifest = &record.installed.current.manifest;
        let Some(spec) = manifest.settings.iter().find(|s| s.key == setting) else {
            return Outcome::default();
        };
        let value = match (&spec.kind, value) {
            (SettingKind::Choice { options, .. }, ExtensionSettingInput::Choice(index)) => {
                match options.get(index) {
                    Some(option) => option.value.clone(),
                    None => return Outcome::default(),
                }
            }
            (SettingKind::Toggle { .. }, ExtensionSettingInput::Toggle(on)) => Json::Bool(on),
            (SettingKind::Number { min, max, .. }, ExtensionSettingInput::Number(n)) => {
                json!(n.clamp(*min, *max))
            }
            _ => return Outcome::default(),
        };
        let mut config = record.config.clone();
        if let Some(fields) = config.as_object_mut() {
            fields.insert(setting.to_owned(), value);
        }
        let config = match manifest.validate_config(&config) {
            Ok(config) => config,
            Err(error) => {
                self.repositories.status = Some(error.to_string());
                return Outcome::default();
            }
        };
        if let Some(record) = self.repositories.records.get_mut(key) {
            record.config = config.clone();
        }
        self.submit(
            ExtensionJob::SaveConfig {
                key: install_key,
                config: config.clone(),
            },
            backend,
        );
        self.set_config(key, config, backend)
    }

    /// The triggers of running installations, for the Mappings page's Trigger list: by
    /// installation key, then in declaration order.
    pub fn declared_triggers(&self) -> Vec<tetrad_ui::ExtensionTriggerChoice> {
        (self.slots.iter())
            .filter(|(_, slot)| slot.state == RunState::Running)
            .flat_map(|(key, slot)| {
                (slot.registrations.triggers.iter()).map(|trigger| {
                    tetrad_ui::ExtensionTriggerChoice {
                        key: key.clone(),
                        trigger: trigger.id.clone(),
                        label: format!("{} · {}", slot.installation.name, trigger.label),
                    }
                })
            })
            .collect()
    }

    /// Themes of the enabled installations, for Settings → Appearance; `None` until the store
    /// is read.
    pub fn themes(&self) -> Option<Vec<super::appearance::ExtensionThemes>> {
        let repositories = &self.repositories;
        repositories.loaded.then(|| {
            (repositories.records.iter())
                .filter(|(_, record)| record.installed.enabled)
                .map(|(key, record)| (key, &record.installed.current.manifest))
                .filter(|(_, manifest)| !manifest.themes.is_empty())
                .map(|(key, manifest)| super::appearance::ExtensionThemes {
                    extension: key.clone(),
                    name: manifest.name.clone(),
                    themes: manifest.themes.clone(),
                })
                .collect()
        })
    }

    /// Enabled installations with a footswitch mode, by key: the first provides the Custom
    /// mode, the others wait (their pages say so).
    fn footswitch_modes(&self) -> impl Iterator<Item = (&String, &Loaded)> {
        (self.repositories.records.iter())
            .filter(|(_, record)| record.installed.enabled)
            .filter(|(_, record)| record.installed.current.manifest.footswitch_mode.is_some())
    }

    /// The installation that provides the Custom footswitch mode.
    pub fn footswitch_mode(&self) -> Option<super::custom::Provider> {
        let (key, record) = self.footswitch_modes().next()?;
        let manifest = &record.installed.current.manifest;
        let mode = manifest.footswitch_mode.as_ref()?;
        Some(super::custom::Provider {
            key: key.clone(),
            stem: record.installed.key.file_stem(),
            extension: manifest.name.clone(),
            name: mode.name.clone(),
            pages: mode.pages.clamp(1, tetrad_ui::CUSTOM_PAGES),
        })
    }

    /// The registered commands of the running extensions, for Custom mode slots.
    pub fn command_choices(&self) -> Vec<tetrad_ui::CommandChoice> {
        let records = &self.repositories.records;
        (self.slots.iter())
            .filter(|(_, slot)| slot.state == RunState::Running)
            .flat_map(|(key, slot)| {
                let manifest = records
                    .get(key)
                    .map(|record| &record.installed.current.manifest);
                (slot.registrations.commands.iter()).map(move |id| {
                    let label = (manifest.into_iter())
                        .flat_map(|manifest| &manifest.commands)
                        .find(|command| &command.id == id)
                        .map_or(id.as_str(), |command| command.label.as_str());
                    let name = manifest.map_or(key.as_str(), |manifest| manifest.name.as_str());
                    tetrad_ui::CommandChoice {
                        key: key.clone(),
                        command: id.clone(),
                        label: format!("{name} · {label}"),
                    }
                })
            })
            .collect()
    }

    /// Everything the Extensions pages show.
    pub fn view(&self, community: &super::community::Installed) -> ExtensionsView {
        let repositories = &self.repositories;
        let provider = self.footswitch_mode();
        let installed = (repositories.records.iter())
            .map(|(key, record)| {
                let manifest = &record.installed.current.manifest;
                let slot = self.slots.get(key);
                let state = match slot.map(|slot| &slot.state) {
                    Some(RunState::Starting) => ExtensionState::Starting,
                    Some(RunState::Running) => ExtensionState::Running,
                    Some(RunState::Faulted(reason)) => ExtensionState::Faulted(reason.clone()),
                    Some(RunState::Disabled) | None => ExtensionState::Disabled,
                };
                let source = &record.installed.key.source;
                let update = (repositories.catalogs.get(source))
                    .and_then(|catalog| catalog.latest(&manifest.id))
                    .filter(|entry| entry.version > manifest.version)
                    .map(|entry| entry.version.to_string());
                let commands = (slot.into_iter())
                    .flat_map(|slot| &slot.registrations.commands)
                    .map(|id| ExtensionButton {
                        id: id.clone(),
                        label: (manifest.commands.iter())
                            .find(|c| &c.id == id)
                            .map_or_else(|| id.clone(), |c| c.label.clone()),
                    })
                    .collect();
                let settings = (manifest.settings.iter())
                    .map(|spec| {
                        let current = record.config.get(&spec.key);
                        let value = match &spec.kind {
                            SettingKind::Choice { options, default } => {
                                ExtensionSettingValue::Choice {
                                    options: options.iter().map(|o| o.label.clone()).collect(),
                                    selected: (options.iter())
                                        .position(|o| Some(&o.value) == current)
                                        .unwrap_or(*default),
                                }
                            }
                            SettingKind::Toggle { default } => ExtensionSettingValue::Toggle(
                                current.and_then(Json::as_bool).unwrap_or(*default),
                            ),
                            SettingKind::Number { min, max, default } => {
                                ExtensionSettingValue::Number {
                                    value: current.and_then(Json::as_i64).unwrap_or(*default),
                                    min: *min,
                                    max: *max,
                                }
                            }
                        };
                        ExtensionSetting {
                            key: spec.key.clone(),
                            label: spec.label.clone(),
                            value,
                        }
                    })
                    .collect();
                InstalledExtension {
                    key: key.clone(),
                    name: manifest.name.clone(),
                    version: manifest.version.to_string(),
                    description: manifest.description.clone(),
                    source: source.to_string(),
                    enabled: record.installed.enabled,
                    state,
                    update,
                    duplicates: match record.installed.enabled {
                        true => repositories.enabled_elsewhere(source, &manifest.id),
                        false => Vec::new(),
                    },
                    commands,
                    settings,
                    status_lines: self.status_lines(key).to_vec(),
                    blocks: block_rows(key, slot, &manifest.blocks, community),
                    log: self.log(key),
                    themes: manifest
                        .themes
                        .iter()
                        .map(|theme| theme.name.clone())
                        .collect(),
                    footswitch_mode: (manifest.footswitch_mode.as_ref()).map(|mode| {
                        let provides = provider.as_ref().is_some_and(|p| &p.key == key);
                        let detail = match &provider {
                            _ if provides => format!(
                                "{} {} · the mode button or Down + Tap switches to it",
                                mode.pages,
                                if mode.pages == 1 { "page" } else { "pages" }
                            ),
                            _ if !record.installed.enabled => {
                                "Turn the extension on to use it".to_owned()
                            }
                            Some(other) => {
                                format!("Not used: {} adds the Custom mode", other.extension)
                            }
                            None => String::new(),
                        };
                        tetrad_ui::ExtensionMode {
                            name: mode.name.clone(),
                            detail,
                            provides,
                        }
                    }),
                }
            })
            .collect();
        let available = (repositories.sources.iter())
            .filter(|source| source.enabled)
            .filter_map(|source| Some((source, repositories.catalogs.get(&source.source)?)))
            .flat_map(|(source, catalog)| {
                let mut ids: Vec<&str> = catalog.extensions.iter().map(|e| e.id.as_str()).collect();
                ids.sort_unstable();
                ids.dedup();
                ids.into_iter()
                    .filter_map(|id| catalog.latest(id))
                    .filter(|entry| {
                        let key = InstallKey {
                            source: source.source.clone(),
                            id: entry.id.clone(),
                        };
                        // Keys compare without case; their strings do not.
                        !(repositories.records.values()).any(|record| record.installed.key == key)
                    })
                    .map(|entry| AvailableExtension {
                        source: source.source.to_string(),
                        id: entry.id.clone(),
                        name: entry.name.clone(),
                        version: entry.version.to_string(),
                        description: entry.description.clone(),
                        duplicates: repositories.enabled_elsewhere(&source.source, &entry.id),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let sources = (repositories.sources.iter())
            .map(|source| ExtensionSource {
                repository: source.source.to_string(),
                default: source.default,
                enabled: source.enabled,
                fingerprint: source.key.fingerprint(),
                status: (repositories.source_status.get(&source.source))
                    .cloned()
                    .unwrap_or_else(|| "Not refreshed".into()),
            })
            .collect();
        ExtensionsView {
            installed,
            available,
            sources,
            pending_trust: (repositories.pending_trust.as_ref()).map(|(source, key)| {
                PendingTrust {
                    repository: source.to_string(),
                    fingerprint: key.fingerprint(),
                }
            }),
            status: repositories.status.clone(),
            busy: repositories.jobs > 0,
        }
    }
}

fn installation(loaded: &Loaded) -> Installation {
    let manifest = &loaded.installed.current.manifest;
    let key = &loaded.installed.key;
    Installation {
        key: key.to_string(),
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.to_string(),
        description: manifest.description.clone(),
        source: key.source.to_string(),
        module: loaded.module.clone(),
        blocks: manifest.blocks.clone(),
        block_irs: loaded.blocks.clone(),
        generated: (loaded.generated.iter())
            .map(|g| ((g.block.clone(), g.variant.clone()), g.ir.clone()))
            .collect(),
        capabilities: Capabilities {
            midi_receive: manifest.has(Capability::MidiReceive),
            midi_send: manifest.has(Capability::MidiSend),
            controls_read: manifest.has(Capability::ControlsRead),
            controls_edit: manifest.has(Capability::ControlsEdit),
            storage: manifest.has(Capability::Storage),
            triggers: manifest.has(Capability::Triggers),
            blocks: manifest.has(Capability::Blocks),
            usb: manifest.has(Capability::Usb),
        },
        config: loaded.config.clone(),
        storage: loaded.storage.clone().into_iter().collect(),
        enabled: loaded.installed.enabled,
    }
}
