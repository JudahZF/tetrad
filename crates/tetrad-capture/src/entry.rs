//! DSPController work around Neural Capture V1 state changes, as typed data.
//!
//! Stock `DSPController::_onNcStateChanged` (ZenUI VA 0x528498) runs synchronously inside every
//! `setNcState` (VA 0x338118). It swaps the active grid for one of three capture presets and mutes
//! or unmutes the outputs. [`DspController`] reproduces its decisions; the app executes the
//! returned [`EntryAction`]s.

use crate::orchestration::NcState;
use tetrad_dsp_protocol::DeploymentTarget;

/// `NC_Recorder` (blob `nc_recorder`): capture player, grid node 1.
pub const RECORDER_MODEL: u32 = 20000;
/// `NC_Trainer` (blob `nc_trainer`): conductor (node 0) and node trainer (node 4).
pub const TRAINER_MODEL: u32 = 20001;
/// `NC_Refiner` (blob `nc_refiner`): refiner and A/B player, grid node 1.
pub const REFINER_MODEL: u32 = 20002;
pub const SPLITTER_MODEL: u32 = 10004;
pub const MIXER_MODEL: u32 = 11000;
/// Lane output control: parameter 2 MUTE, 3 SOLO (VA 0x520498, 0x520540).
pub const LANE_OUTPUT_MODEL: u32 = 23000;
/// Cab the recorder uses before a cab was stored (VA 0x51c780, `0x2ee1`).
pub const DEFAULT_CAB_MODEL: u32 = 12001;
/// The stored cab state is the user default with key `model + 10_000_000` (VA 0x51c780).
pub const CAB_STATE_KEY_OFFSET: u32 = 10_000_000;
/// Grid row and column of the capture cab (VA 0x51c780, 0x51b1ac, 0x528498).
pub const CAB_SITE: (u8, u8) = (1, 4);
/// The NC2 cab (`0x51b7e8`, `0x51334c(.., 0, 3)`).
pub const NC2_CAB_SITE: (u8, u8) = (0, 3);
/// The NC2 cab state is the user default with key `model + 20_000_000` (VA 0x51b7e8,
/// `0x2001d8`).
pub const NC2_CAB_STATE_KEY_OFFSET: u32 = 20_000_000;
/// Capture 2 player (`nc2_player`), NC2 node (0, 0), the audition target (VA 0x515a64).
pub const NC2_PLAYER_MODEL: u32 = 14_001;
/// The FX loop send at (0, 1) of the NC2 preset, bypassed in every scene (VA 0x51b7e8).
pub const NC2_SEND_MODEL: u32 = 13_000;
/// Lane output control (23000) parameters the capture setters write (VA 0x520374, 0x520498,
/// 0x520540).
pub const LANE_VOLUME: u32 = 0;
pub const LANE_MUTE: u32 = 2;
pub const LANE_SOLO: u32 = 3;

/// Hardware variant, stock `0x499a60` (true on Quad Cortex mini, "ATMA").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hardware {
    #[default]
    QuadCortex,
    QuadCortexMini,
}

impl Hardware {
    /// Row-0 output port of the recorder and refiner presets (VA 0x51c780, 0x51b1ac),
    /// and the headphone mix the state-1 tick sets to 0.0 (VA 0x339320).
    pub fn capture_output(self) -> u8 {
        match self {
            Self::QuadCortex => 8,
            Self::QuadCortexMini => 9,
        }
    }

    /// `SetOutputGain` output that capture holds at 1.0 (VA 0x528498 → 0x511a38).
    pub fn monitor_output(self) -> u8 {
        match self {
            Self::QuadCortex => 4,
            Self::QuadCortexMini => 5,
        }
    }

