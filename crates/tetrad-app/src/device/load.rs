//! Preset load, recall, save and the deploy paths of the working preset.

use tetrad_catalog::Specs;
use tetrad_control::codecs::RecallPresetReason;
use tetrad_engine::{Deployment, Update};
use tetrad_library::sidecar::{Creator, Instrument};
use tetrad_preset::Preset;

use super::platform::{DspFile, Marker, Platform};
use super::services::PresetFile;
use super::{
    Action, Current, Device, DeviceError, DspCommand, Effect, FACTORY_SETLIST, PresetRef, State,
    Subsystem, actions, bringup, control, dsp, features, media, preset, services,
};

impl<P: Platform> Device<P> {
    /// Driver preset writes after an effect (`Dsp::take_linked_edits`): a controller write is
    /// stored with the `0x51e4d8` scene rule, a scene-hook write in the recalled scene's slot
    /// only (`0x2e8160`, review r15 R19); engine ops are dropped (the driver sent its DSP
    /// writes). A controller write also echoes to Cortex Control (`0x25d51c`, at once; stock
    /// holds it 100 ms). The next frame draws the value.
    pub(super) fn store_linked_edits(&mut self) {
        let Some(edits) = self.dsp.as_mut().map(dsp::Dsp::take_linked_edits) else {
            return;
        };
        for (slot, edit) in edits {
            let (Some(specs), Some(current)) = (&self.state.specs, self.state.current.get_mut())
            else {
                return;
            };
            let site = tetrad_engine::Site::Slot(slot);
            let stored = if edit.echo {
                preset::set_parameter(current, specs, site, edit.index, edit.value).map(drop)
            } else {
                preset::store_scene_slot(current, specs, site, edit.index, edit.value)
            };
            match stored {
                Ok(()) if edit.echo => {
                    let echo = dsp::block_echo(slot, edit.index, edit.value);
                    self.execute(Effect::ControlNotify(Box::new(echo)));
                }
                Ok(()) => {}
                Err(error) => self.notice(
                    Subsystem::Preset,
                    format!("driver write {slot:?} {}: {error}", edit.index),
                ),
            }
        }
    }

    /// Stock scene recall (`0x527248`) of the committed deployment. Owned: while a capture
    /// preset runs the DSP keeps it; the revert plays the user preset at its scene.
    pub(super) fn recall_scene(
        &mut self,
        metronome: Option<&crate::tempo::ResendPlan>,
    ) -> Result<(), DeviceError> {
        if self.bridges.capture.capture_running() {
            return Ok(());
        }
        let (Some(dsp), Some(specs), Some(current)) =
            (self.dsp.as_mut(), &self.state.specs, &*self.state.current)
        else {
            return Err(DeviceError::Unavailable(
                "the DSP, the catalog and a loaded preset",
            ));
        };
        let (media, failures) = resolve_media(
            &mut self.media,
            &mut self.platform,
            current.deployment.plan(),
            &self.state,
        );
        let context = context(&self.state, specs, &current.deployment, media);
        let pending = dsp.recall_scene(&context, metronome)?;
        self.note_failures(failures);
        self.note_pending(Subsystem::Dsp, pending);
        Ok(())
    }

    /// One update of the committed deployment (`Dsp::apply`, `0x585464`/`0x51e4d8`). Owned:
    /// while a capture preset runs only `State` changes, as in [`Self::deploy_layout`]
    /// (review r11 R4).
    pub(super) fn deploy(&mut self, update: &Update) -> Result<(), DeviceError> {
        if self.bridges.capture.capture_running() {
            return Ok(());
        }
        let (Some(dsp), Some(specs), Some(current)) =
            (self.dsp.as_mut(), &self.state.specs, &*self.state.current)
        else {
            return Err(DeviceError::Unavailable(
                "the DSP, the catalog and a loaded preset",
            ));
        };
        let (media, failures) = resolve_media(
            &mut self.media,
            &mut self.platform,
            current.deployment.plan(),
            &self.state,
        );
        let context = dsp::Context {
            nopoll: self
                .platform
                .marker(Marker::NoPoll)
                .map_err(DeviceError::platform)?,
            ..context(&self.state, specs, &current.deployment, media)
        };
        let pending = dsp.apply(&mut self.platform, update, &context)?;
        self.note_failures(failures);
        self.note_pending(Subsystem::Dsp, pending);
        Ok(())
    }

    /// [`Effect::RestoreLayout`]: `History::step` runs before the commit (it stores the live
    /// preset) and is undone when the deploy fails. Stock `0x649330` pushes the working
    /// preset whatever `0x527d80` did, so a failed deploy still pushes the unchanged preset.
    pub(super) fn restore_layout(
        &mut self,
        change: preset::LayoutChange,
        redo: bool,
    ) -> Result<(), DeviceError> {
        let before = self.state.history.clone();
        let depths = before.depths();
        if let Some(current) = &*self.state.current {
            self.state.history.step(current, redo);
        }
        let result = self.deploy_layout(change);
        if result.is_err() {
            self.state.history = before;
        }
        // The live preset: the frame after a commit, else the unchanged one.
        for effect in control::undo_pushes(&self.state, depths, redo) {
            self.execute(effect);
        }
        result
    }

