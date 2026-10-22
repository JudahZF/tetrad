//! Neural Capture V1 wizard (manual §6, spec `w4-ui-spec-directory` §6): connection diagram
//! pages 01–05, calibration settings with the two-tab panel, the process checklist, the A/B
//! test and the save page with capture type and preferred instrument. Strings are stock
//! (`0xa36efc`…`0xa37954`, types `0x74b5f0`, instruments `0x74b518`). The rear-panel strip,
//! the capture-type glyphs and the checklist marks are owned vectors (stock uses bitmaps).

use tetrad_assets::{Icon, IconSize, Rgb, block};

use crate::draw::{TAB_ACTIVE, TAB_IDLE};
use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, LARGE_BOLD, Layout, Look, MEDIUM, MEDIUM_BOLD,
};
use crate::render::{Align, HEIGHT, WIDTH};
use crate::settings::{InputJack, OutputJack};
use crate::state::{Overlay, TextTarget, stomp_name};
use crate::widgets::{CARD, CLOSE_BUTTON, PAGE, page_header, panel_cells};
use crate::*;

/// UI-only capture state: the calibration tab, the Cab panel and the save page draft.
#[derive(Debug, Clone, Default)]
pub(crate) struct CaptureUi {
    pub tab: CalibrationTab,
    /// The panel shows the capture Cab block's parameters (tile tapped).
    pub cab: bool,
    /// Set once the capture is named; the save page shows while it exists.
    pub draft: Option<CaptureDraft>,
    /// Directory ⋮ "Edit Name" of a stored capture: the save page over the Directory.
    pub edit: Option<CaptureEdit>,
}

/// The capture save page in edit mode (`onEditClicked` `0x78e49c` → `NCSaveDialog` with
/// `+0x158` set, `0x68dfb4`): the stored name, type (`0x68df6c`, record `+0x60`) and
/// instrument (`0x68df84`, `+0x5c`). `None`: the snapshot does not give the stored value; it is
/// kept unless a tile or chip is tapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureEdit {
    pub id: String,
    pub name: String,
    pub device: Option<CaptureDevice>,
    pub instrument: Option<CaptureInstrument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaptureDraft {
    pub name: String,
    pub device: CaptureDevice,
    pub instrument: CaptureInstrument,
}

/// Manual §6 connection diagram pages: heading and one instruction line.
pub(super) const DIAGRAM: [(&str, &str); 5] = [
    (
        "01 REFERENCE INSTRUMENT",
        "Connect your instrument to INPUT 1.",
    ),
    (
        "02 MONITORING DEVICES",
        "Connect your headphones to HP OUTPUT or your monitor speaker to OUTPUTS 1L/2R or 3L/4R.",
    ),
    (
        "03 TARGET DEVICE INPUT",
        "Connect the Quad Cortex's CAPTURE OUT to the target device's input.",
    ),
    (
        "04 TARGET DEVICE OUTPUT",
        "Connect the target device's output (pedal, microphone or load box) to INPUT 2/CAPTURE INPUT.",
    ),
    (
        "05 REVIEW",
        "Once everything is connected correctly, tap NEXT to proceed with the calibration settings.",
    ),
];

/// Process checklist (shot 070) and the stock stage texts (`0xa37048`…`0xa37098`).
const PROCESS: [&str; 4] = [
    "Calibration",
    "Recording Signals",
    "Sanity Check",
    "Training",
];

/// Calibration instructions (`0xa36efc`, `0xa36f40`, `0xa36f78`, %1 = Quad Cortex).
const INSTRUCTIONS: &str = "• Please verify your Quad Cortex is properly connected to the target device.\n• Reduce levels if any of the meters detect clipping.\n• The IN 2 GROUND LIFT can mitigate noise caused by ground loops between Quad Cortex and the target device.";

/// IN 1 LEVEL help (`0xa374a0`); IN 2 is the manual §6 text.
const IN1_HELP: &str = "Adjust this level as you would in The Grid to properly feed the target device while playing your instrument. This setting is for monitoring purposes only and does not affect the CAPTURE OUT training signal level.";
const IN2_HELP: &str = "Adjusts the input gain coming from the target device signal. For optimal results, aim for a peak level around -12 dB.";

/// Meter floor (the idle reading in shot 095) and the IN 2 peak target (manual §6).
const METER_FLOOR_DB: f32 = -40.0;
const TARGET_DB: f32 = -12.0;

/// Spec §6.2 rects.
const PANEL_RECT: Rect = Rect::new(8, 257, 784, 216);
const CAPTURE_DIVIDER: Rgb = Rgb(0x10, 0x10, 0x10);
/// Save page name field and selected instrument chip (shot 030).
const NAME_FIELD: Rgb = Rgb(0x20, 0x24, 0x20);
const CHIP_SELECTED: Rgb = Rgb(0xf8, 0xfc, 0xf8);
/// Stock 095 has no divider at x=635 in the second row.
const CAPTURE_HIDDEN: &[(u32, u32)] = &[(1, 4)];
/// Stock 098 A/B divider: 1 px at x=321 on the 157 px column pitch.
const AB_DIVIDER_X: i32 = PANEL_RECT.x + 2 * 157 - 1;
/// Stock 098 CORTEX and REFERENCE radio tops; each radio is 37 px tall.
const AB_RADIO_Y: [i32; 2] = [293, 404];
/// Stock 098 unselected radio fill.
const AB_RADIO_IDLE: Rgb = Rgb(0x18, 0x20, 0x18);
const TABS: [(CalibrationTab, &str, i32); 2] = [
    (CalibrationTab::Levels, "1", 640),
    (CalibrationTab::Grounding, "2", 710),
];
/// The A/B 🎧 LEVEL knob is panel cell 1 (shot 098), so encoder 1 turns it.
const AB_LEVEL: usize = 1;

/// One cell of the calibration panel. Cell n is encoder n ([`panel_cells`] order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    /// Level knob; `sub` is the grey line under the label, `help` the (i) text.
    Level {
        label: &'static str,
        sub: &'static str,
        id: SettingId,
        help: Option<&'static str>,
    },
    /// Vertical switch. The top option is `On` or choice 1 (`Mic`).
    Switch {
        label: &'static str,
        id: SettingId,
        options: [&'static str; 2],
    },
    Meter {
        label: &'static str,
        input: usize,
    },
    Empty,
}

/// Stock "       LEVEL" (`0xa373e0`): the spaces leave room for the 🎧 icon.
const HP_LEVEL_LABEL: &str = "       LEVEL";
const HP_LEVEL: Cell = Cell::Level {
    label: HP_LEVEL_LABEL,
    sub: "",
    id: SettingId::HeadphoneLevel,
    help: None,
};
const METERS: [Cell; 2] = [
    Cell::Meter {
        label: "IN 1 LEVEL",
        input: 0,
    },
    Cell::Meter {
        label: "IN 2 LEVEL",
        input: 1,
    },
];
const ON_OFF: [&str; 2] = ["On", "Off"];

