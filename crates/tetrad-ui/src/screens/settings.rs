//! Settings (manual §10: Account, System, Device, Support groups), stock frame and pages. Wi-Fi,
//! updates, storage and backups pages live in their own files; the I/O page is `io.rs`.

use tetrad_assets::{ACCENT, BACKGROUND, CANVAS, DIVIDER, FAINT, Icon, IconSize, MUTED, PANEL};
use tetrad_assets::{Rgb, TEXT, block};

use crate::layout::{BODY, BODY_BOLD, ButtonStyle, LARGE, Layout, ListId, Look};
use crate::layout::{MEDIUM, MEDIUM_BOLD, SMALL_BOLD};
use crate::render::{Align, Font, wrap};
use crate::settings::{DeviceAction, PageRow, SettingKind, SettingsPage};
use crate::*;

const ROW: u32 = 60;
const MENU_ROW: u32 = 60;
/// Shots 034, 045, 091: group dropdown and ✓ close in the top row, the page list on the
/// left and the page panel on the right.
const GROUPS: Rect = Rect::new(8, 10, 256, 43);
const MENU: Rect = Rect::new(8, 60, 256, 411);
const PANE: Rect = Rect::new(272, 60, 520, 411);
/// Brightness bars hold 32 segments (shot 034).
const SEGMENTS: i32 = 32;

/// Owned redraws of the stock page icons (bitmaps `icons/system/settings/…`, shots 034, 044,
/// 045, 091, 117).
fn page_icon(page: SettingsPage) -> Icon {
    use SettingsPage::*;
    match page {
        MyAccount => Icon::Person,
        Backups => Icon::CloudSync,
        SendReport => Icon::Bug,
        Connection => Icon::Wifi,
        Updates => Icon::Sync,
        SwapTempoTuner => Icon::Swap,
        Brightness => Icon::Brightness,
        PowerFunctions => Icon::Bolt,
        PowerOnMode | SceneBypass => Icon::Power,
        MasterVolume => Icon::Knob,
        DeviceStorage => Icon::Storage,
        Licenses => Icon::Document,
        FactoryReset => Icon::Factory,
        GlobalBypass => Icon::Cab,
        StompModeBypass => Icon::Footswitch,
        HoldTiming => Icon::Timer,
        LatencyCompensation => Icon::Clock,
        GigViewAccess => Icon::ModeScene,
        Midi => Icon::Midi,
        AboutContact => Icon::Pulse,
        DeviceInformation => Icon::InfoFilled,
        Diagnostics => Icon::PlusBold,
    }
}

fn group_icon(group: SettingsGroup) -> Icon {
    match group {
        SettingsGroup::Account => Icon::Cloud,
        SettingsGroup::System => Icon::Gear,
        SettingsGroup::Device => Icon::Device,
        SettingsGroup::Support => Icon::Envelope,
    }
}

/// Stock group list (seen on the QC, 2026-10-16): the page dims, the header turns black with
/// ▲ and runs into a black list of 60 px rows (24 px icon at x 24, label at x 69); the current
/// group is green with a ✓, and System carries an 8 px green dot when an update is available.
pub(super) fn group_menu(ui: &UiState, l: &mut Layout, group: SettingsGroup) {
    const ROW: u32 = 60;
    let panel = Rect::new(
        GROUPS.x,
        GROUPS.y - 2,
        GROUPS.width,
        (MENU.y - GROUPS.y + 2) as u32 + SettingsGroup::ALL.len() as u32 * ROW,
    );
    l.veil(crate::layout::GREY_VEIL);
    l.popover(panel);
    l.panel(panel, CANVAS, 8.0, None);
    glyph(
        l,
        Rect::new(GROUPS.x + 8, GROUPS.y, 40, GROUPS.height),
        group_icon(group),
        TEXT,
    );
    l.text(
        Rect::new(GROUPS.x + 54, GROUPS.y, 160, GROUPS.height),
        group.title(),
        BODY,
        TEXT,
        Align::Left,
    );
    triangle(
        l,
        Rect::new(GROUPS.right() - 23, GROUPS.y + 17, 14, 8),
        false,
        MUTED,
    );
    for (index, entry) in SettingsGroup::ALL.iter().enumerate() {
        let row = Rect::new(
            panel.x,
            MENU.y + index as i32 * ROW as i32,
            panel.width,
            ROW,
        );
        let current = *entry == group;
        let color = if current { ACCENT } else { TEXT };
        l.control(
            row,
            Control::Choose(index as u16),
            true,
            entry.title(),
            Look::Area,
        );
        glyph(
            l,
            Rect::new(row.x + 8, row.y, 40, ROW),
            group_icon(*entry),
            TEXT,
        );
        l.text(
            Rect::new(row.x + 60, row.y, 150, ROW),
            entry.title(),
            BODY,
            color,
            Align::Left,
        );
        if current {
            l.push(
                Rect::new(row.right() - 24, row.y + 22, 16, 16),
                "",
                Look::Glyph {
                    icon: Icon::Check,
                    size: IconSize::Small,
                    color: ACCENT,
                },
            );
        } else if *entry == SettingsGroup::System && ui.snapshot.updater.available_version.is_some()
        {
            l.push(
                Rect::new(row.right() - 22, row.y + 26, 8, 8),
                "",
                Look::Dot { color: ACCENT },
            );
        }
    }
}

