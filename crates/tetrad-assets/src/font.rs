//! IBM Plex Sans, the stock ZenUI family (tokens §3). The three TTFs are the
//! unmodified upstream files in `fonts/ibm-plex-sans/` (OFL 1.1, see its
//! README), never device bytes. fontdue rasterizes each glyph once per size
//! and weight into an 8-bit coverage mask that is then shared. Pen advances
//! and GPOS pair kerns follow the stock Qt placement per size
//! ([`TextSize::stock_advance`], [`TextSize::stock_kern`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};

use fontdue::{Font, FontSettings};
use ttf_parser::GlyphId;
use ttf_parser::gpos::{PairAdjustment, PositioningSubtable};

use crate::raster::Mask;

/// Stock pixel sizes (`QFont::setPixelSize` sites). The set is closed so
/// glyphs can be cached per size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextSize {
    /// Grid plugin tag ("NLY", `0x5a6004`).
    Px11,
    /// Button text and units (BPM, dB).
    Px14,
    /// Most common: list rows, menu items, control labels.
    Px16,
    /// Page titles, dropdown values, help text.
    Px18,
    /// Card headers and large values.
    Px20,
    /// Header mode label ("STOMP"): 22 px Bold (`st-mp1c`: ink x 693–765, y 64–79).
    Px22,
    /// Scene dropdown letter: 23 px, cap 16 (`st5b`).
    Px23,
    /// Editor labels and the dropdown scene letter.
    Px24,
    /// Gig title and tile names; the last Grid title step.
    Px32,
    /// Grid title step.
    Px36,
    /// Gig scene id, tuner cents, looper letters; a Grid title step.
    Px40,
    /// Grid title steps: the stock title shrinks 70 px by 0.9 until the name fits.
    Px45,
    Px50,
    Px56,
    /// Preset save text field, first step (re-r44 §4).
    Px60,
    /// Grid title step ("1C Main & Bass V2", `st-mp1c`).
    Px63,
    /// Grid preset title and tuner note.
    Px70,
    /// Tempo BPM.
    Px96,
}

const SIZES: usize = 18;

impl TextSize {
    pub const fn px(self) -> u32 {
        match self {
            Self::Px11 => 11,
            Self::Px14 => 14,
            Self::Px16 => 16,
            Self::Px18 => 18,
            Self::Px20 => 20,
            Self::Px22 => 22,
            Self::Px23 => 23,
            Self::Px24 => 24,
            Self::Px32 => 32,
            Self::Px36 => 36,
            Self::Px40 => 40,
            Self::Px45 => 45,
            Self::Px50 => 50,
            Self::Px56 => 56,
            Self::Px60 => 60,
            Self::Px63 => 63,
            Self::Px70 => 70,
            Self::Px96 => 96,
        }
    }

    /// Stock Qt draws text of 64 px and up as outlines (`QT_MAX_CACHED_GLYPH_SIZE`), at the
    /// font's design advances and kerns: "Worship V3" at 70 px puts every glyph within 1 px of
    /// the fractional pen (`st-mp1a`, `st-mp1h`, `stock-home`); whole-pixel advances drift 3 px.
    pub const fn outline(self) -> bool {
        self.px() >= 64
    }

    /// Pen advance of a glyph, as stock Qt places it. Outline sizes keep the design advance.
    /// Smaller sizes use whole pixels, and the rule depends on the size (threshold ink widths
    /// on the QC): 16 px Regular rounds (UNLINK DEVICE 114 px, RESET WI-FI SETTINGS 166 px,
    /// `sa1`), 22 px Bold rounds ("STOMP", `st-mp1c`) and 63 px Bold rounds ("Main & Bass V2",
    /// `st-mp1c`; floor drifts 10 px), and so does 11 px Bold (Grid plugin tag "NLY" in
    /// `stock-home-20261015.png`: L is 8 px after N). The other Grid title steps (36 to 56)
    /// round as 63 does (I). 23 px floors (`sn-0`, measured as "STOMP" before the label was known to be 22 px
    /// Bold; I). Other sizes are not measured and keep fractional advances.
    pub fn stock_advance(self, advance: f32) -> f32 {
        match self {
            Self::Px11
            | Self::Px16
            | Self::Px22
            | Self::Px36
            | Self::Px45
            | Self::Px50
            | Self::Px56
            | Self::Px63 => advance.round(),
            Self::Px23 => advance.floor(),
            _ => advance,
        }
    }

