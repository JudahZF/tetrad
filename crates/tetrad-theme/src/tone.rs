//! [`Palette::tone`]: stock colours that are not a role (the odd greys and tints of single
//! screens, many shaped by the RGB565 screen) follow the theme without a role each.

use crate::{Palette, Rgb};

/// Chroma (max − min channel) under which a stock colour counts as grey. The stock greys are
/// tinted green by up to 10 by RGB565 truncation (`#181c18`, `#1c261c`).
const GREY_CHROMA: u8 = 12;

/// The grey roles from dark to light, as stock orders them by luma.
fn ramp(p: &Palette) -> [Rgb; 11] {
    [
        p.canvas,
        p.background,
        p.surface,
        p.row_selected,
        p.panel,
        p.scrollbar,
        p.section,
        p.faint,
        p.muted,
        p.cable,
        p.text,
    ]
}

/// The hue roles a stock tint can lean toward.
const HUES: [fn(&Palette) -> Rgb; 4] = [|p| p.accent, |p| p.danger, |p| p.warning, |p| p.action];

/// Rec. 601 luma.
fn luma(c: Rgb) -> u8 {
    ((77 * c.0 as u32 + 150 * c.1 as u32 + 29 * c.2 as u32 + 128) >> 8) as u8
}

fn chroma(c: Rgb) -> u8 {
    c.0.max(c.1).max(c.2) - c.0.min(c.1).min(c.2)
}

/// The colour part of `c` above its smallest channel.
fn tint_vector(c: Rgb) -> [f32; 3] {
    let min = c.0.min(c.1).min(c.2);
    [c.0, c.1, c.2].map(|v| f32::from(v - min))
}

fn cosine(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
    let norm = |v: [f32; 3]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b)).max(f32::EPSILON)
}

impl Palette {
    /// This theme's version of `stock`, a chrome colour of the stock look that is not a role.
    /// The stock palette returns it unchanged, so the stock look stays pixel-identical.
    ///
    /// Other palettes split `stock` into a grey under a share of one stock hue role (accent,
    /// danger, warning or action), then rebuild it from this palette: the grey goes through
    /// this palette's grey roles at the same place on the stock ramp, and the share goes to
    /// this palette's version of the hue.
    pub fn tone(&self, stock: Rgb) -> Rgb {
        if *self == Self::STOCK {
            return stock;
        }
        if chroma(stock) < GREY_CHROMA {
            return self.grey(luma(stock));
        }
        let direction = tint_vector(stock);
        let Some(hue) = HUES.iter().max_by(|a, b| {
            let score =
                |role: &&fn(&Palette) -> Rgb| cosine(direction, tint_vector(role(&Self::STOCK)));
            score(a).total_cmp(&score(b))
        }) else {
            return self.grey(luma(stock));
        };
        let stock_hue = hue(&Self::STOCK);
        let share = (f32::from(chroma(stock)) / f32::from(chroma(stock_hue))).min(1.0);
        // The grey under the tint: stock = under × (1 − share) + hue × share.
        let under = if share > 0.99 {
            stock
        } else {
            let channel = |c: u8, h: u8| {
                ((f32::from(c) - share * f32::from(h)) / (1.0 - share)).clamp(0.0, 255.0) as u8
            };
            Rgb(
                channel(stock.0, stock_hue.0),
                channel(stock.1, stock_hue.1),
                channel(stock.2, stock_hue.2),
            )
        };
        self.grey(luma(under))
            .mix(hue(self), (share * 255.0).round() as u8)
    }

    /// The grey of this palette at stock luma `level`, between the two grey roles around it.
    fn grey(&self, level: u8) -> Rgb {
        let stock = ramp(&Self::STOCK).map(luma);
        let mine = ramp(self);
        let upper = stock
            .iter()
            .position(|&l| l >= level)
            .unwrap_or(stock.len() - 1);
        if upper == 0 || stock[upper] == level {
            return mine[upper];
        }
        let (low, high) = (stock[upper - 1], stock[upper]);
        let span = u32::from(high - low);
        let t = (u32::from(level - low) * 255 + span / 2) / span;
        mine[upper - 1].mix(mine[upper], t as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin;

    #[test]
    fn grey_roles_map_to_the_theme_roles() {
        let slate = builtin("slate").unwrap().palette;
        let stock = Palette::STOCK;
        assert_eq!(slate.tone(stock.panel), slate.panel);
        assert_eq!(slate.tone(stock.canvas), slate.canvas);
        assert_eq!(slate.tone(stock.text), slate.text);
        // `#181c18` (an RGB565 grey) sits between canvas and background.
        let between = slate.tone(Rgb(0x18, 0x1c, 0x18));
        assert_ne!(between, Rgb(0x18, 0x1c, 0x18));
        assert!(luma(between) >= luma(slate.background) && luma(between) <= luma(slate.surface));
    }

    #[test]
    fn tints_follow_the_theme_hue() {
        let ember = builtin("ember").unwrap().palette;
        // The stock dark accent tab fill leans to Ember's accent, not to stock green.
        let tab = ember.tone(Rgb(0x10, 0x28, 0x18));
        assert!(tab.0 > tab.1, "{tab}");
        // Hue roles map onto themselves.
        assert_eq!(ember.tone(Palette::STOCK.danger), ember.danger);
        assert_eq!(ember.tone(Palette::STOCK.action), ember.action);
    }

    #[test]
    fn stock_is_unchanged() {
        for c in [
            Rgb(0x18, 0x1c, 0x18),
            Rgb(0x51, 0x09, 0x09),
            Rgb(0xfe, 0x59, 0x48),
        ] {
            assert_eq!(Palette::STOCK.tone(c), c);
        }
    }
}