/// The group's menu pages on this device: "Power On Mode" is a mini page (spec I); the QC
/// System menu (shot 034) has no such row.
fn menu_pages(ui: &UiState, group: SettingsGroup) -> Vec<SettingsPage> {
    group
        .pages()
        .iter()
        .copied()
        .filter(|page| ui.snapshot.mini || *page != SettingsPage::PowerOnMode)
        .collect()
}

pub(super) fn layout(ui: &UiState, l: &mut Layout, page: SettingsPage) {
    let group = page.group();
    // Group dropdown (shot 034): panel fill, icon at x 24, label at x 62, ▼ at x 248.
    l.panel(GROUPS, PANEL, 8.0, None);
    l.control(
        GROUPS,
        Control::Setting(SettingControl::Groups),
        true,
        group.title(),
        Look::Area,
    );
    glyph(
        l,
        Rect::new(GROUPS.x + 8, GROUPS.y, 40, GROUPS.height),
        group_icon(group),
        TEXT,
    );
    l.text(
        Rect::new(GROUPS.x + 54, GROUPS.y, 160, GROUPS.height),
        group.title(),
        BODY,
        TEXT,
        Align::Left,
    );
    triangle(
        l,
        Rect::new(GROUPS.right() - 23, GROUPS.y + 17, 14, 8),
        true,
        TEXT,
    );
    // An available update outside System: an 8 px green dot on the button's top-right corner.
    if group != SettingsGroup::System && ui.snapshot.updater.available_version.is_some() {
        l.push(
            Rect::new(GROUPS.right() - 8, GROUPS.y - 6, 8, 8),
            "",
            Look::Dot { color: ACCENT },
        );
    }
    l.control(
        crate::widgets::CLOSE_BUTTON,
        Control::Back,
        true,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Check,
            fill: PANEL,
        },
    );
    l.panel(MENU, PANEL, 8.0, None);
    let pages = menu_pages(ui, group);
    let update = ui.snapshot.updater.available_version.is_some();
    l.scroll(
        ListId::SettingsMenu,
        MENU,
        pages.len() as u32 * MENU_ROW,
        ui.offset(ListId::SettingsMenu),
        |l, y| {
            for (index, entry) in pages.iter().enumerate() {
                let row = Rect::new(
                    MENU.x,
                    y + index as i32 * MENU_ROW as i32,
                    MENU.width,
                    MENU_ROW,
                );
                crate::widgets::list_row(
                    l,
                    row,
                    Control::Setting(SettingControl::Page(*entry)),
                    true,
                    entry.title(),
                    Some(page_icon(*entry)),
                    *entry == page,
                    None,
                );
                // Stock marks Updates with an 8 px green dot while an update is available.
                if *entry == SettingsPage::Updates && update {
                    l.push(
                        Rect::new(row.right() - 22, row.y + 26, 8, 8),
                        "",
                        Look::Dot { color: ACCENT },
                    );
                }
            }
        },
    );
    l.panel(PANE, PANEL, 8.0, None);
    let pane = PANE.inset(16, 12);
    // Updates and Backups put their own state title there; Device Information starts with
    // its section titles and Diagnostics pages carry their own.
    let own_title = match page {
        SettingsPage::Updates
        | SettingsPage::Backups
        | SettingsPage::DeviceInformation
        | SettingsPage::Connection => true,
        // Linked, stock puts "Device linked to [user]" in the title place (QC, 2026-10-16).
        SettingsPage::MyAccount => ui.snapshot.account.username.is_some(),
        SettingsPage::Diagnostics => ui.support != crate::state::SupportView::List,
        _ => false,
    };
    if !own_title {
        l.text(
            Rect::new(pane.x, pane.y, pane.width, 32),
            page.heading(),
            LARGE,
            TEXT,
            Align::Left,
        );
    }
    let body = Rect::new(pane.x, pane.y + 44, pane.width, pane.height - 44);
    match page {
        SettingsPage::MyAccount => account(ui, l, body),
        SettingsPage::Backups => super::storage::backups(ui, l, pane),
        SettingsPage::Connection => super::wifi::layout(ui, l, PANE),
        SettingsPage::Updates => super::updater::layout(ui, l, pane),
        SettingsPage::DeviceStorage => super::storage::layout(ui, l, body),
        SettingsPage::DeviceInformation => super::support::information(ui, l, PANE),
        SettingsPage::Licenses => super::support::licences(ui, l, PANE),
        SettingsPage::Diagnostics => super::support::diagnostics(ui, l, PANE),
        SettingsPage::Brightness => brightness(ui, l, body),
        SettingsPage::GlobalBypass => global_bypass(ui, l, body),
        SettingsPage::FactoryReset => super::recovery::options(l, body, false),
        SettingsPage::AboutContact => about(l, body),
        // Stock 091 rows (dividers, dropdowns) run 8 px past the text inset, to x 784.
        _ => {
            let area = Rect::new(body.x, body.y, body.width + 8, body.height);
            rows(ui, l, area, ListId::SettingsPage, page.rows());
        }
    }
}

