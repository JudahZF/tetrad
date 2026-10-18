//! V2 Neural Capture (`NeuralCapture2Controller`, ZenUI VA 0x202fd8).
//!
//! Training runs in the cloud. The device routes audio, polls the job, downloads the product,
//! auditions it A/B against the device under test and saves it. The controller is a reducer: inputs
//! in, effects out.

use crate::entry::{GridEdit, Hardware, LANE_MUTE, LANE_VOLUME, NC2_CAB_SITE};
use crate::proto::{Fields, Malformed, Value};
use crate::session::{INPUT_LEVEL_1, INPUT_LEVEL_2, SaveInfo, input_level};

/// Job, download and delete HTTP timeout (VA 0x20068c).
pub const HTTP_TIMEOUT_MS: u32 = 30_000;
/// Re-poll delay (VA 0x1fe630, 0x200ddc).
pub const POLL_RETRY_MS: u32 = 1_000;
/// Setup meter timer (VA 0x2031f4).
pub const SETUP_METER_MS: u32 = 100;
/// IO parameter persistence single-shot, ms (`QTimer::start(500)` at VA 0x1ff7ac).
pub const SAVE_SETTINGS_MS: u32 = 500;
/// IO parameter vector size (`parameters` indices 0..=14, VA 0x1ffbac).
pub const IO_PARAMETERS: usize = 15;
/// Remote error text for a failed download (VA 0xa18ccc), sent with `error_id` 1.
pub const CORRUPT_DOWNLOAD: &str = "Downloaded capture is corrupt!";
/// Capture 2 player module (`nc2_player`, moduleReport id 14001).
pub const CAPTURE2_MODEL_ID: u32 = 14_001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Nc2State {
    Idle = 0,
    Setup = 1,
    Recording = 2,
    Training = 3,
    AuditionDut = 4,
    AuditionCapture = 5,
    Abort = 6,
}

impl TryFrom<i32> for Nc2State {
    type Error = Nc2Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Idle,
            1 => Self::Setup,
            2 => Self::Recording,
            3 => Self::Training,
            4 => Self::AuditionDut,
            5 => Self::AuditionCapture,
            6 => Self::Abort,
            _ => return Err(Nc2Error::State(value)),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Nc2Error {
    #[error("unknown V2 capture state {0}")]
    State(i32),
    #[error("IO parameter index {0} is outside 0..=14")]
    Parameter(i32),
    #[error("save needs an auditioned capture")]
    NothingToSave,
    #[error("job reply is not valid JSON")]
    Json,
    #[error("Capture2 file is not a valid ObfuscatedGraph")]
    Malformed,
    #[error("unsupported Capture2 version {0}.{1}.{2}")]
    Version(i32, i32, i32),
}

impl From<Malformed> for Nc2Error {
    fn from(_: Malformed) -> Self {
        Self::Malformed
    }
}

/// Job `status` strings (globals near VA 0x15450xx).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    PendingRecording,
    Running,
    Success,
    Error,
    Stopped,
    /// Includes a missing or non-string status.
    Other(String),
}

/// Job reply (lambda VA 0x201118). Parsing is as lenient as stock: wrong types read as
/// empty strings or 0, and only invalid JSON fails the request.
#[derive(Debug, Clone, PartialEq)]
pub struct JobReport {
    pub status: JobStatus,
    /// Integral JSON numbers only, else 0 (VA 0xe84e0). Stock never uses it.
    pub progress: i32,
    /// Empty when absent; required for `success`.
    pub product_id: String,
    /// Logged only: `id`, `statusReason`, `statusTitle`, `startedAt`, `error`.
    pub id: String,
    pub status_reason: String,
    pub status_title: String,
    pub started_at: String,
    pub error: String,
}

