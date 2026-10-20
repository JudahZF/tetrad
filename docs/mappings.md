# Mappings

Mappings is one page for everything that a footswitch, a pedal, a MIDI message or an extension
does on your Quad Cortex. Open it from the Grid: **⋮ → Mappings**. In Tetrad it replaces the
stock **Preset MIDI Out** item.

<p align="center">
  <img src="images/mappings.png" width="600" alt="The Mappings page: the footswitch and expression pedal assignments of a preset">
</p>

Each row is one mapping: a **trigger** on the left and an **action** on the right.

## This preset and Global

The page has two tabs:

| Tab | Saved in | Triggers | Actions |
| --- | --- | --- | --- |
| **THIS PRESET** | The loaded preset | Preset loaded, footswitches A–H, EXP 1 and 2, MIDI, extension triggers | System, blocks, Send MIDI |
| **GLOBAL** | The unit (`/media/p4/tetrad/mappings.json`) | MIDI, extension triggers | System |

- Block actions are always saved in the preset, because each preset has its own blocks.
- When a preset mapping and a global mapping have the same trigger, only the preset mapping runs.
- A preset mapping is part of the preset. Save the preset to keep it, as for any other edit.
  Global mappings are saved at once.

## Add or change a mapping

1. Tap **+**, or tap a row.
2. Choose the **Type** of the trigger and its details (a footswitch letter, a pedal, a MIDI
   message or an extension source). For MIDI and extension triggers, tap **LEARN** and send the
   message: the next free message becomes the trigger.
3. Tap the **Action** row. The action menu has three parts:
   - **SYSTEM**: next or previous preset, a scene (or next/previous scene), the tuner, tap
     tempo, a footswitch mode, Gig View, and the Looper buttons.
   - **BLOCKS** (This preset only): a block of the preset, then its bypass or one of its
     parameters.
   - **MIDI OUT** (This preset only): Send MIDI. Tap **Edit messages** to set the messages in
     the stock message editor.
4. Tap ✓. To delete the mapping, tap the trash button.

<p align="center">
  <img src="images/mapping-edit.png" width="600" alt="A mapping from an extension's MIDI channel to the bypass of a Parametric EQ block">
</p>

## How values are used

| Trigger | Action | Result |
| --- | --- | --- |
| MIDI CC | Block parameter | Continuous: value 0–127 sets the parameter from 0 to 100 %. |
| MIDI CC | Bypass, tuner, Gig View, Looper button | Switch: 64 or more is on, less is off. |
| MIDI CC | Other system actions | Runs when the value is 64 or more. |
| Note, footswitch, extension press, Preset loaded | Bypass, tuner, Gig View | Toggles. |
| Note, footswitch, extension press, Preset loaded | Block parameter | Sets the value that the mapping page shows (5 % steps). |
| EXP 1 or 2 | Block parameter or bypass | The stock expression link: set its range and switch in the block's ⋮ menu. |

## Stock data shows as mappings

Some rows are stock preset data. Tetrad shows them in the list and edits the stock data, so
stock CorOS still reads them:

- **Footswitch → block bypass** is the preset's Stomp-mode assignment.
- **EXP 1 or 2 → block** is the block's expression link.
- **Preset loaded, footswitch or EXP → Send MIDI** is the preset's Preset MIDI Out list.

Every other mapping is Tetrad data. In a preset it is kept in one extra field that stock CorOS
ignores and keeps when it saves the preset.

## MIDI

- MIDI triggers on the **QC's own MIDI channel** (Settings → Device → MIDI) can use any CC or
  note that the stock MIDI map does not use. The stock map keeps CC 0–2, 32, 35–62, 64, 120,
  123 and program changes; Tetrad refuses those for a mapping.
- To use **another MIDI channel**, install the MIDI Mapper extension (see
  [extensions.md](extensions.md)). It reserves its channel and offers it as a trigger type.

## Not built yet

- Load a chosen preset and set the tempo to a value.
- Send MIDI from a MIDI trigger or from a global mapping.
- System actions from an expression pedal.