    /// Capture record `platform` (VA 0x497ce0 inside 0x6fa0c4).
    pub fn platform_name(self) -> &'static str {
        match self {
            Self::QuadCortex => "quad",
            Self::QuadCortexMini => "quad_cortex_mini",
        }
    }

    /// The two `SetHeadphoneMixLevel` writes of the state-1 tick, in stock order
    /// (VA 0x339320 → 0x511ae4): mix 1 = 1.0, then mix 8 (9 on mini) = 0.0.
    pub fn start_mixes(self) -> [(u8, f32); 2] {
        [(1, 1.0), (self.capture_output(), 0.0)]
    }
}

/// The capture presets DSPController activates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NcPreset {
    /// States 1–10 (VA 0x51c780).
    Recorder,
    /// State 11 (VA 0x51b5e8).
    Trainer,
    /// States 12–16 (VA 0x51b1ac).
    Refiner,
    /// `nc2Preset` (VA 0x51b7e8, DSPController `+0x7208`), active while the NC2 dialog is
    /// open (`enableNC2` 0x528de4).
    Nc2,
}

/// One capture model on grid row 0. `values` are float parameter indices and values
/// that the builder writes for all 8 scenes; `bypassed` blocks are bypassed in all 8.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NcNode {
    pub column: u8,
    pub model: u32,
    pub values: &'static [(u32, f32)],
    pub bypassed: bool,
}

/// Lane input and output ports (`0x2e434c`). Rows not listed keep new-preset ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanePorts {
    pub row: u8,
    pub input: u32,
    pub output: u32,
}

/// Row-0 splitter/mixer pair (`0x2e4108`) and split points (`0x2e2fb4`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowSplit {
    /// Splitter `TYPE` = the parameter step (`+0x90`) × this index (VA 0x5cc9e0):
    /// step 1 of "Balance, A/B, Crossover".
    pub splitter_type_step: u8,
    /// Splitter `STEREO` (parameter 1) = 1.0, "Split".
    pub splitter_stereo: f32,
    /// Mixer `SPLIT MODE` (parameter 6) = 1.0.
    pub mixer_split_mode: f32,
    /// The `{split, mix}` points stored for row 0.
    pub points: (i32, i32),
}

pub const CAPTURE_SPLIT: RowSplit = RowSplit {
    splitter_type_step: 1,
    splitter_stereo: 1.0,
    mixer_split_mode: 1.0,
    points: (2, 0),
};

/// Model and state of the cab at [`CAB_SITE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CabSource {
    /// The stored cab: model setting (`0x64e868`, else [`DEFAULT_CAB_MODEL`]), state from
    /// the user default `model + CAB_STATE_KEY_OFFSET` when present, and the stored
    /// bypass (`0x64e974`) for all 8 scenes.
    Stored,
    /// The refiner builder places [`DEFAULT_CAB_MODEL`] bypassed in all scenes; entry to
    /// state 12 then puts the recorder's cab block at (1, 4) with its 8 scene bypasses
    /// (VA 0x528498). Stock copies the block's identity, model id, parameters and two more
    /// block fields into the refiner's own block (`0x2ea1c4`..`0x2eec74`).
    RecorderCopy,
    /// The NC2 cab at [`NC2_CAB_SITE`]: model `capture2CabsimId` (`0x64ea80`, else
    /// [`DEFAULT_CAB_MODEL`]), state from the user default `model +`
    /// [`NC2_CAB_STATE_KEY_OFFSET`], and the stored `capture2CabsimBypass` (`0x64eb88`) in
    /// all 8 scenes. Stock builds it bypassed (VA 0x51b7e8); the NC2 open puts the stored
    /// block and bypass in and deploys again (`0x2001d8`, `0x20034c`).
    Nc2Stored,
}

impl CabSource {
    pub fn site(self) -> (u8, u8) {
        match self {
            Self::Nc2Stored => NC2_CAB_SITE,
            Self::Stored | Self::RecorderCopy => CAB_SITE,
        }
    }
}

