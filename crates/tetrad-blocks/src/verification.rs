//! V2: the linked module runs on [`Machine`] under the PRM float rules and the
//! kernel's call protocol (ABI record §2–§4), against the block's Rust twin.
//! No hardware execution is claimed: `Machine` is one step per word, has no
//! pipeline, and refuses circular buffers, so the stack has L7 = 0 here.
//! Interrupts share the C stack (ABI record §4): after every step V2 writes a
//! canary over the words an interrupt would push at and below I7, so a read
//! of a value below I7 fails the twin comparison.

use std::ops::Range;
use std::slice;

use tetrad_sharc::isa48::{MODE1_CBUFEN, MODE1_IRPTEN, MODE1_NESTM, MODE1_RND32};
use tetrad_sharc::machine::{
    AddressUnit, Arithmetic, Bus, BusError, Machine, MachineError, MemoryMap, ProgramError,
    ProgramMemory, RegionError, Stop,
};

use crate::{
    BLOCK_SAMPLES, BlockError, BuiltModule, Callback, HostReference, LinkedModule, OwnedBlock,
    SectionAddresses, StaticLayout,
};

/// MODE1 at callback entry: kernel init (k1 `0x16e223..0x16e22f`) sets CBUFEN,
/// RND32, IRPTEN and NESTM; the interrupt dispatcher (k1 `0x16957b`) sets IRPTEN
/// and RND32 again. The parity enables (bits 27..29) are outside the model.
pub const KERNEL_MODE1: u32 = MODE1_CBUFEN | MODE1_RND32 | MODE1_IRPTEN | MODE1_NESTM;
/// V2 places the instance at the pool starts (roadmap §2a).
pub const V2_ADDRESSES: SectionAddresses = SectionAddresses {
    dm: 0x2e_1800,
    visa: 0x1c_0000,
    isa: 0xe_0000,
};
/// Slot 0's `ctx`: 21 words at DM `0x2c0690` (ABI §2).
const CTX: u32 = 0x2c_0690;
const CTX_WORDS: u32 = 21;
/// Channel A in pool `0x241080`; `io[1]` = A + `0x80` bytes (ABI §3).
const AUDIO: u32 = 0x24_1080;
const CHANNEL_BYTES: u32 = 4 * BLOCK_SAMPLES as u32;
const IO: u32 = 0x24_1200;
/// The kernel's initial I7 byte alias (k1 `0x16e211..0x16e220`).
const STACK_TOP: u32 = 0x24_a7f0;
const STACK_WORDS: u32 = 512;
/// A kernel VISA address after the call site. The run stops before fetching it.
const RETURN: u32 = 0x16_6072;
/// Caller I6: a canary the callee restores but never dereferences.
const CALLER_FRAME: u32 = 0x24_a800;
const STEP_BUDGET: u64 = 200_000;
/// Words an interrupt pushes at and below I7 in the V2 canary.
const INTERRUPT_WORDS: u32 = 8;
const CANARY: u32 = 0x7fc0_dead;

/// Which kernel call a finding belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Entry,
    Callback(Callback),
}