impl JobReport {
    pub fn parse(body: &[u8]) -> Result<Self, Nc2Error> {
        let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| Nc2Error::Json)?;
        let text = |key| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let progress = value
            .get("progress")
            .and_then(serde_json::Value::as_f64)
            .filter(|p| p.fract() == 0.0 && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(p))
            .map_or(0, |p| p as i32);
        let status = match text("status").as_str() {
            "pending" => JobStatus::Pending,
            "pending_recording" => JobStatus::PendingRecording,
            "running" => JobStatus::Running,
            "success" => JobStatus::Success,
            "error" => JobStatus::Error,
            "stopped" => JobStatus::Stopped,
            other => JobStatus::Other(other.to_owned()),
        };
        Ok(Self {
            status,
            progress,
            product_id: text("productId"),
            id: text("id"),
            status_reason: text("statusReason"),
            status_title: text("statusTitle"),
            started_at: text("startedAt"),
            error: text("error"),
        })
    }
}

/// `"https://" + cloudDomain + "/capture/v1/jobs/" + jobId` (VA 0x20068c).
pub fn job_url(cloud_domain: &str, job_id: &str) -> String {
    format!("https://{cloud_domain}/capture/v1/jobs/{job_id}")
}

/// Abort deletes the cloud product: `DELETE /api/v1/user/products/<id>` (VA 0x717c94).
pub fn product_url(cloud_domain: &str, product_id: &str) -> String {
    format!("https://{cloud_domain}/api/v1/user/products/{product_id}")
}

/// Product metadata the download returns (`ProductData`, VA 0x204074).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub name: String,
    pub device: i32,
    pub instrument: i32,
}

/// Routing presets requested from DSPController ([`Nc2Routing::edits`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nc2Route {
    /// Every state change: DSP message 0x11 to core 1, index 1 = 1.0 and index 8|9 = 0.0
    /// (`0x511ae4`; the index depends on the hardware variant).
    Reset,
    /// Setup, before `SetNCOutSel`: `0x520498(3,0,0)`, block 23000 parameter 2 = 0.0 at
    /// column 3.
    SetupPrepare,
    /// Setup, after `SetNCOutSel`: the Setup routing and gains (VA 0x1fec1c).
    Setup,
    /// Recording and Training (VA 0x1fea60).
    Recording,
    /// Device under test audible (VA 0x1fed6c): output index 0 at −40 dB, index 2 at 0 dB.
    AuditionDut,
    /// Capture audible (VA 0x1fee14): the swapped gains.
    AuditionCapture,
}

/// `0x64b8f8` mode 1 with skew 1: a value in `min..=max` as a parameter unit.
fn unit(value: f32, min: f32, max: f32) -> f32 {
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// The NC2 route edits on the NC2 preset. Gains are dB set through `0x64b8f8`: lane VOLUME
/// and the row-0 mixer levels on -40..12 dB, the lane-1 input control parameter 3 on
/// -24..24 dB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Nc2Routing {
    /// NeuralCapture2Controller `+0x2c`: the Recording route ran, so the next audition
    /// route first runs the Setup route (VA 0x1fed6c, 0x1fee14).
    recorded: bool,
}

impl Nc2Routing {
    /// `cab_bypass` is the stored `capture2CabsimBypass` (`0x64eb88`) the Setup route uses.
    pub fn edits(
        &mut self,
        route: Nc2Route,
        hardware: Hardware,
        cab_bypass: bool,
    ) -> Vec<GridEdit> {
        let lane = |row, parameter, value| GridEdit::Lane {
            row,
            parameter,
            value,
        };
        let mute = |on: bool| [0, 2, 3].map(|row| lane(row, LANE_MUTE, f32::from(u8::from(on))));
        // Lane 1 input gain, lane 2 volume, lane 2 input port, lane 0 output port, cab bypass.
        let routing = |input_db, volume_db, input, output, bypassed| {
            let mut edits = mute(true).to_vec();
            edits.extend([
                GridEdit::InputControl {
                    row: 1,
                    parameter: 3,
                    value: unit(input_db, -24.0, 24.0),
                },
                lane(2, LANE_VOLUME, unit(volume_db, -40.0, 12.0)),
                GridEdit::InputPort {
                    row: 2,
                    port: input,
                },
                GridEdit::OutputPort {
                    row: 0,
                    port: output,
                },
                GridEdit::Bypass {
                    site: NC2_CAB_SITE,
                    bypassed,
                },
            ]);
            edits.extend(mute(false));
            edits
        };
        // Row-0 mixer parameters 0 and 2.
        let levels = |first_db, second_db| {
            [(0, first_db), (2, second_db)].map(|(parameter, db)| GridEdit::Mixer {
                row: 0,
                parameter,
                value: unit(db, -40.0, 12.0),
            })
        };
        let setup = || routing(0.0, 0.0, 1, 0x13, cab_bypass);
        let mut audition = |first_db, second_db| {
            let mut edits = if std::mem::take(&mut self.recorded) {
                setup()
            } else {
                Vec::new()
            };
            edits.extend(levels(first_db, second_db));
            edits
        };
        match route {
            // 0x202fd8 → 0x511ae4: core 1 mix 1 = 1.0, mix 8 (9 on mini) = 0.0.
            Nc2Route::Reset => hardware
                .start_mixes()
                .map(|(mix, level)| GridEdit::HeadphoneMix { mix, level })
                .to_vec(),
            Nc2Route::SetupPrepare => vec![lane(3, LANE_MUTE, 0.0)],
            Nc2Route::Setup => setup(),
            Nc2Route::Recording => {
                self.recorded = true;
                let mut edits = routing(2.26, -2.26, 8, 10, true);
                edits.extend(levels(-40.0, 0.0));
                edits
            }
            Nc2Route::AuditionDut => audition(-40.0, 0.0),
            Nc2Route::AuditionCapture => audition(0.0, -40.0),
        }
    }
}

