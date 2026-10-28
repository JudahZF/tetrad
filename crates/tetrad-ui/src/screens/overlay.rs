//! Modal layers: scene label draft, menus, dialogs, choice lists, the on-screen keyboard, the Power
//! & Locking and Master Volume overlays and the Modes configuration. Each opens a new layer that
//! alone receives input.

use tetrad_assets::{Icon, IconSize, Rgb, block};

use super::grid::scene_label;
use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, HUGE_BOLD, LARGE_BOLD, Layout, ListId, Look, MEDIUM,
    MEDIUM_BOLD, SMALL_BOLD, XLARGE_BOLD,
};
use crate::render::{Align, HEIGHT, WIDTH, wrap};
use crate::settings::{DeviceAction, SettingKind};
use crate::state::{
    ChoicePurpose, Confirm, Gesture, LONG_PRESS_MS, MenuKind, Overlay, PowerState, Shift,
    TextTarget,
};
use crate::*;

const RADIUS: f32 = 8.0;
const FULL: Rect = Rect::new(0, 0, WIDTH, HEIGHT);

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    if let Some(draft) = &ui.draft {
        l.overlay();
        scene_draft(ui, l, draft);
    }
    if let Some(overlay) = &ui.overlay {
        // Dropdown lists and contextual menus open in place at their trigger.
        let anchor = trigger(overlay).and_then(|control| l.anchor(control));
        // The volume overlay floats over the page without a scrim.
        if anchor.is_none() && *overlay != Overlay::Volume {
            l.overlay();
        }
        match overlay {
            Overlay::Menu(MenuKind::Main) => main_menu(ui, l, anchor),
            Overlay::Menu(kind) => popup(ui, l, kind, anchor),
            Overlay::Confirm(Confirm::DownloadUpdate) => super::updater::download_confirm(ui, l),
            Overlay::Confirm(Confirm::InstallUpdate) => super::updater::install_confirm(ui, l),
            Overlay::Confirm(Confirm::AbortCapture) => abort_confirm(ui, l, anchor),
            Overlay::Confirm(Confirm::AssignStomp(position)) => assign_stomp(ui, l, *position),
            Overlay::Confirm(Confirm::DevicePreset { preset, delete }) => {
                device_preset_confirm(ui, l, *preset, *delete);
            }
            Overlay::Confirm(Confirm::SettingsPreset { preset, delete, .. }) => {
                device_preset_confirm(ui, l, *preset, *delete);
            }
            Overlay::Confirm(Confirm::UploadOverwrite) => upload_overwrite(ui, l),
            Overlay::Confirm(Confirm::CaptureOverwrite) => capture_overwrite(ui, l),
            Overlay::Confirm(Confirm::CaptureUploadOverwrite) => capture_upload_overwrite(ui, l),
            Overlay::Confirm(Confirm::UpdateBackup(id)) => update_backup(ui, l, *id),
            Overlay::IrExists { .. } => ir_exists(l),
            Overlay::DownloadOverwrite { .. } => download_overwrite(ui, l),
            Overlay::Confirm(Confirm::InternetCheck) => internet_check(ui, l),
            Overlay::Confirm(confirm) => self::confirm(l, *confirm),
            Overlay::Choice(purpose) => choice(ui, l, *purpose, anchor),
            Overlay::Info { title, body } => info(ui, l, title, body),
            Overlay::EditStomp(tile) => edit_stomp(ui, l, *tile),
            Overlay::Notice {
                title,
                body,
                button,
            } => {
                let panel = message_panel(l, title, body);
                l.button(
                    Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
                    Control::Dialog(DialogButton::Primary),
                    true,
                    *button,
                    ButtonStyle::Primary,
                );
            }
            Overlay::CloudUnavailable(gate) => {
                let panel = message_panel(l, gate.title(), gate.body());
                l.button(
                    Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
                    Control::Dialog(DialogButton::Primary),
                    true,
                    gate.button().0,
                    ButtonStyle::Primary,
                );
            }
            Overlay::ExcessiveDownloads { kind, dont_show } => {
                excessive_downloads(l, *kind, *dont_show);
            }
            Overlay::CorruptCaptures { names } => corrupt_captures(l, names),
            Overlay::GlobalEqInhibited {
                global_eq,
                input_gate,
            } => global_eq_inhibited(l, *global_eq, *input_gate),
            Overlay::SceneTarget { swap } => scene_target(ui, l, *swap),
            Overlay::Routing(pair) => routing(ui, l, *pair),
            Overlay::Modes => modes(ui, l),
            Overlay::ModesAdd => {
                modes(ui, l);
                modes_add(ui, l);
            }
            Overlay::ModesInfo => {
                modes(ui, l);
                l.overlay();
                info(ui, l, "MIDI", MODES_INFO);
            }
            Overlay::Power => power(l),
            Overlay::Volume => volume(ui, l),
            Overlay::SettingsPresets { page, menu } => {
                super::settings_presets::layout(ui, l, *page, *menu);
            }
            Overlay::ExtensionConfirm(confirm) => super::extensions::confirm(l, confirm),
            Overlay::ExtensionTrust(pending) => super::extensions::trust(l, pending),
            Overlay::MappingActions(menu) => super::mappings::action_menu(ui, l, *menu),
        }
    }
    if let Some(keyboard) = &ui.keyboard {
        l.overlay();
        match keyboard.metadata_page {
            true => super::preset_metadata::page(ui, l, keyboard),
            false => self::keyboard(ui, l, keyboard),
        }
    }
    if ui.snapshot.dsp_error {
        dsp_error(ui, l);
    }
    match ui.power {
        PowerState::Awake => {}
        PowerState::Locked => lock_indicator(l),
        // Be Right Back: the screen is off.
        PowerState::Away => l.push(FULL, "", Look::Fill(p.canvas)),
    }
}

/// A tap at the last pointer position: the anchor of a menu or list whose
/// trigger is not on screen (scrolled away, or opened without a touch).
fn pointer_anchor(ui: &UiState) -> Rect {
    Rect::new(ui.pointer.0, ui.pointer.1, 1, 1)
}

/// The control whose item an anchored overlay opens from, if it has one.
fn trigger(overlay: &Overlay) -> Option<Control> {
    Some(match overlay {
        Overlay::Choice(ChoicePurpose::Setting(id)) => Control::Setting(SettingControl::Open(*id)),
        Overlay::Choice(ChoicePurpose::RowInput(row)) => Control::Port {
            row: *row,
            output: false,
        },
        Overlay::Choice(ChoicePurpose::RowOutput(row)) => Control::Port {
            row: *row,
            output: true,
        },
        Overlay::Choice(ChoicePurpose::SettingsGroup) => Control::Setting(SettingControl::Groups),
        Overlay::Choice(ChoicePurpose::Extension(setting)) => {
            Control::Extension(ExtensionControl::Setting(*setting))
        }
        Overlay::Choice(ChoicePurpose::Mapping(choice)) => {
            Control::Mapping(MappingControl::Choice(*choice))
        }
        // The ⋮ menu's grey veil instead of the scrim.
        Overlay::MappingActions(_) => Control::Mapping(MappingControl::Actions),
        Overlay::Menu(MenuKind::Main) => Control::OpenMainMenu,
        Overlay::Confirm(Confirm::AbortCapture) => Control::Capture(CaptureRequest::Abort),
        Overlay::Menu(MenuKind::Block(position)) => Control::SelectCell(*position),
        Overlay::Menu(MenuKind::Port { row, output }) => Control::Port {
            row: *row,
            output: *output,
        },
        Overlay::Menu(MenuKind::Backup(id)) => Control::Backup(BackupControl::Item(*id)),
        Overlay::Menu(MenuKind::Looper) => Control::LooperMenu,
        // Its own grey veil (`OptionsPopup` `0x688f3c`) instead of the scrim.
        Overlay::SettingsPresets { page, .. } => {
            Control::SettingsPreset(SettingsPresetControl::Open(*page))
        }
        _ => return None,
    })
}

fn dialog_panel(l: &mut Layout, bounds: Rect, title: &str) {
    let p = l.palette;
    l.panel(bounds, p.panel, RADIUS, None);
    l.text(
        Rect::new(bounds.x + 24, bounds.y + 16, bounds.width - 48, 28),
        title,
        LARGE_BOLD,
        p.text,
        Align::Left,
    );
}

/// Owned "Scene labels" dialog (review R25). Stock has no such modal: the Gig tile EDIT
/// SCENE pencil opens an editor for one scene's name and colour, which tetrad-ui does not
/// have yet. Kept as the only scene-label path until that editor exists.
fn scene_draft(ui: &UiState, l: &mut Layout, draft: &crate::state::SceneDraft) {
    let p = l.palette;
    let panel = Rect::new(80, 70, 640, 340);
    dialog_panel(l, panel, "Scene labels");
    for scene in SceneIndex::all() {
        let index = usize::from(scene.get());
        l.button(
            Rect::new(
                100 + (index % 4) as i32 * 152,
                120 + (index / 4) as i32 * 52,
                140,
                44,
            ),
            Control::SelectScene(scene),
            true,
            scene_label(scene, &draft.entries[index]),
            ButtonStyle::Tab {
                selected: draft.selected == scene,
            },
        );
    }
    let label = &draft.entries[usize::from(draft.selected.get())].label;
    l.text(
        Rect::new(100, 226, 600, 20),
        "Name",
        BODY_BOLD,
        p.muted,
        Align::Left,
    );
    l.control(
        Rect::new(100, 250, 600, 52),
        Control::EditText,
        true,
        if label.is_empty() {
            "Enter text...".to_owned()
        } else {
            label.clone()
        },
        Look::Button {
            style: ButtonStyle::Tab { selected: false },
            icon: Some(Icon::Pencil),
            font: None,
        },
    );
    l.button(
        Rect::new(100, 340, 180, 48),
        Control::CloseDialog,
        true,
        "CLOSE",
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(520, 340, 180, 48),
        Control::ConfirmScenes,
        !ui.snapshot.bank_preview_active,
        "SAVE",
        ButtonStyle::Primary,
    );
}

