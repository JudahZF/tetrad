//! Generated resident ISA48 program: vectors, bring-up, the SID 60 frame
//! handler, the per-frame graph scheduler, owned kernels and the SHARC side of
//! the stock message rings.
//!
//! Everything runs inside the receive-DMA SEC handler; `main` only IDLEs, so
//! no register state is shared with interrupted code. Register use:
//! kernels get `I0` input 0, `I2` input 1, `I1` output, `I3` parameters and may
//! clobber R0-R8, I0-I3, M0 and the loop stack (balanced). The scheduler keeps
//! R9-R13 and I4; the ring server keeps R14, R15, I5 and passes handlers the
//! payload in `I5`, its byte length in `R5` and the type in `R2`.
//! Adapter B kernels ([`emit_block_kernels`]) also set the C run-time (I7,
//! M5-M7) and leave the CCES scratch I12, I13, M4, M12, B4, B12, B13 changed;
//! the resident keeps nothing in those across a kernel call.
//!
//! The message window is byte space: normal-word offsets and modifiers through
//! a window pointer count words (scaled by 4 on hardware, PRM `.08.18`), while
//! plain `MODIFY` counts bytes. L1 data uses the word alias, unscaled.

use std::collections::BTreeMap;
use std::num::NonZeroU16;

use super::audio::{
    DMA_STAT_IRQDONE, MmioOp, RECEIVE_SID, SEC0_END, TRU0_MTR, arm_doorbell, sec_csid,
};
use super::layout::{DataLayout, FRAMES, PARAMETERS_PER_SLOT, control, node};
use super::memory::{CODE_BASE, MESSAGE_WINDOW, SharcCore};
use crate::firmware::graph::{SLOT_COUNT, SlotId};
use crate::resident::audio_port::{StockPort, TDM_SLOTS};
use tetrad_blocks::{BLOCK_SAMPLES, BlockError, OwnedBlock, StaticLayout};
use tetrad_dsp_protocol::{CoreId, DspCore, MessageType};
use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
use tetrad_sharc::emit::{BranchRelocation, EmitError, IsaProgram, Label, LoopCount, OwnedCode};
use tetrad_sharc::isa48::{
    BitOperation, BranchKind, BranchTarget, Condition, DmAccess, IMASK_SECI, Instruction48,
    InterruptMask, MODE1_CBUFEN, MODE1_IRPTEN, MODE1_NESTM, MODE1_RND32, MODE1_SCALAR_CLEAR, Mmask,
    Mode1Mask, SystemBitMask,
};
use tetrad_sharc::machine::{SYSCTL, SYSCTL_IIVT};
use tetrad_sharc::register::{
    Dag1Index, Dag1Length, Dag1Modify, Dag2Index, Dag2Modify, DataReg, FloatReg, SystemRegister,
    UniversalReg,
};

macro_rules! reg {
    ($class:ident, $index:literal) => {
        match $class::new($index) {
            Ok(register) => register,
            Err(_) => panic!("fixed register outside its class"),
        }
    };
}
const R: [DataReg; 16] = [
    reg!(DataReg, 0),
    reg!(DataReg, 1),
    reg!(DataReg, 2),
    reg!(DataReg, 3),
    reg!(DataReg, 4),
    reg!(DataReg, 5),
    reg!(DataReg, 6),
    reg!(DataReg, 7),
    reg!(DataReg, 8),
    reg!(DataReg, 9),
    reg!(DataReg, 10),
    reg!(DataReg, 11),
    reg!(DataReg, 12),
    reg!(DataReg, 13),
    reg!(DataReg, 14),
    reg!(DataReg, 15),
];
const F0: FloatReg = reg!(FloatReg, 0);
const F1: FloatReg = reg!(FloatReg, 1);
const F4: FloatReg = reg!(FloatReg, 4);
const F5: FloatReg = reg!(FloatReg, 5);
const F6: FloatReg = reg!(FloatReg, 6);
const F7: FloatReg = reg!(FloatReg, 7);
const I: [Dag1Index; 8] = [
    reg!(Dag1Index, 0),
    reg!(Dag1Index, 1),
    reg!(Dag1Index, 2),
    reg!(Dag1Index, 3),
    reg!(Dag1Index, 4),
    reg!(Dag1Index, 5),
    reg!(Dag1Index, 6),
    reg!(Dag1Index, 7),
];
const M0: Dag1Modify = reg!(Dag1Modify, 0);
const M1: Dag1Modify = reg!(Dag1Modify, 1);
const M2: Dag1Modify = reg!(Dag1Modify, 2);
/// C run-time constants 0, 1 and -1 (CCES dedicated registers).
const M5: Dag1Modify = reg!(Dag1Modify, 5);
const M6: Dag1Modify = reg!(Dag1Modify, 6);
const M7: Dag1Modify = reg!(Dag1Modify, 7);
/// Indirect calls use the stock-proven `(Mx, I8)` Type9a subset.
const I8: Dag2Index = reg!(Dag2Index, 8);
const M8: Dag2Modify = reg!(Dag2Modify, 8);

const fn ur(register: DataReg) -> UniversalReg {
    UniversalReg::Data(register)
}

const ONE: u32 = 0x3f80_0000;
const TWO: u32 = 0x4000_0000;
const TWO_POW_MINUS_31: u32 = 0x3000_0000;
const TWO_POW_31: u32 = 0x4f00_0000;
/// 2^31 - 128, the largest f32 that `FIX` converts without overflow.
const FULL_SCALE: u32 = 0x4eff_ffff;
/// Messages handled per frame, bounding ring work inside the audio interrupt.
pub const MESSAGES_PER_FRAME: u32 = 4;
/// Vectors that record a fault and halt: PARI, ILOPI, IICDI, SOVFI, ILADI, RINSEQI.
const FAULT_VECTORS: [u32; 6] = [3, 4, 6, 7, 8, 20];
const RESET_VECTOR: u32 = 1;
const SECI_VECTOR: u32 = 15;
const VECTORS: u32 = 32;
const VECTOR_WORDS: u32 = 4;

/// Owned stock-message handlers. Every other type byte is consumed and
/// counted as unsupported.
pub const HANDLED: [(MessageType, &str); 8] = [
    (MessageType::ParameterChange, "msg.parameter"),
    (MessageType::BulkParameterChange, "msg.bulk_parameter"),
    (MessageType::ModelBypass, "msg.bypass"),
    (MessageType::BulkModelBypass, "msg.bulk_bypass"),
    (MessageType::MeterRequest, "msg.meter"),
    (MessageType::DiagnosticsRequest, "msg.diagnostics"),
    (MessageType::BulkStartModel, "msg.bulk_start"),
    (MessageType::DeleteAllModels, "msg.delete_all"),
];
pub const UNSUPPORTED_HANDLER: &str = "msg.unsupported";
pub const KERNEL_GAIN: &str = "kernel.gain";
pub const KERNEL_MIX: &str = "kernel.mix";
pub const INIT: &str = "init";

/// The SHARC side of one protocol core's ring pair, as system byte addresses.
/// Offsets are the ARM layout (ZenUI `0x54e0ac`,
/// `0x54f374`, `0x54f5c0`); tests check them against `tetrad_dsp_host`'s
/// `RingLayout`. All four index bytes share one aligned word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingAddresses {
    /// `[out producer (ARM), out consumer, in producer, in consumer (ARM)]`.
    pub indices: u32,
    pub out_payload: u32,
    pub out_lengths: u32,
    pub out_slots: u32,
    pub out_slot_bytes: u32,
    pub in_payload: u32,
    pub in_lengths: u32,
    pub in_slots: u32,
    pub in_slot_bytes: u32,
}

impl RingAddresses {
    pub const fn new(ring: CoreId) -> Self {
        let (indices, out_payload, out_lengths, in_payload, in_lengths) = match ring {
            CoreId::Dsp(DspCore::Core0) => (0x784, 0x798, 0x1_76d8, 0x5798, 0x1_7700),
            CoreId::Dsp(DspCore::Core1) => (0x788, 0x5f78, 0x1_7738, 0xaf78, 0x1_7760),
            CoreId::Control => (0x78c, 0xb758, 0x1_7798, 0xbf38, 0x1_77d0),
            CoreId::Dsp(DspCore::Core2) => (0x790, 0xc718, 0x1_7808, 0x1_1718, 0x1_7830),
            CoreId::Dsp(DspCore::Core3) => (0x794, 0x1_1ef8, 0x1_7868, 0x1_6ef8, 0x1_7890),
        };
        let (out_slots, out_slot_bytes) = match ring {
            CoreId::Control => (14, 144),
            CoreId::Dsp(_) => (10, 2048),
        };
        Self {
            indices: MESSAGE_WINDOW + indices,
            out_payload: MESSAGE_WINDOW + out_payload,
            out_lengths: MESSAGE_WINDOW + out_lengths,
            out_slots,
            out_slot_bytes,
            in_payload: MESSAGE_WINDOW + in_payload,
            in_lengths: MESSAGE_WINDOW + in_lengths,
            in_slots: 14,
            in_slot_bytes: 144,
        }
    }
}

/// Inputs of one generated resident.
#[derive(Debug, Clone, Copy)]
pub struct CodeConfig<'a> {
    pub core: SharcCore,
    /// Protocol core whose ring pair this resident serves.
    pub ring: CoreId,
    pub layout: &'a DataLayout,
    pub bring_up: &'a [MmioOp],
}

