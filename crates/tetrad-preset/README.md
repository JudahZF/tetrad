# tetrad-preset

Lossless CorOS presets.

- The wire layer (`PresetDocument`) keeps the exact bytes: unknown fields, order, duplicates,
  explicit defaults and packed encodings. A no-op export gives the same bytes.
- The typed layer (`model::Preset`) does structural edits, the stock load-time migrations, new
  presets and the stock save layout.

`schema` holds the preset message tables. Validation is a schema and data check, not a device
compatibility check.

Part of [Tetrad](../../README.md).