    /// Deploys and commits a layout change; its Cortex Control undo frame is kept on the
    /// commit. Owned: while a capture preset runs only `State` changes (as a preset load);
    /// the revert deploys the user preset.
    pub(super) fn deploy_layout(
        &mut self,
        mut change: preset::LayoutChange,
    ) -> Result<(), DeviceError> {
        let capturing = self.bridges.capture.capture_running();
        let (Some(dsp), Some(specs)) = (self.dsp.as_mut(), &self.state.specs) else {
            return Err(DeviceError::Unavailable("the DSP and the catalog"));
        };
        // Probe units (A3): the media of the plan and the DSP push of a drop.
        self.probe.enter();
        let (media, failures) = resolve_media(
            &mut self.media,
            &mut self.platform,
            change.deployment().plan(),
            &self.state,
        );
        self.probe.leave(super::probe::Label::new("resolve_media"));
        let (expression, links) =
            features::expression_for(change.preset(), &self.state.pedals, &self.state.expression);
        let mut context = context(&self.state, specs, change.deployment(), media);
        context.expression = Some(&expression);
        context.nopoll = self
            .platform
            .marker(Marker::NoPoll)
            .map_err(DeviceError::platform)?;
        self.probe.enter();
        let pending = (!capturing)
            .then(|| dsp.apply(&mut self.platform, &change.update, &context))
            .transpose();
        self.probe.leave(super::probe::Label::new("Dsp apply"));
        let pending = pending?;
        let undo = change.take_undo();
        change.commit(
            self.state
                .current
                .get_mut()
                .as_mut()
                .ok_or(DeviceError::Unavailable("a loaded preset"))?,
        );
        if let Some(undo) = undo {
            self.state.history.commit(undo);
        }
        self.note_failures(failures);
        if let Some(pending) = pending {
            self.note_pending(Subsystem::Dsp, pending);
        }
        for detail in links {
            self.notice(Subsystem::Preset, detail);
        }
        let refresh = actions::reduce(
            Action::ExpressionDeployed(Box::new(expression)),
            &mut self.state,
        );
        self.apply(refresh);
        // `0x5122c0` ends every push with the tempo (`0x875a48`); only a preset load and a
        // scene recall resend the metronome.
        if !capturing {
            let tempo = self.dsp_command(DspCommand::Tempo(actions::tempo_bpm(&self.state)));
            self.report(tempo);
        }
        Ok(())
    }

    /// One input gate edit (`Dsp::apply_gate`, `0x51e4d8` case 0x6d60).
    pub(super) fn deploy_gate(
        &mut self,
        update: &tetrad_engine::GateUpdate,
    ) -> Result<(), DeviceError> {
        let (Some(dsp), Some(specs), Some(current)) =
            (self.dsp.as_mut(), &self.state.specs, &*self.state.current)
        else {
            return Err(DeviceError::Unavailable(
                "the DSP, the catalog and a loaded preset",
            ));
        };
        let context = context(&self.state, specs, &current.deployment, None);
        let pending = dsp.apply_gate(&context, update)?;
        self.note_pending(Subsystem::Dsp, pending);
        Ok(())
    }