fn main_menu(ui: &UiState, l: &mut Layout, _anchor: Option<Rect>) {
    let p = l.palette;
    let s = &ui.snapshot;
    // `0x003bbb90`: without a file, Save as, Edit Details, favorites and Delete are grey.
    let loaded = s.preset_id.is_some() && s.preset_file;
    let scene = s.selected_scene.letter();
    let idle = !s.bank_preview_active;
    let item = |action, label: String, icon, enabled| MenuRow::Item(action, label, icon, enabled);
    let mut rows = vec![
        MenuRow::Header("FILE"),
        item(
            MenuAction::CreateNew,
            "Create New".into(),
            Icon::Spark,
            true,
        ),
        item(
            MenuAction::SaveAs,
            "Save as...".into(),
            Icon::SaveArrow,
            loaded,
        ),
        item(
            MenuAction::EditDetails,
            "Edit Details".into(),
            Icon::Edit,
            loaded,
        ),
        item(
            MenuAction::CopyScene,
            format!("Copy Scene {scene}"),
            Icon::Copy,
            idle,
        ),
        item(
            MenuAction::SwapScene,
            format!("Swap Scene {scene}"),
            Icon::Swap,
            idle,
        ),
        // Owned (Tetrad): Mappings in place of stock Preset MIDI Out.
        item(MenuAction::Mappings, "Mappings".into(), Icon::Midi, true),
        item(
            MenuAction::ToggleFavorite,
            if s.favorite {
                "In your favorites"
            } else {
                "Add to favorites"
            }
            .into(),
            Icon::Star,
            loaded,
        ),
        item(
            MenuAction::DeletePreset,
            "Delete Preset".into(),
            Icon::TrashCross,
            loaded,
        ),
        MenuRow::Header("QUAD CORTEX"),
        item(
            MenuAction::NewNeuralCapture,
            "New Neural Capture".into(),
            Icon::NeuralCaptureMenu,
            true,
        ),
        item(
            MenuAction::ModesConfiguration,
            "Modes Configuration".into(),
            Icon::Footswitch,
            true,
        ),
        item(MenuAction::Tempo, "Tempo".into(), Icon::Metronome, true),
        item(
            MenuAction::CpuMonitor,
            "CPU Monitor".into(),
            Icon::Chip,
            true,
        ),
        // Stock 4.1.0 ends here: I/O Settings opens by a swipe down on the Grid, Looper X by a
        // tap on its block.
        item(MenuAction::Settings, "Settings".into(), Icon::Gear, true),
    ];
    // Owned (Tetrad): while Remote runs and its "Show PIN in menu" switch is on, the PIN is the
    // first row of the menu, so that it shows as soon as the menu opens. A tap opens
    // Settings → Tetrad → Remote.
    if let Some(pin) = s.remote.pin.as_ref().filter(|_| s.remote.pin_in_menu) {
        rows.insert(
            0,
            item(
                MenuAction::Remote,
                format!("Remote #{pin}"),
                Icon::Wifi,
                true,
            ),
        );
    }
    // Stock 4.1.0 ⋮ menu (seen on the QC, 2026-10-15): the grid dims, a 270 px #101010 list on
    // the right from y 16 to 436 with 60 px rows, FILE / QUAD CORTEX section headers and a 2 px
    // scroll thumb (seven rows show). An available update puts a 16 px green dot on Settings.
    const ROW: u32 = 60;
    let panel = Rect::new(472, 16, 270, 420);
    let update = s.updater.available_version.is_some();
    let (veil, alpha) = crate::layout::GREY_VEIL;
    l.veil((p.tone(veil), alpha));
    l.popover(panel);
    l.panel(panel, p.tone(crate::widgets::PAGE), RADIUS, None);
    let content = rows.len() as u32 * ROW;
    let offset = ui.offset(ListId::Menu);
    l.scroll(ListId::Menu, panel, content, offset, |l, top| {
        for (index, row) in rows.iter().enumerate() {
            let bounds = Rect::new(panel.x, top + (index as u32 * ROW) as i32, panel.width, ROW);
            match row {
                MenuRow::Header(title) => l.text(
                    Rect::new(bounds.x + 16, bounds.y, bounds.width - 32, ROW),
                    *title,
                    CAPTION,
                    p.text,
                    Align::Left,
                ),
                MenuRow::Item(action, label, icon, enabled) => {
                    l.control(
                        bounds,
                        Control::Menu(*action),
                        *enabled,
                        label.as_str(),
                        Look::ListRow {
                            icon: Some(*icon),
                            value: if *action == MenuAction::CpuMonitor && ui.show_cpu {
                                "On".into()
                            } else {
                                String::new()
                            },
                            selected: false,
                            divider: false,
                            menu: false,
                            font: MEDIUM,
                        },
                    );
                    if update && *action == MenuAction::Settings {
                        let dot = Rect::new(bounds.right() - 43, bounds.y + 22, 16, 16);
                        l.push(dot, "", Look::Dot { color: p.accent });
                    }
                }
            }
        }
    });
    super::scrollbar(l, panel, content, offset, 2);
}

/// One row of the ⋮ main menu.
enum MenuRow {
    Header(&'static str),
    Item(MenuAction, String, Icon, bool),
}

/// Block, preset, port, backup and network menus: the stock contextual popover
/// (shot 040), at the trigger or else at the last touch.
fn popup(ui: &UiState, l: &mut Layout, kind: &MenuKind, anchor: Option<Rect>) {
    let s = &ui.snapshot;
    let items: Vec<(MenuAction, &str, bool)> = match kind {
        MenuKind::Main => unreachable!("main menu has its own layout"),
        MenuKind::Block(position) if s.occupied(*position) => {
            let mut items = vec![
                (MenuAction::ChangeDevice, "Change device", true),
                (MenuAction::CopyDevice, "Copy device", true),
                (MenuAction::PasteDevice, "Paste device", s.clipboard),
                (MenuAction::ResetDefaults, "Reset to defaults", true),
            ];
            // Neural Captures have no Virtual Device Presets (manual "Unsupported Items").
            if s.blocks[position.index()].category != Some(ModelCategory::NeuralCapture) {
                items.extend([
                    (
                        MenuAction::SaveDevicePreset,
                        "Save Current Parameters As\u{2026}",
                        true,
                    ),
                    (MenuAction::SetDefaults, "Set Parameters as Defaults", true),
                ]);
            }
            items.extend([
                (
                    MenuAction::AssignExpression,
                    "Assign expression pedal",
                    true,
                ),
                (MenuAction::ToggleBypass, "Mute/bypass", true),
                (MenuAction::RemoveBlock, "Remove block from the grid", true),
            ]);
            if s.blocks[position.index()].category == Some(ModelCategory::Looper) {
                items.push((MenuAction::Looper, "Looper X", true));
            }
            items
        }
        MenuKind::Block(_) => vec![
            (MenuAction::AddDevice, "Add device", true),
            (MenuAction::PasteDevice, "Paste device", s.clipboard),
        ],
        MenuKind::Port { output, .. } => vec![
            if *output {
                (MenuAction::ChangeOutput, "CHANGE OUTPUT", true)
            } else {
                (MenuAction::ChangeInput, "CHANGE INPUT", true)
            },
            (MenuAction::Routing, "Split / Mix", true),
        ],
        MenuKind::Backup(_) => vec![
            (MenuAction::InstallBackup, "Install backup", true),
            (MenuAction::EditBackupName, "Edit name", true),
            // `0x3fd4b8`: Edit name, Update, Download, Delete Backup.
            (MenuAction::UpdateBackup, "Update", true),
            (MenuAction::DeleteBackup, "Delete Backup", true),
        ],
        // `0x94d14`.
        MenuKind::Looper => vec![
            (
                MenuAction::ReassignLooperActions,
                "Reassign Looper X actions",
                true,
            ),
            (
                MenuAction::ResetLooperActions,
                "Reset Looper X actions",
                true,
            ),
        ],
    };
    let items: Vec<_> = items
        .into_iter()
        .map(|(action, label, enabled)| (Control::Menu(action), label, enabled))
        .collect();
    crate::widgets::popover_menu(
        l,
        anchor.unwrap_or_else(|| pointer_anchor(ui)),
        &items,
        None,
    );
}

fn confirm(l: &mut Layout, confirm: Confirm) {
    let (title, body, primary, danger) = match confirm {
        Confirm::DeletePreset(_) | Confirm::DeleteLoadedPreset { .. } => (
            "Delete Preset",
            "Are you sure you want to delete this preset?",
            "DELETE",
            true,
        ),
        Confirm::Device(DeviceAction::FactoryReset) => (
            "WARNING!",
            "After pressing RESET, the device will be restored to factory settings and all data will be deleted. You will also be logged out of your Neural DSP account on this device. Your Cortex Cloud account will not be changed.",
            "RESET",
            true,
        ),
        Confirm::Device(DeviceAction::ClearPresetMidi) => (
            "Are you sure?",
            "This will delete all your MIDI assignments",
            "DELETE",
            true,
        ),
        Confirm::Device(DeviceAction::ResetSettings) => {
            ("RESET SETTINGS", "Are you sure?", "RESET", true)
        }
        Confirm::Device(DeviceAction::ResetWifi) => (
            "RESET WI-FI SETTINGS",
            "Pressing RESET will erase all stored Wi-Fi networks.",
            "RESET",
            true,
        ),
        Confirm::Device(DeviceAction::Reboot) => (
            "Restart device",
            "Any unsaved changes will be lost",
            "RESTART",
            false,
        ),
        Confirm::Device(DeviceAction::Shutdown) => (
            "SHUTDOWN",
            "Any unsaved changes will be lost",
            "SHUTDOWN",
            true,
        ),
        Confirm::Device(DeviceAction::CloudLogout) => (
            "UNLINK DEVICE",
            "Are you sure you want to unlink this device?",
            "YES, I'M SURE",
            true,
        ),
        Confirm::Device(_) => ("Are you sure?", "", "PROCEED", false),
        Confirm::RestoreBackup(_) => ("Backup ready to install", "Are you sure?", "PROCEED", false),
        Confirm::DeleteBackup(_) => ("Delete Cloud Backup", "Are you sure?", "DELETE", true),
        Confirm::RemoteRestore => (
            "Restore backup",
            "The backup from Remote replaces the presets, IRs, captures and settings on this unit.",
            "RESTORE",
            true,
        ),
        Confirm::AbortCapture => unreachable!("abort confirm has its own layout"),
        // `0x3cea8c`: title `0xa36bac`, text `0xa36b58`, button "OK".
        Confirm::DiscardCapture => (
            "Unsaved capture",
            "A capture has not been saved, are you sure you want to quit?",
            "OK",
            false,
        ),
        Confirm::DownloadUpdate | Confirm::InstallUpdate => {
            unreachable!("the updater confirms have their own layout")
        }
        Confirm::InternetCheck => unreachable!("the internet check popup has its own layout"),
        Confirm::AssignStomp(_) => unreachable!("footswitch assignment has its own layout"),
        Confirm::DevicePreset { .. }
        | Confirm::SettingsPreset { .. }
        | Confirm::UploadOverwrite
        | Confirm::CaptureOverwrite
        | Confirm::CaptureUploadOverwrite
        | Confirm::UpdateBackup(_) => unreachable!("these confirms name their item"),
        // `0x4be114`.
        Confirm::ResetLooperActions => (
            "Reset Looper X Actions",
            "Are you sure you want to reset all footswitch assignments to factory defaults?",
            "ACCEPT",
            false,
        ),
        Confirm::Expression(_) => (
            "Expression pedal",
            "Please press the expression pedal up and down to the maximum position then press Save",
            "SAVE",
            false,
        ),
    };
    let panel = message_panel(l, title, body);
    confirm_buttons(l, panel, "CANCEL", primary, danger);
}

/// "Automatic internet connectivity check" (`0x5d3818`): the stock text and one button that
/// names the change, ENABLE while the check is off, DISABLE while it is on.
fn internet_check(ui: &UiState, l: &mut Layout) {
    let on = ui.snapshot.setting(SettingId::InternetCheck) == Some(&SettingValue::Bool(true));
    let body = format!(
        "When disabled, {} assumes it has an internet connection when it is connected via \
         Wi-Fi with a valid IP address. Enable this if you have issues connecting to Cortex \
         Cloud due to network or geographical restrictions.",
        super::support::product_name(ui.snapshot.mini),
    );
    let panel = message_panel(l, "Automatic internet connectivity check", &body);
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Primary),
        true,
        if on { "DISABLE" } else { "ENABLE" },
        ButtonStyle::Primary,
    );
}

