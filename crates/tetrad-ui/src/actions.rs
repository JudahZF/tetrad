//! What each control does once it is known to be visible and enabled.

use crate::layout::ListId;
use crate::settings::{DeviceAction, SettingKind, SettingsPage};
use crate::state::{
    ChoicePurpose, Confirm, DirectoryNameEdit, InhibitStatus, LooperSwap, MappingsPage, MediaPick,
    MenuKind, Overlay, Shift, SupportView, TextTarget, simplify_text, stomp_name,
};
use crate::*;

impl UiState {
    pub(crate) fn apply(&mut self, control: Control) -> Vec<UiCommand> {
        // Every control closes the scene dropdown, a pick too (stock, seen on the
        // QC 2026-10-16); its badge toggles it.
        let scene_menu = std::mem::take(&mut self.scene_menu);
        let snapshot = &self.snapshot;
        match control {
            Control::SceneMenu => self.scene_menu = !scene_menu,
            // `globalEqButtonReleased` (`0x7e8494`): the flash stops and the popup opens.
            Control::GlobalEqStatus => {
                if let Some(status) = &mut self.inhibit_status {
                    status.steady = true;
                    self.overlay = Some(Overlay::GlobalEqInhibited {
                        global_eq: status.global_eq,
                        input_gate: status.input_gate,
                    });
                }
            }
            Control::Navigate(screen) => return self.navigate(screen),
            Control::SelectScene(scene) => match &mut self.draft {
                Some(draft) => draft.selected = scene,
                None => return vec![UiCommand::SelectScene { scene }],
            },
            Control::SelectCell(position) => {
                if !snapshot.occupied(position) {
                    // Stock opens the device list only from a slot in `stateClickable`
                    // (`slotReleasedSlot` `0x59a9c0`); a slot of a row without input stays
                    // in its first state, `stateInactive` (table `0x5bece4`). Seen on the
                    // QC (shot sn-1): an empty row-2 slot does nothing.
                    if crate::screens::grid::row_has_input(snapshot, position.row()) {
                        self.open_picker(position, false);
                    }
                } else if snapshot.selected_block == Some(position) {
                    self.open_picker(position, true);
                } else if snapshot.blocks[position.index()].category == Some(ModelCategory::Looper)
                {
                    // Manual §4: a tap on the Looper X block opens its full-screen view.
                    let mut commands = vec![UiCommand::InspectCell { position }];
                    commands.extend(self.navigate(Screen::Looper));
                    return commands;
                } else {
                    return vec![UiCommand::InspectCell { position }];
                }
            }
            Control::SelectPreset(id) => {
                return vec![if snapshot.bank_preview_active {
                    UiCommand::PreviewPreset { id }
                } else {
                    UiCommand::RequestPreset { id }
                }];
            }
            Control::EditScenes => self.open_draft(),
            Control::CloseDialog => self.draft = None,
            Control::ConfirmScenes => return self.confirm_scenes(),
            Control::CycleMode => return vec![UiCommand::CycleMode],
            Control::BeginBankPreview => return vec![UiCommand::BeginBankPreview],
            Control::EndBankPreview => return vec![UiCommand::EndBankPreview],
            Control::CancelBankPreview => return vec![UiCommand::CancelBankPreview],
            Control::CancelUpload => return vec![UiCommand::CancelUpload],
            Control::PreviousPresetPage => self.page = self.page.saturating_sub(1),
            Control::NextPresetPage => self.page = (self.page + 1).min(self.last_page()),
            Control::Back => return self.back(),
            Control::OpenMainMenu => {
                self.reset_scroll(ListId::Menu);
                self.overlay = Some(Overlay::Menu(MenuKind::Main));
            }
            Control::Menu(action) => return self.menu_action(action),
            // A preset without a file opens the name keyboard (`0x5ac544` → `0x5ac028`).
            Control::SavePreset if !snapshot.preset_file => {
                self.open_keyboard(TextTarget::PresetSaveAs, String::new());
            }
            Control::SavePreset => return vec![UiCommand::SavePreset],
            Control::Undo => return vec![UiCommand::Undo],
            Control::Redo => return vec![UiCommand::Redo],
            Control::Port { row, output } => {
                self.overlay = Some(Overlay::Menu(MenuKind::Port { row, output }));
            }
            Control::Site(site) => return vec![UiCommand::InspectSite { site }],
            Control::Param(param) => return self.param(param),
            Control::Tile(switch) => return vec![UiCommand::ToggleStomp { switch }],
            Control::StepBank { up } => return vec![UiCommand::StepBank { up }],
            Control::Picker(picker) => return self.picker_control(picker),
            Control::Media(media) => return self.media_control(media),
            Control::Expression(control) => return self.expression_control(control),
            Control::MidiOut(control) => return self.midi_out(control),
            Control::Library(library) => return self.library(library),
            Control::Setting(setting) => return self.setting(setting),
            // Directory drop-downs (category, sort, copy target) are screen pop-ups.
            Control::Choose(index) if self.directory.popup.is_some() => {
                return self.directory_choose(index);
            }
            Control::Choose(index) => return self.choose(index),
            Control::Dialog(button) => return self.dialog(button),
            Control::DspError { reboot } => return vec![UiCommand::DspErrorDialog { reboot }],
            Control::Key(key) => return self.key(key),
            Control::Numeric(key) => {
                let Some(entry) = &mut self.numeric else {
                    return Vec::new();
                };
                let command = entry.press(key);
                if key == NumericKey::Cancel || command.is_some() {
                    self.numeric = None;
                }
                return command.into_iter().collect();
            }
            Control::Looper(action) => {
                if self.looper_swap.is_none() {
                    return vec![UiCommand::Looper { action }];
                }
                let tiles = self.snapshot.looper.tiles.0;
                let tile = tiles.iter().position(|tile| *tile == action);
                let tile = tile.map(|tile| tile as u8);
                self.looper_swap = tile.map(LooperSwap::Assign).or(self.looper_swap);
                return vec![UiCommand::LooperAssign { tile }];
            }
            Control::LooperMenu => self.overlay = Some(Overlay::Menu(MenuKind::Looper)),
            Control::LooperSwapDone => self.looper_swap = None,
            Control::LooperAssignCancel => {
                self.looper_swap = Some(LooperSwap::Choose);
                return vec![UiCommand::LooperAssign { tile: None }];
            }
            Control::LooperLock => {
                return vec![UiCommand::LooperView {
                    shown: !self.snapshot.looper.editor_view,
                }];
            }
            Control::Tempo(tempo) => {
                return match tempo {
                    TempoControl::Slower => vec![UiCommand::NudgeTempo { steps: -1 }],
                    TempoControl::Faster => vec![UiCommand::NudgeTempo { steps: 1 }],
                    TempoControl::Tap => vec![UiCommand::TapTempo],
                    // A drag moves the knob (`Gesture::DragTempo`); a tap does nothing.
                    TempoControl::Knob => Vec::new(),
                };
            }
            Control::Capture(CaptureRequest::Abort) => {
                self.overlay = Some(Overlay::Confirm(Confirm::AbortCapture));
            }
            Control::Capture(request) => {
                let mut commands = vec![UiCommand::Capture { request }];
                if request == CaptureRequest::Exit {
                    commands.extend(self.pop());
                }
                return commands;
            }
            Control::Nc2(request) => return vec![UiCommand::Nc2 { request }],
            Control::SaveCapture => return self.capture_control(CaptureControl::Name),
            Control::CaptureUi(control) => return self.capture_control(control),
            Control::PresetMetadata(control) => return self.preset_metadata(control),
            Control::Wifi(WifiControl::Scan) => return vec![UiCommand::WifiScan],
            Control::Extension(control) => return self.extension_control(control),
            Control::Mapping(control) => return self.mapping_control(control),
            Control::Custom(control) => return self.custom_control(control),
            Control::Theme(index) => {
                let themes = &self.snapshot.appearance.themes;
                return (themes.get(usize::from(index)))
                    .filter(|theme| theme.palette.is_ok() && !theme.selected)
                    .map(|theme| UiCommand::SetTheme(theme.id.clone()))
                    .into_iter()
                    .collect();
            }
            Control::Wifi(WifiControl::DomainSettings) => {
                self.wifi_domains = true;
                return Vec::new();
            }
            Control::Wifi(WifiControl::InternetCheck) => {
                self.overlay = Some(Overlay::Confirm(Confirm::InternetCheck));
                return Vec::new();
            }
            Control::Wifi(WifiControl::Network(index)) => {
                let Some(network) = snapshot.wifi.networks.get(usize::from(index)) else {
                    return Vec::new();
                };
                // Stock cell tap (`0x5d4834`): the joined network disconnects, any other
                // connects (an unsaved secured one asks for its password first).
                if network.connected {
                    return vec![UiCommand::WifiDisconnect];
                }
                let ssid = network.ssid.clone();
                return self.connect(&ssid);
            }
            // Stock forget icon (`0x5d5758`): no confirm.
            Control::Wifi(WifiControl::Forget(index)) => {
                let Some(network) = snapshot.wifi.networks.get(usize::from(index)) else {
                    return Vec::new();
                };
                return vec![UiCommand::WifiForget {
                    ssid: network.ssid.clone(),
                }];
            }
            Control::Updater(UpdaterCommand::CheckAndDownload)
                if snapshot.updater.channel != UpdateChannel::Official =>
            {
                self.overlay = Some(Overlay::Confirm(Confirm::DownloadUpdate));
            }
            Control::Updater(UpdaterCommand::Install) if snapshot.updater.stock_after_install => {
                self.overlay = Some(Overlay::Confirm(Confirm::InstallUpdate));
            }
            Control::Updater(command) => return vec![UiCommand::Updater { command }],
            Control::TetradUpdate(command) => return vec![UiCommand::TetradUpdate { command }],
            Control::RemoteToggle => {
                return vec![UiCommand::SetRemote {
                    enabled: !snapshot.remote.enabled,
                    pin_in_menu: snapshot.remote.pin_in_menu,
                }];
            }
            Control::RemotePinInMenuToggle => {
                return vec![UiCommand::SetRemote {
                    enabled: snapshot.remote.enabled,
                    pin_in_menu: !snapshot.remote.pin_in_menu,
                }];
            }
            Control::ShowChangelog => {
                let body = snapshot.updater.changelog.clone();
                self.reset_scroll(ListId::Changelog);
                self.overlay = Some(Overlay::Info {
                    title: "SEE WHAT'S NEW".into(),
                    body,
                });
            }
            Control::Backup(BackupControl::Create) => {
                self.open_keyboard(TextTarget::BackupName(None), String::new());
            }
            Control::Backup(BackupControl::Item(id)) => {
                self.overlay = Some(Overlay::Menu(MenuKind::Backup(id)));
            }
            Control::Login => self.open_keyboard(TextTarget::LoginEmail, String::new()),
            Control::Action(action) => return self.device_action(action),
            Control::SceneTarget(scene) => {
                if let Some(Overlay::SceneTarget { swap }) = self.overlay {
                    return self.scene_target(swap, scene);
                }
            }
            Control::Mode(mode) => return self.mode(mode),
            Control::Power(control) => return self.power_action(control),
            Control::Routing(routing) => return self.routing(routing),
            Control::SettingsPreset(control) => return self.settings_preset_control(control),
            Control::Editor(control) => return self.editor_control(control),
            Control::RestoreCapture(row) => {
                if let Some(row) = snapshot.quarantine.get(usize::from(row)) {
                    return vec![UiCommand::RestoreQuarantinedCapture {
                        id: row.id.clone(),
                        downloads: row.downloads,
                    }];
                }
            }
            // `0x596c50` clears the mode after any cell tap; `0x59470c` after a tap outside.
            Control::AddToGrid(cell) => {
                let Some(media) = self.add_to_grid.take() else {
                    return Vec::new();
                };
                return vec![match cell {
                    Some(position) => UiCommand::AddToGrid { position, media },
                    None => UiCommand::CancelAddToGrid,
                }];
            }
            Control::QuickSplit(quick) => {
                let pair = match self.editing_site() {
                    Some(EditorSite::Splitter(pair) | EditorSite::Mixer(pair)) => pair,
                    _ => return Vec::new(),
                };
                let mix = (quick == QuickSplit::SplitAndMix).then_some(7);
                return vec![UiCommand::QuickSplit {
                    pair,
                    split: Some(0),
                    mix,
                }];
            }
            Control::PresetMenu(id) => self.preset_menu(id),
            Control::SceneTool { scene, tool } => match tool {
                SceneTool::Edit => {
                    self.open_draft();
                    if let Some(draft) = &mut self.draft {
                        draft.selected = scene;
                    }
                }
                SceneTool::Swap | SceneTool::Copy => {
                    return self.begin_scene_copy(scene, tool == SceneTool::Swap);
                }
            },
            Control::Stomp(edit) => match edit {
                StompEdit::Open(tile) => self.overlay = Some(Overlay::EditStomp(tile)),
                StompEdit::Rename(tile) => {
                    let label = snapshot.stomps[usize::from(tile)].label.clone();
                    let switch = Footswitch::TILES[usize::from(tile)];
                    self.open_keyboard(TextTarget::StompLabel(switch), label);
                }
                StompEdit::Momentary { tile, momentary } => {
                    return vec![UiCommand::SetMomentary { tile, momentary }];
                }
                StompEdit::Block { tile, position } => {
                    let assigned = snapshot.stomps[usize::from(tile)]
                        .blocks
                        .contains(&position);
                    return vec![UiCommand::AssignStomp {
                        position,
                        tile: (!assigned).then_some(tile),
                    }];
                }
            },
            Control::EditBlock(position) => {
                if snapshot.occupied(position) {
                    let mut commands = self.navigate(Screen::Grid);
                    commands.push(UiCommand::InspectCell { position });
                    return commands;
                }
            }
            Control::LooperParams => {
                let looper = GridPosition::all().find(|position| {
                    snapshot.blocks[position.index()].category == Some(ModelCategory::Looper)
                });
                if let Some(position) = looper {
                    let mut commands = self.navigate(Screen::Grid);
                    commands.push(UiCommand::InspectCell { position });
                    return commands;
                }
            }
            Control::EditText => {
                if let Some(draft) = &self.draft {
                    let label = draft.entries[usize::from(draft.selected.0)].label.clone();
                    self.open_keyboard(TextTarget::SceneLabel, label);
                }
            }
        }
        Vec::new()
    }