    /// Stock save (`0x5b7438`): the loaded preset to its file, or to its empty slot under
    /// `name` (taken by another preset: the first empty slot of its setlist, review r41 L2). A requested name on a preset with a file is Edit Details (`0x5b91dc`): the
    /// file then follows the name. `copy_to` is Save As into that setlist and slot; the copy
    /// becomes the loaded slot and LastPreset. Every undo frame takes the saved name (`0x647878`).
    /// The date comes from the platform wall clock; the author fallback is the signed-in
    /// user name (the account id is not stored in settings: empty). Review r39 K8 (O): a
    /// refused file rename after a good save is a notice; the save itself is done.
    pub(super) fn save_preset(
        &mut self,
        name: Option<String>,
        copy_to: Option<(Option<String>, Option<usize>)>,
        instrument: tetrad_ui::PreferredInstrument,
    ) -> Result<(), DeviceError> {
        let now = self
            .platform
            .wall_clock()
            .ok_or(DeviceError::Unavailable("a wall clock for the save date"))?;
        let slot = PresetRef {
            setlist: self.state.settings.settings().last_preset.setlist.clone(),
            index: self.state.performance.footswitch.current_preset,
        };
        // Review r41 L2 (O): a preset with no file whose own slot holds another preset by now
        // (its file was deleted, the slot filled again) saves as Save As into the first empty
        // slot of that setlist (`0x7c3d94`) and loads from there; the other preset stays.
        let unfiled = (self.state.current.as_ref()).is_some_and(|current| current.path.is_none());
        let copy_to = match copy_to {
            None if unfiled && matches!(self.slot_path(&slot), Ok(Some(_))) => {
                Some((slot.setlist.clone(), None))
            }
            copy_to => copy_to,
        };
        // (O) Held Custom mode momentary bypasses save at rest too.
        let custom_held = super::custom::held_bypass_slots(&self.state);
        let custom_down = self.bridges.input.custom_down();
        let state = &mut self.state;
        let (Some(specs), Some(current), Some(library)) =
            (&state.specs, state.current.get_mut(), &mut state.library)
        else {
            return Err(DeviceError::Unavailable(
                "the catalog, a loaded preset and the library",
            ));
        };
        let renamed = name.is_some() && current.path.is_some() && copy_to.is_none();
        let new_file = current.path.is_none() || copy_to.is_some();
        let name = preset::save_name(current, name).ok_or(actions::ActionError::Unsupported(
            "a new preset needs a name (Save As)",
        ))?;
        let account = preset::SaveAccount::of(state.settings.settings());
        let request = preset::SaveRequest {
            slot: &slot,
            name: &name,
            now,
            account_name: &account.name,
            account_id: &account.id,
            creator: match state.platform.is_atma() {
                true => Creator::QuadCortexMini,
                false => Creator::Quad,
            },
        };
        // `0x51898c` (QC only): blocks of held momentary stomps save their bypass inverted.
        let held = if state.platform.is_atma() {
            0
        } else {
            state.performance.held & !custom_down
        };
        let licences = &state.licences;
        let locked = |model| {
            (specs.model(model).and_then(|spec| spec.plugin_id))
                .is_some_and(|plugin| licences.status(plugin) != services::PluginStatus::Active)
        };
        // Review r41 L1: the stamp of the file a rewrite in place replaces.
        let rewritten = (current.path.as_deref())
            .filter(|_| !new_file)
            .and_then(|path| library.presets.media().resolve(path).ok())
            .map(PresetFile::read);
        // The clean frame is the saved copy, so a held momentary stomp shows clean after its
        // release (review r14 P-10).
        let (saved, clean) =
            preset::save_resting(held, &custom_held, specs, locked, current, |current| {
                let saved = match copy_to {
                    Some((setlist, slot)) => preset::save_as_with(
                        &mut library.presets,
                        current,
                        specs,
                        &request,
                        (setlist.as_deref(), slot),
                        instrument,
                    )
                    .map(|(saved, slot)| (saved, Some(slot))),
                    None => preset::save_with(
                        &mut library.presets,
                        current,
                        specs,
                        &request,
                        instrument,
                    )
                    .map(|saved| (saved, None)),
                };
                (saved, preset::Snapshot::of(current))
            });
        let (saved, copy) = saved?;
        // `0x5b7438` → `0x51327c` (`0x2e0df8`): the save commits every block's bypass.
        state.perform.bypass.clear();
        let followed = match renamed {
            true => preset::follow_name(&mut library.presets, current, &slot, &name),
            false => Ok(None),
        };
        state.history.rename(&name);
        if let Some(before) = rewritten {
            let after = PresetFile::read(before.host.clone());
            state.history.refile(&before, after);
        }
        *state.clean.get_mut() = Some(clean);
        // Review r39 K6: the next Edit Details opens on the saved instrument (record `+0x5c`,
        // `0x7c4db8`); `NotSet` kept the stored one.
        if new_file || instrument != tetrad_ui::PreferredInstrument::NotSet {
            state.preset_instrument = instrument;
        }
        if let Some(slot) = &copy {
            state.select_slot(slot);
            self.schedule_settings_flush();
        }
        let stale = match followed {
            Ok(renamed) => saved.search_index.or(renamed),
            Err(error) => {
                let detail = format!("saved, but the file keeps its name: rename refused: {error}");
                self.notice(Subsystem::Library, detail);
                saved.search_index
            }
        };
        if let Some(stale) = stale {
            self.notice(Subsystem::Library, stale.to_string());
        }
        let pushes = self.bridges.control.preset_saved(&self.state);
        self.apply(Ok::<_, DeviceError>(pushes));
        // `0x5b7438`: a save into the loaded slot pushes with reason Save; a save into another
        // slot loads that slot (`0x5b2f40`), so its pushes have reason Other.
        let reason = match copy {
            Some(copy) if copy != slot => RecallPresetReason::Other,
            _ => RecallPresetReason::Save,
        };
        for push in control::preset_pushes(&self.state, reason) {
            self.execute(push);
        }
        self.present = true;
        Ok(())
    }

