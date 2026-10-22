//! Boot selector frame (`tetrad select`, docs/launcher.md): two large buttons, CorOS and Tetrad,
//! with the default one marked, a countdown to the default and the Tetrad version. It is not a
//! [`crate::UiState`] screen: the selector runs before either UI, with no snapshot, no navigation
//! and no state other than [`BootMenu`]. Layout is owned.

use tetrad_assets::{CANVAS, FAINT, MUTED, Palette, TEXT};

use crate::Rect;
use crate::layout::{BODY, ButtonStyle, CAPTION, LARGE, Layout, Look, MEDIUM, XLARGE_BOLD};
use crate::render::{Align, Canvas, FULL, Frame, HEIGHT, WIDTH};

/// The UI that starts after the selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootChoice {
    /// Stock ZenUI (CorOS).
    Stock,
    /// Tetrad (`tetrad run`).
    Tetrad,
}

impl BootChoice {
    /// Left to right on the screen.
    pub const ALL: [Self; 2] = [Self::Stock, Self::Tetrad];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stock => "CorOS",
            Self::Tetrad => "Tetrad",
        }
    }

    /// The button, which is also its touch area.
    pub fn button(self) -> Rect {
        let x = match self {
            Self::Stock => 70,
            Self::Tetrad => 430,
        };
        Rect::new(x, 104, 300, 200)
    }

    /// The choice whose button holds the point.
    pub fn at(x: i32, y: i32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|choice| choice.button().contains(x, y))
    }
}

/// One frame of the selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootMenu<'a> {
    /// Marked with the selected-tab look (green outline, dark green fill); it starts when the
    /// countdown ends.
    pub default: BootChoice,
    /// Whole seconds until the default starts.
    pub seconds_left: u32,
    /// The small bottom line, for example "Tetrad 0.1.0".
    pub version: &'a str,
}

impl BootMenu<'_> {
    pub fn render(&self) -> Frame {
        // Always stock: the boot menu shows before Tetrad reads its settings.
        let mut l = Layout::new(Palette::STOCK);
        l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(CANVAS));
        let line = |y, height| Rect::new(0, y, WIDTH, height);
        l.text(
            line(48, 28),
            "Choose the UI to start",
            LARGE,
            TEXT,
            Align::Center,
        );
        for choice in BootChoice::ALL {
            let style = if choice == self.default {
                ButtonStyle::Tab { selected: true }
            } else {
                ButtonStyle::Secondary
            };
            let look = Look::Button {
                style,
                icon: None,
                font: Some(XLARGE_BOLD),
            };
            l.push(choice.button(), choice.label(), look);
        }
        let countdown = format!("{} starts in {} s", self.default.label(), self.seconds_left);
        l.text(line(340, 26), countdown, MEDIUM, TEXT, Align::Center);
        l.text(
            line(384, 22),
            "Touch a button to start that UI now. The last choice is the default.",
            BODY,
            MUTED,
            Align::Center,
        );
        l.text(line(448, 18), self.version, CAPTION, FAINT, Align::Center);
        let mut frame = Frame::new();
        crate::draw::paint_region(
            &mut Canvas::new(&mut frame, l.palette),
            &l.items,
            FULL,
            |item| (item, false),
        );
        frame
    }
}

#[cfg(test)]
mod tests {
    use tetrad_assets::{ACCENT, SECTION};

    use super::*;
    use crate::draw::TAB_ACTIVE;

    fn menu(default: BootChoice, seconds_left: u32) -> Frame {
        BootMenu {
            default,
            seconds_left,
            version: "Tetrad 0.1.0",
        }
        .render()
    }

    /// Pixels of `frame` inside `rect`.
    fn pixels(frame: &Frame, rect: Rect) -> impl Iterator<Item = tetrad_assets::Rgb> + '_ {
        (rect.y..rect.bottom())
            .flat_map(move |y| (rect.x..rect.right()).map(move |x| frame.pixel(x as u32, y as u32)))
    }

    /// The default button has the selected-tab fill and green outline, the other the raised
    /// grey fill without an outline; both carry their white label. A new default swaps them.
    #[test]
    fn the_default_button_is_marked() {
        for default in BootChoice::ALL {
            let frame = menu(default, 5);
            for choice in BootChoice::ALL {
                let button = choice.button();
                let corner = frame.pixel(button.x as u32 + 12, button.y as u32 + 12);
                let edge: Vec<_> = pixels(&frame, Rect::new(button.x, button.y + 90, 2, 20))
                    .filter(|&pixel| pixel == ACCENT)
                    .collect();
                if choice == default {
                    assert_eq!(corner, TAB_ACTIVE, "{choice:?}");
                    assert!(!edge.is_empty(), "{choice:?} has no green outline");
                } else {
                    assert_eq!(corner, SECTION, "{choice:?}");
                    assert!(edge.is_empty(), "{choice:?} has a green outline");
                }
                assert!(pixels(&frame, button).any(|pixel| pixel == TEXT));
            }
        }
    }

    /// The countdown changes only its own line; the buttons are the touch areas and the gap
    /// between them picks nothing.
    #[test]
    fn the_countdown_redraws_one_line_and_buttons_take_touches() {
        let (five, four) = (menu(BootChoice::Tetrad, 5), menu(BootChoice::Tetrad, 4));
        let changed: Vec<u32> = (0..HEIGHT)
            .filter(|&y| (0..WIDTH).any(|x| five.pixel(x, y) != four.pixel(x, y)))
            .collect();
        assert!(!changed.is_empty());
        assert!(
            changed.iter().all(|y| (336..370).contains(y)),
            "{changed:?}"
        );
        for choice in BootChoice::ALL {
            let (x, y) = choice.button().center();
            assert_eq!(BootChoice::at(x, y), Some(choice));
        }
        assert_eq!(BootChoice::at(400, 200), None);
        assert_eq!(BootChoice::at(200, 400), None);
    }
}
