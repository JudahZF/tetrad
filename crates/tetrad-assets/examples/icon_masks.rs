//! Prints every icon as one line: `name box left top width height alpha-hex`, for the icon
//! export of the Tetrad app (`mobile/scripts/icons.mjs`). Each icon is drawn for a 24 px box at
//! three times its size, so lines keep the weight they have on the unit.

use tetrad_assets::{Icon, render};

const PX: u32 = 24;
const SCALE: f32 = 3.0;

fn main() {
    let side = (PX as f32 * SCALE) as u32;
    for icon in Icon::ALL {
        let mask = render(icon, PX, SCALE);
        let hex: String = mask.alpha.iter().map(|a| format!("{a:02x}")).collect();
        println!(
            "{} {side} {} {} {} {} {hex}",
            kebab(&format!("{icon:?}")),
            mask.left,
            mask.top,
            mask.width,
            mask.height,
        );
    }
}

/// `NcAmpCab` → `nc-amp-cab`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}