    /// Pair kern in pixels from a GPOS value in font units, as stock Qt places it. Outline
    /// sizes keep the scaled value (W→o is −1.75 px at 70 px, `st-mp1a`). The 22 px label and
    /// the Grid title steps (36 to 63) round the kern in Qt's 26.6 fixed point, halves up:
    /// "STOMP" S→T is −32/64 px and gives 0, T→O is −35/64 px and gives −1 (`st-mp1c`). The
    /// 11 px plugin tag rounds the same way: L→Y (−89 units) is −1 px in "NLY", not 0
    /// (`stock-home-20261015.png`). Other sizes truncate toward zero (I; not measured).
    pub fn stock_kern(self, units: i32, em: i32) -> f32 {
        let px = self.px() as i32;
        match self {
            _ if self.outline() => (units * px) as f32 / em as f32,
            Self::Px11
            | Self::Px22
            | Self::Px36
            | Self::Px45
            | Self::Px50
            | Self::Px56
            | Self::Px63 => {
                let fixed = ((units * px * 64) as f32 / em as f32).round() as i32;
                ((fixed + 32) >> 6) as f32
            }
            _ => (units * px / em) as f32,
        }
    }

    /// Cap height: Plex capitals are 698/1000 em.
    pub const fn cap_px(self) -> u32 {
        (self.px() * 698 + 500) / 1000
    }

    /// Baseline distance: hhea ascender 1025 + descender 275 = 1.3 em.
    pub const fn line_px(self) -> u32 {
        (self.px() * 13 + 5) / 10
    }
}

/// The three stock faces. Stock picks Medium by family name, not by weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    Bold,
}

const WEIGHTS: usize = 3;

fn bytes(weight: Weight) -> &'static [u8] {
    match weight {
        Weight::Regular => include_bytes!("../fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf"),
        Weight::Medium => include_bytes!("../fonts/ibm-plex-sans/IBMPlexSans-Medium.ttf"),
        Weight::Bold => include_bytes!("../fonts/ibm-plex-sans/IBMPlexSans-Bold.ttf"),
    }
}

fn face(weight: Weight) -> &'static Font {
    static FACES: [OnceLock<Font>; WEIGHTS] = [const { OnceLock::new() }; WEIGHTS];
    FACES[weight as usize].get_or_init(|| {
        let settings = FontSettings {
            // Text is drawn glyph by glyph; ligature glyphs are never looked up.
            load_substitutions: false,
            ..FontSettings::default()
        };
        Font::from_bytes(bytes(weight), settings).expect("vendored IBM Plex Sans parses")
    })
}

/// One rasterized glyph: coverage placed relative to the pen on the baseline,
/// and the pen advance in whole pixels where the size is measured ([`TextSize::stock_advance`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub mask: Mask,
    pub advance: f32,
}

/// The GPOS `kern` lookups of one face, read once. Stock Qt shapes with HarfBuzz, which applies
/// this feature; fontdue reads only the legacy `kern` table, which these faces lack.
struct Kerning {
    table: ttf_parser::Face<'static>,
    lookups: Vec<u16>,
}

fn kerning(weight: Weight) -> &'static Kerning {
    static KERNING: [OnceLock<Kerning>; WEIGHTS] = [const { OnceLock::new() }; WEIGHTS];
    KERNING[weight as usize].get_or_init(|| {
        let table =
            ttf_parser::Face::parse(bytes(weight), 0).expect("vendored IBM Plex Sans parses");
        let kern = ttf_parser::Tag::from_bytes(b"kern");
        let mut lookups: Vec<u16> = table
            .tables()
            .gpos
            .into_iter()
            .flat_map(|gpos| gpos.features)
            .filter(|feature| feature.tag == kern)
            .flat_map(|feature| feature.lookup_indices)
            .collect();
        lookups.sort_unstable();
        lookups.dedup();
        Kerning { table, lookups }
    })
}