    fn confirm_scenes(&mut self) -> Vec<UiCommand> {
        let Some(draft) = self.draft.take() else {
            return Vec::new();
        };
        if draft.original != self.snapshot.scenes || draft.preset_id != self.snapshot.preset_id {
            return vec![UiCommand::Rejected(UiError::StaleSceneDraft)];
        }
        let mut commands = Vec::with_capacity(SCENE_COUNT * 2);
        // Confirmed emitter order: VA 0x679664 / 0x6795c0.
        for (index, entry) in draft.entries.into_iter().enumerate() {
            let scene = SceneIndex(index as u8);
            commands.push(UiCommand::SetSceneLabel {
                scene,
                label: entry.label,
            });
            if entry.raw_color != 0 {
                commands.push(UiCommand::SetSceneColor {
                    scene,
                    raw_color: entry.raw_color,
                });
            }
        }
        commands
    }

    fn open_picker(&mut self, position: GridPosition, replace: bool) {
        let category = replace
            .then(|| self.snapshot.blocks[position.index()].category)
            .flatten();
        let category = category.or(self.picker.category);
        let (group, instrument) = category
            .and_then(DeviceGroup::of)
            .map_or((None, Instrument::Guitar), |(group, instrument)| {
                (Some(group), instrument)
            });
        self.picker = crate::state::Picker {
            target: Some(position),
            replace,
            category,
            query: String::new(),
            // Adding starts on the category list (shot 076); replacing opens
            // the current device's group (I).
            group: group.filter(|_| replace),
            instrument,
            presets: None,
            captures: Default::default(),
        };
        self.reset_scroll(ListId::PickerCategories);
        self.reset_scroll(ListId::PickerModels);
        self.navigate(Screen::ModelPicker);
        if self.capture_browser() {
            self.open_capture_browser();
        }
    }