/// Tab 1 (shot 095) and tab 2 (manual §6 list; layout I). Row 2 stays on both tabs (I).
/// The OUT 1 and OUT 2 lifts both use the paired OUT 1/2 setting (manual §3: paired
/// outputs share GROUND LIFT; the unpaired per-jack lift has no setting yet).
fn calibration_cells(tab: CalibrationTab) -> [Cell; KNOB_COUNT] {
    let switch = |label, id, options| Cell::Switch { label, id, options };
    let row = match tab {
        CalibrationTab::Levels => [
            Cell::Level {
                label: "IN 1 LEVEL",
                sub: "INST",
                id: SettingId::InputLevel(InputJack::In1),
                help: Some(IN1_HELP),
            },
            Cell::Level {
                label: "IN 2 LEVEL",
                sub: "DEVICE",
                id: SettingId::InputLevel(InputJack::In2),
                help: Some(IN2_HELP),
            },
            switch(
                "IN 1 TYPE",
                SettingId::InputType(InputJack::In1),
                ["Mic", "Instrument"],
            ),
            switch(
                "IN 2 TYPE",
                SettingId::InputType(InputJack::In2),
                ["Mic", "Instrument"],
            ),
        ],
        CalibrationTab::Grounding => [
            switch(
                "IN 2 GROUND LIFT",
                SettingId::InputGroundLift(InputJack::In2),
                ON_OFF,
            ),
            switch("IN 2 PHANTOM", SettingId::Phantom(InputJack::In2), ON_OFF),
            switch(
                "OUT 1 GND LIFT",
                SettingId::OutputGroundLift(OutputJack::Out12),
                ON_OFF,
            ),
            switch(
                "OUT 2 GND LIFT",
                SettingId::OutputGroundLift(OutputJack::Out12),
                ON_OFF,
            ),
        ],
    };
    [
        row[0],
        row[1],
        row[2],
        row[3],
        Cell::Empty,
        METERS[0],
        METERS[1],
        HP_LEVEL,
        Cell::Empty,
        Cell::Empty,
    ]
}

/// Checklist index of the running stage; `Refine` follows training.
fn process_index(step: CaptureStep) -> usize {
    match step {
        CaptureStep::RecordingSignals => 1,
        CaptureStep::SanityCheck => 2,
        _ => 3,
    }
}

/// Meter reading in dB for a 0..=1 linear level, floored at the idle "-40.0 dB" (I: the
/// application reports linear peaks).
fn meter_db(level: f32) -> f32 {
    if level <= 0.0 {
        METER_FLOOR_DB
    } else {
        (20.0 * level.log10()).clamp(METER_FLOOR_DB, 0.0)
    }
}

/// Bar position of a dB reading on the meter scale.
fn meter_fraction(db: f32) -> f32 {
    (db - METER_FLOOR_DB) / -METER_FLOOR_DB
}

/// Block category colour of a capture type (tokens §2.1, sub-type override at `0x2eb380`).
fn device_hue(device: CaptureDevice) -> Rgb {
    match device as u8 {
        1 | 2 => block::AMP,
        3 | 4 => block::CAB,
        5 | 6 | 8 => block::DRIVE,
        7 => block::DYNAMICS,
        _ => block::NEUTRAL,
    }
}

impl UiState {
    /// Encoder `knob` on the capture screen: the Cab parameter while its panel shows, else
    /// the calibration cell or the A/B 🎧 LEVEL.
    pub(crate) fn capture_knob(&mut self, knob: KnobIndex, delta: i32) -> Vec<UiCommand> {
        let index = usize::from(knob.get());
        if let Some(param) = self
            .cab_shown()
            .then(|| self.snapshot.capture.cab.get(index))
        {
            return param
                .map(|param| {
                    vec![UiCommand::Capture {
                        request: CaptureRequest::CabNudge {
                            parameter: param.index,
                            steps: delta,
                        },
                    }]
                })
                .unwrap_or_default();
        }
        let id = match self.snapshot.capture.step {
            CaptureStep::Calibration => match calibration_cells(self.capture.tab).get(index) {
                Some(Cell::Level { id, .. } | Cell::Switch { id, .. }) => Some(*id),
                _ => None,
            },
            CaptureStep::Audition if self.capture.draft.is_none() && index == AB_LEVEL => {
                Some(SettingId::HeadphoneLevel)
            }
            _ => None,
        };
        id.and_then(|id| {
            id.step(self.snapshot.setting(id), delta)
                .map(|value| vec![UiCommand::ChangeSetting { id, value }])
        })
        .unwrap_or_default()
    }

    /// The Cab panel replaces the calibration and A/B cells while the tile is selected.
    fn cab_shown(&self) -> bool {
        self.capture.cab
            && !self.snapshot.capture.cab.is_empty()
            && matches!(
                self.snapshot.capture.step,
                CaptureStep::Calibration | CaptureStep::Audition
            )
    }

    pub(crate) fn capture_control(&mut self, control: CaptureControl) -> Vec<UiCommand> {
        if let Some(edit) = &mut self.capture.edit {
            match control {
                CaptureControl::CloseSave => self.capture.edit = None,
                CaptureControl::Name => {
                    let name = edit.name.clone();
                    self.open_keyboard(TextTarget::CaptureName, name);
                }
                CaptureControl::Device(device) => edit.device = Some(device),
                CaptureControl::Instrument(instrument) => edit.instrument = Some(instrument),
                CaptureControl::Save => return self.save_capture_edit(),
                CaptureControl::Tab(_) | CaptureControl::Cab => {}
            }
            return Vec::new();
        }
        let draft = &mut self.capture.draft;
        match control {
            CaptureControl::Tab(tab) => {
                self.capture.tab = tab;
                self.capture.cab = false;
            }
            CaptureControl::Cab => self.capture.cab = !self.capture.cab,
            CaptureControl::CloseSave => *draft = None,
            CaptureControl::Name => {
                let name = draft.as_ref().map(|d| d.name.clone()).unwrap_or_default();
                self.open_keyboard(TextTarget::CaptureName, name);
            }
            CaptureControl::Device(device) => {
                if let Some(draft) = draft {
                    draft.device = device;
                }
            }
            CaptureControl::Instrument(instrument) => {
                if let Some(draft) = draft {
                    draft.instrument = instrument;
                }
            }
            CaptureControl::Save => {
                if let Some(draft) = draft {
                    return vec![UiCommand::SaveCapture {
                        name: draft.name.clone(),
                        device: draft.device,
                        instrument: draft.instrument,
                    }];
                }
            }
        }
        Vec::new()
    }

    /// Save in edit mode (`metadataSaveSlot` `0x6901c8`): the name simplified with an ASCII
    /// lower-case first letter made upper case (`islower`/`toupper`); under 4 characters the
    /// keyboard opens again; the name of another capture (`0x6f6238`, key differs) shows the
    /// stock message; else the record changes (`0x68f134`). I: the name check reads the
    /// library captures the Directory lists.
    fn save_capture_edit(&mut self) -> Vec<UiCommand> {
        let Some(edit) = &self.capture.edit else {
            return Vec::new();
        };
        let name = stomp_name(&edit.name);
        if name.encode_utf16().count() < 4 {
            self.open_keyboard(TextTarget::CaptureName, name);
            return Vec::new();
        }
        let taken = (self.snapshot.media.iter()).any(|entry| {
            entry.folder == MediaFolder::MyCaptures && entry.value != edit.id && entry.name == name
        });
        if taken {
            self.overlay = Some(Overlay::Notice {
                title: "Name already exists".into(),
                body: format!("{name} already exists. Please choose a different name."),
                button: "OK",
            });
            return Vec::new();
        }
        let Some(edit) = self.capture.edit.take() else {
            return Vec::new();
        };
        vec![UiCommand::Directory(DirectoryRequest::EditCapture {
            id: edit.id,
            name,
            device: edit.device,
            instrument: edit.instrument,
        })]
    }

