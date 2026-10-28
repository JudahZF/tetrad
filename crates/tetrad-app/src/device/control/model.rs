//! The stock receivers' application side: [`StateModel`], the `DeviceModel` over device
//! [`State`]. The topic files add their receivers as more `impl StateModel` blocks. Moved
//! from `control.rs` (round 37).

use std::time::SystemTime;

use tetrad_control::codecs::{
    CloudLoginMessage, CompilerInhibitedModulesMessage, ControlMessage, CortexMessageType,
    GeneralSettingsMessage, GridMessage, GridMoveMessage, IOSettingsMessage, LooperMessage,
    MIDISettingsMessage, MasterVolumeMessage, MessageAction, ModeMessage, RawMessageBytes,
    RecallPresetMessage, RecallPresetReason, SetlistPositionMessage, TunerMessage, UndoRedoMessage,
    VersionDeviceType, VersionMessage,
};
use tetrad_control::device::{
    DeviceError as ModelError, DeviceModel, DeviceResult, GridEcho, GridMoved, IoCommand,
    ModeCommand, Reply, TunerCommand, UndoRedoStep, VersionExchange,
};
use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::tuner_looper::TextPrefix;
use tetrad_engine::{Row, SceneIndex};
use tetrad_hal::hal::Command as HalCommand;
use tetrad_hal::platform::Platform as HalPlatform;
use tetrad_hal::zenjack::NcOutput;
use tetrad_media::factory::{DeviceCipher, DeviceSerial};
use tetrad_preset::{Chain, Model, Param, ParamValue, Preset, PresetDocument, SceneValue};
use tetrad_settings::controls::AtmaVolume;
use tetrad_settings::tuner::InputPortId;

use super::super::actions::{self, Action};
use super::super::bringup::{self, RunOptions};
use super::super::input;
use super::super::meters;
use super::super::preset::{self, UndoPath};
use super::super::services::UploadState;
use super::super::{DspCommand, Effect, PresetRef, State};
use super::{
    AccountRequest, account, available_modes, backup, cloud, engaged_notice, files,
    footswitch_mode, grid, inhibited_modules, io, item_preset, library, looper_status,
    mode_message, models, nc1, nc2, products, refresh_capture_names, resync, sanitized_modes,
    screen, setlist_position, settings, status, undo_frames, updater,
};

/// The refusal of a library write without `--persist` ([`StateModel::library_write`]).
pub(super) const NO_PERSIST: &str = "bring-up refused a library write without --persist";

/// The stock receivers' application side over device [`State`]. Changes become effects.
pub(super) struct StateModel<'a> {
    pub(super) state: &'a mut State,
    pub(super) serial: Option<&'a DeviceSerial>,
    pub(super) model_repo: Option<&'a [u8]>,
    pub(super) capture_lock: bool,
    pub(super) capture_running: bool,
    /// The cached dirty flag (`+0x10`, set by `0x5abcb0`).
    pub(super) dirty: bool,
    pub(super) factory: Option<&'a tetrad_storage::stock::StockDb>,
    pub(super) factory_irs: Option<&'a [String]>,
    pub(super) downloads_in_flight: &'a [String],
    pub(super) upload_states: &'a [(String, UploadState)],
    pub(super) cipher: Option<&'a DeviceCipher>,
    pub(super) wall_clock: Option<SystemTime>,
    pub(super) disk_space: Option<tetrad_sys::fs::FsSpace>,
    pub(super) diagnostics: &'a status::Diagnostics,
    pub(super) effects: Vec<Effect>,
    /// A MasterVolume echo to send [`VOLUME_ECHO_DELAY`] later.
    pub(super) volume_echo: Option<f32>,
    /// A RecallPreset reply waits for the deploy ([`Control::request_done`]).
    pub(super) preset_reply: bool,
    /// The GridMove echo of a drop waits for the deploy ([`grid::DropEcho`]).
    pub(super) drop_echo: Option<grid::DropEcho>,
    pub(super) local_backup: backup::LocalBackup,
    pub(super) cloud: Option<cloud::CloudOp>,
    pub(super) multi_select: bool,
    /// The READ fields of the running run ([`super::Control::bulk_run`]).
    pub(super) bulk: Option<&'a tetrad_control::codecs::BulkOperationMessage>,
    pub(super) link_op: Option<backup::LinkOp>,
    pub(super) options: &'a RunOptions,
    pub(super) input_locked: bool,
    /// Be Right Back runs ([`super::Control`] `standby`): GeneralSettings `power_option`.
    pub(super) standby: bool,
    /// A device dialog to send to Control as a Confirmation (47) after the step.
    pub(super) confirmation: Option<tetrad_control::link::ConfirmationRequest>,
    /// The open upload question ([`super::Control::upload_question`]): its `confirmation_id`
    /// and question serial; its answer takes it.
    pub(super) asked: Option<(u64, u64)>,
    pub(super) updater: updater::UpdaterSeen,
    /// KeepAlive `is_online` (`+0x3d9`): Control may run update checks.
    pub(super) control_online: bool,
    /// Forward response chunks for [`forward::Forwards::answer`] after the step.
    pub(super) forwarded: Vec<(ControlMessage, bool)>,
    /// A host request runs: a File or CloudProduct READ leaves its reply in [`Self::spread`]
    /// for the device passes after it (A3). Else (the resync) it replies at once.
    pub(super) spreads: bool,
    pub(super) spread: Option<files::Spread>,
}

