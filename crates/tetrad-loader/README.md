# tetrad-loader

The stock SHARC boot for the ADSP-SC589: strict ADI LDR and `BLw` parsing, the booter tables of
`loadsharc_SC589`, typed boot plans for SOC1 cores 1 and 2 and the SOC2 slave boot, and an
executor over two injected hardware traits. `tetrad-sys` gives the `/dev/mem` and `/dev/corectrl`
backends. No firmware bytes are in this crate: the plans read the files on the unit.

Part of [Tetrad](../../README.md).