/// The grid a capture preset adds to a new preset (`0x51a370`).
#[derive(Debug, Clone, PartialEq)]
pub struct NcLayout {
    pub nodes: &'static [NcNode],
    pub ports: Vec<LanePorts>,
    pub split: Option<RowSplit>,
    /// Row-0 `{split, mix}` points without a splitter/mixer edit (`0x2e2fb4` alone).
    pub points: Option<(i32, i32)>,
    pub cab: Option<CabSource>,
}

impl NcPreset {
    pub fn layout(self, hardware: Hardware) -> NcLayout {
        const RECORDER: [NcNode; 1] = [NcNode {
            column: 1,
            model: RECORDER_MODEL,
            values: &[],
            bypassed: false,
        }];
        // Node 4 "SET NODE" (p2) first, then node 0 "SET CONDUCTOR" (p1), as built.
        const TRAINER: [NcNode; 2] = [
            NcNode {
                column: 4,
                model: TRAINER_MODEL,
                values: &[(2, 1.0)],
                bypassed: false,
            },
            NcNode {
                column: 0,
                model: TRAINER_MODEL,
                values: &[(1, 1.0)],
                bypassed: false,
            },
        ];
        const REFINER: [NcNode; 1] = [NcNode {
            column: 1,
            model: REFINER_MODEL,
            values: &[],
            bypassed: false,
        }];
        // The send at (0, 1) first, then the player at (0, 0), as built.
        const NC2: [NcNode; 2] = [
            NcNode {
                column: 1,
                model: NC2_SEND_MODEL,
                values: &[],
                bypassed: true,
            },
            NcNode {
                column: 0,
                model: NC2_PLAYER_MODEL,
                values: &[],
                bypassed: false,
            },
        ];
        const NC2_MINI: [NcNode; 2] = [
            NcNode {
                model: NC2_SEND_MODEL + 1,
                ..NC2[0]
            },
            NC2[1],
        ];
        let capture_ports = vec![
            LanePorts {
                row: 0,
                input: 1,
                output: u32::from(hardware.capture_output()),
            },
            LanePorts {
                row: 1,
                input: 2,
                output: 0x13,
            },
        ];
        let lane = |row, input, output| LanePorts { row, input, output };
        match self {
            Self::Recorder => NcLayout {
                nodes: &RECORDER,
                ports: capture_ports,
                split: Some(CAPTURE_SPLIT),
                points: None,
                cab: Some(CabSource::Stored),
            },
            Self::Trainer => NcLayout {
                nodes: &TRAINER,
                ports: vec![lane(0, 0, 0)],
                split: None,
                points: None,
                cab: None,
            },
            Self::Refiner => NcLayout {
                nodes: &REFINER,
                ports: capture_ports,
                split: Some(CAPTURE_SPLIT),
                points: None,
                cab: Some(CabSource::RecorderCopy),
            },
            Self::Nc2 => NcLayout {
                nodes: match hardware {
                    Hardware::QuadCortex => &NC2,
                    Hardware::QuadCortexMini => &NC2_MINI,
                },
                ports: vec![
                    lane(0, 1, 0x13),
                    lane(1, 2, 0),
                    lane(2, 1, u32::from(hardware.capture_output())),
                    lane(3, 8, 0xb),
                ],
                split: None,
                points: Some((-1, 2)),
                cab: Some(CabSource::Nc2Stored),
            },
        }
    }
}

/// One DSPController edit of the active preset (`+0x758`) that the NC2 routes make. Each is
/// a single live edit with its Cortex Control echo, as the stock setter sends it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridEdit {
    /// Lane output control (23000) parameter (`0x520374` VOLUME, `0x520498` MUTE).
    Lane { row: u8, parameter: u32, value: f32 },
    /// Lane input control (28000) parameter (`0x5210d8`).
    InputControl { row: u8, parameter: u32, value: f32 },
    /// Lane input port (`0x519f0c`).
    InputPort { row: u8, port: u32 },
    /// Lane output port (`0x520ac8`).
    OutputPort { row: u8, port: u32 },
    /// Block bypass in the current scene (`0x521170`).
    Bypass { site: (u8, u8), bypassed: bool },
    /// Mixer (11000) parameter of the row's junction (`0x5202a8`); stock then re-sends the
    /// row's mixer values (`0x511548`).
    Mixer { row: u8, parameter: u32, value: f32 },
    /// `0x11` SetHeadphoneMixLevel to core 1 (`0x511ae4`).
    HeadphoneMix { mix: u8, level: f32 },
}