    /// Library slot → migrate → compile → deploy, then the last-preset setting, the tempo
    /// and the MIDI on-load messages. Once the DSP runs the new preset, `State` follows it: a
    /// later tempo failure is only a notice. A successful load closes the bank overlay
    /// (inferred) and presents a frame. [`FACTORY_SETLIST`] reads the factory slots. `undo`
    /// false pushes no load frame ([`Self::load_preset_path`]). Cortex Control then gets the
    /// load's pushes ([`control::preset_pushes`], reason Other): `0x5b2f40` → `0x5b2de4`
    /// sends RecallPreset, its forced `0x5899d8(scene, 1)` the Scene, and its tail the
    /// SetlistPosition.
    pub(super) fn load_preset(
        &mut self,
        target: &PresetRef,
        undo: bool,
    ) -> Result<(), DeviceError> {
        self.state
            .specs
            .as_ref()
            .ok_or(DeviceError::Unavailable("the model catalog"))?;
        let path = self.slot_path(target)?;
        let recents = self.load_path(path, Some(target), undo)?;
        let pushes = control::preset_pushes(&self.state, RecallPresetReason::Other);
        self.execute_load_pushes(pushes, recents);
        Ok(())
    }

    /// The file slot `target` names.
    fn slot_path(&self, target: &PresetRef) -> Result<Option<String>, DeviceError> {
        let library = self
            .state
            .library
            .as_ref()
            .ok_or(DeviceError::Unavailable("the preset library"))?;
        let index = usize::try_from(target.index).unwrap_or(usize::MAX);
        Ok(match target.setlist.as_deref() {
            Some(FACTORY_SETLIST) => library
                .presets
                .factory_slots()?
                .get(index)
                .cloned()
                .flatten(),
            key => preset::last_preset_path(&library.presets, key, index)?,
        })
    }

    /// The load frame of the loaded preset (type 5, `0x647e6c`): the slot that names its file,
    /// the file and its stamp, the working preset and its identity. The slot is `LastPreset`,
    /// else (review r39 K4: a swap reload, an audition) the slot of the `LastPreset` setlist,
    /// then of any setlist, that names the file; `None` when no setlist slot names it.
    fn load_state(&self) -> Option<preset::LoadState> {
        let current = self.state.current.as_ref()?;
        let last = &self.state.settings.settings().last_preset;
        let last = PresetRef {
            setlist: last.setlist.clone(),
            index: u32::try_from(last.preset_index).unwrap_or(u32::MAX),
        };
        let library = (self.state.library.as_ref()).map(|library| &library.presets);
        let file = current.path.as_deref().zip(library);
        let slot = if self.slot_path(&last).ok() == Some(current.path.clone()) {
            Some(last)
        } else {
            file.and_then(|(path, library)| {
                match preset::slot_of(library, last.setlist.as_deref(), path) {
                    Ok(Some(index)) => Some(PresetRef {
                        index: index as u32,
                        ..last
                    }),
                    _ => preset::locate(library, path).ok().flatten(),
                }
            })
        };
        // A path that is not a media file (a Downloads row) has no stamp.
        let file = file.and_then(|(path, library)| {
            let file = PresetFile::read(library.media().resolve(path).ok()?);
            file.unchanged().then_some(file)
        });
        Some(preset::LoadState {
            slot,
            path: current.path.clone(),
            file,
            working: preset::Snapshot::of(current),
            owner: self.state.history.loaded(),
        })
    }

    /// Undo (or redo) of a load frame (`0x649330` case 5 → `0x5b2f40`): the reverse frame is
    /// the selection now; the old selection loads, then the frame's working preset replaces
    /// it (`0x5b2de4` with the override), so its unsaved edits return (I: shown unsaved).
    /// Owned, user data first: the slot must still name the frame's file, and that file must
    /// be the same file (its stamp, review r39 K4; Tetrad's own rewrites of it for the same
    /// preset carry the stamp, `History::refile`, review r41 L1), else nothing loads, a notice says so and
    /// the frame drops. A failed load drops the frame too. Either way the old preset's frames
    /// on top drop with it (K2). A working preset that cannot be put back waits as the next
    /// step (K7). The load writes what any load writes (recents, `LastPreset`) and no preset
    /// file.
    pub(super) fn step_preset_load(&mut self, redo: bool) -> Result<(), DeviceError> {
        let Some(preset::UndoFrame::PresetLoad(frame)) = self.state.history.newest(redo).cloned()
        else {
            return Ok(());
        };
        let preset::LoadState {
            slot,
            path,
            file,
            working,
            ..
        } = *frame;
        let step = if redo { "redo" } else { "undo" };
        let changed = match &slot {
            Some(slot) if self.slot_path(slot).ok() != Some(path.clone()) => Some(format!(
                "slot {} of {}",
                slot.index,
                slot.setlist.as_deref().unwrap_or("the first setlist")
            )),
            _ if file.as_ref().is_some_and(|file| !file.unchanged()) => path.clone(),
            _ => None,
        };
        if let Some(changed) = changed {
            self.state.history.step_load(redo, None);
            self.notice(
                Subsystem::Library,
                format!("load {step} refused: {changed} changed since"),
            );
            return Ok(());
        }
        let depths = self.state.history.depths();
        let history = self.state.history.clone();
        let reverse = self.load_state();
        let loaded = self.load_path(path, slot.as_ref(), true);
        // The load's own frame goes: the reverse frame replaces it.
        self.state.history = history;
        let recents = match loaded {
            Ok(recents) => recents,
            Err(error) => {
                self.state.history.step_load(redo, None);
                return Err(error);
            }
        };
        self.state.history.step_load(redo, reverse);
        let restored = self.put_working(redo, working);
        let pushes = control::undo_pushes(&self.state, depths, redo);
        self.execute_load_pushes(pushes.into(), recents);
        // `0x649330`/`0x649b7c` case 5 push the Scene once more after the load (`0x27c3fc`).
        self.execute(control::scene_push(&self.state));
        self.present = true;
        restored
    }