impl Kerning {
    /// Pair kern from `left` to `right` in pixels at `size` ([`TextSize::stock_kern`]).
    fn pair_px(&self, weight: Weight, left: char, right: char, size: TextSize) -> f32 {
        let units = match (ascii_index(left), ascii_index(right)) {
            (Some(l), Some(r)) => {
                let slot = &ASCII_KERNS[weight as usize][l * ASCII + r];
                match slot.load(Ordering::Relaxed) {
                    0 => {
                        let units = self.pair_units(left, right);
                        slot.store(units + KERN_KNOWN, Ordering::Relaxed);
                        units
                    }
                    stored => stored - KERN_KNOWN,
                }
            }
            _ => self.pair_units(left, right),
        };
        size.stock_kern(units, i32::from(self.table.units_per_em()))
    }

    /// The GPOS pair value in font units (a few lookups and coverage searches: slow on the QC,
    /// so [`Self::pair_px`] keeps the printable ASCII pairs).
    fn pair_units(&self, left: char, right: char) -> i32 {
        let (Some(left), Some(right), Some(gpos)) = (
            self.table.glyph_index(left),
            self.table.glyph_index(right),
            self.table.tables().gpos,
        ) else {
            return 0;
        };
        self.lookups
            .iter()
            .filter_map(|&index| {
                gpos.lookups
                    .get(index)?
                    .subtables
                    .into_iter::<PositioningSubtable>()
                    .find_map(|subtable| match subtable {
                        PositioningSubtable::Pair(pair) => pair_x_advance(pair, left, right),
                        _ => None,
                    })
            })
            .map(i32::from)
            .sum()
    }
}

/// Pair kerns of printable ASCII per face in font units, filled on first use: 0 is not looked
/// up yet, otherwise the units plus [`KERN_KNOWN`].
static ASCII_KERNS: [[AtomicI32; ASCII * ASCII]; WEIGHTS] =
    [const { [const { AtomicI32::new(0) }; ASCII * ASCII] }; WEIGHTS];
const KERN_KNOWN: i32 = 1 << 24;

fn ascii_index(ch: char) -> Option<usize> {
    (' '..='~').contains(&ch).then(|| ch as usize - 32)
}

/// The x advance of `left` before `right` in one pair subtable, if the subtable covers that pair.
fn pair_x_advance(pair: PairAdjustment<'_>, left: GlyphId, right: GlyphId) -> Option<i16> {
    match pair {
        PairAdjustment::Format1 { coverage, sets } => {
            let set = sets.get(coverage.get(left)?)?;
            set.get(right).map(|(first, _)| first.x_advance)
        }
        PairAdjustment::Format2 {
            coverage,
            classes,
            matrix,
        } => {
            coverage.get(left)?;
            matrix
                .get((classes.0.get(left), classes.1.get(right)))
                .map(|(first, _)| first.x_advance)
        }
    }
}

fn rasterize(index: u16, size: TextSize, weight: Weight) -> Glyph {
    let (metrics, alpha) = face(weight).rasterize_indexed(index, size.px() as f32);
    Glyph {
        mask: Mask {
            left: metrics.xmin,
            top: -(metrics.ymin + metrics.height as i32),
            width: metrics.width as u32,
            height: metrics.height as u32,
            alpha,
        },
        advance: size.stock_advance(metrics.advance_width),
    }
}

/// Printable ASCII, the hot path: lock-free, indexed by `ch - ' '`.
const ASCII: usize = 95;
static ASCII_CACHE: [[[OnceLock<Glyph>; ASCII]; WEIGHTS]; SIZES] =
    [const { [const { [const { OnceLock::new() }; ASCII] }; WEIGHTS] }; SIZES];

/// Everything else, keyed by glyph index so the cache is bounded by the
/// font (1019 glyphs per face) however many distinct characters appear.
type Key = (u16, TextSize, Weight);
static OTHER_CACHE: LazyLock<Mutex<HashMap<Key, &'static Glyph>>> = LazyLock::new(Mutex::default);

/// Glyph for `ch`. Characters the font lacks show its `.notdef` box; stored
/// text is never changed.
pub fn glyph(ch: char, size: TextSize, weight: Weight) -> &'static Glyph {
    if let Some(index) = ascii_index(ch) {
        return ASCII_CACHE[size as usize][weight as usize][index]
            .get_or_init(|| rasterize(face(weight).lookup_glyph_index(ch), size, weight));
    }
    let ch = if ch == '\u{a0}' { ' ' } else { ch };
    let index = face(weight).lookup_glyph_index(ch);
    let mut cache = OTHER_CACHE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    cache
        .entry((index, size, weight))
        .or_insert_with(|| Box::leak(Box::new(rasterize(index, size, weight))))
}