fn row_height(row: &PageRow, width: u32) -> u32 {
    match row {
        PageRow::Header(_) => 34,
        PageRow::Text(text) => rich_height(text, MEDIUM, width) + 8,
        PageRow::Radio(_, _, text) => rich_height(text, MEDIUM, width - 48).max(24) + 24,
        PageRow::Setting(_) | PageRow::Action(..) => ROW,
    }
}

/// Generic setting rows: toggles flip on tap, choices open a list, numbers get a stepper,
/// radio options pick their index. Unknown values show a dash and stay disabled.
pub(super) fn rows(ui: &UiState, l: &mut Layout, area: Rect, list: ListId, rows: &[PageRow]) {
    let content: u32 = rows.iter().map(|row| row_height(row, area.width)).sum();
    l.scroll(list, area, content, ui.offset(list), |l, mut y| {
        for row in rows {
            let height = row_height(row, area.width);
            match *row {
                PageRow::Header(title) => {
                    l.text(
                        Rect::new(area.x, y + 10, area.width, 20),
                        title,
                        BODY_BOLD,
                        TEXT,
                        Align::Left,
                    );
                }
                PageRow::Text(text) => {
                    rich_paragraph(
                        l,
                        Rect::new(area.x, y, area.width, height),
                        text,
                        MEDIUM,
                        TEXT,
                    );
                }
                PageRow::Setting(id) => {
                    setting_row(ui, l, Rect::new(area.x, y, area.width, ROW), id);
                }
                PageRow::Radio(id, index, text) => {
                    radio(
                        ui,
                        l,
                        Rect::new(area.x, y, area.width, height),
                        id,
                        index,
                        text,
                    );
                }
                PageRow::Action(action, label) => {
                    let style = if matches!(
                        action,
                        DeviceAction::FactoryReset
                            | DeviceAction::ResetSettings
                            | DeviceAction::Shutdown
                    ) {
                        ButtonStyle::Danger
                    } else {
                        ButtonStyle::Inset
                    };
                    l.button(
                        Rect::new(area.x, y + 8, 260, 44),
                        Control::Action(action),
                        true,
                        label,
                        style,
                    );
                }
            }
            y += height as i32;
        }
    });
}

/// Stock setting row (shot 091): a 60 px flat row with a divider, an optional 22 px (i) help
/// disc at +2, the label and the control at the right end: a vertical switch (the
/// whole row toggles), a black dropdown that opens in place, or a stepper for numbers.
pub(super) fn setting_row(ui: &UiState, l: &mut Layout, bounds: Rect, id: SettingId) {
    let value = ui.snapshot.setting(id);
    let known = value.is_some();
    // The (i) target is left of the row target so both stay disjoint.
    let (label_x, row) = match id.help() {
        Some(help) => {
            let center = (bounds.x + 13, bounds.y + bounds.height as i32 / 2);
            info_button(l, center, id.label(), help);
            (bounds.x + 37, bounds.take_left(44, 0).1)
        }
        None => (bounds.x + 16, bounds),
    };
    let label = |l: &mut Layout, right: i32| {
        l.text(
            Rect::new(
                label_x,
                bounds.y,
                (right - label_x).max(0) as u32,
                bounds.height,
            ),
            id.label(),
            MEDIUM,
            if known { TEXT } else { FAINT },
            Align::Left,
        );
    };
    match id.kind() {
        SettingKind::Toggle => {
            l.control(
                row,
                Control::Setting(SettingControl::Open(id)),
                known,
                id.label(),
                Look::Area,
            );
            let on = match value {
                Some(SettingValue::Bool(on)) => Some(*on),
                _ => None,
            };
            // Stock 091: track at x 718, the On/Off labels end at 776.
            let switch = Rect::new(bounds.right() - 66, bounds.y, 66, bounds.height);
            l.push(switch, "", Look::Switch { on });
            label(l, switch.x - 8);
        }
        SettingKind::Choice(_) => {
            // Stock 091: a 137 × 37 black field at x 647..783 (the shared dropdown is
            // `#121212`). The touch target is 40 px tall.
            let field = Rect::new(
                bounds.right() - 137,
                bounds.y + (bounds.height as i32 - 37) / 2,
                137,
                37,
            );
            l.control(
                Rect::new(field.x, field.y - 1, field.width, 40),
                Control::Setting(SettingControl::Open(id)),
                known,
                id.label(),
                Look::Area,
            );
            l.panel(field, CANVAS, 4.0, None);
            let ink = if known { TEXT } else { FAINT };
            let arrow = Rect::new(field.right() - 24, field.y + 15, 12, 7);
            triangle(l, arrow, true, ink);
            l.text(
                Rect::new(
                    field.x + 8,
                    field.y,
                    (arrow.x - field.x - 14) as u32,
                    field.height,
                ),
                id.format(value),
                MEDIUM,
                ink,
                Align::Left,
            );
            label(l, field.x - 8);
        }
        SettingKind::Number { .. } => {
            let stepper = Rect::new(bounds.right() - 212, bounds.y + 8, 212, bounds.height - 16);
            super::stepper(
                l,
                stepper,
                Control::Setting(SettingControl::Step(id, -1)),
                Control::Setting(SettingControl::Step(id, 1)),
                id.format(value),
                known,
            );
            label(l, stepper.x - 8);
        }
    }
    l.push(
        Rect::new(bounds.x, bounds.bottom() - 1, bounds.width, 1),
        "",
        Look::Fill(DIVIDER),
    );
}

