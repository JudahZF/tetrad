# Preset editing over the Tetrad Link (protocol 2)

**Status: proposed. The app speaks it; the unit does not yet.** The app turns Grid editing on
when the unit's `hello` reply says `protocol: 2` or higher (`src/lib/unit/link.ts`,
`PRESET_PROTOCOL`). Until then the Rig tab says that Grid editing needs a newer Tetrad. The demo
unit (`src/lib/unit/demo.ts`) answers these ops, and it is the reference for the expected
behaviour.

The messages use the Link format of docs/bluetooth.md: a JSON header with `id` and `op`, the
same `id` in the reply, and `{"error": "..."}` for a refusal.

## The preset

The JSON shape is `Preset` in `src/lib/preset/model.ts`:

| Field | Meaning |
| --- | --- |
| `loadId` | A new value at each load of a preset and after each change, from any source (the unit's screen, a footswitch, MIDI, the app). |
| `name`, `place` | `"Plexi Crunch"`, `"12B"` |
| `scenes` | 8 × `{ name, colour }` (`#rrggbb`) |
| `scene` | The current scene, 0–7 |
| `grid` | 4 rows × 8 columns of `Block` or `null`. Row and column are as in `tetrad-preset::edit::Slot`. |
| `edited` | Changes since the last save |

`Block` has `model`, `name`, `category` (the keys of `CATEGORIES`), optional `description` and
`extension`, `bypass`, optional `sceneBypass` (8 booleans), and `params`. Each param has `id`,
`name`, `value`, `min`, `max`, `step`, `unit`, and optional `scenes` (8 values).

## Ops

| `op` | Fields | Reply |
| --- | --- | --- |
| `preset` | | `preset`: the loaded preset |
| `preset-edit` | `loadId`, `edit` | `preset`: the preset after the edit. Refused when `loadId` is not the current one ("The preset changed on the unit: try again"); the app then reads `preset` again. |
| `preset-save` | | `preset`, with `edited: false` |
| `blocks` | | `blocks`: what the unit can add (`BlockModel` in `src/lib/preset/edit.ts`: `model`, `name`, `category`, `description`, `extension`, and `params` with a `default`) |

`edit` is one of (`Edit` in `src/lib/preset/model.ts`):

| `kind` | Fields | Effect |
| --- | --- | --- |
| `move` | `from`, `to` | A drop on another block swaps the two. |
| `copy` | `from`, `to` | `to` must be empty. |
| `add` | `to`, `model` | Replaces a block that is there. |
| `remove` | `at` | |
| `bypass` | `at`, `bypass` | In the current scene when the block has `sceneBypass`. |
| `bypass-scenes` | `at`, `on` | Bypass per scene on or off. |
| `param` | `at`, `param`, `value` | Clamped to the range and step; in the current scene when the parameter has `scenes`. |
| `param-scenes` | `at`, `param`, `on` | Values per scene on or off. |
| `scene` | `scene` | Changes the scene; it is not an edit of the preset (`edited` does not change). |

`src/lib/preset/edit.ts` (`applyEdit`) is the reference for each rule, with its tests in
`edit.test.ts`.

## Events

When the preset changes on the unit (by any source), the unit pushes
`{"event": "preset", "value": <preset>}`. The app shows it at once, so the app and the unit can
edit at the same time. A pushed preset with an older `loadId` than the one the app has is
ignored.

## Load on the link

A knob drag sends many `param` edits. The app sends one edit at a time and, while one is on its
way, keeps only the newest value of each parameter (`src/lib/preset/queue.ts`). At the Link's
rate of about 8 KB/s, a `preset-edit` and its reply (a few KB of preset JSON) take a fraction of
a second. A later protocol could reply with only the changed block, if a full preset turns out
too slow on the unit.

## What the unit needs

- `tetrad-preset::edit` has the Grid, scene and stomp edits on the typed preset; the device side
  maps `Edit` to them and keeps `loadId`.
- The same ops could serve Remote over Wi-Fi as `GET /api/preset` and
  `POST /api/preset/edit`, with the pushed event replaced by a poll.