struct Asm {
    program: IsaProgram,
    unique: u32,
}

impl Asm {
    fn op(&mut self, instruction: Instruction48) -> Result<(), EmitError> {
        self.program.push(instruction).map(drop)
    }
    fn label(&mut self, name: &str) -> Result<(), EmitError> {
        self.program.define(Label::new(name))
    }
    fn fresh(&mut self, stem: &str) -> String {
        self.unique += 1;
        format!("{stem}.{}", self.unique)
    }
    fn branch(
        &mut self,
        kind: BranchKind,
        condition: Condition,
        to: &str,
    ) -> Result<(), EmitError> {
        self.program
            .branch_to(kind, condition, Label::new(to), BranchRelocation::PcRel24)
            .map(drop)
    }
    fn jump_if(&mut self, condition: Condition, to: &str) -> Result<(), EmitError> {
        self.branch(BranchKind::Jump, condition, to)
    }
    fn jump(&mut self, to: &str) -> Result<(), EmitError> {
        self.jump_if(Condition::Always, to)
    }
    fn call(&mut self, to: &str) -> Result<(), EmitError> {
        self.branch(BranchKind::Call, Condition::Always, to)
    }
    fn set(&mut self, dst: UniversalReg, value: u32) -> Result<(), EmitError> {
        self.op(Instruction48::Immediate { dst, value })
    }
    fn mov(&mut self, dst: UniversalReg, src: UniversalReg) -> Result<(), EmitError> {
        self.op(Instruction48::RegisterMove { dst, src })
    }
    fn load(&mut self, dst: DataReg, address: u32) -> Result<(), EmitError> {
        self.op(Instruction48::DmDirectLoad {
            dst,
            address: DmWordAddress32::new(address),
        })
    }
    fn store(&mut self, src: DataReg, address: u32) -> Result<(), EmitError> {
        self.op(Instruction48::DmDirectStore {
            src,
            address: DmWordAddress32::new(address),
        })
    }
    fn load_at(&mut self, dst: DataReg, index: Dag1Index, offset: u32) -> Result<(), EmitError> {
        self.op(Instruction48::DmOffsetLoad {
            dst,
            index,
            offset: offset as i32,
        })
    }
    fn store_at(&mut self, src: DataReg, index: Dag1Index, offset: u32) -> Result<(), EmitError> {
        self.op(Instruction48::DmOffsetStore {
            src,
            index,
            offset: offset as i32,
        })
    }
    /// Load the word at byte `offset` from a message-window (byte-space)
    /// pointer: the encoded offset counts words.
    fn load_window(
        &mut self,
        dst: DataReg,
        index: Dag1Index,
        offset: u32,
    ) -> Result<(), EmitError> {
        assert!(offset.is_multiple_of(4), "window fields are word aligned");
        self.load_at(dst, index, offset / 4)
    }
    fn next_load(
        &mut self,
        dst: DataReg,
        index: Dag1Index,
        modifier: Dag1Modify,
    ) -> Result<(), EmitError> {
        self.op(Instruction48::DmLoad {
            dst,
            index,
            modifier,
            access: DmAccess::PostModify,
        })
    }
    fn next_store(
        &mut self,
        src: DataReg,
        index: Dag1Index,
        modifier: Dag1Modify,
    ) -> Result<(), EmitError> {
        self.op(Instruction48::DmStore {
            src,
            index,
            modifier,
            access: DmAccess::PostModify,
        })
    }
    fn pass(&mut self, register: DataReg) -> Result<(), EmitError> {
        self.op(Instruction48::IntegerPass {
            dst: register,
            src: register,
        })
    }
    /// `COMP(register, value)` through a scratch register.
    fn compare(
        &mut self,
        register: DataReg,
        value: u32,
        scratch: DataReg,
        unsigned: bool,
    ) -> Result<(), EmitError> {
        self.set(ur(scratch), value)?;
        self.op(if unsigned {
            Instruction48::IntegerCompareUnsigned {
                lhs: register,
                rhs: scratch,
            }
        } else {
            Instruction48::IntegerCompare {
                lhs: register,
                rhs: scratch,
            }
        })
    }
    fn increment_word(&mut self, address: u32, scratch: DataReg) -> Result<(), EmitError> {
        self.load(scratch, address)?;
        self.op(Instruction48::IntegerIncrement {
            dst: scratch,
            src: scratch,
        })?;
        self.store(scratch, address)
    }
    /// `dst = src * factor` with shifts and adds; `scratch` is clobbered.
    fn multiply(
        &mut self,
        dst: DataReg,
        src: DataReg,
        factor: u32,
        scratch: DataReg,
    ) -> Result<(), EmitError> {
        self.set(ur(dst), 0)?;
        for bit in 0..32 {
            if factor & (1 << bit) != 0 {
                self.op(Instruction48::LogicalShift {
                    dst: scratch,
                    src,
                    amount: bit as i8,
                })?;
                self.op(Instruction48::IntegerAdd {
                    dst,
                    lhs: dst,
                    rhs: scratch,
                })?;
            }
        }
        Ok(())
    }
    /// A straight-line counter loop; `body` ends the loop.
    fn repeat(&mut self, count: LoopCount, body: &[Instruction48]) -> Result<(), EmitError> {
        let end = self.fresh("loop");
        self.program.counted_loop(count, Label::new(end.as_str()))?;
        let (last, rest) = body.split_last().expect("loop body is never empty");
        for instruction in rest {
            self.op(*instruction)?;
        }
        self.label(&end)?;
        self.op(*last)
    }
    /// The byte at `address` of a little-endian word, into `dst`.
    fn extract_byte(
        &mut self,
        dst: DataReg,
        word: DataReg,
        lane: u8,
        scratch: DataReg,
    ) -> Result<(), EmitError> {
        self.op(Instruction48::LogicalShift {
            dst,
            src: word,
            amount: -8 * lane as i8,
        })?;
        self.set(ur(scratch), 0xff)?;
        self.op(Instruction48::And {
            dst,
            lhs: dst,
            rhs: scratch,
        })
    }
}

fn frames() -> LoopCount {
    LoopCount::Immediate(NonZeroU16::new(FRAMES as u16).expect("nonzero frames"))
}

