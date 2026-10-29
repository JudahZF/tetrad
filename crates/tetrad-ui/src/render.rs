//! Owned 800×480 RGB8 software canvas: clipped fills, anti-aliased shapes,
//! IBM Plex Sans text from the glyph cache, and line icons.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{LazyLock, Mutex};

use tetrad_assets::{Icon, IconSize, Mask, Palette, Rgb, TextSize, Weight, text_width};

use crate::Rect;

pub const WIDTH: u32 = 800;
pub const HEIGHT: u32 = 480;

/// Packed opaque RGB8, row-major, no padding, stride 2400 bytes. This is not
/// a device framebuffer mapping and does not assume its native pixel format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pixels: Vec<u8>,
}

impl Frame {
    pub fn width(&self) -> u32 {
        WIDTH
    }

    pub fn height(&self) -> u32 {
        HEIGHT
    }

    pub fn rgb(&self) -> &[u8] {
        &self.pixels
    }

    /// Encode binary P6 PPM to a caller-owned writer. File creation, overwrite
    /// policy and flushing belong to the application, not the renderer.
    pub fn write_ppm(&self, mut writer: impl Write) -> io::Result<()> {
        write!(writer, "P6\n{WIDTH} {HEIGHT}\n255\n")?;
        writer.write_all(&self.pixels)
    }

    /// A frame filled with the stock background color. Allocate it once and pass it
    /// to [`crate::UiState::render_into`] or keep a [`crate::Renderer`]; both paint every pixel.
    pub fn new() -> Self {
        let b = Palette::STOCK.background;
        Self {
            pixels: [b.0, b.1, b.2].repeat((WIDTH * HEIGHT) as usize),
        }
    }

    /// Copy `other` into this frame's buffer (no allocation).
    pub(crate) fn copy_from(&mut self, other: &Frame) {
        self.pixels.copy_from_slice(&other.pixels);
    }

    /// `other` under a full-frame [`Canvas::tint`] of `color` at `alpha`, in one pass: equal to
    /// [`Self::copy_from`] then that tint, with half the frame-sized memory traffic.
    pub(crate) fn tinted_from(&mut self, other: &Frame, color: Rgb, alpha: u8) {
        let table = tint_table(color, alpha);
        let pairs = self
            .pixels
            .chunks_exact_mut(3)
            .zip(other.pixels.chunks_exact(3));
        for (out, pixel) in pairs {
            for ((value, &source), table) in out.iter_mut().zip(pixel).zip(&table) {
                *value = table[usize::from(source)];
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn pixel(&self, x: u32, y: u32) -> Rgb {
        let offset = (y * WIDTH + x) as usize * 3;
        Rgb(
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
        )
    }
}

impl Default for Frame {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Font {
    pub size: TextSize,
    pub weight: Weight,
    /// Synthetic oblique: only upright IBM Plex faces ship, so italic text
    /// shears the upright glyphs (stock unsaved-preset header, `0x00341104`).
    pub italic: bool,
}

impl Font {
    pub const fn new(size: TextSize, weight: Weight) -> Self {
        Self {
            size,
            weight,
            italic: false,
        }
    }

    pub const fn italic(self, italic: bool) -> Self {
        Self { italic, ..self }
    }

    pub fn width(self, text: &str) -> f32 {
        text_width(text, self.size, self.weight)
    }
}

/// Integer clip box in frame coordinates: `[x0, x1) × [y0, y1)`.
#[derive(Debug, Clone, Copy)]
struct Clip {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Clip {
    fn of(rect: Rect) -> Self {
        let (x0, y0) = (rect.x.max(0), rect.y.max(0));
        let x1 = (i64::from(rect.x) + i64::from(rect.width)).clamp(0, i64::from(WIDTH)) as i32;
        let y1 = (i64::from(rect.y) + i64::from(rect.height)).clamp(0, i64::from(HEIGHT)) as i32;
        Self {
            x0: x0.min(WIDTH as i32),
            y0: y0.min(HEIGHT as i32),
            x1,
            y1,
        }
    }

    fn intersect(self, other: Self) -> Self {
        Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        }
    }
}

/// Intersect with a generous area around the frame using 64-bit math, so
/// shape code can use plain `i32` arithmetic. Off-frame parts are invisible.
fn bounded(rect: Rect) -> Option<Rect> {
    const MARGIN: i64 = 1024;
    let x0 = i64::from(rect.x).max(-MARGIN);
    let y0 = i64::from(rect.y).max(-MARGIN);
    let x1 = (i64::from(rect.x) + i64::from(rect.width)).min(i64::from(WIDTH) + MARGIN);
    let y1 = (i64::from(rect.y) + i64::from(rect.height)).min(i64::from(HEIGHT) + MARGIN);
    (x1 > x0 && y1 > y0)
        .then(|| Rect::new(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32))
}

pub(crate) const FULL: Rect = Rect {
    x: 0,
    y: 0,
    width: WIDTH,
    height: HEIGHT,
};

/// Byte range of the pixels `[x0, x1)` on row `y` (callers keep x0 ≤ x1).
/// Each channel value under `color` at `alpha` ([`Rgb::mix`]), per channel.
fn tint_table(color: Rgb, alpha: u8) -> [[u8; 256]; 3] {
    [color.0, color.1, color.2]
        .map(|ink| std::array::from_fn(|v| Rgb(v as u8, 0, 0).mix(Rgb(ink, 0, 0), alpha).0))
}

fn span_bytes(y: i32, x0: i32, x1: i32) -> std::ops::Range<usize> {
    let row = y as usize * WIDTH as usize;
    (row + x0 as usize) * 3..(row + x1 as usize) * 3
}

/// Blend `color` over the pixels of `bytes`, each at its own `coverage` ([`Canvas::blend`]
/// without the clip test: the caller clips the row).
fn blend_row<'m>(bytes: &mut [u8], coverage: impl Iterator<Item = &'m u8>, color: Rgb) {
    for (pixel, &alpha) in bytes.chunks_exact_mut(3).zip(coverage) {
        blend_pixel(pixel, color, alpha);
    }
}

/// `pixel` (3 bytes) under `color` at `alpha` ([`Rgb::mix`]).
#[inline(always)]
fn blend_pixel(pixel: &mut [u8], color: Rgb, alpha: u8) {
    match alpha {
        0 => {}
        255 => pixel.copy_from_slice(&[color.0, color.1, color.2]),
        _ => {
            let mixed = Rgb(pixel[0], pixel[1], pixel[2]).mix(color, alpha);
            pixel.copy_from_slice(&[mixed.0, mixed.1, mixed.2]);
        }
    }
}

/// One pixel of a precomputed ring, relative to an integer centre.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RingPixel {
    dx: i16,
    dy: i16,
    /// Clockwise degrees from +x (y down), 0..360.
    angle: f32,
    alpha: u8,
}

/// Anti-aliased ring of `width` centred on radius `r` around an integer
/// centre. Built once per geometry, so drawing an arc needs no `atan2`.
pub(crate) fn ring(r: f32, width: f32) -> Vec<RingPixel> {
    let outer = (r + width / 2.0 + 1.0).ceil() as i32;
    let mut pixels = Vec::new();
    for dy in -outer..outer {
        for dx in -outer..outer {
            let (px, py) = (dx as f32 + 0.5, dy as f32 + 0.5);
            let alpha = Canvas::coverage(((px * px + py * py).sqrt() - r).abs() - width / 2.0);
            if alpha > 0 {
                pixels.push(RingPixel {
                    dx: dx as i16,
                    dy: dy as i16,
                    angle: py.atan2(px).to_degrees().rem_euclid(360.0),
                    alpha,
                });
            }
        }
    }
    pixels
}

/// A process-wide cache of leaked values: bounded by its users, alive as long as the program.
type Cache<K, V> = LazyLock<Mutex<HashMap<K, &'static V>>>;

/// Rings drawn so far, by the bits of radius and width. Bounded like [`SHAPES`].
static RINGS: Cache<(u32, u32), [RingPixel]> = LazyLock::new(Mutex::default);
const RINGS_MAX: usize = 64;

/// [`ring`] from a cache, built on first use: a [`crate::layout::Look::Arc`] repaint (a dial
/// turned) would otherwise take a square root per pixel of the ring's box again.
pub(crate) fn cached_ring(r: f32, width: f32) -> Cow<'static, [RingPixel]> {
    let mut rings = RINGS.lock().unwrap_or_else(|poison| poison.into_inner());
    let key = (r.to_bits(), width.to_bits());
    if let Some(pixels) = rings.get(&key) {
        return Cow::Borrowed(*pixels);
    }
    let pixels = ring(r, width);
    if rings.len() >= RINGS_MAX {
        return Cow::Owned(pixels);
    }
    let pixels: &'static [RingPixel] = pixels.leak();
    rings.insert(key, pixels);
    Cow::Borrowed(pixels)
}

/// Geometry of a cached anti-aliased shape mask. Floats are kept by their bits: the mask is
/// the exact output of the per-pixel code for that geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Shape {
    /// [`Canvas::circle`] coverage around a centre whose fractions are `half_x`/`half_y`
    /// half pixels, relative to the centre's whole pixel.
    Circle { half_x: bool, half_y: bool, r: u32 },
    /// [`Canvas::corner_mask`] of a filled corner.
    Corner { k: i32, r: u32 },
    /// [`Canvas::corner_mask`] of an outline corner of `width`.
    OutlineCorner { k: i32, r: u32, width: u32 },
    /// A sloped [`Canvas::line`] from a start whose fractions are `half_x`/`half_y` half
    /// pixels, relative to the start's whole pixel.
    Line {
        half_x: bool,
        half_y: bool,
        dx: u32,
        dy: u32,
        width: u32,
    },
    /// [`Canvas::triangle_down`] filling a box of this size at the origin.
    Triangle { width: u32, height: u32 },
    /// The sheared ([`oblique`]) mask of one glyph.
    Oblique {
        ch: char,
        size: TextSize,
        weight: Weight,
    },
}