    /// Opens on the folder that holds the current item (matched by display name). A capture
    /// opens the capture browser of the device list: stock changes a capture by a block
    /// replace (`0x386024` → `0x704530`), never by a text edit.
    pub(crate) fn open_media(&mut self, mut pick: MediaPick, current: &str) {
        if pick.kind == MediaKind::Capture {
            return self.open_picker(pick.position, true);
        }
        pick.folder = self
            .snapshot
            .media
            .iter()
            .find(|entry| entry.folder.kind() == pick.kind && entry.name == current)
            .map(|entry| entry.folder);
        self.directory.open_picker(pick.kind);
        self.media = Some(pick);
        self.reset_scroll(ListId::MediaFolders);
        self.reset_scroll(ListId::MediaItems);
        self.navigate(Screen::MediaPicker);
    }

    fn media_control(&mut self, control: MediaControl) -> Vec<UiCommand> {
        let Some(pick) = self.media.as_mut() else {
            return Vec::new();
        };
        match control {
            MediaControl::Folder(folder) => {
                pick.folder = Some(folder);
                self.reset_scroll(ListId::MediaItems);
                Vec::new()
            }
            MediaControl::Library => {
                pick.folder = None;
                self.reset_scroll(ListId::MediaItems);
                Vec::new()
            }
            MediaControl::Item(index) => {
                let pick = *pick;
                let mut commands: Vec<UiCommand> = self
                    .snapshot
                    .media
                    .get(usize::from(index))
                    // A selection change while the picker is open makes it stale.
                    .filter(|entry| {
                        entry.folder.kind() == pick.kind
                            && self.snapshot.selected_block == Some(pick.position)
                    })
                    .map(|entry| UiCommand::SelectText {
                        position: pick.position,
                        parameter: pick.parameter,
                        text: entry.value.clone(),
                    })
                    .into_iter()
                    .collect();
                commands.extend(self.pop());
                commands
            }
        }
    }

    fn picker_control(&mut self, control: PickerControl) -> Vec<UiCommand> {
        match control {
            PickerControl::Category(category) => {
                self.picker.category = Some(category);
                self.picker.query.clear();
                if let Some((group, instrument)) = DeviceGroup::of(category) {
                    self.picker.group = Some(group);
                    self.picker.instrument = instrument;
                }
                self.reset_scroll(ListId::PickerModels);
            }
            PickerControl::Model(model_id) => {
                let Some(position) = self.picker.target else {
                    return Vec::new();
                };
                let command = if self.picker.replace {
                    UiCommand::ReplaceBlock { position, model_id }
                } else {
                    UiCommand::AddBlock { position, model_id }
                };
                let mut commands = vec![command];
                commands.extend(self.pop());
                return commands;
            }
            PickerControl::Search => {
                let query = self.picker.query.clone();
                self.open_keyboard(TextTarget::ModelSearch, query);
            }
            PickerControl::ClearSearch => self.picker.query.clear(),
            PickerControl::Paste => {
                let Some(position) = self.picker.target else {
                    return Vec::new();
                };
                let mut commands = vec![UiCommand::PasteBlock { position }];
                commands.extend(self.pop());
                return commands;
            }
            control => return self.device_list(control),
        }
        Vec::new()
    }

    fn param(&mut self, control: ParamControl) -> Vec<UiCommand> {
        if let Some(site) = self.editing_site() {
            return self.site_param(site, control);
        }
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        match control {
            ParamControl::Knob(knob) => {
                self.focus = Some(knob);
                let index = self.param_page * KNOB_COUNT + usize::from(knob.get());
                // A tap flips a toggle, steps a choice to its next option and
                // opens the media picker (stock IR/capture browser flow not
                // recovered; I).
                if let Some(parameter) = self.snapshot.parameters.get(index)
                    && let ParameterKind::Media(kind) = parameter.kind
                {
                    let current = parameter.value.clone();
                    self.open_media(
                        MediaPick {
                            position,
                            parameter: parameter.index,
                            kind,
                            folder: None,
                        },
                        &current,
                    );
                } else if let Some(parameter) = self.snapshot.parameters.get(index)
                    && matches!(
                        parameter.kind,
                        ParameterKind::Toggle { .. }
                            | ParameterKind::Choice { .. }
                            | ParameterKind::ComboBox { .. }
                    )
                {
                    return vec![UiCommand::NudgeParameter {
                        position,
                        parameter: parameter.index,
                        steps: 1,
                    }];
                } else if let Some(parameter) = self.snapshot.parameters.get(index) {
                    let double = self.knob_tap.is_some_and(|(last, at)| {
                        last == knob && self.now.saturating_sub(at) <= crate::state::DOUBLE_TAP_MS
                    });
                    if double {
                        self.knob_tap = None;
                        return vec![UiCommand::DevicePreset {
                            position,
                            request: crate::DevicePresetRequest::RestoreDefault {
                                parameter: parameter.index,
                            },
                        }];
                    }
                    self.knob_tap = Some((knob, self.now));
                }
            }
            ParamControl::Value(knob) => {
                self.open_numeric(ParameterTarget::Block(position), knob);
            }
            ParamControl::Page(page) => self.show_param_page(page),
            ParamControl::Bypass => {
                return vec![UiCommand::SetBlockBypass {
                    position,
                    bypassed: !self.snapshot.cell(position).bypassed,
                }];
            }
            ParamControl::ChangeModel => self.open_picker(position, true),
            ParamControl::Close => return vec![UiCommand::ClearBlockSelection],
            ParamControl::Menu => self.overlay = Some(Overlay::Menu(MenuKind::Block(position))),
            ParamControl::Stomp => {
                self.overlay = Some(Overlay::Confirm(Confirm::AssignStomp(position)));
            }
        }
        Vec::new()
    }