/// Remote `NeuralCapture2Message` (type 63) fields the controller sends. No job status
/// or progress is sent; the client sees outcomes through `state` and errors.
#[derive(Debug, Clone, PartialEq)]
pub enum Nc2Remote {
    /// Field 3 (VA 0x123244).
    State(Nc2State),
    /// Fields 9 and 10 (VA 0x123d18).
    Error { id: i32, text: String },
    /// Field 6 echo; indices 1 and 6 carry the Setup input levels.
    Parameter { index: i32, value: f32 },
    /// Field 5 (VA 0x1256dc).
    SaveInfo(SaveInfo),
    /// Field 7 echo (VA 0x123588).
    CabsimBypass(bool),
    /// Remote reset on start-from-scratch (VA 0x122f04).
    Reset,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Nc2Effect {
    Route(Nc2Route),
    /// `ZenHAL::CZenjack::SetNCOutSel(on, 0, 3, 0, 25000)`.
    NcOutSel(bool),
    /// `enableNC2` (VA 0x528de4): the NC2 grid preset `nc2Preset` on or off
    /// ([`crate::entry::NcPreset::Nc2`], [`crate::entry::NC2_ENABLE`]).
    EnableNc2Preset(bool),
    /// Start (`Some(ms)`) or stop the Setup meter timer.
    MeterTimer(Option<u32>),
    /// Setup meter tick (VA 0x1fe880): request meters 0x4001 then 0x4000.
    RequestInputMeters,
    InputLevel {
        channel: u8,
        level: f32,
    },
    GetJob {
        url: String,
        timeout_ms: u32,
    },
    SchedulePoll {
        delay_ms: u32,
    },
    /// Product service (VA 0x7061b0): `GET /api/v1/products/<id>/download` (base64
    /// payload), `GET /api/v1/products/<id>`, decrypt with the injected key, then
    /// `Capture2File::parse`. Answer with `Nc2Input::Downloaded`.
    DownloadProduct {
        product_id: String,
    },
    DeleteProduct {
        url: String,
        timeout_ms: u32,
    },
    /// Send `Capture2File::sharc_blob` to the Capture 2 block (VA 0x515a64): ReceiveData
    /// clear, then the blob, then ReceiveDataEnd 0x17.
    LoadAudition,
    DropAudition,
    /// Add the auditioned product to library collection 2 (VA 0x1fef08).
    SaveProduct {
        product_id: String,
    },
    /// Apply one IO setting (VA 0x1ffbac; index table in the slice doc).
    IoParameter {
        index: usize,
        value: f32,
    },
    /// (Re)start the IO settings persistence timer.
    SaveSettings {
        delay_ms: u32,
    },
    /// Cab block (0,3) bypass; persist `capture2CabsimBypass` (VA 0x1fe5a4).
    CabsimBypass(bool),
    /// Hide the A/B dialog (`hideDialog` VA 0x3b8320). UI only: the NC2 state, timer and
    /// preset stay as they are.
    HideAbDialog,
    Remote(Nc2Remote),
}

/// Inputs from the remote client, timers, HTTP and the product service.
#[derive(Debug, Clone, PartialEq)]
pub enum Nc2Input {
    /// Action CREATE (0): save the auditioned capture (VA 0x1fef08).
    Save,
    /// Remote `state`: the current state is ignored, 4↔5 is a plain switch and other
    /// changes first hide the A/B dialog if it exists (VA 0x1201bc).
    SetState(i32),
    /// Remote `job_id` → `setTrainingJobId` (VA 0x200b68).
    SetJobId(String),
    /// First `parameters` entry only.
    Parameter {
        index: i32,
        value: f32,
    },
    /// Remote `cabsim_bypass`. `current` is the bypass of grid block (0,3) (VA 0x51334c);
    /// an unchanged value is neither applied nor echoed (VA 0x1201bc, 0x1fe5a4).
    CabsimBypass {
        bypass: bool,
        current: bool,
    },
    /// Remote `save_info`: stored and echoed on reconnect, not used to save (VA 0x1fe858).
    SaveInfo(SaveInfo),
    StartFromScratch,
    Abort,
    /// Product cleanup, then start from scratch (VA 0x203ee4).
    Discard,
    /// The UI showed the NC2 dialog. `showDialog` (VA 0x3d791c) creates the A/B dialog
    /// once; stock never deletes it.
    DialogShown,
    /// `showDialog` (VA 0x3d791c) in full: the A/B dialog exists, then the NC2 open
    /// (VA 0x203af0): job and product dropped, the NC2 preset enabled (`enableNC2(1)`),
    /// then state 1 even when it is already 1 (`+0x68 = -1` first). Remote `open_dialog`
    /// (VA 0x1201bc) shows this dialog. Send [`Nc2Input::RestoreIo`] next.
    Open,
    /// The open's IO restore (VA 0x1ffe80 → 0x1ffcf8): `(index, value)` from
    /// `[NeuralCapture2ControllerSettings]` in stock order. Each is kept, applied and echoed.
    RestoreIo(Vec<(usize, f32)>),
    /// Dialog close (VA 0x203ca4).
    Close,
    /// Control connection or internet restored (VA 0x1fe3d0, 0x1fe4b8).
    Reconnected,
    PollDue,
    /// HTTP body of the job GET.
    JobReply(Vec<u8>),
    /// Transport failure. `online`: internet status 2 or a control connection (VA 0x200ddc).
    JobFailed {
        online: bool,
    },
    /// Product download result; `None` means it failed or is corrupt (VA 0x2044c8).
    Downloaded(Option<Product>),
    MeterTick,
    Meter {
        meter: u16,
        value: f32,
    },
}

/// Remote `NeuralCapture2Message` UPDATE fields the inbound handler reads (VA 0x1201bc).
/// `bool` fields are true only when the field is present and true.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Nc2Update {
    pub state: Option<i32>,
    pub open_dialog: bool,
    pub job_id: Option<String>,
    pub parameters: Vec<(i32, f32)>,
    pub cabsim: bool,
    pub cabsim_model_preset: bool,
    pub cabsim_bypass: Option<bool>,
    pub save_info: Option<SaveInfo>,
    pub start_from_scratch: bool,
}

