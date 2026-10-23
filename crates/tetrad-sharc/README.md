# tetrad-sharc

Owned Rust emission for a restricted scalar SHARC ISA48 subset. The crate
forbids unsafe code and depends only on `thiserror`. It does not copy or link
`qcdsp`, a stock framework, a stock machine-code template, or vendor assets.

`tests/qcdsp_blocks.rs` uses `qcdsp` as a dev-dependency: it runs the generated
code of qcdsp's eight example blocks on this crate's raw machine (PRM
arithmetic). qcdsp's encoder shares no code with that machine.

The encoding authority is the pinned ADI SHARC+ Core Programming Reference
and independently checked encoding fields. Tests retain
only tiny invented numeric vectors. All 19 supported report vectors agree with
the manual and a separate assembler's section bytes; the PM vector that did
not agree is excluded. All nine owned gain words agree with that assembler.
The new research reports 30/48 agreeing extension vectors and 10/10 agreeing
nested-call words. The permanent extension test covers 30 **supported** synthetic
vectors: it omits XOR/push/toggle/L1 operations and failed direct system writes,
and includes manual/raw-backed RTI and MMASK immediate. These test counts differ
from the oracle's 30/48 result. The RTI oracle incorrectly sets bit 39; the PRM
and sparse raw evidence require bit 40. Type17 MMASK uses UREG `0x73`, despite
the oracle's legacy `0x7c`. No observed stock mask is an owned policy or fixture.
The assembler is not a build dependency. No target execution is claimed.

## Public API

`register` provides checked `DataReg` / `FloatReg` (0–15), and DAG1
`Dag1Index`, `Dag1Modify`, `Dag1Base`, `Dag1Length` (0–7). Float and integer
register names share storage. `UniversalReg` permits only R/I/M/B/L classes.
Memory operations accept data registers only, so illegal own-DAG transfers
cannot be expressed. No DAG2 register is accepted.

`isa48::Instruction48::encode()` emits typed float multiply/add/subtract,
absolute value, maximum, clipping, copy-sign and comparison; integer
PASS/decrement/subtract; universal immediate/move; normal-word DM pre/post
load/store, signed-offset load/store, self-index DAG1 MODIFY in both Type 19a
forms (`ModifyWords`: `(NW)`, opcode `0x15`, scaled by the pointer's space;
`ModifyBytes`: plain, opcode `0x16`, never scaled), the 48-bit stock C
return (`C_RETURN`),
checked Type14 direct R0–R15 DM loads/stores with `DmWordAddress32`,
Type18 SET/CLR/TST, restricted Type17 MMASK immediate, Type20 selected stack
pops/conflict-cache flush, non-delayed absolute/relative JUMP/CALL, plain RTS/RTI
and NOP. Arithmetic and data transfers are unconditional. Branch conditions are
Always, Equal, NotEqual, TestFlag (TF=13) and NotTestFlag (NOT TF=29). No compute is combined with a memory instruction.

`Mode1Mask` permits only the report's independently identified bits. SET
permits RND32, CBUFEN, IRPTEN and NESTM; CLR permits the identified mask bits. Setting
PEYEN or other modes outside this scalar policy returns a typed error.
MODE1 changes are emitted as instructions; the caller must handle their
hardware effect latency and surrounding mode preservation. MODIFY's caller
must establish B/L deliberately: L=0 means linear, whereas nonzero L can wrap
even with CBUFEN clear. Circular execution is outside the mathematical model.

`SystemBitMask` keeps Type18 SYSREG selectors distinct from UREG: MODE1=2,
MMASK=3, MODE2=4, IMASK=0xb, STKYX=8. Each variant carries a checked mask.
`Mode2Mask` permits only CADIS (bit 4) and SNEN (bit 0). `InterruptMask` permits
the PRM's writable fields (31:20, 15, 12:11, 8:3); RSTI/EMUI (1:0) and reserved
bits are rejected. `StackEmptyMask` permits LSEM/SSEM/PCEM (26/24/22) only and
is accepted only by TST. SET/CLR cannot write these read-only stack-status bits.
BIT TST's TF predicate means **all** selected bits are set, not any bit.
ASTATX and other system operands are not exposed.