/// `v` on the half-pixel grid, small enough that sums and products of such values stay exact.
fn half_grid(v: f32) -> bool {
    (v * 2.0).fract() == 0.0 && v.abs() < 4096.0
}

/// Shapes drawn so far. The UI uses a few dozen geometries; past [`SHAPES_MAX`] new ones are
/// computed per draw instead, so odd sizes cannot grow it without bound.
static SHAPES: Cache<Shape, Shaped> = LazyLock::new(Mutex::default);
const SHAPES_MAX: usize = 1024;

/// A cached shape mask and, per row, its longest run of full coverage (mask columns
/// `start..end`, empty when shorter than [`Self::SOLID_MIN`]). [`Canvas::shaped`] writes that run
/// without a coverage test per pixel: most of a disc.
#[derive(Debug, Clone)]
struct Shaped {
    mask: Mask,
    solid: Vec<(u32, u32)>,
}

impl Shaped {
    /// Shorter runs do not pay for splitting the row.
    const SOLID_MIN: usize = 8;

    fn new(mask: Mask) -> Self {
        let rows = mask.alpha.chunks_exact(mask.width.max(1) as usize);
        let solid = rows
            .map(|row| {
                let (mut run, mut start) = ((0, 0), 0);
                for (x, &alpha) in row.iter().enumerate() {
                    if alpha != 255 {
                        start = x + 1;
                    } else if x + 1 - start > run.1 - run.0 {
                        run = (start, x + 1);
                    }
                }
                match run.1 - run.0 >= Self::SOLID_MIN {
                    true => (run.0 as u32, run.1 as u32),
                    false => (0, 0),
                }
            })
            .collect();
        Self { mask, solid }
    }
}

/// The mask of `shape` from the cache, built by `build` on first use. Masks are leaked: the
/// cache is bounded and lives as long as the program.
fn shape_mask(shape: Shape, build: impl FnOnce() -> Mask) -> Cow<'static, Shaped> {
    let mut shapes = SHAPES.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(shaped) = shapes.get(&shape) {
        return Cow::Borrowed(*shaped);
    }
    let shaped = Shaped::new(build());
    if shapes.len() >= SHAPES_MAX {
        return Cow::Owned(shaped);
    }
    let shaped: &'static Shaped = Box::leak(Box::new(shaped));
    shapes.insert(shape, shaped);
    Cow::Borrowed(shaped)
}

/// Painter over a borrowed frame. All drawing is clipped to the frame and the
/// current clip rectangle; out-of-range geometry is ignored, never wrapped.
pub(crate) struct Canvas<'a> {
    frame: &'a mut Frame,
    clip: Clip,
    /// The colours `draw` paints item looks with (the palette of the layout it paints).
    pub palette: Palette,
}

impl<'a> Canvas<'a> {
    pub fn new(frame: &'a mut Frame, palette: Palette) -> Self {
        Self {
            frame,
            clip: Clip::of(FULL),
            palette,
        }
    }

    /// Run `draw` with drawing restricted to `rect` (inside any outer clip).
    pub fn clipped(&mut self, rect: Rect, draw: impl FnOnce(&mut Self)) {
        let saved = self.clip;
        self.clip = saved.intersect(Clip::of(rect));
        draw(self);
        self.clip = saved;
    }

    fn blend(&mut self, x: i32, y: i32, color: Rgb, alpha: u8) {
        let c = self.clip;
        if alpha == 0 || x < c.x0 || y < c.y0 || x >= c.x1 || y >= c.y1 {
            return;
        }
        blend_pixel(
            &mut self.frame.pixels[span_bytes(y, x, x + 1)],
            color,
            alpha,
        );
    }

