//! Anti-aliased rasterizer for owned outlines: round-capped strokes (lines,
//! polylines, circular arcs) and filled areas (polygons, boxes, sectors).

/// One shape in design units. Arc angles are degrees, measured clockwise on
/// screen from +x (y grows downward). Negative sweep turns back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Line(f32, f32, f32, f32),
    /// Open polyline stroke through the points (curves traced as short segments).
    Path(&'static [(f32, f32)]),
    Arc {
        cx: f32,
        cy: f32,
        r: f32,
        start: f32,
        sweep: f32,
    },
    /// Filled polygon (even-odd rule).
    Polygon(&'static [(f32, f32)]),
    /// Filled box from (x0, y0) to (x1, y1).
    Rect(f32, f32, f32, f32),
    /// Filled circular sector; a full sweep is a disc.
    Sector {
        cx: f32,
        cy: f32,
        r: f32,
        start: f32,
        sweep: f32,
    },
}

pub(crate) const fn l(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape {
    Shape::Line(x0, y0, x1, y1)
}

pub(crate) const fn path(points: &'static [(f32, f32)]) -> Shape {
    Shape::Path(points)
}

pub(crate) const fn a(cx: f32, cy: f32, r: f32, start: f32, sweep: f32) -> Shape {
    Shape::Arc {
        cx,
        cy,
        r,
        start,
        sweep,
    }
}

pub(crate) const fn dot(x: f32, y: f32) -> Shape {
    Shape::Line(x, y, x, y)
}

pub(crate) const fn circle(cx: f32, cy: f32, r: f32) -> Shape {
    a(cx, cy, r, 0.0, 360.0)
}

pub(crate) const fn fill(points: &'static [(f32, f32)]) -> Shape {
    Shape::Polygon(points)
}

pub(crate) const fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Shape {
    Shape::Rect(x0, y0, x1, y1)
}

pub(crate) const fn pie(cx: f32, cy: f32, r: f32, start: f32, sweep: f32) -> Shape {
    Shape::Sector {
        cx,
        cy,
        r,
        start,
        sweep,
    }
}

pub(crate) const fn disc(cx: f32, cy: f32, r: f32) -> Shape {
    pie(cx, cy, r, 0.0, 360.0)
}

/// Layered artwork: `ink` at full coverage, `dim` at `tone` coverage (the
/// stock two-tone glyphs) and `cut` knocked out of both.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Art {
    pub ink: &'static [Shape],
    pub dim: &'static [Shape],
    pub tone: f32,
    pub cut: &'static [Shape],
}

impl Art {
    pub const fn ink(ink: &'static [Shape]) -> Self {
        Self {
            ink,
            dim: &[],
            tone: 0.0,
            cut: &[],
        }
    }
}

/// 8-bit coverage bitmap. `left`/`top` place pixel (0, 0) relative to the
/// drawing origin (pen position on the baseline for glyphs, top-left for icons).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl Mask {
    pub fn coverage(&self, x: u32, y: u32) -> u8 {
        self.alpha[(y * self.width + x) as usize]
    }
}

/// Maps a design point to pixels: `pixel = point * scale + offset`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Placement {
    pub scale: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    /// Stroke half-width in pixels.
    pub radius: f32,
    /// Put thin shapes' axis-aligned edges on pixel boundaries.
    pub hint: bool,
}

impl Placement {
    fn point(self, x: f32, y: f32) -> (f32, f32) {
        (
            x * self.scale + self.offset_x,
            y * self.scale + self.offset_y,
        )
    }
}

fn arc_point(cx: f32, cy: f32, r: f32, degrees: f32) -> (f32, f32) {
    let (sin, cos) = degrees.to_radians().sin_cos();
    (cx + r * cos, cy + r * sin)
}

/// Squared distance from `p` to the segment `a`–`b`. Callers take the minimum
/// first and one square root after it.
fn segment_distance_sq(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    count_term();
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0)
    };
    (p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)
}

/// A shape already transformed to pixel space.
enum PixelShape {
    /// One stroke segment; a path becomes one per point pair, so each is evaluated only
    /// near itself.
    Segment((f32, f32), (f32, f32)),
    Arc {
        center: (f32, f32),
        r: f32,
        start: f32,
        sweep: f32,
        ends: [(f32, f32); 2],
    },
    /// Filled axis-aligned box `[x0, y0, x1, y1]` with `x0 <= x1`, `y0 <= y1`.
    Box([f32; 4]),
    /// Filled disc (a full sector).
    Disc { center: (f32, f32), r: f32 },
    /// Filled outline, closed from the last point back to the first.
    Area(Vec<(f32, f32)>),
}