    /// `working` replaces the loaded preset after the capture-name pass; a layout difference
    /// deploys it. One that cannot be put back waits as the next undo (or redo) step (review
    /// r39 K7, O).
    fn put_working(
        &mut self,
        redo: bool,
        mut working: preset::Snapshot,
    ) -> Result<(), DeviceError> {
        control::refresh_capture_names(&mut working.preset, &self.state.capture_names);
        let restore = match (&self.state.specs, &*self.state.current) {
            (Some(specs), Some(current)) => {
                preset::Restore::prepare(current, specs, working.clone()).map_err(Into::into)
            }
            _ => Err(DeviceError::Unavailable("the catalog and a loaded preset")),
        };
        let restored = match restore {
            Ok(preset::Restore::Preset(preset)) => {
                if let Some(current) = self.state.current.get_mut().as_mut() {
                    current.preset = *preset;
                }
                Ok(())
            }
            Ok(preset::Restore::Layout(change)) => self.deploy_layout(change),
            Err(error) => Err(error),
        };
        if restored.is_err() {
            self.state.history.keep_working(redo, working);
        }
        restored
    }

    /// `0x7c4c1c` → `0x5b2c54`, after a delete of the selected slot's item: the new-preset
    /// template (`+0x1700`) deploys and becomes the working and the clean preset, with no file
    /// and no load frame; the selection stays, so the next save asks for a name and stores
    /// into that slot. Cortex Control gets RecallPreset (reason Other, no item), Scene and
    /// SetlistPosition.
    pub(super) fn load_template(&mut self) -> Result<(), DeviceError> {
        self.load_preset_path(None, None, false)?;
        for push in control::preset_pushes(&self.state, RecallPresetReason::Other) {
            self.execute(push);
        }
        Ok(())
    }

    /// `0x5b2ce8`, after a slot frame step that changed the selected slot: the preset the slot
    /// names now (the template when it is empty) becomes the clean preset (`+0x28`; the slot
    /// stays selected); then the frame's `working` preset plays on it (`0x527d80`), so the
    /// edits it held return (shown unsaved). It is the preset `owner`
    /// again ([`preset::History::resume`]). No load frame. Cortex Control gets RecallPreset
    /// with reason Undo and the Scene (no SetlistPosition). I: a frame without a working
    /// preset (none was loaded) plays the slot's preset.
    pub(super) fn restore_selected(
        &mut self,
        redo: bool,
        working: Option<preset::Snapshot>,
        owner: preset::LoadId,
    ) -> Result<(), DeviceError> {
        let last = &self.state.settings.settings().last_preset;
        let slot = PresetRef {
            setlist: last.setlist.clone(),
            index: u32::try_from(last.preset_index).unwrap_or(u32::MAX),
        };
        let path = self.slot_path(&slot)?;
        let history = self.state.history.clone();
        let loaded = self.load_preset_path(path, None, false);
        // The load's new identity goes: the frame's preset plays.
        self.state.history = history;
        loaded?;
        self.state.history.resume(owner);
        let restored = working.map_or(Ok(()), |working| self.put_working(redo, working));
        for push in control::restore_pushes(&self.state) {
            self.execute(push);
        }
        self.present = true;
        restored
    }

    /// The forced reload `0x527d80` of `preset` in place ([`Self::recall_prepared`]), after the
    /// capture-name pass `0x7c2da4`.
    pub(super) fn recall_preset(&mut self, mut preset: Preset) -> Result<(), DeviceError> {
        control::refresh_capture_names(&mut preset, &self.state.capture_names);
        let specs = self
            .state
            .specs
            .as_ref()
            .ok_or(DeviceError::Unavailable("the model catalog"))?;
        let current = self
            .state
            .current
            .as_ref()
            .ok_or(DeviceError::Unavailable("a loaded preset"))?;
        let ddc = self
            .state
            .settings
            .settings()
            .general
            .dynamic_delay_compensation;
        let dormant = preset::dormant_models(specs, &self.state.licences);
        let prepared = preset::prepare(preset, specs, ddc, Some(&current.deployment), dormant)?;
        self.recall_prepared(prepared, None)
    }