/// Emit the complete resident at [`CODE_BASE`], vectors first.
pub fn generate(config: CodeConfig<'_>) -> Result<OwnedCode, CodeGenerationError> {
    let ring = RingAddresses::new(config.ring);
    let mut asm = Asm {
        program: IsaProgram::new(PmWordAddress24::new(CODE_BASE)?),
        unique: 0,
    };
    vectors(&mut asm)?;
    init(&mut asm, config, &ring)?;
    sec(&mut asm, config)?;
    frame(&mut asm, config.layout)?;
    kernels(&mut asm)?;
    poll(&mut asm, config.layout, &ring)?;
    send(&mut asm, config, &ring)?;
    handlers(&mut asm, config)?;
    Ok(asm.program.finish()?)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeGenerationError {
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Address(#[from] tetrad_sharc::address::AddressError),
}

fn vectors(asm: &mut Asm) -> Result<(), EmitError> {
    for vector in 0..VECTORS {
        let target = match vector {
            RESET_VECTOR => Some(INIT),
            SECI_VECTOR => Some("sec"),
            _ => None,
        };
        if let Some(target) = target {
            asm.jump(target)?;
            for _ in 1..VECTOR_WORDS {
                asm.op(Instruction48::Nop)?;
            }
        } else if FAULT_VECTORS.contains(&vector) {
            asm.set(ur(R[15]), vector)?;
            asm.jump("fault")?;
            asm.op(Instruction48::Nop)?;
            asm.op(Instruction48::Nop)?;
        } else {
            // Stock shape for unused vectors.
            for _ in 0..VECTOR_WORDS {
                asm.op(Instruction48::Rti)?;
            }
        }
    }
    Ok(())
}

fn mode1(bits: u32) -> Mode1Mask {
    Mode1Mask::new(bits).expect("verified MODE1 bits")
}

fn init(asm: &mut Asm, config: CodeConfig<'_>, ring: &RingAddresses) -> Result<(), EmitError> {
    asm.label(INIT)?;
    // SYSCTL.IIVT: take vectors from L1 PM 0x9_0000, not the boot ROM (stock
    // core-1 init `0x231f2`; PRM DBG_SYSCTL bit 2).
    asm.load(R[0], SYSCTL)?;
    asm.set(ur(R[1]), SYSCTL_IIVT)?;
    asm.op(Instruction48::Or {
        dst: R[0],
        lhs: R[0],
        rhs: R[1],
    })?;
    asm.store(R[0], SYSCTL)?;
    asm.op(Instruction48::Mode1Clear(mode1(
        MODE1_SCALAR_CLEAR | MODE1_IRPTEN | MODE1_NESTM | MODE1_CBUFEN,
    )))?;
    asm.op(Instruction48::Mode1Set(mode1(MODE1_RND32)))?;
    asm.op(Instruction48::MmaskImmediate(
        Mmask::new(0).expect("empty mask"),
    ))?;
    for index in 0..8 {
        asm.set(
            UniversalReg::Length(Dag1Length::new(index).expect("DAG1 length")),
            0,
        )?;
    }
    asm.set(UniversalReg::Dag2Modify(M8), 0)?;
    for op in config.bring_up {
        match *op {
            MmioOp::Write { address, value } => {
                asm.set(ur(R[0]), value)?;
                asm.store(R[0], address)?;
            }
            MmioOp::SetBits { address, mask } => {
                asm.load(R[0], address)?;
                asm.set(ur(R[1]), mask)?;
                asm.op(Instruction48::Or {
                    dst: R[0],
                    lhs: R[0],
                    rhs: R[1],
                })?;
                asm.store(R[0], address)?;
            }
        }
    }
    seed_ring_indices(asm, config.layout, ring)?;
    // SECI plus the fault sources; all are masked at reset (PRM `.06.030`).
    let faults = FAULT_VECTORS
        .iter()
        .fold(0, |mask, vector| mask | 1 << vector);
    asm.op(Instruction48::SystemBit {
        operation: BitOperation::Set,
        operand: SystemBitMask::Interrupt(
            InterruptMask::new(IMASK_SECI | faults).expect("writable IMASK bits"),
        ),
    })?;
    asm.op(Instruction48::Mode1Set(mode1(MODE1_IRPTEN)))?;
    asm.label("main")?;
    asm.op(Instruction48::Idle)?;
    asm.jump("main")?;

    let control = config.layout.control;
    asm.label("fault")?;
    asm.store(R[15], control + control::FAULT)?;
    asm.op(Instruction48::Mode1Clear(mode1(MODE1_IRPTEN)))?;
    asm.label("fault.halt")?;
    asm.op(Instruction48::Idle)?;
    asm.jump("fault.halt")
}

/// Start both rings empty: the out consumer takes the ARM out-producer byte and the in producer
/// takes the ARM in-consumer byte, so stale slots from an earlier run are neither replayed nor
/// republished. The ARM transport does not reset the window. A byte outside the ring reads as slot
/// 0.
fn seed_ring_indices(
    asm: &mut Asm,
    layout: &DataLayout,
    ring: &RingAddresses,
) -> Result<(), EmitError> {
    let control = layout.control;
    for (lane, slots, private, owned_lane) in [
        (0, ring.out_slots, control::OUT_CONSUMER, 1),
        (3, ring.in_slots, control::IN_PRODUCER, 2),
    ] {
        let valid = asm.fresh("init.index");
        asm.load(R[0], ring.indices)?;
        asm.extract_byte(R[2], R[0], lane, R[1])?;
        asm.compare(R[2], slots, R[1], true)?;
        asm.jump_if(Condition::LessThan, &valid)?;
        asm.set(ur(R[2]), 0)?;
        asm.label(&valid)?;
        asm.store(R[2], control + private)?;
        asm.op(Instruction48::DmDirectByteStore {
            src: R[2],
            address: ring.indices + owned_lane,
        })?;
    }
    Ok(())
}

fn sec(asm: &mut Asm, config: CodeConfig<'_>) -> Result<(), EmitError> {
    let control = config.layout.control;
    asm.label("sec")?;
    asm.load(R[0], sec_csid(config.core))?;
    asm.compare(R[0], RECEIVE_SID, R[1], false)?;
    asm.jump_if(Condition::NotEqual, "sec.stray")?;
    asm.set(ur(R[1]), DMA_STAT_IRQDONE)?;
    asm.store(R[1], StockPort::Sport4aReceive.dma().stat.get())?;
    asm.call("frame")?;
    asm.call("poll")?;
    asm.set(ur(R[0]), RECEIVE_SID)?;
    asm.label("sec.end")?;
    asm.store(R[0], SEC0_END)?;
    asm.op(Instruction48::Rti)?;
    asm.label("sec.stray")?;
    asm.increment_word(control + control::STRAY_SOURCES, R[1])?;
    asm.jump("sec.end")
}

fn frame(asm: &mut Asm, layout: &DataLayout) -> Result<(), EmitError> {
    let control = layout.control;
    let clock = UniversalReg::System(SystemRegister::EmulatorClock);
    asm.label("frame")?;
    asm.mov(ur(R[9]), clock)?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.set(UniversalReg::Modify(M1), TDM_SLOTS)?;
    // The completed receive buffer and the transmit buffer of the same parity.
    asm.load(R[0], control + control::RX_PARITY)?;
    asm.set(ur(R[1]), 1)?;
    asm.op(Instruction48::Xor {
        dst: R[2],
        lhs: R[0],
        rhs: R[1],
    })?;
    asm.store(R[2], control + control::RX_PARITY)?;
    asm.pass(R[0])?;
    asm.jump_if(Condition::NotEqual, "frame.odd")?;
    asm.set(ur(R[10]), layout.rx[0])?;
    asm.set(ur(R[11]), layout.tx[0])?;
    asm.jump("frame.inputs")?;
    asm.label("frame.odd")?;
    asm.set(ur(R[10]), layout.rx[1])?;
    asm.set(ur(R[11]), layout.tx[1])?;

    // Deinterleave: TDM word -> f32 in [-1, 1).
    asm.label("frame.inputs")?;
    asm.set(ur(R[5]), TWO_POW_MINUS_31)?;
    asm.set(UniversalReg::Index(I[4]), layout.inputs)?;
    asm.next_load(R[12], I[4], M0)?;
    asm.label("frame.input")?;
    asm.pass(R[12])?;
    asm.jump_if(Condition::Equal, "frame.nodes")?;
    asm.next_load(R[0], I[4], M0)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[0],
        lhs: R[0],
        rhs: R[10],
    })?;
    asm.mov(UniversalReg::Index(I[0]), ur(R[0]))?;
    asm.next_load(R[1], I[4], M0)?;
    asm.mov(UniversalReg::Index(I[1]), ur(R[1]))?;
    asm.repeat(
        frames(),
        &[
            Instruction48::DmLoad {
                dst: R[0],
                index: I[0],
                modifier: M1,
                access: DmAccess::PostModify,
            },
            Instruction48::FloatFromInteger { dst: F0, src: R[0] },
            Instruction48::FloatMultiply {
                dst: F0,
                lhs: F0,
                rhs: F5,
            },
            Instruction48::DmStore {
                src: R[0],
                index: I[1],
                modifier: M0,
                access: DmAccess::PostModify,
            },
        ],
    )?;
    asm.op(Instruction48::IntegerDecrement {
        dst: R[12],
        src: R[12],
    })?;
    asm.jump("frame.input")?;

    // Graph: one record per scheduled node, kernel 0 ends the table.
    asm.label("frame.nodes")?;
    asm.set(UniversalReg::Index(I[4]), layout.nodes)?;
    asm.label("frame.node")?;
    asm.load_at(R[0], I[4], node::KERNEL)?;
    asm.pass(R[0])?;
    asm.jump_if(Condition::Equal, "frame.outputs")?;
    asm.mov(ur(R[13]), clock)?;
    for (offset, index) in [
        (node::INPUT0, I[0]),
        (node::INPUT1, I[2]),
        (node::OUTPUT, I[1]),
        (node::PARAMETERS, I[3]),
    ] {
        asm.load_at(R[1], I[4], offset)?;
        asm.mov(UniversalReg::Index(index), ur(R[1]))?;
    }
    asm.load_at(R[2], I[4], node::FLAGS)?;
    asm.set(ur(R[3]), node::ACTIVE)?;
    asm.op(Instruction48::And {
        dst: R[3],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::Equal, "frame.silence")?;
    asm.set(ur(R[3]), node::BYPASS | node::SELF_BYPASS)?;
    asm.op(Instruction48::And {
        dst: R[3],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.compare(R[3], node::BYPASS, R[1], false)?;
    asm.jump_if(Condition::Equal, "frame.bypass")?;
    asm.mov(UniversalReg::Dag2Index(I8), ur(R[0]))?;
    asm.op(Instruction48::IndirectBranch {
        kind: BranchKind::Call,
        condition: Condition::Always,
        index: I8,
        modifier: M8,
    })?;
    asm.jump("frame.meter")?;
    asm.label("frame.bypass")?;
    asm.set(ur(R[3]), node::MUTE)?;
    asm.op(Instruction48::And {
        dst: R[3],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::NotEqual, "frame.silence")?;
    asm.call("kernel.copy")?;
    asm.jump("frame.meter")?;
    asm.label("frame.silence")?;
    asm.call("kernel.zero")?;
    // Peak |output| since the last meter read, then per-node cycles.
    asm.label("frame.meter")?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.load_at(R[1], I[4], node::OUTPUT)?;
    asm.mov(UniversalReg::Index(I[1]), ur(R[1]))?;
    asm.load_at(R[1], I[4], node::PEAK)?;
    asm.repeat(
        frames(),
        &[
            Instruction48::DmLoad {
                dst: R[0],
                index: I[1],
                modifier: M0,
                access: DmAccess::PostModify,
            },
            Instruction48::FloatAbs { dst: F0, src: F0 },
            Instruction48::FloatMax {
                dst: F1,
                lhs: F1,
                rhs: F0,
            },
        ],
    )?;
    asm.store_at(R[1], I[4], node::PEAK)?;
    asm.mov(ur(R[0]), clock)?;
    asm.op(Instruction48::IntegerSubtract {
        dst: R[0],
        lhs: R[0],
        rhs: R[13],
    })?;
    asm.store_at(R[0], I[4], node::CYCLES)?;
    asm.op(Instruction48::ModifyWords {
        index: I[4],
        words: node::WORDS as i32,
    })?;
    asm.jump("frame.node")?;

    // Interleave: f32 -> TDM word. ALUSAT stays clear (stock never sets it),
    // so FIX would return all ones on overflow (PRM 23.15): clip first.
    asm.label("frame.outputs")?;
    asm.set(ur(R[6]), TWO_POW_31)?;
    asm.set(ur(R[7]), FULL_SCALE)?;
    asm.set(UniversalReg::Index(I[4]), layout.outputs)?;
    asm.next_load(R[12], I[4], M0)?;
    asm.label("frame.output")?;
    asm.pass(R[12])?;
    asm.jump_if(Condition::Equal, "frame.account")?;
    asm.next_load(R[0], I[4], M0)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[0],
        lhs: R[0],
        rhs: R[11],
    })?;
    asm.mov(UniversalReg::Index(I[1]), ur(R[0]))?;
    asm.next_load(R[1], I[4], M0)?;
    asm.mov(UniversalReg::Index(I[0]), ur(R[1]))?;
    asm.repeat(
        frames(),
        &[
            Instruction48::DmLoad {
                dst: R[0],
                index: I[0],
                modifier: M0,
                access: DmAccess::PostModify,
            },
            Instruction48::FloatMultiply {
                dst: F0,
                lhs: F0,
                rhs: F6,
            },
            Instruction48::FloatClip {
                dst: F0,
                src: F0,
                bound: F7,
            },
            Instruction48::FixToInteger { dst: R[0], src: F0 },
            Instruction48::DmStore {
                src: R[0],
                index: I[1],
                modifier: M1,
                access: DmAccess::PostModify,
            },
        ],
    )?;
    asm.op(Instruction48::IntegerDecrement {
        dst: R[12],
        src: R[12],
    })?;
    asm.jump("frame.output")?;

    // CPU accounting in EMUCLK cycles.
    asm.label("frame.account")?;
    asm.mov(ur(R[0]), clock)?;
    asm.op(Instruction48::IntegerSubtract {
        dst: R[0],
        lhs: R[0],
        rhs: R[9],
    })?;
    asm.store(R[0], control + control::CYCLES_LAST)?;
    asm.load(R[1], control + control::CYCLES_PEAK)?;
    asm.op(Instruction48::IntegerCompareUnsigned {
        lhs: R[1],
        rhs: R[0],
    })?;
    asm.jump_if(Condition::GreaterOrEqual, "frame.count")?;
    asm.store(R[0], control + control::CYCLES_PEAK)?;
    asm.label("frame.count")?;
    asm.increment_word(control + control::FRAME_COUNT, R[0])?;
    asm.op(Instruction48::Rts)
}

fn kernels(asm: &mut Asm) -> Result<(), EmitError> {
    let load = |index| Instruction48::DmLoad {
        dst: R[0],
        index,
        modifier: M0,
        access: DmAccess::PostModify,
    };
    let store = Instruction48::DmStore {
        src: R[0],
        index: I[1],
        modifier: M0,
        access: DmAccess::PostModify,
    };
    // Parameter 0 is the linear gain.
    asm.label(KERNEL_GAIN)?;
    asm.load_at(R[4], I[3], 0)?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.repeat(
        frames(),
        &[
            load(I[0]),
            Instruction48::FloatMultiply {
                dst: F0,
                lhs: F0,
                rhs: F4,
            },
            store,
        ],
    )?;
    asm.op(Instruction48::Rts)?;

    asm.label(KERNEL_MIX)?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.repeat(
        frames(),
        &[
            load(I[0]),
            Instruction48::DmLoad {
                dst: R[1],
                index: I[2],
                modifier: M0,
                access: DmAccess::PostModify,
            },
            Instruction48::FloatAdd {
                dst: F0,
                lhs: F0,
                rhs: F1,
            },
            store,
        ],
    )?;
    asm.op(Instruction48::Rts)?;

    // Bypass copies input 0; bypass with mute and an inactive node write silence.
    asm.label("kernel.copy")?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.repeat(frames(), &[load(I[0]), store])?;
    asm.op(Instruction48::Rts)?;
    asm.label("kernel.zero")?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.set(ur(R[0]), 0)?;
    asm.repeat(frames(), &[store])?;
    asm.op(Instruction48::Rts)
}

/// Consume up to [`MESSAGES_PER_FRAME`] outgoing-ring messages. Indices name
/// the last published/consumed slot; a zero length is not yet visible and is
/// not acknowledged (stock reader rule).
fn poll(asm: &mut Asm, layout: &DataLayout, ring: &RingAddresses) -> Result<(), EmitError> {
    let control = layout.control;
    asm.label("poll")?;
    asm.set(ur(R[14]), MESSAGES_PER_FRAME)?;
    asm.label("poll.next")?;
    asm.pass(R[14])?;
    asm.jump_if(Condition::Equal, "poll.done")?;
    asm.load(R[0], ring.indices)?;
    asm.extract_byte(R[2], R[0], 0, R[1])?;
    asm.load(R[3], control + control::OUT_CONSUMER)?;
    asm.op(Instruction48::IntegerCompare {
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::Equal, "poll.done")?;
    asm.op(Instruction48::IntegerIncrement {
        dst: R[3],
        src: R[3],
    })?;
    asm.compare(R[3], ring.out_slots, R[1], false)?;
    asm.jump_if(Condition::NotEqual, "poll.slot")?;
    asm.set(ur(R[3]), 0)?;
    asm.label("poll.slot")?;
    asm.op(Instruction48::IntegerPass {
        dst: R[15],
        src: R[3],
    })?;
    asm.op(Instruction48::LogicalShift {
        dst: R[4],
        src: R[3],
        amount: 2,
    })?;
    asm.set(ur(R[1]), ring.out_lengths)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[4],
        lhs: R[4],
        rhs: R[1],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[4]))?;
    asm.load_at(R[5], I[7], 0)?;
    asm.pass(R[5])?;
    asm.jump_if(Condition::Equal, "poll.done")?;
    asm.multiply(R[4], R[3], ring.out_slot_bytes, R[1])?;
    asm.set(ur(R[1]), ring.out_payload)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[4],
        lhs: R[4],
        rhs: R[1],
    })?;
    asm.mov(UniversalReg::Index(I[5]), ur(R[4]))?;
    asm.load_at(R[0], I[5], 0)?;
    asm.extract_byte(R[2], R[0], 0, R[1])?;
    asm.set(ur(R[1]), layout.message_handlers)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[1],
        lhs: R[1],
        rhs: R[2],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[1]))?;
    asm.load_at(R[1], I[7], 0)?;
    asm.mov(UniversalReg::Dag2Index(I8), ur(R[1]))?;
    asm.op(Instruction48::IndirectBranch {
        kind: BranchKind::Call,
        condition: Condition::Always,
        index: I8,
        modifier: M8,
    })?;
    // Acknowledge after the handler has read the payload.
    asm.store(R[15], control + control::OUT_CONSUMER)?;
    asm.op(Instruction48::DmDirectByteStore {
        src: R[15],
        address: ring.indices + 1,
    })?;
    asm.increment_word(control + control::MESSAGES, R[0])?;
    asm.op(Instruction48::IntegerDecrement {
        dst: R[14],
        src: R[14],
    })?;
    asm.jump("poll.next")?;
    asm.label("poll.done")?;
    asm.op(Instruction48::Rts)
}