/// Stock (i) button (shot 091): a 22 px `#121212` disc with a grey "i", opening the help
/// popup.
pub(super) fn info_button(
    l: &mut Layout,
    center: (i32, i32),
    title: &'static str,
    body: &'static str,
) {
    let target = Rect::new(center.0 - 20, center.1 - 20, 40, 40);
    l.control(
        target,
        Control::Setting(SettingControl::Info(title, body)),
        true,
        "INFO",
        Look::Area,
    );
    let disc = Rect::new(center.0 - 11, center.1 - 11, 22, 22);
    l.push(disc, "", Look::Dot { color: BACKGROUND });
    l.text(disc, "i", SMALL_BOLD, PANEL, Align::Center);
}

/// One radio option (Scene Bypass State Behavior): ring at the left, the stock text
/// beside it; the whole row selects it.
fn radio(ui: &UiState, l: &mut Layout, bounds: Rect, id: SettingId, index: u16, text: &str) {
    let value = ui.snapshot.setting(id);
    let selected = value == Some(&SettingValue::Choice(index));
    l.control(
        bounds,
        Control::Setting(SettingControl::Set(id, i32::from(index))),
        value.is_some(),
        text.lines().next().unwrap_or_default().to_owned(),
        Look::Area,
    );
    let center = (bounds.x + 16, bounds.y + 24);
    let dot = |l: &mut Layout, radius: i32, color: Rgb| {
        let size = (radius * 2) as u32;
        l.push(
            Rect::new(center.0 - radius, center.1 - radius, size, size),
            "",
            Look::Dot { color },
        );
    };
    dot(l, 10, if selected { ACCENT } else { FAINT });
    dot(l, 7, PANEL);
    if selected {
        dot(l, 4, ACCENT);
    }
    rich_paragraph(
        l,
        Rect::new(
            bounds.x + 40,
            bounds.y + 12,
            bounds.width - 48,
            bounds.height - 12,
        ),
        text,
        MEDIUM,
        if selected { TEXT } else { MUTED },
    );
}

/// Stored brightness percent → the 0–32 the bars show (`round(% × 32 / 100)`, I: fits the
/// defaults 50 → 16 and 6 → 2).
pub(crate) fn brightness_segments(percent: i32) -> i32 {
    (percent * SEGMENTS + 50).div_euclid(100)
}

/// The stored percent for `segments` filled bars (the inverse, rounded).
pub(crate) fn brightness_percent(segments: i32) -> i32 {
    (segments.clamp(0, SEGMENTS) * 100 + SEGMENTS / 2) / SEGMENTS
}

/// Bar `index` of the Brightness page (shot 034: bars at y 227, 306, 385).
fn brightness_bar(area: Rect, index: usize) -> Rect {
    Rect::new(area.x, area.y + 113 + index as i32 * 79, 478, 24)
}

const BRIGHTNESS: [SettingId; 3] = [
    SettingId::ScreenBrightness,
    SettingId::LedBrightness,
    SettingId::DimmedLedBrightness,
];

