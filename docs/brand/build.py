"""Build the Tetrad logo files in this folder.

The logo is a Grid block: a 64 px tile with a 2 px ring in the stock green, holding a plectrum
cut into the three visible faces of a tetrahedron. The faces take three block category
colours. With the ring that makes four colours, a tetrad.

Run from anywhere: uvx --with uharfbuzz --from fonttools python docs/brand/build.py
Needs rsvg-convert for the PNG.
"""

import subprocess
from pathlib import Path

import uharfbuzz as hb
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

HERE = Path(__file__).resolve().parent
FONT = HERE.parents[1] / "crates/tetrad-assets/fonts/ibm-plex-sans/IBMPlexSans-Bold.ttf"

RING = "#45f862"  # stock accent (dynamics block)
# Faces, lit from above: the top is brightest and the right darkest, so the solid reads.
LEFT, TOP, RIGHT = "#ff7000", "#ffd236", "#6954ff"  # drive, pitch, cab
TEXT = "#ffffff"

# The pick on a 48 grid: a 351 shape (about 0.9 wide per tall), broad shoulders, round tip.
# Box x 5..43, y 3.5..44.8.
PICK = (
    "M24 44.8C21.2 44.8 19.6 42.9 17.4 39.6C12 31.5 5 22 5 14.5C5 8 12 3.5 24 3.5"
    "C36 3.5 43 8 43 14.5C43 22 36 31.5 30.6 39.6C28.4 42.9 26.8 44.8 24 44.8Z"
)
APEX = (28.5, 19.0)
CORNERS = ((8.2, 8.2), (39.8, 8.2), (24.0, 44.8))  # left shoulder, right shoulder, tip


def pt(p: tuple[float, float]) -> str:
    return f"{p[0]:.2f} {p[1]:.2f}"


def faces(uid: str) -> str:
    """The three faces clipped to the pick. Each face runs out past the outline along its edge
    from the apex; a 0.35 bleed hides anti-aliasing seams between faces."""
    far = [(APEX[0] + 8 * (x - APEX[0]), APEX[1] + 8 * (y - APEX[1])) for x, y in CORNERS]
    tl, tr, tip = far

    def face(b: tuple[float, float], c: tuple[float, float], fill: str) -> str:
        return (
            f'<path d="M{pt(APEX)}L{pt(b)}L{pt(c)}Z" fill="{fill}" stroke="{fill}"'
            ' stroke-width="0.35" stroke-linejoin="round"/>'
        )

    return (
        f'<defs><clipPath id="{uid}"><path d="{PICK}"/></clipPath></defs>'
        f'<g clip-path="url(#{uid})">'
        + face(tl, tip, LEFT)
        + face(tl, tr, TOP)
        + face(tr, tip, RIGHT)
        + "</g>"
    )


def block(uid: str, ring: float, height: float) -> str:
    """The tile on a 64 grid. `ring` is the ring width, `height` the pick height. The pick's mass
    sits high, so its box centre goes a little below the tile centre to look centred."""
    k = height / 41.3
    tx, ty = 32 - 24 * k, 32.6 - 24.15 * k
    h = ring / 2
    return (
        f'<rect x="{h:g}" y="{h:g}" width="{64 - ring:g}" height="{64 - ring:g}" rx="{8 - h:g}"'
        f' fill="#000" stroke="{RING}" stroke-width="{ring:g}"/>'
        f'<g transform="translate({tx:.3f} {ty:.3f}) scale({k:.4f})">{faces(uid)}</g>'
    )


# Grid-true master (2 px ring on 64) for 48 px and up; a heavier cut for 16–32 px, where the
# true ring would fall below one pixel.
MASTER = dict(ring=2.0, height=36.0)
SMALL = dict(ring=5.0, height=42.0)


def word() -> tuple[float, str]:
    """Lowercase 'tetrad' in Plex Bold with Plex kerning: x-height 25, baseline y 0."""
    font = TTFont(FONT)
    glyphs, order = font.getGlyphSet(), font.getGlyphOrder()
    buf = hb.Buffer()
    buf.add_str("tetrad")
    buf.guess_segment_properties()
    hb.shape(hb.Font(hb.Face(hb.Blob.from_file_path(str(FONT)))), buf, {"kern": True})
    s = 25 / font["OS/2"].sxHeight
    pen = SVGPathPen(glyphs)
    x = 0.0
    for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
        glyphs[order[info.codepoint]].draw(TransformPen(pen, (s, 0, 0, -s, x, 0)))
        x += pos.x_advance * s
    return x, pen.getCommands()


def svg(w: float, h: float, body: str) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w:g} {h:g}" role="img"'
        f' aria-label="Tetrad">{body}</svg>\n'
    )


def main() -> None:
    w, d = word()
    gap = 14
    width = round(64 + gap + w + 2, 2)

    def lockup(uid: str, ink: str) -> str:
        return svg(
            width,
            64,
            block(uid, **MASTER) + f'<path fill="{ink}" transform="translate({64 + gap} 46.5)" d="{d}"/>',
        )

    # Avatar: the block on black with room for a round crop (the tile's diagonal fits the circle).
    avatar = '<rect width="64" height="64"/><g transform="translate(11.2 11.2) scale(0.65)">'
    avatar += block("avatar", **MASTER) + "</g>"
    files = {
        "tetrad-logo.svg": svg(64, 64, block("logo", **MASTER)),
        "tetrad-icon.svg": svg(64, 64, block("icon", **SMALL)),
        "tetrad-lockup.svg": lockup("lockup", TEXT),
        # The block keeps its black fill, so on a light page only the word changes ink.
        "tetrad-lockup-light.svg": lockup("lockup-light", "#000"),
        "tetrad-avatar.svg": svg(64, 64, avatar),
    }
    for name, text in files.items():
        (HERE / name).write_text(text)
    subprocess.run(
        ["rsvg-convert", "-w", "512", HERE / "tetrad-avatar.svg", "-o", HERE / "tetrad-avatar.png"],
        check=True,
    )


main()