/// Publish the staged response (`R6` bytes) to the incoming ring, then ring
/// the ARM doorbell. A full ring drops it and counts the drop.
fn send(asm: &mut Asm, config: CodeConfig<'_>, ring: &RingAddresses) -> Result<(), EmitError> {
    let control = config.layout.control;
    asm.label("send")?;
    asm.load(R[0], ring.indices)?;
    asm.extract_byte(R[0], R[0], 3, R[1])?;
    asm.load(R[1], control + control::IN_PRODUCER)?;
    asm.op(Instruction48::IntegerIncrement {
        dst: R[1],
        src: R[1],
    })?;
    asm.compare(R[1], ring.in_slots, R[3], false)?;
    asm.jump_if(Condition::NotEqual, "send.slot")?;
    asm.set(ur(R[1]), 0)?;
    asm.label("send.slot")?;
    asm.op(Instruction48::IntegerCompare {
        lhs: R[1],
        rhs: R[0],
    })?;
    asm.jump_if(Condition::Equal, "send.full")?;
    asm.op(Instruction48::IntegerPass {
        dst: R[12],
        src: R[1],
    })?;
    asm.multiply(R[4], R[12], ring.in_slot_bytes, R[3])?;
    asm.set(ur(R[3]), ring.in_payload)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[4],
        lhs: R[4],
        rhs: R[3],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[4]))?;
    asm.set(UniversalReg::Index(I[6]), config.layout.response)?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    // One word per step through the byte-space slot pointer.
    asm.set(UniversalReg::Modify(M2), 1)?;
    // Words = (bytes + 3) / 4, never zero: every response has a header.
    asm.set(ur(R[8]), 3)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[8],
        lhs: R[8],
        rhs: R[6],
    })?;
    asm.op(Instruction48::LogicalShift {
        dst: R[8],
        src: R[8],
        amount: -2,
    })?;
    asm.repeat(
        LoopCount::Register(R[8]),
        &[
            Instruction48::DmLoad {
                dst: R[0],
                index: I[6],
                modifier: M0,
                access: DmAccess::PostModify,
            },
            Instruction48::DmStore {
                src: R[0],
                index: I[7],
                modifier: M2,
                access: DmAccess::PostModify,
            },
        ],
    )?;
    asm.op(Instruction48::LogicalShift {
        dst: R[4],
        src: R[12],
        amount: 2,
    })?;
    asm.set(ur(R[3]), ring.in_lengths)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[4],
        lhs: R[4],
        rhs: R[3],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[4]))?;
    asm.store_at(R[6], I[7], 0)?;
    asm.store(R[12], control + control::IN_PRODUCER)?;
    asm.op(Instruction48::DmDirectByteStore {
        src: R[12],
        address: ring.indices + 2,
    })?;
    asm.set(ur(R[0]), arm_doorbell(config.core))?;
    asm.store(R[0], TRU0_MTR)?;
    asm.op(Instruction48::Rts)?;
    asm.label("send.full")?;
    asm.increment_word(control + control::DROPPED_RESPONSES, R[0])?;
    asm.op(Instruction48::Rts)
}