/// The cancel button (`cancel`: "CANCEL", or "SKIP" in a batch) and the primary button of a
/// MessageDialog card.
pub(super) fn confirm_buttons(
    l: &mut Layout,
    panel: Rect,
    cancel: &str,
    primary: &str,
    danger: bool,
) {
    l.button(
        Rect::new(panel.x + 24, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Cancel),
        true,
        cancel,
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Primary),
        true,
        primary,
        if danger {
            ButtonStyle::Danger
        } else {
            ButtonStyle::Primary
        },
    );
}

/// Virtual Device Preset confirms. The noun is `0x1f5b58` (Global EQ 4004, I/O Settings
/// 31000, else Virtual Device). OVERWRITE `0x39793c`: title "%1 Overwrite"; the I/O form
/// drops "with the current parameter settings" (kind 2, I: the I/O list). DELETE `0x397cb4`:
/// the preset name as title, DELETE / CANCEL.
fn device_preset_confirm(ui: &UiState, l: &mut Layout, preset: u64, delete: bool) {
    let Some(item) = ui.snapshot.device_presets.iter().find(|p| p.id == preset) else {
        return;
    };
    let noun = match item.model_id {
        4004 => "Global EQ Preset",
        31_000 => "I/O Settings Preset",
        _ => "Virtual Device Preset",
    };
    let (title, body, primary) = if delete {
        (
            item.name.clone(),
            format!("Are you sure you want to delete this {noun}?"),
            "DELETE",
        )
    } else {
        let tail = if item.model_id == 31_000 {
            ""
        } else {
            " with the current parameter settings"
        };
        (
            format!("{noun} Overwrite"),
            format!(
                "Are you sure you want to overwrite the {noun} '{}'{tail}?",
                item.name
            ),
            "OVERWRITE",
        )
    };
    let panel = message_panel(l, &title, &body);
    confirm_buttons(l, panel, "CANCEL", primary, delete);
}

/// The Directory upload overwrite dialog (`0x46485c` → `0x463d2c` → `0x5483f8`): one item
/// "Preset already exists" / "Preset %1 already exists. Overwrite?", OVERWRITE and CANCEL; in
/// a batch "Overwrite Existing Presets?" / "Some of the Presets you are trying to upload
/// already exist in your account.", OVERWRITE and SKIP (C texts). I: the MessageDialog card
/// and button places of the other confirms.
fn upload_overwrite(ui: &UiState, l: &mut Layout) {
    let Some(name) = &ui.snapshot.upload_overwrite else {
        return;
    };
    let panel = if ui.snapshot.upload_batch {
        message_panel(
            l,
            "Overwrite Existing Presets?",
            "Some of the Presets you are trying to upload already exist in your account.",
        )
    } else {
        let body = format!("Preset {name} already exists. Overwrite?");
        message_panel(l, "Preset already exists", &body)
    };
    overwrite_buttons(ui, l, panel);
}

/// OVERWRITE and CANCEL, or SKIP when the question belongs to a batch (`0x463d2c`).
fn overwrite_buttons(ui: &UiState, l: &mut Layout, panel: Rect) {
    let cancel = if ui.snapshot.upload_batch {
        "SKIP"
    } else {
        "CANCEL"
    };
    confirm_buttons(l, panel, cancel, "OVERWRITE", false);
}

/// "Neural Capture already exists" for a cloud capture download (`0x465178` mode 1, C
/// texts): `%1` the product name, `%2` the stored user capture, `%3` the cloud capture;
/// OVERWRITE and CANCEL (`0x463d2c`). I: the MessageDialog card and button places.
fn capture_overwrite(ui: &UiState, l: &mut Layout) {
    let Some(question) = &ui.snapshot.capture_overwrite else {
        return;
    };
    let body = format!(
        "Neural Capture {} already exists on your {} ({}). Would you like to overwrite it?",
        question.name,
        super::product_name(ui.snapshot.mini),
        question.existing
    );
    let panel = message_panel(l, "Neural Capture already exists", &body);
    confirm_buttons(l, panel, "CANCEL", "OVERWRITE", false);
}

/// "Neural Capture already exists" for a capture upload (`0x465178` mode 0, C texts): `%1`
/// the capture name, OVERWRITE and CANCEL; in a batch the batch text, OVERWRITE and SKIP
/// (`0x463d2c`). I: the card and button places.
fn capture_upload_overwrite(ui: &UiState, l: &mut Layout) {
    let Some(name) = &ui.snapshot.capture_upload_overwrite else {
        return;
    };
    let body = match ui.snapshot.upload_batch {
        true => "Some of the neural captures you selected already exist in your account. Would \
                 you like to overwrite them?"
            .to_owned(),
        false => format!(
            "Neural Capture {name} already exists in your account. Would you like to overwrite it?"
        ),
    };
    let panel = message_panel(l, "Neural Capture already exists", &body);
    overwrite_buttons(ui, l, panel);
}

/// "Update Cloud Backup" (`0x3f8730`, C texts): `%1` is the backup date, `%2` the product
/// name (`0x498ce4`); CANCEL and PROCEED (`0x547fd0`). The date has the stock form of the
/// backup list (`0x3f59ac`, ordinal day). I: the card and button places.
fn update_backup(ui: &UiState, l: &mut Layout, id: u64) {
    let date = (ui.snapshot.backups.items.iter())
        .find(|item| item.id == id)
        .map(|item| super::storage::stock_timestamp(&item.detail))
        .unwrap_or_default();
    let body = format!(
        "This will replace the Cloud Backup made on {date} with the current files on your {}",
        super::product_name(ui.snapshot.mini)
    );
    let panel = message_panel(l, "Update Cloud Backup", &body);
    confirm_buttons(l, panel, "CANCEL", "PROCEED", false);
}

/// IRSaveDialog duplicate name (`0x7e503c`): the stock texts, RENAME and OVERWRITE. I: the
/// card and the button places (the dialog layout is not measured); OVERWRITE is drawn as a
/// danger button because it deletes the other IR (O).
fn ir_exists(l: &mut Layout) {
    let panel = message_panel(
        l,
        "Impulse Response already exists",
        "Impulse Response of this name already exists on your device. Please choose a different name or press Overwrite.",
    );
    l.button(
        Rect::new(panel.x + 24, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Cancel),
        true,
        "RENAME",
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Primary),
        true,
        "OVERWRITE",
        ButtonStyle::Danger,
    );
}

/// "Overwrite Existing Presets?" of a Multi Select run (`0x46485c(name, 1)` → `0x463d2c`
/// batch branch): the stock texts ("upload" is the stock word for this download too, C) with
/// `%1` the product name (`0x498ce4`), SKIP and OVERWRITE (`0x463d2c`, C). I: the card and the
/// button places; OVERWRITE is drawn as a danger button (O: it replaces stored presets).
fn download_overwrite(ui: &UiState, l: &mut Layout) {
    let body = format!(
        "Some of the Presets you are trying to upload already exist on your {}.",
        super::product_name(ui.snapshot.mini)
    );
    let panel = message_panel(l, "Overwrite Existing Presets?", &body);
    l.button(
        Rect::new(panel.x + 24, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Cancel),
        true,
        "SKIP",
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Primary),
        true,
        "OVERWRITE",
        ButtonStyle::Danger,
    );
}

/// The MessageDialog card: title, wrapped text and room for one button row. 480 × 240 with a
/// 90 px body; a longer body (the stock Factory Reset WARNING! text, `0xa3d1bc`, wraps to 5
/// lines) grows the panel around the screen centre.
pub(super) fn message_panel(l: &mut Layout, title: &str, body: &str) -> Rect {
    let p = l.palette;
    let lines = wrap(body, MEDIUM, 432.0).len() as u32;
    let body_height = (lines * MEDIUM.size.line_px()).max(90);
    let height = 150 + body_height;
    let panel = Rect::new(160, (HEIGHT as i32 - height as i32) / 2, 480, height);
    dialog_panel(l, panel, title);
    l.paragraph(
        Rect::new(panel.x + 24, panel.y + 60, panel.width - 48, body_height),
        body,
        MEDIUM,
        p.muted,
    );
    panel
}

/// M406 (`0x375b40`, layout `0x376204`): an 800 × 480 veil (opacity 0.7), a `#121212`
/// panel 420 × 236 at (190, 114) (420 × 260 at (190, 102) when both are off), the title
/// (Medium 20 px, centred at y 32), the body (Medium 18 px, 379 wide at (20, 81)), OK and
/// CPU MONITOR (`#1A3AF8`) 157 × 45 at x 48 and 214, y 159 (182). I: the veil colour and
/// the OK look.
fn global_eq_inhibited(l: &mut Layout, global_eq: bool, input_gate: bool) {
    let p = l.palette;
    let (title, body) = match (global_eq, input_gate) {
        (true, true) => (
            "Global EQ and Input Gate disabled",
            "Global EQ and Input Gate were automatically disabled, as the CPU load for this Preset is too high.",
        ),
        (true, false) => (
            "Global EQ disabled",
            "Global EQ was automatically disabled, as the CPU load for this Preset is too high.",
        ),
        _ => (
            "Input Gate disabled",
            "Input Gate was automatically disabled, as the CPU load for this Preset is too high.",
        ),
    };
    let both = global_eq && input_gate;
    let (color, alpha) = crate::layout::LOADING_VEIL;
    let color = p.tone(color);
    l.push(FULL, "", Look::Veil { color, alpha });
    let panel = if both {
        Rect::new(190, 102, 420, 260)
    } else {
        Rect::new(190, 114, 420, 236)
    };
    l.panel(panel, p.background, RADIUS, None);
    let at = |x, y, width, height| Rect::new(panel.x + x, panel.y + y, width, height);
    l.text(
        at(20, 32, 379, 26),
        title,
        LARGE_BOLD,
        p.text,
        Align::Center,
    );
    l.paragraph(
        at(20, 81, 379, if both { 69 } else { 49 }),
        body,
        MEDIUM_BOLD,
        p.text,
    );
    let row = if both { 182 } else { 159 };
    l.button(
        at(48, row, 157, 45),
        Control::Dialog(DialogButton::Cancel),
        true,
        "OK",
        ButtonStyle::Secondary,
    );
    let monitor = at(214, row, 157, 45);
    l.panel(monitor, p.action, RADIUS, None);
    l.control(
        monitor,
        Control::Dialog(DialogButton::Primary),
        true,
        "CPU MONITOR",
        Look::Area,
    );
    l.text(monitor, "CPU MONITOR", BODY_BOLD, p.text, Align::Center);
}

