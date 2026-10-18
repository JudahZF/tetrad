# tetrad-capture

Neural Capture: the V1 capture flow (`session`, `entry`, `orchestration`), the Neural Capture 2
controller (`v2`), stored capture records, and numeric host references of the V1 signal path
(`filters`, `gain`, `tones`, `network`, `resampling`, `pipeline`). Some stages also exist as
owned ISA48 code that runs offline on the `tetrad-sharc` simulator.

The flows are reducers: inputs in, effects out. The application executes the effects. Capture
training on a unit is not tested.

Part of [Tetrad](../../README.md).