/// Start a response at the staging area: header word 0 and total bytes.
fn response_header(
    asm: &mut Asm,
    layout: &DataLayout,
    kind: u8,
    selector: u8,
    total: u32,
) -> Result<(), EmitError> {
    asm.set(UniversalReg::Index(I[6]), layout.response)?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.set(ur(R[1]), u32::from(kind) | (u32::from(selector) << 8))?;
    asm.next_store(R[1], I[6], M0)?;
    asm.set(ur(R[1]), total)?;
    asm.next_store(R[1], I[6], M0)
}

fn handlers(asm: &mut Asm, config: CodeConfig<'_>) -> Result<(), EmitError> {
    let layout = config.layout;
    let control = layout.control;
    let core = config.ring.wire();

    asm.label(UNSUPPORTED_HANDLER)?;
    asm.increment_word(control + control::UNSUPPORTED, R[0])?;
    asm.store(R[2], control + control::LAST_UNSUPPORTED)?;
    asm.op(Instruction48::Rts)?;
    asm.label("msg.reject")?;
    asm.increment_word(control + control::REJECTED, R[0])?;
    asm.op(Instruction48::Rts)?;

    // ParameterChange (20 B): +2 slot, +4 index, +16 f32.
    asm.label("msg.parameter")?;
    asm.compare(R[5], 20, R[1], false)?;
    asm.jump_if(Condition::NotEqual, "msg.reject")?;
    asm.load_at(R[3], I[5], 0)?;
    asm.extract_byte(R[0], R[3], 2, R[1])?;
    asm.load_window(R[6], I[5], 4)?;
    asm.load_window(R[7], I[5], 16)?;
    asm.jump("set_parameter")?;

    // R0 slot, R6 index, R7 value bits. Non-finite values are refused.
    asm.label("set_parameter")?;
    asm.compare(R[0], u32::from(SLOT_COUNT), R[1], true)?;
    asm.jump_if(Condition::GreaterOrEqual, "msg.reject")?;
    asm.compare(R[6], PARAMETERS_PER_SLOT, R[1], true)?;
    asm.jump_if(Condition::GreaterOrEqual, "msg.reject")?;
    asm.op(Instruction48::LogicalShift {
        dst: R[1],
        src: R[7],
        amount: -23,
    })?;
    asm.set(ur(R[3]), 0xff)?;
    asm.op(Instruction48::And {
        dst: R[1],
        lhs: R[1],
        rhs: R[3],
    })?;
    asm.op(Instruction48::IntegerCompare {
        lhs: R[1],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::Equal, "msg.reject")?;
    asm.op(Instruction48::LogicalShift {
        dst: R[0],
        src: R[0],
        amount: PARAMETERS_PER_SLOT.trailing_zeros() as i8,
    })?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[0],
        lhs: R[0],
        rhs: R[6],
    })?;
    asm.set(ur(R[1]), layout.parameters)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[0],
        lhs: R[0],
        rhs: R[1],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[0]))?;
    asm.store_at(R[7], I[7], 0)?;
    asm.op(Instruction48::Rts)?;

    // BulkParameterChange (16 + 12n): +12 n, records {slot, index, f32}.
    asm.label("msg.bulk_parameter")?;
    bulk_records(
        asm,
        12,
        "set_parameter",
        &[(R[0], 0), (R[6], 4), (R[7], 8)],
        None,
    )?;

    // ModelBypass (20 B): +2 slot, +16 f32 (0.0 active, 1.0 bypass).
    asm.label("msg.bypass")?;
    asm.compare(R[5], 20, R[1], false)?;
    asm.jump_if(Condition::NotEqual, "msg.reject")?;
    asm.load_at(R[3], I[5], 0)?;
    asm.extract_byte(R[0], R[3], 2, R[1])?;
    asm.load_window(R[7], I[5], 16)?;
    asm.jump("set_bypass")?;

    // R0 slot, R7 value bits: 0.0 active, 1.0 bypass, 2.0 bypass with mute
    // (stock mode = trunc(value), k1 `0x1659b0`). Other values are refused.
    asm.label("set_bypass")?;
    node_of_slot(asm, layout)?;
    asm.load_at(R[2], I[7], node::FLAGS)?;
    asm.set(ur(R[3]), !(node::BYPASS | node::MUTE))?;
    asm.op(Instruction48::And {
        dst: R[2],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.pass(R[7])?;
    asm.jump_if(Condition::Equal, "set_bypass.store")?;
    asm.set(ur(R[3]), node::BYPASSABLE)?;
    asm.op(Instruction48::And {
        dst: R[3],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::Equal, "msg.reject")?;
    asm.set(ur(R[3]), node::BYPASS)?;
    asm.compare(R[7], ONE, R[1], false)?;
    asm.jump_if(Condition::Equal, "set_bypass.on")?;
    asm.compare(R[7], TWO, R[1], false)?;
    asm.jump_if(Condition::NotEqual, "msg.reject")?;
    asm.set(ur(R[3]), node::BYPASS | node::MUTE)?;
    asm.label("set_bypass.on")?;
    asm.op(Instruction48::Or {
        dst: R[2],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.label("set_bypass.store")?;
    asm.store_at(R[2], I[7], node::FLAGS)?;
    asm.op(Instruction48::Rts)?;

    // BulkModelBypass (16 + 8n): records {slot, f32}.
    asm.label("msg.bulk_bypass")?;
    bulk_records(asm, 8, "set_bypass", &[(R[0], 0), (R[7], 4)], None)?;

    // MeterRequest (16 B) -> BulkMeter 0x0d. Owned meaning: +2 is the first
    // slot; values are output peaks of up to 16 slots, reset on read.
    asm.label("msg.meter")?;
    asm.compare(R[5], 16, R[1], false)?;
    asm.jump_if(Condition::NotEqual, "msg.reject")?;
    asm.load_at(R[3], I[5], 0)?;
    asm.extract_byte(R[0], R[3], 2, R[1])?;
    asm.compare(R[0], u32::from(SLOT_COUNT), R[1], true)?;
    asm.jump_if(Condition::GreaterOrEqual, "msg.reject")?;
    response_header(asm, layout, 0x0d, core, 8 + 76)?;
    asm.next_store(R[0], I[6], M0)?;
    asm.load_window(R[1], I[5], 4)?;
    asm.next_store(R[1], I[6], M0)?;
    asm.set(ur(R[3]), u32::from(SLOT_COUNT))?;
    asm.op(Instruction48::IntegerSubtract {
        dst: R[3],
        lhs: R[3],
        rhs: R[0],
    })?;
    asm.compare(R[3], 16, R[1], true)?;
    asm.jump_if(Condition::LessThan, "msg.meter.count")?;
    asm.set(ur(R[3]), 16)?;
    asm.label("msg.meter.count")?;
    asm.next_store(R[3], I[6], M0)?;
    asm.set(ur(R[1]), layout.slot_index)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[1],
        lhs: R[1],
        rhs: R[0],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[1]))?;
    asm.set(ur(R[12]), 16)?;
    asm.op(Instruction48::IntegerSubtract {
        dst: R[12],
        lhs: R[12],
        rhs: R[3],
    })?;
    asm.label("msg.meter.value")?;
    asm.pass(R[3])?;
    asm.jump_if(Condition::Equal, "msg.meter.pad")?;
    asm.next_load(R[1], I[7], M0)?;
    asm.set(ur(R[4]), 0)?;
    asm.pass(R[1])?;
    asm.jump_if(Condition::Equal, "msg.meter.put")?;
    asm.mov(UniversalReg::Index(I[2]), ur(R[1]))?;
    asm.load_at(R[4], I[2], node::PEAK)?;
    asm.set(ur(R[1]), 0)?;
    asm.store_at(R[1], I[2], node::PEAK)?;
    asm.label("msg.meter.put")?;
    asm.next_store(R[4], I[6], M0)?;
    asm.op(Instruction48::IntegerDecrement {
        dst: R[3],
        src: R[3],
    })?;
    asm.jump("msg.meter.value")?;
    asm.label("msg.meter.pad")?;
    asm.pass(R[12])?;
    asm.jump_if(Condition::Equal, "msg.meter.send")?;
    asm.set(ur(R[1]), 0)?;
    asm.next_store(R[1], I[6], M0)?;
    asm.op(Instruction48::IntegerDecrement {
        dst: R[12],
        src: R[12],
    })?;
    asm.jump("msg.meter.pad")?;
    asm.label("msg.meter.send")?;
    asm.set(ur(R[6]), 8 + 76)?;
    asm.jump("send")?;

    // DiagnosticsRequest (16 B) -> Diagnostics 0x00, 16 words. Stock reads
    // w3 FPEx count, w4 FPEx slot, w12 error flags: not measured here, sent
    // as 0 (not a health claim). Owned extension: w13 last-frame cycles, w14
    // peak cycles, w15 frame count.
    asm.label("msg.diagnostics")?;
    asm.compare(R[5], 16, R[1], false)?;
    asm.jump_if(Condition::NotEqual, "msg.reject")?;
    response_header(asm, layout, 0x00, core % 2, 8 + 64)?;
    asm.set(ur(R[0]), 0)?;
    asm.repeat(
        LoopCount::Immediate(NonZeroU16::new(13).expect("nonzero")),
        &[Instruction48::DmStore {
            src: R[0],
            index: I[6],
            modifier: M0,
            access: DmAccess::PostModify,
        }],
    )?;
    for word in [
        control::CYCLES_LAST,
        control::CYCLES_PEAK,
        control::FRAME_COUNT,
    ] {
        asm.load(R[0], control + word)?;
        asm.next_store(R[0], I[6], M0)?;
    }
    asm.set(ur(R[6]), 8 + 64)?;
    asm.jump("send")?;

    // BulkStartModel (16 + 12n): records {module id, u16 slot | aux << 16,
    // extra}. A slot starts only when its node was built for that module id;
    // anything else is reported failed -> BulkModelStart 0x0c
    // {core, started mask, failed mask}.
    asm.label("msg.bulk_start")?;
    asm.set(ur(R[10]), 0)?;
    asm.set(ur(R[11]), 0)?;
    bulk_records(
        asm,
        12,
        "start_slot",
        &[(R[7], 0), (R[0], 4)],
        Some("msg.bulk_start.reply"),
    )?;
    asm.label("start_slot")?;
    asm.set(ur(R[1]), 0xffff)?;
    asm.op(Instruction48::And {
        dst: R[0],
        lhs: R[0],
        rhs: R[1],
    })?;
    asm.compare(R[0], u32::from(SLOT_COUNT), R[1], true)?;
    asm.jump_if(Condition::GreaterOrEqual, "msg.reject")?;
    asm.set(ur(R[4]), 1)?;
    asm.op(Instruction48::LogicalShiftBy {
        dst: R[4],
        src: R[4],
        amount: R[0],
    })?;
    asm.set(ur(R[1]), layout.slot_index)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[1],
        lhs: R[1],
        rhs: R[0],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[1]))?;
    asm.load_at(R[1], I[7], 0)?;
    asm.pass(R[1])?;
    asm.jump_if(Condition::Equal, "start_slot.failed")?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[1]))?;
    asm.load_at(R[2], I[7], node::FLAGS)?;
    asm.set(ur(R[3]), node::MODULE_KNOWN)?;
    asm.op(Instruction48::And {
        dst: R[3],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::Equal, "start_slot.failed")?;
    asm.load_at(R[3], I[7], node::MODULE)?;
    asm.op(Instruction48::IntegerCompare {
        lhs: R[3],
        rhs: R[7],
    })?;
    asm.jump_if(Condition::Equal, "start_slot.owned")?;
    asm.label("start_slot.failed")?;
    asm.op(Instruction48::Or {
        dst: R[11],
        lhs: R[11],
        rhs: R[4],
    })?;
    asm.op(Instruction48::Rts)?;
    asm.label("start_slot.owned")?;
    asm.set(ur(R[3]), node::ACTIVE)?;
    asm.op(Instruction48::Or {
        dst: R[2],
        lhs: R[2],
        rhs: R[3],
    })?;
    // A (re)start runs an owned kernel's reset and resends its parameters.
    asm.set(ur(R[3]), !node::STARTED)?;
    asm.op(Instruction48::And {
        dst: R[2],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.store_at(R[2], I[7], node::FLAGS)?;
    asm.op(Instruction48::Or {
        dst: R[10],
        lhs: R[10],
        rhs: R[4],
    })?;
    asm.op(Instruction48::Rts)?;
    asm.label("msg.bulk_start.reply")?;
    response_header(asm, layout, 0x0c, core, 8 + 12)?;
    asm.set(ur(R[0]), u32::from(core))?;
    asm.next_store(R[0], I[6], M0)?;
    asm.next_store(R[10], I[6], M0)?;
    asm.next_store(R[11], I[6], M0)?;
    asm.set(ur(R[6]), 8 + 12)?;
    asm.jump("send")?;

    // DeleteAllModels (16 B): every node becomes inactive (silent output) and
    // not started.
    asm.label("msg.delete_all")?;
    asm.set(UniversalReg::Index(I[7]), layout.nodes)?;
    asm.set(ur(R[3]), !(node::ACTIVE | node::STARTED))?;
    asm.label("msg.delete_all.next")?;
    asm.load_at(R[0], I[7], node::KERNEL)?;
    asm.pass(R[0])?;
    asm.jump_if(Condition::Equal, "msg.delete_all.done")?;
    asm.load_at(R[2], I[7], node::FLAGS)?;
    asm.op(Instruction48::And {
        dst: R[2],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.store_at(R[2], I[7], node::FLAGS)?;
    asm.op(Instruction48::ModifyWords {
        index: I[7],
        words: node::WORDS as i32,
    })?;
    asm.jump("msg.delete_all.next")?;
    asm.label("msg.delete_all.done")?;
    asm.op(Instruction48::Rts)
}

/// `I7` = node record of slot `R0`, or reject an unknown/empty slot.
fn node_of_slot(asm: &mut Asm, layout: &DataLayout) -> Result<(), EmitError> {
    asm.compare(R[0], u32::from(SLOT_COUNT), R[1], true)?;
    asm.jump_if(Condition::GreaterOrEqual, "msg.reject")?;
    asm.set(ur(R[1]), layout.slot_index)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[1],
        lhs: R[1],
        rhs: R[0],
    })?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[1]))?;
    asm.load_at(R[1], I[7], 0)?;
    asm.pass(R[1])?;
    asm.jump_if(Condition::Equal, "msg.reject")?;
    asm.mov(UniversalReg::Index(I[7]), ur(R[1]))
}

