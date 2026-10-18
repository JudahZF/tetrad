//! The application side of the dispatcher. ZenUI receivers call controllers
//! directly; here they call one `DeviceModel` that the app implements.
//!
//! Every query and command has a default that returns
//! `DeviceError::Unsupported`, so an app implements only what it supports and
//! every other request fails explicitly. Queries return reply payloads; the
//! dispatcher sets the action (UPDATE) and the request ID. Settings updates
//! receive the decoded message: stock receivers test each field's has-bit and
//! apply only present fields, and the model must do the same.

use crate::block::{CaptureDefault, ModelPresetRecord};
use crate::codecs::{
    BulkOperationMessage, CPULoadMessage, CloudBackupMessage, CloudLoginMessage,
    CloudProductMessage, CompilerInhibitedModulesMessage, ConfirmationAction, ControlMessage,
    CortexMessageType, DefaultParametersMessage, DiagnosticsMessage, EnumValue, ExpPortSettings,
    FileMessage, GainCalibrationMessage, GeneralSettingsMessage, GenerateTestPresetMessage,
    GlobalEQMessage, GlobalEQParameter, GlobalTempoMessage, GridMessage, GridMoveMessage,
    HeadphonesFeedLevel, HeadphonesSettings, IOMeterMessage, IOSettingsMessage, InputPortSettings,
    LicenseMessage, LoadAutomatedTestPresetMessage, LocalBackupMessage, LooperMessage,
    MIDIPortSettings, MIDISettingsMessage, MasterVolumeMessage, ModeMessage, ModelPresetID,
    ModelPresetMessage, ModuleStatsMessage, NeuralCapture2Message, NeuralCaptureMessage,
    OutputPortSettings, PortSettings, PresetSpeedTestMessage, ProductionTestMessage,
    RecallPresetMessage, RecentsFavoritesMessage, RemoteControlMessage, ScreenshotMessage,
    SerializationMessage, SetTestPresetInputOutputPortsMessage, SetTestPresetSplitMixPointsMessage,
    SetlistPositionMessage, TestFarmMessage, TunerMessage, USBPortSettings, UndoRedoMessage,
    UpdaterMessage, VersionMessage,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("{0:?} is not supported by this device model")]
    Unsupported(CortexMessageType),
    #[error("{0:?} request was refused: {1}")]
    Refused(CortexMessageType, String),
}

pub type DeviceResult<T> = Result<T, DeviceError>;

use CortexMessageType as T;

fn unsupported<V>(message_type: CortexMessageType) -> DeviceResult<V> {
    Err(DeviceError::Unsupported(message_type))
}

/// Grid model-meter subscription target (GridModelMeter 37, `0x110a70`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MeterTarget {
    /// Row and model meters for the whole grid (row and column both -1).
    All,
    /// One row meter, rows 0–3 (column -1).
    Row(u8),
    /// One model meter, rows 0–3, columns 0–7.
    Model { row: u8, column: u8 },
}

/// What one Grid (1) UPDATE sub-handler of `0x257408` sends back. `now` goes out inside
/// the receiver, so it carries the request ID. `delayed` is what the Grid sender holds
/// while its suspend flag `+0x44` is set (`0x25b248`, around parameter edits): the
/// DelayedMessageHandler (`0x262b1c`) sends it [`GRID_ECHO_DELAY`] later with the request
/// ID of the edit, and a newer echo for the same target replaces a pending one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridEcho {
    pub now: Vec<ControlMessage>,
    pub delayed: Vec<DelayedGrid>,
}

/// `0x262b1c(handler, message, 100)`.
pub const GRID_ECHO_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// A held Grid echo. `target` names the edited value (row, model or column, parameter);
/// equal targets replace each other. Stock compares with a stored functor that is not
/// read (inferred).
#[derive(Debug, Clone, PartialEq)]
pub struct DelayedGrid {
    pub target: (u32, u32, u32),
    pub message: GridMessage,
}

/// Tuner (6) UPDATE `0x284e30` takes exactly one branch, in this order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TunerCommand {
    InputPort(i32),
    /// Offset from 440 Hz; the DSP base frequency is `440 + offset`.
    ReferenceOffset(f32),
    /// Stock changes graph routing, not a DSP Boolean.
    Mute(bool),
    /// Meter subscription; the dispatcher tracks it for meter pushes.
    EnableMeter(bool),
}

/// The one change an IOSettings (3) UPDATE carries (`0x26acfc`), and the one-port UPDATE
/// the device sends back after each I/O change (`0x26dafc`). Values are the normalized
/// `ConnectorsSettings` floats. Ports are `GainCalInputPortId` (1, 2, 4, 5) and
/// `GainCalOutputPortId` numbers (outputs 1, 2, 4..9; headphone feeds also 3 and 19).
#[derive(Debug, Clone, PartialEq)]
pub enum IoCommand {
    Input {
        port: u32,
        field: IoInputField,
    },
    Output {
        port: u32,
        field: IoOutputField,
    },
    HeadphoneLevel(f32),
    HeadphoneFeed {
        port: u32,
        level: f32,
    },
    UsbHeadphoneSelect(f32),
    UsbLevel(f32),
    UsbDryWet(f32),
    MidiThru(f32),
    /// `calibrating` starts (`0x358330`) or stops (`0x35828c`) pedal 2 if the id is 2,
    /// else pedal 1.
    ExpCalibrating {
        pedal: u8,
        calibrating: bool,
    },
    Xlr12Linked(bool),
    Out34Linked(bool),
    /// `preset_to_load` (`0x3634e8`): an I/O preset.
    LoadPreset(ModelPresetID),
}

