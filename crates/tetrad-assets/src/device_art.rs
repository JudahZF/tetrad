//! Owned redraws of the stock block glyph set `icons/virtual devices/*.png` and
//! `icons/looper/looper_icon.png` (32 × 32, white with grey parts), drawn on the same 32 px grid
//! so a 32 px box matches the stock pixels (QC screenshots, 2026-10-16). Shapes are traced from
//! measurements; no stock pixel is stored. `0x002eb99c` picks the glyph for a block
//! (`tetrad_ui::BlockInfo::icon`).

use crate::icon::Icon;
use crate::raster::{Art, Shape, a, circle, disc, fill, l, path, pie, rect};

/// Design grid of the device glyphs (stock pixels).
pub(crate) const GRID: f32 = 32.0;

/// The grey parts of most glyphs: opaque `#959595` in the stock images.
const GREY: f32 = 149.0 / 255.0;
/// The darker grey of `NC.png` and `multivoicer.png`: opaque `#525252`.
const DARK_GREY: f32 = 82.0 / 255.0;

/// The rounded square of the sidechain marks, on the 32 grid at half scale.
const MARK: &[Shape] = &[
    rect(0.0, 10.0, 32.0, 22.0),
    rect(10.0, 0.0, 22.0, 32.0),
    disc(10.0, 10.0, 10.0),
    disc(22.0, 10.0, 10.0),
    disc(10.0, 22.0, 10.0),
    disc(22.0, 22.0, 10.0),
];