/// Validate `16 + stride * n` (n at +12), then CALL `each` per record with the
/// listed fields (byte offsets in the record) loaded. A rejected record is counted and skipped. Afterwards
/// jump to `then`, or return.
fn bulk_records(
    asm: &mut Asm,
    stride: u32,
    each: &str,
    fields: &[(DataReg, u32)],
    then: Option<&str>,
) -> Result<(), EmitError> {
    let next = asm.fresh("bulk.next");
    let done = asm.fresh("bulk.done");
    asm.load_window(R[8], I[5], 12)?;
    // n < 171 bounds 16 + stride * n inside one 2048-byte slot and avoids wrap.
    asm.compare(R[8], 171, R[1], true)?;
    asm.jump_if(Condition::GreaterOrEqual, "msg.reject")?;
    asm.multiply(R[1], R[8], stride, R[3])?;
    asm.set(ur(R[3]), 16)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[1],
        lhs: R[1],
        rhs: R[3],
    })?;
    asm.op(Instruction48::IntegerCompare {
        lhs: R[1],
        rhs: R[5],
    })?;
    asm.jump_if(Condition::NotEqual, "msg.reject")?;
    asm.mov(ur(R[1]), UniversalReg::Index(I[5]))?;
    asm.set(ur(R[3]), 16)?;
    asm.op(Instruction48::IntegerAdd {
        dst: R[1],
        lhs: R[1],
        rhs: R[3],
    })?;
    asm.mov(UniversalReg::Index(I[6]), ur(R[1]))?;
    asm.label(&next)?;
    asm.pass(R[8])?;
    asm.jump_if(Condition::Equal, &done)?;
    for &(register, offset) in fields {
        asm.load_window(register, I[6], offset)?;
    }
    asm.call(each)?;
    // I6 is a byte-space window pointer: the plain form adds the stride in bytes
    // (review-w4-dsp F1; the NW form would scale it by 4).
    asm.op(Instruction48::ModifyBytes {
        index: I[6],
        bytes: stride as i32,
    })?;
    asm.op(Instruction48::IntegerDecrement {
        dst: R[8],
        src: R[8],
    })?;
    asm.jump(&next)?;
    asm.label(&done)?;
    match then {
        Some(label) => asm.jump(label),
        None => asm.op(Instruction48::Rts),
    }
}

