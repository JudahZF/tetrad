//! Adapter A: static DM state, a VISA entry and stubs, and position-independent ISA.

use qc_format::blob::{CodeRelocation, LinkRange, ModuleBlob, Section, SectionBytes, SectionWords};
use tetrad_sharc::address::PmWordAddress24;
use tetrad_sharc::emit::{BranchRelocation, IsaProgram, Label, OwnedCode};
use tetrad_sharc::isa48::{BranchKind, C_RETURN, Condition, DmAccess, Instruction48};
use tetrad_sharc::register::{Dag1Index, Dag1Modify, DataReg, UniversalReg};
use tetrad_sharc::visa::{VisaCode, VisaInstruction};

use crate::{BlockError, InstanceLayout, OwnedBlock, RoutineLabels};

/// Kernel callbacks an owned blob installs. The value is the `ctx` word.
/// The kernel null-checks every other word, so latency (15) reads 0, `0x03`
/// integers (4) are dropped and the kernel does bypass (13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callback {
    /// R8 = io: `io[0]` = channel A, `io[1]` = channel B, 32 samples each.
    Process = 2,
    /// R8 = runtime index, F12 = value (`0x02` and bulk `0x1d`).
    SetParameter = 3,
    /// Not null-checked (`0xbb18fa`): a blob with state must install it.
    Destroy = 8,
    /// After the NaN guard faulted the slot (`0xbb2c71`). Runs the reset
    /// leaves with R8 = 0: clear all history.
    FaultRecovery = 14,
    /// R8 = 1 at start, for flagged slots and on `0x1f` EnableAll; the reset
    /// leaves get it. R8 = 0 is a no-op, as in stock.
    Reset = 16,
}

impl Callback {
    /// `ctx` order, which is also the stub order in section 2.
    pub const ALL: [Self; 5] = [
        Self::Process,
        Self::SetParameter,
        Self::Destroy,
        Self::FaultRecovery,
        Self::Reset,
    ];
}

/// Where one instance's sections sit: DM bytes, VISA parcels, ISA words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionAddresses {
    pub dm: u32,
    pub visa: u32,
    pub isa: u32,
}