    /// Keyboard result for "Name your Neural Capture": opens the save page or renames it.
    pub(crate) fn capture_named(&mut self, name: String) -> Vec<UiCommand> {
        let name = name.trim().to_owned();
        if let Some(edit) = &mut self.capture.edit {
            edit.name = name;
            return Vec::new();
        }
        match &mut self.capture.draft {
            Some(draft) => draft.name = name,
            None => {
                self.capture.draft = Some(CaptureDraft {
                    name,
                    device: CaptureDevice::default(),
                    instrument: CaptureInstrument::default(),
                });
            }
        }
        Vec::new()
    }
}

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let c = &ui.snapshot.capture;
    if let (Some(draft), CaptureStep::Audition | CaptureStep::Saving) = (&ui.capture.draft, c.step)
    {
        let page = SavePage {
            name: &draft.name,
            device: Some(draft.device),
            instrument: Some(draft.instrument),
            saving: c.step == CaptureStep::Saving,
            edit: false,
        };
        save_page(l, &page);
        return;
    }
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.tone(PAGE)));
    // ✕ asks first while a capture is open; idle and saved close at once.
    let request = if matches!(c.step, CaptureStep::Idle | CaptureStep::Saved) {
        CaptureRequest::Exit
    } else {
        CaptureRequest::Abort
    };
    let free = page_header(l, "Neural Capture", Control::Capture(request), Icon::Close);
    if !c.message.is_empty() {
        l.text(free, c.message.clone(), BODY_BOLD, p.warning, Align::Left);
    }
    match c.step {
        CaptureStep::Idle => intro(l),
        CaptureStep::ConnectionDiagram => diagram(l, c.diagram_step),
        CaptureStep::Calibration => calibration(ui, l),
        CaptureStep::RecordingSignals
        | CaptureStep::SanityCheck
        | CaptureStep::Training
        | CaptureStep::Refine => process(l, c),
        CaptureStep::Audition | CaptureStep::Saving => ab_test(ui, l),
        CaptureStep::Saved => saved(l),
        CaptureStep::Failed => failed(l),
    }
}

/// Black secondary button (CONNECTION DIAGRAM, BACK TO CALIBRATION): canvas fill (`#000000` on stock).
fn black_button(l: &mut Layout, bounds: Rect, control: Control, label: &str, enabled: bool) {
    let p = l.palette;
    l.panel(bounds, p.canvas, 8.0, None);
    l.button(bounds, control, enabled, label, ButtonStyle::Ghost);
}

/// Bottom-right primary action of the full-page steps.
fn next_button(l: &mut Layout, control: Control, label: &str) {
    l.button(
        Rect::new(572, 412, 220, 44),
        control,
        true,
        label,
        ButtonStyle::Primary,
    );
}

/// Owned glyph drawn from looks: an icon in a box without fill.
fn glyph(l: &mut Layout, bounds: Rect, icon: Icon) {
    l.push(
        bounds,
        "",
        Look::Button {
            style: ButtonStyle::Ghost,
            icon: Some(icon),
            font: None,
        },
    );
}

/// Ring of `diameter` px and `width` px stroke on a `fill` background.
fn ring(l: &mut Layout, (cx, cy): (i32, i32), diameter: u32, width: u32, color: Rgb, fill: Rgb) {
    let dot = |l: &mut Layout, d: u32, color| {
        let r = d as i32 / 2;
        l.push(Rect::new(cx - r, cy - r, d, d), "", Look::Dot { color });
    };
    dot(l, diameter, color);
    dot(l, diameter - 2 * width, fill);
}

/// Rounded outline of `width` px on a `fill` background.
fn outline(l: &mut Layout, b: Rect, radius: f32, width: u32, color: Rgb, fill: Rgb) {
    l.panel(b, color, radius, None);
    l.panel(
        b.inset(width, width),
        fill,
        (radius - width as f32).max(0.0),
        None,
    );
}

/// Manual §6 "Tube Amplifier Warning" as the entry page (I: stock entry layout).
fn intro(l: &mut Layout) {
    let p = l.palette;
    l.paragraph(
        Rect::new(20, 66, 760, 50),
        "Capture the sound of an amplifier, cabinet or overdrive pedal.",
        MEDIUM,
        p.text,
    );
    let card = Rect::new(8, 130, 784, 132);
    l.panel(card, p.panel, 8.0, None);
    l.text(
        Rect::new(card.x + 16, card.y + 14, 736, 24),
        "Tube Amplifier Warning",
        MEDIUM_BOLD,
        p.warning,
        Align::Left,
    );
    l.paragraph(
        Rect::new(card.x + 16, card.y + 48, 752, 80),
        "Connecting the speaker output of a tube amplifier directly to the Quad Cortex can cause serious damage to both devices. Use the D.I. output of the captured amplifier while keeping it connected to a speaker cabinet, or connect a reactive load box between the amplifier's speaker output and the Quad Cortex.",
        BODY,
        p.text,
    );
    next_button(l, Control::Capture(CaptureRequest::Begin), "NEXT");
}

fn diagram(l: &mut Layout, step: u8) {
    let p = l.palette;
    let index = usize::from(step).min(DIAGRAM.len() - 1);
    let (title, text) = DIAGRAM[index];
    l.text(
        Rect::new(20, 58, 760, 28),
        title,
        LARGE_BOLD,
        p.text,
        Align::Left,
    );
    l.paragraph(Rect::new(20, 92, 760, 50), text, MEDIUM, p.text);
    rear_panel(l, index);
    if index == 3 {
        l.paragraph(
            Rect::new(20, 354, 760, 48),
            "Tube amplifiers: never connect the speaker output directly. Use a reactive load box.",
            BODY_BOLD,
            p.warning,
        );
    }
    if index > 0 {
        black_button(
            l,
            Rect::new(8, 412, 220, 44),
            Control::Capture(CaptureRequest::Back),
            "BACK",
            true,
        );
    }
    next_button(l, Control::Capture(CaptureRequest::Continue), "NEXT");
}

/// The I/O page map moved down onto the diagram strip.
const REAR_SHIFT: i32 = 102;

/// The rear panel of stock 067 on a dark strip (the I/O page map, `io::rear_panel`): the
/// jacks the page names are green.
fn rear_panel(l: &mut Layout, page: usize) {
    let p = l.palette;
    l.panel(Rect::new(8, 148, 784, 200), p.surface, 8.0, None);
    super::io::rear_panel(l, REAR_SHIFT, |port| names(port, page));
}

/// The diagram pages that name a rear connector.
fn names(port: &super::io::Port, page: usize) -> bool {
    let pages: &[usize] = match (port.tab, port.label) {
        (Some(IoTab::Out12 | IoTab::Out34 | IoTab::Headphones), _) => &[1, 4],
        (_, "CAPTURE\nOUT") => &[2, 4],
        (Some(IoTab::Input2), _) => &[3, 4],
        (Some(IoTab::Input1), _) => &[0, 4],
        _ => &[],
    };
    pages.contains(&page)
}

