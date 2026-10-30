//! Partial redraw: compare the display list with the one last drawn and
//! repaint only the rectangles that changed. Live data (tuner needle, meters,
//! beat dots, one knob) then costs a small part of a full frame. A few recent
//! frames are kept, so going back to a screen (closing the ⋮ menu, an editor or
//! a page over the Grid) starts from its last frame instead of a full repaint.

use crate::draw::{extent, is_pressed, paint_item, paint_region};
use crate::layout::{Item, Look};
use crate::render::{Canvas, FULL};
use crate::{Control, Frame, Rect, UiState};
use tetrad_assets::{Palette, Rgb};

/// More damaged rectangles than this are merged into their bounding box.
const MAX_RECTS: usize = 8;
/// Rectangle count above which [`merge`] gives the bounds without merging.
const MERGE_LIMIT: usize = 32;
/// Recent frames kept besides the shown one (1.1 MB each).
const KEPT: usize = 3;

/// A display list as painted: the layout's items as they are (no per-frame copy) and the
/// control shown pressed.
#[derive(Debug, Clone)]
struct Painted {
    items: Vec<Item>,
    pressed: Option<Control>,
}

impl Painted {
    fn len(&self) -> usize {
        self.items.len()
    }

    /// Each item with whether it draws pressed. Two entries that compare equal paint the same.
    fn entries(&self) -> impl DoubleEndedIterator<Item = (&Item, bool)> + ExactSizeIterator {
        self.items
            .iter()
            .map(|item| (item, is_pressed(item, self.pressed)))
    }

    /// The first `len` items, pressed as here.
    fn prefix(&self, len: usize) -> Self {
        Self {
            items: self.items[..len].to_vec(),
            pressed: self.pressed,
        }
    }

    /// Whether `item` (one of these items) draws pressed.
    fn draws_pressed(&self, item: &Item) -> bool {
        is_pressed(item, self.pressed)
    }
}

impl PartialEq for Painted {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.entries().eq(other.entries())
    }
}

/// Keeps one frame and the display list it shows. [`Renderer::render`]
/// repaints only what changed and reports the changed rectangles, so the
/// application can present those rows alone.
#[derive(Debug, Clone, Default)]
pub struct Renderer {
    frame: Frame,
    /// `None` until the first frame, and after [`Renderer::invalidate`].
    shown: Option<Painted>,
    /// Earlier frames with the lists they show (each equal to a full render of its list),
    /// oldest first.
    kept: Vec<(Painted, Frame)>,
    /// The palette `shown` and `kept` are painted in. Items do not hold every colour they
    /// paint (button styles resolve in `draw`), so a new palette repaints everything.
    palette: Palette,
}

impl Renderer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// Report the full frame on the next [`Renderer::render`] (for example after
    /// the display lost its contents).
    pub fn invalidate(&mut self) {
        self.shown = None;
    }

    /// Bring the frame up to date with `ui`. Returns the repainted rectangles
    /// in frame coordinates: empty when nothing changed, the full frame on the
    /// first call. The result equals [`UiState::render`] pixel for pixel.
    pub fn render(&mut self, ui: &UiState) -> Vec<Rect> {
        let layout = ui.layout();
        if layout.palette != self.palette {
            self.palette = layout.palette;
            self.shown = None;
            self.kept.clear();
        }
        let next = Painted {
            items: layout.items,
            pressed: ui.pressed,
        };
        // A kept frame of exactly this list (the ⋮ menu opened again) comes back as it is: a
        // veil over the whole frame costs ~15 ms on the QC.
        if let Some(index) = self.kept.iter().position(|(list, _)| *list == next)
            && let Some(shown) = self.shown.take_if(|shown| *shown != next)
        {
            let rects = merge(damage(&shown, &next));
            let (_, frame) = self.kept.remove(index);
            let left = std::mem::replace(&mut self.frame, frame);
            self.kept.push((shown, left));
            self.shown = Some(next);
            return rects;
        }
        // Items appended over a list that kept its items (an overlay or a toast opening) paint
        // straight over the current frame: a full render would draw the same pixels underneath
        // first. A few changed items of the list (the key pressed to open it) are repainted from
        // the list first.
        let changed = (self.shown.as_ref())
            .filter(|shown| shown.len() < next.len())
            .map(|shown| -> Vec<Rect> {
                (shown.entries().zip(next.entries()))
                    .filter(|(old, new)| old != new)
                    .flat_map(|((old, _), (new, _))| [extent(old), extent(new)])
                    .flatten()
                    .collect()
            })
            .filter(|changed| !large(changed));
        if let Some(changed) = changed {
            let base = self.shown.as_ref().map_or(0, Painted::len);
            let fixed = merge(changed);
            let mut canvas = Canvas::new(&mut self.frame, self.palette);
            for &rect in &fixed {
                paint_region(&mut canvas, &next.items[..base], rect, |item| {
                    (item, next.draws_pressed(item))
                });
            }
            if !fixed.is_empty() {
                self.shown = Some(next.prefix(base));
            }
            let mut added = &next.items[base..];
            let rects = merge(added.iter().filter_map(extent).collect());
            // A large overlay (the ⋮ menu's veil): keep the frame under it for the close. A
            // leading full-frame veil is written straight into the spare frame (one pass instead
            // of a copy and a tint; each full-frame pass is ~7.5 ms on the QC).
            if large(&rects)
                && let Some(shown) = self.shown.take()
            {
                match added.split_first() {
                    Some((first, rest)) if let Some((color, alpha)) = full_veil(first) => {
                        let mut spare = self.spare();
                        spare.tinted_from(&self.frame, color, alpha);
                        let under = std::mem::replace(&mut self.frame, spare);
                        self.kept.push((shown, under));
                        added = rest;
                    }
                    _ => self.keep(shown, false),
                }
            }
            let mut canvas = Canvas::new(&mut self.frame, self.palette);
            for item in added {
                paint_item(&mut canvas, item, next.draws_pressed(item));
            }
            self.shown = Some(next);
            return if fixed.is_empty() {
                rects
            } else {
                merge(fixed.into_iter().chain(rects).collect())
            };
        }
        let rects = match &self.shown {
            Some(shown) => merge(damage(shown, &next)),
            None => vec![FULL],
        };
        let paint = if large(&rects) {
            self.restore_or_keep(&next, &rects)
        } else {
            rects.clone()
        };
        let mut canvas = Canvas::new(&mut self.frame, self.palette);
        for &rect in &paint {
            paint_region(&mut canvas, &next.items, rect, |item| {
                (item, next.draws_pressed(item))
            });
        }
        self.shown = Some(next);
        rects
    }

    /// Before a large repaint toward `next` (`rects` from the shown frame): swap in the kept
    /// frame closest to `next` when it needs at most half the paint, and keep the shown frame
    /// in its place; otherwise keep the shown frame and paint all of a spare one. Returns the
    /// rectangles to paint over the frame now held.
    fn restore_or_keep(&mut self, next: &Painted, rects: &[Rect]) -> Vec<Rect> {
        let closest = (self.kept.iter().enumerate())
            .map(|(index, (list, _))| (index, merge(damage(list, next))))
            .min_by_key(|(_, paint)| total_area(paint));
        let shown = self.shown.take();
        match closest {
            Some((index, paint)) if total_area(&paint) * 2 <= total_area(rects) => {
                let (_, frame) = self.kept.remove(index);
                let left = std::mem::replace(&mut self.frame, frame);
                if let Some(shown) = shown {
                    self.kept.push((shown, left));
                }
                paint
            }
            // Painting all of a spare frame costs about what the large repaint does, and
            // keeps the shown frame without a copy.
            _ => {
                if let Some(shown) = shown {
                    self.keep(shown, true);
                }
                vec![FULL]
            }
        }
    }

    /// Keep the frame and `shown`, its list, before most of it is painted over. `whole`:
    /// every pixel will be painted, so the frame itself is kept and the paint goes to a spare
    /// frame; otherwise a copy is kept.
    fn keep(&mut self, shown: Painted, whole: bool) {
        let mut spare = self.spare();
        let kept = if whole {
            std::mem::replace(&mut self.frame, spare)
        } else {
            spare.copy_from(&self.frame);
            spare
        };
        self.kept.push((shown, kept));
    }

    /// A frame to paint into: the oldest kept one when all [`KEPT`] are in use, else a new one.
    fn spare(&mut self) -> Frame {
        if self.kept.len() >= KEPT {
            self.kept.remove(0).1
        } else {
            Frame::default()
        }
    }
}

