# tetrad-storage

Storage for the library:

- `stock`: the stock `zenui.db` (schema 410.1) and the read-only `factory.db`.
- `media`: stock files under `/media/p4`, atomic replace, newline lists and the setlist codec.
- `search_index`: the stock search index files, byte for byte.
- The crate root: the owned SQLite format of `tetrad-library`.

Writes are transactional. Opaque BLOBs stay as they are.

Part of [Tetrad](../../README.md).