#[derive(Debug, thiserror::Error)]
pub enum VerificationError {
    #[error(transparent)]
    Block(#[from] BlockError),
    #[error(transparent)]
    Region(#[from] RegionError),
    #[error(transparent)]
    Program(#[from] ProgramError),
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error("{call:?}: {error}")]
    Machine { call: Call, error: MachineError },
    #[error("{call:?} went idle instead of returning")]
    NoReturn { call: Call },
    #[error("{call:?} left {loops} loop, {status} status and {pc} PC stack entries")]
    Stacks {
        call: Call,
        loops: usize,
        status: usize,
        pc: usize,
    },
    #[error("{call:?} changed preserved register {class}{index}")]
    Register {
        call: Call,
        class: &'static str,
        index: usize,
    },
    #[error("{call:?} raised AIS/MIS: the kernel's NaN guard would fault the slot")]
    InvalidFloat { call: Call },
    #[error("{call:?} wrote {address:#x}, outside its allowed memory")]
    Write { call: Call, address: u32 },
    #[error("after the entry ctx[{word}] = {actual:#x}, expected {expected:#x}")]
    Context {
        word: u32,
        actual: u32,
        expected: u32,
    },
    #[error(
        "frame {frame}, channel {channel}, {region} word {word}: machine={actual:#010x}, twin={expected:#010x}"
    )]
    Mismatch {
        frame: usize,
        channel: usize,
        region: &'static str,
        word: usize,
        actual: u32,
        expected: u32,
    },
    #[error("V2 needs at least 64 frames")]
    TooFewFrames,
    #[error("process took {actual} modeled steps; the module stats promise {estimated}")]
    CycleEstimate { actual: u64, estimated: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParameterUpdate {
    pub index: u32,
    /// Any bits: the adapter drops values outside +0..=1, the twin never sees them.
    pub value: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerificationFrame {
    pub samples: [[f32; BLOCK_SAMPLES]; 2],
    /// A live kernel reset (R8 = 1) first, as `0x1f` EnableAll sends on a
    /// same-layout recall.
    pub reset: bool,
    /// Delivered before this frame's process call.
    pub parameters: Vec<ParameterUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationReport {
    pub frames: usize,
    pub calls: usize,
    /// Maximum modeled steps of one process callback (both channels).
    pub maximum_process_cycles: u64,
}

/// Repeatable V2 input set: silence, impulses, full scale and sine. Callers add
/// block-specific parameter steps and decay cases. The channels differ on purpose.
pub fn standard_frames() -> Vec<VerificationFrame> {
    (0..64)
        .map(|frame| {
            let samples = std::array::from_fn(|channel| {
                let sign = if channel == 0 { 1.0 } else { -1.0 };
                std::array::from_fn(|sample| match frame % 4 {
                    0 => 0.0,
                    1 if sample == channel => sign,
                    1 => 0.0,
                    2 if (sample + channel) % 2 == 0 => 1.0,
                    2 => -1.0,
                    _ => {
                        let phase = (frame * BLOCK_SAMPLES + sample) as f32 * 0.03125;
                        phase.sin() * if channel == 0 { 0.75 } else { -0.25 }
                    }
                })
            });
            VerificationFrame {
                samples,
                reset: false,
                parameters: Vec::new(),
            }
        })
        .collect()
}

/// [`standard_frames`] repeated to `count` frames, with `(frame, index, value)`
/// updates delivered in list order before their frame's process call.
pub fn scheduled_frames(count: usize, updates: &[(usize, u32, f32)]) -> Vec<VerificationFrame> {
    let mut frames: Vec<_> = standard_frames().into_iter().cycle().take(count).collect();
    for &(frame, index, value) in updates {
        frames[frame]
            .parameters
            .push(ParameterUpdate { index, value });
    }
    frames
}

/// Runs the entry, reset (R8 = 1), every frame (optional live reset,
/// set-parameter, process), reset with R8 = 0 (no-op), fault recovery (twin
/// [`HostReference::recover`]) and destroy. The twins run once per state of
/// the layout, as the adapter calls the leaves. Each call must
/// return through the C epilogue with the CCES preserved registers, MODE1 and
/// the stacks unchanged, no AIS/MIS, and writes only to its own memory.
/// State and audio words must equal the twin's bits after every call.
pub fn verify_module(
    block: &impl HostReference,
    frames: &[VerificationFrame],
) -> Result<VerificationReport, VerificationError> {
    verify_module_at(block, frames, V2_ADDRESSES)
}

/// [`verify_module`] with the module linked at `at`, for example a real
/// `place()` result: there the VISA section follows the ISA section in the
/// shared code pool, unlike [`V2_ADDRESSES`].
pub fn verify_module_at(
    block: &impl HostReference,
    frames: &[VerificationFrame],
    at: SectionAddresses,
) -> Result<VerificationReport, VerificationError> {
    if frames.len() < 64 {
        return Err(VerificationError::TooFewFrames);
    }
    let bound = BuiltModule::build(block)?.stats.cycles_per_stereo_block;
    let module = LinkedModule::link(block, at)?;
    let mut kernel = Kernel::new(&module)?;
    let mut twins = block.static_layout();
    let state = module.at.dm;
    let dm = state..state + module.layout.dm_bytes;
    let audio = AUDIO..AUDIO + 2 * CHANNEL_BYTES;
    let context = CTX..CTX + 4 * CTX_WORDS;

    kernel.call(
        Call::Entry,
        module.at.visa,
        [CTX, 0, 0],
        slice::from_ref(&context),
    )?;
    for word in 0..CTX_WORDS {
        let expected = match Callback::ALL.into_iter().find(|&c| c as u32 == word) {
            Some(callback) => module.callback(callback),
            None if word == 0 => state,
            None => 0,
        };
        let actual = kernel.memory.map.peek(CTX + 4 * word)?;
        if actual != expected {
            return Err(VerificationError::Context {
                word,
                actual,
                expected,
            });
        }
    }
    let target = |callback: Callback| module.callback(callback);

    let reset = Call::Callback(Callback::Reset);
    kernel.call(
        reset,
        target(Callback::Reset),
        [state, 1, 0],
        slice::from_ref(&dm),
    )?;
    twins
        .states_mut()
        .iter_mut()
        .for_each(|twin| block.reset(twin));
    kernel.compare_state(&module, &twins, 0, "reset state")?;

    let mut maximum_process_cycles = 0;
    for (index, frame) in frames.iter().enumerate() {
        if frame.reset {
            kernel.call(
                reset,
                target(Callback::Reset),
                [state, 1, 0],
                slice::from_ref(&dm),
            )?;
            twins
                .states_mut()
                .iter_mut()
                .for_each(|twin| block.reset(twin));
            kernel.compare_state(&module, &twins, index, "state after a live reset")?;
        }
        for update in &frame.parameters {
            let bits = update.value.to_bits();
            let call = Call::Callback(Callback::SetParameter);
            let entry = target(Callback::SetParameter);
            kernel.call(
                call,
                entry,
                [state, update.index, bits],
                slice::from_ref(&dm),
            )?;
            if bits <= 1.0_f32.to_bits() {
                for twin in twins.states_mut() {
                    block.set_parameter(twin, update.index, update.value);
                }
            }
            kernel.compare_state(&module, &twins, index, "parameter state")?;
        }
        for (channel, samples) in frame.samples.iter().enumerate() {
            for (sample, value) in samples.iter().enumerate() {
                let address = AUDIO + channel as u32 * CHANNEL_BYTES + 4 * sample as u32;
                kernel.memory.map.write(address, value.to_bits())?;
            }
        }
        let call = Call::Callback(Callback::Process);
        let regions = [dm.clone(), audio.clone()];
        let cycles = kernel.call(call, target(Callback::Process), [state, IO, 0], &regions)?;
        if cycles > bound {
            return Err(VerificationError::CycleEstimate {
                actual: cycles,
                estimated: bound,
            });
        }
        maximum_process_cycles = maximum_process_cycles.max(cycles);
        let mut expected_samples = frame.samples;
        match &mut twins {
            StaticLayout::Channels(states) => {
                for (state, samples) in states.iter_mut().zip(&mut expected_samples) {
                    block.process(state, samples);
                }
            }
            StaticLayout::Stereo(state) => block.process_stereo(state, &mut expected_samples),
        }
        for (channel, expected) in expected_samples.into_iter().enumerate() {
            let base = AUDIO + channel as u32 * CHANNEL_BYTES;
            let actual = kernel.words(base, BLOCK_SAMPLES)?;
            let expected = expected.map(f32::to_bits);
            compare(&actual, &expected, index, channel, "audio")?;
        }
        kernel.compare_state(&module, &twins, index, "state")?;
    }

    let last = frames.len();
    kernel.call(reset, target(Callback::Reset), [state, 0, 0], &[])?;
    kernel.compare_state(&module, &twins, last, "state after reset R8 = 0")?;
    let recovery = Call::Callback(Callback::FaultRecovery);
    let garbage = 0xdead_beef;
    kernel.call(
        recovery,
        target(Callback::FaultRecovery),
        [state, garbage, garbage],
        &[dm],
    )?;
    twins
        .states_mut()
        .iter_mut()
        .for_each(|twin| block.recover(twin));
    kernel.compare_state(&module, &twins, last, "state after fault recovery")?;
    kernel.call(
        Call::Callback(Callback::Destroy),
        target(Callback::Destroy),
        [state, 0, 0],
        &[],
    )?;
    Ok(VerificationReport {
        frames: frames.len(),
        calls: kernel.calls,
        maximum_process_cycles,
    })
}

/// One word of a [`StaticLayout`] state: `channel` 0 is A (or the stereo
/// state), 1 is B.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateWord {
    pub channel: usize,
    pub word: usize,
}

/// The state words that one live reset (R8 = 1) writes on `Machine`, in write
/// order, after the entry. The frame can preempt a reset (ABI record §3), so a
/// process call can see any prefix of this list (review N1).
pub fn live_reset_writes(block: &impl OwnedBlock) -> Result<Vec<StateWord>, VerificationError> {
    let module = LinkedModule::link(block, V2_ADDRESSES)?;
    let mut kernel = Kernel::new(&module)?;
    let state = module.at.dm;
    let dm = state..state + module.layout.dm_bytes;
    let context = CTX..CTX + 4 * CTX_WORDS;
    kernel.call(
        Call::Entry,
        module.at.visa,
        [CTX, 0, 0],
        slice::from_ref(&context),
    )?;
    kernel.call(
        Call::Callback(Callback::Reset),
        module.callback(Callback::Reset),
        [state, 1, 0],
        slice::from_ref(&dm),
    )?;
    let channel_b = module.layout.channel_b_offset_bytes;
    Ok(kernel
        .memory
        .writes
        .iter()
        .filter(|address| dm.contains(address))
        .map(|address| {
            let offset = address - state;
            match channel_b {
                Some(b) if offset >= b => StateWord {
                    channel: 1,
                    word: ((offset - b) / 4) as usize,
                },
                _ => StateWord {
                    channel: 0,
                    word: (offset / 4) as usize,
                },
            }
        })
        .collect())
}

/// A bus that logs every write address.
struct LoggedMemory {
    map: MemoryMap,
    writes: Vec<u32>,
}

impl Bus for LoggedMemory {
    fn read(&mut self, address: u32) -> Result<u32, BusError> {
        self.map.peek(address)
    }
    fn write(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        self.writes.push(address);
        self.map.write(address, value)
    }
    fn write_byte(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        self.writes.push(address);
        self.map.write_byte(address, value)
    }
}

/// The modeled core plus the kernel side of the call protocol.
struct Kernel {
    machine: Machine,
    program: ProgramMemory,
    memory: LoggedMemory,
    calls: usize,
}

impl Kernel {
    fn new(module: &LinkedModule) -> Result<Self, VerificationError> {
        let mut program = ProgramMemory::default();
        program.load(&module.isa)?;
        program.load_visa(module.at.visa, &module.visa)?;
        let mut map = MemoryMap::default();
        let stack_base = STACK_TOP - 4 * (STACK_WORDS - 1);
        for (base, words) in [
            (CTX, CTX_WORDS),
            (AUDIO, 2 * BLOCK_SAMPLES as u32),
            (IO, 2),
            (stack_base, STACK_WORDS),
            (module.at.dm, module.layout.dm_bytes / 4),
        ] {
            map.add(base, AddressUnit::Byte, words as usize)?;
        }
        map.load_bytes(module.at.dm, &module.dm)?;
        map.write(IO, AUDIO)?;
        map.write(IO + 4, AUDIO + CHANNEL_BYTES)?;
        let mut machine = Machine::reset();
        machine.arithmetic = Arithmetic::Prm;
        Ok(Self {
            machine,
            program,
            memory: LoggedMemory {
                map,
                writes: Vec::new(),
            },
            calls: 0,
        })
    }

    /// One kernel call with R4, R8, R12 = `arguments`: canary registers, the C
    /// run-time constants, MODE1 at entry, then the kernel's CJUMP frame
    /// (R2 = I6, I6 = I7, push old I6 and return − 1). Returns modeled steps.
    fn call(
        &mut self,
        call: Call,
        entry: u32,
        arguments: [u32; 3],
        writable: &[Range<u32>],
    ) -> Result<u64, VerificationError> {
        let machine = &mut self.machine;
        for n in 0..16 {
            machine.r[n] = 0x5a5a_0000 | n as u32;
            machine.i[n] = 0x0002_0000 + 0x100 * n as u32;
            machine.b[n] = machine.i[n];
            machine.m[n] = 0x40 + n as i32;
        }
        machine.extension = [0; 16];
        machine.l = [0; 16];
        machine.ustat = [0x0051_0000, 0x0052_0000, 0x0053_0000, 0x0054_0000];
        for (zero, one, minus_one) in [(5, 6, 7), (13, 14, 15)] {
            (machine.m[zero], machine.m[one], machine.m[minus_one]) = (0, 1, -1);
        }
        machine.mode1 = KERNEL_MODE1;
        [machine.r[4], machine.r[8], machine.r[12]] = arguments;
        (machine.i[6], machine.i[7]) = (CALLER_FRAME, STACK_TOP);
        let before = preserved(machine);

        let frame = machine.i[7];
        self.memory.map.write(frame, machine.i[6])?;
        self.memory.map.write(frame - 4, RETURN - 1)?;
        machine.r[2] = machine.i[6];
        (machine.i[6], machine.i[7]) = (frame, frame - 8);
        machine.stkyx = 0;
        self.memory.writes.clear();
        let start = machine.cycles();
        machine.set_pc(entry);
        let stack = STACK_TOP - 4 * (STACK_WORDS - 1)..=STACK_TOP;
        loop {
            match machine.run(&self.program, &mut self.memory, Some(RETURN), 1) {
                Ok(Stop::Breakpoint(_)) => break,
                Ok(Stop::Idle) => return Err(VerificationError::NoReturn { call }),
                Err(MachineError::StepBudget(_)) if machine.cycles() - start < STEP_BUDGET => {}
                Err(error) => return Err(VerificationError::Machine { call, error }),
            }
            // Bypasses the write log: these are the interrupt's writes.
            for word in 0..INTERRUPT_WORDS {
                let address = machine.i[7].wrapping_sub(4 * word);
                if stack.contains(&address) {
                    self.memory.map.write(address, CANARY)?;
                }
            }
        }
        self.calls += 1;

        let (loops, status, pc) = (
            machine.loop_stack_depth(),
            machine.status_stack_depth(),
            machine.pc_stack_depth(),
        );
        if loops + status + pc != 0 {
            return Err(VerificationError::Stacks {
                call,
                loops,
                status,
                pc,
            });
        }
        if let Some(&(class, index, _)) = preserved(machine)
            .iter()
            .zip(&before)
            .find_map(|(after, before)| (after != before).then_some(before))
        {
            return Err(VerificationError::Register { call, class, index });
        }
        if machine.stkyx != 0 {
            return Err(VerificationError::InvalidFloat { call });
        }
        // Callee stack words lie below the CJUMP pair.
        let stack = STACK_TOP - 4 * (STACK_WORDS - 1)..frame - 4;
        if let Some(&address) = self.memory.writes.iter().find(|&&address| {
            !stack.contains(&address) && !writable.iter().any(|range| range.contains(&address))
        }) {
            return Err(VerificationError::Write { call, address });
        }
        Ok(machine.cycles() - start)
    }

    fn words(&self, base: u32, count: usize) -> Result<Vec<u32>, BusError> {
        (0..count as u32)
            .map(|word| self.memory.map.peek(base + 4 * word))
            .collect()
    }

    fn compare_state(
        &self,
        module: &LinkedModule,
        twins: &StaticLayout,
        frame: usize,
        region: &'static str,
    ) -> Result<(), VerificationError> {
        let offsets = std::iter::once(0).chain(module.layout.channel_b_offset_bytes);
        for (channel, (twin, offset)) in twins.states().iter().zip(offsets).enumerate() {
            let actual = self.words(module.at.dm + offset, twin.len())?;
            compare(&actual, twin, frame, channel, region)?;
        }
        Ok(())
    }
}

/// CCES callee-preserved and dedicated registers (compiler manual "Preserved
/// Registers", "Dedicated Registers"), data registers with all 40 bits.
/// Scratch: R0–R2, R4, R8, R12, I4, I12, I13, M4, M12, B4, B12, B13, LCNTR.
fn preserved(machine: &Machine) -> Vec<(&'static str, usize, u64)> {
    let mut view = Vec::with_capacity(80);
    for n in [3, 5, 6, 7, 9, 10, 11, 13, 14, 15] {
        let wide = u64::from(machine.r[n]) << 8 | u64::from(machine.extension[n]);
        view.push(("R", n, wide));
    }
    for n in (0..16).filter(|n| ![4, 12, 13].contains(n)) {
        view.push(("I", n, machine.i[n].into()));
        view.push(("B", n, machine.b[n].into()));
    }
    for n in (0..16).filter(|n| ![4, 12].contains(n)) {
        view.push(("M", n, u64::from(machine.m[n] as u32)));
    }
    for n in 0..16 {
        view.push(("L", n, machine.l[n].into()));
    }
    for (n, value) in machine.ustat.iter().enumerate() {
        view.push(("USTAT", n + 1, (*value).into()));
    }
    for (name, value) in [
        ("MODE1", machine.mode1),
        ("MODE2", machine.mode2),
        ("MMASK", machine.mmask),
        ("IMASK", machine.imask),
    ] {
        view.push((name, 0, value.into()));
    }
    view
}

fn compare(
    actual: &[u32],
    expected: &[u32],
    frame: usize,
    channel: usize,
    region: &'static str,
) -> Result<(), VerificationError> {
    match actual.iter().zip(expected).position(|(a, b)| a != b) {
        Some(word) => Err(VerificationError::Mismatch {
            frame,
            channel,
            region,
            word,
            actual: actual[word],
            expected: expected[word],
        }),
        None => Ok(()),
    }
}
