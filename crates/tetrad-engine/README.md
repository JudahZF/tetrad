# tetrad-engine

The preset-to-DSP compiler and the owned 4×8 grid model. The path is
`import_layout` → `route` → `fit` → `compile` → `Deployment`. It follows the stock compiler:
routing checks, core assignment, per-core run lists with routers and links, latency alignment
and the stock incremental update rules. The crate also has a small host reference renderer.

Part of [Tetrad](../../README.md).
