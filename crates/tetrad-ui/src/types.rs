//! Bounded identities and geometry shared by every screen.

pub const SCENE_COUNT: usize = 8;
pub const GRID_COLUMNS: usize = 8;
pub const GRID_ROWS: usize = 4;
pub const GRID_CELLS: usize = GRID_ROWS * GRID_COLUMNS;
pub const MAX_PRESETS: usize = 128;
pub const MAX_TEXT_BYTES: usize = 4096;
/// Bound for catalog-style lists (models, setlists, networks, backups, parameters).
pub const MAX_LIST_ITEMS: usize = 1024;
/// Owned bound for the Virtual Device Preset list (user and factory rows of every model).
/// Stock lists them all: `factory.db` (4.1.0) holds 2149 presets of 243 models, at most 30
/// per model, and a model takes at most 32 user presets (`0x1f58a0`). 8192 leaves room for
/// about 6000 user presets; with stock names (at most 24 characters) the list stays under
/// 1 MB. The application shares the list (`Arc`), so a sync does not copy it.
pub const MAX_DEVICE_PRESETS: usize = 8192;
/// Owned bound for the media picker list (the pinned factory catalog has 690
/// IR entities); a picker item index fits in `u16`.
pub const MAX_MEDIA_ITEMS: usize = 8192;
/// Scene/stomp/preset footswitches A–H.
pub const FOOTSWITCH_COUNT: usize = 8;
/// Parameter cells per editor page: the stock 5 × 2 panel, one per encoder
/// (A–H, BANK DOWN, TEMPO; spec `w4-ui-spec-grid` §4). The encoder-to-knob
/// order is the application's choice; stock order is inferred.
pub const KNOB_COUNT: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UiError {
    #[error("scene index must be in 0..8")]
    InvalidScene,
    #[error("grid position must be within four rows and eight columns")]
    InvalidGridPosition,
    #[error("text exceeds the owned 4096-byte limit")]
    TextTooLong,
    #[error("preset list exceeds the owned 128-entry limit")]
    TooManyPresets,
    #[error("preset IDs must be unique")]
    DuplicatePreset,
    #[error("scene draft is stale after an application snapshot change")]
    StaleSceneDraft,
    #[error("a snapshot list exceeds its owned bound")]
    ListTooLong,
    #[error("snapshot list IDs must be unique")]
    DuplicateId,
    #[error("snapshot numbers must be finite and within their documented range")]
    InvalidNumber,
    #[error("knob index must be in 0..6")]
    InvalidKnob,
    #[error("text contains characters or a length the keyboard target does not accept")]
    InvalidName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneIndex(pub(crate) u8);

impl SceneIndex {
    pub fn new(index: u8) -> Result<Self, UiError> {
        if index < SCENE_COUNT as u8 {
            Ok(Self(index))
        } else {
            Err(UiError::InvalidScene)
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }

    pub(crate) fn letter(self) -> char {
        char::from(b'A' + self.0)
    }

    pub(crate) fn all() -> impl Iterator<Item = Self> {
        (0..SCENE_COUNT as u8).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GridPosition {
    pub(crate) row: u8,
    pub(crate) column: u8,
}

impl GridPosition {
    pub fn new(row: u8, column: u8) -> Result<Self, UiError> {
        if row < GRID_ROWS as u8 && column < GRID_COLUMNS as u8 {
            Ok(Self { row, column })
        } else {
            Err(UiError::InvalidGridPosition)
        }
    }

    pub fn row(self) -> u8 {
        self.row
    }

    pub fn column(self) -> u8 {
        self.column
    }

    pub(crate) fn index(self) -> usize {
        usize::from(self.row) * GRID_COLUMNS + usize::from(self.column)
    }

    pub(crate) fn all() -> impl Iterator<Item = Self> {
        (0..GRID_CELLS).map(|index| Self {
            row: (index / GRID_COLUMNS) as u8,
            column: (index % GRID_COLUMNS) as u8,
        })
    }
}

/// The half of the target block a drop lands on. Stock `SlotItem` (`0x5a6fe4`) splits the
/// item at width / 2: the left rect (signal 1, tested first) makes room to the right, the
/// right rect to the left.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DropSide {
    #[default]
    Left,
    Right,
}

/// Grid row 0..4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Row(pub(crate) u8);

impl Row {
    pub fn new(row: u8) -> Result<Self, UiError> {
        if row < GRID_ROWS as u8 {
            Ok(Self(row))
        } else {
            Err(UiError::InvalidGridPosition)
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// Row pair holding split/mix points: rows 1–2 and rows 3–4.
    pub fn pair(self) -> RowPair {
        if self.0 < 2 {
            RowPair::Upper
        } else {
            RowPair::Lower
        }
    }
}

/// Stock `SplitControlPointGrid` exists once per row pair (`0x00595b6c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RowPair {
    Upper,
    Lower,
}

impl RowPair {
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// A Grid element with its own parameter editor besides blocks (manual "INPUT & OUTPUT
/// BLOCKS", "SPLITTER & MIXER BLOCKS").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditorSite {
    /// INPUT GATE CONTROL of a row (hold the input tile).
    InputGate(Row),
    /// LANE OUTPUT CONTROL of a row (hold the output tile): VOLUME, PAN, MUTE, SOLO.
    LaneOutput(Row),
    Splitter(RowPair),
    Mixer(RowPair),
}

impl EditorSite {
    /// The row the editor keeps in view above the docked panel.
    pub fn row(self) -> Row {
        match self {
            Self::InputGate(row) | Self::LaneOutput(row) => row,
            Self::Splitter(pair) | Self::Mixer(pair) => Row(match pair {
                RowPair::Upper => 0,
                RowPair::Lower => 2,
            }),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::InputGate(_) => "Input Gate Control",
            Self::LaneOutput(_) => "Lane Output Control",
            Self::Splitter(_) => "Splitter",
            Self::Mixer(_) => "Mixer",
        }
    }
}

/// Physical footswitches. Upper row A–D, lower row E–H (stock `0x678304`,
/// `0x65dbcc`). Bank and TEMPO switches have no scene/stomp tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Footswitch {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    BankDown,
    BankUp,
    /// HAL stomp element 9. Its hold and double-tap shortcuts are tetrad-core's.
    Tempo,
}

impl Footswitch {
    pub const TILES: [Self; FOOTSWITCH_COUNT] = [
        Self::A,
        Self::B,
        Self::C,
        Self::D,
        Self::E,
        Self::F,
        Self::G,
        Self::H,
    ];

    /// Tile index 0..8 for A–H.
    pub fn tile(self) -> Option<usize> {
        Self::TILES.iter().position(|switch| *switch == self)
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// Parameter knob slot 0..6 on the current editor page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KnobIndex(pub(crate) u8);

impl KnobIndex {
    pub fn new(index: u8) -> Result<Self, UiError> {
        if index < KNOB_COUNT as u8 {
            Ok(Self(index))
        } else {
            Err(UiError::InvalidKnob)
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// Integer half-open display rectangle. Layout coordinates are owned design
/// unless a doc comment cites recovered stock geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, x: i32, y: i32) -> bool {
        let x = i64::from(x);
        let y = i64::from(y);
        x >= i64::from(self.x)
            && y >= i64::from(self.y)
            && x < i64::from(self.x) + i64::from(self.width)
            && y < i64::from(self.y) + i64::from(self.height)
    }

    pub fn right(self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    pub fn center(self) -> (i32, i32) {
        let half = |origin: i32, size: u32| origin.saturating_add_unsigned(size / 2);
        (half(self.x, self.width), half(self.y, self.height))
    }

    /// Shrink by `dx` on the left and right and `dy` on the top and bottom.
    pub fn inset(self, dx: u32, dy: u32) -> Self {
        Self {
            x: self.x.saturating_add_unsigned(dx),
            y: self.y.saturating_add_unsigned(dy),
            width: self.width.saturating_sub(dx.saturating_mul(2)),
            height: self.height.saturating_sub(dy.saturating_mul(2)),
        }
    }

    pub fn grow(self, dx: u32, dy: u32) -> Self {
        Self {
            x: self.x.saturating_sub_unsigned(dx),
            y: self.y.saturating_sub_unsigned(dy),
            width: self.width.saturating_add(dx.saturating_mul(2)),
            height: self.height.saturating_add(dy.saturating_mul(2)),
        }
    }

    pub fn intersect(self, other: Rect) -> Option<Rect> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// Split off `width` pixels from the left; returns (left, rest).
    pub(crate) fn take_left(self, width: u32, gap: u32) -> (Rect, Rect) {
        let width = width.min(self.width);
        (
            Rect { width, ..self },
            Rect {
                x: self.x + (width + gap) as i32,
                width: self.width.saturating_sub(width + gap),
                ..self
            },
        )
    }

    /// Split off `width` pixels from the right; returns (rest, right).
    pub(crate) fn take_right(self, width: u32, gap: u32) -> (Rect, Rect) {
        let width = width.min(self.width);
        (
            Rect {
                width: self.width.saturating_sub(width + gap),
                ..self
            },
            Rect {
                x: self.right() - width as i32,
                width,
                ..self
            },
        )
    }
}