    fn setting(&mut self, control: SettingControl) -> Vec<UiCommand> {
        match control {
            // The Power Functions lock row locks at once, as POWER → LOCK does (manual
            // "Power & Locking"); the row is not on screen while locked.
            SettingControl::Open(SettingId::LockScreen) => {
                return self.power_action(crate::PowerControl::Lock);
            }
            SettingControl::Open(id) => match id.kind() {
                SettingKind::Toggle => return self.step_setting(id, 1),
                SettingKind::Choice(_) => {
                    let current = match self.snapshot.setting(id) {
                        Some(SettingValue::Choice(index)) => usize::from(*index),
                        _ => 0,
                    };
                    self.open_choice(ChoicePurpose::Setting(id), current);
                }
                SettingKind::Number { .. } if let Screen::GlobalEq(tab) = self.screen() => {
                    return self.eq_tap(tab, id);
                }
                SettingKind::Number { .. } => return self.brightness_tap(id),
            },
            SettingControl::Step(id, steps) => return self.step_setting(id, i32::from(steps)),
            SettingControl::Page(page) => {
                self.reset_scroll(ListId::SettingsPage);
                self.support = SupportView::List;
                self.wifi_domains = false;
                self.extensions = crate::state::ExtensionsPage::List;
                return self.navigate(Screen::Settings(page));
            }
            SettingControl::ExtensionSettings(index) => {
                let installed = &self.snapshot.extensions.installed;
                let Some(key) = installed.get(usize::from(index)).map(|e| e.key.clone()) else {
                    return Vec::new();
                };
                self.reset_scroll(ListId::SettingsPage);
                self.extensions = crate::state::ExtensionsPage::List;
                self.extension_section = Some(key);
                return self.navigate(Screen::Settings(SettingsPage::ExtensionSettings));
            }
            SettingControl::IoTab(tab) => {
                self.reset_scroll(ListId::IoPage);
                return self.navigate(Screen::Io(tab));
            }
            SettingControl::EditDeviceName => {
                let name = self.snapshot.about.device_name.clone();
                let name = name.unwrap_or_else(|| self.default_device_name());
                self.open_keyboard(TextTarget::DeviceName, name);
            }
            SettingControl::Licence(package) => {
                self.support = SupportView::Licence(package);
                self.reset_scroll(ListId::SettingsPage);
                return vec![UiCommand::ShowLicence {
                    package: Some(crate::screens::licence_file(package).to_owned()),
                }];
            }
            SettingControl::Diagnostics(page) => {
                self.support = SupportView::Diagnostics(page);
                self.reset_scroll(ListId::SettingsPage);
            }
            SettingControl::Groups => {
                let current = match self.screen() {
                    Screen::Settings(page) => page.group() as usize,
                    _ => 0,
                };
                self.open_choice(ChoicePurpose::SettingsGroup, current);
            }
            SettingControl::Set(id, raw) => return self.set_setting(id, raw),
            SettingControl::Info(title, body) => {
                self.reset_scroll(ListId::Changelog);
                self.overlay = Some(Overlay::Info {
                    title: title.into(),
                    body: body.into(),
                });
            }
        }
        Vec::new()
    }

    /// Open a choice list scrolled so the current option shows near the top.
    /// Port enum discriminants equal their `ALL` index.
    pub(crate) fn open_choice(&mut self, purpose: ChoicePurpose, current: usize) {
        let offset = current.saturating_sub(2) as i32 * crate::screens::CHOICE_ROW as i32;
        self.scroll.insert(ListId::Choice, offset);
        self.overlay = Some(Overlay::Choice(purpose));
    }

    fn step_setting(&self, id: SettingId, steps: i32) -> Vec<UiCommand> {
        id.step(self.snapshot.setting(id), steps)
            .map(|value| vec![UiCommand::ChangeSetting { id, value }])
            .unwrap_or_default()
    }

    fn choose(&mut self, index: u16) -> Vec<UiCommand> {
        let Some(Overlay::Choice(purpose)) = self.overlay.take() else {
            return Vec::new();
        };
        let index = usize::from(index);
        match purpose {
            ChoicePurpose::Setting(id) => vec![UiCommand::ChangeSetting {
                id,
                value: SettingValue::Choice(index as u16),
            }],
            ChoicePurpose::RowInput(row) => InputPort::ALL
                .get(index)
                .map(|port| vec![UiCommand::SetRowInput { row, port: *port }])
                .unwrap_or_default(),
            ChoicePurpose::RowOutput(row) => OutputPort::ALL
                .get(index)
                .map(|port| vec![UiCommand::SetRowOutput { row, port: *port }])
                .unwrap_or_default(),
            ChoicePurpose::SaveAsSetlist => {
                match (self.save_as.take(), self.snapshot.setlists.get(index)) {
                    (Some((name, instrument)), Some(setlist)) => vec![UiCommand::SavePresetAs {
                        name,
                        setlist: Some(setlist.id),
                        instrument,
                    }],
                    _ => Vec::new(),
                }
            }
            ChoicePurpose::FilterType(parameter) => self.choose_filter_type(parameter, index),
            ChoicePurpose::Extension(setting) => self.choose_extension_setting(setting, index),
            ChoicePurpose::Mapping(choice) => self.choose_mapping(choice, index),
            ChoicePurpose::SettingsGroup => match SettingsGroup::ALL.get(index) {
                Some(group) => {
                    self.reset_scroll(ListId::SettingsMenu);
                    self.reset_scroll(ListId::SettingsPage);
                    self.navigate(Screen::Settings(group.pages()[0]))
                }
                None => Vec::new(),
            },
        }
    }

    /// Lane control and junction editors: a knob tap flips a toggle or steps a choice;
    /// paging and CLOSE as for blocks.
    fn site_param(&mut self, site: EditorSite, control: ParamControl) -> Vec<UiCommand> {
        match control {
            ParamControl::Knob(knob) => {
                self.focus = Some(knob);
                let index = self.param_page * KNOB_COUNT + usize::from(knob.get());
                if let Some(parameter) = self.snapshot.parameters.get(index)
                    && matches!(
                        parameter.kind,
                        ParameterKind::Toggle { .. }
                            | ParameterKind::Choice { .. }
                            | ParameterKind::ComboBox { .. }
                    )
                {
                    return vec![UiCommand::NudgeSite {
                        site,
                        parameter: parameter.index,
                        steps: 1,
                    }];
                }
            }
            ParamControl::Value(knob) => self.open_numeric(ParameterTarget::Site(site), knob),
            ParamControl::Page(page) => self.show_param_page(page),
            ParamControl::Close => return vec![UiCommand::ClearBlockSelection],
            ParamControl::Bypass
            | ParamControl::ChangeModel
            | ParamControl::Menu
            | ParamControl::Stomp => {}
        }
        Vec::new()
    }