/// `enableNC2(1)` (VA 0x528de4): mute, suspend expressions, lane states of the user preset,
/// monitor at unity (`0x06` built inline), the NC2 preset active, locks, deploy without
/// unmute, lane MUTE/SOLO cleared, then the unmute with flag 0 (`+0x75c` is set).
pub const NC2_ENABLE: [EntryAction; 9] = [
    EntryAction::MasterMute {
        mute: true,
        flag: true,
    },
    EntryAction::SuspendExpressions,
    EntryAction::SaveLaneStates,
    EntryAction::MonitorUnity,
    EntryAction::Activate(NcPreset::Nc2),
    EntryAction::GridLock(true),
    EntryAction::Deploy,
    EntryAction::ClearLaneMuteSolo,
    EntryAction::Unmute { flag: false },
];

/// One step of `_onNcStateChanged` (VA 0x528498) or of the NC2 preset work
/// (`enableNC2` 0x528de4, NC2 routes and audition), in stock order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntryAction {
    /// Store the cab at [`CAB_SITE`] of the active recorder or refiner: its model
    /// (`0x64e880`), bypass (`0x64e98c`) and state as user default
    /// `model + CAB_STATE_KEY_OFFSET` (`0x6684dc`). Runs before every change.
    StoreCab,
    /// `0x20` MasterMute to cores 0 then 1, body `mute | flag << 1` (VA 0x876470).
    MasterMute { mute: bool, flag: bool },
    /// `0x557b94(1, 1, 0)`: stored value 0.0 and `0x44` MetronomeParameter
    /// `(index 4, 4)` (VA 0x875d48). Meaning inferred: metronome off.
    MetronomeOff,
    /// Disconnect `expPluggedChanged` from the expression handler (VA 0x3213e8).
    SuspendExpressions,
    /// Remember the user preset's lane MUTE and SOLO per row (VA 0x50e818).
    SaveLaneStates,
    /// `0x06` SetOutputGain to [`Hardware::monitor_output`], raw f32 body 1.0, not
    /// stored as the user level (VA 0x511a38 with `temporary = 1`).
    MonitorUnity,
    /// From state 16 only: make a new empty preset (`0x51a370`) active and load it
    /// without unmute (`0x5122c0(0, 0)`).
    LoadEmpty,
    /// Build the preset ([`NcPreset::layout`]), make it the active preset
    /// (`+0x758`, signal 0x1e). The refiner flag `+0x75c` is set for the refiner only.
    Activate(NcPreset),
    /// Compiler capture locks `+0x3409` and `+0x3411`, each with a remote notice
    /// (VA 0x582db8, 0x582d08). Meaning of the locks inferred.
    GridLock(bool),
    /// Compile and load the active preset (VA 0x5122c0 → 0x585464, "Loading Preset").
    Deploy,
    /// For rows 0..=3: lane output control MUTE (p2) = 0.0, then SOLO (p3) = 0.0, as
    /// single edits with remote echo (VA 0x520498, 0x520540).
    ClearLaneMuteSolo,
    /// IO controller override (VA 0x7d6440): `true` sends SetInputGain 0.0 to inputs
    /// 1, 2, 4 and 5; `false` restores the stored gains.
    InputGainOverride(bool),
    /// VA 0x512268: MasterMute `{ mute: false, flag }`, then `modelUmuteSignal`.
    /// Stock skips both while `0x6a8244` reports its guard (meaning unresolved).
    Unmute { flag: bool },
    /// State 0: restore normal operation. Stock order (VA 0x528498 case 0): input
    /// override on, 100 ms, restore `+0x828`, no active preset, locks off, the held-stomp
    /// scan (map `+0xf034`: a held stomp whose switch is down sets `+0xf031`, so the reload
    /// leaves the momentary release armed), reload the user preset
    /// (`0x527d80(user, 1, 0)`), input override off, 100 ms, restore the stored monitor
    /// gain, restore lane MUTE/SOLO (`0x524588`), [`EntryAction::Unmute`] with flag false,
    /// release the held stomps whose switch is up (`0x524358` → `0x523f80`), then the
    /// stored metronome Enabled (`0x512e0c`).
    RevertNormal,
    /// `enableNC2(0)` (VA 0x528de4): the state-0 revert without the held-stomp steps and the
    /// metronome, and with `0x527d80(user, 2, 0)`.
    RevertNc2,
    /// An NC2 route: its [`GridEdit`]s ([`crate::v2::Nc2Routing::edits`]) on the active
    /// preset (VA 0x1fec1c, 0x1fea60, 0x1fed6c, 0x1fee14, 0x202fd8).
    Nc2Route(crate::v2::Nc2Route),
    /// One NC2 IO parameter (VA 0x1ff730, 0x1ff190), stored in
    /// `[NeuralCapture2ControllerSettings]` (`0x1fe998`).
    Nc2Io { index: usize, value: f32 },
    /// The NC2 cab bypass: block (0, 3) (`0x521170`), stored as `capture2CabsimBypass`
    /// (VA 0x1fe5a4).
    Nc2CabBypass(bool),
    /// The NC2 audition (VA 0x515a64): only while the NC2 preset is active, the downloaded
    /// blob to the Capture2Driver at (0, 0) as ReceiveData clear then the blob
    /// (`0x5661ec`). Stock logs a failed load and goes on.
    LoadAudition,
}