impl SectionAddresses {
    /// The container's link bases. Relocation moves them to a placement.
    pub const LINK: Self = Self {
        dm: Section::Dm.base(),
        visa: Section::Code.base(),
        isa: Section::Isa.base(),
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleStats {
    pub dm_bytes: u32,
    pub visa_bytes: u32,
    pub isa_bytes: u32,
    /// Modeled steps of one process callback: two mono leaves or one stereo
    /// leaf, the ISA adapter and VISA stub. CPU units are unknown.
    pub cycles_per_stereo_block: u64,
    /// Static DM state needs a separate blob placement for every instance.
    pub reuse_pmco: bool,
}

/// One instance linked at fixed addresses, ready to load and run.
#[derive(Debug, Clone)]
pub struct LinkedModule {
    pub at: SectionAddresses,
    pub layout: InstanceLayout,
    pub dm: Vec<u8>,
    /// Section 2 in execution order; the entry is the first parcel.
    pub visa: Vec<u16>,
    pub isa: OwnedCode,
    callbacks: [u32; Callback::ALL.len()],
}

impl LinkedModule {
    pub fn link(block: &impl OwnedBlock, at: SectionAddresses) -> Result<Self, BlockError> {
        Ok(assemble(block, at)?.0)
    }

    /// The VISA address the entry stores in `ctx[callback]`.
    pub fn callback(&self, callback: Callback) -> u32 {
        let index = Callback::ALL
            .iter()
            .position(|&c| c == callback)
            .expect("every callback is installed");
        self.at.visa + self.callbacks[index]
    }
}

/// The plaintext owned blob for `BlobLoad`, linked at [`SectionAddresses::LINK`].
#[derive(Debug, Clone)]
pub struct BuiltModule {
    pub model_id: u32,
    pub blob: ModuleBlob,
    pub stats: ModuleStats,
    pub layout: InstanceLayout,
}

impl BuiltModule {
    pub fn build(block: &impl OwnedBlock) -> Result<Self, BlockError> {
        let (linked, relocations, process_steps) = assemble(block, SectionAddresses::LINK)?;
        // Section 3 has no relocation records. Reject words that depend on
        // where any section lies (for example an immediate DM address).
        let moved = SectionAddresses {
            dm: linked.at.dm + 0x100,
            visa: linked.at.visa + 0x100,
            isa: linked.at.isa + 0x100,
        };
        if assemble(block, moved)?.0.isa.words() != linked.isa.words()
            || linked.isa.words().iter().any(|w| absolute(w.bits()))
        {
            return Err(BlockError::PositionDependentBody);
        }
        let contents = vec![
            SectionBytes {
                section: Section::Dm,
                bytes: linked.dm,
            },
            SectionBytes {
                section: Section::Code,
                bytes: linked.visa.iter().flat_map(|p| p.to_le_bytes()).collect(),
            },
            SectionBytes {
                section: Section::Isa,
                bytes: linked.isa.isa48_le_bytes(),
            },
        ];
        let size = |index: usize| {
            u32::try_from(contents[index].bytes.len()).map_err(|_| BlockError::SizeOverflow)
        };
        // Mono leaves run once per channel state.
        let process_calls = if linked.layout.channel_b_offset_bytes.is_some() {
            2
        } else {
            1
        };
        let stats = ModuleStats {
            dm_bytes: linked.layout.dm_bytes,
            visa_bytes: size(1)?,
            isa_bytes: size(2)?,
            cycles_per_stereo_block: block
                .process_cycles()
                .checked_mul(process_calls)
                .and_then(|steps| steps.checked_add(process_steps))
                .ok_or(BlockError::SizeOverflow)?,
            reuse_pmco: false,
        };
        // VISA and ISA share one physical pool. An ISA word takes three parcels.
        if stats.isa_bytes / 2 + stats.visa_bytes / 2 > 0x1_0000 {
            return Err(BlockError::Invalid("code exceeds the shared ISA/VISA pool"));
        }
        let ranges = contents
            .iter()
            .map(|section| LinkRange::for_contents(section.section, section.bytes.len()))
            .collect::<Result<Vec<_>, _>>()?;
        let blob = ModuleBlob {
            contents,
            relocations: vec![SectionWords {
                section: Section::Code,
                words: relocations
                    .iter()
                    .flat_map(|record| record.words())
                    .collect(),
            }],
            ranges,
            entry: Section::Code.base(),
        };
        blob.validate()?;
        Ok(Self {
            model_id: block.model_id(),
            blob,
            stats,
            layout: linked.layout,
        })
    }
}

/// Type 8a absolute JUMP/CALL and Type 14 direct DM: fixed addresses that a
/// copied section 3 cannot honor.
fn absolute(word: u64) -> bool {
    let absolute_branch = word >> 40 == 0x06 && word & (1 << 40) == 0;
    absolute_branch || matches!(word >> 40, 0x10 | 0x11 | 0x1b)
}

/// The linked image, its section-2 relocation records (meaningful at
/// [`SectionAddresses::LINK`]) and the modeled steps of the process callback
/// outside the leaves.
fn assemble(
    block: &impl OwnedBlock,
    at: SectionAddresses,
) -> Result<(LinkedModule, Vec<CodeRelocation>, u64), BlockError> {
    let (layout, dm) = block.static_layout().materialize()?;
    let adapter = compile_adapter(block, layout, PmWordAddress24::new(at.isa)?)?;
    let shim = emit_shim(at, &adapter.targets)?;
    let linked = LinkedModule {
        at,
        layout,
        dm,
        visa: shim.parcels,
        isa: adapter.code,
        callbacks: shim.callbacks,
    };
    Ok((linked, shim.relocations, adapter.process_steps + STUB_STEPS))
}

const R0: DataReg = DataReg::of(0);
const R2: DataReg = DataReg::of(2);
const R4: DataReg = DataReg::of(4);
const R8: DataReg = DataReg::of(8);
const R12: DataReg = DataReg::of(12);
const I4: Dag1Index = Dag1Index::of(4);
/// The C stack pointer; push with `DM(I7, M7)`. `RFRAME` pops the frame.
const I7: Dag1Index = Dag1Index::of(7);
const M7: Dag1Modify = Dag1Modify::of(7);
/// A stub's CJUMP and two pushes, plus the executed C return (load,
/// JUMP (DB), RFRAME, NOP).
const STUB_STEPS: u64 = 7;
/// Stub parcels: CJUMP (3), push R2 (1), push literal (3), C return (6).
const STUB_RETURN: u32 = 7;

/// Section 3: the body, then one stereo adapter per callback that calls a leaf.
struct Adapter {
    code: OwnedCode,
    /// ISA addresses for the stubs of process, set-parameter, fault recovery, reset.
    targets: [(Callback, u32); 4],
    process_steps: u64,
}

fn compile_adapter(
    block: &impl OwnedBlock,
    layout: InstanceLayout,
    origin: PmWordAddress24,
) -> Result<Adapter, BlockError> {
    let mut program = IsaProgram::new(origin);
    let RoutineLabels {
        process,
        set_parameter,
        reset,
    } = block.emit_body(&mut program)?;
    let entries = [
        (Callback::Process, Label::new("stock.process")),
        (Callback::SetParameter, Label::new("stock.set_parameter")),
        (Callback::Reset, Label::new("stock.reset")),
        (Callback::FaultRecovery, Label::new("stock.fault_recovery")),
    ];
    let done = Label::new("stock.return");
    // In words (the states are whole words): the NW MODIFY scales them.
    let channel_b = layout
        .channel_b_offset_bytes
        .map(|bytes| i32::try_from(bytes / 4))
        .transpose()
        .map_err(|_| BlockError::SizeOverflow)?;

    program.define(entries[0].1.clone())?;
    emit_leaf_calls(&mut program, &process, &[R4, R8], channel_b, true)?;

    // Drop any value outside +0..=1.0, NaN and infinity included: as unsigned
    // bits these are exactly the words above 1.0 (stock does not clamp).
    program.define(entries[1].1.clone())?;
    program.push(Instruction48::Immediate {
        dst: UniversalReg::Data(R0),
        value: 1.0_f32.to_bits(),
    })?;
    program.push(Instruction48::IntegerCompareUnsigned { lhs: R12, rhs: R0 })?;
    branch(&mut program, Condition::GreaterThan, &done)?;
    emit_leaf_calls(
        &mut program,
        &set_parameter,
        &[R4, R8, R12],
        channel_b,
        false,
    )?;

    // The reset leaves get R8 = 0 from fault recovery, the kernel flag else.
    let reset_leaves = Label::new("stock.reset_leaves");
    program.define(entries[3].1.clone())?;
    program.push(Instruction48::Immediate {
        dst: UniversalReg::Data(R8),
        value: 0,
    })?;
    branch(&mut program, Condition::Always, &reset_leaves)?;
    program.define(entries[2].1.clone())?;
    program.push(Instruction48::IntegerPass { dst: R8, src: R8 })?;
    branch(&mut program, Condition::Equal, &done)?;
    program.define(reset_leaves)?;
    emit_leaf_calls(&mut program, &reset, &[R4, R8], channel_b, false)?;
    program.define(done)?;
    c_return(&mut program)?;

    let code = program.finish()?;
    let address = |label: &Label| {
        code.symbols()
            .get(label)
            .map(|address| address.get())
            .ok_or_else(|| BlockError::MissingRoutine(label.clone()))
    };
    // The straight-line process adapter ends where set-parameter begins.
    let process_steps = u64::from(address(&entries[1].1)? - address(&entries[0].1)?);
    let mut targets = [(Callback::Process, 0); 4];
    for (target, (callback, label)) in targets.iter_mut().zip(&entries) {
        *target = (*callback, address(label)?);
    }
    Ok(Adapter {
        code,
        targets,
        process_steps,
    })
}

/// The stock 48-bit C return to the stub's CJUMP literal. `RFRAME` also drops
/// any adapter pushes.
fn c_return(program: &mut IsaProgram) -> Result<(), BlockError> {
    for word in C_RETURN {
        program.push(word)?;
    }
    Ok(())
}

fn branch(
    program: &mut IsaProgram,
    condition: Condition,
    target: &Label,
) -> Result<(), BlockError> {
    program.branch_to(
        BranchKind::Jump,
        condition,
        target.clone(),
        BranchRelocation::PcRel24,
    )?;
    Ok(())
}

/// Call `leaf` once per state with R4 = I4 = that state, then the C return.
/// Straight-line, so its word count is its step count.
///
/// - One stereo state (`channel_b` = `None`): one call. R8 and R12 pass
///   through, so process gets the `io` descriptor.
/// - Channel states: A, then B at `channel_b` words. The kernel arguments in
///   `saved` are leaf scratch, so a stack frame keeps them. With `audio`,
///   channel k gets R8 = `io[k]`.
fn emit_leaf_calls(
    program: &mut IsaProgram,
    leaf: &Label,
    saved: &[DataReg],
    channel_b: Option<i32>,
    audio: bool,
) -> Result<(), BlockError> {
    let mv = |dst, src| Instruction48::RegisterMove { dst, src };
    let call = |program: &mut IsaProgram| {
        program.branch_to(
            BranchKind::Call,
            Condition::Always,
            leaf.clone(),
            BranchRelocation::PcRel24,
        )
    };
    let Some(channel_b) = channel_b else {
        program.push(mv(UniversalReg::Index(I4), UniversalReg::Data(R4)))?;
        call(program)?;
        return c_return(program);
    };
    for &register in saved {
        program.push(Instruction48::DmStore {
            src: register,
            index: I7,
            modifier: M7,
            access: DmAccess::PostModify,
        })?;
    }
    for channel in 0..2 {
        if channel == 1 {
            for (slot, &register) in saved.iter().enumerate() {
                program.push(Instruction48::DmOffsetLoad {
                    dst: register,
                    index: I7,
                    offset: (saved.len() - slot) as i32,
                })?;
            }
        }
        if audio {
            program.push(mv(UniversalReg::Index(I4), UniversalReg::Data(R8)))?;
            program.push(Instruction48::DmOffsetLoad {
                dst: R8,
                index: I4,
                offset: channel,
            })?;
        }
        program.push(mv(UniversalReg::Index(I4), UniversalReg::Data(R4)))?;
        if channel == 1 {
            program.push(Instruction48::ModifyWords {
                index: I4,
                words: channel_b,
            })?;
            program.push(mv(UniversalReg::Data(R4), UniversalReg::Index(I4)))?;
        }
        call(program)?;
    }
    c_return(program)
}

struct Shim {
    parcels: Vec<u16>,
    relocations: Vec<CodeRelocation>,
    /// Parcel offsets of the stubs, in [`Callback::ALL`] order.
    callbacks: [u32; Callback::ALL.len()],
}

/// Section 2: the entry, then one stub per callback in the stock wrapper shape
/// (ABI record §4; fortin `0x93c`): `CJUMP adapter (DB)`, push R2, push the
/// return literal (stub + 6, kind 7), then the stock C epilogue. The ISA
/// adapter returns to the epilogue with the 48-bit C return. Destroy frees
/// nothing (static state), so its stub is the epilogue alone.
fn emit_shim(at: SectionAddresses, targets: &[(Callback, u32)]) -> Result<Shim, BlockError> {
    let entry_parcels = emit_entry(at, [0; Callback::ALL.len()])?.0.len() as u32;
    let mut stubs = VisaCode::default();
    let mut callbacks = [0; Callback::ALL.len()];
    let mut relocations = Vec::new();
    for (offset, callback) in callbacks.iter_mut().zip(Callback::ALL) {
        *offset = entry_parcels + stubs.parcels().len() as u32;
        if let Some(&(_, target)) = targets.iter().find(|(c, _)| *c == callback) {
            let jump = stubs.push(VisaInstruction::CJump(PmWordAddress24::new(target)?))?;
            stubs.push(VisaInstruction::Push(R2))?;
            let back = *offset + STUB_RETURN - 1;
            let literal = stubs.push(VisaInstruction::PushImmediate(at.visa + back))?;
            relocations.extend([
                CodeRelocation {
                    kind: 11,
                    offset: entry_parcels + jump.0,
                    target_base: Section::Isa.base(),
                    target_offset: target - at.isa,
                },
                CodeRelocation {
                    kind: 7,
                    offset: entry_parcels + literal.0,
                    target_base: Section::Code.base(),
                    target_offset: back,
                },
            ]);
        }
        stubs.push(VisaInstruction::CReturn)?;
    }
    let (mut parcels, mut entry_relocations) = emit_entry(at, callbacks)?;
    parcels.extend_from_slice(stubs.parcels());
    entry_relocations.extend(relocations);
    Ok(Shim {
        parcels,
        relocations: entry_relocations,
        callbacks,
    })
}

/// R4 = `&ctx`: `I4 = R4`, then `R2 = pointer; DM(word, I4) = R2` for ctx[0]
/// (state) and each callback, then the C return. 39 parcels; the ABI record's
/// layout has 38 because it uses the 32-bit Type 5b transfer.
fn emit_entry(
    at: SectionAddresses,
    callbacks: [u32; Callback::ALL.len()],
) -> Result<(Vec<u16>, Vec<CodeRelocation>), BlockError> {
    // Type 5a (48-bit) is valid VISA; tetrad-sharc has no Type 5b encoder yet.
    let transfer = Instruction48::RegisterMove {
        dst: UniversalReg::Index(I4),
        src: UniversalReg::Data(R4),
    }
    .encode()?
    .bits();
    let mut entry = VisaCode::default();
    let lead = [
        (transfer >> 32) as u16,
        (transfer >> 16) as u16,
        transfer as u16,
    ];
    let pointers = std::iter::once((0, Section::Dm, at.dm, 0)).chain(
        Callback::ALL
            .into_iter()
            .zip(callbacks)
            .map(|(callback, offset)| (callback as u32, Section::Code, at.visa, offset)),
    );
    let mut relocations = Vec::new();
    for (word, section, base, offset) in pointers {
        let site = entry.push(VisaInstruction::Immediate {
            dst: UniversalReg::Data(R2),
            value: base + offset,
        })?;
        relocations.push(CodeRelocation {
            kind: 8,
            offset: lead.len() as u32 + site.0,
            target_base: section.base(),
            target_offset: offset,
        });
        entry.push(VisaInstruction::DmOffsetStore {
            src: R2,
            index: I4,
            offset: word as i32,
        })?;
    }
    entry.push(VisaInstruction::CReturn)?;
    let mut parcels = lead.to_vec();
    parcels.extend_from_slice(entry.parcels());
    Ok((parcels, relocations))
}