/// Brightness (shot 034): the ▲/▼/TEMPO help (`0xa3af5c`), three 32-segment bars with
/// their 0–32 value and the three mode icons that toggle dimmed LEDs.
fn brightness(ui: &UiState, l: &mut Layout, area: Rect) {
    rich_paragraph(
        l,
        Rect::new(area.x, area.y + 4, area.width, 72),
        "Turn the ▲, ▼, and TEMPO footswitches to adjust the brightness. Tap the Modes at \
         the bottom to toggle dimmed LED lights for each one individually.",
        MEDIUM,
        TEXT,
    );
    for (index, id) in BRIGHTNESS.into_iter().enumerate() {
        let top = area.y + 80 + index as i32 * 79;
        let value = match ui.snapshot.setting(id) {
            Some(SettingValue::Number(percent)) => Some(brightness_segments(*percent)),
            _ => None,
        };
        l.text(
            Rect::new(area.x, top, 300, 24),
            id.label(),
            MEDIUM,
            TEXT,
            Align::Left,
        );
        l.text(
            Rect::new(area.right() - 112, top, 100, 24),
            value.map_or_else(|| "-".to_owned(), |value| value.to_string()),
            MEDIUM,
            TEXT,
            Align::Right,
        );
        let bar = brightness_bar(area, index);
        // One 44 px tall target per bar: a tap fills the bar up to its x (I).
        l.control(
            Rect::new(bar.x, bar.y - 10, bar.width, 44),
            Control::Setting(SettingControl::Open(id)),
            value.is_some(),
            id.label(),
            Look::Area,
        );
        l.panel(bar, BACKGROUND, 2.0, None);
        let inner = bar.inset(2, 2);
        for segment in 0..value.unwrap_or(0).min(SEGMENTS) {
            let x = inner.x + segment * inner.width as i32 / SEGMENTS;
            let next = inner.x + (segment + 1) * inner.width as i32 / SEGMENTS;
            l.push(
                Rect::new(x, inner.y, (next - x - 2) as u32, inner.height),
                "",
                Look::Fill(ACCENT),
            );
        }
    }
    let modes = [
        (SettingId::PresetDimmed, Icon::ModePreset),
        (SettingId::SceneDimmed, Icon::ModeScene),
        (SettingId::StompDimmed, Icon::Footswitch),
    ];
    for (index, (id, icon)) in modes.into_iter().enumerate() {
        let on = ui.snapshot.setting(id) == Some(&SettingValue::Bool(true));
        let target = Rect::new(area.x + index as i32 * 40, area.bottom() - 40, 40, 40);
        l.control(
            target,
            Control::Setting(SettingControl::Open(id)),
            ui.snapshot.setting(id).is_some(),
            id.label(),
            Look::Area,
        );
        glyph(l, target, icon, if on { TEXT } else { FAINT });
    }
}

impl UiState {
    /// A tap on a Brightness bar fills it up to the touch point (the pointer of the tap).
    pub(crate) fn brightness_tap(&self, id: SettingId) -> Vec<UiCommand> {
        let Some(index) = BRIGHTNESS.iter().position(|bar| *bar == id) else {
            return Vec::new();
        };
        let pane = PANE.inset(16, 12);
        let body = Rect::new(pane.x, pane.y + 44, pane.width, pane.height - 44);
        let inner = brightness_bar(body, index).inset(2, 2);
        let along = (self.pointer.0 - inner.x).clamp(0, inner.width as i32 - 1);
        let segments = along * SEGMENTS / inner.width as i32 + 1;
        self.set_setting(id, brightness_percent(segments))
    }

    /// Brightness encoders (help `0xa3af5c`): ▼ (BANK DOWN, knob 8) the LEDs, TEMPO (knob 9)
    /// the dimmed LEDs, one segment per detent. ▲ (BANK UP, the Screen bar) is not a
    /// `KnobIndex`; touch sets it.
    pub(crate) fn brightness_knob(&mut self, knob: KnobIndex, delta: i32) -> Vec<UiCommand> {
        let id = match knob.get() {
            8 => SettingId::LedBrightness,
            9 => SettingId::DimmedLedBrightness,
            _ => return Vec::new(),
        };
        let Some(SettingValue::Number(percent)) = self.snapshot.setting(id) else {
            return Vec::new();
        };
        let segments = brightness_segments(*percent) + delta;
        self.set_setting(id, brightness_percent(segments))
    }

    /// A direct value request ([`SettingControl::Set`]) shaped by the setting kind.
    pub(crate) fn set_setting(&self, id: SettingId, raw: i32) -> Vec<UiCommand> {
        let value = match id.kind() {
            SettingKind::Toggle => SettingValue::Bool(raw != 0),
            SettingKind::Choice(options) => match u16::try_from(raw) {
                Ok(index) if usize::from(index) < options.len() => SettingValue::Choice(index),
                _ => return Vec::new(),
            },
            SettingKind::Number { bounds, .. } => SettingValue::Number(match bounds {
                Some((min, max)) => raw.clamp(min, max),
                None => raw,
            }),
        };
        if self.snapshot.setting(id) == Some(&value) {
            return Vec::new();
        }
        vec![UiCommand::ChangeSetting { id, value }]
    }
}