    /// Directory Multi Select for Cortex Control BulkOperation READ (r21-control MR 5):
    /// `None` while Multi Select is off, else the number of checked items. Tetrad runs bulk
    /// actions as single requests, so there is no running bulk job to report.
    pub fn multi_select(&self) -> Option<usize> {
        self.directory.multi.as_ref().map(|checked| checked.len())
    }

    /// M406 `globalEqGateDisabledSlot` (`0x5b030c`): a deploy changed which global modules
    /// it switched off for CPU load. Any module off shows the flashing header status for
    /// 10 s; a tap on it opens the popup. Stock never opens the popup by itself. Nothing off
    /// hides the status.
    pub fn set_global_eq_inhibited(&mut self, global_eq: bool, input_gate: bool) {
        self.inhibit_status = (global_eq || input_gate).then_some(InhibitStatus {
            global_eq,
            input_gate,
            since: self.now,
            steady: false,
        });
    }

    /// Shows the Corrupt Neural Capture dialog for the captures the media lane removed
    /// (`0x54663c`, a `QStringList` of capture names). An empty list shows nothing.
    pub fn show_corrupt_captures(&mut self, names: Vec<String>) {
        if !names.is_empty() {
            self.overlay = Some(Overlay::CorruptCaptures { names });
        }
    }

    /// The cloud search or download of an NC editor DOWNLOAD failed
    /// (`0x3c841c`, `0x3c86a8` → `0x3c5aa0(editor, 1)`): the editor of that capture shows
    /// "Re-downloading is not possible" instead of the button.
    pub fn capture_download_failed(&mut self, hash: String) {
        self.capture_download_failed = Some(hash);
    }

    /// The capture of an NC editor DOWNLOAD is stored: an earlier failure of it clears.
    pub fn capture_download_stored(&mut self, hash: &str) {
        if self.capture_download_failed.as_deref() == Some(hash) {
            self.capture_download_failed = None;
        }
    }

    /// The first boot after an update (`0x54491c`, marker `changelog_boot` `0x4e4398`): the
    /// popup `+0x120` with title "CorOS: %1" (`0x351c58`) and the release notes (`0x351d24`).
    /// I: the popup class is not named; Tetrad uses its scrolled info card.
    pub fn show_update_notes(&mut self, version: &str, notes: String) {
        self.reset_scroll(ListId::Changelog);
        self.overlay = Some(Overlay::Info {
            title: format!("CorOS: {version}").into(),
            body: notes,
        });
    }

    /// Shows the Excessive Downloads dialog (`0x22cab0`; the device runs the check).
    pub fn show_excessive_downloads(&mut self, kind: ExcessDownloads) {
        self.overlay = Some(Overlay::ExcessiveDownloads {
            kind,
            dont_show: false,
        });
    }

    /// The box flips; CANCEL and SHOW DOWNLOADS close and store a checked box
    /// (`cancelSlot`, `showDownloadsSlot`). I: SHOW DOWNLOADS opens the Directory category
    /// (the Downloads folder view is not built).
    fn excessive_downloads_button(
        &mut self,
        kind: ExcessDownloads,
        dont_show: bool,
        button: DialogButton,
    ) -> Vec<UiCommand> {
        if button == DialogButton::Check {
            self.overlay = Some(Overlay::ExcessiveDownloads {
                kind,
                dont_show: !dont_show,
            });
            return Vec::new();
        }
        let mut commands: Vec<UiCommand> = dont_show
            .then_some(UiCommand::ChangeSetting {
                id: SettingId::DontShowExcessDownloads(kind),
                value: SettingValue::Bool(true),
            })
            .into_iter()
            .collect();
        if button == DialogButton::Primary {
            self.directory.category = match kind {
                ExcessDownloads::Captures => crate::screens::Category::Captures,
                ExcessDownloads::Presets => crate::screens::Category::Presets,
            };
            commands.extend(self.navigate(Screen::Presets));
        }
        commands
    }

    /// IRSaveDialog confirm (`0x7e542c`): the name simplified; the name of another user IR
    /// opens "Impulse Response already exists" (`0x7e503c`); else the IR is renamed. I: the
    /// check reads the user IRs the Directory lists (factory IRs cannot be deleted).
    fn rename_ir(&mut self, id: String, text: String) -> Vec<UiCommand> {
        let name = simplify_text(&text);
        let other = (self.snapshot.media.iter())
            .find(|entry| {
                entry.folder == MediaFolder::MyIrs && entry.value != id && entry.name == name
            })
            .map(|entry| entry.value.clone());
        if let Some(other) = other {
            self.overlay = Some(Overlay::IrExists { id, name, other });
            return Vec::new();
        }
        self.next_bulk_rename();
        vec![UiCommand::Directory(DirectoryRequest::RenameMedia {
            kind: MediaKind::Ir,
            id,
            name,
        })]
    }

