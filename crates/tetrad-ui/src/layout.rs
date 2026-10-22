//! Display list: the single layout source for drawing, hit testing and
//! semantic activation. Screens append items; overlays start a new layer.

use tetrad_assets::{Icon, IconSize, Palette, Rgb, TextSize, Weight};

use crate::render::{Align, Font};
use crate::{Control, ParameterKind, Rect};

// Stock size/weight pairs (tokens §3). `_BOLD` names map to Medium, the stock
// emphasis face; only XLARGE_BOLD (gig tile names) uses Bold.

/// 14 px Medium: button text and badges.
pub(crate) const SMALL_BOLD: Font = Font::new(TextSize::Px14, Weight::Medium);
/// 14 px Regular: units (BPM, dB) next to values.
pub(crate) const CAPTION: Font = Font::new(TextSize::Px14, Weight::Regular);
/// 16 px Regular, the most common stock size: list rows, menu items, labels.
pub(crate) const BODY: Font = Font::new(TextSize::Px16, Weight::Regular);
pub(crate) const BODY_BOLD: Font = Font::new(TextSize::Px16, Weight::Medium);
/// 18 px Regular: page titles and dropdown values.
pub(crate) const MEDIUM: Font = Font::new(TextSize::Px18, Weight::Regular);
pub(crate) const MEDIUM_BOLD: Font = Font::new(TextSize::Px18, Weight::Medium);
/// 20 px: card headers ("MIDI Settings") and large values.
pub(crate) const LARGE: Font = Font::new(TextSize::Px20, Weight::Regular);
pub(crate) const LARGE_BOLD: Font = Font::new(TextSize::Px20, Weight::Medium);
pub(crate) const XLARGE_BOLD: Font = Font::new(TextSize::Px32, Weight::Bold);
pub(crate) const HUGE_BOLD: Font = Font::new(TextSize::Px70, Weight::Medium);

/// Scrollable lists. Offsets persist per list while the UI lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ListId {
    Choice,
    /// The ⋮ main menu list.
    Menu,
    PickerCategories,
    PickerModels,
    MediaFolders,
    MediaItems,
    /// Virtual Device Presets pane rows.
    PickerPresets,
    /// Expression assignment cells.
    ExpressionCells,
    Setlists,
    PresetResults,
    SettingsMenu,
    SettingsPage,
    /// The Extensions list (kept while a detail page or Repositories shows).
    Extensions,
    /// The Mappings list (kept while a detail page shows).
    Mappings,
    /// The Mappings action menu.
    MappingActions,
    IoPage,
    Wifi,
    Backups,
    /// Device Storage quarantined captures.
    Quarantine,
    Changelog,
    /// Directory bank column (manual §5).
    Banks,
    /// Directory favorites list.
    Favorites,
    /// Directory capture/IR list and the presets List View.
    DirectoryItems,
    /// Rows of the open Directory cloud folder.
    CloudItems,
    /// Global EQ / I/O preset popup rows.
    SettingsPresets,
    /// Cab editor IR table of mic n (`CabsimPanel` `+0x13c`).
    CabIrs(u8),
}