    /// Blend `color` at `alpha` down column `x` over rows `[y0, y1)`, clipped once.
    fn column(&mut self, x: i32, y0: i32, y1: i32, color: Rgb, alpha: u8) {
        let c = self.clip;
        let (y0, y1) = (y0.max(c.y0), y1.min(c.y1));
        if alpha == 0 || x < c.x0 || x >= c.x1 || y0 >= y1 {
            return;
        }
        let rows = span_bytes(y0, x, x + 1).start..span_bytes(y1 - 1, x, x + 1).end;
        for row in self.frame.pixels[rows].chunks_mut(WIDTH as usize * 3) {
            blend_pixel(&mut row[..3], color, alpha);
        }
    }

    /// Blend one color at one coverage over the pixels `[x0, x1)` of row `y`.
    fn span(&mut self, y: i32, x0: i32, x1: i32, color: Rgb, alpha: u8) {
        let c = self.clip;
        let (x0, x1) = (x0.max(c.x0), x1.min(c.x1));
        if alpha == 0 || y < c.y0 || y >= c.y1 || x0 >= x1 {
            return;
        }
        let bytes = &mut self.frame.pixels[span_bytes(y, x0, x1)];
        if alpha == 255 {
            // Write one pixel, then double the filled prefix with memcpy.
            bytes[..3].copy_from_slice(&[color.0, color.1, color.2]);
            let mut filled = 3;
            while filled < bytes.len() {
                let count = filled.min(bytes.len() - filled);
                bytes.copy_within(..count, filled);
                filled += count;
            }
            return;
        }
        // `Rgb::mix` per channel, (a·(255−t) + b·t + 127) / 255, on 4-pixel chunks so it
        // vectorizes: x / 255 == (x + 1 + (x >> 8)) >> 8 for every x up to 65 152.
        let keep = 255 - u16::from(alpha);
        let add = [color.0, color.1, color.2].map(|b| u16::from(b) * u16::from(alpha) + 127);
        let mix = |a: u8, add: u16| {
            let x = u16::from(a) * keep + add;
            ((x + 1 + (x >> 8)) >> 8) as u8
        };
        let pattern: [u16; 12] = std::array::from_fn(|i| add[i % 3]);
        let mut chunks = bytes.chunks_exact_mut(12);
        for chunk in &mut chunks {
            for (channel, add) in chunk.iter_mut().zip(pattern) {
                *channel = mix(*channel, add);
            }
        }
        for (channel, add) in chunks.into_remainder().iter_mut().zip(pattern) {
            *channel = mix(*channel, add);
        }
    }

    /// Paint the first row, then copy it to the others: one copy per row.
    pub fn fill(&mut self, rect: Rect, color: Rgb) {
        let c = self.clip.intersect(Clip::of(rect));
        if c.x0 >= c.x1 || c.y0 >= c.y1 {
            return;
        }
        self.span(c.y0, c.x0, c.x1, color, 255);
        let first = span_bytes(c.y0, c.x0, c.x1);
        for y in c.y0 + 1..c.y1 {
            let to = span_bytes(y, c.x0, c.x1).start;
            self.frame.pixels.copy_within(first.clone(), to);
        }
    }

    /// Blend `color` at `alpha` over `rect` (translucent veil). A large veil looks each channel
    /// up in a table of its 256 blends: on the QC (no NEON) a load per byte costs about half
    /// the multiply-and-divide of [`Self::span`].
    pub fn tint(&mut self, rect: Rect, color: Rgb, alpha: u8) {
        /// Pixels from which the 768-entry table pays for itself.
        const TABLE_AREA: i32 = 4096;
        let c = self.clip.intersect(Clip::of(rect));
        let area = (c.x1 - c.x0).max(0) * (c.y1 - c.y0).max(0);
        if area < TABLE_AREA || alpha == 0 || alpha == 255 {
            for y in c.y0..c.y1 {
                self.span(y, c.x0, c.x1, color, alpha);
            }
            return;
        }
        let table = tint_table(color, alpha);
        for y in c.y0..c.y1 {
            for pixel in self.frame.pixels[span_bytes(y, c.x0, c.x1)].chunks_exact_mut(3) {
                for (value, table) in pixel.iter_mut().zip(&table) {
                    *value = table[usize::from(*value)];
                }
            }
        }
    }

    /// [`crate::layout::Look::Fade`] over `rect`.
    pub fn fade(&mut self, rect: Rect, card: Rgb, faded: Rgb, keep: u8) {
        let c = self.clip.intersect(Clip::of(rect));
        let (keep, under) = (u16::from(keep), 255 - u16::from(keep));
        let base = [faded.0, faded.1, faded.2].map(|value| u16::from(value) * under);
        for y in c.y0..c.y1 {
            for pixel in self.frame.pixels[span_bytes(y, c.x0, c.x1)].chunks_exact_mut(3) {
                if *pixel == [card.0, card.1, card.2] {
                    pixel.copy_from_slice(&[faded.0, faded.1, faded.2]);
                    continue;
                }
                for (value, base) in pixel.iter_mut().zip(base) {
                    *value = ((base + u16::from(*value) * keep) / 255) as u8;
                }
            }
        }
    }

    /// Darken everything inside the clip toward black (modal scrim).
    pub fn dim(&mut self, keep: u8) {
        let c = self.clip;
        if c.x0 >= c.x1 {
            return;
        }
        // channel × keep / 255 as a multiply and shift, which vectorizes.
        let factor = (u32::from(keep) * 65_536).div_ceil(255);
        for y in c.y0..c.y1 {
            for channel in &mut self.frame.pixels[span_bytes(y, c.x0, c.x1)] {
                *channel = ((u32::from(*channel) * factor) >> 16) as u8;
            }
        }
    }

    /// Anti-aliased coverage from a signed distance (negative inside).
    fn coverage(distance: f32) -> u8 {
        ((0.5 - distance).clamp(0.0, 1.0) * 255.0).round() as u8
    }

    /// Coverage of one corner square of side `k` for corner radius `r`,
    /// indexed `[b * k + a]` by a pixel's whole-pixel distance from the
    /// vertical (`a`) and horizontal (`b`) edge. The four corners mirror it.
    fn corner_mask(k: i32, r: f32, coverage: impl Fn(f32) -> u8) -> Mask {
        let side = k.max(0) as usize;
        let mut alpha = Vec::with_capacity(side * side);
        for b in 0..side {
            for a in 0..side {
                let (qx, qy) = (r - (a as f32 + 0.5), r - (b as f32 + 0.5));
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                alpha.push(coverage(outside + qx.max(qy).min(0.0) - r));
            }
        }
        Mask {
            left: 0,
            top: 0,
            width: side as u32,
            height: side as u32,
            alpha,
        }
    }