/// What one UPDATE message asks for. Stock handles only the first present field.
#[derive(Debug, Clone, PartialEq)]
pub enum Nc2Request {
    /// Show the dialog (stock never hides it here); the UI owns it.
    OpenDialog,
    Input(Nc2Input),
    /// Cab model change; the grid service owns it (VA 0x200428).
    Cabsim,
    /// Cab model-preset change; the grid service owns it.
    CabsimModelPreset,
    /// Read the bypass of grid block (0,3), then send `Nc2Input::CabsimBypass`.
    CabsimBypass(bool),
}

impl Nc2Update {
    /// Stock order (VA 0x1201bc, has-bits): state 0x40, open_dialog 0x200, job_id 1,
    /// `parameters[0]`, then cabsim 8 before cabsim_model_preset 0x10, cabsim_bypass 0x100,
    /// save_info 4 and start_from_scratch.
    pub fn request(self) -> Option<Nc2Request> {
        let input = if let Some(state) = self.state {
            Nc2Input::SetState(state)
        } else if self.open_dialog {
            return Some(Nc2Request::OpenDialog);
        } else if let Some(job_id) = self.job_id {
            Nc2Input::SetJobId(job_id)
        } else if let Some(&(index, value)) = self.parameters.first() {
            Nc2Input::Parameter { index, value }
        } else if self.cabsim {
            return Some(Nc2Request::Cabsim);
        } else if self.cabsim_model_preset {
            return Some(Nc2Request::CabsimModelPreset);
        } else if let Some(bypass) = self.cabsim_bypass {
            return Some(Nc2Request::CabsimBypass(bypass));
        } else if let Some(info) = self.save_info {
            Nc2Input::SaveInfo(info)
        } else if self.start_from_scratch {
            Nc2Input::StartFromScratch
        } else {
            return None;
        };
        Some(Nc2Request::Input(input))
    }
}