    fn dialog(&mut self, button: DialogButton) -> Vec<UiCommand> {
        let Some(overlay) = self.overlay.take() else {
            return Vec::new();
        };
        // OVERWRITE needs the explicit tap (O: it deletes a user IR); RENAME and every other
        // close keep both IRs.
        if let Overlay::IrExists { id, name, other } = overlay {
            if button == DialogButton::Primary {
                self.next_bulk_rename();
                return vec![UiCommand::OverwriteIr { id, name, other }];
            }
            let edit = DirectoryNameEdit::Media {
                kind: MediaKind::Ir,
                id,
            };
            self.open_keyboard(TextTarget::DirectoryName(edit), name);
            return Vec::new();
        }
        if let Overlay::DownloadOverwrite {
            ids,
            setlist,
            slots,
        } = overlay
        {
            let overwrite = button == DialogButton::Primary;
            return self.answer_download_overwrite(ids, setlist, slots, overwrite);
        }
        if let Overlay::ExcessiveDownloads { kind, dont_show } = overlay {
            return self.excessive_downloads_button(kind, dont_show, button);
        }
        if let Overlay::CloudUnavailable(gate) = overlay {
            return match (button, gate.button().1) {
                (DialogButton::Primary, Some(page)) => self.navigate(Screen::Settings(page)),
                _ => Vec::new(),
            };
        }
        // M406: OK closes (`onOkButtonReleased`); CPU MONITOR closes and turns the CPU
        // monitor on (`onCpuMonButtonReleased` → `showCpuMonitor()`).
        // CANCEL closes (`cancelButtonSlot`); SEND REPORT stays disabled (no report service).
        if let Overlay::CorruptCaptures { .. } = overlay {
            return Vec::new();
        }
        // OK also hides the header status (`globalEqDisableAccept` →
        // `globalEqGateStatusTimerSlot`).
        if let Overlay::GlobalEqInhibited { .. } = overlay {
            match button {
                DialogButton::Primary => self.show_cpu = true,
                _ => self.inhibit_status = None,
            }
            return Vec::new();
        }
        if let Overlay::ExtensionTrust(shown) = overlay {
            return self.answer_trust(shown, button == DialogButton::Primary);
        }
        if let Overlay::ExtensionConfirm(confirm) = overlay {
            return match button {
                DialogButton::Primary => crate::screens::extensions::confirmed(confirm),
                _ => Vec::new(),
            };
        }
        let Overlay::Confirm(confirm) = overlay else {
            return Vec::new();
        };
        if button != DialogButton::Primary {
            return match confirm {
                Confirm::Expression(pedal) => {
                    vec![UiCommand::ExpressionCalibration { pedal, save: false }]
                }
                Confirm::DiscardCapture => vec![UiCommand::Capture {
                    request: CaptureRequest::Discard(false),
                }],
                Confirm::UploadOverwrite => self.answer_upload(false),
                Confirm::CaptureOverwrite | Confirm::CaptureUploadOverwrite => {
                    self.answer_capture(confirm, false)
                }
                _ => Vec::new(),
            };
        }
        match confirm {
            Confirm::DeletePreset(id) => vec![UiCommand::DeletePreset { id }],
            Confirm::DeleteLoadedPreset { id, load } => {
                vec![UiCommand::DeleteLoadedPreset { id, load }]
            }
            Confirm::Device(DeviceAction::ClearPresetMidi) => self.clear_preset_midi(),
            // Manual §10: the device stays locked while the reset runs (`0xa3d360`).
            Confirm::Device(action @ DeviceAction::FactoryReset) => {
                self.resetting = true;
                vec![UiCommand::Device { action }]
            }
            Confirm::Device(action) => vec![UiCommand::Device { action }],
            Confirm::RestoreBackup(id) => vec![UiCommand::RestoreBackup { id }],
            Confirm::DeleteBackup(id) => vec![UiCommand::DeleteBackup { id }],
            Confirm::UpdateBackup(id) => vec![UiCommand::UpdateBackup { id }],
            Confirm::AbortCapture => {
                let mut commands = vec![UiCommand::Capture {
                    request: CaptureRequest::Abort,
                }];
                commands.extend(self.pop());
                commands
            }
            Confirm::DiscardCapture => vec![UiCommand::Capture {
                request: CaptureRequest::Discard(true),
            }],
            Confirm::UploadOverwrite => self.answer_upload(true),
            Confirm::CaptureOverwrite | Confirm::CaptureUploadOverwrite => {
                self.answer_capture(confirm, true)
            }
            Confirm::Expression(pedal) => {
                vec![UiCommand::ExpressionCalibration { pedal, save: true }]
            }
            Confirm::DownloadUpdate => vec![UiCommand::Updater {
                command: UpdaterCommand::CheckAndDownload,
            }],
            Confirm::InstallUpdate => vec![UiCommand::Updater {
                command: UpdaterCommand::Install,
            }],
            Confirm::ResetLooperActions => vec![UiCommand::ResetLooperActions],
            // The one button flips the check (`0x5d5c8c`).
            Confirm::InternetCheck => {
                let on = self.snapshot.setting(SettingId::InternetCheck)
                    == Some(&SettingValue::Bool(true));
                vec![UiCommand::ChangeSetting {
                    id: SettingId::InternetCheck,
                    value: SettingValue::Bool(!on),
                }]
            }
            Confirm::AssignStomp(position) => vec![UiCommand::AssignStomp {
                position,
                tile: None,
            }],
            Confirm::DevicePreset { preset, delete } => self.device_preset_request(if delete {
                DevicePresetRequest::Delete(preset)
            } else {
                DevicePresetRequest::Overwrite(preset)
            }),
            Confirm::SettingsPreset {
                page,
                preset,
                delete,
            } => vec![UiCommand::SettingsPreset {
                page,
                request: if delete {
                    SettingsPresetRequest::Delete(preset)
                } else {
                    SettingsPresetRequest::Overwrite(preset)
                },
            }],
        }
    }

    fn device_action(&mut self, action: DeviceAction) -> Vec<UiCommand> {
        match action {
            DeviceAction::CalibrateExpression(pedal) => {
                self.overlay = Some(Overlay::Confirm(Confirm::Expression(pedal)));
                vec![UiCommand::Device { action }]
            }
            DeviceAction::ContinueBoot => {
                let mut commands = vec![UiCommand::Device { action }];
                commands.extend(self.navigate(Screen::Grid));
                commands
            }
            _ => {
                self.overlay = Some(Overlay::Confirm(Confirm::Device(action)));
                Vec::new()
            }
        }
    }

    /// Copy / Swap Scene (`0x5ad9d4`): the dialog opens and the device enters footswitch
    /// scene copy mode with `source` on the clipboard.
    fn begin_scene_copy(&mut self, source: SceneIndex, swap: bool) -> Vec<UiCommand> {
        self.scene_source = Some(source);
        self.overlay = Some(Overlay::SceneTarget { swap });
        vec![UiCommand::BeginSceneCopy { source, swap }]
    }

    /// An on-screen destination (owned): the copy or swap, then the device leaves scene copy
    /// mode. A footswitch destination is the device's (`0x20eec4`).
    pub(crate) fn scene_target(&mut self, swap: bool, scene: SceneIndex) -> Vec<UiCommand> {
        self.overlay = None;
        let current = self
            .scene_source
            .take()
            .unwrap_or(self.snapshot.selected_scene);
        let mut commands = Vec::new();
        if scene != current {
            commands.push(if swap {
                UiCommand::SwapScene {
                    first: current,
                    second: scene,
                }
            } else {
                UiCommand::CopyScene {
                    from: current,
                    to: scene,
                }
            });
        }
        commands.push(UiCommand::EndSceneCopy);
        commands
    }

    fn mode(&mut self, control: ModeControl) -> Vec<UiCommand> {
        let cycle = |mode: FootswitchMode| match mode {
            FootswitchMode::Preset => FootswitchMode::Scene,
            FootswitchMode::Scene => FootswitchMode::Stomp,
            _ => FootswitchMode::Preset,
        };
        let mut rows = self.snapshot.hybrid;
        match control {
            ModeControl::Allow(mode) => {
                let allowed = !self.snapshot.allowed_modes.contains(&mode);
                return vec![UiCommand::SetModeAllowed { mode, allowed }];
            }
            ModeControl::CycleUpper => rows.upper = cycle(rows.upper),
            ModeControl::CycleLower => rows.lower = cycle(rows.lower),
            control @ (ModeControl::Slot(_)
            | ModeControl::SwapRows
            | ModeControl::Add
            | ModeControl::Insert(_)
            | ModeControl::Info) => return self.modes_config(control),
        }
        vec![UiCommand::SetHybridRows { rows }]
    }

    fn routing(&mut self, control: RoutingControl) -> Vec<UiCommand> {
        let Some(Overlay::Routing(pair)) = self.overlay else {
            return Vec::new();
        };
        let view = self.snapshot.pairs[pair.index()];
        let step = |current: Option<u8>, delta: i8| {
            Some(match current {
                Some(column) => (i16::from(column) + i16::from(delta)).clamp(0, 7) as u8,
                None if delta > 0 => 0,
                None => 7,
            })
        };
        vec![match control {
            RoutingControl::Split(delta) => UiCommand::SetSplit {
                pair,
                column: step(view.split, delta),
            },
            RoutingControl::Mix(delta) => UiCommand::SetMix {
                pair,
                column: step(view.mix, delta),
            },
            RoutingControl::SplitOff => UiCommand::SetSplit { pair, column: None },
            RoutingControl::MixOff => UiCommand::SetMix { pair, column: None },
        }]
    }