/// DSPController's view of the capture state (`+0x844`) and its active preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DspController {
    last: Option<NcState>,
    active: Option<NcPreset>,
}

impl DspController {
    /// The last state seen (stock starts at 0).
    pub fn last(&self) -> NcState {
        self.last.unwrap_or(NcState::Inactive)
    }

    pub fn active(&self) -> Option<NcPreset> {
        self.active
    }

    /// The actions for one `ncStateChanged`. Every state is reported, also repeats.
    pub fn enter(&mut self, state: NcState) -> Vec<EntryAction> {
        use EntryAction as A;
        let last = self.last();
        let mut actions = Vec::new();
        if state != last && matches!(self.active, Some(NcPreset::Recorder | NcPreset::Refiner)) {
            actions.push(A::StoreCab);
        }
        match state {
            NcState::Initial if last == NcState::MicSetup => actions.push(A::MasterMute {
                mute: true,
                flag: true,
            }),
            NcState::Initial if last == NcState::Initial => {}
            NcState::Initial => {
                actions.extend([A::MetronomeOff, A::SuspendExpressions]);
                if self.active.is_none() {
                    actions.push(A::SaveLaneStates);
                }
                actions.push(A::MonitorUnity);
                if last == NcState::HandleError {
                    actions.push(A::LoadEmpty);
                }
                actions.extend([
                    A::Activate(NcPreset::Recorder),
                    A::GridLock(true),
                    A::Deploy,
                    A::ClearLaneMuteSolo,
                ]);
                if last == NcState::Inactive {
                    actions.push(A::InputGainOverride(true));
                }
                self.active = Some(NcPreset::Recorder);
            }
            NcState::MicSetup => actions.push(self.unmute()),
            NcState::Training | NcState::AutoRefine if state != last => {
                let preset = if state == NcState::Training {
                    NcPreset::Trainer
                } else {
                    NcPreset::Refiner
                };
                self.active = Some(preset);
                actions.extend([A::Activate(preset), A::Deploy, self.unmute()]);
            }
            NcState::Inactive if last != NcState::Inactive => {
                self.active = None;
                actions.push(A::RevertNormal);
            }
            _ => {}
        }
        self.last = Some(state);
        actions
    }