/// `NeuralCapture2Controller` state, job and product identity.
#[derive(Debug, Clone)]
pub struct Nc2Controller {
    cloud_domain: String,
    state: Nc2State,
    job_id: String,
    product_id: String,
    in_flight: bool,
    product: Option<Product>,
    save_info: Option<SaveInfo>,
    parameters: [Option<f32>; IO_PARAMETERS],
    levels: [Option<f32>; 2],
    report: Option<JobReport>,
    /// `NeuralCapture2Dialog+0x148` (VA 0x3d750c) is set.
    ab_dialog: bool,
    /// The save emitted `newCaptureSaved()` (VA 0x9728ac at the end of 0x1fef08); the dialog
    /// shows mode 3 (VA 0x3d7fc8) until the next `ncStateChanged()` (VA 0x3dcc2c).
    saved: bool,
}

impl Nc2Controller {
    pub fn new(cloud_domain: impl Into<String>) -> Self {
        Self {
            cloud_domain: cloud_domain.into(),
            state: Nc2State::Idle,
            job_id: String::new(),
            product_id: String::new(),
            in_flight: false,
            product: None,
            save_info: None,
            parameters: [None; IO_PARAMETERS],
            levels: [None; 2],
            report: None,
            ab_dialog: false,
            saved: false,
        }
    }

    pub fn state(&self) -> Nc2State {
        self.state
    }
    /// The NC2 dialog shows "Neural Capture Saved." (mode 3, VA 0x3d7dc0).
    pub fn saved(&self) -> bool {
        self.saved
    }
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
    pub fn product_id(&self) -> &str {
        &self.product_id
    }
    /// The auditioned product, between download and save or cleanup.
    pub fn product(&self) -> Option<&Product> {
        self.product.as_ref()
    }
    pub fn report(&self) -> Option<&JobReport> {
        self.report.as_ref()
    }