/// "Corrupt Neural Capture" (`0x2b0cf8`, text `0x54663c`): an 800 × 480 veil (0.7), a
/// `#121212` panel 419 × 416 at (190, 32), the title (Medium 20 px) in a 419 × 82 header,
/// the body (Medium 16 px) over 225 px from y 82, CANCEL and SEND REPORT (`#1A3AF8`)
/// 157 × 45 at (48, 339) and (213, 339). SEND REPORT is enabled only online (`0x2b13b8`);
/// Tetrad has no report service, so it stays disabled. Owned: the stock rich text
/// (`<b>%1</b>`) as three paragraphs, the names in Medium.
fn corrupt_captures(l: &mut Layout, names: &[String]) {
    let p = l.palette;
    let (color, alpha) = crate::layout::LOADING_VEIL;
    let color = p.tone(color);
    l.push(FULL, "", Look::Veil { color, alpha });
    let panel = Rect::new(190, 32, 419, 416);
    l.panel(panel, p.background, RADIUS, None);
    let at = |x, y, width, height| Rect::new(panel.x + x, panel.y + y, width, height);
    l.text(
        at(0, 0, 419, 82),
        "Corrupt Neural Capture",
        LARGE_BOLD,
        p.text,
        Align::Center,
    );
    let intro =
        "CorOS has encountered an issue with Neural Capture and removed the following Capture(s):";
    let outro = "Restore a backup, or re-download where applicable, to recover these Captures.";
    let list = names.join(", ");
    let width = 371;
    let height = |text: &str, font: crate::render::Font| {
        wrap(text, font, width as f32).len() as u32 * font.size.line_px()
    };
    let mut y = 82;
    for (text, font) in [(intro, BODY), (list.as_str(), BODY_BOLD), (outro, BODY)] {
        let h = height(text, font).min(225 - (y - 82).min(225) as u32);
        l.paragraph(at(24, y, width, h), text, font, p.text);
        y += h as i32 + 16;
    }
    l.button(
        at(48, 339, 157, 45),
        Control::Dialog(DialogButton::Cancel),
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    let report = at(213, 339, 157, 45);
    l.panel(report, p.action, RADIUS, None);
    l.control(
        report,
        Control::Dialog(DialogButton::Primary),
        false,
        "SEND REPORT",
        Look::Area,
    );
    l.text(report, "SEND REPORT", BODY_BOLD, p.faint, Align::Center);
}

/// DSPErrorDialog (ctor `0x6c6248`), above every other layer: an 800 × 480 `#323232` veil
/// (0.7), a `#FE5948` card 568 × 275 at (116, 103) with radius 10, black text: the title
/// (IBM Plex Sans Medium 20 px, `<b>`) at y 27 over the card width, the body (Regular 18 px)
/// in 488 × 162 at (40, 43), both centred (`0x84`). CANCEL and REBOOT: black, radius 4,
/// Medium 16 px, 158 × 45 at (59, 197) and (351, 197). I: white button text (the Button
/// default; the ctor sets none). `%1` is the product name (`0x498ce4`).
fn dsp_error(ui: &UiState, l: &mut Layout) {
    /// Stock DSPErrorDialog card.
    const ERROR_CARD: Rgb = Rgb(0xfe, 0x59, 0x48);
    const TITLE: crate::render::Font =
        crate::render::Font::new(tetrad_assets::TextSize::Px20, tetrad_assets::Weight::Bold);
    let p = l.palette;
    let (veil, alpha) = crate::layout::LOADING_VEIL;
    l.veil((p.tone(veil), alpha));
    let card = Rect::new(116, 103, 568, 275);
    l.panel(card, p.tone(ERROR_CARD), 10.0, None);
    let at = |x, y, width, height| Rect::new(card.x + x, card.y + y, width, height);
    l.text(
        at(0, 27, 568, TITLE.size.line_px()),
        "Something went wrong",
        TITLE,
        p.on_accent,
        Align::Center,
    );
    let body = format!(
        "Your {} experienced an unexpected problem. If you continue to experience this, please \
         send a crash report to help us diagnose the issue.",
        super::product_name(ui.snapshot.mini)
    );
    let height = (wrap(&body, MEDIUM, 488.0).len() as u32 * MEDIUM.size.line_px()).min(162);
    let paragraph = Look::Paragraph {
        font: MEDIUM,
        color: p.on_accent,
        align: Align::Center,
    };
    l.push(
        at(40, 43 + (162 - height as i32) / 2, 488, height),
        body,
        paragraph,
    );
    for (x, reboot, label) in [(59, false, "CANCEL"), (351, true, "REBOOT")] {
        let button = at(x, 197, 158, 45);
        l.panel(button, p.canvas, 4.0, None);
        l.control(
            button,
            Control::DspError { reboot },
            true,
            label,
            Look::Area,
        );
        l.text(button, label, BODY_BOLD, p.text, Align::Center);
    }
}

/// "Excessive %1 in Downloads folder" (`0x22bd50`): the text, a "Don't show again" box
/// (`icons/24/checkbox_unchecked.png`, owned box) and CANCEL / SHOW DOWNLOADS.
fn excessive_downloads(l: &mut Layout, kind: crate::ExcessDownloads, dont_show: bool) {
    let p = l.palette;
    let name = kind.label();
    let body = format!(
        "Your {name} Downloads folder has a large number of saved {name}, which can result in \
         a sluggish UI. To improve performance, please remove some files from the Downloads \
         folder."
    );
    let panel = message_panel(l, &format!("Excessive {name} in Downloads folder"), &body);
    let row = Rect::new(panel.x + 24, panel.bottom() - 68, 200, 48);
    let box_ = Rect::new(row.x, row.y + 12, 24, 24);
    l.panel(box_, p.surface, 4.0, Some(p.muted));
    if dont_show {
        l.push(
            box_,
            "",
            Look::Glyph {
                icon: Icon::Check,
                size: IconSize::Small,
                color: p.text,
            },
        );
    }
    l.control(
        row,
        Control::Dialog(DialogButton::Check),
        true,
        "Don't show again",
        Look::Area,
    );
    l.text(
        Rect::new(row.x + 32, row.y, row.width - 32, row.height),
        "Don't show again",
        BODY,
        p.text,
        Align::Left,
    );
    l.button(
        Rect::new(panel.right() - 344, panel.bottom() - 68, 120, 48),
        Control::Dialog(DialogButton::Cancel),
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(panel.right() - 212, panel.bottom() - 68, 188, 48),
        Control::Dialog(DialogButton::Primary),
        true,
        "SHOW DOWNLOADS",
        ButtonStyle::Primary,
    );
}

/// "Assign footswitch" (`0x3b5424`): a footswitch press assigns; UNASSIGN shows only for an
/// assigned block. The Hybrid text names the stomp row (`0xa34b64`).
fn assign_stomp(ui: &UiState, l: &mut Layout, position: GridPosition) {
    let s = &ui.snapshot;
    let letter = |tile: usize| char::from(b'A' + tile as u8);
    let stomps: Vec<usize> = (0..FOOTSWITCH_COUNT)
        .filter(|tile| ui.tile_mode(*tile) == FootswitchMode::Stomp)
        .collect();
    let body = match (stomps.first(), stomps.last()) {
        (Some(first), Some(last)) if stomps.len() < FOOTSWITCH_COUNT => format!(
            "Press the target footswitch ({}-{}) to assign. Target footswitches are restricted \
             by Hybrid Mode settings.",
            letter(*first),
            letter(*last)
        ),
        _ => "Press the target footswitch to assign".to_owned(),
    };
    let panel = message_panel(l, "Assign footswitch", &body);
    l.button(
        Rect::new(panel.x + 24, panel.bottom() - 68, 200, 48),
        Control::Dialog(DialogButton::Cancel),
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    if s.selected_block == Some(position) && s.selected_stomp.is_some() {
        l.button(
            Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
            Control::Dialog(DialogButton::Primary),
            true,
            "UNASSIGN",
            ButtonStyle::Danger,
        );
    }
}

/// Gig View EDIT STOMP (manual §3: "Customizes the selected Stomp name and device block
/// assignments"; `0x2c56f8`). Owned layout: the name, Latching / Momentary (`0xa34c04`,
/// `0xa34c10`) and one chip per Grid block, lit when assigned to this stomp.
fn edit_stomp(ui: &UiState, l: &mut Layout, tile: u8) {
    const CHIP: (u32, u32) = (124, 40);
    let p = l.palette;
    let s = &ui.snapshot;
    let stomp = &s.stomps[usize::from(tile)];
    let blocks: Vec<GridPosition> = GridPosition::all()
        .filter(|position| s.occupied(*position))
        .collect();
    let rows = blocks.len().div_ceil(4).clamp(1, 5) as u32;
    let height = 236 + rows * (CHIP.1 + 6);
    let panel = Rect::new(120, (HEIGHT as i32 - height as i32) / 2, 560, height);
    dialog_panel(l, panel, &format!("Stomp {}", char::from(b'A' + tile)));
    l.button(
        Rect::new(panel.x + 24, panel.y + 56, 248, 44),
        Control::Stomp(StompEdit::Rename(tile)),
        true,
        if stomp.label.is_empty() {
            "-"
        } else {
            stomp.label.as_str()
        },
        ButtonStyle::Secondary,
    );
    for (index, (label, momentary)) in [("Latching", false), ("Momentary", true)]
        .into_iter()
        .enumerate()
    {
        l.button(
            Rect::new(panel.x + 284 + index as i32 * 128, panel.y + 56, 124, 44),
            Control::Stomp(StompEdit::Momentary { tile, momentary }),
            true,
            label,
            ButtonStyle::Tab {
                selected: stomp.momentary == momentary,
            },
        );
    }
    let top = panel.y + 116;
    if blocks.is_empty() {
        l.text(
            Rect::new(panel.x + 24, top, panel.width - 48, CHIP.1),
            "No device blocks on the Grid",
            BODY,
            p.muted,
            Align::Left,
        );
    }
    for (index, position) in blocks.into_iter().take(20).enumerate() {
        let chip = Rect::new(
            panel.x + 24 + (index % 4) as i32 * (CHIP.0 as i32 + 4),
            top + (index / 4) as i32 * (CHIP.1 as i32 + 6),
            CHIP.0,
            CHIP.1,
        );
        l.button(
            chip,
            Control::Stomp(StompEdit::Block { tile, position }),
            true,
            s.cell(position).label.clone(),
            ButtonStyle::Tab {
                selected: stomp.blocks.contains(&position),
            },
        );
    }
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 68, 200, 48),
        Control::Back,
        true,
        "DONE",
        ButtonStyle::Primary,
    );
}