/// The capture preset's Cab block between the action buttons (shots 095, 098): a dim
/// tile with the wire drawn through it. With a cab from the application it is a control:
/// a tap shows its parameters in the panel, selected with the cab colour.
fn capture_block(ui: &UiState, l: &mut Layout, tile: Rect) {
    let p = l.palette;
    let has_cab = !ui.snapshot.capture.cab.is_empty();
    // Shot 095 samples: border #201c38, glyph #383c38, wire #c0c0c0.
    let border = if ui.cab_shown() {
        block::CAB
    } else {
        block::CAB.mix(p.canvas, 200)
    };
    let dim = p.section.mix(p.canvas, 32);
    outline(l, tile, 12.0, 2, border, p.canvas);
    let (cx, cy) = tile.center();
    ring(l, (cx, cy), 28, 2, dim, p.canvas);
    ring(l, (cx, cy), 10, 3, dim, p.canvas);
    for (dx, dy) in [(-16, -16), (16, -16), (-16, 16), (16, 16)] {
        l.push(
            Rect::new(cx + dx - 2, cy + dy - 2, 4, 4),
            "",
            Look::Dot { color: dim },
        );
    }
    l.push(
        Rect::new(tile.x - 12, cy, tile.width + 24, 1),
        "",
        Look::Fill(p.text.mix(p.canvas, 63)),
    );
    if has_cab {
        l.control(
            tile,
            Control::CaptureUi(CaptureControl::Cab),
            true,
            "CAB",
            Look::Area,
        );
    }
}

/// One Cab parameter cell: a knob with the application's value text, or the text alone
/// for a media parameter (IR names change in the Grid media picker only).
fn cab_cell(l: &mut Layout, b: Rect, param: &ParameterView) {
    let p = l.palette;
    if let ParameterKind::Media(_) = param.kind {
        cell_label(l, b, &param.label, true);
        l.text(
            Rect::new(b.x + 10, b.y + 40, b.width.saturating_sub(20), 24),
            param.value.clone(),
            BODY,
            p.muted,
            Align::Left,
        );
        return;
    }
    l.push(
        b,
        param.label.clone(),
        Look::Knob {
            value: param.value.clone(),
            kind: param.kind,
            color: p.accent,
            focused: false,
        },
    );
}

/// The panel cells: the Cab parameters (encoder n turns parameter n) or `cells`.
fn panel(ui: &UiState, l: &mut Layout, cells: impl FnOnce(&mut Layout)) {
    if !ui.cab_shown() {
        return cells(l);
    }
    let p = l.palette;
    let bounds = panel_cells(
        l,
        PANEL_RECT,
        2,
        p.tone(PAGE),
        p.tone(CAPTURE_DIVIDER),
        CAPTURE_HIDDEN,
    );
    for (param, b) in ui.snapshot.capture.cab.iter().zip(bounds) {
        cab_cell(l, b, param);
    }
}

/// Calibration settings (shot 095).
fn calibration(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.paragraph(Rect::new(20, 62, 680, 100), INSTRUCTIONS, MEDIUM, p.text);
    black_button(
        l,
        Rect::new(8, 185, 220, 44),
        Control::Capture(CaptureRequest::Back),
        "CONNECTION DIAGRAM",
        true,
    );
    l.button(
        Rect::new(252, 185, 220, 44),
        Control::Capture(CaptureRequest::StartCapture),
        true,
        "START CAPTURE",
        ButtonStyle::Primary,
    );
    capture_block(ui, l, Rect::new(522, 172, 66, 66));
    let current = ui.capture.tab;
    for (tab, label, x) in TABS {
        let selected = tab == current && !ui.cab_shown();
        // The selected tab takes the card colour and runs into the card; the other is
        // `#181c18` (shot 095).
        let height = if selected { 60 } else { 46 };
        let bounds = Rect::new(x, 206, 66, height);
        l.panel(
            bounds,
            p.tone(if selected { CARD } else { TAB_IDLE }),
            8.0,
            None,
        );
        l.control(
            Rect::new(x, 206, 66, 46),
            Control::CaptureUi(CaptureControl::Tab(tab)),
            true,
            label,
            Look::Area,
        );
        l.text(
            Rect::new(x, 206, 66, 46),
            label,
            MEDIUM,
            if selected { p.text } else { p.muted },
            Align::Center,
        );
    }
    panel(ui, l, |l| {
        let cells = panel_cells(
            l,
            PANEL_RECT,
            2,
            p.tone(PAGE),
            p.tone(CAPTURE_DIVIDER),
            CAPTURE_HIDDEN,
        );
        for (cell, bounds) in calibration_cells(current).into_iter().zip(cells) {
            panel_cell(ui, l, bounds, cell);
        }
    });
}

fn cell_label(l: &mut Layout, b: Rect, label: &str, enabled: bool) {
    let p = l.palette;
    l.text(
        Rect::new(b.x + 10, b.y + 6, b.width.saturating_sub(12), 28),
        label,
        BODY,
        if enabled { p.text } else { p.faint },
        Align::Left,
    );
}

fn panel_cell(ui: &UiState, l: &mut Layout, b: Rect, cell: Cell) {
    let p = l.palette;
    let s = &ui.snapshot;
    match cell {
        Cell::Level {
            label,
            sub,
            id,
            help,
        } => {
            // The (i) key owns the label row, so touch targets stay disjoint.
            let touch = if help.is_some() {
                Rect::new(b.x, b.y + 42, b.width, b.height - 42)
            } else {
                b
            };
            level_knob(ui, l, b, touch, label, id);
            if label == HP_LEVEL_LABEL {
                glyph(l, Rect::new(b.x + 8, b.y + 8, 24, 24), Icon::Headphones);
            }
            if !sub.is_empty() {
                l.text(
                    Rect::new(b.x + 10, b.y + 30, 80, 18),
                    sub,
                    CAPTION,
                    p.faint,
                    Align::Left,
                );
            }
            // Stock 095: the R32 ⓘ, centred 26 px left of the cell edge.
            if let Some(help) = help {
                super::settings::info_button(l, (b.right() - 26, b.y + 22), label, help);
            }
        }
        Cell::Switch { label, id, options } => {
            let value = s.setting(id);
            let top = match value {
                Some(SettingValue::Bool(on)) => Some(*on),
                Some(SettingValue::Choice(index)) => Some(*index == 1),
                _ => None,
            };
            let enabled = top.is_some();
            l.control(
                b,
                Control::Setting(SettingControl::Step(id, 1)),
                enabled,
                label,
                Look::Area,
            );
            cell_label(l, b, label, enabled);
            // Stock switch (shot 095): black slot, green ring at the active end.
            let track = Rect::new(b.x + 16, b.y + 42, 22, 46);
            l.panel(track, p.canvas, 11.0, None);
            for (row, option) in options.into_iter().enumerate() {
                let y = track.y + row as i32 * 23;
                let on = top == Some(row == 0);
                l.text(
                    Rect::new(track.right() + 16, y, b.width.saturating_sub(60), 23),
                    option,
                    BODY,
                    if on { p.text } else { p.faint },
                    Align::Left,
                );
                if on {
                    ring(l, (track.x + 11, y + 11), 19, 3, p.accent, p.canvas);
                }
            }
        }
        Cell::Meter { label, input } => {
            cell_label(l, b, label, true);
            let db = meter_db(s.capture.input_levels[input]);
            let number = format!("{db:.1}");
            let value = Rect::new(b.x + 10, b.y + 40, 70, 24);
            l.text(value, number.clone(), BODY, p.text, Align::Left);
            l.text(
                Rect::new(
                    value.x + BODY.width(&number).ceil() as i32 + 4,
                    value.y,
                    30,
                    24,
                ),
                "dB",
                CAPTION,
                p.muted,
                Align::Left,
            );
            let bar = Rect::new(b.x + 10, b.y + 76, b.width - 20, 14);
            l.push(
                bar,
                "",
                Look::Meter {
                    level: meter_fraction(db),
                },
            );
            let tick = bar.x + (bar.width as f32 * meter_fraction(TARGET_DB)) as i32;
            l.push(
                Rect::new(tick, bar.y, 2, bar.height),
                "",
                Look::Fill(p.panel),
            );
        }
        Cell::Empty => {}
    }
}