impl StateModel<'_> {
    pub(super) fn current(
        &mut self,
        message_type: CortexMessageType,
    ) -> DeviceResult<&mut super::super::Current> {
        self.state
            .current
            .get_mut()
            .as_mut()
            .ok_or_else(|| ModelError::Refused(message_type, "no preset is loaded".into()))
    }

    pub(super) fn settings(&mut self, change: impl FnOnce(&mut tetrad_settings::device::Settings)) {
        let effects = actions::update_settings(self.state, change);
        self.effects.extend(effects);
    }

    /// Runs one host change through the device reducer; a refusal is the host's error.
    /// Reduces `action` at once, so the device never sees it as an effect: the bring-up
    /// refusal of `Effect::Action(action)` runs here (review r18 F12, review-2 R1). The
    /// PHANTOM arms are left out: Control keeps its PHANTOM state and only the hardware write
    /// is refused (`bringup::phantom_input`).
    pub(super) fn act(
        &mut self,
        message_type: CortexMessageType,
        action: Action,
    ) -> DeviceResult<()> {
        let effect = Effect::Action(action);
        if let Some(refused) = self.options.refusal(&effect)
            && bringup::phantom_input(&effect).is_none()
        {
            let refused = format!("bring-up refused {refused}");
            return Err(ModelError::Refused(message_type, refused));
        }
        let Effect::Action(action) = effect else {
            unreachable!("built above")
        };
        let effects = actions::reduce(action, self.state)
            .map_err(|error| ModelError::Refused(message_type, error.to_string()))?;
        self.effects.extend(effects);
        Ok(())
    }

    /// Queues a device effect after the bring-up check [`Self::act`] makes for actions (a
    /// preset save is refused without `--persist`).
    pub(super) fn queue(
        &mut self,
        message_type: CortexMessageType,
        effect: Effect,
    ) -> DeviceResult<()> {
        if let Some(refused) = self.options.refusal(&effect) {
            let refused = format!("bring-up refused {refused}");
            return Err(ModelError::Refused(message_type, refused));
        }
        self.effects.push(effect);
        Ok(())
    }

    /// A library write a receiver runs itself (list files, setlists, `zenui.db`), not as an
    /// action: refused without `--persist`, as `RunOptions::refusal` refuses library actions.
    pub(super) fn library_write(&self, message_type: CortexMessageType) -> DeviceResult<()> {
        if self.options.persist {
            return Ok(());
        }
        Err(ModelError::Refused(message_type, NO_PERSIST.into()))
    }

    /// One undoable edit. The frame is taken before `edit` (`0x648f1c` by `path`) and kept
    /// only when the edit commits: at once, or with the deploy of the layout change it
    /// queued ([`preset::LayoutChange::record_undo`]). Stock edits cannot fail after the
    /// frame; a refused edit here leaves the history as it was.
    pub(super) fn undoable(
        &mut self,
        path: UndoPath,
        edit: impl FnOnce(&mut Self) -> DeviceResult<()>,
    ) -> DeviceResult<()> {
        let Some(current) = &*self.state.current else {
            return edit(self);
        };
        let frame = self.state.history.begin(current, self.state.now, path);
        let queued = self.effects.len();
        edit(self)?;
        let change = self.effects[queued..]
            .iter_mut()
            .find_map(|effect| match effect {
                Effect::DeployLayout(change) => Some(change),
                _ => None,
            });
        match change {
            Some(change) => change.record_undo(frame),
            None => self.state.history.commit(frame),
        }
        Ok(())
    }
}

fn scene_slot(index: i32) -> Option<SceneIndex> {
    u8::try_from(index)
        .ok()
        .and_then(|index| SceneIndex::new(index).ok())
}