/// Input port fields, in the stock priority order (`0x2673c4`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IoInputField {
    GroundLift(f32),
    /// `round(2v)`: 0 TS, 1 XLR, 2 XLR+48V.
    InputType(f32),
    Zmode(f32),
    Level(f32),
}

/// Output port fields, in the stock priority order (`0x26775c`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IoOutputField {
    /// Only XLR ports 1, 4 and 5 take it.
    GroundLift(f32),
    Level(f32),
    Mute(bool),
}

impl IoCommand {
    /// One branch per message: `settings` (first input port, else first output port, else
    /// headphones, USB, MIDI, EXP), else `xlr1_2_linked`, `out3_4_linked`, `preset_to_load`.
    /// Within a port the first present field wins. `None` when nothing applies.
    pub fn from_update(message: &IOSettingsMessage) -> Option<Self> {
        if let Some(settings) = &message.settings {
            return Self::from_ports(settings);
        }
        if let Some(linked) = message.xlr1_2_linked {
            return Some(Self::Xlr12Linked(linked));
        }
        if let Some(linked) = message.out3_4_linked {
            return Some(Self::Out34Linked(linked));
        }
        message.preset_to_load.clone().map(Self::LoadPreset)
    }

    fn from_ports(settings: &PortSettings) -> Option<Self> {
        if let Some(input) = settings.in_port.first() {
            let port = input.input_port_id.unwrap_or(0);
            let field = if let Some(v) = input.ground_lift {
                IoInputField::GroundLift(v)
            } else if let Some(v) = input.input_type {
                IoInputField::InputType(v)
            } else if let Some(v) = input.input_zmode {
                IoInputField::Zmode(v)
            } else {
                IoInputField::Level(input.level?)
            };
            return Some(Self::Input { port, field });
        }
        if let Some(output) = settings.out_port.first() {
            let port = output.output_port_id.unwrap_or(0);
            let field = match output.ground_lift {
                Some(v) if matches!(port, 1 | 4 | 5) => IoOutputField::GroundLift(v),
                _ => match (output.level, output.mute) {
                    (Some(v), _) => IoOutputField::Level(v),
                    (None, Some(mute)) => IoOutputField::Mute(mute),
                    (None, None) => return None,
                },
            };
            return Some(Self::Output { port, field });
        }
        if let Some(hp) = &settings.hp_port {
            if let Some(level) = hp.level {
                return Some(Self::HeadphoneLevel(level));
            }
            let feed = hp.hp_feed.first()?;
            return Some(Self::HeadphoneFeed {
                port: feed.output_port_id.unwrap_or(0),
                level: feed.level?,
            });
        }
        if let Some(usb) = &settings.usb_port {
            return usb
                .hp_select
                .map(Self::UsbHeadphoneSelect)
                .or(usb.level.map(Self::UsbLevel))
                .or(usb.dry_wet.map(Self::UsbDryWet));
        }
        if let Some(midi) = &settings.midi_port {
            return midi.midi_thru.map(Self::MidiThru);
        }
        let exp = settings.exp_port.first()?;
        Some(Self::ExpCalibrating {
            pedal: if exp.exp_port_id == Some(2) { 2 } else { 1 },
            calibrating: exp.calibrating?,
        })
    }