/// Stock knob cell for a dB setting drawn over `b`; the arc spans the setting bounds.
/// `touch` is the part of the cell that takes taps.
fn level_knob(ui: &UiState, l: &mut Layout, b: Rect, touch: Rect, label: &str, id: SettingId) {
    let p = l.palette;
    let value = ui.snapshot.setting(id);
    let position = match value {
        Some(SettingValue::Number(raw)) => id.knob_position(*raw),
        _ => 0.0,
    };
    l.control(
        touch,
        Control::Setting(SettingControl::Open(id)),
        value.is_some(),
        label,
        Look::Area,
    );
    l.push(
        b,
        label,
        Look::Knob {
            value: id.format(value),
            kind: ParameterKind::Continuous { position },
            color: if value.is_some() { p.accent } else { p.faint },
            focused: false,
        },
    );
}

/// Owned checklist marks (shot 070): green ✓ when done, white → on the running stage, an
/// empty box when pending (I: pending not shown in the shot).
fn check_mark(l: &mut Layout, b: Rect, done: bool, current: bool) {
    let p = l.palette;
    l.panel(b, p.canvas, 1.0, None);
    let (x, y) = (b.x as f32, b.y as f32);
    if done {
        l.line((x + 3.5, y + 9.5), (x + 7.5, y + 13.5), 2.0, p.accent);
        l.line((x + 7.5, y + 13.5), (x + 14.5, y + 4.5), 2.0, p.accent);
    } else if current {
        l.line((x + 3.5, y + 9.0), (x + 14.0, y + 9.0), 2.0, p.text);
        l.line((x + 10.0, y + 5.5), (x + 14.0, y + 9.0), 2.0, p.text);
        l.line((x + 10.0, y + 12.5), (x + 14.0, y + 9.0), 2.0, p.text);
    }
}

/// Static spinner (shot 070): a 3 px `#404040` ring of radius 10 with a white arc from the
/// top, about 100° (one frame of the stock animation).
fn spinner(l: &mut Layout, (cx, cy): (i32, i32)) {
    let p = l.palette;
    let bounds = Rect::new(cx - 11, cy - 11, 23, 23);
    for (start, sweep, color) in [(0.0, 360.0, p.section), (265.0, 100.0, p.text)] {
        l.push(
            bounds,
            "",
            Look::Arc {
                width: 3.0,
                start,
                sweep,
                color,
            },
        );
    }
}

/// Capture process (shot 070): checklist left, stage text and progress right.
fn process(l: &mut Layout, c: &CaptureView) {
    let p = l.palette;
    let list = Rect::new(8, 60, 256, 412);
    l.panel(list, p.panel, 8.0, None);
    let current = process_index(c.step);
    for (index, name) in PROCESS.into_iter().enumerate() {
        let y = list.y + 28 + index as i32 * 50;
        check_mark(
            l,
            Rect::new(16, y - 9, 18, 18),
            index < current,
            index == current,
        );
        l.text(
            Rect::new(44, y - 12, list.width - 44, 24),
            name,
            MEDIUM,
            p.text,
            Align::Left,
        );
    }
    let pane = Rect::new(272, 60, 520, 412);
    l.panel(pane, p.panel, 8.0, None);
    let (title, text) = match c.step {
        CaptureStep::RecordingSignals => (
            "Neural Capture in progress",
            "Recording a bunch of signals that will be used for modeling.",
        ),
        CaptureStep::SanityCheck => ("Neural Capture in progress", "Sanity check."),
        CaptureStep::Refine => (
            "Refine",
            "Play a few chords to further refine the model, then press STOP when finished",
        ),
        _ => (
            "Neural Capture in progress",
            "The core of Neural Capture. Training a neural network to emulate the sound of your favorite device.",
        ),
    };
    l.text(
        Rect::new(288, 78, 488, 24),
        title,
        MEDIUM_BOLD,
        p.text,
        Align::Left,
    );
    l.paragraph(Rect::new(288, 124, 488, 60), text, MEDIUM, p.text);
    if let Some(fraction) = c.progress {
        l.text(
            Rect::new(288, 208, 120, 24),
            format!("{:.0}%", fraction * 100.0),
            MEDIUM,
            p.text,
            Align::Left,
        );
        let track = Rect::new(288, 244, 488, 16);
        l.push(track, "", Look::Fill(p.background));
        let filled = (track.width as f32 * fraction.clamp(0.0, 1.0)).round() as u32;
        l.push(
            Rect::new(track.x, track.y, filled, track.height),
            "",
            Look::Fill(p.accent),
        );
    }
    spinner(l, (764, 220));
    if c.step == CaptureStep::Refine {
        l.button(
            Rect::new(288, pane.bottom() - 60, 220, 44),
            Control::Capture(CaptureRequest::Stop),
            true,
            "STOP",
            ButtonStyle::Primary,
        );
    }
}

/// A/B test (shot 098). While the device exports (`Saving`) the buttons wait.
fn ab_test(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let c = &ui.snapshot.capture;
    let idle = c.step == CaptureStep::Audition;
    l.paragraph(
        Rect::new(20, 62, 680, 60),
        "Your Neural Capture is ready. Switch between the reference and Quad Cortex using the buttons below.",
        MEDIUM,
        p.text,
    );
    black_button(
        l,
        Rect::new(8, 180, 220, 44),
        Control::Capture(CaptureRequest::BackToCalibration),
        "BACK TO CALIBRATION",
        idle,
    );
    l.button(
        Rect::new(244, 180, 220, 44),
        Control::SaveCapture,
        idle,
        "SAVE",
        ButtonStyle::Primary,
    );
    capture_block(ui, l, Rect::new(514, 166, 66, 66));
    panel(ui, l, |l| {
        ab_panel(ui, l, "CORTEX", idle, |source| {
            Control::Capture(CaptureRequest::Audition(source))
        });
    });
}

