# tetrad-blocks

Owned DSP blocks for the SHARC. Each block is one ISA48 body, built with `tetrad-sharc`, wrapped
as a module blob in the stock block ABI. No stock code is copied.

| Block | Stock model ID |
| --- | --- |
| Volume | 16006 |
| Gain | 16005 |
| Simple Gate | 16003 |
| Graphic-9 | 4002 |
| Low-High Cut | 4003 |

The tests run each block on the `tetrad-sharc` machine model and compare it with a host reference.
Blocks deploy only with `tetrad bringup --owned <model> --allow-owned`, only on DSP core 0.
Only muted runs are tested on a unit.

Part of [Tetrad](../../README.md).