    /// The UPDATE that carries exactly this change (the device echo, `0x26dafc`).
    pub fn to_update(&self) -> IOSettingsMessage {
        let ports = |settings: PortSettings| IOSettingsMessage {
            action: Some(crate::codecs::MessageAction::Update.into()),
            settings: Some(settings),
            ..Default::default()
        };
        match self.clone() {
            Self::Input { port, field } => {
                let mut input = InputPortSettings {
                    input_port_id: Some(port),
                    ..Default::default()
                };
                match field {
                    IoInputField::GroundLift(v) => input.ground_lift = Some(v),
                    IoInputField::InputType(v) => input.input_type = Some(v),
                    IoInputField::Zmode(v) => input.input_zmode = Some(v),
                    IoInputField::Level(v) => input.level = Some(v),
                }
                ports(PortSettings {
                    in_port: vec![input],
                    ..Default::default()
                })
            }
            Self::Output { port, field } => {
                let mut output = OutputPortSettings {
                    output_port_id: Some(port),
                    ..Default::default()
                };
                match field {
                    IoOutputField::GroundLift(v) => output.ground_lift = Some(v),
                    IoOutputField::Level(v) => output.level = Some(v),
                    IoOutputField::Mute(mute) => output.mute = Some(mute),
                }
                ports(PortSettings {
                    out_port: vec![output],
                    ..Default::default()
                })
            }
            Self::HeadphoneLevel(level) => ports(PortSettings {
                hp_port: Some(HeadphonesSettings {
                    level: Some(level),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            Self::HeadphoneFeed { port, level } => ports(PortSettings {
                hp_port: Some(HeadphonesSettings {
                    hp_feed: vec![HeadphonesFeedLevel {
                        level: Some(level),
                        output_port_id: Some(port),
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            Self::UsbHeadphoneSelect(v) | Self::UsbLevel(v) | Self::UsbDryWet(v) => {
                let mut usb = USBPortSettings::default();
                match self {
                    Self::UsbHeadphoneSelect(_) => usb.hp_select = Some(v),
                    Self::UsbLevel(_) => usb.level = Some(v),
                    _ => usb.dry_wet = Some(v),
                }
                ports(PortSettings {
                    usb_port: Some(usb),
                    ..Default::default()
                })
            }
            Self::MidiThru(v) => ports(PortSettings {
                midi_port: Some(MIDIPortSettings { midi_thru: Some(v) }),
                ..Default::default()
            }),
            Self::ExpCalibrating { pedal, calibrating } => ports(PortSettings {
                exp_port: vec![ExpPortSettings {
                    exp_port_id: Some(i32::from(pedal)),
                    calibrating: Some(calibrating),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            Self::Xlr12Linked(linked) => IOSettingsMessage {
                action: Some(crate::codecs::MessageAction::Update.into()),
                xlr1_2_linked: Some(linked),
                ..Default::default()
            },
            Self::Out34Linked(linked) => IOSettingsMessage {
                action: Some(crate::codecs::MessageAction::Update.into()),
                out3_4_linked: Some(linked),
                ..Default::default()
            },
            Self::LoadPreset(preset) => IOSettingsMessage {
                action: Some(crate::codecs::MessageAction::Update.into()),
                preset_to_load: Some(preset),
                ..Default::default()
            },
        }
    }
}

/// Mode (14) UPDATE `0x271aa0` takes one branch, in this order: `mode` (has-bit 4, below 9,
/// `0x58c738`), a non-empty list (has-bit 1, `0x58ce2c`), the ATMA page (has-bit 8,
/// `0x589c98`). A message with none of them does nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeCommand {
    Mode(u32),
    AvailableModes(Vec<u32>),
    AtmaPage(u32),
}

/// When a receiver's reply goes out. Stock applies a request inside its receiver and then
/// replies; a device that applies some of the work after the link step defers the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// The receiver replies at once from the model's state.
    Now,
    /// The device sends the reply after it applied the request, inside that request's
    /// context (`ControlLink::notify_for` with `Step::request`).
    Deferred,
}

/// What a GridMove (12) did in `0x26411c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridMoved {
    /// The check `0x26321c` passed and the drop ran (`0x5beb78` → `Chain::dropSlot`
    /// `0x59f620`). Stock sends no preset: the drop's own pushes (the undo frame lists, Grid
    /// echoes) are the answer.
    Dropped,
    /// The check failed and nothing moved: the current preset follows at once
    /// (`0x2778d4(.., 0)`, reason Other with the library item).
    Refused,
}

/// UndoRedo (21) UPDATE `0x1ba218`: undo has priority over redo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoRedoStep {
    Undo,
    Redo,
}

/// Normal-mode Version READ (`0x2a59d0`) starts a key exchange; the stock
/// reply follows asynchronously from `0x2a532c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionExchange {
    /// Reply now with `version_info`.
    Ready,
    /// The app calls `ControlLink::complete_version` when the exchange ends.
    Pending,
}

/// The queries and commands the stock receivers make, grouped by type.
pub trait DeviceModel {
    // --- Session and system ------------------------------------------------

    /// Connection edge (`0x6e1d74`). On disconnect stock cancels an active
    /// cloud bulk operation, resets the LocalBackup, ModelPreset and forward
    /// receivers, disconnects library feeds and refreshes the cloud token.
    /// On connect it reconnects the library feeds. Default: nothing to do.
    fn connection_changed(&mut self, connected: bool) -> DeviceResult<()> {
        let _ = connected;
        Ok(())
    }

    /// Version (10) builder `0x2a36d0`: every info field (kernel, git hash,
    /// wireless, U-Boot, app and bootloader firmware, serial, custom name, MAC,
    /// encoder firmware, cloud endpoint, device type, ESS). The dispatcher
    /// keeps only the requested fields when the request names any.
    fn version_info(&mut self) -> DeviceResult<VersionMessage> {
        unsupported(T::Version)
    }

    fn begin_version_exchange(
        &mut self,
        request: &VersionMessage,
    ) -> DeviceResult<VersionExchange> {
        let _ = request;
        unsupported(T::Version)
    }

    /// Version UPDATE with `custom_name`.
    fn set_device_name(&mut self, name: &str) -> DeviceResult<()> {
        let _ = name;
        unsupported(T::Version)
    }

    /// ModelRepo (51) READ `0x11e180`: the prepared content of
    /// `/opt/neuraldsp/ModelRepo.xml` (`0x11e3cc`). Empty sends nothing.
    fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
        unsupported(T::ModelRepo)
    }

    /// SuspendConnection (53) with `suspend = false` runs the deferred
    /// resume callback (`0x1380d8` → `0x3fcd58`).
    fn resume_suspended(&mut self) -> DeviceResult<()> {
        unsupported(T::SuspendConnection)
    }

    /// Confirmation (47) answer, or Cancel on disconnect (`0xf9994`).
    fn confirmation_answered(
        &mut self,
        confirmation_id: u64,
        response: ConfirmationAction,
    ) -> DeviceResult<()> {
        let _ = (confirmation_id, response);
        unsupported(T::Confirmation)
    }

    /// ProductionAutomationMode (11) `0x273f3c` → `0x6e192c`.
    fn set_production_automation(&mut self, enable: bool) -> DeviceResult<()> {
        let _ = enable;
        unsupported(T::ProductionAutomationMode)
    }

    /// SystemTimeSync (43) `0x1ba1b0`: stock sets the clock only when it is
    /// not already set.
    fn set_system_time(&mut self, ms_since_epoch: u64) -> DeviceResult<()> {
        let _ = ms_since_epoch;
        unsupported(T::SystemTimeSync)
    }

    // --- Preset, grid and scenes ---------------------------------------------

    /// Grid (1) UPDATE with a preset (`0x257408`, action switch): the first non-empty
    /// preset field picks one sub-handler. Returns the echoes it sends.
    fn update_grid(&mut self, update: &GridMessage) -> DeviceResult<GridEcho> {
        let _ = update;
        unsupported(T::Grid)
    }

    /// Grid (1) DELETE with a preset (`0x257408`, action switch).
    fn delete_grid_preset(&mut self, update: &GridMessage) -> DeviceResult<()> {
        let _ = update;
        unsupported(T::Grid)
    }

    /// GridMove (12) `0x26411c`: a move that fails the check `0x26321c` changes nothing and
    /// [`GridMoved::Refused`] sends [`Self::current_preset`] at once; a drop sends no preset.
    fn move_grid(&mut self, moves: &GridMoveMessage) -> DeviceResult<GridMoved> {
        let _ = moves;
        unsupported(T::GridMove)
    }

    /// RecallPreset (15) state: the working preset (`0x7c2bdc`); stock always sets
    /// `reason` (`0x277468`).
    fn current_preset(&mut self) -> DeviceResult<RecallPresetMessage> {
        unsupported(T::RecallPreset)
    }

    /// RecallPreset UPDATE `0x274220`: load the carried preset. [`Reply::Now`] sends
    /// [`Self::current_preset`] at once.
    fn recall_preset(&mut self, recall: &RecallPresetMessage) -> DeviceResult<Reply> {
        let _ = recall;
        unsupported(T::RecallPreset)
    }

    fn mode(&mut self) -> DeviceResult<ModeMessage> {
        unsupported(T::Mode)
    }

    fn set_mode(&mut self, command: ModeCommand) -> DeviceResult<()> {
        let _ = command;
        unsupported(T::Mode)
    }

    /// Scene (13) getter `0x589a30`: 0–7.
    fn scene(&mut self) -> DeviceResult<u32> {
        unsupported(T::Scene)
    }

    /// Non-forced `setScene` `0x5899d8`.
    fn select_scene(&mut self, scene: u32) -> DeviceResult<()> {
        let _ = scene;
        unsupported(T::Scene)
    }

    /// SceneCopy (22) `0x51a2a8`, then stock reselects `to`.
    fn copy_scene(&mut self, from: i32, to: i32, swap: bool) -> DeviceResult<()> {
        let _ = (from, to, swap);
        unsupported(T::SceneCopy)
    }

    /// SceneLabel (23); index is 0–7.
    fn scene_label(&mut self, index: i32) -> DeviceResult<String> {
        let _ = index;
        unsupported(T::SceneLabel)
    }

    fn set_scene_label(&mut self, index: i32, label: &str) -> DeviceResult<()> {
        let _ = (index, label);
        unsupported(T::SceneLabel)
    }

    /// SceneColor (48); index is 0–7.
    fn scene_color(&mut self, index: i32) -> DeviceResult<u32> {
        let _ = index;
        unsupported(T::SceneColor)
    }

    fn set_scene_color(&mut self, index: i32, color: u32) -> DeviceResult<()> {
        let _ = (index, color);
        unsupported(T::SceneColor)
    }

    /// UndoRedo (21) READ: packed undo and redo frame stacks.
    fn undo_redo(&mut self) -> DeviceResult<UndoRedoMessage> {
        unsupported(T::UndoRedo)
    }

    fn undo_redo_step(&mut self, step: UndoRedoStep) -> DeviceResult<()> {
        let _ = step;
        unsupported(T::UndoRedo)
    }

    /// PresetDirty (34) READ `0x5ae16c`. Control cannot set it.
    fn preset_dirty(&mut self) -> DeviceResult<bool> {
        unsupported(T::PresetDirty)
    }

    fn setlist_position(&mut self) -> DeviceResult<SetlistPositionMessage> {
        unsupported(T::SetlistPosition)
    }

    fn set_setlist_position(&mut self, position: &SetlistPositionMessage) -> DeviceResult<()> {
        let _ = position;
        unsupported(T::SetlistPosition)
    }

    /// File (4) `0x250090`, switched on `action` (0 CREATE, 1 UPDATE, 2 DELETE, 3 READ,
    /// 4 MOVE, 5 COPY, 8 SWAP; 6 and 7 only clear the bulk import). `file_type` is the folder
    /// product type: 0 presets, 1 IRs, 2 captures. Returns what goes out inside the request,
    /// in order: READ pushes one UPDATE per folder (`0x2453fc`) with the request ID, the full
    /// READ first the `user_content_estimate`; writes send nothing (their library signals
    /// push).
    fn file(&mut self, request: &FileMessage) -> DeviceResult<Vec<FileMessage>> {
        let _ = request;
        unsupported(T::File)
    }

    /// The library pushes of the resync (`0x29a4f8` → `0x25057c`), in stock order. They are
    /// File pushes, not a READ reply.
    fn file_listing(&mut self) -> DeviceResult<Vec<FileMessage>> {
        unsupported(T::File)
    }

    /// DefaultParameters (19) CREATE `0x15eb0c`: store the block at row/column as its model's
    /// defaults (`0x6684c8`, `REPLACE INTO default_param`, then `default_exp_switch`). An
    /// empty slot stores nothing. The receiver then replies UPDATE with row and column.
    fn save_default_parameters(&mut self, row: i32, column: i32) -> DeviceResult<()> {
        let _ = (row, column);
        unsupported(T::DefaultParameters)
    }

    /// DefaultParameters UPDATE `0x15eb0c` → `0x66824c` → `0x15d74c`: reset the block at
    /// row/column to its stored defaults (catalog defaults without rows). Returns the Grid
    /// echoes of the written parameters (`0x523644` → `0x25d51c`).
    fn apply_default_parameters(
        &mut self,
        update: &DefaultParametersMessage,
    ) -> DeviceResult<GridEcho> {
        let _ = update;
        unsupported(T::DefaultParameters)
    }

    /// ModelPreset (71) `0x118f54`: the replies stock sends, in order.
    fn model_preset(
        &mut self,
        request: &ModelPresetMessage,
    ) -> DeviceResult<Vec<ModelPresetMessage>> {
        let _ = request;
        unsupported(T::ModelPreset)
    }

    /// DefaultParameters (19) resync keys: `SELECT DISTINCT model_id FROM default_param`
    /// (`0x6a2390`), in any order. The dispatcher sorts and filters them
    /// (`block::default_parameter_keys`).
    fn default_param_ids(&mut self) -> DeviceResult<Vec<String>> {
        unsupported(T::DefaultParameters)
    }

    /// The library capture for one DefaultParameters key and its default block
    /// (`0x15d890`: capture library lookup `+0x40`, then `+0x5c`; block `0x704530`). `None`
    /// when the library has no such capture: stock sends no entry for the key.
    fn capture_default(&mut self, key: &str) -> DeviceResult<Option<CaptureDefault>> {
        let _ = key;
        unsupported(T::DefaultParameters)
    }

    /// ModelPreset (71) resync records in `0x1f6280` order: factory model presets, user
    /// model presets, then the special default entries (`0x1f6038`).
    fn model_preset_records(&mut self) -> DeviceResult<Vec<ModelPresetRecord>> {
        unsupported(T::ModelPreset)
    }

    /// BulkOperation (57) READ state; `0xed8d0`.
    fn bulk_operation(&mut self) -> DeviceResult<BulkOperationMessage> {
        unsupported(T::BulkOperation)
    }

    /// BulkOperation DELETE: set the multiselection abort flag when it is busy (`0xed8d0`).
    fn cancel_bulk_operation(&mut self) -> DeviceResult<()> {
        unsupported(T::BulkOperation)
    }

    // --- Settings ---------------------------------------------------------------

    /// IOSettings READ `0x269ab4`: the reply mirrors the request; an empty request gets the
    /// full dump (`0x268a14` → `0x17be68`).
    fn io_settings(&mut self, request: &IOSettingsMessage) -> DeviceResult<IOSettingsMessage> {
        let _ = request;
        unsupported(T::IOSettings)
    }

    /// IOSettings UPDATE `0x26acfc`: one change per message ([`IoCommand::from_update`]).
    fn update_io_settings(&mut self, command: IoCommand) -> DeviceResult<()> {
        let _ = command;
        unsupported(T::IOSettings)
    }

    fn update_midi_settings(&mut self, update: &MIDISettingsMessage) -> DeviceResult<()> {
        let _ = update;
        unsupported(T::MIDISettings)
    }

    /// GeneralSettings READ `0x16c964`: every field.
    fn general_settings(&mut self) -> DeviceResult<GeneralSettingsMessage> {
        unsupported(T::GeneralSettings)
    }

    fn update_general_settings(&mut self, update: &GeneralSettingsMessage) -> DeviceResult<()> {
        let _ = update;
        unsupported(T::GeneralSettings)
    }

    /// MasterVolume (17) READ: `volume` and `engaged`.
    fn master_volume(&mut self) -> DeviceResult<MasterVolumeMessage> {
        unsupported(T::MasterVolume)
    }

    /// Normal UPDATE `0x1962c8` reads only `volume`.
    fn set_master_volume(&mut self, volume: f32) -> DeviceResult<()> {
        let _ = volume;
        unsupported(T::MasterVolume)
    }

    /// GlobalTempo (33) READ `0x10aa1c`: one float Param per stored entry.
    fn global_tempo(&mut self) -> DeviceResult<GlobalTempoMessage> {
        unsupported(T::GlobalTempo)
    }

    /// GlobalTempo UPDATE gate `0x109b60`: first Param, index, float value.
    fn set_global_tempo_param(&mut self, index: u32, value: f32) -> DeviceResult<()> {
        let _ = (index, value);
        unsupported(T::GlobalTempo)
    }

    /// GlobalEQ (38) state sender `0x16e044`.
    fn global_eq(&mut self) -> DeviceResult<GlobalEQMessage> {
        unsupported(T::GlobalEQ)
    }

    /// One parameter, index below 28 (`0x2fa314`).
    fn set_global_eq_parameter(&mut self, parameter: &GlobalEQParameter) -> DeviceResult<()> {
        let _ = parameter;
        unsupported(T::GlobalEQ)
    }

    fn set_global_eq_bypass(&mut self, bypassed: bool) -> DeviceResult<()> {
        let _ = bypassed;
        unsupported(T::GlobalEQ)
    }

    fn load_global_eq_preset(&mut self, preset: &ModelPresetID) -> DeviceResult<()> {
        let _ = preset;
        unsupported(T::GlobalEQ)
    }

    /// CompilerInhibitedModules (42) READ `0x15ac7c`.
    fn compiler_inhibited_modules(&mut self) -> DeviceResult<CompilerInhibitedModulesMessage> {
        unsupported(T::CompilerInhibitedModules)
    }

    // --- Meters and status (subscribed timers) -----------------------------------

    /// `sendIOMeters` `0x6e0040`: stock sends only when the block changed.
    fn io_meters(&mut self) -> DeviceResult<IOMeterMessage> {
        unsupported(T::IOMeter)
    }

    /// `sendCPULoad` `0x6e013c`: the 4 × 8 core/column table.
    fn cpu_load(&mut self) -> DeviceResult<CPULoadMessage> {
        unsupported(T::CPULoad)
    }

    /// `sendDiagnostics` `0x23e6d4`: DSP cores, SOC2 ARM, HID TX failures.
    fn diagnostics(&mut self) -> DeviceResult<DiagnosticsMessage> {
        unsupported(T::Diagnostics)
    }

    /// `sendGridModelMeters` `0x110194` → `0x25da6c`: Grid (1) UPDATE
    /// messages with update_type MeterUpdate and a BinaryPreset-shaped body
    /// holding the active row meters (`input_control` parameter 2) and the model meters per
    /// column (`0x25da6c`); no bypass flags.
    fn grid_model_meters(&mut self, targets: &[MeterTarget]) -> DeviceResult<Vec<ControlMessage>> {
        let _ = targets;
        unsupported(T::GridModelMeter)
    }

    /// The typed form of `grid_model_meters` that the link sends. Its default
    /// converts that method's output and refuses any non-Grid message.
    fn grid_model_meter_updates(
        &mut self,
        targets: &[MeterTarget],
    ) -> DeviceResult<Vec<GridMessage>> {
        self.grid_model_meters(targets)?
            .into_iter()
            .map(|message| match message {
                ControlMessage::Grid(grid) => Ok(grid),
                other => Err(DeviceError::Refused(
                    other.message_type(),
                    "grid meters carry Grid messages only".into(),
                )),
            })
            .collect()
    }

    /// ModuleStats (35) READ.
    fn module_stats(&mut self) -> DeviceResult<ModuleStatsMessage> {
        unsupported(T::ModuleStats)
    }

    // --- Tuner, looper and UI -----------------------------------------------------

    /// Tuner (6) READ: input port, reference offset and mute.
    fn tuner(&mut self) -> DeviceResult<TunerMessage> {
        unsupported(T::Tuner)
    }

    fn update_tuner(&mut self, command: TunerCommand) -> DeviceResult<()> {
        let _ = command;
        unsupported(T::Tuner)
    }

    /// ShowTuner (27) `0x6e0330`; teardown sends `false`.
    fn show_tuner(&mut self, show: bool) -> DeviceResult<()> {
        let _ = show;
        unsupported(T::ShowTuner)
    }

    /// Looper (28) READ `0x18bad8`: cached DSP looper status.
    fn looper(&mut self) -> DeviceResult<LooperMessage> {
        unsupported(T::Looper)
    }

    /// ShowGigView (24).
    fn gig_view_shown(&mut self) -> DeviceResult<bool> {
        unsupported(T::ShowGigView)
    }

    fn show_gig_view(&mut self, show: bool) -> DeviceResult<()> {
        let _ = show;
        unsupported(T::ShowGigView)
    }

    /// GigViewButton (55) `0x109ab4`; the model bounds the index (8 or 4
    /// buttons by mode).
    fn gig_view_button(&mut self, index: u64, pressed: bool) -> DeviceResult<()> {
        let _ = (index, pressed);
        unsupported(T::GigViewButton)
    }

    /// Screenshot (25) READ `0x1b6058`: the stored preset at `folder_name`/`index` (the factory
    /// folder when `is_factory`; index below 256, slot occupied) renders offscreen as the
    /// 800 × 384 grid on a transparent background (`0x691040`); the app later sends UPDATE
    /// {`png`, `is_factory`, `folder_name` (not factory), `index`} with the request's ID
    /// (`0x1b6a5c`), flags 0. A newer request replaces a pending one, which gets no reply.
    fn request_screenshot(&mut self, request: &ScreenshotMessage) -> DeviceResult<()> {
        let _ = request;
        unsupported(T::Screenshot)
    }

    fn remote_control(
        &mut self,
        request: &RemoteControlMessage,
    ) -> DeviceResult<Vec<RemoteControlMessage>> {
        let _ = request;
        unsupported(T::RemoteControl)
    }

    /// EnableCaptureOut (16): `CZenjack::SetNCOutSel(enable, 0, 3, 0, 25000)`.
    fn enable_capture_out(&mut self, enable: bool) -> DeviceResult<()> {
        let _ = enable;
        unsupported(T::EnableCaptureOut)
    }

    // --- Library, models and cloud ------------------------------------------------

    /// RecentsFavorites (20) `0x1aa02c`: the reply, if the branch sends one.
    fn recents_favorites(
        &mut self,
        request: &RecentsFavoritesMessage,
    ) -> DeviceResult<Option<RecentsFavoritesMessage>> {
        let _ = request;
        unsupported(T::RecentsFavorites)
    }

    /// RecentSearches (39) `0x1a7140`.
    fn recent_searches(&mut self) -> DeviceResult<Vec<String>> {
        unsupported(T::RecentSearches)
    }

    fn store_recent_searches(&mut self, searches: &[String]) -> DeviceResult<()> {
        let _ = searches;
        unsupported(T::RecentSearches)
    }

    fn clear_recent_searches(&mut self) -> DeviceResult<()> {
        unsupported(T::RecentSearches)
    }

    /// NewModels (50) `0x1267b8`: models flagged as new.
    fn new_models(&mut self) -> DeviceResult<Vec<u32>> {
        unsupported(T::NewModels)
    }

    /// NewModels DELETE: clear the flag for one model (`0x38fae4`).
    fn clear_new_model(&mut self, model: u32) -> DeviceResult<()> {
        let _ = model;
        unsupported(T::NewModels)
    }

    /// PinnedModels (54) `0x12ba94`: pinned model hashes and captures.
    fn pinned_models(&mut self) -> DeviceResult<(Vec<u32>, Vec<String>)> {
        unsupported(T::PinnedModels)
    }

    /// CREATE pins, DELETE unpins.
    fn pin_models(&mut self, pin: bool, models: &[u32], captures: &[String]) -> DeviceResult<()> {
        let _ = (pin, models, captures);
        unsupported(T::PinnedModels)
    }

    /// License (58) READ state (`0x114154`), sent transformed.
    fn licenses(&mut self) -> DeviceResult<LicenseMessage> {
        unsupported(T::License)
    }

    /// License UPDATE: refresh from the license server (`0x1f0e80`).
    fn refresh_licenses(&mut self) -> DeviceResult<()> {
        unsupported(T::License)
    }

    /// License DELETE `0x1f170c(ctrl, id, 1)` for each `new_plugin_ids` entry (low 32 bits):
    /// a licensed id loses its "new" mark. No HTTP call and no deactivation.
    fn acknowledge_new_plugin(&mut self, plugin: u32) -> DeviceResult<()> {
        let _ = plugin;
        unsupported(T::License)
    }

    fn cloud_login(&mut self, request: &CloudLoginMessage) -> DeviceResult<Vec<CloudLoginMessage>> {
        let _ = request;
        unsupported(T::CloudLogin)
    }

    fn cloud_backup(
        &mut self,
        request: &CloudBackupMessage,
    ) -> DeviceResult<Vec<CloudBackupMessage>> {
        let _ = request;
        unsupported(T::CloudBackup)
    }

    fn cloud_product(
        &mut self,
        request: &CloudProductMessage,
    ) -> DeviceResult<Vec<CloudProductMessage>> {
        let _ = request;
        unsupported(T::CloudProduct)
    }

    /// The backup file text that Control sent in LocalBackup UPDATE chunks
    /// ("Applying local backup of size", `0x2ff33c`, then `0x4080cc`). Stock
    /// replies UPDATE {`can_apply_backup`: false} when no receive was opened
    /// (`0x17cdc8`), later UPDATE {`applied_backup`} (`0x17dab8`).
    fn apply_local_backup(&mut self, backup_json: String) -> DeviceResult<Vec<LocalBackupMessage>> {
        let _ = backup_json;
        unsupported(T::LocalBackup)
    }

    /// LocalBackup (40) `0x17d104` requests other than incoming chunks: CREATE
    /// (start a backup; the app suspends the link and later sends the file
    /// with `ControlLink::send_local_backup`) and UPDATE with
    /// `can_apply_backup` (open a receive; reply `can_apply_backup`).
    fn local_backup(
        &mut self,
        request: &LocalBackupMessage,
    ) -> DeviceResult<Vec<LocalBackupMessage>> {
        let _ = request;
        unsupported(T::LocalBackup)
    }

    /// Logs (44) CREATE `0x114cd4`: start the log upload.
    fn upload_logs(&mut self, correlation_id: Option<&str>) -> DeviceResult<()> {
        let _ = correlation_id;
        unsupported(T::Logs)
    }

    /// ProcessDownloadsQueue (45) UPDATE `0x1326a8`.
    fn process_downloads_queue(&mut self, queue_type: Option<i32>) -> DeviceResult<()> {
        let _ = queue_type;
        unsupported(T::ProcessDownloadsQueue)
    }

    /// Updater (60) `0x1391b4`: READ returns state; UPDATE runs `command`.
    fn updater(&mut self, request: &UpdaterMessage) -> DeviceResult<Option<UpdaterMessage>> {
        let _ = request;
        unsupported(T::Updater)
    }

    /// A forward response (29, 30, 31, 61) for a request the device sent with
    /// `Notification::Forward`. `last` is true on the final chunk. The message is moved, so
    /// the payload chunk reaches the app without a copy (review r21 H8).
    fn forward_response(&mut self, response: ControlMessage, last: bool) -> DeviceResult<()> {
        let _ = last;
        unsupported(response.message_type())
    }

    // --- Capture -----------------------------------------------------------------

    fn neural_capture(
        &mut self,
        request: &NeuralCaptureMessage,
    ) -> DeviceResult<Vec<NeuralCaptureMessage>> {
        let _ = request;
        unsupported(T::NeuralCapture)
    }

    fn neural_capture2(
        &mut self,
        request: &NeuralCapture2Message,
    ) -> DeviceResult<Vec<NeuralCapture2Message>> {
        let _ = request;
        unsupported(T::NeuralCapture2)
    }

    // --- Production automation and test --------------------------------------------

    fn preset_speed_test(&mut self, request: &PresetSpeedTestMessage) -> DeviceResult<()> {
        let _ = request;
        unsupported(T::PresetSpeedTest)
    }

    fn gain_calibration(
        &mut self,
        request: &GainCalibrationMessage,
    ) -> DeviceResult<Vec<GainCalibrationMessage>> {
        let _ = request;
        unsupported(T::GainCalibration)
    }

    fn serialization(
        &mut self,
        request: &SerializationMessage,
    ) -> DeviceResult<SerializationMessage> {
        let _ = request;
        unsupported(T::Serialization)
    }

    fn test_farm(&mut self, request: &TestFarmMessage) -> DeviceResult<Vec<TestFarmMessage>> {
        let _ = request;
        unsupported(T::TestFarm)
    }

    /// ProductionTest (66): only the production-automation receiver
    /// `0x132884` acts (tempo LED, all-LED color); Normal `0x13285c` is empty.
    fn production_test(&mut self, request: &ProductionTestMessage) -> DeviceResult<()> {
        let _ = request;
        unsupported(T::ProductionTest)
    }

    fn load_automated_test_preset(
        &mut self,
        request: &LoadAutomatedTestPresetMessage,
    ) -> DeviceResult<()> {
        let _ = request;
        unsupported(T::LoadAutomatedTestPreset)
    }

    fn set_test_preset_ports(
        &mut self,
        request: &SetTestPresetInputOutputPortsMessage,
    ) -> DeviceResult<()> {
        let _ = request;
        unsupported(T::SetTestPresetInputOutputPorts)
    }

    fn set_test_preset_split_mix(
        &mut self,
        request: &SetTestPresetSplitMixPointsMessage,
    ) -> DeviceResult<()> {
        let _ = request;
        unsupported(T::SetTestPresetSplitMixPoints)
    }

    fn generate_test_preset(&mut self, request: &GenerateTestPresetMessage) -> DeviceResult<()> {
        let _ = request;
        unsupported(T::GenerateTestPreset)
    }
}

/// The Control-version gate of `0x2a59d0`: older than 4.1.0 is incompatible.
pub const MIN_CONTROL_VERSION: [u32; 3] = [4, 1, 0];

/// `0x6eb5d8`: the first differing common component decides; when the common
/// components are equal, the shorter version is older (`"4.1"` < `"4.1.0"`).
/// Non-numeric components count as zero (owned).
pub(crate) fn control_version_is_older(version: &str) -> bool {
    let parts: Vec<u32> = version
        .split('.')
        .map(|part| part.trim().parse::<u32>().unwrap_or(0))
        .collect();
    for (part, minimum) in parts.iter().zip(MIN_CONTROL_VERSION) {
        if *part != minimum {
            return *part < minimum;
        }
    }
    parts.len() < MIN_CONTROL_VERSION.len()
}

/// `0x2a36d0`: a request with none of the info fields gets every field;
/// otherwise only the requested ones. Comms and Control version fields are
/// never filled by the builder.
pub(crate) fn mask_version(mut info: VersionMessage, request: &VersionMessage) -> VersionMessage {
    macro_rules! info_fields {
        ($($field:ident),*) => {
            if false $(|| request.$field.is_some())* {
                $(if request.$field.is_none() {
                    info.$field = None;
                })*
            }
        };
    }
    info_fields!(
        linux_kernel_version,
        zenos_git_hash,
        zenwireless_fw_version,
        uboot_version,
        app_fw_version,
        bootloader_fw_version,
        device_serial_number,
        custom_name,
        mac_address,
        zencoder_fw_app,
        zencoder_fw_bootloader,
        cloud_endpoint,
        device_type,
        is_ess
    );
    info.comms_version = None;
    info.cortex_control_version = None;
    info.cortex_control_version_valid = None;
    info
}

/// Confirmation (47) `0xf986c`: Ok runs the ok callback; Cancel, an absent
/// response or an unknown number run the cancel callback.
pub(crate) fn confirmation_response(
    response: Option<EnumValue<ConfirmationAction>>,
) -> ConfirmationAction {
    match response {
        Some(EnumValue::Known(ConfirmationAction::Ok)) => ConfirmationAction::Ok,
        _ => ConfirmationAction::Cancel,
    }
}

#[cfg(test)]
mod io_tests {
    use super::*;

    /// `0x26acfc` takes one branch: the first input port before any output port, the first
    /// present field in stock order, output ground lift only on XLR ports. Every command
    /// survives its own echo encoding.
    #[test]
    fn io_update_takes_one_branch_and_round_trips() {
        let update = IOSettingsMessage {
            settings: Some(PortSettings {
                in_port: vec![
                    InputPortSettings {
                        input_port_id: Some(2),
                        level: Some(0.25),
                        input_type: Some(1.0),
                        ..Default::default()
                    },
                    InputPortSettings {
                        input_port_id: Some(1),
                        ground_lift: Some(1.0),
                        ..Default::default()
                    },
                ],
                out_port: vec![OutputPortSettings {
                    output_port_id: Some(4),
                    level: Some(0.5),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            xlr1_2_linked: Some(true),
            ..Default::default()
        };
        assert_eq!(
            IoCommand::from_update(&update),
            Some(IoCommand::Input {
                port: 2,
                field: IoInputField::InputType(1.0)
            })
        );
        let unlifted = IOSettingsMessage {
            settings: Some(PortSettings {
                out_port: vec![OutputPortSettings {
                    output_port_id: Some(2),
                    ground_lift: Some(1.0),
                    mute: Some(true),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            IoCommand::from_update(&unlifted),
            Some(IoCommand::Output {
                port: 2,
                field: IoOutputField::Mute(true)
            })
        );
        for command in [
            IoCommand::Input {
                port: 4,
                field: IoInputField::Level(0.5),
            },
            IoCommand::Output {
                port: 1,
                field: IoOutputField::GroundLift(1.0),
            },
            IoCommand::HeadphoneLevel(0.5),
            IoCommand::HeadphoneFeed {
                port: 19,
                level: 0.75,
            },
            IoCommand::UsbHeadphoneSelect(1.0),
            IoCommand::UsbLevel(0.5),
            IoCommand::UsbDryWet(1.0),
            IoCommand::MidiThru(1.0),
            IoCommand::ExpCalibrating {
                pedal: 2,
                calibrating: true,
            },
            IoCommand::Xlr12Linked(false),
            IoCommand::Out34Linked(true),
        ] {
            assert_eq!(IoCommand::from_update(&command.to_update()), Some(command));
        }
    }
}