impl PixelShape {
    fn new(shape: Shape, place: Placement) -> Self {
        let hint = place.hint;
        let corner = |x: f32, y: f32| {
            let (x, y) = place.point(x, y);
            if hint { (x.round(), y.round()) } else { (x, y) }
        };
        match shape {
            Shape::Line(x0, y0, x1, y1) => {
                let (mut a, mut b) = (place.point(x0, y0), place.point(x1, y1));
                // Thin axis-aligned strokes centre on pixel rows or columns.
                if hint {
                    let snap = |v: f32| v.floor() + 0.5;
                    if x0 == x1 {
                        (a.0, b.0) = (snap(a.0), snap(b.0));
                    }
                    if y0 == y1 {
                        (a.1, b.1) = (snap(a.1), snap(b.1));
                    }
                }
                Self::Segment(a, b)
            }
            Shape::Path(_) => unreachable!("paths convert by segment"),
            Shape::Arc {
                cx,
                cy,
                r,
                start,
                sweep,
            } => {
                let center = place.point(cx, cy);
                let r = r * place.scale;
                Self::Arc {
                    center,
                    r,
                    start,
                    sweep,
                    ends: [
                        arc_point(center.0, center.1, r, start),
                        arc_point(center.0, center.1, r, start + sweep),
                    ],
                }
            }
            Shape::Polygon(points) => {
                Self::Area(points.iter().map(|&(x, y)| corner(x, y)).collect())
            }
            Shape::Rect(x0, y0, x1, y1) => {
                let ((x0, y0), (x1, y1)) = (corner(x0, y0), corner(x1, y1));
                Self::Box([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
            }
            Shape::Sector {
                cx, cy, r, sweep, ..
            } if sweep.abs() >= 360.0 => Self::Disc {
                center: place.point(cx, cy),
                r: r * place.scale,
            },
            Shape::Sector {
                cx,
                cy,
                r,
                start,
                sweep,
            } => {
                let (center, r) = (place.point(cx, cy), r * place.scale);
                let steps = (sweep.abs() / 6.0).ceil().max(4.0) as u32;
                let mut points: Vec<(f32, f32)> = (0..=steps)
                    .map(|step| {
                        arc_point(
                            center.0,
                            center.1,
                            r,
                            start + sweep * step as f32 / steps as f32,
                        )
                    })
                    .collect();
                points.push(center);
                Self::Area(points)
            }
        }
    }

    /// Pixel coverage at `p`: strokes reach `radius` from their centre line,
    /// areas cover their inside; both ramp over one pixel.
    fn coverage(&self, p: (f32, f32), radius: f32) -> f32 {
        let signed = match *self {
            Self::Segment(a, b) => segment_distance_sq(p, a, b).sqrt() - radius,
            Self::Arc {
                center,
                r,
                start,
                sweep,
                ends,
            } => {
                count_term();
                let (vx, vy) = (p.0 - center.0, p.1 - center.1);
                let ring = ((vx * vx + vy * vy).sqrt() - r).abs();
                // Every point of the circle, the arc ends too, is at least `ring` away:
                // a pixel off the ring needs no angle.
                if sweep.abs() >= 360.0 || ring - radius >= 0.5 {
                    ring - radius
                } else {
                    let angle = vy.atan2(vx).to_degrees();
                    let relative = if sweep >= 0.0 {
                        (angle - start).rem_euclid(360.0)
                    } else {
                        (start - angle).rem_euclid(360.0)
                    };
                    let distance = if relative <= sweep.abs() {
                        ring
                    } else {
                        let [a, b] = ends;
                        let da = (p.0 - a.0).powi(2) + (p.1 - a.1).powi(2);
                        let db = (p.0 - b.0).powi(2) + (p.1 - b.1).powi(2);
                        da.min(db).sqrt()
                    };
                    distance - radius
                }
            }
            Self::Box([x0, y0, x1, y1]) => {
                count_term();
                let (dx, dy) = ((x0 - p.0).max(p.0 - x1), (y0 - p.1).max(p.1 - y1));
                if dx <= 0.0 && dy <= 0.0 {
                    dx.max(dy)
                } else {
                    (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt()
                }
            }
            Self::Disc { center, r } => {
                count_term();
                ((p.0 - center.0).powi(2) + (p.1 - center.1).powi(2)).sqrt() - r
            }
            Self::Area(_) => unreachable!("areas paint by scanline"),
        };
        (0.5 - signed).clamp(0.0, 1.0)
    }

    fn extend_bounds(&self, bounds: &mut [f32; 4]) {
        let mut add = |(x, y): (f32, f32)| {
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(y);
            bounds[2] = bounds[2].max(x);
            bounds[3] = bounds[3].max(y);
        };
        match *self {
            Self::Segment(a, b) => {
                add(a);
                add(b);
            }
            Self::Arc {
                center,
                r,
                start,
                sweep,
                ..
            } => {
                for step in 0..=24 {
                    add(arc_point(
                        center.0,
                        center.1,
                        r,
                        start + sweep * step as f32 / 24.0,
                    ));
                }
            }
            Self::Box([x0, y0, x1, y1]) => {
                add((x0, y0));
                add((x1, y1));
            }
            Self::Disc { center, r } => {
                add((center.0 - r, center.1 - r));
                add((center.0 + r, center.1 + r));
            }
            Self::Area(ref points) => points.iter().copied().for_each(add),
        }
    }
}

/// The mask being painted: its placement and size in pixels.
#[derive(Clone, Copy)]
struct Frame {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

impl Frame {
    /// Mask pixel ranges `(x0..x1, y0..y1)` of `bounds` grown by `pad`, clipped to the mask.
    fn clip(self, bounds: [f32; 4], pad: f32) -> (std::ops::Range<i32>, std::ops::Range<i32>) {
        let x0 = ((bounds[0] - pad).floor() as i32 - self.left).max(0);
        let y0 = ((bounds[1] - pad).floor() as i32 - self.top).max(0);
        let x1 = ((bounds[2] + pad).ceil() as i32 - self.left).min(self.width as i32);
        let y1 = ((bounds[3] + pad).ceil() as i32 - self.top).min(self.height as i32);
        (x0..x1, y0..y1)
    }

    /// Centre of mask pixel (x, y) in pixel space.
    fn centre(self, x: i32, y: i32) -> (f32, f32) {
        ((self.left + x) as f32 + 0.5, (self.top + y) as f32 + 0.5)
    }

    fn index(self, x: i32, y: i32) -> usize {
        (y as u32 * self.width + x as u32) as usize
    }
}

fn shape_bounds(shape: &PixelShape) -> [f32; 4] {
    let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    shape.extend_bounds(&mut bounds);
    bounds
}

/// Paints a closed outline (even-odd rule) into `out` by coverage maximum. The inside test
/// runs once per row (edge crossings), the edge distance only within 1.5 px of each edge:
/// further out the coverage ramp is already 0 or 1.
fn paint_area(points: &[(f32, f32)], frame: Frame, out: &mut [f32]) {
    let edges: Vec<_> = (0..points.len())
        .map(|i| (points[(i + points.len() - 1) % points.len()], points[i]))
        .collect();
    let mut near = vec![f32::MAX; out.len()];
    for &(a, b) in &edges {
        let bounds = [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)];
        let (columns, rows) = frame.clip(bounds, 1.5);
        for y in rows {
            for x in columns.clone() {
                let i = frame.index(x, y);
                near[i] = near[i].min(segment_distance_sq(frame.centre(x, y), a, b));
            }
        }
    }
    let bounds = points.iter().fold(
        [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
        |[x0, y0, x1, y1], &(x, y)| [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
    );
    let (columns, rows) = frame.clip(bounds, 1.5);
    let mut crossings = Vec::new();
    for y in rows {
        let py = frame.centre(0, y).1;
        crossings.clear();
        crossings.extend(
            (edges.iter())
                .filter(|&&((_, y0), (_, y1))| (y0 > py) != (y1 > py))
                .map(|&((x0, y0), (x1, y1))| x0 + (py - y0) * (x1 - x0) / (y1 - y0)),
        );
        for x in columns.clone() {
            let px = frame.centre(x, y).0;
            let inside = crossings.iter().filter(|&&cx| px < cx).count() % 2 == 1;
            let i = frame.index(x, y);
            let distance = near[i].sqrt();
            let signed = if inside { -distance } else { distance };
            out[i] = out[i].max((0.5 - signed).clamp(0.0, 1.0));
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Distance terms evaluated on this thread (segments, boxes, discs, rings): the work
    /// measure of [`rasterize`].
    pub(crate) static TERMS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[inline(always)]
fn count_term() {
    #[cfg(test)]
    TERMS.with(|terms| terms.set(terms.get() + 1));
}

/// Box-filter coverage of layered artwork. Output is deterministic.
///
/// Each shape is evaluated only over its own pixel bounds, a path by segment and an area
/// by scanline: a glyph costs about its outline length, not shapes × glyph area. The QC
/// (Cortex-A5) is slow at float code: before r49 the first Factory 1A Grid frame spent 3.4 s
/// here.
pub(crate) fn rasterize(art: &Art, place: Placement) -> Mask {
    let convert = |shapes: &[Shape]| -> Vec<PixelShape> {
        let mut out = Vec::new();
        for &shape in shapes {
            match shape {
                Shape::Path(points) => out.extend(points.windows(2).map(|pair| {
                    PixelShape::Segment(
                        place.point(pair[0].0, pair[0].1),
                        place.point(pair[1].0, pair[1].1),
                    )
                })),
                shape => out.push(PixelShape::new(shape, place)),
            }
        }
        out
    };
    let (ink, dim, cut) = (convert(art.ink), convert(art.dim), convert(art.cut));
    if ink.is_empty() && dim.is_empty() {
        return Mask {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            alpha: Vec::new(),
        };
    }
    let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for shape in ink.iter().chain(&dim) {
        shape.extend_bounds(&mut bounds);
    }
    let pad = place.radius + 1.5;
    let left = (bounds[0] - pad).floor() as i32;
    let top = (bounds[1] - pad).floor() as i32;
    let frame = Frame {
        left,
        top,
        width: ((bounds[2] + pad).ceil() as i32 - left).max(0) as u32,
        height: ((bounds[3] + pad).ceil() as i32 - top).max(0) as u32,
    };
    // Coverage of one layer: the maximum over its shapes, each only inside its padded bounds.
    let layer = |shapes: &[PixelShape], radius: f32| {
        let mut out = vec![0.0f32; (frame.width * frame.height) as usize];
        for shape in shapes {
            if let PixelShape::Area(points) = shape {
                paint_area(points, frame, &mut out);
                continue;
            }
            let (columns, rows) = frame.clip(shape_bounds(shape), radius + 1.5);
            for y in rows {
                for x in columns.clone() {
                    let i = frame.index(x, y);
                    out[i] = out[i].max(shape.coverage(frame.centre(x, y), radius));
                }
            }
        }
        out
    };
    // Knock-out lines are thinner so cut letters stay open.
    let (ink, dim, cut) = (
        layer(&ink, place.radius),
        layer(&dim, place.radius),
        layer(&cut, place.radius * 0.7),
    );
    let alpha = (0..ink.len())
        .map(|i| {
            let coverage = ink[i].max(dim[i] * art.tone) * (1.0 - cut[i]);
            (coverage * 255.0).round() as u8
        })
        .collect();
    Mask {
        left,
        top,
        width: frame.width,
        height: frame.height,
        alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arcs_respect_direction_and_lines_cover_their_centre() {
        let place = Placement {
            scale: 10.0,
            offset_x: 0.0,
            offset_y: 0.0,
            radius: 1.0,
            hint: true,
        };
        // Clockwise quarter from +x to +y (screen down) covers the lower-right
        // quadrant only; the negative sweep covers the upper-right instead.
        let lower = rasterize(&Art::ink(const { &[a(5.0, 5.0, 4.0, 0.0, 90.0)] }), place);
        let upper = rasterize(&Art::ink(const { &[a(5.0, 5.0, 4.0, 0.0, -90.0)] }), place);
        let probe = |mask: &Mask, x: i32, y: i32| {
            let (mx, my) = (x - mask.left, y - mask.top);
            if mx >= 0 && my >= 0 && (mx as u32) < mask.width && (my as u32) < mask.height {
                mask.coverage(mx as u32, my as u32)
            } else {
                0
            }
        };
        // 45 degrees below +x at radius 40 px from (50, 50).
        assert_eq!(probe(&lower, 78, 78), 255);
        assert_eq!(probe(&upper, 78, 78), 0);
        assert_eq!(probe(&upper, 78, 21), 255);
        let line = rasterize(&Art::ink(const { &[l(0.0, 0.0, 2.0, 0.0)] }), place);
        assert_eq!(probe(&line, 10, 0), 255);
        assert_eq!(probe(&line, 10, 4), 0);
    }
}