/// Global Bypass (shot 045): help with the inline bypass icon (`0xa3b968`), the grey
/// footnote (`0xa3ba18`), then a Cab tile and an IR Loader tile, each with four ROW power
/// buttons (bypassed = highlighted, I).
fn global_bypass(ui: &UiState, l: &mut Layout, area: Rect) {
    rich_paragraph(
        l,
        Rect::new(area.x, area.y + 4, area.width, 72),
        "Globally bypass Cabs, IR Loaders, or Neural Captures of cabs* on any row. Globally \
         bypassed devices will have a bypass icon ⏻ but will not appear bypassed on The Grid.",
        MEDIUM,
        TEXT,
    );
    l.paragraph(
        Rect::new(area.x, area.y + 76, area.width, 44),
        "*Neural Captures need to have the Capture Type set to \"Cab\" to be bypassed.",
        BODY,
        MUTED,
    );
    for (index, icon) in [Icon::Cab, Icon::IrLoader].into_iter().enumerate() {
        let id_of = if index == 0 {
            SettingId::GlobalBypassCab
        } else {
            SettingId::GlobalBypassIr
        };
        let top = area.y + 144 + index as i32 * 106;
        l.push(
            Rect::new(area.x, top, 70, 70),
            "",
            Look::Block {
                hue: Some(block::CAB),
                fill: CANVAS,
                icon: Some(icon),
                bypassed: false,
                selected: false,
                drop_target: false,
                ghost: false,
                capture_two: false,
            },
        );
        for row in 0..GRID_ROWS as u8 {
            let id = id_of(Row(row));
            let x = area.x + 176 + i32::from(row) * 82;
            l.text(
                Rect::new(x, top, 66, 20),
                id.label(),
                BODY,
                MUTED,
                Align::Center,
            );
            let value = ui.snapshot.setting(id);
            let bypassed = value == Some(&SettingValue::Bool(true));
            l.control(
                Rect::new(x, top + 26, 66, 44),
                Control::Setting(SettingControl::Open(id)),
                value.is_some(),
                format!("{} {}", if index == 0 { "CAB" } else { "IR" }, id.label()),
                Look::Button {
                    style: if bypassed {
                        ButtonStyle::Tab { selected: true }
                    } else {
                        ButtonStyle::Inset
                    },
                    icon: Some(Icon::Power),
                    font: None,
                },
            );
        }
        if index == 0 {
            l.push(
                Rect::new(area.x, top + 86, area.width, 1),
                "",
                Look::Fill(DIVIDER),
            );
        }
    }
}

/// About Us (shot 044 layout): black card, divider and the support block. Owned text: the
/// stock card names the company and carries its logo and QR code.
fn about(l: &mut Layout, area: Rect) {
    let card = Rect::new(area.x, area.y, area.width, 124);
    l.panel(card, CANVAS, 12.0, None);
    l.text(
        Rect::new(card.x + 20, card.y + 16, card.width - 40, 24),
        "Tetrad",
        MEDIUM_BOLD,
        TEXT,
        Align::Left,
    );
    l.paragraph(
        Rect::new(card.x + 20, card.y + 42, card.width - 40, 72),
        "An owned reconstruction of the Quad Cortex userspace.\nNot made or supported by Neural DSP Technologies.",
        MEDIUM,
        TEXT,
    );
    l.push(
        Rect::new(area.x, card.bottom() + 20, area.width, 1),
        "",
        Look::Fill(DIVIDER),
    );
    l.paragraph(
        Rect::new(area.x, card.bottom() + 52, area.width, 120),
        "For help with this build, read the project documentation in docs/.\n\nDevice support from the manufacturer does not cover this software.",
        MEDIUM,
        TEXT,
    );
}

fn account(ui: &UiState, l: &mut Layout, area: Rect) {
    match &ui.snapshot.account.username {
        // Stock 4.1.0 on the QC (2026-10-16): the link line as the title, a white paragraph,
        // a black 219 × 45 UNLINK DEVICE.
        Some(user) => {
            l.text(
                Rect::new(area.x, area.y - 44, area.width, 32),
                format!("Device linked to [{user}]"),
                LARGE,
                TEXT,
                Align::Left,
            );
            l.paragraph(
                Rect::new(area.x, area.y + 2, area.width, 50),
                "You are ready to send & receive Presets & Neural Captures and use Cloud backups.",
                MEDIUM,
                TEXT,
            );
            // 16 px label: ink 114 × 11 on `sa1.png` (18 px would be 128 × 13).
            l.button_in(
                Rect::new(area.x, area.y + 71, 219, 45),
                Control::Action(DeviceAction::CloudLogout),
                true,
                "UNLINK DEVICE",
                ButtonStyle::Inset,
                BODY,
            );
        }
        None => {
            l.paragraph(
                Rect::new(area.x, area.y, area.width, 60),
                "Link Quad Cortex with your account to use the Cloud service.",
                MEDIUM,
                TEXT,
            );
            l.button(
                Rect::new(area.x, area.y + 70, 240, 44),
                Control::Login,
                true,
                "LOGIN",
                ButtonStyle::Primary,
            );
        }
    }
}

/// A bare icon centred in `bounds`.
pub(super) fn glyph(l: &mut Layout, bounds: Rect, icon: Icon, color: Rgb) {
    l.push(
        bounds,
        "",
        Look::Glyph {
            icon,
            size: IconSize::Medium,
            color,
        },
    );
}