    pub fn apply(&mut self, input: Nc2Input) -> Result<Vec<Nc2Effect>, Nc2Error> {
        let mut effects = Vec::new();
        match input {
            Nc2Input::Save => {
                let product = self.product.take().ok_or(Nc2Error::NothingToSave)?;
                effects.push(Nc2Effect::SaveProduct {
                    product_id: std::mem::take(&mut self.product_id),
                });
                effects.push(Nc2Effect::Remote(Nc2Remote::SaveInfo(SaveInfo {
                    name: product.name,
                    device: product.device,
                    instrument: product.instrument,
                })));
                effects.push(Nc2Effect::DropAudition);
                self.saved = true;
            }
            Nc2Input::SetState(value) => {
                let target = Nc2State::try_from(value)?;
                // VA 0x1201bc returns before the dialog close when the state is unchanged.
                if target == self.state {
                    return Ok(effects);
                }
                let audition = [Nc2State::AuditionDut, Nc2State::AuditionCapture];
                if self.ab_dialog && !(audition.contains(&self.state) && audition.contains(&target))
                {
                    effects.push(Nc2Effect::HideAbDialog);
                }
                self.enter(target, &mut effects);
            }
            Nc2Input::SetJobId(job_id) => {
                self.job_id = job_id;
                self.poll(&mut effects);
            }
            Nc2Input::Parameter { index, value } => {
                let slot = usize::try_from(index)
                    .ok()
                    .filter(|&i| i < IO_PARAMETERS)
                    .ok_or(Nc2Error::Parameter(index))?;
                if changed(self.parameters[slot], value) {
                    self.parameters[slot] = Some(value);
                    self.apply_io(slot, value, &mut effects);
                    effects.push(Nc2Effect::SaveSettings {
                        delay_ms: SAVE_SETTINGS_MS,
                    });
                }
            }
            Nc2Input::RestoreIo(stored) => {
                for (slot, value) in stored {
                    if slot < IO_PARAMETERS {
                        self.parameters[slot] = Some(value);
                        self.apply_io(slot, value, &mut effects);
                    }
                }
            }
            Nc2Input::Open => {
                self.ab_dialog = true;
                if self.product.take().is_some() {
                    effects.push(Nc2Effect::DropAudition);
                }
                self.product_id.clear();
                self.job_id.clear();
                self.in_flight = false;
                self.report = None;
                effects.push(Nc2Effect::EnableNc2Preset(true));
                self.transition(Nc2State::Setup, &mut effects);
            }
            Nc2Input::CabsimBypass { bypass, current } => {
                if bypass != current {
                    effects.push(Nc2Effect::CabsimBypass(bypass));
                    effects.push(Nc2Effect::Remote(Nc2Remote::CabsimBypass(bypass)));
                }
            }
            Nc2Input::SaveInfo(info) => self.save_info = Some(info),
            Nc2Input::StartFromScratch => self.start_from_scratch(&mut effects),
            Nc2Input::Abort => {
                self.release_product(&mut effects);
                self.in_flight = false;
                self.enter(Nc2State::Abort, &mut effects);
            }
            Nc2Input::Discard => {
                self.release_product(&mut effects);
                self.start_from_scratch(&mut effects);
            }
            Nc2Input::DialogShown => self.ab_dialog = true,
            Nc2Input::Close => self.close(&mut effects),
            Nc2Input::Reconnected => {
                if let Some(info) = &self.save_info {
                    effects.push(Nc2Effect::Remote(Nc2Remote::SaveInfo(info.clone())));
                }
                if self.state == Nc2State::Training && !self.in_flight {
                    effects.push(Nc2Effect::SchedulePoll {
                        delay_ms: POLL_RETRY_MS,
                    });
                }
            }
            Nc2Input::PollDue => self.poll(&mut effects),
            Nc2Input::JobReply(body) => {
                self.in_flight = false;
                match JobReport::parse(&body) {
                    Ok(report) => self.job_report(report, &mut effects),
                    Err(_) => self.job_failed(true, &mut effects),
                }
            }
            Nc2Input::JobFailed { online } => {
                self.in_flight = false;
                self.job_failed(online, &mut effects);
            }
            // VA 0x204074: audition, then 5 even if the load fails (only logged).
            Nc2Input::Downloaded(Some(product)) => {
                self.product = Some(product);
                effects.push(Nc2Effect::LoadAudition);
                self.enter(Nc2State::AuditionCapture, &mut effects);
            }
            Nc2Input::Downloaded(None) => self.download_failed(&mut effects),
            Nc2Input::MeterTick if self.state == Nc2State::Setup => {
                effects.push(Nc2Effect::RequestInputMeters);
            }
            Nc2Input::Meter { meter, value }
                if self.state == Nc2State::Setup
                    && (meter == INPUT_LEVEL_1 || meter == INPUT_LEVEL_2) =>
            {
                self.level(meter, input_level(value), &mut effects);
            }
            Nc2Input::MeterTick | Nc2Input::Meter { .. } => {}
        }
        Ok(effects)
    }