`Mmask` permits only the scalar-clear mask plus IRPTEN/NESTM: status push can
clear those MODE1 bits. RND32/CBUFEN, bank selections and every other bit are
outside this owned policy. `MmaskImmediate` is a separate Type17 instruction;
it does not add system destinations to `UniversalReg` or register moves.
Direct MODE1/MODE2/ASTATX/STKYX/PCSTK/PCSTKP writes are not supported;
IRPTL and IMASK are writable system UREGs (see the resident extension).

`StackPops` names LOOP/STS/PCSTK and their combinations. `PopStacks` may include
conflict-cache flush; `FlushConflictCache` also stands alone. Push and mixed
push/pop operations cannot be represented by that API. The flush affects the
instruction-conflict cache only. L1 I/DM/PM caches and BTB maintenance remain
unsupported. Encodings do not establish stack restoration, interrupt state,
cache effects or effect latency; these require independent execution proof.


`InstructionWord48` rejects bits above 47 and serializes to `[u8; 6]` in
little-endian container order. Its checked raw constructor supports inspection;
`IsaProgram` has no raw-word insertion. Zero is an actual ISA48 NOP.

`address` separates `PmWordAddress24`, signed `PcRelative24`, `PcWordOffset`,
`DmWordAddress32`, `ContainerByteOffset` and `VisaParcelOffset`. The DM type
means an explicitly supplied normal-word alias, with no inferred physical
mapping or conversion to byte aliases. Branch ranges are checked, never
truncated: unsigned absolute 24 bits, signed relative 24 bits. Relative
displacements use `target - branch_start` in PC words.

`IsaProgram` defines `Label`s, accepts instructions, and records pending
`PcRel24` / `AbsPm24` branch fixups. `finish()` returns immutable `OwnedCode`
only after every fixup resolves. Duplicate/undefined symbols, labels that do
not name an instruction, program address overflow and branch range violations
have typed errors. Pending branches are private records, not executable zero
placeholders. No arbitrary relocation-site mutation is exposed. DM literals
can be supplied through typed caller addresses and universal immediates;
DM symbol fixups are not implemented.

`OwnedCode` exposes `origin()`, `words()`, `word_count()`, `symbols()` and
`isa48_le_bytes()`. These are section words and bytes, not an ELF/LDR image
or an automatically selected boot entry.

`fir` adds `FirLength`, checked `FirLayout`, `compile_fir` and immutable
`CompiledFir` for caller-prepared 512/2048-tap, fixed-32 callbacks. The 355-word
leaf uses existing ISA48 forms and the raw decoder. `simulate_fir` validates
operands/modes/ranges, allocates fresh scratch copies, checks each float result,
and commits state/DM only on success. Raw `run_scalar` retains partial-effect
semantics.