/// The A/B panel (shot 098; NC2 `0x164` at the same rect): the CAPTURE (Cortex) /
/// REFERENCE radio pair, selected green on black, and the 🎧 LEVEL knob.
fn ab_panel(
    ui: &UiState,
    l: &mut Layout,
    capture_label: &str,
    enabled: bool,
    control: impl Fn(AuditionSource) -> Control,
) {
    let p = l.palette;
    l.panel(PANEL_RECT, p.tone(CARD), 8.0, None);
    let half = PANEL_RECT.height as i32 / 2;
    l.push(
        Rect::new(PANEL_RECT.x, PANEL_RECT.y + half - 1, PANEL_RECT.width, 2),
        "",
        Look::Fill(p.tone(PAGE)),
    );
    let cell = PANEL_RECT.width as i32 / 5;
    l.push(
        Rect::new(AB_DIVIDER_X, PANEL_RECT.y + 15, 1, 78),
        "",
        Look::Fill(p.tone(PAGE)),
    );
    for (row, (label, source)) in [
        (capture_label, AuditionSource::Capture),
        ("REFERENCE", AuditionSource::Reference),
    ]
    .into_iter()
    .enumerate()
    {
        let selected = ui.snapshot.capture.audition == source;
        let b = Rect::new(24, AB_RADIO_Y[row], 125, 37);
        l.panel(
            b,
            p.tone(if selected { PAGE } else { AB_RADIO_IDLE }),
            4.0,
            None,
        );
        l.control(b, control(source), enabled, label, Look::Area);
        l.text(
            b,
            label,
            BODY,
            if selected { p.accent } else { p.faint },
            Align::Center,
        );
    }
    let knob = Rect::new(PANEL_RECT.x + cell, PANEL_RECT.y, cell as u32, half as u32);
    panel_cell(ui, l, knob, HP_LEVEL);
}

/// `NeuralCapture2Dialog` (ctor `0x3dadc8`): a full-screen `#121212` modal over every
/// screen while Cortex Control runs Neural Capture 2. Its modes come from the NC2 state
/// (slot `0x3dcc2c`). The `neuralcapture/C2BG.png` backdrop is not drawn (stock bitmap).
pub(super) fn nc2(ui: &UiState, l: &mut Layout, dialog: Nc2Dialog) {
    let p = l.palette;
    l.overlay();
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.background));
    let close = Control::Nc2(Nc2Request::Close);
    match dialog {
        Nc2Dialog::InProgress | Nc2Dialog::LostConnection => {
            // Modes 0 and 2 (`0x3d7714`, `0x3d7a4c`): NC icon 32 × 32 at (384, 168), the
            // 18 px status line 310 × 23 at (245, 216), ABORT THE PROCESS 211 × 45 (black,
            // radius 8) at (294, 268), or at (294, 316) under the red lost-connection line
            // (459 × 33 at (170, 254), `#ff2727`).
            glyph(l, Rect::new(384, 168, 32, 32), Icon::NeuralCapture);
            l.text(
                Rect::new(245, 216, 310, 23),
                "Neural Capture Version 2 in progress.",
                MEDIUM,
                p.text,
                Align::Center,
            );
            let abort_y = if dialog == Nc2Dialog::LostConnection {
                l.text(
                    Rect::new(170, 254, 459, 33),
                    "Lost connection to Cortex Control, please reconnect!",
                    MEDIUM,
                    p.danger,
                    Align::Center,
                );
                316
            } else {
                268
            };
            black_button(
                l,
                Rect::new(294, abort_y, 211, 45),
                close,
                "ABORT THE PROCESS",
                true,
            );
        }
        Nc2Dialog::Saved => {
            // Mode 3 (`0x3d7dc0`): the status line "Neural Capture Saved." (310 × 23 at
            // (245, 216), centred) and the ABORT button renamed EXIT at (294, 268); EXIT is
            // `doneButtonSlot`, so it closes NC2. The A/B panel and buttons hide. The NC icon
            // follows `0x3dd274` (not traced; drawn as in mode 0, I).
            glyph(l, Rect::new(384, 168, 32, 32), Icon::NeuralCapture);
            l.text(
                Rect::new(245, 216, 310, 23),
                "Neural Capture Saved.",
                MEDIUM,
                p.text,
                Align::Center,
            );
            black_button(l, Rect::new(294, 268, 211, 45), close, "EXIT", true);
        }
        Nc2Dialog::Ready => {
            // Mode 1 (`0x3dca14`): "A/B Test" header with the ✕ (done button, close
            // icon), the text 509 × 52 at (16, 62), RESTART CAPTURE (black) and SAVE NEURAL
            // CAPTURE (`#1a3af8`), both 220 × 45 at y 181, then the A/B panel.
            page_header(l, "A/B Test", close, Icon::Close);
            l.paragraph(
                Rect::new(16, 62, 509, 52),
                "Your Neural Capture is ready. Use the buttons below to toggle between the CAPTURE and the REFERENCE signal.",
                MEDIUM,
                p.text,
            );
            black_button(
                l,
                Rect::new(8, 181, 220, 45),
                Control::Nc2(Nc2Request::Restart),
                "RESTART CAPTURE",
                true,
            );
            l.button(
                Rect::new(252, 181, 220, 45),
                Control::Nc2(Nc2Request::Save),
                true,
                "SAVE NEURAL CAPTURE",
                ButtonStyle::Primary,
            );
            ab_panel(ui, l, "CAPTURE", true, |source| {
                Control::Nc2(Nc2Request::Audition(source))
            });
        }
    }
}

/// What the save page shows: a new capture's draft, or a stored capture in edit mode.
struct SavePage<'a> {
    name: &'a str,
    device: Option<CaptureDevice>,
    instrument: Option<CaptureInstrument>,
    saving: bool,
    /// Edit mode keeps the record in its folder, so the destination tile is not drawn (I).
    edit: bool,
}

/// The save page in edit mode over the Directory (re-r31 §2).
pub(super) fn edit_page(l: &mut Layout, edit: &CaptureEdit) {
    let page = SavePage {
        name: &edit.name,
        device: edit.device,
        instrument: edit.instrument,
        saving: false,
        edit: true,
    };
    save_page(l, &page);
}

/// Save page (shot 030): header chips, then capture type and preferred instrument.
/// Shot 030: page `#101010`; folder pill and unselected chips `#181c18`, name field
/// `#202420`, selected chip `#f8fcf8`.
fn save_page(l: &mut Layout, page: &SavePage<'_>) {
    let p = l.palette;
    let saving = page.saving;
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.tone(PAGE)));
    let divider = |l: &mut Layout, x: i32| {
        l.push(Rect::new(x, 18, 2, 24), "", Look::Fill(p.section));
    };
    l.control(
        Rect::new(8, 8, 98, 44),
        Control::CaptureUi(CaptureControl::CloseSave),
        !saving,
        "CLOSE",
        Look::IconButton {
            icon: Icon::Close,
            fill: p.panel,
        },
    );
    divider(l, 114);
    // Only the "My Captures" root is a destination here (I: folder picker not built).
    if !page.edit {
        destination(l);
    }
    divider(l, 296);
    let shown = if page.name.is_empty() {
        "Name"
    } else {
        page.name
    };
    let width = (BODY.width(shown).ceil() as u32 + 26).clamp(74, 280);
    let name = Rect::new(306, 8, width, 44);
    l.panel(name, p.tone(NAME_FIELD), 8.0, None);
    l.control(
        name,
        Control::CaptureUi(CaptureControl::Name),
        !saving,
        "NAME",
        Look::Area,
    );
    l.text(name, shown, BODY, p.text, Align::Center);
    divider(l, name.right() + 9);
    // Metadata page indicator: green outline, the filled document glyph (shot 030).
    let meta = Rect::new(name.right() + 20, 8, 66, 44);
    outline(l, meta, 8.0, 1, p.accent, p.tone(TAB_ACTIVE));
    let (cx, cy) = meta.center();
    l.push(
        Rect::new(cx - 12, cy - 12, 24, 24),
        "",
        Look::Glyph {
            icon: Icon::Document,
            size: IconSize::Medium,
            color: p.accent,
        },
    );
    l.control(
        CLOSE_BUTTON,
        Control::CaptureUi(CaptureControl::Save),
        !saving,
        "SAVE",
        Look::IconButton {
            icon: Icon::Save,
            fill: p.action,
        },
    );
    l.text(
        Rect::new(157, 90, 400, 20),
        "TYPE OF CAPTURE",
        BODY,
        p.muted,
        Align::Left,
    );
    l.text(
        Rect::new(157, 117, 400, 26),
        page.device.map_or("", CaptureDevice::label),
        MEDIUM,
        p.text,
        Align::Left,
    );
    for (index, device) in CaptureDevice::V1.into_iter().enumerate() {
        let tile = Rect::new(158 + (index as i32 * 417) / 5, 160, 66, 66);
        device_tile(l, tile, device, Some(device) == page.device, !saving);
    }
    l.text(
        Rect::new(157, 278, 400, 20),
        "PREFERRED INSTRUMENT",
        BODY,
        p.muted,
        Align::Left,
    );
    for (index, instrument) in CaptureInstrument::CHOICES.into_iter().enumerate() {
        let chip = Rect::new(
            157 + (index as i32 % 3) * 165,
            315 + (index as i32 / 3) * 44,
            157,
            36,
        );
        let selected = Some(instrument) == page.instrument;
        let fill = p.tone(if selected { CHIP_SELECTED } else { TAB_IDLE });
        l.panel(chip, fill, 4.0, None);
        l.control(
            chip,
            Control::CaptureUi(CaptureControl::Instrument(instrument)),
            !saving,
            instrument.label(),
            Look::Area,
        );
        l.text(
            chip,
            instrument.label(),
            BODY,
            if selected { p.canvas } else { p.muted },
            Align::Center,
        );
    }
}