    /// One IO parameter (VA 0x1ff730, 0x1ff190): applied, except the level echoes 1 and 6,
    /// then echoed. Impedances 13 and 14 echo their step (`lround(v * 8) / 8`, 9 steps).
    fn apply_io(&mut self, slot: usize, value: f32, effects: &mut Vec<Nc2Effect>) {
        if slot != 1 && slot != 6 {
            effects.push(Nc2Effect::IoParameter { index: slot, value });
        }
        let echo = if matches!(slot, 13 | 14) {
            (value * 8.0).round() / 8.0
        } else {
            value
        };
        effects.push(Nc2Effect::Remote(Nc2Remote::Parameter {
            index: slot as i32,
            value: echo,
        }));
    }

    /// State entry effects (VA 0x202fd8). Same-state calls are no-ops.
    fn enter(&mut self, target: Nc2State, effects: &mut Vec<Nc2Effect>) {
        if target != self.state {
            self.transition(target, effects);
        }
    }

    fn transition(&mut self, target: Nc2State, effects: &mut Vec<Nc2Effect>) {
        self.state = target;
        self.saved = false;
        effects.push(Nc2Effect::Route(Nc2Route::Reset));
        effects.push(Nc2Effect::MeterTimer(None));
        match target {
            Nc2State::Idle => {}
            Nc2State::Setup => {
                effects.push(Nc2Effect::Route(Nc2Route::SetupPrepare));
                effects.push(Nc2Effect::NcOutSel(true));
                effects.push(Nc2Effect::Route(Nc2Route::Setup));
                effects.push(Nc2Effect::Route(Nc2Route::AuditionDut));
                effects.push(Nc2Effect::MeterTimer(Some(SETUP_METER_MS)));
            }
            Nc2State::Recording | Nc2State::Training => {
                effects.push(Nc2Effect::Route(Nc2Route::Recording));
            }
            Nc2State::AuditionDut => effects.push(Nc2Effect::Route(Nc2Route::AuditionDut)),
            Nc2State::AuditionCapture => {
                effects.push(Nc2Effect::Route(Nc2Route::AuditionCapture));
            }
            Nc2State::Abort => effects.push(Nc2Effect::NcOutSel(false)),
        }
        effects.push(Nc2Effect::Remote(Nc2Remote::State(target)));
    }

    /// `startProgressCheck` (VA 0x20068c).
    fn poll(&mut self, effects: &mut Vec<Nc2Effect>) {
        if self.in_flight || self.job_id.is_empty() {
            return;
        }
        self.in_flight = true;
        effects.push(Nc2Effect::GetJob {
            url: job_url(&self.cloud_domain, &self.job_id),
            timeout_ms: HTTP_TIMEOUT_MS,
        });
    }

    /// `progressRequestSucceeded` (VA 0x1fe630).
    fn job_report(&mut self, report: JobReport, effects: &mut Vec<Nc2Effect>) {
        if matches!(self.state, Nc2State::Recording | Nc2State::Training) {
            match report.status {
                JobStatus::Pending | JobStatus::PendingRecording | JobStatus::Running => {
                    effects.push(Nc2Effect::SchedulePoll {
                        delay_ms: POLL_RETRY_MS,
                    });
                }
                // Stock downloads with an empty id and fails with "Empty product id!".
                JobStatus::Success if report.product_id.is_empty() => {
                    self.download_failed(effects);
                }
                JobStatus::Success => {
                    self.product_id = report.product_id.clone();
                    effects.push(Nc2Effect::DownloadProduct {
                        product_id: self.product_id.clone(),
                    });
                }
                _ => {}
            }
        }
        self.report = Some(report);
    }

    /// `progressRequestFailed` (VA 0x200ddc).
    fn job_failed(&mut self, online: bool, effects: &mut Vec<Nc2Effect>) {
        if online && self.state == Nc2State::Training && !self.in_flight {
            effects.push(Nc2Effect::SchedulePoll {
                delay_ms: POLL_RETRY_MS,
            });
        }
    }

    /// VA 0x2044c8.
    fn download_failed(&mut self, effects: &mut Vec<Nc2Effect>) {
        effects.push(Nc2Effect::Remote(Nc2Remote::Error {
            id: 1,
            text: CORRUPT_DOWNLOAD.to_owned(),
        }));
        self.enter(Nc2State::Setup, effects);
    }