```rust
use tetrad_sharc::address::PmWordAddress24;
use tetrad_sharc::gain::{GAIN_ABI, compile_gain};

let code = compile_gain(PmWordAddress24::new(0x12000)?)?;
assert_eq!(code.word_count(), 9);
assert_eq!(code.isa48_le_bytes().len(), 54);
assert_eq!(GAIN_ABI.maximum_samples, 32);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Owned gain leaf contract

`compile_gain(origin)` emits the report's nine-instruction loop using typed
operations and linked labels. `GAIN_ABI` exposes its typed register and mode
requirements. The caller supplies I0=input, I1=output, F4=finite gain and
R8=count 0–32, normal 32-bit DM word aliases, L0=L1=0, RND32 set, and
broadcast/SIMD/TRUNCATE/ALUSAT/bit-reversal modes clear. CBUFEN may be set
when both lengths are zero. All registers use the caller's active banks.

The loop clobbers R0/F0, R2, M0, I0, I1 and arithmetic flags. F4, R8,
all other registers and MODE1 are preserved. Exact input/output alias is
allowed; partial overlap must be rejected. The caller must validate both
buffer ranges, lifetimes and a matching owned CALL/RTS convention before
hardware use. There are no delay slots, indirect calls, C frames, context
layout assumptions or stock services. Encoding agreement does not establish
instruction timing, ABI integration or hardware arithmetic parity.

## Mathematical simulator

`simulator` decodes emitted raw words independently of the encoder. It never
uses existing disassembler text. Its Type 3a modifier field is bits 40:38,
excluding the post-modify U bit. It models immediate R/I/M/B/L writes
(including B's update of I), the scalar arithmetic operations above,
normal-word linear DM pre/post transfers, direct scalar DM transfers, direct
JUMP/CALL, NOP and RTS. Direct DM uses all 32 address bits without DAG updates;
it accesses only the caller's `DmMemory` region and has no MMIO side effects.
The model rejects nonzero lengths on DAG accesses. Register moves, signed-offset
transfers and MODIFY can be emitted but are not simulated.

`run_scalar` preserves the leaf invocation convention: RTS at depth zero returns
to the host. Direct CALL pushes PC+1 onto a fixed 30-entry PC stack; nested RTS
pops in LIFO order. `run_linked` instead starts with an empty PC stack and stops
**before** an explicitly supplied linked stop instruction at depth zero. RTS
at depth zero is an underflow there. Both validate taken branch/call targets
and CALL return addresses against linked code. Overflow, underflow, a nonempty
stack at the stop instruction, invalid targets and exhausted budgets are errors.
No software C frame or return-minus-one literal is involved.

RTI returns `UnmodeledSemantics::InterruptReturn`; it is never executed as RTS.
Stack pops, conflict-cache flush, MODE/MMASK/IMASK/STKYX operations and TF/NOT TF
also return explicit unmodeled-semantics errors. Thus TF's all-bits predicate is
documented and encoded, but not simulated. PCSTKP=0 is not a recovery API.

`SampleCount`, `GainInvocation`, `DmMemory` and `simulate_gain` validate
count, finite gain, modes, lengths, overflow, both caller bounds and overlap
before installing the gain ABI. Memory stores are bounded and recorded as
typed DM addresses. Zero-length ranges may name the supplied region's end.
Linear one-past indices must remain representable. `ScalarState` exposes
raw register arrays and ALU zero for inspection. `run_scalar` is the lower
level entry without gain argument setup; it still checks scalar modes and
every memory access. A step budget and missing-RTS errors bound execution.
Execution errors may leave partial changes; caller validation errors do not.

`run_scalar_normal_floats` is an opt-in numerical policy for owned neural leaves.
Each float operand and result must be normal or signed zero. Nonfinite or subnormal
values return `NonNormalFloat` before that instruction changes data or ALU zero;
earlier instruction effects remain. Integer bit patterns, loads and moves are not
float operands. Default `run_scalar` and `run_linked` keep the existing host-f32
behavior. This policy refuses exceptional numbers; it does not emulate SHARC
flush-to-zero. RECIPS remains unsupported because its seed ROM is unknown.

`run_scalar_prm` (2026-10-10) is the opt-in PRM float model: denormal operands and
post-rounding results flush to signed zero, RND32 selects 24-bit or 40-bit rounding
with the 8-bit register extension, FLOAT rounds at 40 bits, NaN inputs give all ones
and NaN compares clear AZ (PRM 4.06, 5.84, 23.x; two NaN results are inferred). The
default paths keep host-f32 behavior. `Machine` has the same model as the opt-in `Arithmetic::Prm`
mode (40-bit registers, RND32 from MODE1, AIS/MIS stickies), which tetrad-blocks V2 uses.
Its 32-bit stores keep register bits 39:8 (PRM 4.06: the eight LSBs are ignored on
register reads), so a store after a 40-bit compute truncates.

In the default paths, host `f32` calculations are a mathematical reference only. SHARC
denormals, NaNs/infinities, extended precision, exceptional/status flags,
interrupt/status/loop stacks, hazards, mode latency and timing are not modeled. Only ALU zero is
modeled; floating multiplication does not update it. The bounded CALL model
checks architectural PC+1/LIFO behavior, not interrupt interactions or hardware
stack timing. Numerical mode preconditions are unchanged; encodable mode changes
do not silently enable unsupported arithmetic.

Tests cover 0/1/32 samples, zero/unit/fractional/negative gains, separate and
in-place buffers, asymmetric register preservation, exact write addresses,
unchanged canaries, multiple instances, invalid counts/gains/modes/bounds,
partial overlap, missing RTS, exhausted budgets, nested calls, exact stack
capacity, bad code targets and direct-DM bounds/canaries. Independent raw-decoder
regressions reject unverified PM/LW/system fields and the RTI oracle bug.

## Resident extension (2026-10-10)

Evidence: the ADI SHARC+ Programming Reference (PRM).
New typed forms: integer add/increment/COMP/COMPU, AND/OR/XOR, FLOAT, FIX,
Type6a LSHIFT immediate (12-bit count, DATAEX sign fill), shifter LSHIFT by
register, Type9a non-delayed `JUMP/CALL (Mx, Iy)` (DAG2), PUSH STS, IDLE,
Type14d `DM(addr32) = Rn (bw)`, conditions LT/LE/GE/GT, DAG2 I/M registers and
the system UREGs LCNTR, CURLCNTR, EMUCLK, USTAT1/2, IRPTL and IMASK. CURLCNTR
and EMUCLK are read-only destinations (`EncodeError::ReadOnlyRegister`).
`IsaProgram::counted_loop(LoopCount, last)` links Type12a
`LCNTR = n|Rn, DO last UNTIL LCE`; the end must follow the DO within rel23.

`ldr` emits raw ADI LDR streams in the stock shape (FIRST|IGNORE, literal,
zero fill, FINAL; DMACODE 1; 4-byte aligned; literals split at the stock
`0x800`-byte maximum) that `tetrad_loader::RawLdr` parses.

`machine` is the resident execution model: full R/I/M/B/L register file,
30-deep PC stack, 6-deep loop stack, 15-deep status stack, counter loops,
32 four-word vectors at PM `0x90000` when `SYSCTL.IIVT` (DM `0x30024`, core
state) is set, else the boot ROM, IRPTL/IMASK/IMASKP with IRPTEN and
NESTM, status push and MMASK on entry, RTI, IDLE and an injected `Bus`
(`MemoryMap` has word- and byte-addressed regions with LE byte lanes).
Normal-word DAG offsets and modifiers scale ×4 for byte-space pointers (DM
`0x24_0000` and up; PRM `.08.18`), and so does `MODIFY (NW)`; plain MODIFY
does not (stock: volume `0x034` `I7=MODIFY(I7,-0xa)(NW)` reserves ten frame
words; volume `0x1b0` `I5=MODIFY(I4,0x24)` is state byte 36). `FIX` follows the
non-saturating PRM rule (all ones on NaN, infinity or overflow). One
step is one cycle of `EMUCLK`; this is not pipeline timing. It refuses: a
zero loop count (PRM: 2^32 iterations), branches in the last three loop
instructions, nested loops sharing an end, saturation and bit-reversal modes and
Type3a post-modify/stores of DAG1 registers.

## Frame kernels (2026-10-22)

For qcdsp networks (NAM WaveNets) the machine also runs, in ISA and VISA: Type 1a (compute
with a DM and a PM move; no compute, single-function, or float multifunction mul±add and
mul with dual add/sub; Type 2a takes the same multifunction computes) and the 32-bit VISA Type 1b; Type 3a/3b with G=1 (DAG2) and a compute
field; DAG2 in Types 15a/15b, 16a and 19a; S0-S15 as uregs. MODE1 PEYEN (SIMD), BDCST1 and
BDCST9 are modeled: computes and transfers act on PEy with the complementary registers, PEy
addresses the explicit address plus one normal word (+4 in byte space), a load through I1 or
I9 with its broadcast bit gives PEy the same word, and a SIMD branch condition must hold on
both PEs. Circular buffers wrap post-modify and MODIFY within `[B, B+L)` (M and L ×4 in byte
space); pre-modify does not wrap. Stock code switches these modes with Type 18a BIT SET/CLR
(`140200200000` sets PEYEN, `142200200000` clears it). The machine refuses VISA code whose last
31 parcels of a loop body are not 48-bit instructions (`VisaLoopTail`, PRM `.06.062`), an
instruction that writes one data register twice, and |M| ≥ L in a ring. `tests/machine_simd.rs`
builds its words field by field from the PRM figures and checks stock words.

## Math leaves (2026-10-10)

`math/` has ISA48 leaves with exact host twins: `exp2`, `db_to_linear`, `log2`,
`linear_to_db`, `sin`, `cos`, `tan` and an owned `reciprocal` (integer seed + 3 Newton steps,
no `RECIPS`). Argument in F4, result in F0, CCES scratch registers only, no memory, PC-relative
branches. `math::emit(program, &[Leaf])` adds the requested leaves and their dependencies. The
module doc has the domain, cycle and max-ulp table (ignored test `error_table` re-measures it).

`math::prm` has the host twins of single computes under `Arithmetic::Prm`: `mul`, `add`, `sub`
on f32 at the callback mode (RND32 = 1, an independent f64 rule), and for code that brackets a
loop with `BIT CLR/SET MODE1 RND32` (the stock cascades) `Precision::{Single, Extended}` on
`Float40` registers: `From<f32>` is a 32-bit load, `store()` the truncating 32-bit store. The
extended ops run the machine's own compute; a unit test checks them against an independent
exact integer rule (u128) on 40-bit operands with nonzero extensions, every exponent gap and
cancelling sums.

## VISA shim (2026-10-10)

`visa.rs` encodes the owned-block shim forms (ureg = imm32, DM store/load with offset,
`I7 = MODIFY(I7, -n)(NW)`, `CJUMP addr (DB)`, the `DM(I7, M7)` pushes of a register
(Type 3c) and a literal (Type 16a), NOP and the stock C epilogue) and `Machine` switches
ISA/VISA decoding by fetch address (`VISA_SPACE`), with delay-slot jumps and RFRAME.
CJUMP sets R2 = I6 and I6 = I7 before its two delay slots. The qcdsp verification extension
(2026-10-16) also decodes Type 5b universal-register moves, general Type 3c normal-word
post-modify data transfers, and universal-register Type 15a/15b offsets. DAG1 store
sources remain refused. MIN, NEG and floating PASS are modeled under PRM rules, including
signed zero, NaN and denormal flushing. Literal-vector tests cover these forms; the
qcdsp workspace executes its generated routines through this raw decoder. Other VISA
short forms are refused, never decoded with a wrong length. Every VISA form the owned blobs emit has a
stock site. Three forms that owned code runs in ISA have stock sites only in VISA code
(PRM only for ISA): relative JUMP/CALL, E2-active DO loops and the non-delayed `RTS` (stock
ISA returns use `RTS (DB)`). H1 is their first hardware proof.

## Unsupported milestones

Cross-index MODIFY, condition-terminated DO loops, conditional Type 1 and Type 3, MRF
multifunction, Type9a forms outside `(Mx, I8)`, L1 cache
maintenance and unknown flags remain unavailable. Open hardware gates: a probe of Type14d in ISA code space (PRM
lists Addr = ISA), the SYSCTL address and byte-space bound, SHARC-side
cache/coherency of shared L2,
and every timing claim. The leaf `simulator` keeps its narrow contract: RTI,
stack and system operations stay `Unmodeled` there.

## Package checks

From the repository root, without building storage, SQLite or CLI:

```sh
rustfmt --edition 2024 --check crates/tetrad-sharc/src/*.rs crates/tetrad-sharc/tests/*.rs
cargo test -p tetrad-sharc --all-targets --offline --locked
cargo test -p tetrad-sharc --doc --offline --locked
cargo doc -p tetrad-sharc --no-deps --offline --locked
cargo clippy -p tetrad-sharc --all-targets --offline --locked -- -D warnings
```