/// The "My Captures" destination pill of the save page header.
fn destination(l: &mut Layout) {
    let p = l.palette;
    let folder = Rect::new(124, 8, 163, 44);
    l.panel(folder, p.tone(TAB_IDLE), 8.0, None);
    l.push(
        Rect::new(folder.x + 10, folder.y + 10, 24, 24),
        "",
        Look::Glyph {
            icon: Icon::FolderFilled,
            size: IconSize::Medium,
            color: p.text,
        },
    );
    l.text(
        Rect::new(folder.x + 48, folder.y, folder.width - 52, folder.height),
        "My Captures",
        BODY,
        p.text,
        Align::Left,
    );
}

/// Capture type tile: category-colour outline (brighter and thicker when selected), an
/// owned glyph (white when selected, else grey `#848484`, shot 030), and the 20 px
/// capture badge at the lower left on typed tiles.
fn device_tile(l: &mut Layout, tile: Rect, device: CaptureDevice, selected: bool, enabled: bool) {
    let p = l.palette;
    let hue = device_hue(device);
    if selected {
        outline(l, tile, 12.0, 3, hue, p.canvas);
    } else {
        outline(l, tile, 12.0, 2, hue.mix(p.canvas, 128), p.canvas);
    }
    l.control(
        tile.inset(3, 3),
        Control::CaptureUi(CaptureControl::Device(device)),
        enabled,
        device.label(),
        Look::Area,
    );
    let ink = if selected {
        p.text
    } else {
        p.text.mix(p.canvas, 123)
    };
    let (cx, cy) = tile.center();
    let box_ = |w: u32, h: u32| Rect::new(cx - w as i32 / 2, cy - h as i32 / 2, w, h);
    let line = |l: &mut Layout, b: Rect, y: i32| {
        l.push(Rect::new(b.x + 3, y, b.width - 6, 2), "", Look::Fill(ink));
    };
    let dot = |l: &mut Layout, (x, y): (i32, i32), size: u32| {
        let half = size as i32 / 2;
        l.push(
            Rect::new(x - half, y - half, size, size),
            "",
            Look::Dot { color: ink },
        );
    };
    match device {
        // Stock first tile and badge: the striped Neural Capture disc, 32 px, no fill.
        CaptureDevice::Default => l.push(
            box_(32, 32),
            "",
            Look::Glyph {
                icon: Icon::NeuralCapture,
                size: IconSize::Large,
                color: ink,
            },
        ),
        CaptureDevice::Amp => {
            let head = box_(32, 18);
            outline(l, head, 3.0, 2, ink, p.canvas);
            line(l, head, cy - 1);
        }
        CaptureDevice::ComboAmp => {
            let combo = box_(30, 30);
            outline(l, combo, 3.0, 2, ink, p.canvas);
            line(l, combo, combo.y + 8);
            dot(l, (combo.right() - 7, combo.y + 5), 4);
        }
        CaptureDevice::AmpAndCab => {
            let cab = Rect::new(cx - 16, cy - 10, 32, 24);
            l.push(Rect::new(cx - 7, cab.y - 4, 14, 2), "", Look::Fill(ink));
            outline(l, cab, 3.0, 2, ink, p.canvas);
            ring(l, (cx, cy + 2), 14, 2, ink, p.canvas);
        }
        // The Grid Cab glyph: a 26 px speaker ring, an 8 px cone, four corner screws.
        CaptureDevice::Cab => {
            ring(l, (cx, cy), 26, 2, ink, p.canvas);
            dot(l, (cx, cy), 8);
            for (dx, dy) in [(-13, -13), (13, -13), (-13, 13), (13, 13)] {
                dot(l, (cx + dx, cy + dy), 4);
            }
        }
        // Stompbox: 22 × 30 body with side jacks, three LEDs, a knob, the footswitch.
        _ => {
            let body = Rect::new(cx - 11, cy - 16, 22, 30);
            for x in [body.x - 2, body.right()] {
                l.push(Rect::new(x, cy - 9, 2, 7), "", Look::Fill(ink));
            }
            outline(l, body, 3.0, 2, ink, p.canvas);
            for dx in [-5, 0, 5] {
                dot(l, (cx + dx, cy - 10), 3);
            }
            dot(l, (cx, cy - 1), 9);
            l.push(Rect::new(cx, cy - 5, 1, 4), "", Look::Fill(p.canvas));
            dot(l, (cx, cy + 8), 3);
        }
    }
    if device != CaptureDevice::Default {
        let badge = Rect::new(tile.x - 4, tile.bottom() - 22, 24, 24);
        l.push(badge, "", Look::Dot { color: p.canvas });
        l.push(
            badge.inset(2, 2),
            "",
            Look::Glyph {
                icon: Icon::NeuralCapture,
                size: IconSize::Compact,
                color: p.text,
            },
        );
    }
}

fn saved(l: &mut Layout) {
    let p = l.palette;
    l.paragraph(
        Rect::new(20, 66, 760, 50),
        "Neural Capture Saved.",
        MEDIUM,
        p.text,
    );
    next_button(l, Control::Capture(CaptureRequest::Exit), "EXIT");
}