/// The 32-grid art of a device glyph, or `None` for other icons.
pub(crate) fn art(icon: Icon) -> Option<Art> {
    let art = match icon {
        // `AMP.png`: a head with round corners, its outline split by one bar.
        Icon::Amp => Art::ink(
            const {
                &[
                    rect(3.0, 7.0, 29.0, 9.0),
                    rect(3.0, 23.0, 29.0, 25.0),
                    rect(1.0, 9.0, 3.0, 23.0),
                    rect(29.0, 9.0, 31.0, 23.0),
                    rect(1.0, 17.0, 31.0, 19.0),
                    pie(3.0, 9.0, 2.0, 180.0, 90.0),
                    pie(29.0, 9.0, 2.0, 270.0, 90.0),
                    pie(29.0, 23.0, 2.0, 0.0, 90.0),
                    pie(3.0, 23.0, 2.0, 90.0, 90.0),
                ]
            },
        ),
        // `comboamp.png`: a combo cabinet with its control strip and a knob.
        Icon::ComboAmp => Art::ink(
            const {
                &[
                    rect(1.0, 1.0, 31.0, 3.0),
                    rect(1.0, 29.0, 31.0, 31.0),
                    rect(1.0, 1.0, 3.0, 31.0),
                    rect(29.0, 1.0, 31.0, 31.0),
                    rect(1.0, 10.0, 31.0, 12.0),
                    disc(25.5, 6.5, 1.7),
                ]
            },
        ),
        // `NC_ampcab.png`: a head over a cabinet with one speaker.
        Icon::NcAmpCab => Art::ink(
            const {
                &[
                    rect(3.0, 0.0, 28.0, 2.0),
                    rect(3.0, 0.0, 5.0, 6.0),
                    rect(26.0, 0.0, 28.0, 6.0),
                    rect(0.0, 8.0, 32.0, 10.0),
                    rect(0.0, 30.0, 32.0, 32.0),
                    rect(0.0, 8.0, 2.0, 32.0),
                    rect(30.0, 8.0, 32.0, 32.0),
                    circle(16.0, 20.0, 7.0),
                    disc(16.0, 20.0, 2.65),
                ]
            },
        ),
        // `NC.png`: a disc of stripes, bright on the right, dim and offset on the left.
        Icon::NeuralCapture => Art {
            ink: const { &[pie(16.0, 16.0, 16.0, 270.0, 180.0)] },
            dim: const { &[pie(16.0, 16.0, 16.0, 90.0, 180.0)] },
            tone: DARK_GREY,
            cut: const {
                &[
                    rect(16.0, 3.0, 32.0, 6.0),
                    rect(16.0, 9.0, 32.0, 12.0),
                    rect(16.0, 15.0, 32.0, 18.0),
                    rect(16.0, 21.0, 32.0, 24.0),
                    rect(16.0, 27.0, 32.0, 30.0),
                    rect(0.0, 0.0, 16.0, 3.0),
                    rect(0.0, 6.0, 16.0, 9.0),
                    rect(0.0, 12.0, 16.0, 15.0),
                    rect(0.0, 18.0, 16.0, 21.0),
                    rect(0.0, 24.0, 16.0, 27.0),
                    rect(0.0, 30.0, 16.0, 32.0),
                ]
            },
        },
        // `NC_pedal.png`: a stomp box with three knobs, a big knob and a switch.
        Icon::NcPedal => Art {
            ink: const {
                &[
                    rect(6.0, 1.0, 25.0, 3.0),
                    rect(6.0, 29.0, 24.0, 31.0),
                    rect(5.0, 2.0, 7.0, 29.0),
                    rect(24.0, 3.0, 26.0, 30.0),
                    rect(4.0, 8.0, 7.0, 15.0),
                    rect(24.0, 8.0, 27.0, 15.0),
                    disc(10.5, 6.45, 1.45),
                    disc(15.5, 6.45, 1.45),
                    disc(20.55, 6.5, 1.45),
                    disc(15.5, 15.5, 4.5),
                    disc(15.5, 24.55, 1.45),
                ]
            },
            dim: &[],
            tone: 0.0,
            cut: const { &[l(15.5, 11.7, 15.5, 14.3)] },
        },
        // `cabsim.png`: a speaker ring and cone; the corner screws are dim.
        Icon::Cab => Art {
            ink: const { &[circle(16.0, 16.0, 12.0), disc(16.0, 16.0, 4.0)] },
            dim: const {
                &[
                    disc(3.0, 3.0, 2.0),
                    disc(29.0, 3.0, 2.0),
                    disc(3.0, 29.0, 2.0),
                    disc(29.0, 29.0, 2.0),
                ]
            },
            tone: GREY,
            cut: &[],
        },
        // `overdrive.png`: one clipped wave.
        Icon::Overdrive => Art::ink(
            const {
                &[path(&[
                    (2.0, 15.8),
                    (6.0, 4.0),
                    (12.0, 4.0),
                    (20.0, 28.0),
                    (26.0, 28.0),
                    (30.0, 16.0),
                ])]
            },
        ),
        // `fuzz.png`: the word FUZZ as one square wave; the Z diagonals are dim.
        Icon::Fuzz => Art {
            ink: const {
                &[
                    rect(2.0, 2.0, 4.0, 30.0),
                    rect(0.0, 28.0, 4.0, 30.0),
                    rect(2.0, 2.0, 7.0, 4.0),
                    rect(2.0, 12.0, 7.0, 14.0),
                    rect(9.0, 2.0, 11.0, 30.0),
                    rect(9.0, 28.0, 16.0, 30.0),
                    rect(14.0, 2.0, 16.0, 30.0),
                    rect(18.0, 2.0, 22.0, 4.0),
                    rect(20.0, 2.0, 22.0, 30.0),
                    rect(20.0, 28.0, 26.0, 30.0),
                    rect(24.0, 2.0, 30.0, 4.0),
                    rect(28.0, 2.0, 30.0, 30.0),
                    rect(28.0, 28.0, 32.0, 30.0),
                ]
            },
            dim: const {
                &[
                    rect(7.0, 2.0, 9.0, 4.0),
                    rect(16.0, 2.0, 18.0, 4.0),
                    rect(24.0, 4.0, 26.0, 28.0),
                ]
            },
            tone: 97.0 / 255.0,
            cut: &[],
        },
        // `delay.png`: a wave and its dim repeat; the repeat stops where it goes behind the wave.
        Icon::Delay => Art {
            ink: const {
                &[path(&[
                    (1.95, 4.25),
                    (3.0, 6.2),
                    (4.0, 9.75),
                    (5.0, 14.95),
                    (6.0, 20.75),
                    (7.0, 24.8),
                    (8.0, 27.25),
                    (9.8, 28.1),
                    (11.0, 26.65),
                    (12.0, 23.6),
                    (13.0, 19.0),
                    (14.0, 13.05),
                    (14.6, 10.1),
                    (15.25, 7.55),
                    (16.45, 4.4),
                    (18.0, 3.9),
                    (19.4, 5.4),
                    (20.0, 7.45),
                    (21.0, 11.25),
                    (21.8, 15.9),
                    (22.5, 19.75),
                    (23.3, 23.55),
                    (24.2, 26.25),
                    (25.15, 27.85),
                ])]
            },
            dim: const {
                &[
                    path(&[
                        (6.85, 4.1),
                        (7.75, 5.65),
                        (8.6, 8.1),
                        (9.4, 11.75),
                        (10.05, 15.15),
                        (9.85, 17.5),
                    ]),
                    path(&[
                        (13.85, 28.0),
                        (15.95, 26.75),
                        (16.6, 25.0),
                        (17.4, 21.9),
                        (18.0, 18.85),
                        (18.7, 14.35),
                        (18.65, 14.3),
                    ]),
                    path(&[
                        (22.65, 3.95),
                        (23.15, 4.05),
                        (24.35, 5.35),
                        (25.3, 8.45),
                        (26.0, 11.3),
                        (26.6, 14.75),
                        (27.1, 17.6),
                        (27.6, 20.2),
                        (28.1, 22.55),
                        (28.7, 24.95),
                        (29.5, 26.8),
                        (30.15, 27.85),
                    ]),
                ]
            },
            tone: GREY,
            cut: &[],
        },
        // `reverb.png`: a wireframe cube; the hidden edges are dim.
        Icon::Reverb => Art {
            ink: const {
                &[
                    rect(2.0, 11.0, 21.0, 13.0),
                    rect(2.0, 11.0, 4.0, 31.0),
                    rect(2.0, 29.0, 21.0, 31.0),
                    l(20.5, 12.5, 20.5, 29.5),
                    rect(12.0, 1.0, 32.0, 3.0),
                    rect(30.0, 1.0, 32.0, 21.0),
                    l(12.4, 2.0, 3.2, 11.5),
                    l(30.95, 2.0, 20.95, 12.0),
                    l(31.55, 18.9, 21.1, 30.0),
                ]
            },
            dim: const {
                &[
                    l(12.35, 2.0, 12.4, 20.25),
                    rect(12.0, 19.0, 30.0, 21.0),
                    l(12.45, 20.0, 3.9, 29.6),
                ]
            },
            tone: GREY,
            cut: &[],
        },
        // `comp.png`: one wave period between two arrows that press it.
        Icon::Compressor => Art::ink(
            const {
                &[
                    path(&[
                        (2.25, 15.25),
                        (3.0, 13.35),
                        (4.1, 11.75),
                        (5.0, 10.75),
                        (5.85, 9.95),
                        (6.9, 9.4),
                        (7.6, 9.1),
                        (9.6, 9.0),
                        (10.05, 9.1),
                        (11.5, 9.4),
                        (11.95, 9.95),
                        (12.95, 10.75),
                        (14.25, 12.4),
                        (15.5, 14.65),
                        (16.0, 16.1),
                        (17.0, 18.3),
                        (18.05, 20.0),
                        (19.0, 21.2),
                        (20.0, 22.05),
                        (20.55, 22.6),
                        (21.9, 22.9),
                        (22.4, 23.0),
                        (24.55, 22.9),
                        (25.1, 22.6),
                        (26.15, 22.05),
                        (27.0, 21.25),
                        (27.9, 20.25),
                        (28.95, 18.75),
                        (29.75, 16.75),
                    ]),
                    fill(&[(13.0, 2.0), (19.0, 2.0), (16.0, 6.7)]),
                    fill(&[(16.0, 25.3), (19.0, 30.0), (13.0, 30.0)]),
                ]
            },
        ),
        // `pitch.png`: an S-curve over dim axes.
        Icon::Pitch => Art {
            ink: const {
                &[path(&[
                    (3.55, 28.0),
                    (6.15, 27.6),
                    (8.4, 26.6),
                    (10.25, 25.3),
                    (12.0, 23.6),
                    (13.2, 22.1),
                    (20.65, 10.15),
                    (22.2, 8.2),
                    (24.15, 6.4),
                    (26.25, 5.0),
                    (28.65, 4.2),
                    (30.1, 4.05),
                ])]
            },
            dim: const { &[rect(1.0, 3.0, 3.0, 29.0), rect(1.0, 15.0, 31.0, 17.0)] },
            tone: GREY,
            cut: &[],
        },
        // `multivoicer.png`: five voices, each a dot over a dim bar.
        Icon::Multivoicer => Art {
            ink: const {
                &[
                    disc(16.0, 4.0, 2.0),
                    disc(4.0, 8.0, 2.0),
                    disc(28.0, 8.0, 2.0),
                    disc(10.0, 13.0, 2.0),
                    disc(22.0, 13.0, 2.0),
                ]
            },
            dim: const {
                &[
                    rect(15.0, 8.0, 17.0, 30.0),
                    rect(3.0, 12.0, 5.0, 30.0),
                    rect(27.0, 12.0, 29.0, 30.0),
                    rect(9.0, 17.0, 11.0, 30.0),
                    rect(21.0, 17.0, 23.0, 30.0),
                ]
            },
            tone: DARK_GREY,
            cut: &[],
        },
        // `modulation.png`: a decaying wave.
        Icon::Modulation => Art::ink(
            const {
                &[path(&[
                    (2.15, 4.3),
                    (2.6, 5.8),
                    (3.2, 9.0),
                    (3.8, 13.25),
                    (4.3, 17.05),
                    (4.9, 21.2),
                    (5.4, 24.15),
                    (6.2, 26.75),
                    (7.15, 28.15),
                    (8.6, 26.8),
                    (9.3, 24.55),
                    (9.9, 21.7),
                    (10.6, 17.3),
                    (11.3, 12.9),
                    (11.9, 10.0),
                    (12.65, 7.45),
                    (13.85, 6.2),
                    (15.3, 7.25),
                    (16.1, 9.85),
                    (16.8, 13.0),
                    (17.4, 16.2),
                    (18.2, 19.7),
                    (19.0, 21.9),
                    (20.05, 22.45),
                    (21.0, 22.15),
                    (21.8, 20.25),
                    (22.4, 18.05),
                    (23.1, 15.0),
                    (24.1, 12.7),
                    (26.0, 12.5),
                    (26.9, 13.45),
                    (27.75, 15.65),
                    (28.25, 17.35),
                    (29.05, 18.25),
                ])]
            },
        ),
        // `morph.png`: an alien head with two slanted eyes.
        Icon::Morph => Art::ink(
            const {
                &[
                    path(&[
                        (4.65, 16.6),
                        (4.05, 14.1),
                        (4.1, 10.1),
                        (6.85, 6.0),
                        (9.25, 4.0),
                        (12.0, 2.65),
                        (15.05, 1.95),
                        (18.2, 2.2),
                        (20.9, 3.1),
                        (23.75, 4.7),
                        (25.75, 6.65),
                        (26.9, 8.3),
                        (28.05, 11.6),
                        (27.7, 15.7),
                    ]),
                    path(&[
                        (5.15, 18.0),
                        (6.75, 21.25),
                        (8.5, 23.5),
                        (11.75, 27.0),
                        (15.25, 29.95),
                    ]),
                    path(&[
                        (27.25, 16.9),
                        (26.0, 19.7),
                        (23.6, 23.5),
                        (20.25, 27.0),
                        (16.8, 29.9),
                    ]),
                    l(7.9, 13.95, 14.0, 21.0),
                    path(&[
                        (8.0, 14.1),
                        (9.7, 14.4),
                        (13.2, 17.25),
                        (13.3, 17.75),
                        (14.2, 21.05),
                    ]),
                    path(&[
                        (8.8, 14.25),
                        (8.15, 14.95),
                        (9.3, 18.3),
                        (11.6, 20.2),
                        (13.1, 20.8),
                    ]),
                    l(24.05, 13.9, 18.05, 21.05),
                    path(&[
                        (23.4, 14.25),
                        (22.4, 14.4),
                        (19.75, 16.0),
                        (18.65, 17.85),
                        (17.8, 21.05),
                    ]),
                    path(&[
                        (23.2, 14.05),
                        (24.0, 14.85),
                        (22.6, 18.45),
                        (20.75, 20.15),
                        (18.25, 20.9),
                    ]),
                    disc(10.85, 16.25, 2.2),
                    disc(20.85, 17.9, 2.7),
                ]
            },
        ),
        // `freeze.png`: a snowflake.
        Icon::Freeze => Art::ink(
            const {
                &[
                    rect(15.0, 1.0, 17.0, 12.0),
                    rect(15.0, 20.0, 17.0, 31.0),
                    l(19.3, 14.0, 28.2, 8.85),
                    l(19.4, 18.0, 28.05, 23.1),
                    l(12.35, 18.0, 3.35, 23.25),
                    l(12.35, 14.0, 3.5, 8.9),
                    l(15.4, 7.5, 11.55, 3.55),
                    l(16.6, 7.5, 20.45, 3.55),
                    l(15.7, 24.55, 20.3, 29.05),
                    l(15.3, 25.5, 12.0, 28.8),
                    l(23.35, 11.8, 25.0, 5.65),
                    l(23.95, 11.75, 29.45, 13.3),
                    l(24.0, 20.25, 29.5, 18.7),
                    l(23.45, 20.6, 24.9, 26.2),
                    l(8.65, 21.0, 7.35, 25.95),
                    l(9.1, 20.05, 2.95, 18.4),
                    l(9.3, 11.85, 2.9, 13.65),
                    l(8.65, 11.1, 7.35, 6.0),
                    path(&[
                        (15.9, 12.0),
                        (19.55, 14.15),
                        (19.45, 17.85),
                        (16.2, 19.75),
                        (12.45, 18.1),
                        (12.55, 14.0),
                        (16.15, 12.0),
                    ]),
                ]
            },
        ),
        // `BitCrusher.png`: a pixel space invader.
        Icon::BitCrusher => Art {
            ink: const {
                &[
                    rect(11.0, 4.0, 13.0, 7.0),
                    rect(19.0, 4.0, 21.0, 7.0),
                    rect(7.0, 7.0, 25.0, 20.0),
                    rect(1.0, 14.0, 3.0, 20.0),
                    rect(29.0, 14.0, 31.0, 20.0),
                    rect(1.0, 18.0, 31.0, 20.0),
                    rect(7.0, 20.0, 12.0, 22.0),
                    rect(14.0, 20.0, 18.0, 22.0),
                    rect(20.0, 20.0, 25.0, 22.0),
                    rect(10.0, 22.0, 12.0, 26.0),
                    rect(20.0, 22.0, 22.0, 26.0),
                    rect(7.0, 26.0, 12.0, 28.0),
                    rect(20.0, 26.0, 26.0, 28.0),
                ]
            },
            dim: &[],
            tone: 0.0,
            cut: const { &[disc(11.3, 10.3, 1.4), disc(20.7, 10.3, 1.4)] },
        },
        // `synth.png`: a keyboard under a control strip.
        Icon::Synth => Art::ink(
            const {
                &[
                    rect(2.0, 2.0, 30.0, 4.0),
                    rect(2.0, 6.0, 30.0, 8.0),
                    rect(2.0, 2.0, 4.0, 8.0),
                    rect(28.0, 2.0, 30.0, 8.0),
                    rect(6.0, 4.0, 8.0, 6.0),
                    rect(10.0, 4.0, 18.0, 6.0),
                    rect(2.0, 10.0, 5.0, 24.0),
                    rect(11.0, 10.0, 13.0, 24.0),
                    rect(19.0, 10.0, 21.0, 24.0),
                    rect(27.0, 10.0, 30.0, 24.0),
                    rect(2.0, 24.0, 7.0, 30.0),
                    rect(9.0, 24.0, 15.0, 30.0),
                    rect(17.0, 24.0, 23.0, 30.0),
                    rect(25.0, 24.0, 30.0, 30.0),
                ]
            },
        ),
        // `filter.png`: a low-pass response.
        Icon::Filter => {
            Art::ink(const { &[rect(0.0, 10.0, 23.0, 12.0), l(22.7, 11.5, 30.5, 25.0)] })
        }
        // `EQ.png`: three faders, each broken by its knob.
        Icon::Eq => Art::ink(
            const {
                &[
                    rect(3.0, 1.0, 5.0, 7.0),
                    rect(3.0, 17.0, 5.0, 31.0),
                    disc(4.0, 12.0, 3.0),
                    rect(15.0, 1.0, 17.0, 16.0),
                    rect(15.0, 26.0, 17.0, 31.0),
                    disc(16.0, 21.0, 3.0),
                    rect(27.0, 1.0, 29.0, 11.0),
                    rect(27.0, 21.0, 29.0, 31.0),
                    disc(28.0, 16.0, 3.0),
                ]
            },
        ),
        // `ir.png`: an impulse and its reflections as bars above and below a line.
        Icon::IrLoader => Art::ink(
            const {
                &[
                    rect(3.0, 2.0, 5.0, 18.0),
                    rect(7.0, 9.0, 9.0, 18.0),
                    rect(23.0, 10.0, 25.0, 18.0),
                    rect(27.0, 14.0, 29.0, 18.0),
                    rect(11.0, 18.0, 13.0, 25.0),
                    rect(15.0, 18.0, 17.0, 30.0),
                    rect(19.0, 18.0, 21.0, 22.0),
                ]
            },
        ),
        // `wah.png`: a treadle in its frame; the frame is dim.
        Icon::Wah => Art {
            ink: const {
                &[
                    rect(10.0, 1.0, 22.0, 3.0),
                    l(11.05, 2.6, 11.75, 24.45),
                    l(21.0, 2.6, 20.15, 26.05),
                    rect(11.0, 25.0, 21.0, 27.0),
                ]
            },
            dim: const {
                &[
                    rect(6.0, 3.0, 11.0, 5.0),
                    rect(21.0, 3.0, 26.0, 5.0),
                    l(7.05, 4.0, 8.55, 30.05),
                    l(25.0, 3.9, 23.4, 30.15),
                    rect(8.0, 29.0, 24.0, 31.0),
                ]
            },
            tone: GREY,
            cut: &[],
        },
        // `loop.png`: two arrows in a loop.
        Icon::FxLoop => Art::ink(
            const {
                &[
                    a(9.0, 16.0, 7.0, 270.0, -180.0),
                    rect(8.0, 8.0, 14.0, 10.0),
                    rect(8.0, 22.0, 11.0, 24.0),
                    fill(&[(13.0, 4.8), (18.8, 9.0), (13.0, 13.2)]),
                    a(23.0, 16.0, 7.0, 270.0, 180.0),
                    rect(21.0, 8.0, 24.0, 10.0),
                    rect(18.0, 22.0, 24.0, 24.0),
                    fill(&[(19.2, 18.6), (13.2, 23.0), (19.2, 27.4)]),
                ]
            },
        ),
        // `send.png`: the loop with only its send arrow lit.
        Icon::FxSend => Art {
            ink: const {
                &[
                    a(9.0, 16.0, 7.0, 270.0, -180.0),
                    rect(8.0, 8.0, 14.0, 10.0),
                    rect(8.0, 22.0, 11.0, 24.0),
                    fill(&[(13.0, 4.8), (18.8, 9.0), (13.0, 13.2)]),
                ]
            },
            dim: const {
                &[
                    a(23.0, 16.0, 7.0, 270.0, 180.0),
                    rect(21.0, 8.0, 24.0, 10.0),
                    rect(18.0, 22.0, 24.0, 24.0),
                    fill(&[(19.2, 18.6), (13.2, 23.0), (19.2, 27.4)]),
                ]
            },
            tone: 0.15,
            cut: &[],
        },
        // `return.png`: the loop with only its return arrow lit.
        Icon::FxReturn => Art {
            ink: const {
                &[
                    a(23.0, 16.0, 7.0, 270.0, 180.0),
                    rect(21.0, 8.0, 24.0, 10.0),
                    rect(18.0, 22.0, 24.0, 24.0),
                    fill(&[(19.2, 18.6), (13.2, 23.0), (19.2, 27.4)]),
                ]
            },
            dim: const {
                &[
                    a(9.0, 16.0, 7.0, 270.0, -180.0),
                    rect(8.0, 8.0, 14.0, 10.0),
                    rect(8.0, 22.0, 11.0, 24.0),
                    fill(&[(13.0, 4.8), (18.8, 9.0), (13.0, 13.2)]),
                ]
            },
            tone: 0.15,
            cut: &[],
        },
        // `icons/looper/looper_icon.png`: two tape reels over the tape path.
        Icon::LooperReels => Art::ink(
            const {
                &[
                    circle(8.0, 13.0, 7.0),
                    disc(8.0, 13.0, 2.0),
                    circle(24.0, 13.0, 7.0),
                    disc(24.0, 13.0, 2.0),
                    disc(3.4, 10.45, 1.25),
                    disc(12.7, 10.45, 1.3),
                    disc(8.1, 18.15, 1.2),
                    disc(23.95, 7.85, 1.2),
                    disc(19.3, 15.55, 1.3),
                    disc(28.7, 15.55, 1.3),
                    l(2.5, 18.25, 3.9, 21.8),
                    l(29.1, 19.4, 28.1, 21.65),
                    disc(6.0, 26.0, 2.0),
                    disc(26.0, 26.0, 2.0),
                    l(11.0, 26.0, 21.05, 26.0),
                ]
            },
        ),
        // `blend.png`: a solid disc over a ring; their overlap is hatched inside the disc edge. The
        // two short lines fill the corners where the disc meets the ring.
        Icon::Blend => Art::ink(
            const {
                &[
                    pie(12.5, 12.5, 11.5, 117.0, 217.0),
                    a(12.5, 12.45, 10.5, -35.0, 190.0),
                    circle(19.5, 19.5, 10.5),
                    l(23.8, 10.3, 22.6, 10.25),
                    l(16.85, 10.8, 22.25, 16.35),
                    l(12.85, 12.8, 19.95, 20.05),
                    l(10.85, 16.8, 16.4, 22.35),
                    l(9.6, 23.2, 14.3, 22.9),
                ]
            },
        ),
        // `doubler.png`: two bar waveforms, one above the other.
        Icon::Doubler => Art::ink(
            const {
                &[
                    rect(1.0, 6.0, 3.0, 10.0),
                    rect(5.0, 5.0, 7.0, 11.0),
                    rect(9.0, 1.0, 11.0, 15.0),
                    rect(13.0, 4.0, 15.0, 12.0),
                    rect(17.0, 5.0, 19.0, 11.0),
                    rect(21.0, 6.0, 23.0, 10.0),
                    rect(25.0, 7.0, 27.0, 9.0),
                    rect(29.0, 7.0, 31.0, 9.0),
                    rect(1.0, 23.0, 3.0, 25.0),
                    rect(5.0, 22.0, 7.0, 26.0),
                    rect(9.0, 21.0, 11.0, 27.0),
                    rect(13.0, 22.0, 15.0, 26.0),
                    rect(17.0, 19.0, 19.0, 29.0),
                    rect(21.0, 17.0, 23.0, 31.0),
                    rect(25.0, 20.0, 27.0, 28.0),
                    rect(29.0, 22.0, 31.0, 26.0),
                ]
            },
        ),
        // `utility.png`: a fan of dim blades around a bright handle with a ball at each end.
        Icon::Utility => Art {
            ink: const {
                &[
                    l(5.85, 4.0, 4.45, 24.0),
                    l(8.15, 4.15, 9.55, 24.0),
                    disc(7.0, 4.25, 2.2),
                    disc(7.0, 26.3, 3.7),
                    disc(8.15, 26.6, 3.3),
                ]
            },
            dim: const {
                &[
                    path(&[
                        (10.2, 28.1),
                        (13.5, 20.65),
                        (15.5, 16.1),
                        (17.5, 11.9),
                        (18.8, 8.95),
                        (19.4, 7.6),
                        (19.6, 6.2),
                        (17.8, 5.7),
                        (16.1, 7.8),
                        (14.1, 10.0),
                        (12.5, 11.5),
                        (12.1, 12.45),
                    ]),
                    path(&[
                        (17.35, 16.55),
                        (18.75, 15.8),
                        (22.15, 14.6),
                        (26.55, 12.9),
                        (27.25, 13.05),
                        (28.0, 14.0),
                        (27.5, 14.8),
                        (25.25, 16.5),
                        (22.4, 18.5),
                        (19.8, 20.3),
                        (17.4, 22.0),
                        (15.6, 23.3),
                        (12.35, 25.55),
                    ]),
                    path(&[
                        (19.35, 24.25),
                        (20.5, 24.2),
                        (22.7, 24.8),
                        (25.9, 25.6),
                        (29.0, 26.8),
                        (29.0, 28.05),
                    ]),
                    rect(10.0, 28.0, 29.0, 30.0),
                ]
            },
            tone: GREY,
            cut: &[],
        },
        // `multiple_stomps.png`: three stacked 2 px rounded boxes (Gig View, a stomp with several
        // blocks). The back and middle boxes show only their top corners and right side (the stock
        // image has no ink on their other edges); the front box is whole. Straight edges are
        // `rect`s on the pixel grid (a thin `l` stroke snaps to pixel centres and goes grey at its
        // edges); corners are arcs of radius 2 on the outline's centre line.
        Icon::MultipleDevices => Art::ink(
            const {
                &[
                    rect(19.0, 0.0, 29.0, 2.0),
                    a(19.0, 3.0, 2.0, 180.0, 90.0),
                    a(29.0, 3.0, 2.0, 270.0, 90.0),
                    rect(30.0, 3.0, 32.0, 13.0),
                    a(29.0, 13.0, 2.0, 0.0, 90.0),
                    rect(12.0, 8.0, 22.0, 10.0),
                    a(12.0, 11.0, 2.0, 180.0, 90.0),
                    a(22.0, 11.0, 2.0, 270.0, 90.0),
                    rect(23.0, 11.0, 25.0, 21.0),
                    a(22.0, 21.0, 2.0, 0.0, 90.0),
                    rect(20.0, 22.0, 22.0, 24.0),
                    rect(4.0, 15.0, 14.0, 17.0),
                    a(4.0, 18.0, 2.0, 180.0, 90.0),
                    a(14.0, 18.0, 2.0, 270.0, 90.0),
                    rect(15.0, 18.0, 17.0, 28.0),
                    a(14.0, 28.0, 2.0, 0.0, 90.0),
                    rect(4.0, 29.0, 14.0, 31.0),
                    a(4.0, 28.0, 2.0, 90.0, 90.0),
                    rect(1.0, 18.0, 3.0, 28.0),
                ]
            },
        ),
        // `icons/24/sidechain_source.png` as the Grid shows it (20 px box, `st5a.png`): a 16 px
        // square with 5 px corners; the dot and the ▶ are cut out.
        Icon::SidechainSource => Art {
            ink: MARK,
            dim: &[],
            tone: 0.0,
            cut: const {
                &[
                    disc(8.0, 16.0, 4.0),
                    fill(&[(16.0, 8.0), (28.0, 16.0), (16.0, 24.0)]),
                ]
            },
        },
        // `sidechain_sink.png`: the source mark mirrored, ▶●.
        Icon::SidechainSink => Art {
            ink: MARK,
            dim: &[],
            tone: 0.0,
            cut: const {
                &[
                    fill(&[(4.0, 8.0), (16.0, 16.0), (4.0, 24.0)]),
                    disc(24.0, 16.0, 4.0),
                ]
            },
        },
        _ => return None,
    };
    Some(art)
}