    /// Blend the corner squares one row slice at a time. `k` is at most half the width rounded
    /// up, so `a` runs up from the left edge in the left squares and up from the right edge in
    /// the right ones.
    fn paint_corners(&mut self, rect: Rect, k: i32, mask: &[u8], color: Rgb) {
        let c = self.clip;
        for (corner, (cx0, cy0, cx1, cy1)) in Self::corners(rect, k).into_iter().enumerate() {
            let (x0, x1) = (cx0.max(c.x0), cx1.min(c.x1));
            if x0 >= x1 {
                continue;
            }
            for y in cy0.max(c.y0)..cy1.min(c.y1) {
                let b = (y - rect.y).min(rect.bottom() - 1 - y) as usize;
                let row = &mask[b * k as usize..][..k as usize];
                let bytes = &mut self.frame.pixels[span_bytes(y, x0, x1)];
                if corner % 2 == 0 {
                    let coverage = &row[(x0 - rect.x) as usize..(x1 - rect.x) as usize];
                    blend_row(bytes, coverage.iter(), color);
                } else {
                    let coverage = &row[(rect.right() - x1) as usize..(rect.right() - x0) as usize];
                    blend_row(bytes, coverage.iter().rev(), color);
                }
            }
        }
    }

    /// Corner squares of side `k` at the four corners of `rect`. Far squares
    /// start no earlier than `k` from the near edge, so squares never overlap.
    fn corners(rect: Rect, k: i32) -> [(i32, i32, i32, i32); 4] {
        let (x0, y0, x1, y1) = (rect.x, rect.y, rect.right(), rect.bottom());
        let (xr, yb) = ((x1 - k).max(x0 + k), (y1 - k).max(y0 + k));
        [
            (x0, y0, x0 + k, y0 + k),
            (xr, y0, x1, y0 + k),
            (x0, yb, x0 + k, y1),
            (xr, yb, x1, y1),
        ]
    }

    /// Integer-aligned edges are fully covered, so only the corner squares
    /// need anti-aliasing.
    pub fn round_rect(&mut self, rect: Rect, radius: f32, color: Rgb) {
        let Some(rect) = bounded(rect) else {
            return;
        };
        let r = radius
            .min(rect.width as f32 / 2.0)
            .min(rect.height as f32 / 2.0)
            .max(0.0);
        let k = r.ceil() as i32;
        let (x0, y0, x1, y1) = (rect.x, rect.y, rect.right(), rect.bottom());
        let middle = (y1 - y0 - 2 * k).max(0) as u32;
        self.fill(Rect::new(x0, y0 + k, rect.width, middle), color);
        let yb = (y1 - k).max(y0 + k);
        for y in (y0..y0 + k).chain(yb..y1) {
            self.span(y, x0 + k, x1 - k, color, 255);
        }
        let shape = Shape::Corner { k, r: r.to_bits() };
        let mask = shape_mask(shape, || Self::corner_mask(k, r, Self::coverage));
        self.paint_corners(rect, k, &mask.mask.alpha, color);
    }

    /// Rounded outline of `width` pixels drawn inside `rect`: straight strips
    /// with one coverage per row or column, and distance tests in the corners.
    pub fn round_outline(&mut self, rect: Rect, radius: f32, width: f32, color: Rgb) {
        let Some(rect) = bounded(rect) else {
            return;
        };
        let ring = |d: f32| Self::coverage((d + width / 2.0).abs() - width / 2.0);
        let half = (rect.width.min(rect.height) / 2) as i32;
        let n = (width.ceil() as i32 + 1).min(half);
        let k = (radius.ceil() as i32).max(n).min(half);
        let (x0, y0, x1, y1) = (rect.x, rect.y, rect.right(), rect.bottom());
        for t in 0..n {
            let alpha = ring(-(t as f32 + 0.5));
            self.span(y0 + t, x0 + k, x1 - k, color, alpha);
            self.span(y1 - 1 - t, x0 + k, x1 - k, color, alpha);
            self.column(x0 + t, y0 + k, y1 - k, color, alpha);
            self.column(x1 - 1 - t, y0 + k, y1 - k, color, alpha);
        }
        let r = radius
            .min(rect.width as f32 / 2.0)
            .min(rect.height as f32 / 2.0);
        let shape = Shape::OutlineCorner {
            k,
            r: r.to_bits(),
            width: width.to_bits(),
        };
        let mask = shape_mask(shape, || Self::corner_mask(k, r, ring));
        self.paint_corners(rect, k, &mask.mask.alpha, color);
    }