fn choice(ui: &UiState, l: &mut Layout, purpose: ChoicePurpose, anchor: Option<Rect>) {
    let s = &ui.snapshot;
    let (title, options, current): (&str, Vec<&str>, Option<usize>) = match purpose {
        ChoicePurpose::FilterType(parameter) => {
            super::eq_editor::filter_type_selector(ui, l, parameter);
            return;
        }
        ChoicePurpose::Setting(id) => match id.kind() {
            SettingKind::Choice(options) => (
                id.label(),
                options.to_vec(),
                match s.setting(id) {
                    Some(SettingValue::Choice(index)) => Some(usize::from(*index)),
                    _ => None,
                },
            ),
            _ => (id.label(), Vec::new(), None),
        },
        ChoicePurpose::RowInput(row) => (
            "CHANGE INPUT",
            InputPort::ALL.iter().map(|port| port.label()).collect(),
            InputPort::ALL
                .iter()
                .position(|port| *port == s.rows[usize::from(row.get())].input),
        ),
        ChoicePurpose::RowOutput(row) => (
            "CHANGE OUTPUT",
            OutputPort::ALL.iter().map(|port| port.label()).collect(),
            OutputPort::ALL
                .iter()
                .position(|port| *port == s.rows[usize::from(row.get())].output),
        ),
        // Owned dialog (review R25); stock opens the Directory to pick a setlist and slot
        // (`0x7900c4`). tetrad-ui has no Directory save-destination mode yet: kept.
        ChoicePurpose::SaveAsSetlist => (
            "Save to Setlist",
            s.setlists
                .iter()
                .map(|setlist| setlist.title.as_str())
                .collect(),
            s.active_setlist
                .and_then(|id| s.setlists.iter().position(|setlist| setlist.id == id)),
        ),
        ChoicePurpose::Extension(setting) => match super::extensions::choice(ui, setting) {
            Some((label, options, selected)) => (label, options, Some(selected)),
            None => return,
        },
        ChoicePurpose::Mapping(choice) => match super::mappings::choice(ui, choice) {
            Some((options, selected)) => ("", options, selected),
            None => return,
        },
        ChoicePurpose::SettingsGroup => {
            if let Screen::Settings(page) = ui.screen() {
                super::settings::group_menu(ui, l, page.group());
            }
            return;
        }
    };
    if purpose != ChoicePurpose::SaveAsSetlist {
        let anchor = anchor.unwrap_or_else(|| pointer_anchor(ui));
        crate::widgets::dropdown_list(l, anchor, &options, current, ui.offset(ListId::Choice));
        return;
    }
    let panel = Rect::new(200, 40, 400, HEIGHT - 80);
    dialog_panel(l, panel, title);
    let viewport = Rect::new(
        panel.x + 12,
        panel.y + 56,
        panel.width - 24,
        panel.height - 56 - 64,
    );
    let row = super::CHOICE_ROW;
    l.scroll(
        ListId::Choice,
        viewport,
        options.len() as u32 * row,
        ui.offset(ListId::Choice),
        |l, y| {
            for (index, option) in options.iter().enumerate() {
                l.control(
                    Rect::new(
                        viewport.x,
                        y + index as i32 * row as i32,
                        viewport.width,
                        row - 4,
                    ),
                    Control::Choose(index as u16),
                    true,
                    *option,
                    Look::Row {
                        hue: None,
                        icon: (current == Some(index)).then_some(Icon::Check),
                        value: String::new(),
                        chevron: false,
                        selected: current == Some(index),
                    },
                );
            }
        },
    );
    l.button(
        Rect::new(panel.x + 12, panel.bottom() - 56, panel.width - 24, 44),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
}

fn info(ui: &UiState, l: &mut Layout, title: &str, body: &str) {
    let p = l.palette;
    let panel = Rect::new(100, 40, 600, HEIGHT - 80);
    dialog_panel(l, panel, title);
    let viewport = Rect::new(
        panel.x + 24,
        panel.y + 60,
        panel.width - 48,
        panel.height - 60 - 72,
    );
    let lines = wrap(body, BODY, viewport.width as f32).len() as u32;
    let content = lines * BODY.size.line_px();
    l.scroll(
        ListId::Changelog,
        viewport,
        content,
        ui.offset(ListId::Changelog),
        |l, y| {
            l.paragraph(
                Rect::new(viewport.x, y, viewport.width, content.max(1)),
                body,
                BODY,
                p.text,
            );
        },
    );
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 64, 200, 48),
        Control::Back,
        true,
        "GOT IT",
        ButtonStyle::Primary,
    );
}