/// A filled ▲ (or ▼ with `down`) inside `bounds`, from 1 px rows: the stock font has no
/// such glyph.
pub(super) fn triangle(l: &mut Layout, bounds: Rect, down: bool, color: Rgb) {
    let height = bounds.height as i32;
    let half = bounds.width as f32 / 2.0;
    let cx = bounds.x as f32 + half;
    for row in 0..height {
        let rows_from_tip = if down { height - row } else { row + 1 };
        let reach = half * rows_from_tip as f32 / height as f32;
        let y = (bounds.y + row) as f32 + 0.5;
        l.line((cx - reach, y), (cx + reach, y), 1.0, color);
    }
}

/// Inline marks of the stock rich texts: ▲ / ▼ footswitch arrows and the ⏻ bypass icon.
#[derive(Clone, Copy)]
enum Mark {
    Up,
    Down,
    Bypass,
}

const MARK_WIDTH: f32 = 18.0;

fn mark(ch: char) -> Option<Mark> {
    match ch {
        '▲' => Some(Mark::Up),
        '▼' => Some(Mark::Down),
        '⏻' => Some(Mark::Bypass),
        _ => None,
    }
}

/// A word's pieces: text runs and inline marks.
fn pieces(word: &str) -> Vec<Result<&str, Mark>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (index, ch) in word.char_indices() {
        if let Some(mark) = mark(ch) {
            if start < index {
                out.push(Ok(&word[start..index]));
            }
            out.push(Err(mark));
            start = index + ch.len_utf8();
        }
    }
    if start < word.len() {
        out.push(Ok(&word[start..]));
    }
    out
}

fn word_width(word: &str, font: Font) -> f32 {
    pieces(word)
        .into_iter()
        .map(|piece| match piece {
            Ok(text) => font.width(text),
            Err(_) => MARK_WIDTH,
        })
        .sum()
}

/// Lines of a rich text as words, wrapped like [`wrap`].
fn rich_lines(text: &str, font: Font, width: f32) -> Vec<Vec<&str>> {
    let space = font.width(" ");
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line: Vec<&str> = Vec::new();
        let mut used = 0.0;
        for word in paragraph.split_whitespace() {
            let w = word_width(word, font);
            if !line.is_empty() && used + space + w > width {
                lines.push(std::mem::take(&mut line));
                used = 0.0;
            }
            used += if line.is_empty() { w } else { space + w };
            line.push(word);
        }
        lines.push(line);
    }
    lines
}

fn rich_height(text: &str, font: Font, width: u32) -> u32 {
    let lines = if text.chars().any(|ch| mark(ch).is_some()) {
        rich_lines(text, font, width as f32).len()
    } else {
        wrap(text, font, width as f32).len()
    };
    lines as u32 * font.size.line_px()
}