    /// Anti-aliased disc. Centres and radii on the half-pixel grid (every UI disc) draw a
    /// cached mask: there every coordinate difference below is exact, so the mask built
    /// around the centre's fraction holds the same coverage as the per-pixel loop.
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgb) {
        if half_grid(cx) && half_grid(cy) && half_grid(r) && r >= 0.0 {
            let (x, y) = (cx.floor(), cy.floor());
            let (fx, fy) = (cx - x, cy - y);
            let shape = Shape::Circle {
                half_x: fx != 0.0,
                half_y: fy != 0.0,
                r: r.to_bits(),
            };
            let mask = shape_mask(shape, || Self::circle_mask(fx, fy, r));
            self.shaped(&mask, x as i32, y as i32, color);
            return;
        }
        let c = self.clip;
        let rows =
            ((cy - r - 1.0).floor() as i32).max(c.y0)..((cy + r + 1.0).ceil() as i32).min(c.y1);
        for y in rows {
            for x in
                ((cx - r - 1.0).floor() as i32).max(c.x0)..((cx + r + 1.0).ceil() as i32).min(c.x1)
            {
                self.blend(x, y, color, Self::disc_coverage(x, y, cx, cy, r));
            }
        }
    }

    fn disc_coverage(x: i32, y: i32, cx: f32, cy: f32, r: f32) -> u8 {
        let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt() - r;
        Self::coverage(d)
    }

    /// [`Self::circle`] coverage around `(cx, cy)`, over the same pixel box.
    fn circle_mask(cx: f32, cy: f32, r: f32) -> Mask {
        let (left, top) = ((cx - r - 1.0).floor() as i32, (cy - r - 1.0).floor() as i32);
        let (right, bottom) = ((cx + r + 1.0).ceil() as i32, (cy + r + 1.0).ceil() as i32);
        let alpha = (top..bottom)
            .flat_map(|y| (left..right).map(move |x| Self::disc_coverage(x, y, cx, cy, r)))
            .collect();
        Mask {
            left,
            top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
            alpha,
        }
    }

    /// Part of a precomputed `ring` around `(cx, cy)`, from `start` sweeping
    /// clockwise by `sweep` degrees (0° = +x, y down). Butt ends.
    pub fn arc(
        &mut self,
        (cx, cy): (i32, i32),
        ring: &[RingPixel],
        start: f32,
        sweep: f32,
        color: Rgb,
    ) {
        if sweep <= 0.0 {
            return;
        }
        // `rem_euclid` is `fmodf` (a libm call per pixel on the QC). Below one turn the
        // remainder is the difference itself, so the branch gives the same value.
        let offset = |angle: f32| {
            let turn = angle - start;
            match turn.abs() < 360.0 {
                true if turn < 0.0 => turn + 360.0,
                true => turn,
                false => turn.rem_euclid(360.0),
            }
        };
        for pixel in ring {
            if sweep < 360.0 && offset(pixel.angle) > sweep {
                continue;
            }
            let x = cx.saturating_add(i32::from(pixel.dx));
            let y = cy.saturating_add(i32::from(pixel.dy));
            self.blend(x, y, color, pixel.alpha);
        }
    }

    /// Round-capped anti-aliased line. Horizontal lines (grid rows) blend one
    /// coverage per row between the caps.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Rgb) {
        let half = width / 2.0;
        let c = self.clip;
        if y0 == y1 && x0 != x1 {
            let (left, right) = (x0.min(x1), x0.max(x1));
            // Pixels whose centre projects inside the segment.
            let inner = ((left - 0.5).ceil() as i32, (right - 0.5).floor() as i32 + 1);
            let outer_l = ((left - half - 1.0).floor() as i32).max(c.x0);
            let outer_r = ((right + half + 1.0).ceil() as i32).min(c.x1);
            let top = ((y0 - half - 1.0).floor() as i32).max(c.y0);
            let bottom = ((y0 + half + 1.0).ceil() as i32).min(c.y1);
            for y in top..bottom {
                let dy = y as f32 + 0.5 - y0;
                self.span(y, inner.0, inner.1, color, Self::coverage(dy.abs() - half));
                for x in (outer_l..inner.0).chain(inner.1..outer_r) {
                    let end = if (x as f32) + 0.5 < left { left } else { right };
                    let d = ((x as f32 + 0.5 - end).powi(2) + dy * dy).sqrt() - half;
                    self.blend(x, y, color, Self::coverage(d));
                }
            }
            return;
        }
        // On the half-pixel grid (with a quarter-pixel half width) every difference below is
        // exact, so a mask built at the start's fraction holds the same coverage.
        if [x0, y0, x1, y1, width].into_iter().all(half_grid) {
            let (x, y) = (x0.floor(), y0.floor());
            let (fx, fy) = (x0 - x, y0 - y);
            let shape = Shape::Line {
                half_x: fx != 0.0,
                half_y: fy != 0.0,
                dx: (x1 - x0).to_bits(),
                dy: (y1 - y0).to_bits(),
                width: width.to_bits(),
            };
            let (x1, y1) = (x1 - x, y1 - y);
            let mask = shape_mask(shape, || Self::line_mask(fx, fy, x1, y1, half));
            self.shaped(&mask, x as i32, y as i32, color);
            return;
        }
        let (left, top, right, bottom) = Self::line_box(x0, y0, x1, y1, half);
        for y in top.max(c.y0)..bottom.min(c.y1) {
            for x in left.max(c.x0)..right.min(c.x1) {
                self.blend(x, y, color, Self::line_coverage(x, y, x0, y0, x1, y1, half));
            }
        }
    }

    /// Pixel box of a sloped line: `(left, top, right, bottom)`, right and bottom exclusive.
    fn line_box(x0: f32, y0: f32, x1: f32, y1: f32, half: f32) -> (i32, i32, i32, i32) {
        (
            (x0.min(x1) - half - 1.0).floor() as i32,
            (y0.min(y1) - half - 1.0).floor() as i32,
            (x0.max(x1) + half + 1.0).ceil() as i32,
            (y0.max(y1) + half + 1.0).ceil() as i32,
        )
    }

    fn line_coverage(x: i32, y: i32, x0: f32, y0: f32, x1: f32, y1: f32, half: f32) -> u8 {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let length = dx * dx + dy * dy;
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let t = if length == 0.0 {
            0.0
        } else {
            (((px - x0) * dx + (py - y0) * dy) / length).clamp(0.0, 1.0)
        };
        let d = ((px - x0 - t * dx).powi(2) + (py - y0 - t * dy).powi(2)).sqrt() - half;
        Self::coverage(d)
    }

    fn line_mask(x0: f32, y0: f32, x1: f32, y1: f32, half: f32) -> Mask {
        let (left, top, right, bottom) = Self::line_box(x0, y0, x1, y1, half);
        let alpha = (top..bottom)
            .flat_map(|y| {
                (left..right).map(move |x| Self::line_coverage(x, y, x0, y0, x1, y1, half))
            })
            .collect();
        Mask {
            left,
            top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
            alpha,
        }
    }

    /// Blend a coverage mask whose origin sits at `(x, y)`.
    fn mask(&mut self, mask: &Mask, x: i32, y: i32, color: Rgb) {
        self.mask_rows(mask, x, y, |bytes, coverage, _, _| {
            blend_row(bytes, coverage.iter(), color);
        });
    }

    /// Blend a cached shape whose origin sits at `(x, y)`. The run of full coverage of each
    /// row ([`Shaped`]) is written without a coverage test per pixel (a plain store loop: no
    /// call overhead on the short runs).
    fn shaped(&mut self, shaped: &Shaped, x: i32, y: i32, color: Rgb) {
        self.mask_rows(&shaped.mask, x, y, |bytes, coverage, row, left| {
            let (start, end) = shaped.solid[row];
            let at = |column: u32| (column as usize).clamp(left, left + coverage.len()) - left;
            let run = at(start)..at(end);
            if run.is_empty() {
                blend_row(bytes, coverage.iter(), color);
                return;
            }
            let (head, rest) = bytes.split_at_mut(run.start * 3);
            let (body, tail) = rest.split_at_mut(run.len() * 3);
            blend_row(head, coverage[..run.start].iter(), color);
            for pixel in body.chunks_exact_mut(3) {
                pixel.copy_from_slice(&[color.0, color.1, color.2]);
            }
            blend_row(tail, coverage[run.end..].iter(), color);
        });
    }

    /// Calls `row(frame bytes, coverage, mask row, first mask column)` for each row of `mask`
    /// placed at `(x, y)`, with both slices cut to the clip.
    fn mask_rows(
        &mut self,
        mask: &Mask,
        x: i32,
        y: i32,
        mut row: impl FnMut(&mut [u8], &[u8], usize, usize),
    ) {
        let c = self.clip;
        let x0 = i64::from(x) + i64::from(mask.left);
        let y0 = i64::from(y) + i64::from(mask.top);
        let left = (i64::from(c.x0) - x0).max(0);
        let right = (i64::from(c.x1) - x0).min(i64::from(mask.width));
        let top = (i64::from(c.y0) - y0).max(0);
        let bottom = (i64::from(c.y1) - y0).min(i64::from(mask.height));
        if left >= right || top >= bottom {
            return;
        }
        for my in top..bottom {
            let start = (my * i64::from(mask.width)) as usize;
            let coverage = &mask.alpha[start + left as usize..start + right as usize];
            let bytes = span_bytes((y0 + my) as i32, (x0 + left) as i32, (x0 + right) as i32);
            row(
                &mut self.frame.pixels[bytes],
                coverage,
                my as usize,
                left as usize,
            );
        }
    }

    /// Filled downward triangle inside `rect`: the full-width top edge and
    /// the apex at the bottom centre (dropdown ▼).
    /// Inside a box of up to 4096 px the coverage depends on the box size only (positions in
    /// it are exact), so the box draws a cached mask.
    pub fn triangle_down(&mut self, rect: Rect, color: Rgb) {
        let small = [rect.x, rect.y, rect.width as i32, rect.height as i32]
            .iter()
            .all(|v| v.unsigned_abs() < 4096);
        if small {
            let shape = Shape::Triangle {
                width: rect.width,
                height: rect.height,
            };
            let at_origin = Rect::new(0, 0, rect.width, rect.height);
            let mask = shape_mask(shape, || {
                let alpha = (0..rect.height as i32)
                    .flat_map(|y| {
                        (0..rect.width as i32)
                            .map(move |x| Self::triangle_coverage(at_origin, x, y))
                    })
                    .collect();
                Mask {
                    left: 0,
                    top: 0,
                    width: rect.width,
                    height: rect.height,
                    alpha,
                }
            });
            self.shaped(&mask, rect.x, rect.y, color);
            return;
        }
        let c = self.clip;
        for y in rect.y.max(c.y0)..rect.bottom().min(c.y1) {
            for x in rect.x.max(c.x0)..rect.right().min(c.x1) {
                self.blend(x, y, color, Self::triangle_coverage(rect, x, y));
            }
        }
    }

    fn triangle_coverage(rect: Rect, x: i32, y: i32) -> u8 {
        let (w, h) = (rect.width as f32, rect.height as f32);
        let (cx, top) = (rect.x as f32 + w / 2.0, rect.y as f32);
        // Unit normal of the right edge; the left edge mirrors it.
        let length = (h * h + w * w / 4.0).sqrt();
        let (nx, ny) = (h / length, w / 2.0 / length);
        let (px, py) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - top);
        let side = (px.abs() * nx + py * ny) - w / 2.0 * nx;
        Self::coverage(side.max(-py))
    }

    pub fn icon(&mut self, icon: Icon, size: IconSize, x: i32, y: i32, color: Rgb) {
        self.mask(tetrad_assets::icon(icon, size), x, y, color);
    }

    /// Icon centred in `rect`.
    pub fn icon_in(&mut self, rect: Rect, icon: Icon, size: IconSize, color: Rgb) {
        let px = size.px() as i32;
        self.icon(
            icon,
            size,
            rect.x + (rect.width as i32 - px) / 2,
            rect.y + (rect.height as i32 - px) / 2,
            color,
        );
    }

    /// Draw one line starting at the pen position on `baseline`. Returns the
    /// pen x after the text.
    pub fn text_at(&mut self, x: f32, baseline: i32, text: &str, font: Font, color: Rgb) -> f32 {
        let mut pen = x;
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            let glyph = tetrad_assets::glyph(ch, font.size, font.weight);
            if font.italic {
                let shape = Shape::Oblique {
                    ch,
                    size: font.size,
                    weight: font.weight,
                };
                let mask = shape_mask(shape, || oblique(&glyph.mask));
                self.shaped(&mask, pen.round() as i32, baseline, color);
            } else {
                self.mask(&glyph.mask, pen.round() as i32, baseline, color);
            }
            pen += tetrad_assets::pen_advance(ch, chars.peek().copied(), font.size, font.weight);
        }
        pen
    }

    /// Single line vertically centred on cap height inside `rect`, aligned
    /// horizontally and shortened with an ellipsis when too long. Stored text
    /// is never changed.
    pub fn text(&mut self, rect: Rect, text: &str, font: Font, color: Rgb, align: Align) {
        // Text that fits (the usual case) is measured once and not copied.
        let width = font.width(text);
        let (fitted, width) = if width <= rect.width as f32 {
            (Cow::Borrowed(text), width)
        } else {
            let fitted = fit(text, font, rect.width as f32);
            let width = font.width(&fitted);
            (Cow::Owned(fitted), width)
        };
        let x = match align {
            Align::Left => rect.x as f32,
            Align::Center => rect.x as f32 + (rect.width as f32 - width) / 2.0,
            Align::Right => rect.x as f32 + rect.width as f32 - width,
        };
        let cap = font.size.cap_px() as i32;
        let baseline = rect.y + (rect.height as i32 + cap) / 2;
        self.clipped(rect.grow(0, 4), |canvas| {
            canvas.text_at(x, baseline, &fitted, font, color);
        });
    }

    /// Word-wrapped paragraph from the top of `rect`. Returns lines drawn.
    pub fn paragraph(
        &mut self,
        rect: Rect,
        text: &str,
        font: Font,
        color: Rgb,
        align: Align,
    ) -> u32 {
        let line = font.size.line_px() as i32;
        let lines = wrap(text, font, rect.width as f32);
        let mut drawn = 0;
        for (index, content) in lines.iter().enumerate() {
            let y = rect.y + index as i32 * line;
            if y + line > rect.y + rect.height as i32 {
                break;
            }
            self.text(
                Rect {
                    y,
                    height: line as u32,
                    ..rect
                },
                content,
                font,
                color,
                align,
            );
            drawn += 1;
        }
        drawn
    }
}