/// Copy / Swap Scene (`0x5ad9d4`): the stock MessageDialog `0x2b0a3c` with title "%1 Scene
/// %2" (`0x2b071c`), the destination text (mini: "page & footswitch") and CANCEL. On the mini
/// the letter is the footswitch (`0x1d361c`: scene & 3) and " (Page %1)" is roman
/// (`0x1d37fc`: scene >> 2 → "I", "II"), as "Scene B (Page II)" (C, review r15 R11). The
/// scene buttons are owned (review R25).
fn scene_target(ui: &UiState, l: &mut Layout, swap: bool) {
    let p = l.palette;
    let current = ui.scene_source.unwrap_or(ui.snapshot.selected_scene);
    let mini = ui.snapshot.mini;
    let verb = if swap { "Swap" } else { "Copy" };
    let title = if mini {
        let letter = char::from(b'A' + current.get() % 4);
        let page = if current.get() < 4 { "I" } else { "II" };
        format!("{verb} Scene {letter} (Page {page})")
    } else {
        format!("{verb} Scene {}", current.letter())
    };
    let panel = Rect::new(160, 100, 480, 290);
    dialog_panel(l, panel, &title);
    l.text(
        Rect::new(panel.x + 24, panel.y + 56, panel.width - 48, 24),
        if mini {
            "Press Scene destination page & footswitch."
        } else {
            "Press Scene destination footswitch."
        },
        MEDIUM,
        p.muted,
        Align::Left,
    );
    for scene in SceneIndex::all() {
        let index = i32::from(scene.get());
        l.button(
            Rect::new(
                panel.x + 24 + (index % 4) * 110,
                panel.y + 96 + (index / 4) * 56,
                100,
                48,
            ),
            Control::SceneTarget(scene),
            scene != current,
            scene.letter().to_string(),
            ButtonStyle::Secondary,
        );
    }
    l.button(
        Rect::new(panel.right() - 224, panel.bottom() - 64, 200, 48),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
}

/// Owned "ROWS 1/2" split/mix dialog (review R25). Stock adds a split or mix by holding an
/// empty slot and drags the S / M tokens; tetrad-ui has no token drag yet, so this stays the
/// only routing path.
fn routing(ui: &UiState, l: &mut Layout, pair: RowPair) {
    let p = l.palette;
    let view = ui.snapshot.pairs[pair.index()];
    let title = match pair {
        RowPair::Upper => "ROWS 1/2",
        RowPair::Lower => "ROWS 3/4",
    };
    let panel = Rect::new(140, 110, 520, 260);
    dialog_panel(l, panel, title);
    // A split sits before its column's slot, a mix after it (grid wiring).
    let split = |value: Option<u8>| match value {
        None => "NONE".to_owned(),
        Some(0) => "AT START".to_owned(),
        Some(column) => format!("AFTER SLOT {column}"),
    };
    let mix = |value: Option<u8>| {
        value.map_or("NONE".to_owned(), |column| {
            format!("AFTER SLOT {}", column + 1)
        })
    };
    for (index, (label, value, text, down, up, off)) in [
        (
            "Split",
            view.split,
            split(view.split),
            RoutingControl::Split(-1),
            RoutingControl::Split(1),
            RoutingControl::SplitOff,
        ),
        (
            "Mix",
            view.mix,
            mix(view.mix),
            RoutingControl::Mix(-1),
            RoutingControl::Mix(1),
            RoutingControl::MixOff,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let y = panel.y + 64 + index as i32 * 60;
        l.text(
            Rect::new(panel.x + 24, y, 80, 48),
            label,
            crate::layout::MEDIUM_BOLD,
            p.text,
            Align::Left,
        );
        super::stepper(
            l,
            Rect::new(panel.x + 96, y, 270, 48),
            Control::Routing(down),
            Control::Routing(up),
            text,
            true,
        );
        l.button(
            Rect::new(panel.x + 380, y, 116, 48),
            Control::Routing(off),
            value.is_some(),
            "REMOVE",
            ButtonStyle::Danger,
        );
    }
    l.button(
        Rect::new(panel.right() - 184, panel.bottom() - 64, 160, 48),
        Control::Back,
        true,
        "DONE",
        ButtonStyle::Primary,
    );
}

/// Capture quit confirm (`0x003cd75c`): "Capture in progress" (`0xa36b1c`) while the
/// device records or trains, else "Unsaved capture" (`0xa36b58`); stock buttons via
/// `0x00547fd0` are OK plus a cancel. Which wizard steps count as "in progress" is
/// inferred. Drawn as a black card anchored at the abort control.
fn abort_confirm(ui: &UiState, l: &mut Layout, anchor: Option<Rect>) {
    const SIZE: (u32, u32) = (336, 172);
    let p = l.palette;
    let running = matches!(
        ui.snapshot.capture.step,
        CaptureStep::RecordingSignals
            | CaptureStep::SanityCheck
            | CaptureStep::Training
            | CaptureStep::Refine
    );
    let (title, body) = if running {
        (
            "Capture in progress",
            "A capture is in progress, are you sure you want to quit?",
        )
    } else {
        (
            "Unsaved capture",
            "A capture has not been saved, are you sure you want to quit?",
        )
    };
    let card = match anchor {
        Some(anchor) => {
            let x = (anchor.right() - SIZE.0 as i32).clamp(8, (WIDTH - SIZE.0) as i32 - 8);
            let below = anchor.bottom() + 4;
            let y = if below + SIZE.1 as i32 <= HEIGHT as i32 - 8 {
                below
            } else {
                (anchor.y - 4 - SIZE.1 as i32).max(8)
            };
            Rect::new(x, y, SIZE.0, SIZE.1)
        }
        None => Rect::new(
            (WIDTH - SIZE.0) as i32 / 2,
            (HEIGHT - SIZE.1) as i32 / 2,
            SIZE.0,
            SIZE.1,
        ),
    };
    l.popover(card);
    l.panel(card, p.canvas, RADIUS, None);
    l.text(
        Rect::new(card.x + 16, card.y + 12, card.width - 32, 26),
        title,
        MEDIUM_BOLD,
        p.text,
        Align::Left,
    );
    l.paragraph(
        Rect::new(card.x + 16, card.y + 44, card.width - 32, 48),
        body,
        BODY,
        p.muted,
    );
    let row = card.y + card.height as i32 - 60;
    l.button(
        Rect::new(card.x + 16, row, 144, 44),
        Control::Dialog(DialogButton::Cancel),
        true,
        "CANCEL",
        ButtonStyle::Inset,
    );
    l.button(
        Rect::new(card.right() - 160, row, 144, 44),
        Control::Dialog(DialogButton::Primary),
        true,
        "OK",
        ButtonStyle::Primary,
    );
}

/// Modes configuration geometry, decoded from `0x002cc3cc` (the CYCLE track) and
/// manual shot 021. Slot size and row are measured (±4 px).
const CYCLE_TRACK: Rect = Rect::new(39, 158, 720, 76);
const SLOT: (u32, u32) = (170, 80);
const SLOT_Y: i32 = 194;
const SLOT_GAP: u32 = 24;
/// The add button (owned size; stock shows "Tap the add button…", `0xa256a8`).
const ADD_WIDTH: u32 = 80;
/// Stock footswitch slots in the cycle (tetrad-settings `ModeSettings::MAX_SLOTS`).
const MAX_SLOTS: usize = 3;
/// Device map row colours (shot 021): first hybrid row yellow, second violet. Data: they
/// stay fixed under themes.
const ROW_UPPER: Rgb = block::PITCH;
const ROW_LOWER: Rgb = block::CAB;
const SINGLE_MODES: [FootswitchMode; 3] = [
    FootswitchMode::Preset,
    FootswitchMode::Scene,
    FootswitchMode::Stomp,
];

/// Remove target while a slot is dragged: "Drag Modes to the top-right corner to remove
/// them" (manual §3). The box is owned.
fn remove_zone() -> Rect {
    Rect::new(560, 4, 236, 136)
}

/// The mode cycle as Modes configuration edits it: `allowed_modes` in order; the one
/// `Hybrid` entry uses `hybrid`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cycle {
    modes: Vec<FootswitchMode>,
    hybrid: HybridRows,
}

impl Cycle {
    fn of(ui: &UiState) -> Self {
        Self {
            modes: ui.snapshot.allowed_modes.clone(),
            hybrid: ui.snapshot.hybrid,
        }
    }

    fn slots(&self) -> usize {
        self.modes
            .iter()
            .map(|mode| {
                if *mode == FootswitchMode::Hybrid {
                    2
                } else {
                    1
                }
            })
            .sum()
    }

    fn uses(&self, mode: FootswitchMode) -> bool {
        self.modes.contains(&mode)
            || (self.modes.contains(&FootswitchMode::Hybrid)
                && (self.hybrid.upper == mode || self.hybrid.lower == mode))
    }

    /// Modes the add button offers.
    fn missing(&self) -> Vec<FootswitchMode> {
        if self.slots() >= MAX_SLOTS {
            return Vec::new();
        }
        SINGLE_MODES
            .into_iter()
            .filter(|mode| !self.uses(*mode))
            .collect()
    }

    /// Drop slot `from` on slot `onto`: two single modes become one hybrid slot at
    /// `onto`; `onto` takes the first row (inferred).
    fn merge(&mut self, from: usize, onto: usize) -> bool {
        let (Some(&first), Some(&second)) = (self.modes.get(onto), self.modes.get(from)) else {
            return false;
        };
        if from == onto
            || first == FootswitchMode::Hybrid
            || second == FootswitchMode::Hybrid
            || self.modes.contains(&FootswitchMode::Hybrid)
        {
            return false;
        }
        self.modes[onto] = FootswitchMode::Hybrid;
        self.hybrid = HybridRows {
            upper: first,
            lower: second,
        };
        self.modes.remove(from);
        true
    }

    /// Long press on a hybrid slot: its two modes become single slots in place.
    fn split(&mut self, slot: usize) -> bool {
        if self.modes.get(slot) != Some(&FootswitchMode::Hybrid) {
            return false;
        }
        self.modes.remove(slot);
        let rows = [self.hybrid.upper, self.hybrid.lower];
        for mode in rows.into_iter().rev() {
            if !self.modes.contains(&mode) {
                self.modes.insert(slot, mode);
            }
        }
        true
    }

    fn command(self) -> Vec<UiCommand> {
        vec![UiCommand::SetModeCycle {
            modes: self.modes,
            hybrid: self.hybrid,
        }]
    }
}

/// Slot boxes of the cycle row, then the add button when a mode is missing; the row is
/// centred on the screen (stock: two slots at x 218 and 412).
fn cycle_row(cycle: &Cycle) -> (Vec<Rect>, Option<Rect>) {
    let add = !cycle.missing().is_empty();
    let count = cycle.modes.len() as u32;
    let width = count * SLOT.0 + u32::from(add) * ADD_WIDTH;
    let gaps = (count + u32::from(add)).saturating_sub(1) * SLOT_GAP;
    let mut x = (WIDTH as i32 - (width + gaps) as i32) / 2;
    let slots = (0..count)
        .map(|_| {
            let rect = Rect::new(x, SLOT_Y, SLOT.0, SLOT.1);
            x += (SLOT.0 + SLOT_GAP) as i32;
            rect
        })
        .collect();
    (slots, add.then(|| Rect::new(x, SLOT_Y, ADD_WIDTH, SLOT.1)))
}

/// Modes configuration (stock `0xa25e90`, shot 021): help text, the CYCLE loop with one
/// card per slot, and a device map coloured by the hybrid rows. Gestures are in
/// [`UiState::drop_mode`] and [`UiState::modes_config`].
/// Modes configuration ⓘ text (`0xa25f64`, a "MIDI" dialog with OK, `0x2d4f00`).
const MODES_INFO: &str = "Mode Slot 1: CC#47 value 0\nMode Slot 2: CC#47 value 1\nMode Slot 3: \
     CC#47 value 2\n\nThe default order for the slots is Preset Mode,\nScene Mode, Stomp \
     Mode.\n\nIf a slot is empty, MIDI will not recall any mode.";

fn modes(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let cycle = Cycle::of(ui);
    l.push(FULL, "", Look::Fill(p.background));
    crate::widgets::page_header(l, "Modes configuration", Control::Back, Icon::Check);
    // Stock 021: a grey ⓘ disc on a `#1e1e1e` button.
    let info = Rect::new(610, 8, 66, 44);
    l.panel(info, p.surface, 8.0, None);
    l.control(
        info,
        Control::Mode(ModeControl::Info),
        true,
        "INFO",
        Look::Area,
    );
    let disc = Rect::new(info.x + 22, info.y + 11, 22, 22);
    l.push(disc, "", Look::Dot { color: p.muted });
    l.text(disc, "i", SMALL_BOLD, p.background, Align::Center);
    l.push(
        Rect::new(160, 82, 480, 48),
        "Drag a Mode on top of another to create a Hybrid\nMode. Use a long press to break a Hybrid Mode apart.",
        Look::Paragraph {
            font: BODY,
            color: p.text,
            align: Align::Center,
        },
    );
    // 2 px #2e2e2e loop, radius 38, with a gap for its "CYCLE" label.
    l.panel(CYCLE_TRACK, p.panel, 38.0, None);
    l.panel(CYCLE_TRACK.inset(2, 2), p.background, 36.0, None);
    let label = Rect::new(366, 149, 66, 18);
    l.push(label, "", Look::Fill(p.background));
    l.text(label, "CYCLE", CAPTION, p.section, Align::Center);
    triangle(l, CYCLE_TRACK.x, 190, true);
    triangle(l, CYCLE_TRACK.right(), 190, false);

    let dragging = match ui.gesture {
        Gesture::DragMode { from } => Some(usize::from(from)),
        _ => None,
    };
    let (slots, add) = cycle_row(&cycle);
    for (index, (mode, rect)) in cycle.modes.iter().zip(&slots).enumerate() {
        if dragging == Some(index) {
            l.panel(*rect, p.surface, RADIUS, Some(p.panel));
            continue;
        }
        mode_slot(l, &cycle, *mode, *rect, Some(index as u8), None);
    }
    if let Some(add) = add {
        l.panel(add, p.panel, RADIUS, None);
        l.control(
            add,
            Control::Mode(ModeControl::Add),
            dragging.is_none(),
            "ADD MODE",
            Look::Button {
                style: ButtonStyle::Ghost,
                icon: Some(Icon::Plus),
                font: None,
            },
        );
    }
    device_map(l, &cycle);
    if let Some(from) = dragging
        && let Some(mode) = cycle.modes.get(from)
    {
        let zone = remove_zone();
        l.push(
            zone,
            "REMOVE",
            Look::DropZone {
                active: zone.contains(ui.pointer.0, ui.pointer.1),
            },
        );
        let ghost = Rect::new(
            ui.pointer.0 - SLOT.0 as i32 / 2,
            ui.pointer.1 - SLOT.1 as i32 / 2,
            SLOT.0,
            SLOT.1,
        );
        mode_slot(l, &cycle, *mode, ghost, None, Some(p.accent));
    }
}

/// One cycle card: icon + name, or the two rows of the hybrid with their row dots and
/// the swap target at the right edge.
fn mode_slot(
    l: &mut Layout,
    cycle: &Cycle,
    mode: FootswitchMode,
    rect: Rect,
    slot: Option<u8>,
    border: Option<Rgb>,
) {
    let p = l.palette;
    l.panel(rect, p.panel, RADIUS, border);
    if let Some(slot) = slot {
        l.control(
            rect,
            Control::Mode(ModeControl::Slot(slot)),
            true,
            mode_name(mode),
            Look::Area,
        );
    }
    let row = |l: &mut Layout, mode: FootswitchMode, y: i32| {
        mode_icon(l, mode, rect.x + 16, y + 3, p.text);
        l.text(
            Rect::new(rect.x + 56, y, 80, 30),
            mode_name(mode),
            MEDIUM,
            p.text,
            Align::Left,
        );
    };
    if mode != FootswitchMode::Hybrid {
        row(l, mode, rect.y + 25);
        return;
    }
    row(l, cycle.hybrid.upper, rect.y + 6);
    row(l, cycle.hybrid.lower, rect.y + 44);
    l.push(
        Rect::new(rect.x + 128, rect.y + 16, 12, 12),
        "",
        Look::Dot { color: ROW_UPPER },
    );
    l.push(
        Rect::new(rect.x + 141, rect.y + 52, 12, 12),
        "",
        Look::Dot { color: ROW_LOWER },
    );
    // ↓↑ swap mark between the dots.
    let (down, up) = ((rect.x + 133) as f32, (rect.x + 147) as f32);
    let (top, bottom) = ((rect.y + 32) as f32, (rect.y + 47) as f32);
    for (x, from, to) in [(down, top, bottom), (up, bottom, top)] {
        l.line((x, from), (x, to), 1.5, p.faint);
        let head = if to > from { -4.0 } else { 4.0 };
        l.line((x - 3.0, to + head), (x, to), 1.5, p.faint);
        l.line((x + 3.0, to + head), (x, to), 1.5, p.faint);
    }
    if slot.is_some() {
        l.control(
            Rect::new(rect.right() - 48, rect.y, 48, rect.height),
            Control::Mode(ModeControl::SwapRows),
            true,
            "SWAP ROWS",
            Look::Area,
        );
    }
}

/// Filled cycle arrowhead, 18 px wide and 14 px high, centred on `x` (`0x002cc3cc`):
/// down on the left end, up on the right end.
fn triangle(l: &mut Layout, x: i32, top: i32, down: bool) {
    let p = l.palette;
    for row in 0..14 {
        let half = 9 - (if down { row } else { 13 - row }) * 9 / 14;
        l.push(
            Rect::new(x - half, top + row, (2 * half) as u32, 1),
            "",
            Look::Fill(p.panel),
        );
    }
}

/// Outline of the device (shot 021): screen, volume knob, BANK/TEMPO switches and the
/// A–H footswitches, coloured by the hybrid rows.
fn device_map(l: &mut Layout, cycle: &Cycle) {
    const LINE: Rgb = Rgb(0x52, 0x52, 0x52);
    let p = l.palette;
    let line = p.tone(LINE);
    let ring = |l: &mut Layout, x: i32, y: i32, r: u32, fill: Option<Rgb>| {
        let outer = Rect::new(x - r as i32, y - r as i32, 2 * r, 2 * r);
        match fill {
            Some(color) => l.push(outer, "", Look::Dot { color }),
            None => {
                l.push(outer, "", Look::Dot { color: line });
                l.push(
                    outer.inset(1, 1),
                    "",
                    Look::Dot {
                        color: p.background,
                    },
                );
            }
        }
    };
    l.panel(
        Rect::new(282, 308, 236, 150),
        p.background,
        12.0,
        Some(line),
    );
    l.panel(Rect::new(344, 316, 112, 62), p.background, 3.0, Some(line));
    ring(l, 314, 347, 11, None);
    for y in [347, 403, 437] {
        ring(l, 487, y, 6, None);
    }
    let hybrid = cycle.modes.contains(&FootswitchMode::Hybrid);
    for (y, color) in [(403, ROW_UPPER), (437, ROW_LOWER)] {
        for x in [314, 357, 400, 443] {
            ring(l, x, y, 6, hybrid.then_some(color));
        }
    }
}

/// The add button's menu: the modes not in the cycle, anchored at the button.
fn modes_add(ui: &UiState, l: &mut Layout) {
    let missing = Cycle::of(ui).missing();
    let items: Vec<_> = missing
        .iter()
        .map(|mode| {
            (
                Control::Mode(ModeControl::Insert(*mode)),
                mode_name(*mode),
                true,
            )
        })
        .collect();
    let anchor = l
        .anchor(Control::Mode(ModeControl::Add))
        .unwrap_or_else(|| pointer_anchor(ui));
    crate::widgets::popover_menu(l, anchor, &items, None);
}

fn mode_name(mode: FootswitchMode) -> &'static str {
    match mode {
        FootswitchMode::Preset => "Preset",
        FootswitchMode::Scene => "Scene",
        FootswitchMode::Stomp => "Stomp",
        FootswitchMode::Hybrid => "Hybrid",
        FootswitchMode::Custom => "Custom",
    }
}

