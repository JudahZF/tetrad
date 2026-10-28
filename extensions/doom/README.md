# DOOM

The shareware episode of DOOM on the Quad Cortex screen, played with the footswitches. The
extension is a [program](../../docs/extensions.md#ship-a-program) (API 7): it runs the original
qcDOOM binary unchanged.

| File | What it is | Licence |
| --- | --- | --- |
| `qcdoom` | The prebuilt binary of [qcDOOM](https://github.com/VanIseghemThomas/qcDOOM) by Thomas Van Iseghem, commit `d24ce5a`: fbDOOM with the QC footswitches as input. | GPL-2.0, like the DOOM source; the source is the same commit. |
| `doom1.wad` | The DOOM shareware WAD v1.9 (md5 `f0cefca49926d00903cf57551d901abe`), from Ubuntu's `doom-wad-shareware` package. | id Software lets anyone share the shareware WAD unchanged; the package's copyright file has the statement. |

The files are not in this repository. `./fetch.sh` downloads them into `app/` and checks their
SHA-256. Then package and publish as for every extension; `tetrad-ext package` copies the files
next to the package as release assets.

## Controls

| Switch | Action |
| --- | --- |
| A | Turn left |
| B | Forward |
| C | Turn right |
| D | Fire |
| E | Strafe left |
| F | Back |
| G | Strafe right |
| H | Use (doors, switches) |
| TEMPO | Menu (Escape) |
| DOWN | Enter |
| UP | Backspace |

In the menu, A, B, C and F move and DOWN chooses. qcDOOM gives no switch the `y` key, so the
quit question of DOOM cannot be answered: hold the screen for 2 seconds to quit. The footswitch
LEDs keep the colours that Tetrad gave them. DOOM has no sound here; the amp sound goes on.

Saved games and the DOOM settings are in the asset folder of the extension,
`/media/p4/tetrad/assets/tetrad.doom/`.