impl crate::UiState {
    /// Render the current projection and UI state into a new frame.
    /// Deterministic: no clock, file, device or external asset access.
    pub fn render(&self) -> Frame {
        let mut frame = Frame::new();
        self.render_into(&mut frame);
        frame
    }

    /// Full redraw into a caller-owned frame (no allocation). For partial
    /// redraws use [`crate::Renderer`].
    pub fn render_into(&self, frame: &mut Frame) {
        let layout = self.layout();
        let items = layout.items;
        crate::draw::paint_region(
            &mut Canvas::new(frame, layout.palette),
            &items,
            FULL,
            |item| (item, crate::draw::is_pressed(item, self.pressed)),
        );
    }
}

/// Size of [`crate::UiState::preset_grid`]: the grid area below the 96 px header.
pub const PRESET_GRID_WIDTH: u32 = WIDTH;
pub const PRESET_GRID_HEIGHT: u32 = HEIGHT - crate::state::HEADER as u32;

impl crate::UiState {
    /// Cortex Control Screenshot (25): a stored preset's Grid as the offscreen grid renderer
    /// draws it (`0x691040`, ctor `0x691c14`, paint `0x692260`): 800 × 384 from y 96, on a
    /// transparent fill, without a selection or an editor. Packed RGBA rows, top first, not
    /// premultiplied. The grid is drawn over black and over white; the difference gives each
    /// pixel's alpha, so anti-aliased edges keep their coverage.
    pub fn preset_grid(mut snapshot: crate::UiSnapshot) -> Result<Vec<u8>, crate::UiError> {
        snapshot.selected_block = None;
        let mut ui = crate::UiState::new();
        ui.set_snapshot(snapshot)?;
        let layout = crate::screens::grid::offscreen(&ui);
        let [black, white] = [Rgb(0, 0, 0), Rgb(255, 255, 255)].map(|fill| {
            let mut frame = Frame::new();
            let mut canvas = Canvas::new(&mut frame, layout.palette);
            canvas.fill(FULL, fill);
            for item in &layout.items {
                crate::draw::item(&mut canvas, item, None);
            }
            frame.pixels
        });
        let start = (WIDTH * crate::state::HEADER as u32 * 3) as usize;
        let mut rgba = Vec::with_capacity((PRESET_GRID_WIDTH * PRESET_GRID_HEIGHT * 4) as usize);
        for (on_black, on_white) in black[start..]
            .chunks_exact(3)
            .zip(white[start..].chunks_exact(3))
        {
            let spread = (0..3)
                .map(|c| on_white[c].saturating_sub(on_black[c]))
                .max()
                .unwrap_or(0);
            let alpha = 255 - spread;
            for &value in on_black {
                let colour = (u16::from(value) * 255)
                    .checked_div(u16::from(alpha))
                    .unwrap_or(0);
                rgba.push(colour.min(255) as u8);
            }
            rgba.push(alpha);
        }
        Ok(rgba)
    }
}