    fn connect(&mut self, ssid: &str) -> Vec<UiCommand> {
        let Some(network) = self.snapshot.wifi.networks.iter().find(|n| n.ssid == ssid) else {
            return Vec::new();
        };
        if network.secured && !network.saved {
            let target = TextTarget::WifiPassword(network.ssid.clone());
            self.open_keyboard(target, String::new());
            Vec::new()
        } else {
            vec![UiCommand::WifiConnect {
                ssid: network.ssid.clone(),
                password: None,
            }]
        }
    }

    fn menu_action(&mut self, action: MenuAction) -> Vec<UiCommand> {
        let Some(Overlay::Menu(kind)) = self.overlay.take() else {
            return Vec::new();
        };
        let snapshot = &self.snapshot;
        match (kind, action) {
            (_, MenuAction::CreateNew) => return vec![UiCommand::CreatePreset],
            (_, MenuAction::SaveAs) => {
                let title = snapshot.preset_title.clone();
                self.open_keyboard(TextTarget::PresetSaveAs, title);
            }
            (MenuKind::Main, MenuAction::EditDetails) => {
                let title = snapshot.preset_title.clone();
                self.open_keyboard(TextTarget::PresetDetails, title);
            }
            (_, action @ (MenuAction::CopyScene | MenuAction::SwapScene)) => {
                let source = snapshot.selected_scene;
                return self.begin_scene_copy(source, action == MenuAction::SwapScene);
            }
            (_, MenuAction::ToggleFavorite) => {
                if let Some(id) = snapshot.preset_id {
                    return vec![UiCommand::SetFavorite {
                        id,
                        favorite: !snapshot.favorite,
                    }];
                }
            }
            (MenuKind::Main, MenuAction::DeletePreset) => {
                if let Some(id) = snapshot.preset_id {
                    let load = snapshot.preset_load;
                    self.overlay = Some(Overlay::Confirm(Confirm::DeleteLoadedPreset { id, load }));
                }
            }
            (_, MenuAction::NewNeuralCapture) => {
                self.capture = crate::screens::CaptureUi::default();
                let mut commands = self.navigate(Screen::Capture);
                commands.push(UiCommand::Capture {
                    request: CaptureRequest::Begin,
                });
                return commands;
            }
            (_, MenuAction::Tempo) => return self.navigate(Screen::Tempo),
            (_, MenuAction::ModesConfiguration) => self.overlay = Some(Overlay::Modes),
            (_, MenuAction::CpuMonitor) => self.show_cpu = !self.show_cpu,
            (_, MenuAction::Looper) => return self.navigate(Screen::Looper),
            (_, MenuAction::Mappings) => {
                self.mappings = MappingsPage {
                    tab: self.mappings.tab,
                    ..MappingsPage::default()
                };
                self.reset_scroll(ListId::Mappings);
                return self.navigate(Screen::Mappings);
            }
            (_, MenuAction::Remote) => {
                self.reset_scroll(ListId::SettingsPage);
                return self.navigate(Screen::Settings(SettingsPage::Remote));
            }
            (_, MenuAction::Settings) => {
                return self.navigate(Screen::Settings(SettingsPage::MyAccount));
            }
            (MenuKind::Block(position), MenuAction::ChangeDevice) => {
                self.open_picker(position, true)
            }
            (MenuKind::Block(position), MenuAction::AddDevice) => self.open_picker(position, false),
            (MenuKind::Block(position), MenuAction::CopyDevice) => {
                return vec![UiCommand::CopyBlock { position }];
            }
            (MenuKind::Block(position), MenuAction::PasteDevice) => {
                return vec![UiCommand::PasteBlock { position }];
            }
            (MenuKind::Block(position), MenuAction::ResetDefaults) => {
                return vec![UiCommand::ResetBlockDefaults { position }];
            }
            (MenuKind::Block(position), MenuAction::SaveDevicePreset) => {
                self.open_keyboard(TextTarget::DevicePresetSaveAs(position), String::new());
            }
            (MenuKind::Block(position), MenuAction::SetDefaults) => {
                return vec![UiCommand::DevicePreset {
                    position,
                    request: crate::DevicePresetRequest::SetDefaults,
                }];
            }
            (MenuKind::Block(position), MenuAction::ToggleBypass) => {
                return vec![UiCommand::SetBlockBypass {
                    position,
                    bypassed: !snapshot.cell(position).bypassed,
                }];
            }
            (MenuKind::Block(position), MenuAction::RemoveBlock) => {
                return vec![UiCommand::RemoveBlock { position }];
            }
            (MenuKind::Port { row, .. }, MenuAction::ChangeInput) => {
                let input = snapshot.rows[usize::from(row.get())].input;
                self.open_choice(ChoicePurpose::RowInput(row), input as usize);
            }
            (MenuKind::Port { row, .. }, MenuAction::ChangeOutput) => {
                let output = snapshot.rows[usize::from(row.get())].output;
                self.open_choice(ChoicePurpose::RowOutput(row), output as usize);
            }
            (MenuKind::Port { row, .. }, MenuAction::Routing) => {
                self.overlay = Some(Overlay::Routing(row.pair()));
            }
            (MenuKind::Backup(id), MenuAction::InstallBackup) => {
                self.overlay = Some(Overlay::Confirm(Confirm::RestoreBackup(id)));
            }
            (MenuKind::Backup(id), MenuAction::EditBackupName) => {
                let name = (self.snapshot.backups.items.iter())
                    .find(|backup| backup.id == id)
                    .map(|backup| backup.title.clone())
                    .unwrap_or_default();
                self.open_keyboard(TextTarget::BackupName(Some(id)), name);
            }
            (MenuKind::Backup(id), MenuAction::UpdateBackup) => {
                self.overlay = Some(Overlay::Confirm(Confirm::UpdateBackup(id)));
            }
            (MenuKind::Backup(id), MenuAction::DeleteBackup) => {
                self.overlay = Some(Overlay::Confirm(Confirm::DeleteBackup(id)));
            }
            (MenuKind::Looper, MenuAction::ReassignLooperActions) => {
                self.looper_swap = Some(LooperSwap::Choose);
            }
            (MenuKind::Looper, MenuAction::ResetLooperActions) => {
                self.overlay = Some(Overlay::Confirm(Confirm::ResetLooperActions));
            }
            (MenuKind::Block(position), MenuAction::AssignExpression) => {
                return self.open_expression(position);
            }
            // Items shown disabled (Preset MIDI Out) or actions that do not
            // match the menu target.
            _ => {}
        }
        Vec::new()
    }