/// The colour and alpha of a veil over the whole frame, which [`Frame::tinted_from`] can write.
fn full_veil(item: &Item) -> Option<(Rgb, u8)> {
    match item.look {
        Look::Veil { color, alpha }
            if item.clip.is_none() && extent(item) == Some(FULL) && alpha != 0 && alpha != 255 =>
        {
            Some((color, alpha))
        }
        _ => None,
    }
}

/// A change that repaints more than half of the frame (a screen or a large overlay).
fn large(rects: &[Rect]) -> bool {
    total_area(rects) * 2 > area(FULL)
}

fn total_area(rects: &[Rect]) -> u64 {
    rects.iter().copied().map(area).sum()
}

/// Extents of items that differ. Equal-length lists compare pairwise (the
/// usual live update); otherwise everything between the common prefix and
/// suffix is damaged.
fn damage(old: &Painted, new: &Painted) -> Vec<Rect> {
    let extents = |items: &[Item]| items.iter().filter_map(extent).collect();
    if old.len() == new.len() {
        return old
            .entries()
            .zip(new.entries())
            .filter(|(a, b)| a != b)
            .flat_map(|((a, _), (b, _))| [extent(a), extent(b)])
            .flatten()
            .collect();
    }
    let prefix = (old.entries().zip(new.entries()))
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = (old.entries().skip(prefix).rev())
        .zip(new.entries().skip(prefix).rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut rects: Vec<Rect> = extents(&old.items[prefix..old.len() - suffix]);
    rects.extend(extents(&new.items[prefix..new.len() - suffix]));
    rects
}

fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (x1, y1) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32)
}

fn area(rect: Rect) -> u64 {
    u64::from(rect.width) * u64::from(rect.height)
}

/// Merge rectangles that overlap or whose union wastes little area, then cap
/// the count. Repainting overlapping rectangles is correct but slower. Above
/// [`MERGE_LIMIT`] rectangles (a screen change lists one per item) the result is
/// their bounds at once: each merge restarts the pair scan, and on the QC 560
/// I/O page items took 7–11 ms per merge.
fn merge(mut rects: Vec<Rect>) -> Vec<Rect> {
    if rects.len() > MERGE_LIMIT {
        return rects.into_iter().reduce(union).into_iter().collect();
    }
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let joined = union(rects[i], rects[j]);
                let overlap = rects[i].intersect(rects[j]).is_some();
                if overlap || area(joined) * 4 <= (area(rects[i]) + area(rects[j])) * 5 {
                    rects[i] = joined;
                    rects.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    if rects.len() > MAX_RECTS {
        let bounds = rects.iter().copied().reduce(union);
        rects = bounds.into_iter().collect();
    }
    rects
}