/// Shorten with a trailing ellipsis so the text fits `width` pixels.
/// Shear a glyph mask about its baseline by about 12°: each row moves right
/// by its height above the baseline times tan 12°, split over two pixels so
/// the edges stay anti-aliased. Advances do not change.
fn oblique(mask: &Mask) -> Mask {
    const SLANT: f32 = 0.2126; // tan 12°
    let shift = |row: i32| -((mask.top + row) as f32 + 0.5) * SLANT;
    let rows = mask.height as i32;
    let min = shift(rows - 1).min(0.0).floor() as i32;
    let max = shift(0).max(0.0).ceil() as i32;
    let width = mask.width as i32 + max - min + 1;
    let mut alpha = vec![0u16; (width * rows) as usize];
    for row in 0..rows {
        let s = shift(row) - min as f32;
        let (whole, frac) = (s.floor() as i32, s.fract());
        let src = &mask.alpha[(row * mask.width as i32) as usize..][..mask.width as usize];
        let dst = &mut alpha[(row * width) as usize..][..width as usize];
        for (x, &a) in src.iter().enumerate() {
            let at = (x as i32 + whole) as usize;
            let a = f32::from(a);
            dst[at] += (a * (1.0 - frac)).round() as u16;
            dst[at + 1] += (a * frac).round() as u16;
        }
    }
    Mask {
        left: mask.left + min,
        top: mask.top,
        width: width as u32,
        height: mask.height,
        alpha: alpha.into_iter().map(|a| a.min(255) as u8).collect(),
    }
}