    fn key(&mut self, key: Key) -> Vec<UiCommand> {
        let Some(keyboard) = &mut self.keyboard else {
            return Vec::new();
        };
        let (_, max) = keyboard.target.limits();
        let push = |keyboard: &mut crate::state::Keyboard, ch: char| -> bool {
            if keyboard.text.chars().count() >= max {
                return false;
            }
            if keyboard.target.accepts(ch) {
                keyboard.text.push(ch);
            }
            true
        };
        match key {
            Key::Char(ch) => {
                let ch = if keyboard.shift == Shift::Off {
                    ch
                } else {
                    ch.to_ascii_uppercase()
                };
                if keyboard.shift == Shift::Once {
                    keyboard.shift = Shift::Off;
                }
                if !push(keyboard, ch) {
                    self.show_toast("Character limit exceeded");
                }
            }
            Key::Space => {
                if !push(keyboard, ' ') {
                    self.show_toast("Character limit exceeded");
                }
            }
            Key::Backspace => {
                keyboard.text.pop();
            }
            Key::Shift => {
                keyboard.shift = match keyboard.shift {
                    Shift::Off => Shift::Once,
                    Shift::Once => Shift::Locked,
                    Shift::Locked => Shift::Off,
                };
            }
            Key::Layer(layer) => keyboard.layer = layer,
            Key::Cancel => {
                self.keyboard = None;
                self.directory.rename_queue.clear();
            }
            Key::Done => return self.commit_text(),
        }
        Vec::new()
    }

    fn commit_text(&mut self) -> Vec<UiCommand> {
        let Some(keyboard) = self.keyboard.take() else {
            return Vec::new();
        };
        let (min, _) = keyboard.target.limits();
        let text = if keyboard.target.secret() {
            keyboard.text.clone()
        } else {
            simplify_text(&keyboard.text)
        };
        if text.chars().count() < min {
            let message = match keyboard.target {
                TextTarget::WifiPassword(_) => "Invalid password length",
                _ if keyboard.target.restricted() => "New names can't be shorter than 4 characters",
                _ => "Text is too short",
            };
            self.keyboard = Some(keyboard);
            self.show_toast(message);
            return Vec::new();
        }
        match keyboard.target {
            TextTarget::SceneLabel => {
                if let Some(draft) = &mut self.draft {
                    draft.entries[usize::from(draft.selected.0)].label = text;
                }
                Vec::new()
            }
            TextTarget::PresetSaveAs => self.save_as(preset_name(&text), keyboard.instrument),
            TextTarget::PresetRename(id) => {
                self.next_bulk_rename();
                vec![UiCommand::RenamePreset {
                    id,
                    name: preset_name(&text),
                }]
            }
            TextTarget::PresetDetails => vec![UiCommand::EditPresetDetails {
                name: preset_name(&text),
                instrument: keyboard.instrument,
            }],
            TextTarget::SetlistName => vec![UiCommand::CreateSetlist { name: text }],
            TextTarget::DirectoryName(edit) => {
                if let DirectoryNameEdit::Media {
                    kind: MediaKind::Ir,
                    id,
                } = &edit
                {
                    return self.rename_ir(id.clone(), text);
                }
                self.next_bulk_rename();
                vec![UiCommand::Directory(edit.request(text))]
            }
            TextTarget::LibrarySearch => {
                self.commit_search(text);
                Vec::new()
            }
            TextTarget::ModelSearch => {
                self.picker.query = text;
                self.reset_scroll(ListId::PickerModels);
                Vec::new()
            }
            TextTarget::DevicePresetName(preset) => self
                .device_preset_request(crate::DevicePresetRequest::Rename { preset, name: text }),
            TextTarget::SettingsPresetName { page, preset } => vec![UiCommand::SettingsPreset {
                page,
                request: match preset {
                    Some(preset) => SettingsPresetRequest::Rename { preset, name: text },
                    None => SettingsPresetRequest::Create { name: text },
                },
            }],
            TextTarget::DevicePresetSaveAs(position) => vec![UiCommand::DevicePreset {
                position,
                request: crate::DevicePresetRequest::Create { name: text },
            }],
            TextTarget::WifiPassword(ssid) => vec![UiCommand::WifiConnect {
                ssid,
                password: Some(text),
            }],
            TextTarget::CaptureName => self.capture_named(text),
            TextTarget::StompLabel(switch) => vec![UiCommand::RenameStomp {
                switch,
                label: stomp_name(&text),
            }],
            TextTarget::SlotLabel(scope, page, slot) => {
                vec![UiCommand::Custom(crate::CustomCommand::Slot {
                    scope,
                    page,
                    slot,
                    edit: crate::SlotEdit::SetLabel(text),
                })]
            }
            TextTarget::LoginEmail => {
                self.open_keyboard(TextTarget::LoginPassword(text), String::new());
                Vec::new()
            }
            // Empty saves the default name (`0x64fd2c`).
            TextTarget::DeviceName => vec![UiCommand::SetDeviceName {
                name: if text.is_empty() {
                    self.default_device_name()
                } else {
                    text
                },
            }],
            TextTarget::BackupName(None) => vec![UiCommand::CreateBackup { name: text }],
            TextTarget::BackupName(Some(id)) => vec![UiCommand::RenameBackup { id, name: text }],
            TextTarget::ExtensionSource => {
                vec![UiCommand::Extension(crate::ExtensionCommand::AddSource {
                    repository: text,
                })]
            }
            TextTarget::LoginPassword(email) => vec![UiCommand::CloudLogin {
                email,
                password: text,
            }],
        }
    }

    /// Save as... after the name: a preset with a file picks a destination setlist when
    /// there is a choice (manual §5: the active setlist by default).
    fn save_as(&mut self, name: String, instrument: PreferredInstrument) -> Vec<UiCommand> {
        let snapshot = &self.snapshot;
        if !snapshot.preset_file {
            return vec![UiCommand::SavePresetAs {
                name,
                setlist: None,
                instrument,
            }];
        }
        if snapshot.setlists.len() > 1 {
            self.save_as = Some((name, instrument));
            self.open_choice(
                ChoicePurpose::SaveAsSetlist,
                snapshot
                    .active_setlist
                    .and_then(|id| snapshot.setlists.iter().position(|s| s.id == id))
                    .unwrap_or(0),
            );
            return Vec::new();
        }
        vec![UiCommand::SavePresetAs {
            name,
            setlist: snapshot.active_setlist,
            instrument,
        }]
    }
}

/// Preset names start with a capital letter (`0x58e46c`).
fn preset_name(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

impl UiState {
    fn default_device_name(&self) -> String {
        crate::screens::default_device_name(self.snapshot.mini).to_owned()
    }
    /// A TabBar tap jumps to the page (`0x609480`).
    fn show_param_page(&mut self, page: u8) {
        self.param_page = usize::from(page).min(self.last_param_page());
        self.focus = None;
    }

    /// The numeric keyboard of the knob's parameter (`0x4c62d4`), when it has a range.
    fn open_numeric(&mut self, target: ParameterTarget, knob: KnobIndex) {
        if let Some(parameter) = self.parameter_at(knob)
            && let Some(range) = parameter.entry
        {
            let index = parameter.index;
            self.focus = Some(knob);
            let purpose = crate::screens::NumericPurpose::Parameter {
                target,
                parameter: index,
            };
            self.numeric = Some(crate::screens::NumericEntry::new(purpose, range));
        }
    }
}