/// Mode glyph at `x`, `y` (top-left), the same icons as the Grid mode label.
fn mode_icon(l: &mut Layout, mode: FootswitchMode, x: i32, y: i32, color: Rgb) {
    let icon = match mode {
        FootswitchMode::Preset => Icon::ModePreset,
        FootswitchMode::Scene => Icon::ModeScene,
        FootswitchMode::Stomp | FootswitchMode::Hybrid => Icon::Footswitch,
        FootswitchMode::Custom => Icon::Grid,
    };
    let size = IconSize::Medium;
    l.push(
        Rect::new(x, y, size.px(), size.px()),
        "",
        Look::Glyph { icon, size, color },
    );
}

/// Power & Locking functions (`PowerButtonPopup`, `0x7d5690`): `#121212` card at
/// (42, 90, 716, 299), radius 10, over a 70 % `#323232` veil. Child boxes are the
/// decoded card-relative positions; the help box is 14 px taller so the synthetic font
/// fits its four lines. The full-size QC strings are used (`0x499a60` false).
fn power(l: &mut Layout) {
    const CARD: Rect = Rect::new(42, 90, 716, 299);
    let p = l.palette;
    let at = |x: i32, y: i32, w: u32, h: u32| Rect::new(CARD.x + x, CARD.y + y, w, h);
    l.panel(CARD, p.background, 10.0, None);
    l.text(
        at(237, 32, 242, 26),
        "Power & Locking functions",
        MEDIUM_BOLD,
        p.text,
        Align::Center,
    );
    l.control(
        at(175, 82, 371, 38),
        Control::Power(PowerControl::Lock),
        true,
        "Lock Touchscreen and Master Volume knob",
        Look::Button {
            style: ButtonStyle::Primary,
            icon: Some(Icon::Lock),
            font: None,
        },
    );
    l.push(
        at(67, 124, 587, 84),
        "Hold the power button to unlock them.\n\nWhen entering Be Right Back, all outputs \
         will be disabled and the screen will turn off.\nA long press on the power button \
         will wake Quad Cortex and re-enable the audio outputs.",
        Look::Paragraph {
            font: CAPTION,
            color: p.muted,
            align: Align::Center,
        },
    );
    // `0x007d54c4` buttons: 157×48, radius 4, `#1a3af8`; CANCEL is black.
    for (x, control, label) in [
        (32, PowerControl::Cancel, "CANCEL"),
        (197, PowerControl::Shutdown, "SHUTDOWN"),
        (362, PowerControl::Reboot, "REBOOT"),
        (527, PowerControl::BeRightBack, "BE RIGHT BACK"),
    ] {
        let rect = at(x, 222, 157, 48);
        let fill = if control == PowerControl::Cancel {
            p.canvas
        } else {
            p.action
        };
        l.panel(rect, fill, 4.0, None);
        l.control(rect, Control::Power(control), true, label, Look::Area);
        l.text(rect, label, BODY_BOLD, p.text, Align::Center);
    }
}

/// Master Volume overlay (manual §3, shot 041; placement and sizes inferred): value,
/// level bar and the four assignment tiles that toggle the
/// `MasterVolume*` settings. It closes after [`volume_ms`] without a turn or a touch.
fn volume(ui: &UiState, l: &mut Layout) {
    const CARD: Rect = Rect::new(205, 32, 390, 196);
    let p = l.palette;
    l.popover(CARD);
    l.panel(CARD, p.canvas, 10.0, Some(p.section));
    let percent = ui.volume_percent.min(100);
    l.text(
        Rect::new(CARD.x, CARD.y + 4, CARD.width, 76),
        percent.to_string(),
        HUGE_BOLD,
        p.accent,
        Align::Center,
    );
    l.push(
        Rect::new(221, 118, 358, 12),
        "",
        Look::Progress {
            fraction: f32::from(percent) / 100.0,
            color: p.accent,
        },
    );
    for (x, id, label) in [
        (221, SettingId::MasterVolumeOut12, "OUT 1/2"),
        (310, SettingId::MasterVolumeOut34, "OUT 3/4"),
        (399, SettingId::MasterVolumeSend12, "SEND 1/2"),
        (499, SettingId::MasterVolumeHeadphones, "HEADPHONES"),
    ] {
        let tile = Rect::new(x, 142, 80, 64);
        let on = matches!(ui.snapshot.setting(id), Some(SettingValue::Bool(true)));
        let (fill, ink) = if on {
            (p.accent, p.on_accent)
        } else {
            (p.surface, p.muted)
        };
        l.panel(tile, fill, RADIUS, None);
        l.control(
            tile,
            Control::Setting(SettingControl::Open(id)),
            true,
            label,
            Look::Area,
        );
        let head = Rect::new(tile.x + 8, tile.y + 6, 68, 20);
        if id == SettingId::MasterVolumeHeadphones {
            let size = IconSize::Medium;
            l.push(
                Rect::new(head.x, head.y - 2, size.px(), size.px()),
                label,
                Look::Glyph {
                    icon: Icon::Headphones,
                    size,
                    color: ink,
                },
            );
        } else {
            l.text(head, label, SMALL_BOLD, ink, Align::Left);
        }
        let check = Rect::new(tile.x + 8, tile.y + 34, 22, 22);
        if on {
            l.panel(check, p.canvas, 4.0, None);
            l.push(
                check,
                "",
                Look::IconButton {
                    icon: Icon::Check,
                    fill: p.canvas,
                },
            );
        } else {
            l.panel(check, p.surface, 4.0, Some(p.faint));
        }
    }
}

/// Lock indicator at the top right while the touchscreen is locked (manual §3; box and
/// place inferred). Stock draws `icons/24/lock.png` (`0x11ff744`); this is the owned lock
/// glyph. It sits under the header (y 82–107): the Grid ⋮, ✓ buttons and the mode label
/// fill x 550–787 above it, and the first Grid row starts at y 109.
fn lock_indicator(l: &mut Layout) {
    let p = l.palette;
    l.push(
        Rect::new(766, 82, 26, 26),
        "LOCKED",
        Look::IconButton {
            icon: Icon::Lock,
            fill: p.section,
        },
    );
}

/// Master Volume overlay lifetime after the last turn or touch: 1 s on the full-size QC,
/// 2 s on the mini (`_updateSurfacePotentiometer` `0x500024`, touch restart `0x4ffec8`).
fn volume_ms(mini: bool) -> u64 {
    if mini { 2000 } else { 1000 }
}
/// Full-size unlock hold: the icon fades 0.025 per 25 ms step and unlocks below 0.01, so
/// after 40 steps (`0x7d48c4`, `0x7d4a94`).
const UNLOCK_HOLD_MS: u64 = 40 * 25;
/// The hold cancels this long after the last POWER press (`0x7d4864`,
/// `cancelUnlockTimerFired` `0x7d4bc4`); stock ignores full-size releases.
const UNLOCK_CANCEL_MS: u64 = 350;
/// Full-size POWER guard after the popup opens or a hold unlocks (`0x6a8678`, `0x6a827c`).
const POWER_GUARD_MS: u64 = 3000;

impl UiState {
    /// The touchscreen and the Master Volume knob are locked (LOCK). The application
    /// must ignore the knob while this is true.
    pub fn locked(&self) -> bool {
        self.power == PowerState::Locked
    }

    /// Be Right Back is active: outputs and the screen are off until the next POWER press.
    pub fn away(&self) -> bool {
        self.power == PowerState::Away
    }

    /// Lock and Be Right Back drop touch and semantic input; a touch on the Master
    /// Volume overlay keeps it open. Returns whether `input` is dropped.
    pub(crate) fn power_gate(&mut self, input: &InputEvent) -> bool {
        let touch = matches!(
            input,
            InputEvent::PointerDown { .. }
                | InputEvent::PointerMove { .. }
                | InputEvent::PointerUp { .. }
                | InputEvent::Activate(_)
                | InputEvent::Back
                | InputEvent::TextChanged(_)
                | InputEvent::DraftColor(_)
        );
        if self.power != PowerState::Awake {
            return touch;
        }
        if touch && self.overlay == Some(Overlay::Volume) {
            self.volume_until = self.now + volume_ms(self.snapshot.mini);
        }
        false
    }

    /// POWER slot `0x6a852c`. The full-size QC acts on a press, the mini on a release (its
    /// press starts the 3 s power-off timer in the application). Be Right Back wakes at
    /// once (`0x6a82ac`); a lock ends after one mini press or a full-size hold; otherwise,
    /// unless another popup is open, the full-size QC shows the Power & Locking overlay
    /// (again) and restarts the 3 s guard, and the mini toggles it (`0x6a862c`).
    pub(crate) fn power_button(&mut self, pressed: bool) -> Vec<UiCommand> {
        let mini = self.snapshot.mini;
        if pressed == mini || self.now < self.power_guard_until {
            return Vec::new();
        }
        match self.power {
            PowerState::Away => {
                self.power = PowerState::Awake;
                vec![UiCommand::BeRightBack { active: false }]
            }
            PowerState::Locked if mini => self.unlock(),
            PowerState::Locked => {
                self.power_held.get_or_insert(self.now);
                self.power_last = self.now;
                Vec::new()
            }
            PowerState::Awake => {
                let popup = self.keyboard.is_some()
                    || self.draft.is_some()
                    || !matches!(self.overlay, None | Some(Overlay::Power | Overlay::Volume));
                if !popup {
                    self.cancel_pointer();
                    let hide = mini && self.overlay == Some(Overlay::Power);
                    self.overlay = (!hide).then_some(Overlay::Power);
                    if !mini {
                        self.power_guard_until = self.now + POWER_GUARD_MS;
                    }
                }
                Vec::new()
            }
        }
    }