fn failed(l: &mut Layout) {
    let p = l.palette;
    l.paragraph(
        Rect::new(20, 66, 760, 50),
        "There was an error during the Sanity Check stage",
        MEDIUM,
        p.text,
    );
    black_button(
        l,
        Rect::new(8, 412, 260, 44),
        Control::Capture(CaptureRequest::StartFromScratch),
        "START FROM SCRATCH",
        true,
    );
    next_button(
        l,
        Control::Capture(CaptureRequest::BackToCalibration),
        "BACK TO CALIBRATION",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manual §6 checklist: stages before the running one are done; refine follows
    /// training; the diagram has the five manual pages in order.
    #[test]
    fn process_checklist_and_diagram_pages_follow_the_manual() {
        assert_eq!(process_index(CaptureStep::RecordingSignals), 1);
        assert_eq!(process_index(CaptureStep::SanityCheck), 2);
        assert_eq!(process_index(CaptureStep::Training), 3);
        assert_eq!(process_index(CaptureStep::Refine), 3);
        assert!(DIAGRAM[0].0.starts_with("01") && DIAGRAM[4].0 == "05 REVIEW");
    }

    /// Stock sub-type colours (`0x2eb380`): amps red, cabs violet, pedals orange.
    #[test]
    fn capture_types_use_the_stock_category_colours() {
        assert_eq!(device_hue(CaptureDevice::ComboAmp), block::AMP);
        assert_eq!(device_hue(CaptureDevice::Cab), block::CAB);
        assert_eq!(device_hue(CaptureDevice::Pedal), block::DRIVE);
        assert_eq!(device_hue(CaptureDevice::Compressor), block::DYNAMICS);
        assert_eq!(device_hue(CaptureDevice::Default), block::NEUTRAL);
    }

    fn capture_ui(step: CaptureStep) -> UiState {
        let mut ui = UiState::new();
        let mut snapshot = ui.snapshot().clone();
        snapshot.capture.step = step;
        snapshot.settings.insert(
            SettingId::InputGroundLift(InputJack::In2),
            SettingValue::Bool(false),
        );
        ui.set_snapshot(snapshot).unwrap();
        ui.navigate(Screen::Capture);
        ui
    }

    fn press(ui: &mut UiState, control: Control) -> Vec<UiCommand> {
        ui.handle_input(InputEvent::Activate(control))
    }

    /// Manual §6 save: SAVE names the capture, the save page sets type and instrument, and
    /// the floppy sends all three (stock `saveCaptureSignal`); ✕ leaves without a command.
    #[test]
    fn save_page_sends_name_type_and_instrument() {
        let mut ui = capture_ui(CaptureStep::Audition);
        assert!(press(&mut ui, Control::SaveCapture).is_empty());
        for ch in "lead".chars() {
            press(&mut ui, Control::Key(Key::Char(ch)));
        }
        assert!(press(&mut ui, Control::Key(Key::Done)).is_empty());
        let device = Control::CaptureUi(CaptureControl::Device(CaptureDevice::Cab));
        assert!(ui.hit_regions().iter().any(|r| r.control == device));
        press(&mut ui, device);
        press(
            &mut ui,
            Control::CaptureUi(CaptureControl::Instrument(CaptureInstrument::Bass)),
        );
        assert_eq!(
            press(&mut ui, Control::CaptureUi(CaptureControl::Save)),
            vec![UiCommand::SaveCapture {
                name: "lead".into(),
                device: CaptureDevice::Cab,
                instrument: CaptureInstrument::Bass,
            }]
        );
        assert!(press(&mut ui, Control::CaptureUi(CaptureControl::CloseSave)).is_empty());
        assert!(ui.hit_regions().iter().all(|r| r.control != device));
    }

    /// Encoders follow the open calibration tab: cell 0 is IN 1 LEVEL on tab 1 and
    /// IN 2 GROUND LIFT on tab 2.
    #[test]
    fn encoders_follow_the_calibration_tab() {
        let mut ui = capture_ui(CaptureStep::Calibration);
        let knob = KnobIndex::new(0).unwrap();
        // IN 1 LEVEL has no value in this snapshot: nothing to step.
        assert!(
            ui.handle_input(InputEvent::Knob { knob, delta: 1 })
                .is_empty()
        );
        press(
            &mut ui,
            Control::CaptureUi(CaptureControl::Tab(CalibrationTab::Grounding)),
        );
        assert_eq!(
            ui.handle_input(InputEvent::Knob { knob, delta: 1 }),
            vec![UiCommand::ChangeSetting {
                id: SettingId::InputGroundLift(InputJack::In2),
                value: SettingValue::Bool(true),
            }]
        );
    }

    /// The capture Cab tile is a control only with a cab from the application; a tap shows
    /// its parameters and encoder n steps cab parameter n; a tab returns to calibration.
    #[test]
    fn cab_tile_turns_the_encoders_onto_the_capture_cab() {
        let mut ui = capture_ui(CaptureStep::Calibration);
        let cab = Control::CaptureUi(CaptureControl::Cab);
        assert!(ui.hit_regions().iter().all(|r| r.control != cab));
        let mut snapshot = ui.snapshot().clone();
        snapshot.capture.cab = vec![ParameterView {
            entry: None,
            scene: false,
            index: 3,
            label: "Mic Distance".into(),
            value: "1.0 in".into(),
            kind: ParameterKind::Continuous { position: 0.2 },
        }];
        ui.set_snapshot(snapshot).unwrap();
        assert!(ui.hit_regions().iter().any(|r| r.control == cab));
        press(&mut ui, cab);
        let knob = KnobIndex::new(0).unwrap();
        assert_eq!(
            ui.handle_input(InputEvent::Knob { knob, delta: -2 }),
            vec![UiCommand::Capture {
                request: CaptureRequest::CabNudge {
                    parameter: 3,
                    steps: -2,
                },
            }]
        );
        press(
            &mut ui,
            Control::CaptureUi(CaptureControl::Tab(CalibrationTab::Grounding)),
        );
        assert!(matches!(
            ui.handle_input(InputEvent::Knob { knob, delta: 1 })[..],
            [UiCommand::ChangeSetting { .. }]
        ));
    }

    /// `0x3cea8c`: a held Back opens "Unsaved capture"; OK answers `Discard(true)`, and the
    /// confirm closes when the application no longer holds the decision.
    #[test]
    fn unsaved_capture_confirm_answers_the_held_back() {
        let mut ui = capture_ui(CaptureStep::Audition);
        let mut snapshot = ui.snapshot().clone();
        snapshot.capture.unsaved_prompt = true;
        ui.set_snapshot(snapshot.clone()).unwrap();
        assert_eq!(
            press(&mut ui, Control::Dialog(DialogButton::Primary)),
            vec![UiCommand::Capture {
                request: CaptureRequest::Discard(true),
            }]
        );
        snapshot.capture.unsaved_prompt = false;
        ui.set_snapshot(snapshot.clone()).unwrap();
        snapshot.capture.unsaved_prompt = true;
        ui.set_snapshot(snapshot.clone()).unwrap();
        snapshot.capture.unsaved_prompt = false;
        ui.set_snapshot(snapshot).unwrap();
        assert!(press(&mut ui, Control::Dialog(DialogButton::Primary)).is_empty());
    }

    /// The meter reads the idle "-40.0 dB" at silence and full scale at 0 dBFS.
    #[test]
    fn meter_scale_floors_at_minus_forty() {
        assert_eq!(meter_db(0.0), METER_FLOOR_DB);
        assert_eq!(meter_db(1e-6), METER_FLOOR_DB);
        assert!((meter_db(0.5) + 6.02).abs() < 0.01);
        assert_eq!(meter_fraction(meter_db(1.0)), 1.0);
    }
}