    /// `0x527d80`: a prepared preset replaces the working preset in place, with the load's
    /// deploy rule, capture guard and follow-up (tempo, metronome, on-load MIDI). The file
    /// path, `LastPreset`, the undo history and the clean preset stay. A Cortex Control
    /// RecallPreset (`0x5b22a8`) keeps its `undo` frame on the commit.
    pub(super) fn recall_prepared(
        &mut self,
        prepared: preset::Prepared,
        undo: Option<preset::PendingFrame>,
    ) -> Result<(), DeviceError> {
        if self.options.is_bringup() && self.options.stage != bringup::Stage::Full {
            return Err(DeviceError::Unavailable(
                "a Control preset recall before stage full",
            ));
        }
        let (Some(_), Some(current)) = (&self.state.specs, &*self.state.current) else {
            return Err(DeviceError::Unavailable("the catalog and a loaded preset"));
        };
        let path = current.path.clone();
        let (expression, capturing) = self.push_prepared(&prepared)?;
        if let Some(undo) = undo {
            self.state.history.commit(undo);
        }
        *self.state.current.get_mut() = Some(Current {
            path,
            preset: prepared.preset,
            migration: prepared.migration,
            deployment: prepared.deployment,
        });
        self.state.perform.bypass.clear();
        self.state.perform.preset_loaded = true;
        self.finish_load(expression, capturing);
        Ok(())
    }

    /// [`Self::load_preset`] of a resolved `.pb` device path (`None`: the new-preset shape).
    /// `slot` `None` is a bring-up `--preset` file: `LastPreset` and the footswitch index stay.
    /// `undo` false (`0x5b3810` with `updateUndo` 0, a MIDI program change) pushes no load
    /// frame; the loaded preset still gets a new identity ([`preset::History::record_load`]).
    /// The recent's Cortex Control pushes go out at the end; [`Self::load_preset`] places
    /// them among the load's pushes instead ([`Self::load_path`]).
    pub(super) fn load_preset_path(
        &mut self,
        path: Option<String>,
        slot: Option<&PresetRef>,
        undo: bool,
    ) -> Result<(), DeviceError> {
        let recents = self.load_path(path, slot, undo)?;
        recents.into_iter().for_each(|push| self.execute(push));
        Ok(())
    }

    /// [`Self::load_preset_path`] that returns the Cortex Control pushes of the recent it
    /// records ([`Self::record_recent`]) for the caller to place.
    fn load_path(
        &mut self,
        path: Option<String>,
        slot: Option<&PresetRef>,
        undo: bool,
    ) -> Result<Vec<Effect>, DeviceError> {
        let specs = self
            .state
            .specs
            .as_ref()
            .ok_or(DeviceError::Unavailable("the model catalog"))?;
        let library = self
            .state
            .library
            .as_ref()
            .ok_or(DeviceError::Unavailable("the preset library"))?;
        // r29-media: a stored DOWNLOADS preset (`cloud-0-1/<id>`) reads from its table.
        // `0x58ed3c`: the sidecar instrument starts the Save As picker (I: a stored download
        // and a new preset start on none).
        let (mut read, instrument) = match path
            .as_deref()
            .and_then(|path| media::read_download(&self.state, path))
        {
            Some(read) => (read?, tetrad_ui::PreferredInstrument::NotSet),
            None => {
                let sidecar =
                    (path.as_deref()).and_then(|path| library.presets.sidecar(path).ok().flatten());
                let instrument = sidecar.as_ref().and_then(|s| s.text("instrument_type"));
                let mut read = preset::read(&library.presets, path.as_deref(), specs)?;
                // Factory presets store no name in the file; stock shows the sidecar name
                // ("Brit 2203", seen on the QC 2026-10-15), else the file name (I).
                if read.name.as_deref().is_none_or(str::is_empty)
                    && let Some(path) = path.as_deref()
                {
                    let stored = sidecar.as_ref().and_then(|s| s.text("name"));
                    read.name = Some(match stored.filter(|name| !name.is_empty()) {
                        Some(name) => name.to_owned(),
                        None => super::ui::project::preset_title(path),
                    });
                }
                (
                    read,
                    preset::preferred_instrument(Instrument::from_text(
                        instrument.unwrap_or_default(),
                    )),
                )
            }
        };
        control::refresh_capture_names(&mut read, &self.state.capture_names);
        let recents = self.record_recent(path.as_deref());
        let specs = self
            .state
            .specs
            .as_ref()
            .ok_or(DeviceError::Unavailable("the model catalog"))?;
        let previous = self
            .state
            .current
            .as_ref()
            .map(|current| &current.deployment);
        let ddc = self
            .state
            .settings
            .settings()
            .general
            .dynamic_delay_compensation;
        let dormant = preset::dormant_models(specs, &self.state.licences);
        let pushed = preset::prepare(read, specs, ddc, previous, dormant)
            .map_err(DeviceError::from)
            .and_then(|prepared| Ok((self.push_prepared(&prepared)?, prepared)));
        let ((expression, capturing), prepared) = match pushed {
            Ok(pushed) => pushed,
            Err(error) => {
                // No load pushes follow: the recent's go out alone.
                recents.into_iter().for_each(|push| self.execute(push));
                return Err(error);
            }
        };
        let before = self.load_state().filter(|_| undo);
        if let Some(target) = slot {
            self.state.select_slot(target);
            self.schedule_settings_flush();
        }
        self.state.performance.footswitch.overlay = None;
        self.state.preset_instrument = instrument;
        *self.state.current.get_mut() = Some(Current {
            path,
            preset: prepared.preset,
            migration: prepared.migration,
            deployment: prepared.deployment,
        });
        self.state.perform.bypass.clear();
        self.state.perform.preset_loaded = true;
        // `0x5b2f40` → `0x5898f4(0)` (`0x5b31e8`): a load ends Copy/Swap Scene mode.
        self.state.perform.scene_clipboard = None;
        self.state.performance.footswitch.scene_copy = false;
        // Stock keeps the history and records the load as an undoable frame (type 5,
        // `0x648f1c`, `0x00647e6c`); a boot load has nothing before it.
        self.state.history.record_load(before);
        *self.state.clean.get_mut() = self.state.current.as_ref().map(preset::Snapshot::of);
        self.finish_load(expression, capturing);
        Ok(recents)
    }