/// A paragraph whose ▲, ▼ and ⏻ draw as inline icons.
pub(super) fn rich_paragraph(l: &mut Layout, bounds: Rect, text: &str, font: Font, color: Rgb) {
    if !text.chars().any(|ch| mark(ch).is_some()) {
        l.paragraph(bounds, text, font, color);
        return;
    }
    let line_px = font.size.line_px() as i32;
    let space = font.width(" ");
    for (index, words) in rich_lines(text, font, bounds.width as f32)
        .iter()
        .enumerate()
    {
        let y = bounds.y + index as i32 * line_px;
        if y + line_px > bounds.bottom() {
            break;
        }
        let mut x = bounds.x as f32;
        let mut run = String::new();
        let mut run_x = x;
        let flush = |l: &mut Layout, run: &mut String, run_x: f32| {
            if !run.is_empty() {
                let width = font.width(run).ceil() as u32 + 4;
                l.text(
                    Rect::new(run_x as i32, y, width, line_px as u32),
                    std::mem::take(run),
                    font,
                    color,
                    Align::Left,
                );
            }
        };
        for (position, word) in words.iter().enumerate() {
            if position > 0 {
                if !run.is_empty() {
                    run.push(' ');
                }
                x += space;
            }
            for piece in pieces(word) {
                match piece {
                    Ok(part) => {
                        if run.is_empty() {
                            run_x = x;
                        }
                        run.push_str(part);
                        x += font.width(part);
                    }
                    Err(mark) => {
                        flush(l, &mut run, run_x);
                        let slot = Rect::new(x as i32, y, MARK_WIDTH as u32, line_px as u32);
                        match mark {
                            Mark::Up | Mark::Down => {
                                let size = Rect::new(slot.x + 3, slot.y + line_px / 2 - 5, 12, 10);
                                triangle(l, size, matches!(mark, Mark::Down), color);
                            }
                            Mark::Bypass => l.push(
                                slot,
                                "",
                                Look::Glyph {
                                    icon: Icon::Power,
                                    size: IconSize::Small,
                                    color,
                                },
                            ),
                        }
                        x += MARK_WIDTH;
                    }
                }
            }
        }
        flush(l, &mut run, run_x);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_ui(page: SettingsPage, values: &[(SettingId, SettingValue)]) -> UiState {
        let mut snapshot = UiSnapshot::default();
        snapshot.settings.extend(values.iter().cloned());
        let mut ui = UiState::new();
        ui.set_snapshot(snapshot).expect("valid snapshot");
        ui.navigate(Screen::Settings(page));
        ui
    }

    fn center(ui: &UiState, control: Control) -> (i32, i32) {
        ui.hit_regions()
            .into_iter()
            .find(|region| region.control == control)
            .map(|region| region.bounds.center())
            .expect("control on screen")
    }

    /// Shot 034: stored percent ↔ 0–32 bars (defaults 50 → 16, 6 → 2); a tap fills the bar
    /// to the touch point; ▼ (knob 8) steps the LEDs one segment.
    #[test]
    fn brightness_bars_show_32_segments_and_set_by_tap_and_encoder() {
        assert_eq!((brightness_segments(50), brightness_segments(6)), (16, 2));
        for segments in 0..=SEGMENTS {
            assert_eq!(brightness_segments(brightness_percent(segments)), segments);
        }
        let leds = SettingId::LedBrightness;
        let mut ui = settings_ui(
            SettingsPage::Brightness,
            &[
                (SettingId::ScreenBrightness, SettingValue::Number(50)),
                (leds, SettingValue::Number(50)),
            ],
        );
        let bar = Control::Setting(SettingControl::Open(leds));
        let (_, y) = center(&ui, bar);
        let right_end = 288 + 478 - 4;
        ui.handle_input(InputEvent::PointerDown { x: right_end, y });
        assert_eq!(
            ui.handle_input(InputEvent::PointerUp { x: right_end, y }),
            vec![UiCommand::ChangeSetting {
                id: leds,
                value: SettingValue::Number(100),
            }]
        );
        let knob = KnobIndex::new(8).expect("knob");
        assert_eq!(
            ui.handle_input(InputEvent::Knob { knob, delta: -1 }),
            vec![UiCommand::ChangeSetting {
                id: leds,
                value: SettingValue::Number(brightness_percent(15)),
            }]
        );
    }

    /// Shot 034 / spec I: "Power On Mode" is a mini page. The QC System menu omits it, so
    /// Factory Reset shows without a scroll.
    #[test]
    fn power_on_mode_is_listed_on_the_mini_only() {
        let page = |page| Control::Setting(SettingControl::Page(page));
        for mini in [false, true] {
            let mut ui = UiState::new();
            ui.set_snapshot(UiSnapshot {
                mini,
                ..UiSnapshot::default()
            })
            .expect("valid snapshot");
            ui.navigate(Screen::Settings(SettingsPage::Brightness));
            let listed: Vec<Control> = ui.hit_regions().iter().map(|r| r.control).collect();
            assert_eq!(listed.contains(&page(SettingsPage::PowerOnMode)), mini);
            if !mini {
                assert!(listed.contains(&page(SettingsPage::FactoryReset)));
            }
        }
    }

    /// Shot 091 / `0x7cb78`: the (i) button opens the stock help; the row still toggles.
    #[test]
    fn midi_rows_open_their_stock_help() {
        let thru = SettingId::MidiThru;
        let mut ui = settings_ui(SettingsPage::Midi, &[(thru, SettingValue::Bool(false))]);
        let help = Control::Setting(SettingControl::Info(
            "MIDI Thru",
            thru.help().expect("stock help"),
        ));
        ui.handle_input(InputEvent::Activate(help));
        assert!(ui.overlay_open());
        ui.handle_input(InputEvent::Back);
        assert_eq!(
            ui.handle_input(InputEvent::Activate(Control::Setting(
                SettingControl::Open(thru)
            ))),
            vec![UiCommand::ChangeSetting {
                id: thru,
                value: SettingValue::Bool(true),
            }]
        );
    }

    /// Manual §10 (`0xa3d360`): a confirmed factory reset locks the screen; Back and
    /// touches do nothing.
    #[test]
    fn confirmed_factory_reset_locks_the_screen() {
        let mut ui = settings_ui(SettingsPage::FactoryReset, &[]);
        let reset = Control::Action(DeviceAction::FactoryReset);
        ui.handle_input(InputEvent::Activate(reset));
        assert_eq!(
            ui.handle_input(InputEvent::Activate(Control::Dialog(DialogButton::Primary))),
            vec![UiCommand::Device {
                action: DeviceAction::FactoryReset,
            }]
        );
        assert!(ui.hit_regions().is_empty());
        assert!(ui.handle_input(InputEvent::Back).is_empty());
        assert_eq!(ui.screen(), Screen::Settings(SettingsPage::FactoryReset));
    }
}