    /// Product cleanup shared by abort and discard (VA 0x2038cc).
    fn release_product(&mut self, effects: &mut Vec<Nc2Effect>) {
        if !self.product_id.is_empty() {
            effects.push(Nc2Effect::DeleteProduct {
                url: product_url(&self.cloud_domain, &self.product_id),
                timeout_ms: HTTP_TIMEOUT_MS,
            });
            self.product_id.clear();
        }
        if self.product.take().is_some() {
            effects.push(Nc2Effect::DropAudition);
        }
    }

    /// VA 0x203d40.
    fn start_from_scratch(&mut self, effects: &mut Vec<Nc2Effect>) {
        effects.push(Nc2Effect::Remote(Nc2Remote::Reset));
        if self.product.take().is_some() {
            effects.push(Nc2Effect::DropAudition);
        }
        self.product_id.clear();
        self.job_id.clear();
        self.in_flight = false;
        self.report = None;
        self.enter(Nc2State::Setup, effects);
    }

    /// VA 0x203ca4: → 0, stop the timer, disable the NC2 preset.
    fn close(&mut self, effects: &mut Vec<Nc2Effect>) {
        self.enter(Nc2State::Idle, effects);
        effects.push(Nc2Effect::MeterTimer(None));
        effects.push(Nc2Effect::EnableNc2Preset(false));
    }

    fn level(&mut self, meter: u16, level: f32, effects: &mut Vec<Nc2Effect>) {
        let (channel, index) = if meter == INPUT_LEVEL_1 {
            (1, 1)
        } else {
            (2, 6)
        };
        effects.push(Nc2Effect::InputLevel { channel, level });
        let last = &mut self.levels[usize::from(channel - 1)];
        if changed(*last, level) {
            *last = Some(level);
            effects.push(Nc2Effect::Remote(Nc2Remote::Parameter {
                index,
                value: level,
            }));
        }
    }
}

/// Stock relative 1e-5 change test before applying or echoing a parameter.
fn changed(last: Option<f32>, value: f32) -> bool {
    last.is_none_or(|last| (last - value).abs() > last.abs().min(value.abs()) * 1e-5)
}

/// Decrypted Capture 2 file: protobuf `snng_obfuscate.ObfuscatedGraph` (VA 0x6f2b60).
/// `data` is opaque on ARM; only the SHARC `nc2_player` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture2File<'a> {
    pub metadata: Vec<&'a str>,
    pub identity: u32,
    pub data: &'a [u8],
}

impl<'a> Capture2File<'a> {
    /// Stock accepts only version 0.1.0.
    pub const VERSION: (i32, i32, i32) = (0, 1, 0);

    pub fn parse(bytes: &'a [u8]) -> Result<Self, Nc2Error> {
        let mut version = (0, 0, 0);
        let mut file = Self {
            metadata: Vec::new(),
            identity: 0,
            data: &[],
        };
        for field in Fields(bytes) {
            match field? {
                (1, Value::Bytes(message)) => version = Self::version(message)?,
                (2, Value::Bytes(text)) => file
                    .metadata
                    .push(std::str::from_utf8(text).map_err(|_| Nc2Error::Malformed)?),
                (3, Value::Fixed32(identity)) => file.identity = identity,
                (4, Value::Bytes(data)) => file.data = data,
                (1..=4, _) => return Err(Nc2Error::Malformed),
                _ => {}
            }
        }
        if version != Self::VERSION {
            return Err(Nc2Error::Version(version.0, version.1, version.2));
        }
        Ok(file)
    }

    fn version(message: &[u8]) -> Result<(i32, i32, i32), Nc2Error> {
        let mut version = (0, 0, 0);
        for field in Fields(message) {
            match field? {
                // proto3 int32: negative values use ten-byte varints; truncation is the rule.
                (1, Value::Varint(v)) => version.0 = v as i32,
                (2, Value::Varint(v)) => version.1 = v as i32,
                (3, Value::Varint(v)) => version.2 = v as i32,
                (1..=3, _) => return Err(Nc2Error::Malformed),
                _ => {}
            }
        }
        Ok(version)
    }

    /// The bytes sent to the Capture 2 block: identity (LE) then data (VA 0x6f3ac4).
    pub fn sharc_blob(&self) -> Vec<u8> {
        [&self.identity.to_le_bytes()[..], self.data].concat()
    }
}