    /// `0x5b2f40` → `0x7c3820`: a setlist preset (not an audition) goes to the front of the
    /// preset Recents. Returns the list pushes (`0x1abd00` removed, `0x1aabe8` added). Stock
    /// runs `0x7c3820` after the push `0x5b2de4`, which has no failure return, so a preset
    /// that fails to deploy is still recorded; Tetrad writes the entry before its deploy for
    /// that reason. The pushes go after the load's Scene and before its SetlistPosition (S7
    /// `[1756]`, `[1757]`; [`Self::execute_load_pushes`]).
    fn record_recent(&mut self, path: Option<&str>) -> Vec<Effect> {
        let path = path.filter(|path| self.options.persist && !media::auditioned_path(path));
        let (Some(path), Some(library)) = (path, self.state.library.as_mut()) else {
            return Vec::new();
        };
        let entry = tetrad_library::stock::ItemRef {
            folder: path
                .rsplit_once('/')
                .map_or("", |(folder, _)| folder)
                .to_owned(),
            item: path.to_owned(),
        };
        let list =
            tetrad_library::stock::ItemList::Recents(tetrad_storage::media::Collection::Presets);
        let recorded = library.presets.items(list).and_then(|before| {
            library.presets.push_item(list, &entry)?;
            Ok((before, library.presets.items(list)?))
        });
        self.report(recorded.map_err(DeviceError::from))
            .map_or_else(Vec::new, |(before, after)| {
                control::list_pushes(&self.state, list, &before, &after, Some(&entry))
            })
    }

    /// Runs the load's Cortex Control `pushes` with the `recents` pushes before its
    /// SetlistPosition (`0x5b2f40`: `0x7c3820` after `0x5b2de4`, then the position), or last
    /// when `pushes` has none.
    fn execute_load_pushes(&mut self, pushes: Vec<Effect>, recents: Vec<Effect>) {
        let position = pushes.iter().position(|push| {
            matches!(push, Effect::ControlNotify(message)
                if matches!(**message, tetrad_control::codecs::ControlMessage::SetlistPosition(_)))
        });
        let mut pushes = pushes;
        let at = position.unwrap_or(pushes.len());
        pushes.splice(at..at, recents);
        pushes.into_iter().for_each(|push| self.execute(push));
    }

    /// The push head of a load and a recall (`0x527d80`): the on-load MIDI Out, then the
    /// deploy of `prepared` with its media, expression and `nopoll` marker. Returns the
    /// deployed expression and whether a capture preset runs (the push was skipped). Failures
    /// of media and links are notices; a refused deploy is the error and changes no `State`.
    fn push_prepared(
        &mut self,
        prepared: &preset::Prepared,
    ) -> Result<(features::Expression, bool), DeviceError> {
        // `0x527d80` → `0x331c28`: the on-load MIDI Out goes before the compare and the
        // deploy (review r11 N7).
        let midi = self
            .bridges
            .midi
            .preset_loading(&prepared.preset, &self.state);
        self.apply(midi);
        let specs = self
            .state
            .specs
            .as_ref()
            .ok_or(DeviceError::Unavailable("the model catalog"))?;
        let dsp = self
            .dsp
            .as_mut()
            .ok_or(DeviceError::Unavailable("the DSP"))?;
        let (media, failures) = resolve_media(
            &mut self.media,
            &mut self.platform,
            prepared.deployment.plan(),
            &self.state,
        );
        let (expression, links) = features::expression_for(
            &prepared.preset,
            &self.state.pedals,
            &features::Expression::default(),
        );
        let mut context = context(&self.state, specs, &prepared.deployment, media);
        context.expression = Some(&expression);
        context.nopoll = self
            .platform
            .marker(Marker::NoPoll)
            .map_err(DeviceError::platform)?;
        // `0x5122c0`: while a capture preset runs (`+0x88d0`) stock redeploys it unchanged;
        // here the push is skipped. The new preset is the user preset the revert plays
        // (`reload_user`, review w5 R10).
        let capturing = self.bridges.capture.capture_running();
        if !capturing {
            let pending = dsp.apply(&mut self.platform, &prepared.update, &context)?;
            self.note_pending(Subsystem::Dsp, pending);
        }
        self.note_failures(failures);
        for detail in links {
            self.notice(Subsystem::Preset, detail);
        }
        Ok((expression, capturing))
    }

