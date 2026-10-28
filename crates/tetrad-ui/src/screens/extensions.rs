//! Owned Extensions pages (Tetrad; stock has none): the Tetrad › Extensions page with the
//! installed and available extensions, the detail page of one installation, Repositories,
//! and the settings section that each extension with settings adds to the Tetrad group. The
//! host decides installation, trust and state: each control sends an [`ExtensionCommand`]
//! and the pages show the next snapshot. Positions are inside the
//! settings content panel and follow the generic settings rows (60 px rows with a divider,
//! 34 px section headers, black fields and buttons on the panel).

use tetrad_assets::{Icon, IconSize, Rgb};

use crate::layout::SMALL_BOLD;
use crate::layout::{BODY, BODY_BOLD, ButtonStyle, CAPTION, LARGE, Layout, ListId, Look, MEDIUM};
use crate::render::{Align, fit, wrap};
use crate::state::{ChoicePurpose, ExtensionConfirm, ExtensionsPage, Overlay, TextTarget};
use crate::*;

const ROW: u32 = 60;
const HEADER: u32 = 34;
const AVAILABLE_ROW: u32 = 76;
const SOURCE_ROW: u32 = 84;
const EMPTY_ROW: u32 = 44;
/// Command buttons: two columns of 44 px buttons on a 52 px pitch.
const COMMAND_PITCH: u32 = 52;
const STATUS_LINE: u32 = 24;
const BLOCK_ROW: u32 = 48;
/// The detail page shows the newest log lines.
const LOG_LINES: usize = 10;
const LOG_LINE: u32 = 18;
const BADGE: (u32, u32) = (88, 24);
/// Header buttons: 40 px high, centred on the 32 px page title.
const HEADER_BUTTON: u32 = 40;

/// Dark fills of the Starting and Faulted badges, as the accent fill is for Running.
const WARNING_FILL: Rgb = Rgb(0x33, 0x2d, 0x10);
const DANGER_FILL: Rgb = Rgb(0x3a, 0x14, 0x14);

/// The installation whose detail page or settings section is open.
fn open_extension(ui: &UiState) -> Option<&InstalledExtension> {
    let key = match (&ui.extensions, ui.screen()) {
        (_, Screen::Settings(SettingsPage::ExtensionSettings)) => ui.extension_section.as_ref()?,
        (ExtensionsPage::Detail(key), _) => key,
        _ => return None,
    };
    (ui.snapshot.extensions.installed.iter()).find(|extension| extension.key == *key)
}

/// An enabled extension with settings has a section in the Tetrad group.
pub(super) fn has_section(extension: &InstalledExtension) -> bool {
    extension.enabled && !extension.settings.is_empty()
}

/// The installation whose settings section is open, while it has one.
pub(super) fn section_extension(ui: &UiState) -> Option<&InstalledExtension> {
    (ui.screen() == Screen::Settings(SettingsPage::ExtensionSettings))
        .then(|| open_extension(ui))
        .flatten()
        .filter(|extension| has_section(extension))
}

/// Settings › Tetrad › (extension name): the extension's settings in the detail page rows.
pub(super) fn section(ui: &UiState, l: &mut Layout, body: Rect) {
    let Some(extension) = section_extension(ui) else {
        return;
    };
    let area = Rect::new(body.x, body.y, body.width + 8, body.height);
    let content = extension.settings.len() as u32 * ROW;
    let offset = ui.offset(ListId::SettingsPage);
    l.scroll(ListId::SettingsPage, area, content, offset, |l, y| {
        for (index, setting) in extension.settings.iter().enumerate() {
            let row = Rect::new(area.x, y + index as i32 * ROW as i32, area.width, ROW);
            setting_row(l, row, index as u16, setting);
        }
    });
}

/// Updates and Repositories put their own title in the page title place.
pub(super) fn own_title(ui: &UiState) -> bool {
    match ui.extensions {
        ExtensionsPage::List => false,
        ExtensionsPage::Detail(_) => open_extension(ui).is_some(),
        ExtensionsPage::Sources => true,
    }
}

/// `pane` is the settings content panel inside its 16 × 12 px inset.
pub(super) fn layout(ui: &UiState, l: &mut Layout, pane: Rect) {
    match (&ui.extensions, open_extension(ui)) {
        (ExtensionsPage::Sources, _) => sources(ui, l, pane),
        (ExtensionsPage::Detail(_), Some(extension)) => detail(ui, l, pane, extension),
        _ => list(ui, l, pane),
    }
}

/// The default repository reads "Default", any other its `owner/repo`.
fn source_label<'a>(view: &ExtensionsView, source: &'a str) -> &'a str {
    let default = (view.sources.iter())
        .any(|entry| entry.default && entry.repository.eq_ignore_ascii_case(source));
    if default { "Default" } else { source }
}

/// Generic settings section header (`PageRow::Header`).
fn header(l: &mut Layout, x: i32, y: i32, width: u32, title: &str) {
    let p = l.palette;
    l.text(
        Rect::new(x, y + 10, width, 20),
        title,
        BODY_BOLD,
        p.text,
        Align::Left,
    );
}

pub(super) fn divider(l: &mut Layout, row: Rect) {
    let p = l.palette;
    l.push(
        Rect::new(row.x, row.bottom() - 1, row.width, 1),
        "",
        Look::Fill(p.divider),
    );
}

fn empty(l: &mut Layout, row: Rect, text: &str) {
    let p = l.palette;
    l.text(
        Rect::new(row.x + 16, row.y, row.width - 32, row.height),
        text,
        MEDIUM,
        p.muted,
        Align::Left,
    );
}

fn state_badge(l: &mut Layout, bounds: Rect, state: &ExtensionState) {
    let p = l.palette;
    let (label, ink, fill) = match state {
        ExtensionState::Running => ("Running", p.accent, p.accent_fill),
        ExtensionState::Starting => ("Starting", p.warning, p.tone(WARNING_FILL)),
        ExtensionState::Disabled => ("Disabled", p.muted, p.surface),
        ExtensionState::Faulted(_) => ("Faulted", p.danger, p.tone(DANGER_FILL)),
    };
    l.panel(bounds, fill, bounds.height as f32 / 2.0, None);
    l.text(bounds, label, SMALL_BOLD, ink, Align::Center);
}

/// Back arrow left of an own page title: [`Control::Back`] returns to the Extensions list.
fn title_row(l: &mut Layout, pane: Rect, title: &str, right: i32) {
    let p = l.palette;
    l.control(
        Rect::new(pane.x - 8, pane.y - 4, 40, HEADER_BUTTON),
        Control::Back,
        true,
        "BACK",
        Look::IconButton {
            icon: Icon::Back,
            fill: p.panel,
        },
    );
    let x = pane.x + 40;
    l.text(
        Rect::new(x, pane.y, (right - 8 - x).max(0) as u32, 32),
        title,
        LARGE,
        p.text,
        Align::Left,
    );
}