/// Initial shadow of a parameter word: a NaN, so the parameter handler (which
/// refuses non-finite values) never stores it. The image also starts an owned
/// node's slot parameter words at this value; else the first frame would
/// deliver their initial bits to the block.
pub const NOT_DELIVERED: u32 = 0xffff_ffff;
/// Words of the shared adapter-B stack, ending at `stack_top` (the image's
/// `DataLayout::stack_top`). The adapter pushes 2; `tests/adapter_b.rs` maps
/// only these words, so a deeper leaf stack faults there.
pub const BLOCK_STACK_WORDS: u32 = 64;
const _: () = assert!(FRAMES as usize == BLOCK_SAMPLES, "one leaf call per frame");
/// Bypass ramp of the stock kernel mixer: 0.02 s (k1 `0x165922`) at 48 kHz
/// (k1 `0x161fd9`) = 960 samples = 30 frames.
pub const BYPASS_RAMP_FRAMES: u32 = 30;
/// Per-sample step = (target - gain) * 1/960, as stock `|target - gain| * rate`
/// (k1 `0x162043`). The f32 value of the rate is inferred.
const BYPASS_RATE: f32 = 1.0 / 960.0;
const _: () = assert!(BYPASS_RAMP_FRAMES * FRAMES == 960);

/// Bypass mixer words of an owned node, after its parameter shadows.
mod mixer {
    /// `node::BYPASS | node::MUTE` of the record that the ramp targets.
    pub const MODE: u32 = 0;
    /// f32 gain of the dry input.
    pub const DRY: u32 = 1;
    /// f32 gain of the processed output.
    pub const WET: u32 = 2;
    pub const DRY_STEP: u32 = 3;
    pub const WET_STEP: u32 = 4;
    /// Frames left in the ramp; 0: the gains are the targets of `MODE`.
    pub const FRAMES_LEFT: u32 = 5;
}

/// One owned node (adapter B): `block` runs in `slot` as a mono resident
/// kernel. `memory` is the first L1 word of the [`block_memory`] image.
#[derive(Clone, Copy)]
pub struct BlockNode<'a> {
    pub slot: SlotId,
    pub block: &'a dyn OwnedBlock,
    pub memory: u32,
}

/// Adapter-B code: one segment per distinct body (bodies share label names
/// such as `math.*`), then the node kernels; contiguous from the origin.
/// Empty (the default) without owned blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockCode {
    pub segments: Vec<OwnedCode>,
    /// The node-table `KERNEL` word of each owned slot.
    pub kernels: BTreeMap<SlotId, PmWordAddress24>,
}