    /// The tail of a load and a recall, after the caller's `State` writes: the deployed
    /// expression, a frame, then the tempo and the metronome resend unless a capture preset
    /// runs, and last the preset-loaded MIDI.
    fn finish_load(&mut self, expression: features::Expression, capturing: bool) {
        // First: no load-triggered action (a mapping, an extension) may release a held Custom
        // momentary bypass into the new preset's blocks.
        super::custom::preset_loaded(&mut self.state);
        let refresh = actions::reduce(
            Action::ExpressionDeployed(Box::new(expression)),
            &mut self.state,
        );
        self.apply(refresh);
        self.present = true;
        // `0x527d80`: the push ends with the tempo (`0x5122c0` → `0x875a48`), then the
        // metronome resend (`0x512c74`). During capture stock sends no tempo and resends the
        // capture preset's metronome; the revert sends both for this preset.
        if !capturing {
            let bpm = actions::tempo_bpm(&self.state);
            let tempo = self.dsp_command(DspCommand::Tempo(bpm));
            self.report(tempo);
            let metronome = actions::reduce(Action::MetronomeResend, &mut self.state);
            self.apply(metronome);
        }
        let midi = self.bridges.midi.preset_loaded(&self.state);
        self.apply(midi);
        // The first loaded preset is also the point to load the extension store: start-up
        // never waits for extension work.
        let extensions = self.extensions(|extensions, backend, _| {
            let mut outcome = extensions.boot(backend);
            let loaded = extensions.preset_loaded(backend);
            outcome.effects.extend(loaded.effects);
            outcome
        });
        self.apply(Ok::<_, super::DeviceError>(extensions));
        if !self.bridges.mappings.loaded {
            use super::platform::ServiceBackends;
            use tetrad_services::exec::Files;
            let path = std::path::Path::new(super::mappings::GLOBAL_FILE);
            let stored = self
                .platform
                .services()
                .files()
                .read_bounded(path, 256 * 1024);
            let problem = match stored {
                Ok(bytes) => self.bridges.mappings.load_global(bytes.as_deref()),
                Err(error) => Some(error.to_string()),
            };
            if let Some(problem) = problem {
                self.notice(Subsystem::Extensions, format!("Global mappings: {problem}"));
            }
        }
        self.load_remote();
        self.load_boot_selector();
        let now = tetrad_dsp_host::session::Clock::now(&self.clock);
        let mappings = (self.bridges.mappings).on_preset_loaded(&self.state, now);
        self.apply(Ok::<_, super::DeviceError>(mappings));
    }
}

/// The push context of `deployment` with the global rows and the resolved media. The tuner
/// screen does not gate deploys or the lane I/O push: `+0x88d8` is the tuner engine flag
/// (on from boot, off only on the Neural Capture screen; w3-research-dsp-features §2 T1-T3).
pub(super) fn context<'a>(
    state: &'a State,
    specs: &'a Specs,
    deployment: &'a Deployment,
    media: Option<&'a dsp::ResolvedMedia>,
) -> dsp::Context<'a> {
    dsp::Context {
        media,
        tuner_active: state.tuner_engine,
        tuner_muted: state.tuner_muted,
        expression: Some(&state.expression),
        looper_context: features::looper_context(state),
        tempo_bpm: Some(actions::tempo_bpm(state)),
        preset: state.current.as_ref().map(|current| &current.preset),
        ..dsp::Context::new(specs, deployment, state.settings.settings())
    }
}

/// The IRs `plan` names (every block string, all scenes) through the media library, with
/// the load failures to report; nothing without the library. The media is borrowed from the
/// library's process cache: a push copies no IR.
pub(super) fn resolve_media<'m>(
    library: &'m mut Option<media::MediaLibrary>,
    platform: &mut impl Platform,
    plan: &tetrad_engine::DeploymentPlan,
    state: &State,
) -> (Option<&'m dsp::ResolvedMedia>, Vec<String>) {
    let Some(library) = library else {
        return (None, Vec::new());
    };
    let database = state
        .library
        .as_ref()
        .and_then(|library| library.database.as_ref());
    let values = plan
        .deployed()
        .flat_map(|deployed| &deployed.texts)
        .flat_map(|text| text.scenes.iter().flatten())
        .map(String::as_str);
    let read = |file: DspFile<'_>| platform.dsp_file(file).map_err(|error| error.to_string());
    let (media, failures) = library.resolve(values, read, database);
    (Some(media), failures)
}