    /// `touchUnlocked`: the full-size QC also starts the POWER guard (`0x6a827c`).
    fn unlock(&mut self) -> Vec<UiCommand> {
        self.power = PowerState::Awake;
        self.power_held = None;
        if !self.snapshot.mini {
            self.power_guard_until = self.now + POWER_GUARD_MS;
        }
        vec![UiCommand::SetLocked { locked: false }]
    }

    /// The knob moved: show (or refresh) the Master Volume overlay. It does not replace
    /// a dialog, menu, keyboard or the scene draft (inferred).
    pub(crate) fn master_volume(&mut self, percent: u8) {
        self.volume_percent = percent.min(100);
        if self.power != PowerState::Awake {
            return;
        }
        let free = self.keyboard.is_none()
            && self.draft.is_none()
            && matches!(self.overlay, None | Some(Overlay::Volume));
        if free {
            self.overlay = Some(Overlay::Volume);
            self.volume_until = self.now + volume_ms(self.snapshot.mini);
        }
    }

    pub(crate) fn power_action(&mut self, control: PowerControl) -> Vec<UiCommand> {
        self.overlay = None;
        match control {
            PowerControl::Cancel => Vec::new(),
            PowerControl::Lock => {
                self.power = PowerState::Locked;
                vec![UiCommand::SetLocked { locked: true }]
            }
            PowerControl::Shutdown => vec![UiCommand::Device {
                action: DeviceAction::Shutdown,
            }],
            PowerControl::Reboot => vec![UiCommand::Device {
                action: DeviceAction::Reboot,
            }],
            PowerControl::BeRightBack => {
                let mut commands = self.close_modals();
                self.power = PowerState::Away;
                commands.push(UiCommand::BeRightBack { active: true });
                commands
            }
        }
    }

    /// Clock work of these overlays: volume timeout, the POWER unlock hold and the long
    /// press that splits a hybrid slot.
    pub(crate) fn overlay_tick(&mut self) -> Vec<UiCommand> {
        if self.overlay == Some(Overlay::Volume) && self.now >= self.volume_until {
            self.overlay = None;
        }
        if let Some(since) = self.power_held {
            let unlock_at = since + UNLOCK_HOLD_MS;
            let cancel_at = self.power_last + UNLOCK_CANCEL_MS;
            if unlock_at <= cancel_at.min(self.now) {
                return self.unlock();
            }
            if self.now >= cancel_at {
                self.power_held = None;
            }
        }
        if let Gesture::Press {
            control: Some(control @ Control::Mode(ModeControl::Slot(slot))),
            origin,
            at,
            long_fired: false,
        } = self.gesture
            && self.overlay == Some(Overlay::Modes)
            && self.now.saturating_sub(at) >= LONG_PRESS_MS
        {
            self.pressed = None;
            self.gesture = Gesture::Press {
                control: Some(control),
                origin,
                at,
                long_fired: true,
            };
            let mut cycle = Cycle::of(self);
            if cycle.split(usize::from(slot)) {
                return cycle.command();
            }
        }
        Vec::new()
    }

    /// End of a slot drag: the remove zone removes it (one slot stays), another slot
    /// merges with it, the CYCLE row moves it.
    pub(crate) fn drop_mode(&mut self, from: u8, x: i32, y: i32) -> Vec<UiCommand> {
        let from = usize::from(from);
        let mut cycle = Cycle::of(self);
        if from >= cycle.modes.len() {
            return Vec::new();
        }
        if remove_zone().contains(x, y) {
            if cycle.modes.len() < 2 {
                return Vec::new();
            }
            cycle.modes.remove(from);
            return cycle.command();
        }
        let (slots, _) = cycle_row(&cycle);
        if let Some(onto) = slots.iter().position(|slot| slot.contains(x, y)) {
            return if cycle.merge(from, onto) {
                cycle.command()
            } else {
                Vec::new()
            };
        }
        let band = Rect::new(
            0,
            CYCLE_TRACK.y - 20,
            WIDTH,
            (SLOT_Y - CYCLE_TRACK.y) as u32 + SLOT.1 + 40,
        );
        if !band.contains(x, y) {
            return Vec::new();
        }
        let to = slots
            .iter()
            .enumerate()
            .filter(|(index, slot)| *index != from && slot.center().0 < x)
            .count();
        if to == from {
            return Vec::new();
        }
        let mode = cycle.modes.remove(from);
        cycle.modes.insert(to, mode);
        cycle.command()
    }

    /// Taps on the Modes configuration: swap the hybrid rows, open the add menu, add a mode.
    pub(crate) fn modes_config(&mut self, control: ModeControl) -> Vec<UiCommand> {
        let mut cycle = Cycle::of(self);
        match control {
            ModeControl::SwapRows => {
                let rows = cycle.hybrid;
                cycle.hybrid = HybridRows {
                    upper: rows.lower,
                    lower: rows.upper,
                };
                cycle.command()
            }
            ModeControl::Add => {
                self.overlay = Some(Overlay::ModesAdd);
                Vec::new()
            }
            ModeControl::Info => {
                self.reset_scroll(ListId::Changelog);
                self.overlay = Some(Overlay::ModesInfo);
                Vec::new()
            }
            ModeControl::Insert(mode) => {
                self.overlay = Some(Overlay::Modes);
                if !cycle.missing().contains(&mode) {
                    return Vec::new();
                }
                cycle.modes.push(mode);
                cycle.command()
            }
            ModeControl::Slot(_)
            | ModeControl::Allow(_)
            | ModeControl::CycleUpper
            | ModeControl::CycleLower => Vec::new(),
        }
    }
}

const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const DIGITS: [&str; 3] = ["1234567890", "-@#$%&()'", "!^_{}~."];
const SYMBOLS: [&str; 3] = ["+=*/\\|<>[]", ":;\",?`.'-", "@#$%&()"];

fn keyboard(ui: &UiState, l: &mut Layout, keyboard: &crate::state::Keyboard) {
    let p = l.palette;
    let target = &keyboard.target;
    let (_, max) = target.limits();
    l.push(Rect::new(0, 0, WIDTH, 200), "", Look::Fill(p.background));
    if target.preset_save() {
        super::preset_metadata::header(ui, l, keyboard);
        super::preset_metadata::field(l, keyboard);
    } else {
        text_field(l, keyboard, max);
    }
    keys(l, keyboard);
}

/// The owned title, field, counter and CANCEL of the keyboards not traced yet (O).
fn text_field(l: &mut Layout, keyboard: &crate::state::Keyboard, max: usize) {
    let p = l.palette;
    let target = &keyboard.target;
    l.text(
        Rect::new(24, 18, 600, 20),
        target.title(),
        BODY_BOLD,
        p.muted,
        Align::Left,
    );
    let field = Rect::new(24, 44, 752, 60);
    l.panel(field, p.surface, 10.0, Some(p.accent));
    let shown = if target.secret() {
        "*".repeat(keyboard.text.chars().count())
    } else {
        keyboard.text.clone()
    };
    let font = XLARGE_BOLD;
    let text_area = field.inset(16, 0);
    if shown.is_empty() {
        l.text(text_area, "Enter text...", font, p.faint, Align::Left);
    } else {
        l.text(text_area, shown.clone(), font, p.text, Align::Left);
    }
    let caret = (font.width(&shown).ceil() as i32).min(text_area.width as i32 - 2);
    l.push(
        Rect::new(text_area.x + caret + 2, field.y + 14, 2, 32),
        "",
        Look::Fill(p.accent),
    );
    let left = max.saturating_sub(keyboard.text.chars().count());
    l.text(
        Rect::new(24, 112, 400, 20),
        format!("Characters left: {left}"),
        BODY_BOLD,
        p.muted,
        Align::Left,
    );
    l.button(
        Rect::new(616, 112, 160, 40),
        Control::Key(Key::Cancel),
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
}

fn keys(l: &mut Layout, keyboard: &crate::state::Keyboard) {
    let p = l.palette;
    let target = &keyboard.target;
    l.push(
        Rect::new(0, 200, WIDTH, HEIGHT - 200),
        "",
        Look::Fill(p.surface),
    );
    let rows = match keyboard.layer {
        KeyLayer::Letters => LETTERS,
        KeyLayer::Digits => DIGITS,
        KeyLayer::Symbols => SYMBOLS,
    };
    let upper = keyboard.shift != Shift::Off && keyboard.layer == KeyLayer::Letters;
    let row_y = |row: i32| 210 + row * 66;
    let key = |l: &mut Layout,
               bounds: Rect,
               key: Key,
               label: String,
               icon: Option<Icon>,
               accent: bool,
               enabled: bool| {
        l.control(
            bounds,
            Control::Key(key),
            enabled,
            label,
            Look::Key { icon, accent },
        );
    };
    for (row, chars) in rows.iter().enumerate() {
        let count = chars.chars().count() as i32;
        let side = if row == 2 { 111 + 6 } else { 0 };
        let width = 72;
        let span = count * width + (count - 1) * 6;
        let start = if row == 2 {
            13 + side
        } else {
            (WIDTH as i32 - span) / 2
        };
        for (index, ch) in chars.chars().enumerate() {
            let shown = if upper { ch.to_ascii_uppercase() } else { ch };
            key(
                l,
                Rect::new(
                    start + index as i32 * (width + 6),
                    row_y(row as i32),
                    width as u32,
                    60,
                ),
                Key::Char(ch),
                shown.to_string(),
                None,
                false,
                target.accepts(shown),
            );
        }
    }
    let third = row_y(2);
    match keyboard.layer {
        KeyLayer::Letters => key(
            l,
            Rect::new(13, third, 111, 60),
            Key::Shift,
            "Caps".into(),
            Some(Icon::Shift),
            keyboard.shift != Shift::Off,
            true,
        ),
        KeyLayer::Digits => key(
            l,
            Rect::new(13, third, 111, 60),
            Key::Layer(KeyLayer::Symbols),
            "#%~".into(),
            None,
            false,
            !target.restricted(),
        ),
        KeyLayer::Symbols => key(
            l,
            Rect::new(13, third, 111, 60),
            Key::Layer(KeyLayer::Digits),
            "123".into(),
            None,
            false,
            true,
        ),
    }
    key(
        l,
        Rect::new(676, third, 111, 60),
        Key::Backspace,
        "Backspace".into(),
        Some(Icon::Backspace),
        false,
        true,
    );
    let bottom = row_y(3);
    let (layer, label) = if keyboard.layer == KeyLayer::Letters {
        (KeyLayer::Digits, "123")
    } else {
        (KeyLayer::Letters, "ABC")
    };
    key(
        l,
        Rect::new(13, bottom, 111, 60),
        Key::Layer(layer),
        label.into(),
        None,
        false,
        true,
    );
    key(
        l,
        Rect::new(130, bottom, 501, 60),
        Key::Space,
        "Space".into(),
        None,
        false,
        target.accepts(' '),
    );
    let done = if matches!(target, TextTarget::LibrarySearch | TextTarget::ModelSearch) {
        "Search"
    } else {
        "Done"
    };
    key(
        l,
        Rect::new(637, bottom, 150, 60),
        Key::Done,
        done.into(),
        None,
        true,
        true,
    );
}