pub(crate) fn fit(text: &str, font: Font, width: f32) -> String {
    if font.width(text) <= width {
        return text.to_owned();
    }
    let ellipsis = font.width("…");
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if font.width(&out) + ellipsis > width {
            out.pop();
            break;
        }
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// Greedy word wrap; explicit newlines start new lines. Over-long words are
/// left for `fit` to shorten.
pub(crate) fn wrap(text: &str, font: Font, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_owned()
            } else {
                format!("{current} {word}")
            };
            if font.width(&candidate) <= width || current.is_empty() {
                current = candidate;
            } else {
                lines.push(std::mem::replace(&mut current, word.to_owned()));
            }
        }
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both veil blends, the vectorized span (one row) and the table (a large area, and
    /// [`Frame::tinted_from`]), are bit-equal to `Rgb::mix` for every channel value and
    /// coverage, the tail bytes past the last 4-pixel block included.
    #[test]
    fn tint_equals_mix_for_every_value_and_coverage() {
        let row = WIDTH as usize * 3;
        let mut source = Frame::new();
        for (i, byte) in source.pixels[..row].iter_mut().enumerate() {
            *byte = (i % 256) as u8;
        }
        let want = |x: usize, color: Rgb, alpha: u8| {
            let p = &source.pixels[x * 3..x * 3 + 3];
            Rgb(p[0], p[1], p[2]).mix(color, alpha)
        };
        let mut frame = Frame::new();
        for alpha in 1..=254u8 {
            let color = Rgb(alpha, 255 - alpha, 0x63);
            for rows in [1, 8] {
                frame.copy_from(&source);
                Canvas::new(&mut frame, Palette::STOCK).tint(
                    Rect::new(0, 0, WIDTH - 1, rows),
                    color,
                    alpha,
                );
                for x in 0..WIDTH as usize - 1 {
                    assert_eq!(
                        frame.pixel(x as u32, 0),
                        want(x, color, alpha),
                        "x {x} alpha {alpha}"
                    );
                }
                let last = WIDTH as usize - 1;
                assert_eq!(frame.pixel(last as u32, 0), want(last, color, 0), "outside");
            }
            frame.tinted_from(&source, color, alpha);
            for x in 0..WIDTH as usize {
                assert_eq!(
                    frame.pixel(x as u32, 0),
                    want(x, color, alpha),
                    "copy x {x}"
                );
            }
        }
    }

    /// Cached disc masks hold the per-pixel coverage for centres and radii on the half-pixel
    /// grid, near the frame edges and under a clip too.
    #[test]
    fn cached_discs_equal_the_per_pixel_disc() {
        let (mut cached, mut direct) = (Frame::new(), Frame::new());
        let clip = Rect::new(-20, 30, 300, 420);
        let fractions = [(0.0, 0.0), (0.5, 0.0), (0.0, 0.5), (0.5, 0.5)];
        for half in 1..=80 {
            let r = half as f32 / 2.0;
            for (index, (fx, fy)) in fractions.into_iter().enumerate() {
                let n = half * 4 + index;
                let cx = (n * 37 % 840) as f32 - 20.0 + fx;
                let cy = (n * 53 % 520) as f32 - 20.0 + fy;
                let color = Rgb(n as u8, 255 - half as u8, 0x40);
                let rect = if n % 2 == 0 { FULL } else { clip };
                Canvas::new(&mut cached, Palette::STOCK)
                    .clipped(rect, |canvas| canvas.circle(cx, cy, r, color));
                Canvas::new(&mut direct, Palette::STOCK).clipped(rect, |canvas| {
                    for y in (cy - r - 1.0).floor() as i32..(cy + r + 1.0).ceil() as i32 {
                        for x in (cx - r - 1.0).floor() as i32..(cx + r + 1.0).ceil() as i32 {
                            canvas.blend(x, y, color, Canvas::disc_coverage(x, y, cx, cy, r));
                        }
                    }
                });
            }
        }
        assert!(cached == direct);
    }

    /// Cached sloped lines (half-pixel grid) and ▼ triangles hold the per-pixel coverage
    /// wherever they are drawn.
    #[test]
    fn cached_lines_and_triangles_equal_the_per_pixel_shapes() {
        let (mut cached, mut direct) = (Frame::new(), Frame::new());
        for n in 0..240 {
            // Each geometry four times: every start fraction.
            let (shape, fraction) = (n / 4, n % 4);
            let (x0, y0) = ((n * 37 % 840) as f32 - 20.0, (n * 53 % 520) as f32 - 20.0);
            let (x0, y0) = (
                x0 + (fraction % 2) as f32 / 2.0,
                y0 + (fraction / 2) as f32 / 2.0,
            );
            let (dx, dy) = (
                (shape % 7) as f32 * 5.5 - 15.0,
                (shape % 11) as f32 * 3.5 - 17.0,
            );
            let (x1, y1) = (x0 + dx, y0 + dy);
            let width = [1.0, 1.5, 2.0, 3.0][shape % 4];
            let color = Rgb(n as u8, 0x80, 255 - n as u8);
            let rect = Rect::new(x0 as i32, y0 as i32, 4 + n as u32 % 13, 3 + n as u32 % 9);
            let mut canvas = Canvas::new(&mut cached, Palette::STOCK);
            if y0 != y1 {
                canvas.line(x0, y0, x1, y1, width, color);
            }
            canvas.triangle_down(rect, color);
            let mut canvas = Canvas::new(&mut direct, Palette::STOCK);
            let (left, top, right, bottom) = Canvas::line_box(x0, y0, x1, y1, width / 2.0);
            for y in top..bottom {
                for x in left..right {
                    if y0 != y1 {
                        let alpha = Canvas::line_coverage(x, y, x0, y0, x1, y1, width / 2.0);
                        canvas.blend(x, y, color, alpha);
                    }
                }
            }
            for y in rect.y..rect.bottom() {
                for x in rect.x..rect.right() {
                    canvas.blend(x, y, color, Canvas::triangle_coverage(rect, x, y));
                }
            }
        }
        assert!(cached == direct);
    }

    /// Corner squares painted by row slices (the right ones mirrored) equal the per-pixel blend
    /// at the nearer edge, for odd and even sizes, every `k` up to half the width rounded up,
    /// and clips that cut through the corners.
    #[test]
    fn corner_rows_equal_the_per_pixel_corners() {
        let (mut rows, mut direct) = (Frame::new(), Frame::new());
        let mask: Vec<u8> = (0..400).map(|i| (i * 37 % 256) as u8).collect();
        for n in 0..120 {
            let (width, height) = (1 + n as u32 % 13, 1 + n as u32 % 7 * 3);
            let rect = Rect::new(n * 53 % 780 - 6, n * 29 % 460 - 6, width, height);
            let k = (n % 3 + width.min(height).div_ceil(2) as i32 - 2).max(0);
            let clip = match n % 3 {
                0 => FULL,
                1 => Rect::new(rect.x + 1, rect.y, width / 2 + 1, height),
                _ => Rect::new(rect.x + width as i32 / 2, rect.y + 1, width, height / 2 + 1),
            };
            let color = Rgb(n as u8, 0x60, 255 - n as u8);
            let mask = &mask[..(k * k) as usize];
            Canvas::new(&mut rows, Palette::STOCK)
                .clipped(clip, |canvas| canvas.paint_corners(rect, k, mask, color));
            Canvas::new(&mut direct, Palette::STOCK).clipped(clip, |canvas| {
                for (x0, y0, x1, y1) in Canvas::corners(rect, k) {
                    for y in y0..y1 {
                        let b = (y - rect.y).min(rect.bottom() - 1 - y);
                        for x in x0..x1 {
                            let a = (x - rect.x).min(rect.right() - 1 - x);
                            canvas.blend(x, y, color, mask[(b * k + a) as usize]);
                        }
                    }
                }
            });
        }
        assert!(rows == direct);
    }

    #[test]
    fn extreme_rectangles_and_unicode_text_are_clipped_to_rgb_frame() {
        let mut frame = Frame::new();
        let mut canvas = Canvas::new(&mut frame, Palette::STOCK);
        canvas.fill(
            Rect {
                x: i32::MIN,
                y: i32::MIN,
                width: u32::MAX,
                height: u32::MAX,
            },
            Palette::STOCK.accent,
        );
        assert!(canvas.frame.rgb().chunks_exact(3).all(|rgb| rgb
            == [
                Palette::STOCK.accent.0,
                Palette::STOCK.accent.1,
                Palette::STOCK.accent.2
            ]));
        let font = Font::new(TextSize::Px20, Weight::Medium);
        canvas.text(
            Rect {
                x: -4,
                y: -4,
                width: 36,
                height: 14,
            },
            "aé🙂long",
            font,
            Palette::STOCK.text,
            Align::Left,
        );
        canvas.round_rect(
            Rect {
                x: i32::MAX - 4,
                y: i32::MAX - 4,
                width: u32::MAX,
                height: u32::MAX,
            },
            12.0,
            Palette::STOCK.on_accent,
        );
        canvas.arc(
            (-50, 900),
            &ring(40.0, 8.0),
            0.0,
            360.0,
            Palette::STOCK.on_accent,
        );
        canvas.arc(
            (i32::MAX, i32::MIN),
            &ring(40.0, 8.0),
            0.0,
            360.0,
            Palette::STOCK.on_accent,
        );
        assert_eq!(frame.rgb().len(), 800 * 480 * 3);
        assert_eq!(frame.pixel(799, 479), Palette::STOCK.accent);
    }

    #[test]
    fn fitting_adds_an_ellipsis_only_when_needed_and_wrap_keeps_words() {
        let font = Font::new(TextSize::Px16, Weight::Regular);
        assert_eq!(fit("OUTPUT", font, 200.0), "OUTPUT");
        let short = fit("A VERY LONG PRESET NAME INDEED", font, 60.0);
        assert!(short.ends_with('…') && font.width(&short) <= 60.0);
        let lines = wrap("one two three four five six", font, 60.0);
        assert!(lines.len() > 1);
        assert_eq!(lines.join(" "), "one two three four five six");
    }
}
