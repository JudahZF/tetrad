# Tetrad logo

The logo is a block from the Grid: a 64 px tile with a 2 px stock-green ring. In the tile is a
plectrum cut into the three visible faces of a tetrahedron. The faces use three block category
colours: drive orange, pitch yellow and cab purple. With the green ring, that makes four colours,
a tetrad.

| File | Use |
|---|---|
| `tetrad-lockup.svg` | The block and the word "tetrad". README header, docs, anywhere with room for the name. |
| `tetrad-lockup-light.svg` | The same with a black word, for light pages. The block keeps its black fill. |
| `tetrad-logo.svg` | The block alone, at 48 px and larger. |
| `tetrad-icon.svg` | The block alone, at 16–32 px: a heavier ring and a larger pick, so both still show. |
| `tetrad-avatar.svg`, `tetrad-avatar.png` | GitHub avatar and app icon: the block on black, safe for a round crop. |

Rules:

- The faces keep their order: the top face is the brightest, the right face the darkest. Other
  orders make the pick read flat.
- The word is lowercase IBM Plex Sans Bold in white, outlined, with Plex kerning.
- The logo is drawn for a black background, like the UI. On a light page, use the light lockup.

`build.py` makes every file here from the vendored font:
`uvx --with uharfbuzz --from fonttools python docs/brand/build.py`