/// The picker drawer veil: 65 % of `#636763`; black becomes `#404340`, white `#999b99`
/// (shot 076).
pub(crate) const GREY_VEIL: (Rgb, u8) = (Rgb(0x63, 0x67, 0x63), 166);
/// The loading overlay (`0x81b74c`): `#323232` at opacity 0.7.
pub(crate) const LOADING_VEIL: (Rgb, u8) = (Rgb(0x32, 0x32, 0x32), 179);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonStyle {
    /// Blue action fill (Save, START CAPTURE).
    Primary,
    /// Raised grey fill (`#404040`, grid spec §1), visible on every surface.
    Secondary,
    /// Black fill on a card (ASSIGN, UNMUTE, CONNECTION DIAGRAM); pure `#000000` on the QC
    /// (UNLINK DEVICE, RESET WI-FI SETTINGS, 2026-10-16).
    Inset,
    /// Transparent until pressed; used for icon buttons.
    Ghost,
    Danger,
    /// Segmented tabs and chips: green outline on a dark green fill when selected.
    Tab {
        selected: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Look {
    Fill(Rgb),
    Panel {
        fill: Rgb,
        radius: f32,
        border: Option<Rgb>,
    },
    Line {
        from: (f32, f32),
        to: (f32, f32),
        width: f32,
        color: Rgb,
    },
    Dot {
        color: Rgb,
    },
    /// Part of a ring inside the bounds (outer edge on them), from `start` clockwise by
    /// `sweep` degrees (0° = +x, y down). The capture spinner (shot 070).
    Arc {
        width: f32,
        start: f32,
        sweep: f32,
        color: Rgb,
    },
    Text {
        font: Font,
        color: Rgb,
        align: Align,
    },
    Paragraph {
        font: Font,
        color: Rgb,
        align: Align,
    },
    Button {
        style: ButtonStyle,
        icon: Option<Icon>,
        /// The label font; `None` picks it by height (18 px from 44 px, else 14 px).
        font: Option<Font>,
    },
    /// Icon-only chrome button (page ✓/✕, header icons).
    IconButton {
        icon: Icon,
        fill: Rgb,
    },
    /// Black field: value left, ▼ right. Opens a list anchored under it.
    Dropdown {
        value: String,
    },
    /// Flat row inside one panel: optional icon, label, trailing value. Selected
    /// rows get green text on a darker band; `divider` draws a 1 px line under
    /// it; `menu` draws ⋮ at the right end.
    ListRow {
        icon: Option<Icon>,
        value: String,
        selected: bool,
        divider: bool,
        menu: bool,
        /// Label font: `BODY` in lists; the ⋮ main menu uses `MEDIUM` (seen on the QC).
        font: Font,
    },
    /// Boxed list row with optional leading hue, trailing value and chevron.
    Row {
        hue: Option<Rgb>,
        icon: Option<Icon>,
        value: String,
        chevron: bool,
        selected: bool,
    },
    Switch {
        on: Option<bool>,
    },
    /// Grid block (shot 013): a `fill` tile (black, or tinted for a plugin block) with a 2 px
    /// `hue` outline and the white category `icon` (the label only when there is no icon). No
    /// hue is an empty slot, drawn only while pressed or as a drop target.
    Block {
        hue: Option<Rgb>,
        fill: Rgb,
        icon: Option<Icon>,
        bypassed: bool,
        selected: bool,
        drop_target: bool,
        ghost: bool,
        /// A Capture 2.0 block (model 14001): the stock inner ring.
        capture_two: bool,
    },
    /// Grid row I/O tile: the label on two lines split at its first space
    /// ("In 1" → "In" / "1"); an empty label draws "+". `faded`: a path B port that the
    /// pair's junction replaces, the whole tile at opacity 0.2.
    Port {
        faded: bool,
    },
    /// A bare icon centred in its bounds.
    Glyph {
        icon: Icon,
        size: IconSize,
        color: Rgb,
    },
    /// Parameter cell: label top-left, value bottom-left, knob (or switch,
    /// dropdown) on the right with its value arc in `color` (the block colour, or the
    /// expression green or scene white of an assigned knob).
    Knob {
        value: String,
        kind: ParameterKind,
        color: Rgb,
        focused: bool,
    },
    Progress {
        fraction: f32,
        color: Rgb,
    },
    Meter {
        level: f32,
    },
    Signal {
        bars: u8,
    },
    Key {
        icon: Option<Icon>,
        accent: bool,
    },
    DropZone {
        active: bool,
    },
    /// Invisible touch area that highlights while pressed.
    Area,
    /// Invisible touch area around a popover; a tap there closes it.
    Dismiss,
    /// Dim everything drawn before it (modal layer start).
    Scrim,
    /// Stock translucent veil over its bounds: `color` blended at `alpha`. The picker
    /// drawer's light grey turns the black Grid `#404440` (shots 076, 090).
    Veil {
        color: Rgb,
        alpha: u8,
    },
    /// Stock opacity over a card (a bypassed block's parameter cells): each pixel still in
    /// `card` becomes `faded`; any other keeps `keep`/255 of itself over `faded`, truncated as
    /// Qt blends into its RGB565 surface. So content reads as painted at that opacity over the
    /// already faded card ([`crate::render::Canvas::fade`]).
    Fade {
        card: Rgb,
        faded: Rgb,
        keep: u8,
    },
    Toast,
    /// Stock action tile (Looper X, manual §4): title top-left, footswitch letter top-right,
    /// a centred icon or short mark.
    ActionTile {
        letter: char,
        title: String,
        mark: TileMark,
        hue: Rgb,
        active: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum TileMark {
    Icon(Icon),
    Text(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub bounds: Rect,
    /// Visible window inside a scroll viewport.
    pub clip: Option<Rect>,
    pub control: Option<Control>,
    pub enabled: bool,
    pub label: String,
    pub look: Look,
}

impl Item {
    /// Bounds that can receive touches: clipped to the viewport, if any.
    pub fn visible(&self) -> Option<Rect> {
        match self.clip {
            Some(clip) => self.bounds.intersect(clip),
            None => Some(self.bounds),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scroller {
    pub id: ListId,
    pub viewport: Rect,
    pub content: u32,
    pub layer: usize,
}

impl Scroller {
    pub fn max_offset(self) -> i32 {
        self.content.saturating_sub(self.viewport.height) as i32
    }
}

#[derive(Debug)]
pub(crate) struct Layout {
    /// The colours every builder draws with: the active theme, or stock on Recovery and the
    /// boot menu.
    pub palette: Palette,
    pub items: Vec<Item>,
    /// First item of the topmost (interactive) layer.
    pub layer: usize,
    pub scrollers: Vec<Scroller>,
    clip: Option<Rect>,
}

impl Layout {
    pub fn new(palette: Palette) -> Self {
        Self {
            palette,
            items: Vec::new(),
            layer: 0,
            scrollers: Vec::new(),
            clip: None,
        }
    }

    pub fn push(&mut self, bounds: Rect, label: impl Into<String>, look: Look) {
        self.add(bounds, None, true, label.into(), look);
    }

    pub fn control(
        &mut self,
        bounds: Rect,
        control: Control,
        enabled: bool,
        label: impl Into<String>,
        look: Look,
    ) {
        self.add(bounds, Some(control), enabled, label.into(), look);
    }

    fn add(
        &mut self,
        bounds: Rect,
        control: Option<Control>,
        enabled: bool,
        label: String,
        look: Look,
    ) {
        if let Some(clip) = self.clip
            && bounds.intersect(clip).is_none()
        {
            return;
        }
        self.items.push(Item {
            bounds,
            clip: self.clip,
            control,
            enabled,
            label,
            look,
        });
    }

    pub fn text(
        &mut self,
        bounds: Rect,
        text: impl Into<String>,
        font: Font,
        color: Rgb,
        align: Align,
    ) {
        self.push(bounds, text, Look::Text { font, color, align });
    }

    pub fn paragraph(&mut self, bounds: Rect, text: impl Into<String>, font: Font, color: Rgb) {
        self.push(
            bounds,
            text,
            Look::Paragraph {
                font,
                color,
                align: Align::Left,
            },
        );
    }

    pub fn button(
        &mut self,
        bounds: Rect,
        control: Control,
        enabled: bool,
        label: impl Into<String>,
        style: ButtonStyle,
    ) {
        self.control(
            bounds,
            control,
            enabled,
            label,
            Look::Button {
                style,
                icon: None,
                font: None,
            },
        );
    }

    /// [`Self::button`] with its label in `font` (stock Settings buttons are 16 px at 45 px).
    pub fn button_in(
        &mut self,
        bounds: Rect,
        control: Control,
        enabled: bool,
        label: impl Into<String>,
        style: ButtonStyle,
        font: Font,
    ) {
        self.control(
            bounds,
            control,
            enabled,
            label,
            Look::Button {
                style,
                icon: None,
                font: Some(font),
            },
        );
    }

    /// Icon-only ghost button; `label` names it for automation and tests.
    pub fn icon_button(
        &mut self,
        bounds: Rect,
        control: Control,
        enabled: bool,
        label: impl Into<String>,
        icon: Icon,
    ) {
        self.control(
            bounds,
            control,
            enabled,
            label,
            Look::Button {
                style: ButtonStyle::Ghost,
                icon: Some(icon),
                font: None,
            },
        );
    }

    pub fn panel(&mut self, bounds: Rect, fill: Rgb, radius: f32, border: Option<Rgb>) {
        self.push(
            bounds,
            "",
            Look::Panel {
                fill,
                radius,
                border,
            },
        );
    }

    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Rgb) {
        let x0 = from.0.min(to.0).floor() as i32 - width.ceil() as i32;
        let y0 = from.1.min(to.1).floor() as i32 - width.ceil() as i32;
        let x1 = from.0.max(to.0).ceil() as i32 + width.ceil() as i32;
        let y1 = from.1.max(to.1).ceil() as i32 + width.ceil() as i32;
        self.push(
            Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32),
            "",
            Look::Line {
                from,
                to,
                width,
                color,
            },
        );
    }

    /// Start a modal layer: earlier items stay drawn but stop receiving input.
    pub fn overlay(&mut self) {
        self.push(
            Rect::new(0, 0, crate::WIDTH, crate::HEIGHT),
            "",
            Look::Scrim,
        );
        self.layer = self.items.len();
    }

    /// Start a modal layer under a stock [`Look::Veil`] instead of the dark scrim.
    pub fn veil(&mut self, (color, alpha): (Rgb, u8)) {
        let look = Look::Veil { color, alpha };
        self.push(Rect::new(0, 0, crate::WIDTH, crate::HEIGHT), "", look);
        self.layer = self.items.len();
    }

    /// Start a popover layer (anchored dropdown list or ⋮ menu) over `panel`.
    /// Earlier items stay visible and undimmed but stop receiving input; a tap
    /// outside `panel` sends [`Control::Back`]. Bands thinner than a touch
    /// target are left inert.
    pub fn popover(&mut self, panel: Rect) {
        self.layer = self.items.len();
        let (w, h) = (crate::WIDTH as i32, crate::HEIGHT as i32);
        let (top, bottom) = (panel.y.clamp(0, h), panel.bottom().clamp(0, h));
        let (left, right) = (panel.x.clamp(0, w), panel.right().clamp(0, w));
        for (x0, y0, x1, y1) in [
            (0, 0, w, top),
            (0, bottom, w, h),
            (0, top, left, bottom),
            (right, top, w, bottom),
        ] {
            if x1 - x0 >= 40 && y1 - y0 >= 40 {
                let band = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
                self.control(band, Control::Back, true, "CLOSE", Look::Dismiss);
            }
        }
    }

    /// Bounds of the visible item that carries `control`: the trigger a
    /// popover anchors to.
    pub fn anchor(&self, control: Control) -> Option<Rect> {
        self.items
            .iter()
            .rev()
            .filter(|item| item.control == Some(control))
            .find_map(Item::visible)
    }

    /// Lay out scrolled content inside `viewport`. `build` receives the top y
    /// of the content (already shifted by the clamped offset).
    pub fn scroll(
        &mut self,
        id: ListId,
        viewport: Rect,
        content: u32,
        offset: i32,
        build: impl FnOnce(&mut Self, i32),
    ) {
        let scroller = Scroller {
            id,
            viewport,
            content,
            layer: self.layer,
        };
        let offset = offset.clamp(0, scroller.max_offset());
        self.scrollers.push(scroller);
        self.clipped(viewport, |layout| build(layout, viewport.y - offset));
    }

    /// Restrict items built by `build` to `viewport` (inside any outer clip).
    pub fn clipped(&mut self, viewport: Rect, build: impl FnOnce(&mut Self)) {
        let outer = self.clip;
        self.clip = Some(match outer {
            Some(clip) => clip.intersect(viewport).unwrap_or(Rect::new(0, 0, 0, 0)),
            None => viewport,
        });
        build(self);
        self.clip = outer;
    }

    /// Scroller of the top layer containing a point.
    pub fn scroller_at(&self, x: i32, y: i32) -> Option<Scroller> {
        self.scrollers
            .iter()
            .rev()
            .find(|scroller| scroller.layer == self.layer && scroller.viewport.contains(x, y))
            .copied()
    }
}