/// The running job or last result under the title; a ring marks a running job.
fn status_line(l: &mut Layout, view: &ExtensionsView, area: Rect) {
    let p = l.palette;
    let mut x = area.x;
    if view.busy {
        l.push(
            Rect::new(x, area.y + 4, 16, 16),
            "",
            Look::Arc {
                width: 2.0,
                start: 0.0,
                sweep: 270.0,
                color: p.accent,
            },
        );
        x += 24;
    }
    let text = (view.status.as_deref()).or(view.busy.then_some("Working…"));
    if let Some(text) = text {
        l.text(
            Rect::new(x, area.y, (area.right() - x) as u32, area.height),
            text,
            CAPTION,
            p.muted,
            Align::Left,
        );
    }
}

/// The scrolled list under the title and the status line.
fn list_area(pane: Rect) -> Rect {
    Rect::new(pane.x, pane.y + 72, pane.width + 8, pane.height - 72)
}

/// Extensions: REFRESH and REPOSITORIES by the title, the status line, then Installed and
/// Available.
fn list(ui: &UiState, l: &mut Layout, pane: Rect) {
    let view = &ui.snapshot.extensions;
    let top = pane.y - 4;
    let repositories = Rect::new(pane.right() - 148, top, 148, HEADER_BUTTON);
    let refresh = Rect::new(repositories.x - 8 - 108, top, 108, HEADER_BUTTON);
    l.button_in(
        refresh,
        Control::Extension(ExtensionControl::Refresh),
        !view.busy,
        "REFRESH",
        ButtonStyle::Inset,
        BODY,
    );
    l.button_in(
        repositories,
        Control::Extension(ExtensionControl::Sources),
        true,
        "REPOSITORIES",
        ButtonStyle::Inset,
        BODY,
    );
    status_line(l, view, Rect::new(pane.x, pane.y + 44, pane.width, 24));
    let area = list_area(pane);
    let installed = match view.installed.len() as u32 {
        0 => EMPTY_ROW,
        rows => rows * ROW,
    };
    let available = match view.available.len() as u32 {
        0 => EMPTY_ROW,
        rows => rows * AVAILABLE_ROW,
    };
    let content = 2 * HEADER + installed + 8 + available;
    let offset = ui.offset(ListId::Extensions);
    l.scroll(ListId::Extensions, area, content, offset, |l, mut y| {
        header(l, area.x, y, area.width, "Installed");
        y += HEADER as i32;
        if view.installed.is_empty() {
            empty(
                l,
                Rect::new(area.x, y, area.width, EMPTY_ROW),
                "No extensions installed",
            );
        }
        for (index, extension) in view.installed.iter().enumerate() {
            let row = Rect::new(area.x, y + index as i32 * ROW as i32, area.width, ROW);
            installed_row(l, view, row, index as u16, extension);
        }
        y += installed as i32 + 8;
        header(l, area.x, y, area.width, "Available");
        y += HEADER as i32;
        if view.available.is_empty() {
            empty(
                l,
                Rect::new(area.x, y, area.width, EMPTY_ROW),
                "Refresh to see available extensions",
            );
        }
        for (index, extension) in view.available.iter().enumerate() {
            let row = Rect::new(
                area.x,
                y + index as i32 * AVAILABLE_ROW as i32,
                area.width,
                AVAILABLE_ROW,
            );
            available_row(l, view, row, index as u16, extension);
        }
    });
    super::scrollbar(l, area, content, offset, 4);
}

/// Name, version and source, the state badge and a chevron; the row opens the detail page.
fn installed_row(
    l: &mut Layout,
    view: &ExtensionsView,
    row: Rect,
    index: u16,
    extension: &InstalledExtension,
) {
    let p = l.palette;
    l.control(
        row,
        Control::Extension(ExtensionControl::Open(index)),
        true,
        extension.name.clone(),
        Look::Area,
    );
    let chevron = Rect::new(row.right() - 40, row.y, 24, row.height);
    l.push(
        chevron,
        "",
        Look::Glyph {
            icon: Icon::Forward,
            size: IconSize::Small,
            color: p.muted,
        },
    );
    let badge = Rect::new(
        chevron.x - 8 - BADGE.0 as i32,
        row.y + (row.height - BADGE.1) as i32 / 2,
        BADGE.0,
        BADGE.1,
    );
    state_badge(l, badge, &extension.state);
    let x = row.x + 16;
    let width = (badge.x - 12 - x) as u32;
    l.text(
        Rect::new(x, row.y + 8, width, 22),
        extension.name.clone(),
        BODY,
        p.text,
        Align::Left,
    );
    let meta = format!(
        "{} · {}",
        extension.version,
        source_label(view, &extension.source)
    );
    // A duplicate warning, else the update note, keeps at least half the line; the version and
    // source give way first.
    let note = if extension.duplicates.is_empty() {
        (extension.update.as_ref()).map(|version| (format!(" · Update to {version}"), p.accent))
    } else {
        let from = extension.duplicates.join(", ");
        Some((format!(" · Also on from {from}"), p.warning))
    };
    let reserved = note.as_ref().map_or(0, |(note, _)| {
        (CAPTION.width(note).ceil() as u32).min(width / 2)
    });
    let meta_width = (CAPTION.width(&meta).ceil() as u32 + 1).min(width - reserved);
    l.text(
        Rect::new(x, row.y + 32, meta_width, 18),
        meta,
        CAPTION,
        p.muted,
        Align::Left,
    );
    if let Some((note, color)) = note {
        l.text(
            Rect::new(x + meta_width as i32, row.y + 32, width - meta_width, 18),
            note,
            CAPTION,
            color,
            Align::Left,
        );
    }
    divider(l, row);
}

/// Name, description, version and source, and INSTALL (disabled while a job runs).
fn available_row(
    l: &mut Layout,
    view: &ExtensionsView,
    row: Rect,
    index: u16,
    extension: &AvailableExtension,
) {
    let p = l.palette;
    let install = Rect::new(
        row.right() - 16 - 104,
        row.y + (row.height - HEADER_BUTTON) as i32 / 2,
        104,
        HEADER_BUTTON,
    );
    l.button_in(
        install,
        Control::Extension(ExtensionControl::Install(index)),
        !view.busy,
        "INSTALL",
        ButtonStyle::Inset,
        BODY,
    );
    let x = row.x + 16;
    let width = (install.x - 12 - x) as u32;
    // A second installation from another source doubles its blocks, and a program runs
    // outside the extension limits: the warning takes the description's line.
    let note = if !extension.duplicates.is_empty() {
        let from = extension.duplicates.join(", ");
        (format!("Also installed from {from}"), p.warning)
    } else if extension.app {
        (APP_WARNING.to_owned(), p.warning)
    } else {
        (extension.description.clone(), p.muted)
    };
    let lines = [
        (8, 22, extension.name.clone(), BODY, p.text),
        (31, 18, note.0, CAPTION, note.1),
        (
            50,
            18,
            format!(
                "{} · {}",
                extension.version,
                source_label(view, &extension.source)
            ),
            CAPTION,
            p.muted,
        ),
    ];
    for (dy, height, text, font, color) in lines {
        l.text(
            Rect::new(x, row.y + dy, width, height),
            text,
            font,
            color,
            Align::Left,
        );
    }
    divider(l, row);
}