    /// The `0x512268` flag: a capture preset is active and it is not the refiner.
    fn unmute(&self) -> EntryAction {
        EntryAction::Unmute {
            flag: matches!(self.active, Some(NcPreset::Recorder | NcPreset::Trainer)),
        }
    }
}

/// The deployment map of the active capture preset: grid `(row, column)` → DSP target
/// (DSPController `+0xb910`, compiler map `+0xe3c`, lookup VA 0x879ff0).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturePlacement {
    entries: Vec<((u8, u8), DeploymentTarget)>,
}

impl CapturePlacement {
    /// Later entries for the same site replace earlier ones.
    pub fn new(entries: impl IntoIterator<Item = ((u8, u8), DeploymentTarget)>) -> Self {
        let mut placement = Self::default();
        for (site, target) in entries {
            placement.entries.retain(|&(known, _)| known != site);
            placement.entries.push((site, target));
        }
        placement
    }

    /// VA 0x510ba0: the DSP target of row 0, column `node`. Stock uses slot 0xff (-1)
    /// when the site is not deployed; this returns `None`. Meter requests take only the
    /// slot: stock passes a constant core (VA 0x3cbedc).
    pub fn node_target(&self, node: u8) -> Option<DeploymentTarget> {
        self.entries
            .iter()
            .find(|&&(site, _)| site == (0, node))
            .map(|&(_, target)| target)
    }