/// Pen advance of `left` when `right` follows it (`None` at the end of a line): the stock
/// advance plus the pair kern to `right`.
pub fn pen_advance(left: char, right: Option<char>, size: TextSize, weight: Weight) -> f32 {
    let kern = right.map_or(0.0, |right| {
        kerning(weight).pair_px(weight, left, right, size)
    });
    glyph(left, size, weight).advance + kern
}

pub fn text_width(text: &str, size: TextSize, weight: Weight) -> f32 {
    let mut chars = text.chars().peekable();
    let mut width = 0.0;
    while let Some(ch) = chars.next() {
        width += pen_advance(ch, chars.peek().copied(), size, weight);
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_ascii_has_ink_and_missing_characters_show_a_box() {
        for ch in (33u8..127).map(char::from).chain("…°·×é€".chars()) {
            let glyph = glyph(ch, TextSize::Px16, Weight::Regular);
            assert!(glyph.mask.alpha.iter().any(|&alpha| alpha > 128), "{ch:?}");
        }
        assert!(
            glyph(' ', TextSize::Px96, Weight::Bold)
                .mask
                .alpha
                .is_empty()
        );
        let notdef = glyph('🙂', TextSize::Px16, Weight::Regular);
        assert!(notdef.mask.alpha.iter().any(|&alpha| alpha > 200));
        assert_eq!(notdef, glyph('\u{e000}', TextSize::Px16, Weight::Regular));
    }

    /// Masks sit on the baseline at the stock cap height (0.698 em), so
    /// layout can centre text on `cap_px`.
    #[test]
    fn capitals_match_the_stock_cap_height() {
        for size in [TextSize::Px16, TextSize::Px40, TextSize::Px96] {
            let h = &glyph('H', size, Weight::Medium).mask;
            assert_eq!(h.top + h.height as i32, 0, "{size:?}");
            assert!((-h.top).abs_diff(size.cap_px() as i32) <= 1, "{size:?}");
        }
        assert_eq!(TextSize::Px16.cap_px(), 11);
        assert_eq!(TextSize::Px16.line_px(), 21);
    }

    /// The ASCII pair cache returns the GPOS kern on the first (lookup) and later (cached)
    /// calls, negative kerns included. Pins the three size rules with stock pairs.
    #[test]
    fn cached_ascii_kerns_equal_the_gpos_lookup() {
        let size = TextSize::Px24;
        for weight in [Weight::Regular, Weight::Medium, Weight::Bold] {
            let kerning = kerning(weight);
            let em = i32::from(kerning.table.units_per_em());
            for (left, right) in (' '..='~').flat_map(|l| (' '..='~').map(move |r| (l, r))) {
                let want = size.stock_kern(kerning.pair_units(left, right), em);
                assert_eq!(kerning.pair_px(weight, left, right, size), want);
                assert_eq!(kerning.pair_px(weight, left, right, size), want);
            }
        }
        let bold =
            |left, right, size| kerning(Weight::Bold).pair_px(Weight::Bold, left, right, size);
        assert_eq!(bold('W', 'o', TextSize::Px24), 0.0);
        assert_eq!(bold('W', 'o', TextSize::Px70), -1.75);
        assert_eq!(bold('S', 'T', TextSize::Px22), 0.0);
        assert_eq!(bold('T', 'O', TextSize::Px22), -1.0);
    }

    /// The stock plugin tag "NLY" (11 px Bold): N, L and Y start 8 and 13 px apart, so the
    /// whole text is 20 px (`stock-home-20261015.png`).
    #[test]
    fn plugin_tag_pen_matches_the_stock_shot() {
        let (size, weight) = (TextSize::Px11, Weight::Bold);
        assert_eq!(pen_advance('N', Some('L'), size, weight), 8.0);
        assert_eq!(pen_advance('L', Some('Y'), size, weight), 5.0);
        assert_eq!(text_width("NLY", size, weight), 20.0);
    }

    /// Plex digits are tabular, so changing values do not shift layout.
    #[test]
    fn digits_are_tabular() {
        let zero = glyph('0', TextSize::Px18, Weight::Medium).advance;
        assert!(
            "123456789"
                .chars()
                .all(|digit| glyph(digit, TextSize::Px18, Weight::Medium).advance == zero)
        );
    }
}
