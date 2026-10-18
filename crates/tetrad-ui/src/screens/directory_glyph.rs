//! Owned line glyphs for Directory chrome that `tetrad_assets::Icon` lacks (multi select,
//! filter, sort, cloud, heart, clock, category marks). Drawn as display-list lines on a
//! 10×10 grid scaled into a 24 px box, like the asset icons; clouds and setlist folders are
//! solid shapes. Solid stock glyphs (▼, binoculars, broken heart, PRESET and capture marks)
//! are shared `Icon`s. No stock bitmap is used.

use tetrad_assets::{Icon, IconSize, Rgb};

use crate::Rect;
use crate::layout::{Layout, Look};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Glyph {
    MultiSelect,
    Filter,
    Sort,
    Download,
    Cloud,
    CloudUp,
    Heart,
    BrokenHeart,
    Clock,
    Binoculars,
    /// Category marks: PRESET mode glyph, capture disc, IR impulse, plug.
    Presets,
    Captures,
    Irs,
    Plugins,
    /// Captures/IRs library root.
    Library,
    FolderPlus,
    /// Filled ▼ of a drop-down.
    Caret,
}

type Segment = (f32, f32, f32, f32);

const fn s(x0: f32, y0: f32, x1: f32, y1: f32) -> Segment {
    (x0, y0, x1, y1)
}

const CLOUD: &[Segment] = &[
    s(2.5, 8.0, 1.2, 7.0),
    s(1.2, 7.0, 1.2, 5.6),
    s(1.2, 5.6, 2.6, 4.6),
    s(2.6, 4.6, 3.6, 2.6),
    s(3.6, 2.6, 5.6, 2.0),
    s(5.6, 2.0, 7.2, 3.2),
    s(7.2, 3.2, 7.8, 4.6),
    s(7.8, 4.6, 8.9, 5.4),
    s(8.9, 5.4, 8.8, 7.2),
    s(8.8, 7.2, 7.6, 8.0),
];

const HEART: &[Segment] = &[
    s(5.0, 8.6, 1.4, 5.0),
    s(1.4, 5.0, 1.0, 3.4),
    s(1.0, 3.4, 1.8, 1.9),
    s(1.8, 1.9, 3.3, 1.5),
    s(3.3, 1.5, 4.4, 2.0),
    s(4.4, 2.0, 5.0, 2.9),
    s(5.0, 2.9, 5.6, 2.0),
    s(5.6, 2.0, 6.7, 1.5),
    s(6.7, 1.5, 8.2, 1.9),
    s(8.2, 1.9, 9.0, 3.4),
    s(9.0, 3.4, 8.6, 5.0),
    s(8.6, 5.0, 5.0, 8.6),
];

impl Glyph {
    fn segments(self) -> &'static [Segment] {
        match self {
            Self::MultiSelect => {
                const {
                    &[
                        s(1.0, 2.5, 6.0, 2.5),
                        s(1.0, 5.0, 6.0, 5.0),
                        s(1.0, 7.5, 4.5, 7.5),
                        s(6.0, 7.0, 7.0, 8.2),
                        s(7.0, 8.2, 9.2, 5.6),
                        s(7.4, 2.0, 8.0, 2.8),
                        s(8.0, 2.8, 9.2, 1.4),
                    ]
                }
            }
            Self::Filter => {
                const {
                    &[
                        s(1.0, 1.6, 9.0, 1.6),
                        s(1.0, 1.6, 4.1, 5.4),
                        s(9.0, 1.6, 5.9, 5.4),
                        s(4.1, 5.4, 4.1, 9.0),
                        s(5.9, 5.4, 5.9, 8.0),
                        s(4.1, 9.0, 5.9, 8.0),
                    ]
                }
            }
            Self::Sort => {
                const {
                    &[
                        s(1.0, 2.0, 3.6, 2.0),
                        s(1.0, 4.4, 4.8, 4.4),
                        s(1.0, 6.8, 6.0, 6.8),
                        s(1.0, 9.0, 9.0, 9.0),
                        s(8.0, 0.8, 8.0, 7.6),
                        s(6.8, 2.0, 8.0, 0.8),
                        s(8.0, 0.8, 9.2, 2.0),
                        s(6.8, 6.4, 8.0, 7.6),
                        s(8.0, 7.6, 9.2, 6.4),
                    ]
                }
            }
            Self::Download => {
                const {
                    &[
                        s(5.0, 1.0, 5.0, 6.6),
                        s(3.0, 4.6, 5.0, 6.6),
                        s(5.0, 6.6, 7.0, 4.6),
                        s(1.4, 6.4, 1.4, 8.8),
                        s(1.4, 8.8, 8.6, 8.8),
                        s(8.6, 8.8, 8.6, 6.4),
                    ]
                }
            }
            Self::Cloud => CLOUD,
            Self::CloudUp => {
                const {
                    &[
                        s(5.0, 8.8, 5.0, 4.6),
                        s(3.6, 6.0, 5.0, 4.6),
                        s(5.0, 4.6, 6.4, 6.0),
                    ]
                }
            }
            Self::Heart => HEART,
            Self::Clock => const { &[s(5.0, 5.0, 5.0, 2.6), s(5.0, 5.0, 6.8, 6.2)] },
            Self::Irs => {
                const {
                    &[
                        s(1.2, 2.0, 1.2, 6.0),
                        s(2.6, 1.0, 2.6, 7.0),
                        s(4.0, 4.0, 4.0, 8.6),
                        s(5.4, 5.0, 5.4, 7.6),
                        s(6.8, 5.4, 6.8, 6.8),
                        s(8.2, 5.6, 8.2, 6.4),
                        s(1.2, 6.0, 9.0, 6.0),
                    ]
                }
            }
            Self::Plugins => {
                const {
                    &[
                        s(3.5, 1.0, 3.5, 3.5),
                        s(6.5, 1.0, 6.5, 3.5),
                        s(2.0, 3.5, 8.0, 3.5),
                        s(2.0, 3.5, 2.6, 6.2),
                        s(8.0, 3.5, 7.4, 6.2),
                        s(2.6, 6.2, 7.4, 6.2),
                        s(5.0, 6.2, 5.0, 9.0),
                    ]
                }
            }
            Self::Library => {
                const {
                    &[
                        s(1.5, 1.5, 1.5, 8.5),
                        s(3.3, 1.5, 3.3, 8.5),
                        s(5.0, 1.5, 5.0, 8.5),
                        s(6.4, 1.8, 9.0, 8.3),
                    ]
                }
            }
            Self::FolderPlus => {
                const {
                    &[
                        s(3.0, 1.5, 5.0, 1.5),
                        s(5.0, 1.5, 6.0, 3.0),
                        s(6.0, 3.0, 9.0, 3.0),
                        s(9.0, 3.0, 9.0, 8.0),
                        s(9.0, 8.0, 5.6, 8.0),
                        s(3.0, 1.5, 3.0, 5.0),
                        s(0.8, 7.3, 4.0, 7.3),
                        s(2.4, 5.7, 2.4, 8.9),
                    ]
                }
            }
            Self::BrokenHeart | Self::Binoculars | Self::Presets | Self::Captures | Self::Caret => {
                &[]
            }
        }
    }

    /// Circles as (centre x, centre y, radius) on the 10×10 grid.
    fn circles(self) -> &'static [(f32, f32, f32)] {
        match self {
            Self::Clock => &[(5.0, 5.0, 4.0)],
            _ => &[],
        }
    }

    /// Glyphs drawn as a shared icon.
    fn icon(self) -> Option<Icon> {
        match self {
            Self::BrokenHeart => Some(Icon::HeartBroken),
            Self::Binoculars => Some(Icon::Binoculars),
            Self::Presets => Some(Icon::ModePreset),
            Self::Captures => Some(Icon::NeuralCapture),
            Self::Caret => Some(Icon::Caret),
            _ => None,
        }
    }
}