    /// VA 0x510be4: the first site deployed at `target`, in map order (row, column).
    pub fn site(&self, target: DeploymentTarget) -> Option<(u8, u8)> {
        self.entries
            .iter()
            .filter(|&&(_, deployed)| deployed == target)
            .map(|&(site, _)| site)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use EntryAction as A;
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    fn target(core: DspCore, slot: u32) -> DeploymentTarget {
        DeploymentTarget {
            core,
            slot: ModuleSlot::try_from(slot).unwrap(),
        }
    }

    /// Stock reactions over a full run (VA 0x528498, deploy args from objdump at
    /// 0x528704, 0x528998, 0x528acc): open, setup, train, refine, save, close.
    #[test]
    fn controller_follows_the_stock_state_reactions() {
        let mut dsp = DspController::default();
        let recorder_from = |extra: &[EntryAction]| {
            let mut plan = vec![A::MetronomeOff, A::SuspendExpressions];
            plan.extend_from_slice(extra);
            plan
        };
        let mut open = recorder_from(&[A::SaveLaneStates, A::MonitorUnity]);
        open.extend([
            A::Activate(NcPreset::Recorder),
            A::GridLock(true),
            A::Deploy,
            A::ClearLaneMuteSolo,
            A::InputGainOverride(true),
        ]);
        assert_eq!(dsp.enter(NcState::Initial), open);
        assert_eq!(dsp.enter(NcState::Initial), []);
        assert_eq!(dsp.enter(NcState::DiagramStep1), [A::StoreCab]);
        dsp.enter(NcState::MicSetup);
        // 6 → 1 only mutes; 1 → 6 unmutes with the flag of a non-refiner preset.
        assert_eq!(
            dsp.enter(NcState::Initial),
            [
                A::StoreCab,
                A::MasterMute {
                    mute: true,
                    flag: true
                }
            ]
        );
        assert_eq!(
            dsp.enter(NcState::MicSetup),
            [A::StoreCab, A::Unmute { flag: true }]
        );
        assert_eq!(dsp.enter(NcState::Calibrate), [A::StoreCab]);
        assert_eq!(
            dsp.enter(NcState::Training),
            [
                A::StoreCab,
                A::Activate(NcPreset::Trainer),
                A::Deploy,
                A::Unmute { flag: true }
            ]
        );
        assert_eq!(
            dsp.enter(NcState::AutoRefine),
            [
                A::Activate(NcPreset::Refiner),
                A::Deploy,
                A::Unmute { flag: false }
            ]
        );
        assert_eq!(dsp.enter(NcState::AutoRefine), []);
        dsp.enter(NcState::HandleError);
        // 16 → 1: an empty preset loads first; no lane states are saved over capture.
        let mut restart = recorder_from(&[A::MonitorUnity, A::LoadEmpty]);
        restart.insert(0, A::StoreCab);
        restart.extend([
            A::Activate(NcPreset::Recorder),
            A::GridLock(true),
            A::Deploy,
            A::ClearLaneMuteSolo,
        ]);
        assert_eq!(dsp.enter(NcState::Initial), restart);
        assert_eq!(dsp.enter(NcState::Inactive), [A::StoreCab, A::RevertNormal]);
        assert_eq!(dsp.active(), None);
        assert_eq!(dsp.enter(NcState::Inactive), []);
    }

    /// Builders 0x51c780, 0x51b5e8 and 0x51b1ac; ports and mixes follow `0x499a60`.
    #[test]
    fn capture_presets_match_the_stock_builders() {
        let recorder = NcPreset::Recorder.layout(Hardware::QuadCortex);
        assert_eq!(recorder.nodes[0].model, RECORDER_MODEL);
        assert_eq!(recorder.nodes[0].column, 1);
        assert_eq!(
            recorder.ports,
            [
                LanePorts {
                    row: 0,
                    input: 1,
                    output: 8
                },
                LanePorts {
                    row: 1,
                    input: 2,
                    output: 0x13
                }
            ]
        );
        assert_eq!(recorder.split, Some(CAPTURE_SPLIT));
        assert_eq!(recorder.cab, Some(CabSource::Stored));

        let trainer = NcPreset::Trainer.layout(Hardware::QuadCortexMini);
        let sites: Vec<_> = trainer
            .nodes
            .iter()
            .map(|n| (n.column, n.model, n.values))
            .collect();
        assert_eq!(
            sites,
            [
                (4, TRAINER_MODEL, &[(2, 1.0)][..]),
                (0, TRAINER_MODEL, &[(1, 1.0)][..])
            ]
        );
        assert_eq!((trainer.split, trainer.cab), (None, None));

        let refiner = NcPreset::Refiner.layout(Hardware::QuadCortexMini);
        assert_eq!(refiner.nodes[0].model, REFINER_MODEL);
        assert_eq!(refiner.ports[0].output, 9);
        assert_eq!(refiner.cab, Some(CabSource::RecorderCopy));
        assert_eq!(Hardware::QuadCortexMini.start_mixes(), [(1, 1.0), (9, 0.0)]);
    }

    /// 0x510ba0 reads row 0 only; 0x510be4 returns the first site for a target.
    #[test]
    fn placement_maps_nodes_and_replies_both_ways() {
        let placement = CapturePlacement::new([
            ((1, 4), target(DspCore::Core1, 7)),
            ((0, 4), target(DspCore::Core1, 2)),
            ((0, 0), target(DspCore::Core0, 3)),
            ((0, 1), target(DspCore::Core0, 9)),
            ((0, 1), target(DspCore::Core0, 5)),
        ]);
        assert_eq!(placement.node_target(1), Some(target(DspCore::Core0, 5)));
        assert_eq!(placement.node_target(4), Some(target(DspCore::Core1, 2)));
        assert_eq!(placement.node_target(2), None);
        assert_eq!(placement.site(target(DspCore::Core0, 5)), Some((0, 1)));
        assert_eq!(placement.site(target(DspCore::Core1, 7)), Some((1, 4)));
        assert_eq!(placement.site(target(DspCore::Core0, 9)), None);
    }
}