/// What a program may do, on the Available row.
const APP_WARNING: &str = "Runs a program with full access to the unit";

/// One block of the detail page.
enum Part<'a> {
    Meta,
    /// The program button and what the program may do.
    App(&'a str),
    Description(Vec<String>),
    Enabled,
    Fault(Vec<String>),
    Update(&'a str),
    Header(&'static str),
    Setting(u16, &'a ExtensionSetting),
    /// Up to two command buttons from this index.
    Commands(usize),
    StatusLine(&'a str),
    Block(&'a ExtensionBlock),
    Mode(&'a ExtensionMode),
    Log(&'a [String]),
    Remove,
}

impl Part<'_> {
    fn height(&self) -> u32 {
        match self {
            Self::Meta => 28,
            Self::Description(lines) => lines.len() as u32 * BODY.size.line_px() + 12,
            Self::Fault(lines) => (lines.len() as u32 * CAPTION.size.line_px() + 32).max(72),
            Self::Header(_) => HEADER,
            Self::Commands(_) => COMMAND_PITCH,
            Self::StatusLine(_) => STATUS_LINE,
            Self::Block(_) => BLOCK_ROW,
            Self::Mode(_) | Self::App(_) => ROW,
            Self::Log(lines) => lines.len() as u32 * LOG_LINE + 16,
            Self::Enabled | Self::Update(_) | Self::Setting(..) | Self::Remove => ROW,
        }
    }
}

/// Width of the fault text beside RETRY.
const FAULT_TEXT: u32 = 488 - 32 - 104 - 16;

fn parts(extension: &InstalledExtension, width: u32) -> Vec<Part<'_>> {
    let mut parts = vec![Part::Meta];
    if !extension.description.trim().is_empty() {
        parts.push(Part::Description(wrap(
            &extension.description,
            BODY,
            width as f32,
        )));
    }
    parts.push(Part::Enabled);
    if let ExtensionState::Faulted(fault) = &extension.state {
        parts.push(Part::Fault(wrap(fault, CAPTION, FAULT_TEXT as f32)));
    }
    if let Some(version) = &extension.update {
        parts.push(Part::Update(version));
    }
    if let Some(label) = &extension.app {
        parts.push(Part::Header("Program"));
        parts.push(Part::App(label));
    }
    if !extension.settings.is_empty() {
        parts.push(Part::Header("Settings"));
        parts.extend(
            (extension.settings.iter().enumerate())
                .map(|(index, setting)| Part::Setting(index as u16, setting)),
        );
    }
    if !extension.commands.is_empty() {
        parts.push(Part::Header("Commands"));
        parts.extend((0..extension.commands.len()).step_by(2).map(Part::Commands));
    }
    if !extension.blocks.is_empty() {
        parts.push(Part::Header("DSP blocks"));
        parts.extend(extension.blocks.iter().map(Part::Block));
    }
    if let Some(mode) = &extension.footswitch_mode {
        parts.push(Part::Header("Footswitch mode"));
        parts.push(Part::Mode(mode));
    }
    if !extension.themes.is_empty() {
        parts.push(Part::Header("Themes (Settings → Appearance)"));
        parts.extend(extension.themes.iter().map(|name| Part::StatusLine(name)));
    }
    if !extension.status_lines.is_empty() {
        parts.push(Part::Header("Status"));
        parts.extend(
            extension
                .status_lines
                .iter()
                .map(|line| Part::StatusLine(line)),
        );
    }
    if !extension.log.is_empty() {
        let first = extension.log.len().saturating_sub(LOG_LINES);
        parts.push(Part::Header("Log"));
        parts.push(Part::Log(&extension.log[first..]));
    }
    parts.push(Part::Remove);
    parts
}

/// An extension's page: back arrow, name and state badge, then its description, the enable
/// switch, fault and update rows, settings, commands, DSP blocks, status lines, log and REMOVE.
fn detail(ui: &UiState, l: &mut Layout, pane: Rect, extension: &InstalledExtension) {
    let view = &ui.snapshot.extensions;
    let badge = Rect::new(pane.right() - BADGE.0 as i32, pane.y + 4, BADGE.0, BADGE.1);
    title_row(l, pane, &extension.name, badge.x);
    state_badge(l, badge, &extension.state);
    let area = Rect::new(pane.x, pane.y + 44, pane.width + 8, pane.height - 44);
    let parts = parts(extension, pane.width);
    let content: u32 = parts.iter().map(Part::height).sum();
    let offset = ui.offset(ListId::SettingsPage);
    l.scroll(ListId::SettingsPage, area, content, offset, |l, mut y| {
        for part in &parts {
            let height = part.height();
            let row = Rect::new(area.x, y, area.width, height);
            detail_part(l, view, extension, part, row);
            y += height as i32;
        }
    });
}

fn detail_part(
    l: &mut Layout,
    view: &ExtensionsView,
    extension: &InstalledExtension,
    part: &Part,
    row: Rect,
) {
    let p = l.palette;
    let control = |control| Control::Extension(control);
    match part {
        Part::Meta => l.text(
            Rect::new(row.x, row.y, row.width - 8, 20),
            format!(
                "Version {} · {}",
                extension.version,
                source_label(view, &extension.source)
            ),
            CAPTION,
            p.muted,
            Align::Left,
        ),
        Part::Description(lines) => l.paragraph(
            Rect::new(row.x, row.y, row.width - 8, row.height),
            lines.join("\n"),
            BODY,
            p.text,
        ),
        Part::Enabled => {
            l.control(
                row,
                control(ExtensionControl::Enable),
                true,
                "Enabled",
                Look::Area,
            );
            switch_row(l, row, "Enabled", Some(extension.enabled));
        }
        Part::Fault(lines) => {
            let card = Rect::new(row.x, row.y + 4, row.width - 8, row.height - 8);
            l.panel(card, p.surface, 8.0, None);
            l.paragraph(
                Rect::new(card.x + 16, card.y + 12, FAULT_TEXT, card.height - 16),
                lines.join("\n"),
                CAPTION,
                p.danger,
            );
            l.button_in(
                Rect::new(
                    card.right() - 16 - 104,
                    card.y + (card.height - HEADER_BUTTON) as i32 / 2,
                    104,
                    HEADER_BUTTON,
                ),
                control(ExtensionControl::Retry),
                true,
                "RETRY",
                ButtonStyle::Inset,
                BODY,
            );
        }
        Part::Update(version) => {
            let button = Rect::new(row.right() - 120, row.y + 10, 120, HEADER_BUTTON);
            l.button_in(
                button,
                control(ExtensionControl::Update),
                !view.busy,
                "UPDATE",
                ButtonStyle::Primary,
                BODY,
            );
            label(
                l,
                row,
                button.x - 8,
                format!("Version {version} is available"),
            );
            divider(l, row);
        }
        Part::Header(title) => header(l, row.x, row.y, row.width, title),
        Part::Setting(index, setting) => setting_row(l, row, *index, setting),
        Part::Commands(first) => {
            let width = (row.width - 8 - 8) / 2;
            for (column, button) in extension.commands.iter().skip(*first).take(2).enumerate() {
                l.button_in(
                    Rect::new(row.x + column as i32 * (width + 8) as i32, row.y, width, 44),
                    control(ExtensionControl::Run((first + column) as u16)),
                    true,
                    fit(&button.label, BODY, (width - 24) as f32),
                    ButtonStyle::Inset,
                    BODY,
                );
            }
        }
        Part::App(label) => {
            let width = 220;
            let button = Rect::new(row.x, row.y + 8, width, 44);
            l.button_in(
                button,
                control(ExtensionControl::Launch),
                extension.enabled,
                fit(label, BODY, (width - 24) as f32),
                ButtonStyle::Primary,
                BODY,
            );
            let x = button.right() + 16;
            let text = Rect::new(x, row.y + 8, (row.right() - 8 - x).max(0) as u32, 22);
            let lines = [
                ("Full access to the unit", p.warning),
                ("Hold the screen for 2 seconds to quit", p.muted),
            ];
            for (line, (words, color)) in lines.into_iter().enumerate() {
                let text = Rect::new(text.x, text.y + line as i32 * 22, text.width, 20);
                let words = fit(words, CAPTION, text.width as f32);
                l.text(text, words, CAPTION, color, Align::Left);
            }
            divider(l, row);
        }
        Part::Mode(mode) => {
            let button = Rect::new(row.right() - 8 - 104, row.y + 10, 104, HEADER_BUTTON);
            l.button_in(
                button,
                Control::Custom(CustomControl::Edit),
                mode.provides,
                "EDIT",
                ButtonStyle::Inset,
                BODY,
            );
            let width = (button.x - 16 - row.x).max(0) as u32;
            l.text(
                Rect::new(row.x, row.y + 8, width, 22),
                mode.name.as_str(),
                BODY,
                p.text,
                Align::Left,
            );
            l.text(
                Rect::new(row.x, row.y + 32, width, 18),
                fit(&mode.detail, CAPTION, width as f32),
                CAPTION,
                if mode.provides { p.muted } else { p.warning },
                Align::Left,
            );
            divider(l, row);
        }
        Part::Block(block) => {
            let width = row.width - 8;
            l.text(
                Rect::new(row.x, row.y + 2, width, 22),
                block.name.as_str(),
                BODY,
                p.text,
                Align::Left,
            );
            l.text(
                Rect::new(row.x, row.y + 24, width, 18),
                fit(&block.detail, CAPTION, width as f32),
                CAPTION,
                if block.ready { p.muted } else { p.warning },
                Align::Left,
            );
        }
        Part::StatusLine(line) => l.text(
            Rect::new(row.x, row.y, row.width - 8, row.height),
            *line,
            BODY,
            p.text,
            Align::Left,
        ),
        Part::Log(lines) => {
            let panel = Rect::new(row.x, row.y, row.width - 8, row.height - 8);
            l.panel(panel, p.canvas, 8.0, None);
            for (index, line) in lines.iter().enumerate() {
                l.text(
                    Rect::new(
                        panel.x + 12,
                        panel.y + 4 + index as i32 * LOG_LINE as i32,
                        panel.width - 24,
                        LOG_LINE,
                    ),
                    line.as_str(),
                    CAPTION,
                    p.muted,
                    Align::Left,
                );
            }
        }
        Part::Remove => l.button(
            Rect::new(row.x, row.y + 8, 260, 44),
            control(ExtensionControl::Remove),
            true,
            "REMOVE",
            ButtonStyle::Danger,
        ),
    }
}

/// A row label from x + 16 to `right`, as generic setting rows.
pub(super) fn label(l: &mut Layout, row: Rect, right: i32, text: impl Into<String>) {
    let p = l.palette;
    let x = row.x + 16;
    l.text(
        Rect::new(x, row.y, (right - x).max(0) as u32, row.height),
        text,
        MEDIUM,
        p.text,
        Align::Left,
    );
}

/// A generic toggle row's look: label and the vertical switch at the right end.
pub(super) fn switch_row(l: &mut Layout, row: Rect, text: &str, on: Option<bool>) {
    let switch = Rect::new(row.right() - 66, row.y, 66, row.height);
    l.push(switch, "", Look::Switch { on });
    label(l, row, switch.x - 8, text);
    divider(l, row);
}

/// A manifest setting in the generic setting row styles: a toggle flips with the whole
/// row, a choice opens a dropdown list from its black field, a number has a stepper.
fn setting_row(l: &mut Layout, row: Rect, index: u16, setting: &ExtensionSetting) {
    let p = l.palette;
    let open = Control::Extension(ExtensionControl::Setting(index));
    match &setting.value {
        ExtensionSettingValue::Toggle(on) => {
            l.control(row, open, true, setting.label.clone(), Look::Area);
            switch_row(l, row, &setting.label, Some(*on));
            return;
        }
        ExtensionSettingValue::Choice { options, selected } => {
            let field = Rect::new(
                row.right() - 137,
                row.y + (row.height as i32 - 37) / 2,
                137,
                37,
            );
            l.control(
                Rect::new(field.x, field.y - 1, field.width, 40),
                open,
                true,
                setting.label.clone(),
                Look::Area,
            );
            l.panel(field, p.canvas, 4.0, None);
            let arrow = Rect::new(field.right() - 24, field.y + 15, 12, 7);
            super::settings::triangle(l, arrow, true, p.text);
            l.text(
                Rect::new(
                    field.x + 8,
                    field.y,
                    (arrow.x - field.x - 14) as u32,
                    field.height,
                ),
                options.get(*selected).map_or("-", String::as_str),
                MEDIUM,
                p.text,
                Align::Left,
            );
            label(l, row, field.x - 8, setting.label.as_str());
        }
        ExtensionSettingValue::Number { value, .. } => {
            let stepper = Rect::new(row.right() - 212, row.y + 8, 212, row.height - 16);
            super::stepper(
                l,
                stepper,
                Control::Extension(ExtensionControl::Step(index, -1)),
                Control::Extension(ExtensionControl::Step(index, 1)),
                value.to_string(),
                true,
            );
            label(l, row, stepper.x - 8, setting.label.as_str());
        }
    }
    divider(l, row);
}

/// Repositories: back arrow, title and ADD REPOSITORY, the status line, then one row per
/// source with its key fingerprint, status, enable switch and REMOVE (not for the default).
fn sources(ui: &UiState, l: &mut Layout, pane: Rect) {
    let view = &ui.snapshot.extensions;
    let add = Rect::new(pane.right() - 176, pane.y - 4, 176, HEADER_BUTTON);
    title_row(l, pane, "Repositories", add.x);
    l.button_in(
        add,
        Control::Extension(ExtensionControl::AddSource),
        !view.busy,
        "ADD REPOSITORY",
        ButtonStyle::Inset,
        BODY,
    );
    status_line(l, view, Rect::new(pane.x, pane.y + 44, pane.width, 24));
    let area = list_area(pane);
    let content = match view.sources.len() as u32 {
        0 => EMPTY_ROW,
        rows => rows * SOURCE_ROW,
    };
    let offset = ui.offset(ListId::SettingsPage);
    l.scroll(ListId::SettingsPage, area, content, offset, |l, y| {
        if view.sources.is_empty() {
            empty(
                l,
                Rect::new(area.x, y, area.width, EMPTY_ROW),
                "No repositories",
            );
        }
        for (index, source) in view.sources.iter().enumerate() {
            let row = Rect::new(
                area.x,
                y + index as i32 * SOURCE_ROW as i32,
                area.width,
                SOURCE_ROW,
            );
            source_row(l, row, index as u16, source);
        }
    });
    super::scrollbar(l, area, content, offset, 4);
}

fn source_row(l: &mut Layout, row: Rect, index: u16, source: &ExtensionSource) {
    let p = l.palette;
    let remove = Rect::new(
        row.right() - 16 - 44,
        row.y + (row.height - 44) as i32 / 2,
        44,
        44,
    );
    let switch = Rect::new(remove.x - 12 - 66, row.y + 12, 66, row.height - 24);
    l.control(
        switch,
        Control::Extension(ExtensionControl::SourceEnabled(index)),
        true,
        source.repository.clone(),
        Look::Area,
    );
    l.push(
        switch,
        "",
        Look::Switch {
            on: Some(source.enabled),
        },
    );
    if !source.default {
        l.icon_button(
            remove,
            Control::Extension(ExtensionControl::RemoveSource(index)),
            true,
            "REMOVE",
            Icon::Trash,
        );
    }
    let x = row.x + 16;
    let width = (switch.x - 12 - x) as u32;
    let pill = 72;
    let name_width = if source.default {
        width - pill - 8
    } else {
        width
    };
    let name = fit(&source.repository, BODY, name_width as f32);
    let name_px = BODY.width(&name).ceil() as u32 + 2;
    l.text(
        Rect::new(x, row.y + 10, name_px, 22),
        name,
        BODY,
        p.text,
        Align::Left,
    );
    if source.default {
        let badge = Rect::new(x + name_px as i32 + 8, row.y + 10, pill, 22);
        l.panel(badge, p.accent_fill, 11.0, None);
        l.text(badge, "Default", SMALL_BOLD, p.accent, Align::Center);
    }
    l.text(
        Rect::new(x, row.y + 36, width, 18),
        format!("Key {}", source.fingerprint),
        CAPTION,
        p.muted,
        Align::Left,
    );
    l.text(
        Rect::new(x, row.y + 56, width, 18),
        source.status.as_str(),
        CAPTION,
        p.muted,
        Align::Left,
    );
    divider(l, row);
}

/// A fingerprint without spaces in groups of four, so the dialog can wrap all of it.
fn grouped(fingerprint: &str) -> String {
    if fingerprint.contains(char::is_whitespace) {
        return fingerprint.to_owned();
    }
    let chars: Vec<char> = fingerprint.chars().collect();
    let groups: Vec<String> = chars
        .chunks(4)
        .map(|group| group.iter().collect())
        .collect();
    groups.join(" ")
}

/// The host's trust question for a repository key: CANCEL / TRUST.
pub(super) fn trust(l: &mut Layout, pending: &PendingTrust) {
    let panel = super::overlay::message_panel(
        l,
        &format!("Trust {}?", pending.repository),
        &format!(
            "Key fingerprint {}. Only trust repositories you know; extensions run code on your QC.",
            grouped(&pending.fingerprint)
        ),
    );
    super::overlay::confirm_buttons(l, panel, "CANCEL", "TRUST", false);
}

pub(super) fn confirm(l: &mut Layout, confirm: &ExtensionConfirm) {
    let (title, body) = match confirm {
        ExtensionConfirm::Remove { name, .. } => (
            "Remove extension",
            format!("Remove {name}? Its settings are kept."),
        ),
        ExtensionConfirm::RemoveSource(repository) => (
            "Remove repository",
            format!("Remove {repository} from your repositories?"),
        ),
    };
    let panel = super::overlay::message_panel(l, title, &body);
    super::overlay::confirm_buttons(l, panel, "CANCEL", "REMOVE", true);
}

/// The request of a confirmed REMOVE.
pub(crate) fn confirmed(confirm: ExtensionConfirm) -> Vec<UiCommand> {
    vec![UiCommand::Extension(match confirm {
        ExtensionConfirm::Remove { key, .. } => ExtensionCommand::Remove { key },
        ExtensionConfirm::RemoveSource(repository) => ExtensionCommand::RemoveSource { repository },
    })]
}

/// Title, options and selection of choice setting `index` of the open extension.
pub(super) fn choice(ui: &UiState, index: u16) -> Option<(&str, Vec<&str>, usize)> {
    let setting = open_extension(ui)?.settings.get(usize::from(index))?;
    match &setting.value {
        ExtensionSettingValue::Choice { options, selected } => Some((
            setting.label.as_str(),
            options.iter().map(String::as_str).collect(),
            *selected,
        )),
        _ => None,
    }
}

/// Whether the controls of both views address the same items: a touch in progress on an
/// Extensions control is cancelled when its rows move (status and log lines may change).
pub(crate) fn same_targets(a: &ExtensionsView, b: &ExtensionsView) -> bool {
    type Targets<'a> = (
        Vec<(&'a str, Vec<&'a str>, Vec<&'a str>)>,
        Vec<(&'a str, &'a str)>,
        Vec<&'a str>,
    );
    fn targets(view: &ExtensionsView) -> Targets<'_> {
        (
            (view.installed.iter())
                .map(|extension| {
                    (
                        extension.key.as_str(),
                        (extension.settings.iter())
                            .map(|s| s.key.as_str())
                            .collect(),
                        (extension.commands.iter())
                            .map(|c| c.id.as_str())
                            .chain(extension.app.as_deref())
                            .collect(),
                    )
                })
                .collect(),
            (view.available.iter())
                .map(|extension| (extension.source.as_str(), extension.id.as_str()))
                .collect(),
            (view.sources.iter())
                .map(|source| source.repository.as_str())
                .collect(),
        )
    }
    a == b || targets(a) == targets(b)
}

impl UiState {
    pub(crate) fn extension_control(&mut self, control: ExtensionControl) -> Vec<UiCommand> {
        use ExtensionControl as C;
        let send = |command| vec![UiCommand::Extension(command)];
        let view = &self.snapshot.extensions;
        match control {
            C::Refresh => return send(ExtensionCommand::Refresh),
            C::Sources => {
                self.extensions = ExtensionsPage::Sources;
                self.reset_scroll(ListId::SettingsPage);
                return Vec::new();
            }
            C::Open(index) => {
                if let Some(key) = (view.installed.get(usize::from(index))).map(|e| e.key.clone()) {
                    self.extensions = ExtensionsPage::Detail(key);
                    self.reset_scroll(ListId::SettingsPage);
                }
                return Vec::new();
            }
            C::Install(index) => {
                return (view.available.get(usize::from(index)))
                    .map(|extension| {
                        send(ExtensionCommand::Install {
                            source: extension.source.clone(),
                            id: extension.id.clone(),
                        })
                    })
                    .unwrap_or_default();
            }
            C::AddSource => {
                self.open_keyboard(TextTarget::ExtensionSource, String::new());
                return Vec::new();
            }
            C::SourceEnabled(index) => {
                return (view.sources.get(usize::from(index)))
                    .map(|source| {
                        send(ExtensionCommand::SetSourceEnabled {
                            repository: source.repository.clone(),
                            enabled: !source.enabled,
                        })
                    })
                    .unwrap_or_default();
            }
            C::RemoveSource(index) => {
                if let Some(source) = view.sources.get(usize::from(index))
                    && !source.default
                {
                    let confirm = ExtensionConfirm::RemoveSource(source.repository.clone());
                    self.overlay = Some(Overlay::ExtensionConfirm(confirm));
                }
                return Vec::new();
            }
            _ => {}
        }
        let Some(extension) = open_extension(self) else {
            return Vec::new();
        };
        let key = extension.key.clone();
        match control {
            C::Enable => send(ExtensionCommand::SetEnabled {
                key,
                enabled: !extension.enabled,
            }),
            C::Retry => send(ExtensionCommand::Retry { key }),
            C::Launch if extension.enabled && extension.app.is_some() => {
                send(ExtensionCommand::Launch { key })
            }
            C::Launch => Vec::new(),
            C::Update => send(ExtensionCommand::Update { key }),
            C::Remove => {
                let name = extension.name.clone();
                self.overlay = Some(Overlay::ExtensionConfirm(ExtensionConfirm::Remove {
                    key,
                    name,
                }));
                Vec::new()
            }
            C::Run(index) => (extension.commands.get(usize::from(index)))
                .map(|button| {
                    send(ExtensionCommand::Run {
                        key,
                        command: button.id.clone(),
                    })
                })
                .unwrap_or_default(),
            C::Setting(index) => {
                let Some(setting) = extension.settings.get(usize::from(index)) else {
                    return Vec::new();
                };
                match setting.value {
                    ExtensionSettingValue::Toggle(on) => send(ExtensionCommand::SetSetting {
                        key,
                        setting: setting.key.clone(),
                        value: ExtensionSettingInput::Toggle(!on),
                    }),
                    ExtensionSettingValue::Choice { selected, .. } => {
                        self.open_choice(ChoicePurpose::Extension(index), selected);
                        Vec::new()
                    }
                    ExtensionSettingValue::Number { .. } => Vec::new(),
                }
            }
            C::Step(index, steps) => {
                let Some(setting) = extension.settings.get(usize::from(index)) else {
                    return Vec::new();
                };
                let ExtensionSettingValue::Number { value, min, max } = setting.value else {
                    return Vec::new();
                };
                let next = value
                    .saturating_add(i64::from(steps))
                    .clamp(min, max.max(min));
                if next == value {
                    return Vec::new();
                }
                send(ExtensionCommand::SetSetting {
                    key,
                    setting: setting.key.clone(),
                    value: ExtensionSettingInput::Number(next),
                })
            }
            C::Refresh
            | C::Sources
            | C::Open(_)
            | C::Install(_)
            | C::AddSource
            | C::SourceEnabled(_)
            | C::RemoveSource(_) => Vec::new(),
        }
    }

    /// A pick from the dropdown of choice setting `setting` of the open extension.
    pub(crate) fn choose_extension_setting(&self, setting: u16, index: usize) -> Vec<UiCommand> {
        let Some(extension) = open_extension(self) else {
            return Vec::new();
        };
        match extension.settings.get(usize::from(setting)) {
            Some(ExtensionSetting {
                key,
                value: ExtensionSettingValue::Choice { options, .. },
                ..
            }) if index < options.len() => {
                vec![UiCommand::Extension(ExtensionCommand::SetSetting {
                    key: extension.key.clone(),
                    setting: key.clone(),
                    value: ExtensionSettingInput::Choice(index),
                })]
            }
            _ => Vec::new(),
        }
    }

    /// The answer to the trust question shown; it does not show again until the host asks
    /// about another key.
    pub(crate) fn answer_trust(&mut self, shown: PendingTrust, trust: bool) -> Vec<UiCommand> {
        let repository = shown.repository.clone();
        self.trust_answered = Some(shown);
        vec![UiCommand::Extension(ExtensionCommand::TrustSource {
            repository,
            trust,
        })]
    }

    /// The trust question is level-triggered: it shows on the Extensions page while the host
    /// asks, is not answered and no other modal is open; it closes when the host withdraws it.
    pub(crate) fn show_extension_trust(&mut self) {
        let pending = &self.snapshot.extensions.pending_trust;
        if pending.is_none() {
            self.trust_answered = None;
        }
        if let Some(Overlay::ExtensionTrust(shown)) = &self.overlay
            && pending.as_ref() != Some(shown)
        {
            self.overlay = None;
        }
        let Some(pending) = pending else {
            return;
        };
        if self.trust_answered.as_ref() == Some(pending)
            || self.modal()
            || self.screen() != Screen::Settings(SettingsPage::Extensions)
        {
            return;
        }
        self.overlay = Some(Overlay::ExtensionTrust(pending.clone()));
    }

    /// A new snapshot: a detail page whose installation is gone returns to the list, a
    /// settings section whose extension is gone, off or without settings opens the
    /// Extensions page, and a REMOVE or dropdown for an item that is gone closes.
    pub(crate) fn follow_extensions(&mut self) {
        if self.screen() == Screen::Settings(SettingsPage::ExtensionSettings)
            && section_extension(self).is_none()
        {
            self.extension_section = None;
            self.reset_scroll(ListId::SettingsPage);
            self.navigate(Screen::Settings(SettingsPage::Extensions));
        }
        let view = &self.snapshot.extensions;
        let installed = |key: &str| view.installed.iter().any(|extension| extension.key == key);
        let detail_gone =
            matches!(&self.extensions, ExtensionsPage::Detail(key) if !installed(key));
        let stale = match &self.overlay {
            Some(Overlay::ExtensionConfirm(ExtensionConfirm::Remove { key, .. })) => {
                !installed(key)
            }
            Some(Overlay::ExtensionConfirm(ExtensionConfirm::RemoveSource(repository))) => {
                !(view.sources.iter()).any(|source| source.repository == *repository)
            }
            Some(Overlay::Choice(ChoicePurpose::Extension(index))) => {
                choice(self, *index).is_none()
            }
            _ => false,
        };
        if stale {
            self.overlay = None;
        }
        if detail_gone {
            self.extensions = ExtensionsPage::List;
            self.reset_scroll(ListId::SettingsPage);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::activate;

    const DEFAULT: &str = "tetrad-dev/extensions";
    const OTHER: &str = "night-owl/tools";

    /// Two installed extensions (the first with one setting of each kind and a command), one
    /// catalog entry and the default plus one other repository.
    pub(crate) fn sample() -> ExtensionsView {
        let setting = |key: &str, value| ExtensionSetting {
            key: key.into(),
            label: key.into(),
            value,
        };
        let installed = |id: &str, state| InstalledExtension {
            key: format!("{DEFAULT}:{id}"),
            name: id.into(),
            version: "1.0.0".into(),
            description: "Does things.".into(),
            source: DEFAULT.into(),
            enabled: true,
            state,
            update: None,
            duplicates: Vec::new(),
            commands: Vec::new(),
            settings: Vec::new(),
            status_lines: Vec::new(),
            blocks: Vec::new(),
            log: Vec::new(),
            themes: Vec::new(),
            footswitch_mode: None,
            app: None,
        };
        let mapper = InstalledExtension {
            update: Some("1.1.0".into()),
            commands: vec![ExtensionButton {
                id: "learn".into(),
                label: "Learn".into(),
            }],
            settings: vec![
                setting(
                    "mode",
                    ExtensionSettingValue::Choice {
                        options: vec!["A".into(), "B".into()],
                        selected: 0,
                    },
                ),
                setting("thru", ExtensionSettingValue::Toggle(false)),
                setting(
                    "velocity",
                    ExtensionSettingValue::Number {
                        value: 127,
                        min: 0,
                        max: 127,
                    },
                ),
            ],
            status_lines: vec!["ready".into()],
            blocks: Vec::new(),
            log: vec!["started".into()],
            ..installed("mapper", ExtensionState::Running)
        };
        let source = |repository: &str, default| ExtensionSource {
            repository: repository.into(),
            default,
            enabled: true,
            fingerprint: "3f2a 9c01".into(),
            status: "Up to date".into(),
        };
        ExtensionsView {
            installed: vec![
                mapper,
                installed("clock", ExtensionState::Faulted("boom".into())),
            ],
            available: vec![AvailableExtension {
                source: OTHER.into(),
                id: "strobe".into(),
                name: "Strobe".into(),
                version: "0.1.0".into(),
                description: String::new(),
                duplicates: Vec::new(),
                app: false,
            }],
            sources: vec![source(DEFAULT, true), source(OTHER, false)],
            pending_trust: None,
            status: None,
            busy: false,
        }
    }

    fn page(extensions: ExtensionsView) -> UiState {
        let mut ui = UiState::new();
        ui.set_snapshot(UiSnapshot {
            extensions,
            ..UiSnapshot::default()
        })
        .expect("valid snapshot");
        ui.navigate(Screen::Settings(SettingsPage::Extensions));
        ui
    }

    fn ext(control: ExtensionControl) -> Control {
        Control::Extension(control)
    }

    fn sent(command: ExtensionCommand) -> Vec<UiCommand> {
        vec![UiCommand::Extension(command)]
    }

    fn key(id: &str) -> String {
        format!("{DEFAULT}:{id}")
    }

    fn with_extensions(ui: &mut UiState, extensions: ExtensionsView) {
        let snapshot = UiSnapshot {
            extensions,
            ..ui.snapshot().clone()
        };
        ui.set_snapshot(snapshot).expect("valid snapshot");
    }

    /// The Tetrad menu lists an enabled extension with settings under its name. Its section
    /// changes that extension's settings, and closes to Extensions when it is turned off.
    #[test]
    fn extension_settings_sections_follow_the_tetrad_menu() {
        let mut ui = page(sample());
        // Open from a detail page: the fallback below still opens the list.
        activate(&mut ui, ext(ExtensionControl::Open(0)));
        let sections: Vec<(Control, String)> = (ui.hit_regions().into_iter())
            .filter(|r| {
                matches!(
                    r.control,
                    Control::Setting(SettingControl::ExtensionSettings(_))
                )
            })
            .map(|r| (r.control, r.label))
            .collect();
        // "clock" has no settings, so it has no section.
        let mapper = Control::Setting(SettingControl::ExtensionSettings(0));
        assert_eq!(sections, [(mapper, "mapper".to_owned())]);
        activate(&mut ui, mapper);
        assert_eq!(
            ui.screen(),
            Screen::Settings(SettingsPage::ExtensionSettings)
        );
        assert_eq!(
            SettingsPage::ExtensionSettings.group(),
            SettingsGroup::Tetrad
        );
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Setting(1))),
            sent(ExtensionCommand::SetSetting {
                key: key("mapper"),
                setting: "thru".into(),
                value: ExtensionSettingInput::Toggle(true),
            })
        );
        let mut off = sample();
        off.installed[0].enabled = false;
        with_extensions(&mut ui, off);
        assert_eq!(ui.screen(), Screen::Settings(SettingsPage::Extensions));
        // The Extensions page still opens its list, not a detail page.
        assert!(!own_title(&ui));
    }

    #[test]
    fn list_requests_refresh_and_install_and_names_the_default_source() {
        let mut ui = page(sample());
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Install(0))),
            sent(ExtensionCommand::Install {
                source: OTHER.into(),
                id: "strobe".into(),
            })
        );
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Refresh)),
            sent(ExtensionCommand::Refresh)
        );
        let view = &ui.snapshot().extensions;
        assert_eq!(source_label(view, DEFAULT), "Default");
        assert_eq!(source_label(view, OTHER), OTHER);
        // A running job disables the job buttons.
        with_extensions(
            &mut ui,
            ExtensionsView {
                busy: true,
                ..sample()
            },
        );
        assert!(activate(&mut ui, ext(ExtensionControl::Install(0))).is_empty());
        assert!(activate(&mut ui, ext(ExtensionControl::Refresh)).is_empty());
    }

    #[test]
    fn detail_controls_request_state_settings_and_commands() {
        let mut ui = page(sample());
        activate(&mut ui, ext(ExtensionControl::Open(0)));
        assert!(own_title(&ui));
        let mapper = key("mapper");
        let set = |setting: &str, value| {
            sent(ExtensionCommand::SetSetting {
                key: mapper.clone(),
                setting: setting.into(),
                value,
            })
        };
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Enable)),
            sent(ExtensionCommand::SetEnabled {
                key: mapper.clone(),
                enabled: false,
            })
        );
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Update)),
            sent(ExtensionCommand::Update {
                key: mapper.clone()
            })
        );
        // A choice opens its dropdown; the pick is the request.
        assert!(activate(&mut ui, ext(ExtensionControl::Setting(0))).is_empty());
        assert_eq!(
            activate(&mut ui, Control::Choose(1)),
            set("mode", ExtensionSettingInput::Choice(1))
        );
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Setting(1))),
            set("thru", ExtensionSettingInput::Toggle(true))
        );
        // The number and the commands sit further down the page: scroll, as a user does.
        ui.handle_input(InputEvent::PointerDown { x: 500, y: 440 });
        ui.handle_input(InputEvent::PointerMove { x: 500, y: 240 });
        ui.handle_input(InputEvent::PointerUp { x: 500, y: 240 });
        // Numbers step inside min..max: at the maximum, + asks nothing.
        assert!(activate(&mut ui, ext(ExtensionControl::Step(2, 1))).is_empty());
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Step(2, -1))),
            set("velocity", ExtensionSettingInput::Number(126))
        );
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Run(0))),
            sent(ExtensionCommand::Run {
                key: mapper.clone(),
                command: "learn".into(),
            })
        );
        // A faulted extension offers RETRY; Back returns to the list.
        ui.handle_input(InputEvent::Back);
        assert_eq!(ui.extensions, ExtensionsPage::List);
        activate(&mut ui, ext(ExtensionControl::Open(1)));
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::Retry)),
            sent(ExtensionCommand::Retry { key: key("clock") })
        );
    }

    /// A program shows its button on the extension's page and starts on a tap, only while the
    /// extension is on.
    #[test]
    fn a_program_button_launches_while_the_extension_is_on() {
        let mut view = sample();
        view.installed[1].app = Some("Play DOOM".into());
        let mut ui = page(view.clone());
        activate(&mut ui, ext(ExtensionControl::Open(1)));
        let button = |ui: &UiState| {
            (ui.hit_regions().into_iter())
                .find(|region| region.control == ext(ExtensionControl::Launch))
                .expect("the program button shows")
        };
        let shown = button(&ui);
        assert_eq!((shown.label.as_str(), shown.enabled), ("Play DOOM", true));
        let centre = shown.bounds.center();
        assert_eq!(
            crate::tests::tap(&mut ui, centre.0, centre.1),
            sent(ExtensionCommand::Launch { key: key("clock") })
        );

        view.installed[1].enabled = false;
        with_extensions(&mut ui, view);
        assert!(!button(&ui).enabled);
        assert!(activate(&mut ui, ext(ExtensionControl::Launch)).is_empty());
    }

    #[test]
    fn removal_asks_for_confirmation_first() {
        let mut ui = page(sample());
        activate(&mut ui, ext(ExtensionControl::Open(1)));
        assert!(activate(&mut ui, ext(ExtensionControl::Remove)).is_empty());
        assert!(ui.overlay_open());
        assert!(activate(&mut ui, Control::Dialog(DialogButton::Cancel)).is_empty());
        activate(&mut ui, ext(ExtensionControl::Remove));
        assert_eq!(
            activate(&mut ui, Control::Dialog(DialogButton::Primary)),
            sent(ExtensionCommand::Remove { key: key("clock") })
        );
        // Repositories: the default source has no REMOVE; another asks first.
        ui.handle_input(InputEvent::Back);
        activate(&mut ui, ext(ExtensionControl::Sources));
        let controls: Vec<Control> = ui.hit_regions().iter().map(|r| r.control).collect();
        assert!(!controls.contains(&ext(ExtensionControl::RemoveSource(0))));
        assert!(activate(&mut ui, ext(ExtensionControl::RemoveSource(1))).is_empty());
        assert_eq!(
            activate(&mut ui, Control::Dialog(DialogButton::Primary)),
            sent(ExtensionCommand::RemoveSource {
                repository: OTHER.into(),
            })
        );
        assert_eq!(
            activate(&mut ui, ext(ExtensionControl::SourceEnabled(0))),
            sent(ExtensionCommand::SetSourceEnabled {
                repository: DEFAULT.into(),
                enabled: false,
            })
        );
    }

    #[test]
    fn add_repository_types_owner_and_repo() {
        let mut ui = page(sample());
        activate(&mut ui, ext(ExtensionControl::Sources));
        activate(&mut ui, ext(ExtensionControl::AddSource));
        assert!(ui.keyboard_open());
        ui.handle_input(InputEvent::TextChanged("someone/pedals".into()));
        assert_eq!(
            activate(&mut ui, Control::Key(Key::Done)),
            sent(ExtensionCommand::AddSource {
                repository: "someone/pedals".into(),
            })
        );
    }

    /// The trust question shows while the host asks, answers once, and every close says no.
    #[test]
    fn trust_dialog_answers_the_pending_repository() {
        let pending = PendingTrust {
            repository: OTHER.into(),
            fingerprint: "3f2a9c0177be".into(),
        };
        let asking = ExtensionsView {
            pending_trust: Some(pending.clone()),
            ..sample()
        };
        let mut ui = page(sample());
        with_extensions(&mut ui, asking.clone());
        assert_eq!(ui.overlay, Some(Overlay::ExtensionTrust(pending.clone())));
        assert_eq!(
            activate(&mut ui, Control::Dialog(DialogButton::Primary)),
            sent(ExtensionCommand::TrustSource {
                repository: OTHER.into(),
                trust: true,
            })
        );
        // Answered: it does not open again while the host still asks.
        with_extensions(&mut ui, asking.clone());
        assert!(!ui.overlay_open());
        // A new question opens again; Back answers no.
        with_extensions(&mut ui, sample());
        with_extensions(&mut ui, asking.clone());
        assert!(ui.overlay_open());
        assert_eq!(
            ui.handle_input(InputEvent::Back),
            sent(ExtensionCommand::TrustSource {
                repository: OTHER.into(),
                trust: false,
            })
        );
        // Withdrawn by the host: the dialog closes without an answer.
        with_extensions(&mut ui, sample());
        with_extensions(&mut ui, asking);
        with_extensions(&mut ui, sample());
        assert!(!ui.overlay_open());
        assert_eq!(grouped("3f2a9c0177be"), "3f2a 9c01 77be");
    }

    #[test]
    fn detail_page_closes_when_its_extension_is_gone() {
        let mut ui = page(sample());
        activate(&mut ui, ext(ExtensionControl::Open(1)));
        activate(&mut ui, ext(ExtensionControl::Remove));
        let mut gone = sample();
        gone.installed.truncate(1);
        with_extensions(&mut ui, gone);
        assert_eq!(ui.extensions, ExtensionsPage::List);
        assert!(!ui.overlay_open(), "the REMOVE confirm closes with it");
        assert_eq!(ui.screen(), Screen::Settings(SettingsPage::Extensions));
        assert!(
            ui.hit_regions()
                .iter()
                .any(|r| r.control == ext(ExtensionControl::Open(0)))
        );
    }
}