/// Draw `glyph` in a 24 px box centred in `area`.
pub(crate) fn glyph(l: &mut Layout, area: Rect, glyph: Glyph, color: Rgb) {
    const BOX: f32 = 24.0;
    let scale = BOX / 10.0;
    let (cx, cy) = area.center();
    let (x0, y0) = (cx as f32 - BOX / 2.0, cy as f32 - BOX / 2.0);
    if let Some(icon) = glyph.icon() {
        let size = IconSize::Medium;
        l.push(area, "", Look::Glyph { icon, size, color });
        return;
    }
    let width = 1.8;
    let point = |x: f32, y: f32| (x0 + x * scale, y0 + y * scale);
    // Stock clouds are solid (shots 054, 047); the upload arrow is cut out of the cloud.
    if matches!(glyph, Glyph::Cloud | Glyph::CloudUp) {
        let disc = |l: &mut Layout, x: f32, y: f32, w: f32, h: f32| {
            let (left, top) = point(x, y);
            let size = ((w * scale) as u32, (h * scale) as u32);
            let radius = size.0.min(size.1) as f32 / 2.0;
            l.panel(
                Rect::new(left as i32, top as i32, size.0, size.1),
                color,
                radius,
                None,
            );
        };
        disc(l, 0.8, 4.6, 8.4, 3.8);
        disc(l, 1.6, 2.8, 4.2, 4.2);
        disc(l, 3.4, 1.4, 4.6, 4.6);
        if glyph == Glyph::CloudUp {
            for &(ax, ay, bx, by) in glyph.segments() {
                l.line(point(ax, ay), point(bx, by), width, tetrad_assets::CANVAS);
            }
        }
        return;
    }
    for &(ax, ay, bx, by) in glyph.segments() {
        l.line(point(ax, ay), point(bx, by), width, color);
    }
    for &(x, y, r) in glyph.circles() {
        const STEPS: usize = 16;
        let at = |step: usize| {
            let angle = step as f32 * std::f32::consts::TAU / STEPS as f32;
            point(x + r * angle.cos(), y + r * angle.sin())
        };
        for step in 0..STEPS {
            l.line(at(step), at(step + 1), width, color);
        }
    }
}

/// Solid setlist folder with its index cut out in the page colour (shot 054).
pub(crate) fn numbered_folder(l: &mut Layout, area: Rect, index: usize, color: Rgb) {
    let (cx, cy) = area.center();
    let body = Rect::new(cx - 12, cy - 8, 24, 18);
    l.panel(Rect::new(body.x, body.y - 3, 10, 6), color, 1.5, None);
    l.panel(body, color, 2.0, None);
    l.text(
        Rect::new(body.x, body.y - 3, body.width, body.height),
        index.to_string(),
        crate::layout::CAPTION,
        tetrad_assets::CANVAS,
        crate::render::Align::Center,
    );
}