impl BlockCode {
    pub fn word_count(&self) -> usize {
        self.segments.iter().map(OwnedCode::word_count).sum()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlockKernelError {
    #[error(transparent)]
    Block(#[from] BlockError),
    #[error(transparent)]
    Emit(#[from] EmitError),
    #[error(transparent)]
    Address(#[from] tetrad_sharc::address::AddressError),
    #[error("model {0} has one stereo state; resident nodes are mono")]
    Stereo(u32),
    #[error("model {model} parameter {index} is outside the {PARAMETERS_PER_SLOT} slot words")]
    ParameterIndex { model: u32, index: u32 },
    #[error("slot {0:?} has two owned blocks")]
    DuplicateSlot(SlotId),
}

/// Node memory: channel A state, one shadow per parameter, the bypass mixer.
#[derive(Debug, Clone, Copy)]
struct NodeMemory {
    state: u32,
    shadows: u32,
    mixer: u32,
}

impl NodeMemory {
    fn new(block: &dyn OwnedBlock, memory: u32) -> Self {
        let state_words = block.static_layout().states()[0].len().max(1) as u32;
        let shadows = memory + state_words;
        Self {
            state: memory,
            shadows,
            mixer: shadows + block.parameters().len() as u32,
        }
    }
}

/// Initial words of a [`BlockNode`]'s memory: the block's channel A state
/// (path b nodes are mono; A = left as in adapter A), a [`NOT_DELIVERED`]
/// shadow per parameter, then six bypass mixer words (active and settled).
/// The started flag is [`node::STARTED`] in the node record, which the
/// resident clears on a restart.
pub fn block_memory(block: &dyn OwnedBlock) -> Vec<u32> {
    let parameters = block.parameters().len();
    let mut words = block.static_layout().states()[0].clone();
    words.resize(words.len().max(1), 0);
    words.extend(std::iter::repeat_n(NOT_DELIVERED, parameters));
    // MODE, DRY, WET, DRY_STEP, WET_STEP, FRAMES_LEFT.
    words.extend([0, 0, ONE, 0, 0, 0]);
    words
}

/// Adapter B: owned tetrad-blocks bodies as resident node kernels, placed from
/// `origin` (for example the end of [`generate`]'s code). Each kernel follows
/// the resident kernel ABI (`I0` input, `I1` output, `I3` the slot's
/// parameter words) and keeps R12 and I4 (the node record) for the
/// scheduler. Per call it: sets the C run-time (stack at `stack_top`,
/// M5-M7); when the record's [`node::STARTED`] flag is clear (the first call
/// after an image load, DeleteAllModels or BulkStartModel) sets it, marks
/// every shadow [`NOT_DELIVERED`] and runs the reset leaf (R8 = 1), as V2
/// does after the entry; sends
/// each parameter word that changed since the last call to the set-parameter
/// leaf, in index order, dropping bits above 1.0 unsigned like adapter A;
/// copies the input to the output and runs the process leaf on it in place,
/// also while the node is bypassed (stock k1 `0x166049..0x16607f`: process
/// runs on a copy, then the bypass mixer). The output is then the stock
/// bypass mixer: `dry * DRY + processed * WET`, two gains that ramp linearly
/// per sample over [`BYPASS_RAMP_FRAMES`] towards the targets of the record's
/// mode: active (0, 1), bypass (1, 0), bypass with mute (0, 0) (k1
/// `0x1659b0`). A settled node writes the processed output, the dry input or
/// silence. A reset settles the gains at the current mode. The scheduler
/// calls these kernels also while bypassed ([`node::SELF_BYPASS`]).
/// The leaves see L1 word addresses, the kernel's modes (RND32; CBUFEN is
/// clear but every L is 0) and R4 = I4 = channel A state. Blocks with a
/// [`StaticLayout::Stereo`] state are refused.
pub fn emit_block_kernels(
    origin: PmWordAddress24,
    nodes: &[BlockNode<'_>],
    stack_top: u32,
) -> Result<BlockCode, BlockKernelError> {
    let mut slots = Vec::new();
    for node in nodes {
        let block = node.block;
        if slots.contains(&node.slot) {
            return Err(BlockKernelError::DuplicateSlot(node.slot));
        }
        slots.push(node.slot);
        if let StaticLayout::Stereo(_) = block.static_layout() {
            return Err(BlockKernelError::Stereo(block.model_id()));
        }
        if let Some(spec) = block
            .parameters()
            .iter()
            .find(|spec| spec.runtime_index >= PARAMETERS_PER_SLOT)
        {
            return Err(BlockKernelError::ParameterIndex {
                model: block.model_id(),
                index: spec.runtime_index,
            });
        }
    }

    // Nodes with one model id share its body.
    let mut segments = Vec::new();
    let mut bodies: BTreeMap<u32, [PmWordAddress24; 3]> = BTreeMap::new();
    let mut next = origin.get();
    for node in nodes {
        let model = node.block.model_id();
        if bodies.contains_key(&model) {
            continue;
        }
        let mut program = IsaProgram::new(PmWordAddress24::new(next)?);
        let labels = node.block.emit_body(&mut program)?;
        let code = program.finish()?;
        let address = |label: &Label| {
            code.symbols()
                .get(label)
                .copied()
                .ok_or_else(|| BlockError::MissingRoutine(label.clone()))
        };
        bodies.insert(
            model,
            [
                address(&labels.process)?,
                address(&labels.set_parameter)?,
                address(&labels.reset)?,
            ],
        );
        next += code.word_count() as u32;
        segments.push(code);
    }

    let mut asm = Asm {
        program: IsaProgram::new(PmWordAddress24::new(next)?),
        unique: 0,
    };
    for node in nodes {
        let [process, set_parameter, reset] = bodies[&node.block.model_id()];
        let memory = NodeMemory::new(node.block, node.memory);
        block_kernel(
            &mut asm,
            node,
            memory,
            stack_top,
            [process, set_parameter, reset],
        )?;
    }
    let kernels_code = asm.program.finish()?;
    let kernels = nodes
        .iter()
        .map(|node| (node.slot, kernels_code.symbols()[&block_label(node.slot)]))
        .collect();
    segments.push(kernels_code);
    Ok(BlockCode { segments, kernels })
}

fn block_label(slot: SlotId) -> Label {
    Label::new(format!("block.{}", slot.get()))
}

fn block_kernel(
    asm: &mut Asm,
    node: &BlockNode<'_>,
    memory: NodeMemory,
    stack_top: u32,
    [process, set_parameter, reset]: [PmWordAddress24; 3],
) -> Result<(), EmitError> {
    let call = |asm: &mut Asm, target: PmWordAddress24| {
        asm.op(Instruction48::Branch {
            kind: BranchKind::Call,
            condition: Condition::Always,
            target: BranchTarget::Absolute(target),
        })
    };
    let state = |asm: &mut Asm| -> Result<(), EmitError> {
        asm.set(ur(R[4]), memory.state)?;
        asm.mov(UniversalReg::Index(I[4]), ur(R[4]))
    };
    let mixer = |word: u32| memory.mixer + word;
    let name = block_label(node.slot);
    let label = |part: &str| format!("{}.{part}", name.name());
    asm.program.define(name.clone())?;
    asm.set(UniversalReg::Index(I[7]), stack_top)?;
    asm.set(UniversalReg::Modify(M5), 0)?;
    asm.set(UniversalReg::Modify(M6), 1)?;
    asm.set(UniversalReg::Modify(M7), -1_i32 as u32)?;
    asm.mov(ur(R[0]), UniversalReg::Index(I[4]))?;
    for saved in [R[0], R[12]] {
        asm.op(Instruction48::DmStore {
            src: saved,
            index: I[7],
            modifier: M7,
            access: DmAccess::PostModify,
        })?;
    }

    // I4 is still the node record; R4 = its bypass mode. A clear STARTED flag
    // (first call or a restart) settles the mixer at the mode, runs the reset
    // and resends every parameter word.
    let parameters = label("parameters");
    asm.load_at(R[2], I[4], node::FLAGS)?;
    asm.set(ur(R[3]), node::BYPASS | node::MUTE)?;
    asm.op(Instruction48::And {
        dst: R[4],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.set(ur(R[3]), node::STARTED)?;
    asm.op(Instruction48::And {
        dst: R[0],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::NotEqual, &label("running"))?;
    asm.op(Instruction48::Or {
        dst: R[2],
        lhs: R[2],
        rhs: R[3],
    })?;
    asm.store_at(R[2], I[4], node::FLAGS)?;
    asm.store(R[4], mixer(mixer::MODE))?;
    mixer_targets(asm, R[4])?;
    asm.store(R[0], mixer(mixer::DRY))?;
    asm.store(R[1], mixer(mixer::WET))?;
    asm.set(ur(R[0]), 0)?;
    asm.store(R[0], mixer(mixer::FRAMES_LEFT))?;
    asm.set(ur(R[0]), NOT_DELIVERED)?;
    for shadow in (memory.shadows..).take(node.block.parameters().len()) {
        asm.store(R[0], shadow)?;
    }
    state(asm)?;
    asm.set(ur(R[8]), 1)?;
    call(asm, reset)?;
    asm.jump(&parameters)?;

    // A new mode ramps both gains from their current values to its targets.
    asm.label(&label("running"))?;
    asm.load(R[3], mixer(mixer::MODE))?;
    asm.op(Instruction48::IntegerCompare {
        lhs: R[4],
        rhs: R[3],
    })?;
    asm.jump_if(Condition::Equal, &parameters)?;
    asm.store(R[4], mixer(mixer::MODE))?;
    mixer_targets(asm, R[4])?;
    asm.set(ur(R[5]), BYPASS_RATE.to_bits())?;
    for (target, float, gain, step) in [
        (R[0], F0, mixer::DRY, mixer::DRY_STEP),
        (R[1], F1, mixer::WET, mixer::WET_STEP),
    ] {
        asm.load(R[6], mixer(gain))?;
        asm.op(Instruction48::FloatSubtract {
            dst: float,
            lhs: float,
            rhs: F6,
        })?;
        asm.op(Instruction48::FloatMultiply {
            dst: float,
            lhs: float,
            rhs: F5,
        })?;
        asm.store(target, mixer(step))?;
    }
    asm.set(ur(R[0]), BYPASS_RAMP_FRAMES)?;
    asm.store(R[0], mixer(mixer::FRAMES_LEFT))?;

    asm.label(&parameters)?;
    for (shadow, spec) in (memory.shadows..).zip(node.block.parameters()) {
        let next = asm.fresh("block.parameter");
        asm.load_at(R[1], I[3], spec.runtime_index)?;
        asm.load(R[0], shadow)?;
        asm.op(Instruction48::IntegerCompare {
            lhs: R[1],
            rhs: R[0],
        })?;
        asm.jump_if(Condition::Equal, &next)?;
        asm.store(R[1], shadow)?;
        asm.compare(R[1], ONE, R[0], true)?;
        asm.jump_if(Condition::GreaterThan, &next)?;
        state(asm)?;
        asm.set(ur(R[8]), spec.runtime_index)?;
        asm.mov(ur(R[12]), ur(R[1]))?;
        call(asm, set_parameter)?;
        asm.label(&next)?;
    }

    // Kernels never alias: process the copy in the output buffer.
    let load = |index| Instruction48::DmLoad {
        dst: R[0],
        index,
        modifier: M0,
        access: DmAccess::PostModify,
    };
    let store = Instruction48::DmStore {
        src: R[0],
        index: I[1],
        modifier: M0,
        access: DmAccess::PostModify,
    };
    asm.mov(ur(R[8]), UniversalReg::Index(I[1]))?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.repeat(frames(), &[load(I[0]), store])?;
    state(asm)?;
    call(asm, process)?;
    asm.load(R[12], stack_top - 1)?;
    asm.load(R[0], stack_top)?;
    asm.mov(UniversalReg::Index(I[4]), ur(R[0]))?;

    // Bypass mixer. Settled: active keeps the processed output, bypass
    // copies the dry input, bypass with mute writes silence.
    let done = label("done");
    asm.load_at(R[1], I[4], node::INPUT0)?;
    asm.mov(UniversalReg::Index(I[0]), ur(R[1]))?;
    asm.load_at(R[1], I[4], node::OUTPUT)?;
    asm.mov(UniversalReg::Index(I[1]), ur(R[1]))?;
    asm.set(UniversalReg::Modify(M0), 1)?;
    asm.load(R[0], mixer(mixer::FRAMES_LEFT))?;
    asm.pass(R[0])?;
    asm.jump_if(Condition::NotEqual, &label("ramp"))?;
    asm.load(R[2], mixer(mixer::MODE))?;
    asm.pass(R[2])?;
    asm.jump_if(Condition::Equal, &done)?;
    asm.compare(R[2], node::BYPASS, R[3], false)?;
    asm.jump_if(Condition::NotEqual, &label("mute"))?;
    asm.repeat(frames(), &[load(I[0]), store])?;
    asm.jump(&done)?;
    asm.label(&label("mute"))?;
    asm.set(ur(R[0]), 0)?;
    asm.repeat(frames(), &[store])?;
    asm.jump(&done)?;

    // Ramp: per sample `dry * DRY + processed * WET`, then each gain += step
    // (stock core k1 `0x16296a`; the use-then-step order is inferred). The
    // last ramp frame ends exactly at the targets.
    asm.label(&label("ramp"))?;
    asm.op(Instruction48::IntegerDecrement {
        dst: R[0],
        src: R[0],
    })?;
    asm.store(R[0], mixer(mixer::FRAMES_LEFT))?;
    for (register, word) in [
        (R[4], mixer::DRY),
        (R[5], mixer::WET),
        (R[6], mixer::DRY_STEP),
        (R[7], mixer::WET_STEP),
    ] {
        asm.load(register, mixer(word))?;
    }
    // A leaf may change M5; the processed sample is read in place.
    asm.set(UniversalReg::Modify(M5), 0)?;
    asm.repeat(
        frames(),
        &[
            load(I[0]),
            Instruction48::DmLoad {
                dst: R[1],
                index: I[1],
                modifier: M5,
                access: DmAccess::PreModify,
            },
            Instruction48::FloatMultiply {
                dst: F0,
                lhs: F0,
                rhs: F4,
            },
            Instruction48::FloatMultiply {
                dst: F1,
                lhs: F1,
                rhs: F5,
            },
            Instruction48::FloatAdd {
                dst: F0,
                lhs: F0,
                rhs: F1,
            },
            store,
            Instruction48::FloatAdd {
                dst: F4,
                lhs: F4,
                rhs: F6,
            },
            Instruction48::FloatAdd {
                dst: F5,
                lhs: F5,
                rhs: F7,
            },
        ],
    )?;
    asm.store(R[4], mixer(mixer::DRY))?;
    asm.store(R[5], mixer(mixer::WET))?;
    asm.load(R[0], mixer(mixer::FRAMES_LEFT))?;
    asm.pass(R[0])?;
    asm.jump_if(Condition::NotEqual, &done)?;
    asm.load(R[2], mixer(mixer::MODE))?;
    mixer_targets(asm, R[2])?;
    asm.store(R[0], mixer(mixer::DRY))?;
    asm.store(R[1], mixer(mixer::WET))?;
    asm.label(&done)?;
    asm.op(Instruction48::Rts)
}

/// R0 = dry and R1 = wet target gain (f32 bits) of bypass mode `mode`
/// (`node::BYPASS | node::MUTE` bits): stock dry = 1.0 only for bypass, wet =
/// 1.0 only for active (k1 `0x1659b0`). Clobbers R3.
fn mixer_targets(asm: &mut Asm, mode: DataReg) -> Result<(), EmitError> {
    let (dry, wet) = (asm.fresh("mixer.dry"), asm.fresh("mixer.wet"));
    asm.set(ur(R[0]), 0)?;
    asm.set(ur(R[1]), 0)?;
    asm.compare(mode, node::BYPASS, R[3], false)?;
    asm.jump_if(Condition::NotEqual, &dry)?;
    asm.set(ur(R[0]), ONE)?;
    asm.label(&dry)?;
    asm.pass(mode)?;
    asm.jump_if(Condition::NotEqual, &wet)?;
    asm.set(ur(R[1]), ONE)?;
    asm.label(&wet)
}