impl DeviceModel for StateModel<'_> {
    /// `0x6e1d74` on a disconnect resets the LocalBackup receiver ([`backup::disconnected`]).
    fn connection_changed(&mut self, connected: bool) -> DeviceResult<()> {
        if !connected {
            backup::disconnected(self);
        }
        Ok(())
    }

    /// Builder `0x2a36d0` (field = source, in has-bit order): kernel `linuxKernelVersion`,
    /// Zenwireless and U-Boot (`CVersions`), `app_fw_version` and `bootloader_fw_version` the
    /// Zenjack application and bootloader (`CZenjack`), serial `0x66b55c`, custom name
    /// `0x64fc30`, MAC `0x66c4b8`, the Zencoder pair (`CZencoder`), device type and ESS. The
    /// values are the boot reads of [`State::info`](super::info::DeviceInfo); a value not read
    /// is absent. Field 4 (named a git hash) carries the CorOS version, and an unset custom name
    /// is "Neural DSP <product>" (`0x64fc30`); Cortex Control shows "does not support CorOS
    /// version" without field 4 (seen on the QC 2026-10-16). Zenwireless and U-Boot keep the
    /// newline of their stock shell pipelines; `cloud_endpoint` is the stage name (`0x2a3514`).
    fn version_info(&mut self) -> DeviceResult<VersionMessage> {
        let platform = self.state.platform;
        let info = &self.state.info;
        let line = |value: &Option<String>| value.as_ref().map(|value| format!("{value}\n"));
        let product = if platform.is_atma() {
            "Quad Cortex mini"
        } else {
            "Quad Cortex"
        };
        let custom_name = (self.state.settings.settings().general.custom_name.clone())
            .unwrap_or_else(|| format!("Neural DSP {product}"));
        let cloud = tetrad_services::cloud::endpoint::CloudStage::from_env(
            std::env::var("CLOUD_CUSTOM_ENDPOINT").ok().as_deref(),
            std::env::var("CLOUD_ENDPOINT").ok().as_deref(),
        );
        let version = |version: &Option<tetrad_hal::zencoder::FirmwareVersion>| {
            version.as_ref().map(ToString::to_string)
        };
        Ok(VersionMessage {
            linux_kernel_version: info.kernel.clone(),
            zenos_git_hash: Some(tetrad_firmware::running().version.to_owned()),
            zenwireless_fw_version: line(&info.wireless),
            uboot_version: line(&info.uboot),
            app_fw_version: version(&info.zenjack[0]),
            bootloader_fw_version: version(&info.zenjack[1]),
            device_serial_number: self.serial.map(|serial| serial.as_str().to_owned()),
            device_type: Some(
                if platform.is_atma() {
                    VersionDeviceType::Atma
                } else {
                    VersionDeviceType::Qc
                }
                .into(),
            ),
            is_ess: Some(platform == HalPlatform::QcEss),
            custom_name: Some(custom_name),
            mac_address: info.mac.clone(),
            zencoder_fw_app: version(&info.zencoder[0]),
            zencoder_fw_bootloader: version(&info.zencoder[1]),
            cloud_endpoint: Some(cloud.name().to_owned()),
            ..Default::default()
        })
    }

    /// Normal-mode Version READ (`0x2a59d0` → `0x2a4dd4`): DSP request `0x42` (`0x5152c4`:
    /// core 3, the serial `+0xb884` as local 8-bit text of at most 32 bytes, wait 0, 100 ms).
    /// The reply waits for the answer ([`Control::complete_key_exchange`]); stock has no
    /// timeout. Without a serial stock sends an empty text.
    fn begin_version_exchange(
        &mut self,
        _request: &VersionMessage,
    ) -> DeviceResult<VersionExchange> {
        let text = self
            .serial
            .map_or(&b""[..], |serial| serial.as_str().as_bytes());
        let text = TextPrefix::try_from(text)
            .map_err(|error| ModelError::Refused(CortexMessageType::Version, error.to_string()))?;
        self.effects
            .push(Effect::Dsp(DspCommand::Request(Request::Text42(text))));
        Ok(VersionExchange::Pending)
    }

    fn set_device_name(&mut self, name: &str) -> DeviceResult<()> {
        let name = (!name.is_empty()).then(|| name.to_owned());
        self.settings(|settings| settings.general.custom_name = name);
        Ok(())
    }

    fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
        self.model_repo.map(<[u8]>::to_vec).ok_or_else(|| {
            ModelError::Refused(
                CortexMessageType::ModelRepo,
                "ModelRepo payload is not installed".into(),
            )
        })
    }

    /// Confirmation (47) `0xf986c`: the answer runs the dialog's ok or cancel callback. An
    /// upload question ([`super::Control::upload_question`]): Ok overwrites (`0x4635f0`),
    /// Cancel skips (`0x463700`; also on disconnect, `0xf9994`). "Preset Conflict"
    /// (`models::preset_conflict`) has empty callbacks.
    fn confirmation_answered(
        &mut self,
        confirmation_id: u64,
        response: tetrad_control::codecs::ConfirmationAction,
    ) -> DeviceResult<()> {
        let Some((_, question)) = self.asked.take_if(|(id, _)| *id == confirmation_id) else {
            return Ok(());
        };
        let overwrite = response == tetrad_control::codecs::ConfirmationAction::Ok;
        let answer = AccountRequest::UploadAnswer {
            question,
            overwrite,
        };
        self.effects.push(Effect::ControlAccount(answer));
        Ok(())
    }

    /// `0x3fcd58` runs a deferred resume callback; the device defers none (inferred).
    fn resume_suspended(&mut self) -> DeviceResult<()> {
        Ok(())
    }

    /// READ and the resync: the working preset with its library item ([`item_preset`]),
    /// reason Other (`0x277468(.., item, 0)`).
    fn current_preset(&mut self) -> DeviceResult<RecallPresetMessage> {
        self.current(CortexMessageType::RecallPreset)?;
        let bytes = item_preset(self.state).unwrap_or_default();
        Ok(RecallPresetMessage {
            preset: Some(RawMessageBytes { bytes }),
            reason: Some(RecallPresetReason::Other.into()),
            ..Default::default()
        })
    }

    /// RecallPreset UPDATE or CREATE (`0x274220`): the BinaryPreset becomes the working
    /// preset as one undoable edit (`0x5b22a8`: direct undo frame `0x648f1c`, then the
    /// restore `0x527d80`). [`Effect::ControlRecall`] deploys the prepared preset and keeps
    /// the frame on the commit; the reply follows with the working preset, the new one or
    /// the live one after a refused deploy. Owned: a message without a preset or with one
    /// that does not decode or import is refused. The capture names pass first
    /// ([`refresh_capture_names`]).
    fn recall_preset(&mut self, recall: &RecallPresetMessage) -> DeviceResult<Reply> {
        let refused = |what: String| ModelError::Refused(CortexMessageType::RecallPreset, what);
        let bytes = recall
            .preset
            .as_ref()
            .ok_or_else(|| refused("no preset".into()))?;
        let mut preset = PresetDocument::parse(&bytes.bytes, tetrad_preset::Limits::default())
            .and_then(|document| Preset::try_from(&document))
            .map_err(|error| refused(error.to_string()))?;
        refresh_capture_names(&mut preset, &self.state.capture_names);
        let state = &*self.state;
        let (Some(specs), Some(current)) = (&state.specs, &*state.current) else {
            return Err(refused("no preset is loaded".into()));
        };
        let ddc = state.settings.settings().general.dynamic_delay_compensation;
        let dormant = preset::dormant_models(specs, &state.licences);
        let prepared = preset::prepare(preset, specs, ddc, Some(&current.deployment), dormant)
            .map_err(|error| refused(error.to_string()))?;
        let undo = state.history.begin(current, state.now, UndoPath::Direct);
        self.effects.push(Effect::ControlRecall {
            prepared: Box::new(prepared),
            undo,
        });
        self.preset_reply = true;
        Ok(Reply::Deferred)
    }

    fn update_grid(&mut self, update: &GridMessage) -> DeviceResult<GridEcho> {
        grid::update(self, update)
    }

    fn delete_grid_preset(&mut self, update: &GridMessage) -> DeviceResult<()> {
        grid::delete(self, update)
    }

    fn move_grid(&mut self, moves: &GridMoveMessage) -> DeviceResult<GridMoved> {
        grid::move_grid(self, moves)
    }

    fn scene(&mut self) -> DeviceResult<u32> {
        let scene = self.current(CortexMessageType::Scene)?.deployment.scene();
        Ok(scene.index() as u32)
    }

    /// Non-forced `setScene` `0x5899d8`: the same scene does nothing.
    fn select_scene(&mut self, scene: u32) -> DeviceResult<()> {
        let index = i32::try_from(scene)
            .ok()
            .and_then(scene_slot)
            .ok_or_else(|| {
                ModelError::Refused(CortexMessageType::Scene, format!("scene {scene}"))
            })?;
        self.current(CortexMessageType::Scene)?;
        self.act(CortexMessageType::Scene, Action::SelectScene(index))
    }

    fn scene_label(&mut self, index: i32) -> DeviceResult<String> {
        let slot = scene_slot(index).ok_or_else(|| {
            ModelError::Refused(CortexMessageType::SceneLabel, format!("scene {index}"))
        })?;
        let labels = &self
            .current(CortexMessageType::SceneLabel)?
            .preset
            .scene_labels;
        Ok(labels.get(slot.index()).cloned().unwrap_or_default())
    }

    /// Stock stores eight labels; missing ones become empty (inferred).
    fn set_scene_label(&mut self, index: i32, label: &str) -> DeviceResult<()> {
        let scene = scene_slot(index).ok_or_else(|| {
            ModelError::Refused(CortexMessageType::SceneLabel, format!("scene {index}"))
        })?;
        let label = label.to_owned();
        self.act(
            CortexMessageType::SceneLabel,
            Action::SceneLabel { scene, label },
        )
    }

    fn scene_color(&mut self, index: i32) -> DeviceResult<u32> {
        let slot = scene_slot(index).ok_or_else(|| {
            ModelError::Refused(CortexMessageType::SceneColor, format!("scene {index}"))
        })?;
        let colors = &self
            .current(CortexMessageType::SceneColor)?
            .preset
            .scene_colors;
        Ok(colors.get(slot.index()).copied().unwrap_or(0))
    }

    /// Stock stores eight packed colors; missing ones become 0 (inferred).
    fn set_scene_color(&mut self, index: i32, color: u32) -> DeviceResult<()> {
        let scene = scene_slot(index).ok_or_else(|| {
            ModelError::Refused(CortexMessageType::SceneColor, format!("scene {index}"))
        })?;
        self.act(
            CortexMessageType::SceneColor,
            Action::SceneColor {
                scene,
                raw_color: color,
            },
        )
    }

    /// `0x1b048c` → `0x51a2a8`, then `0x5899d8(to, force)`. No undo frame.
    fn copy_scene(&mut self, from: i32, to: i32, swap: bool) -> DeviceResult<()> {
        let slot = |index| {
            scene_slot(index).ok_or_else(|| {
                ModelError::Refused(CortexMessageType::SceneCopy, format!("scene {index}"))
            })
        };
        let (first, second) = (slot(from)?, slot(to)?);
        self.act(
            CortexMessageType::SceneCopy,
            Action::CopyScene {
                first,
                second,
                swap,
            },
        )
    }

    /// `0x1ba218` UPDATE → the controller undo (`0x649330`) or redo (`0x649b7c`).
    fn undo_redo_step(&mut self, step: UndoRedoStep) -> DeviceResult<()> {
        let action = match step {
            UndoRedoStep::Undo => Action::Undo,
            UndoRedoStep::Redo => Action::Redo,
        };
        self.act(CortexMessageType::UndoRedo, action)
    }

    /// `0x1ba218` READ: the frame type lists.
    fn undo_redo(&mut self) -> DeviceResult<UndoRedoMessage> {
        let frames = undo_frames(&self.state.history);
        Ok(UndoRedoMessage {
            undo_frames: frames.undo,
            redo_frames: frames.redo,
            ..Default::default()
        })
    }

    /// `0x1a5dc0` READ: the cached flag (`+0x10`).
    fn preset_dirty(&mut self) -> DeviceResult<bool> {
        Ok(self.dirty)
    }

    /// `0x271aa0` READ: the mode, the ATMA page (ATMA only) and the available list.
    fn mode(&mut self) -> DeviceResult<ModeMessage> {
        let mode = &self.state.settings.settings().mode;
        Ok(ModeMessage {
            available_modes: Some(available_modes(mode)),
            ..mode_message(self.state)
        })
    }

    /// `0x271aa0` UPDATE. [`Control::sync`] pushes the changes.
    fn set_mode(&mut self, command: ModeCommand) -> DeviceResult<()> {
        let stored = &self.state.settings.settings().mode;
        match command {
            // `0x58c738(mode, save)`: an unavailable mode becomes the first available one.
            ModeCommand::Mode(mode) => {
                let mode = footswitch_mode(mode)?;
                let available = stored.available();
                let mode = if available.contains(&mode) {
                    mode
                } else {
                    available.first().copied().unwrap_or(mode)
                };
                self.act(CortexMessageType::Mode, Action::SetMode(mode))
            }
            // `0x58ce2c`: the sanitised list (`0x58bcbc`); the action keeps an equal list,
            // else takes the list and its first mode.
            ModeCommand::AvailableModes(modes) => {
                let modes = sanitized_modes(&modes);
                if modes.is_empty() {
                    return Err(ModelError::Refused(
                        CortexMessageType::Mode,
                        "no available mode".into(),
                    ));
                }
                self.act(CortexMessageType::Mode, Action::SetAllowedModes(modes))
            }
            // `0x589c98` keeps the page; `0x589a60` saves `"atmaPage"` only on ATMA. Owned:
            // the QC holds no unsaved page, so the QC ignores it (READ omits it there).
            ModeCommand::AtmaPage(page) => {
                if !self.state.platform.is_atma() {
                    return Ok(());
                }
                let page = i32::try_from(page).map_err(|_| {
                    ModelError::Refused(CortexMessageType::Mode, format!("ATMA page {page}"))
                })?;
                self.settings(|settings| settings.mode.atma_page = page);
                Ok(())
            }
        }
    }

    /// `0x1962c8` READ: the level (`0x1fb830`) and engaged (`0x1fb848`, state 1). On ATMA
    /// every set path writes state 1 and the level is the saved AtmaVolume.
    fn master_volume(&mut self) -> DeviceResult<MasterVolumeMessage> {
        let (volume, engaged) = if self.state.platform.is_atma() {
            (self.state.settings.settings().atma_volume.get(), true)
        } else {
            let volume = &self.state.master_volume;
            (volume.level, volume.engaged())
        };
        Ok(MasterVolumeMessage {
            volume: Some(volume),
            engaged: Some(engaged),
            ..Default::default()
        })
    }

    /// `0x1fbb7c`, then the echo `0x19af74` [`VOLUME_ECHO_DELAY`] later. QC: an equal level
    /// does nothing; else `engaged` false goes out at once when Control did not own the
    /// level (`0x19b518`), and the knob must pick the level up (`input::control_volume`).
    /// ATMA: clamp to 0..1, round to 0.01, the DSP level (`0x511bb0`) and the saved
    /// AtmaVolume (`0x64e754`). Owned: a non-finite level is refused (stock passes NaN on,
    /// `0x1fbc64`).
    fn set_master_volume(&mut self, volume: f32) -> DeviceResult<()> {
        if !self.state.platform.is_atma() {
            let owned = self.state.master_volume.owner == input::VolumeOwner::Control;
            let effects = input::control_volume(self.state, volume);
            if effects.is_empty() {
                return Ok(());
            }
            if !owned {
                self.effects.push(engaged_notice(false));
            }
            self.effects.extend(effects);
            self.volume_echo = Some(self.state.master_volume.level);
            return Ok(());
        }
        let volume = AtmaVolume::new(volume.clamp(0.0, 1.0)).map_err(|error| {
            ModelError::Refused(CortexMessageType::MasterVolume, error.to_string())
        })?;
        self.effects.push(actions::master_volume(volume.get()));
        self.settings(|settings| settings.atma_volume = volume);
        self.volume_echo = Some(volume.get());
        Ok(())
    }

    /// `0x1b8244` READ: the Gig View is visible.
    fn gig_view_shown(&mut self) -> DeviceResult<bool> {
        Ok(self.state.screen == tetrad_ui::Screen::Gig)
    }

    /// `0x1b8244` UPDATE shows or hides the Gig View.
    fn show_gig_view(&mut self, show: bool) -> DeviceResult<()> {
        self.act(CortexMessageType::ShowGigView, Action::ShowGigView(show))
    }

    fn compiler_inhibited_modules(&mut self) -> DeviceResult<CompilerInhibitedModulesMessage> {
        let (gate, eq) = inhibited_modules(self.capture_lock, self.state);
        Ok(CompilerInhibitedModulesMessage {
            global_gate: Some(gate),
            global_eq: Some(eq),
            ..Default::default()
        })
    }

    fn setlist_position(&mut self) -> DeviceResult<SetlistPositionMessage> {
        Ok(setlist_position(self.state))
    }

    /// User setlists only; factory, downloads and plugin folders are not in the library.
    fn set_setlist_position(&mut self, position: &SetlistPositionMessage) -> DeviceResult<()> {
        let elsewhere = [
            position.is_factory,
            position.is_downloads,
            position.is_plugin,
        ];
        if elsewhere.contains(&Some(true)) {
            return Err(ModelError::Refused(
                CortexMessageType::SetlistPosition,
                "factory, downloads and plugin folders".into(),
            ));
        }
        let target = PresetRef {
            setlist: position.folder_key.clone().filter(|key| !key.is_empty()),
            index: position.position.unwrap_or(0),
        };
        self.act(
            CortexMessageType::SetlistPosition,
            Action::LoadPreset {
                target,
                reload: true,
            },
        )
    }

    fn tuner(&mut self) -> DeviceResult<TunerMessage> {
        let tuner = &self.state.settings.settings().tuner;
        Ok(TunerMessage {
            input_port_id: Some(tuner.input.to_stock()),
            frequency: Some(tuner.reference_offset_hz()),
            mute: Some(tuner.mutes_outputs),
            meter: Some(self.state.tuner_hz),
            ..Default::default()
        })
    }

    /// Tuner settings and meter subscriptions use the shared reducer and DSP path.
    fn update_tuner(&mut self, command: TunerCommand) -> DeviceResult<()> {
        let refused = |what: String| ModelError::Refused(CortexMessageType::Tuner, what);
        match command {
            TunerCommand::InputPort(port) => {
                let choice = InputPortId::from_stock(port)
                    .ok()
                    .and_then(|input| {
                        InputPortId::TUNER_CHOICES
                            .iter()
                            .position(|choice| *choice == input)
                    })
                    .ok_or_else(|| refused(format!("input port {port}")))?;
                self.act(
                    CortexMessageType::Tuner,
                    Action::ChangeSetting {
                        id: tetrad_ui::SettingId::TunerInput,
                        value: tetrad_ui::SettingValue::Choice(choice as u16),
                    },
                )?;
            }
            TunerCommand::ReferenceOffset(offset) => {
                let mut tuner = self.state.settings.settings().tuner.clone();
                tuner
                    .set_reference_offset_hz(offset)
                    .map_err(|error| refused(error.to_string()))?;
                self.act(
                    CortexMessageType::Tuner,
                    Action::SetTunerReference(tuner.reference_offset_hz()),
                )?;
            }
            TunerCommand::Mute(mute) => {
                self.act(
                    CortexMessageType::Tuner,
                    Action::ChangeSetting {
                        id: tetrad_ui::SettingId::TunerMute,
                        value: tetrad_ui::SettingValue::Bool(mute),
                    },
                )?;
            }
            TunerCommand::EnableMeter(enabled) => self.act(
                CortexMessageType::Tuner,
                Action::TunerMeterSubscribed(enabled),
            )?,
        }
        Ok(())
    }

    fn show_tuner(&mut self, show: bool) -> DeviceResult<()> {
        self.act(CortexMessageType::Tuner, Action::ShowTuner(show))
    }

    fn global_tempo(&mut self) -> DeviceResult<tetrad_control::codecs::GlobalTempoMessage> {
        let controller = super::super::features::tempo_controller(self.state).map_err(|error| {
            ModelError::Refused(CortexMessageType::GlobalTempo, error.to_string())
        })?;
        Ok(controller.read_response())
    }

    fn set_global_tempo_param(&mut self, index: u32, value: f32) -> DeviceResult<()> {
        self.act(
            CortexMessageType::GlobalTempo,
            Action::SetTempoParameter {
                index,
                value,
                remote: true,
            },
        )
    }

    /// Looper READ `0x18bad8`: the controller's record copy (`+0xb828`), its state
    /// (`+0xb824`), one-shot play (`+0xb880`), duplicate sync wait (`+0xb881`) and
    /// `quantize_enabled` (`+0xb882`, writers `0x513df0`/`0x513dbc`/`0x513e24`; a presence
    /// field, false without a Looper X: S7 capture `[121]`). Owned: refused before the first
    /// DSP record (stock sends a zero record).
    fn looper(&mut self) -> DeviceResult<LooperMessage> {
        let status = self.state.looper.ok_or_else(|| {
            ModelError::Refused(CortexMessageType::Looper, "no DSP looper status yet".into())
        })?;
        let playback = &self.state.looper_playback;
        Ok(LooperMessage {
            action: Some(MessageAction::Update.into()),
            status: Some(looper_status(&status)),
            state: Some(playback.cached_state),
            one_shot_play: Some(playback.oneshot),
            sync_start_waiting: Some(playback.dupwait),
            quantize_enabled: Some(super::looper::quantize_enabled(self.state).unwrap_or(false)),
            ..Default::default()
        })
    }

    /// Every model flagged new, over all firmware versions, in settings order. An expired list
    /// holds the `0` marker (`0x002053ec`), which names no model: the S7 capture QC (every
    /// list expired) answers no ids (C).
    fn new_models(&mut self) -> DeviceResult<Vec<u32>> {
        let new = &self.state.settings.settings().new_models.0;
        Ok(new
            .values()
            .flatten()
            .filter(|&&id| id != 0)
            .filter_map(|&id| u32::try_from(id).ok())
            .collect())
    }

    fn io_settings(&mut self, request: &IOSettingsMessage) -> DeviceResult<IOSettingsMessage> {
        Ok(io::read(self.state, request))
    }

    /// One change per UPDATE; each action echoes its one-port UPDATE (`0x26dafc`).
    fn update_io_settings(&mut self, command: IoCommand) -> DeviceResult<()> {
        if let IoCommand::LoadPreset(preset) = command {
            let load = io::load_preset(self, &preset)?;
            self.effects.push(Effect::Action(load));
            return Ok(());
        }
        if let IoCommand::ExpCalibrating { pedal, calibrating } = command {
            // `0x358330` starts tracking (`0x32103c`); `0x35828c` stores the tracked range
            // when there is one (`0x320eb8` → `0x320a34`). Both echo the flag (`0x26fbd4`).
            self.effects
                .push(Effect::ExpressionCalibration { pedal, calibrating });
            let echo = ControlMessage::IOSettings(command.to_update());
            self.effects.push(Effect::ControlNotify(Box::new(echo)));
            return Ok(());
        }
        let actions = io::update(self.state, command)
            .map_err(|what| ModelError::Refused(CortexMessageType::IOSettings, what))?;
        for action in actions {
            self.act(CortexMessageType::IOSettings, action)?;
        }
        Ok(())
    }

    /// License UPDATE `0x113094` → `0x1f0e80`; no reply. The services bridge reports the
    /// result.
    fn refresh_licenses(&mut self) -> DeviceResult<()> {
        self.effects
            .push(Effect::ControlAccount(AccountRequest::RefreshLicences));
        Ok(())
    }

    fn neural_capture(
        &mut self,
        request: &tetrad_control::codecs::NeuralCaptureMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::NeuralCaptureMessage>> {
        self.effects.extend(nc1::neural_capture(request)?);
        Ok(Vec::new())
    }

    fn neural_capture2(
        &mut self,
        request: &tetrad_control::codecs::NeuralCapture2Message,
    ) -> DeviceResult<Vec<tetrad_control::codecs::NeuralCapture2Message>> {
        self.effects
            .extend(nc2::neural_capture2(request, self.state)?);
        Ok(Vec::new())
    }

    fn file(
        &mut self,
        request: &tetrad_control::codecs::FileMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::FileMessage>> {
        files::file(self, request)
    }

    fn file_listing(&mut self) -> DeviceResult<Vec<tetrad_control::codecs::FileMessage>> {
        files::listing(self)
    }

    /// Screenshot (25) READ: the stored preset's grid as a PNG ([`screen::screenshot`]).
    fn request_screenshot(
        &mut self,
        request: &tetrad_control::codecs::ScreenshotMessage,
    ) -> DeviceResult<()> {
        let reply = screen::screenshot(self.state, request)?;
        self.effects
            .push(Effect::ControlNotify(Box::new(reply.into())));
        Ok(())
    }

    /// RemoteControl (72) `0x133d2c`. READ with `screenshot` grabs the window
    /// ([`Effect::ControlFrame`]; the reply follows the render). Not built: the graphics tree
    /// (`0x49dfcc`, a Qt item dump), CREATE/DELETE (forward device mouse events, `0x49d714`)
    /// and UPDATE (synthesise mouse events, `0x49eaf4`).
    fn remote_control(
        &mut self,
        request: &tetrad_control::codecs::RemoteControlMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::RemoteControlMessage>> {
        let read = request.action == Some(MessageAction::Read.into());
        if !read || request.screenshot.is_none() {
            return Err(ModelError::Unsupported(CortexMessageType::RemoteControl));
        }
        self.effects.push(Effect::ControlFrame);
        Ok(Vec::new())
    }

    /// BulkOperation (57) READ `0xed8d0`: `is_multiselection_active` is Directory Multi
    /// Select (`0x843c60`). While a run (upload, multi-key DELETE or download) runs and its
    /// overlay shows, also the overlay text, bar value, blocking, cancellable, the stored
    /// folders and the type (re-r44 §2 step 4). The other multi-item library jobs run inside
    /// one device step.
    fn bulk_operation(&mut self) -> DeviceResult<tetrad_control::codecs::BulkOperationMessage> {
        Ok(tetrad_control::codecs::BulkOperationMessage {
            is_multiselection_active: Some(self.multi_select),
            ..self.bulk.cloned().unwrap_or_default()
        })
    }

    /// BulkOperation DELETE `0xed8d0` sets the abort flag (`+0x7c`): a running run stops
    /// before its next item ([`AccountRequest::CancelBulkRun`]; nothing when none runs). The
    /// other bulk jobs run inside one device step.
    fn cancel_bulk_operation(&mut self) -> DeviceResult<()> {
        let cancel = Effect::ControlAccount(AccountRequest::CancelBulkRun);
        self.queue(
            tetrad_control::codecs::CortexMessageType::BulkOperation,
            cancel,
        )
    }

    /// EnableCaptureOut (16) `0x240108`: `SetNCOutSel(enable, 0, 3, 0, 25000)` with no guard;
    /// the receiver then replies with the value.
    fn enable_capture_out(&mut self, enable: bool) -> DeviceResult<()> {
        let output = if enable {
            NcOutput::Capture
        } else {
            NcOutput::Normal
        };
        self.effects
            .push(Effect::Hal(HalCommand::nc_output(output)));
        Ok(())
    }

    /// Updater (60) `0x1391b4`: READ (action 3) replies the view, UPDATE runs `command`.
    fn updater(
        &mut self,
        request: &tetrad_control::codecs::UpdaterMessage,
    ) -> DeviceResult<Option<tetrad_control::codecs::UpdaterMessage>> {
        if request.action == Some(MessageAction::Read.into()) {
            return Ok(Some(self.updater.read()));
        }
        updater::update(
            &self.updater,
            request,
            self.control_online,
            &mut self.effects,
        )
    }

    /// The link matched the response to an open forward; the bridge passes it on after the
    /// step.
    fn forward_response(&mut self, response: ControlMessage, last: bool) -> DeviceResult<()> {
        self.forwarded.push((response, last));
        Ok(())
    }

    /// SystemTimeSync UPDATE `0x1ba1b0`: the services bridge sets the clock while unset.
    fn set_system_time(&mut self, ms_since_epoch: u64) -> DeviceResult<()> {
        self.effects
            .push(Effect::ControlAccount(AccountRequest::SetClock(
                ms_since_epoch,
            )));
        Ok(())
    }

    fn licenses(&mut self) -> DeviceResult<tetrad_control::codecs::LicenseMessage> {
        Ok(account::licences(self.state))
    }

    fn acknowledge_new_plugin(&mut self, plugin: u32) -> DeviceResult<()> {
        account::acknowledge_new_plugin(self, plugin)
    }

    fn cloud_login(&mut self, request: &CloudLoginMessage) -> DeviceResult<Vec<CloudLoginMessage>> {
        account::cloud_login(self, request)
    }

    fn cloud_backup(
        &mut self,
        request: &tetrad_control::codecs::CloudBackupMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::CloudBackupMessage>> {
        cloud::request(self, request)
    }

    /// Logs (44) CREATE `0x114cd4`: stock ignores `correlation_id` and makes its own
    /// (`0x182868`). The upload is the support report's, which Tetrad never sends (O), so the
    /// services bridge refuses it.
    fn upload_logs(&mut self, _correlation_id: Option<&str>) -> DeviceResult<()> {
        self.effects
            .push(Effect::ControlAccount(AccountRequest::UploadLogs));
        Ok(())
    }

    /// ProcessDownloadsQueue (45) UPDATE `0x1326a8`: logged out does nothing; logged in,
    /// stock refreshes the cloud downloads model of `queue_type` (0 presets `+0x70`, else
    /// captures `+0x74`; vtable `+200`, then `+0x7c(-1, -1, 0, 0)` unless that Downloads
    /// folder is on screen). Tetrad runs the downloads-queue client of that kind (I: that
    /// `0x1326a8` reaches the stock fetchMore `0x431178` is not traced); each stored product
    /// pushes `0x250634`. The stores write the library: bring-up refuses the request without
    /// `--persist`.
    fn process_downloads_queue(&mut self, queue_type: Option<i32>) -> DeviceResult<()> {
        use tetrad_services::cloud::services::ProductKind;
        if self.state.settings.settings().account.is_logged_out() {
            return Ok(());
        }
        let kind = match queue_type.unwrap_or_default() {
            0 => ProductKind::Preset,
            _ => ProductKind::NeuralCapture,
        };
        let request = Effect::ControlAccount(AccountRequest::DownloadsQueue(kind));
        self.queue(CortexMessageType::ProcessDownloadsQueue, request)
    }

    /// CloudProduct (46): [`products`].
    fn cloud_product(
        &mut self,
        request: &tetrad_control::codecs::CloudProductMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::CloudProductMessage>> {
        products::cloud_product(self, request)
    }

    /// GainCalibration (62) `0x101bb8` (PA `0x101bcc`, same body; r23-control §4). Stock
    /// acts only with the calibration view (`0x545694`) on an ESS board (`0x497fd8`): UPDATE
    /// sets live output gains (`0x36ea84`) and echoes them, COPY saves them to the U-Boot
    /// environment (`0x36f3c4` → `fw_setenv` `0x47c310`). Tetrad refuses both on ESS: a U-Boot
    /// write on a device that never ran Tetrad can leave it unbootable. Without ESS stock does
    /// nothing, so Tetrad does nothing.
    fn gain_calibration(
        &mut self,
        _request: &tetrad_control::codecs::GainCalibrationMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::GainCalibrationMessage>> {
        if !self.state.platform.has_ess_codec() {
            return Ok(Vec::new());
        }
        Err(ModelError::Refused(
            CortexMessageType::GainCalibration,
            "Tetrad does not write the U-Boot environment (gain calibration)".into(),
        ))
    }

    fn io_meters(&mut self) -> DeviceResult<tetrad_control::codecs::IOMeterMessage> {
        Ok(status::io_meters(self.state))
    }

    fn diagnostics(&mut self) -> DeviceResult<tetrad_control::codecs::DiagnosticsMessage> {
        Ok(status::diagnostics(self.diagnostics))
    }

    fn module_stats(&mut self) -> DeviceResult<tetrad_control::codecs::ModuleStatsMessage> {
        Ok(status::module_stats(self.state))
    }

    fn cpu_load(&mut self) -> DeviceResult<tetrad_control::codecs::CPULoadMessage> {
        Ok(status::cpu_load(self.state))
    }

    fn global_eq(&mut self) -> DeviceResult<tetrad_control::codecs::GlobalEQMessage> {
        settings::global_eq_message(self.state).ok_or_else(|| {
            ModelError::Refused(
                CortexMessageType::GlobalEQ,
                "no Global EQ model (4004)".into(),
            )
        })
    }

    fn set_global_eq_parameter(
        &mut self,
        parameter: &tetrad_control::codecs::GlobalEQParameter,
    ) -> DeviceResult<()> {
        settings::set_eq_parameter(self, parameter)
    }

    fn set_global_eq_bypass(&mut self, bypassed: bool) -> DeviceResult<()> {
        settings::set_eq_bypass(self, bypassed)
    }

    fn load_global_eq_preset(
        &mut self,
        preset: &tetrad_control::codecs::ModelPresetID,
    ) -> DeviceResult<()> {
        settings::load_eq_preset(self, preset)
    }

    fn update_midi_settings(&mut self, update: &MIDISettingsMessage) -> DeviceResult<()> {
        settings::update_midi(self, update)
    }

    fn general_settings(&mut self) -> DeviceResult<GeneralSettingsMessage> {
        Ok(settings::general_read(self))
    }

    fn update_general_settings(&mut self, update: &GeneralSettingsMessage) -> DeviceResult<()> {
        settings::update_general(self, update)
    }

    fn recents_favorites(
        &mut self,
        request: &tetrad_control::codecs::RecentsFavoritesMessage,
    ) -> DeviceResult<Option<tetrad_control::codecs::RecentsFavoritesMessage>> {
        library::recents_favorites(self, request)
    }

    fn recent_searches(&mut self) -> DeviceResult<Vec<String>> {
        library::recent_searches(self)
    }

    fn store_recent_searches(&mut self, searches: &[String]) -> DeviceResult<()> {
        library::store_recent_searches(self, searches)
    }

    fn clear_recent_searches(&mut self) -> DeviceResult<()> {
        library::clear_recent_searches(self)
    }

    fn pinned_models(&mut self) -> DeviceResult<(Vec<u32>, Vec<String>)> {
        library::pinned_models(self)
    }

    fn pin_models(&mut self, pin: bool, models: &[u32], captures: &[String]) -> DeviceResult<()> {
        library::pin_models(self, pin, models, captures)
    }

    fn model_preset(
        &mut self,
        request: &tetrad_control::codecs::ModelPresetMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::ModelPresetMessage>> {
        models::model_preset(self, request)
    }

    fn default_param_ids(&mut self) -> DeviceResult<Vec<String>> {
        resync::default_param_ids(self)
    }

    fn capture_default(
        &mut self,
        key: &str,
    ) -> DeviceResult<Option<tetrad_control::block::CaptureDefault>> {
        resync::capture_default(self, key)
    }

    fn model_preset_records(
        &mut self,
    ) -> DeviceResult<Vec<tetrad_control::block::ModelPresetRecord>> {
        resync::model_preset_records(self)
    }

    fn save_default_parameters(&mut self, row: i32, column: i32) -> DeviceResult<()> {
        models::save_defaults(self, row, column)
    }

    fn apply_default_parameters(
        &mut self,
        update: &tetrad_control::codecs::DefaultParametersMessage,
    ) -> DeviceResult<GridEcho> {
        let (row, column) = (update.row.unwrap_or(0), update.column.unwrap_or(0));
        models::reset_defaults(self, row, column)
    }

    /// LocalBackup (40) CREATE and `can_apply_backup` (`0x17d104`, [`backup::request`]).
    fn local_backup(
        &mut self,
        request: &tetrad_control::codecs::LocalBackupMessage,
    ) -> DeviceResult<Vec<tetrad_control::codecs::LocalBackupMessage>> {
        backup::request(self, request)
    }

    fn apply_local_backup(
        &mut self,
        backup_json: String,
    ) -> DeviceResult<Vec<tetrad_control::codecs::LocalBackupMessage>> {
        backup::apply(self, backup_json)
    }

    /// `sendGridModelMeters` `0x110194` → `0x25da6c`, skipped while a capture preset runs
    /// (`+0x758`): one Grid UPDATE whose body has a chain per row (`row`) with an active row
    /// meter (`input_control` parameter 2, [`meters::row_meter_level`]) or model meter values. Each
    /// block with values is a model with its `column` and one parameter per meter (`index`,
    /// `is_led` for a `floatWithLed` parameter, type 12; the value). Row meters are
    /// `State::row_meters`, model meters `State::model_meters` (`0x110f60`). Rows and blocks
    /// are the subscribed ones (I: stock keeps one map and clears it only on DELETE All). A row
    /// meter goes only while the row's Input Gate is on (`0x110194`: the subscription `0x4df684`
    /// and not `+0x390`, set by `inputGateBypassChangedSlot` `0x110298` from `0x515140`: the
    /// Input Gate inhibit flags `+0x3794`/`+0x3795`, or BYPASS (parameter 1) above 0 at the
    /// current scene). Nothing to send sends nothing.
    fn grid_model_meters(
        &mut self,
        targets: &[tetrad_control::device::MeterTarget],
    ) -> DeviceResult<Vec<ControlMessage>> {
        use tetrad_control::device::MeterTarget;
        if self.capture_running {
            return Ok(Vec::new());
        }
        let subscribed = |row: usize, column: Option<usize>| {
            (targets.iter()).any(|target| match (*target, column) {
                (MeterTarget::All, _) => true,
                (MeterTarget::Row(subscribed), None) => usize::from(subscribed) == row,
                (MeterTarget::Model { row: r, column: c }, Some(column)) => {
                    usize::from(r) == row && usize::from(c) == column
                }
                _ => false,
            })
        };
        let value = |value: f32| SceneValue {
            value: Some(ParamValue::Float(value)),
            ..Default::default()
        };
        let specs = self.state.specs.as_ref();
        let current = self.state.current.as_ref();
        let gate_on = |row: usize| {
            (u8::try_from(row).ok().and_then(|row| Row::new(row).ok()))
                .is_some_and(|row| meters::gate_on(self.state, row))
        };
        let chains: Vec<Chain> = (0..self.state.row_meters.len())
            .filter_map(|row| {
                let row_meter =
                    (self.state.row_meters[row]).filter(|_| subscribed(row, None) && gate_on(row));
                let models: Vec<Model> = (self.state.model_meters.cells())
                    .filter(|(slot, values)| {
                        slot.row.index() == row
                            && !values.is_empty()
                            && subscribed(row, Some(slot.column.index()))
                    })
                    .map(|(slot, values)| {
                        let spec = (current.zip(specs)).and_then(|(current, specs)| {
                            let site = tetrad_engine::Site::Slot(slot);
                            let layout = current.deployment.layout();
                            specs.model(layout.instance(site)?.model.0)
                        });
                        let led = |index: u32| {
                            (spec.and_then(|spec| spec.parameter(index)))
                                .is_some_and(|param| {
                                    param.kind == tetrad_catalog::ParameterType::FloatWithLed
                                })
                                .then_some(true)
                        };
                        Model {
                            column: Some(slot.column.index() as u32),
                            params: (values.iter())
                                .map(|(&index, &v)| Param {
                                    index: Some(index),
                                    is_led: led(index),
                                    values: vec![value(v)],
                                    ..Default::default()
                                })
                                .collect(),
                            ..Default::default()
                        }
                    })
                    .collect();
                (row_meter.is_some() || !models.is_empty()).then(|| Chain {
                    row: Some(row as u32),
                    input_control: (row_meter.into_iter())
                        .map(|meter| Model {
                            params: vec![Param {
                                index: Some(2),
                                values: vec![value(meters::row_meter_level(meter, specs))],
                                ..Default::default()
                            }],
                            ..Default::default()
                        })
                        .collect(),
                    models,
                    ..Default::default()
                })
            })
            .collect();
        if chains.is_empty() {
            return Ok(Vec::new());
        }
        let preset = Preset {
            chains,
            ..Default::default()
        };
        Ok(vec![ControlMessage::Grid(GridMessage {
            action: Some(MessageAction::Update.into()),
            preset: Some(RawMessageBytes {
                bytes: preset.to_bytes(),
            }),
            ..Default::default()
        })])
    }

    /// GigViewButton (55) UPDATE `0x109ab4`: the input bridge bounds the tile (8, mini 4) and
    /// runs the press or release; an index above 255 is out of range there too.
    fn gig_view_button(&mut self, index: u64, pressed: bool) -> DeviceResult<()> {
        if let Ok(tile) = u8::try_from(index) {
            self.effects.push(Effect::GigViewButton { tile, pressed });
        }
        Ok(())
    }

    fn clear_new_model(&mut self, model: u32) -> DeviceResult<()> {
        let id = i32::try_from(model).map_err(|_| {
            ModelError::Refused(CortexMessageType::NewModels, format!("model {model}"))
        })?;
        self.settings(|settings| {
            for ids in settings.new_models.0.values_mut() {
                ids.retain(|&other| other != id);
            }
        });
        Ok(())
    }
}
