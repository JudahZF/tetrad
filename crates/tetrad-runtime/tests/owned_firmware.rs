//! The owned resident end to end: LDR image, boot, audio frames through the
//! simulated SPORT/DMA/SEC, and the stock ARM transport on the message rings.
use std::collections::BTreeMap;

use tetrad_dsp_host::device::{MemoryError, SharedMemory};
use tetrad_dsp_host::transport::{DspTransport, RingLayout};
use tetrad_dsp_protocol::fragment::{Fragmenter, MessageMetadata};
use tetrad_dsp_protocol::packet::ParameterChange;
use tetrad_dsp_protocol::responses::{ResponseBody, ResponseFrame};
use tetrad_dsp_protocol::{
    CodecLimits, CoreId, DeploymentTarget, DspCore, MessageType, ModuleSlot,
};
use tetrad_loader::{BlockData, Limits, RawLdr, Signature};
use tetrad_runtime::firmware::code::RingAddresses;
use tetrad_runtime::firmware::graph::{Channel, GraphSpec, KernelKind, NodeSpec, SlotId, Source};
use tetrad_runtime::firmware::image::{FirmwareError, ResidentConfig, ResidentImage};
use tetrad_runtime::firmware::layout::{control, node};
use tetrad_runtime::firmware::memory::{MESSAGE_WINDOW, MESSAGE_WINDOW_BYTES, SharcCore};
use tetrad_runtime::firmware::sim::{SimulatedSoc, TdmBlock};
use tetrad_sharc::ldr::LDR_LITERAL_MAX;
use tetrad_sharc::machine::{
    Bus, InterruptBit, Machine, MachineError, ProgramMemory, ROM_IVT_BASE, SYSCTL_IIVT, Stop,
};

const RING: CoreId = CoreId::Dsp(DspCore::Core0);
const BUDGET: u64 = 200_000;
/// Module id slot 5 answers to in BulkStartModel.
const SLOT5_MODULE: u32 = 22000;
/// The SHARC ring index word: `[out producer, out consumer, in producer, in consumer]`.
const INDICES: u32 = MESSAGE_WINDOW + 0x784;

fn slot(n: u8) -> SlotId {
    SlotId::new(n).unwrap()
}
fn channel(n: u8) -> Channel {
    Channel::new(n).unwrap()
}

/// ch0 -> gain(5) -> ch2; ch1 -> gain(6); mix(7) = gain(5) + gain(6) -> ch3.
fn config() -> ResidentConfig {
    ResidentConfig {
        core: SharcCore::Core1,
        ring: RING,
        graph: graph(),
        module_ids: BTreeMap::from([(slot(5), SLOT5_MODULE), (slot(6), 22001)]),
    }
}

fn graph() -> GraphSpec {
    GraphSpec {
        nodes: vec![
            NodeSpec {
                slot: slot(5),
                kernel: KernelKind::Gain,
                inputs: vec![Source::Input(channel(0))],
            },
            NodeSpec {
                slot: slot(6),
                kernel: KernelKind::Gain,
                inputs: vec![Source::Input(channel(1))],
            },
            NodeSpec {
                slot: slot(7),
                kernel: KernelKind::Mix,
                inputs: vec![Source::Node(slot(5)), Source::Node(slot(6))],
            },
        ],
        outputs: vec![
            (channel(2), Source::Node(slot(5))),
            (channel(3), Source::Node(slot(7))),
        ],
    }
}

/// The ARM's view of the message window. Single-threaded: barriers are no-ops.
struct ArmWindow<'a>(&'a mut SimulatedSoc);

impl ArmWindow<'_> {
    fn check(offset: usize, len: usize) -> Result<u32, MemoryError> {
        if offset + len > MESSAGE_WINDOW_BYTES as usize {
            return Err(MemoryError::OutOfBounds {
                offset,
                len,
                window: MESSAGE_WINDOW_BYTES as usize,
            });
        }
        Ok(MESSAGE_WINDOW + offset as u32)
    }
}

impl SharedMemory for ArmWindow<'_> {
    fn len(&self) -> usize {
        MESSAGE_WINDOW_BYTES as usize
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        let start = Self::check(offset, buf.len())?;
        buf.copy_from_slice(&self.0.memory().read_bytes(start, buf.len()).unwrap());
        Ok(())
    }
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let start = Self::check(offset, data.len())?;
        self.0.memory_mut().load_bytes(start, data).unwrap();
        Ok(())
    }
    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        let mut bytes = [0; 4];
        self.read(offset, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        self.write(offset, &value.to_le_bytes())
    }
    fn acquire_barrier(&mut self) {}
    fn publish_barrier(&mut self) {}
}

struct Rig {
    image: ResidentImage,
    soc: SimulatedSoc,
    program: ProgramMemory,
    machine: Machine,
}

impl Rig {
    fn boot() -> Self {
        Self::boot_with(|_| {})
    }
    /// Boot after `prepare` sets up the SoC (for example stale window bytes).
    fn boot_with(prepare: impl FnOnce(&mut SimulatedSoc)) -> Self {
        let image = ResidentImage::build(&config()).unwrap();
        let mut soc = SimulatedSoc::new(SharcCore::Core1);
        prepare(&mut soc);
        let mut program = ProgramMemory::default();
        image.install(&mut program, &mut soc).unwrap();
        let mut machine = Machine::reset();
        machine.set_pc(image.entry().get());
        soc.start();
        assert_eq!(
            machine.run(&program, &mut soc, None, BUDGET),
            Ok(Stop::Idle)
        );
        Self {
            image,
            soc,
            program,
            machine,
        }
    }
    fn frame(&mut self, input: &TdmBlock) -> TdmBlock {
        let output = self.soc.transfer_frame(input).unwrap();
        assert!(self.soc.interrupt_pending());
        self.machine.raise(InterruptBit::SECI);
        assert_eq!(
            self.machine.run(&self.program, &mut self.soc, None, BUDGET),
            Ok(Stop::Idle)
        );
        output
    }
    fn control(&self, word: u32) -> u32 {
        self.soc
            .memory()
            .peek(self.image.layout().control + word)
            .unwrap()
    }
    fn node(&self, slot: u8, word: u32) -> u32 {
        let record = self
            .soc
            .memory()
            .peek(self.image.layout().slot_index + u32::from(slot))
            .unwrap();
        self.soc.memory().peek(record + word).unwrap()
    }
    /// Sets `node::STARTED` in `slot`'s record, as an owned kernel does after its reset.
    fn mark_started(&mut self, slot: u8) {
        let record = self
            .soc
            .memory()
            .peek(self.image.layout().slot_index + u32::from(slot))
            .unwrap();
        let flags = self.node(slot, node::FLAGS) | node::STARTED;
        self.soc
            .memory_mut()
            .write(record + node::FLAGS, flags)
            .unwrap();
    }
    fn arm(&mut self) -> DspTransport<ArmWindow<'_>> {
        DspTransport::new(ArmWindow(&mut self.soc)).unwrap()
    }
    fn send(&mut self, message: MessageType, selector: u8, payload: &[u8]) {
        let metadata = MessageMetadata {
            message_type: message as u8,
            core: RING,
            slot: selector,
            argument: 0,
        };
        let mut fragments = Fragmenter::new(metadata, payload, CodecLimits::default()).unwrap();
        self.arm()
            .try_send_fragment(fragments.next().unwrap())
            .unwrap();
    }
    /// Stock publication order for bulk layouts (`+12` is a record count, so
    /// generic framing does not apply): payload, length, then producer byte.
    fn publish_raw(&mut self, bytes: &[u8]) {
        let layout = RingLayout::outgoing(RING);
        let mut arm = ArmWindow(&mut self.soc);
        let mut index = [0];
        arm.read(layout.producer_offset, &mut index).unwrap();
        let slot = (usize::from(index[0]) + 1) % usize::from(layout.slot_count);
        arm.write(layout.payload_offset + slot * layout.slot_bytes, bytes)
            .unwrap();
        arm.write_u32(layout.length_offset + 4 * slot, bytes.len() as u32)
            .unwrap();
        arm.write(layout.producer_offset, &[slot as u8]).unwrap();
    }
    fn receive(&mut self) -> Option<Vec<u8>> {
        self.arm().try_receive(RING).unwrap()
    }
    fn indices(&self) -> Vec<u8> {
        self.soc.memory().read_bytes(INDICES, 4).unwrap()
    }
}

/// Channel 0 carries `a`, channel 1 carries `b`, everything else is zero.
fn block(a: i32, b: i32) -> TdmBlock {
    let mut block = [[0; 8]; 32];
    for (frame, row) in block.iter_mut().enumerate() {
        row[0] = a * (frame as i32 + 1);
        row[1] = b * (frame as i32 + 1);
    }
    block
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

#[test]
fn sharc_ring_addresses_match_the_arm_transport_layout() {
    for ring in [
        CoreId::Dsp(DspCore::Core0),
        CoreId::Dsp(DspCore::Core1),
        CoreId::Dsp(DspCore::Core2),
        CoreId::Dsp(DspCore::Core3),
        CoreId::Control,
    ] {
        let (out, incoming) = (RingLayout::outgoing(ring), RingLayout::incoming(ring));
        let at = |offset: usize| MESSAGE_WINDOW + offset as u32;
        let sharc = RingAddresses::new(ring);
        assert_eq!(sharc.indices, at(out.producer_offset), "{ring:?}");
        assert_eq!(
            [
                out.consumer_offset,
                incoming.producer_offset,
                incoming.consumer_offset
            ],
            [1, 2, 3].map(|lane| out.producer_offset + lane)
        );
        assert_eq!(
            (sharc.out_payload, sharc.out_lengths),
            (at(out.payload_offset), at(out.length_offset))
        );
        assert_eq!(
            (sharc.out_slots, sharc.out_slot_bytes),
            (u32::from(out.slot_count), out.slot_bytes as u32)
        );
        assert_eq!(
            (sharc.in_payload, sharc.in_lengths),
            (at(incoming.payload_offset), at(incoming.length_offset))
        );
        assert_eq!(
            (sharc.in_slots, sharc.in_slot_bytes),
            (u32::from(incoming.slot_count), incoming.slot_bytes as u32)
        );
    }
}

#[test]
fn image_round_trips_through_tetrad_loader_and_boot_brings_up_audio() {
    let rig = Rig::boot();
    let ldr = rig.image.ldr().unwrap();
    let parsed = RawLdr::parse(ldr.bytes(), Limits::default()).unwrap();
    parsed.validate_signature(Signature::Core1).unwrap();
    let blocks = parsed.blocks();
    let flags: Vec<u16> = blocks.iter().map(|b| b.header.flags()).collect();
    let last = blocks.len() - 1;
    assert_eq!(
        (flags[0], flags[last - 1], flags[last]),
        (0x5001, 0x0101, 0x8001)
    );
    assert!(flags[1..last - 1].iter().all(|flag| *flag == 0x0001));
    assert_eq!(blocks[0].header.target_address(), rig.image.entry().get());
    assert_eq!(blocks[0].header.argument() as usize, blocks[last].offset);
    // Literals: at most the stock 0x800 bytes, contiguous per region. Code
    // starts at the stock vector aperture as 6-byte LE ISA48 words.
    let mut code = Vec::new();
    let mut data_target = None;
    for block in &blocks[1..last - 1] {
        let BlockData::Literal(bytes) = block.data else {
            panic!("literal")
        };
        assert!(bytes.len() <= LDR_LITERAL_MAX);
        let target = block.header.target_address();
        if target < 0x282c_0000 {
            assert_eq!(target, 0x2824_0000 + code.len() as u32);
            code.extend_from_slice(bytes);
        } else {
            data_target.get_or_insert(target);
        }
    }
    let expected = rig.image.code().isa48_le_bytes();
    assert_eq!(&code[..expected.len()], expected);
    assert_eq!(data_target, Some(0x282c_0000));

    // Boot ran the bring-up plan: SEC on, SID 60 to core 1, DMA then SPORT enabled.
    let writes = rig.soc.mmio_writes();
    assert_eq!(writes[0], (0x3108_9000, 1));
    assert!(writes.contains(&(0x3108_99e0, 0x0100_0005)));
    let rx_dma = writes.iter().position(|w| *w == (0x3102_4008, 0x0051_4223));
    let rx_sport = writes.iter().position(|w| *w == (0x3100_2400, 0x0008_91f3));
    assert!(rx_dma.unwrap() < rx_sport.unwrap());
    assert!(rig.machine.is_idle());
}

#[test]
fn frames_run_the_graph_with_two_frame_latency_and_cpu_accounting() {
    let mut rig = Rig::boot();
    let (a, b) = (1 << 20, -(1 << 18));
    for _ in 0..2 {
        assert_eq!(rig.frame(&block(a, b)), [[0; 8]; 32]);
    }
    let output = rig.frame(&block(0, 0));
    for (frame, row) in output.iter().enumerate() {
        let k = frame as i32 + 1;
        assert_eq!(row[2], a * k, "unity gain, frame {frame}");
        assert_eq!(row[3], (a + b) * k, "mix, frame {frame}");
        assert_eq!([row[0], row[1], row[4], row[7]], [0; 4]);
    }
    assert_eq!(rig.control(control::FRAME_COUNT), 3);
    let cycles = rig.control(control::CYCLES_LAST);
    // Guard: one 32-frame block at 450 MHz / 1.5 kHz leaves 300k cycles.
    assert!(cycles > 0 && cycles < 300_000, "{cycles}");
    assert!(rig.control(control::CYCLES_PEAK) >= cycles);
    assert!(rig.node(7, node::CYCLES) > 0);
}

#[test]
fn stock_arm_transport_drives_parameters_bypass_meters_and_diagnostics() {
    let mut rig = Rig::boot();
    let target = DeploymentTarget {
        core: DspCore::Core0,
        slot: ModuleSlot::try_from(5).unwrap(),
    };
    let half = ParameterChange::from_template([0; 20], target, 0, 0.5);
    rig.arm().send_parameter(&half).unwrap();
    rig.send(MessageType::ModelBypass, 6, &1.0f32.to_le_bytes());
    rig.send(MessageType::ModelBypass, 7, &1.0f32.to_le_bytes());
    rig.send(MessageType::ModelBypass, 5, &3.0f32.to_le_bytes());
    rig.frame(&block(0, 0));
    assert_eq!(rig.control(control::MESSAGES), 4);
    // Mix has two inputs and 3.0 has no stock meaning: both refused.
    assert_eq!(rig.control(control::REJECTED), 2);
    assert_eq!(rig.node(6, node::FLAGS) & node::BYPASS, node::BYPASS);

    let (a, b) = (1 << 22, 1 << 21);
    rig.frame(&block(a, b));
    rig.frame(&block(0, 0));
    let output = rig.frame(&block(0, 0));
    assert_eq!(output[0][2], a / 2);
    assert_eq!(output[0][3], a / 2 + b);

    rig.send(MessageType::MeterRequest, 5, &[]);
    rig.send(MessageType::DiagnosticsRequest, 0, &[]);
    // A boot sync message without an owned handler is consumed and counted.
    rig.send(MessageType::DdrTestA, 0, &[]);
    rig.frame(&block(0, 0));
    assert_eq!(rig.control(control::UNSUPPORTED), 1);
    assert_eq!(
        rig.control(control::LAST_UNSUPPORTED),
        MessageType::DdrTestA as u32
    );
    assert_eq!(rig.soc.doorbells(), 2);

    let meter = rig.receive().unwrap();
    let ResponseBody::BulkMeter(meter) = *ResponseFrame::decode(&meter).unwrap().body() else {
        panic!("bulk meter")
    };
    assert_eq!((meter.first, meter.count), (5, 16));
    // Peak of slot 5 since frame one: (a/2 * 32) / 2^31.
    let peak = f32::from_bits(meter.values()[0]);
    assert_eq!(peak, (a / 2 * 32) as f32 / 2f32.powi(31));
    assert_eq!(rig.node(5, node::PEAK), 0, "reset on read");

    let diagnostics = rig.receive().unwrap();
    let frame = ResponseFrame::decode(&diagnostics).unwrap();
    let ResponseBody::Diagnostics(diagnostics) = *frame.body() else {
        panic!("diagnostics")
    };
    assert_eq!(
        (diagnostics.fpex_count(), diagnostics.error_flags()),
        (0, 0)
    );
    assert_eq!(diagnostics.words[15], 5, "owned frame-count extension word");
    assert!(rig.receive().is_none());
    assert_eq!(rig.control(control::MESSAGES), 7);

    // 2.0 is bypass with mute (stock k1 `0x1659b0`): slot 6 writes silence.
    rig.send(MessageType::ModelBypass, 6, &2.0f32.to_le_bytes());
    rig.frame(&block(0, 0));
    let muted = node::BYPASS | node::MUTE;
    assert_eq!(rig.node(6, node::FLAGS) & muted, muted);
    rig.frame(&block(a, b));
    rig.frame(&block(0, 0));
    let output = rig.frame(&block(0, 0));
    assert_eq!((output[0][2], output[0][3]), (a / 2, a / 2));
}

#[test]
fn bulk_messages_follow_the_recovered_record_layouts() {
    let mut rig = Rig::boot();
    let record = |words: [u32; 3]| {
        words
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<_>>()
    };
    let header = |kind: MessageType, count: u32, bytes: usize| {
        let mut header = vec![kind as u8, 0, 0, 0, 8, 0, 0, 0];
        header.extend((bytes as u32).to_le_bytes());
        header.extend(count.to_le_bytes());
        header
    };
    // Slot 40 is invalid (rejected); slot 6 gain = 0.25.
    let mut bulk = header(MessageType::BulkParameterChange, 2, 40);
    bulk.extend(record([40, 0, 0.5f32.to_bits()]));
    bulk.extend(record([6, 0, 0.25f32.to_bits()]));
    rig.publish_raw(&bulk);
    // F7: DeleteAllModels, then BulkStartModel each clear an owned kernel's
    // started flag, so a restart resets it and resends its parameters.
    for slot in [5, 6] {
        rig.mark_started(slot);
    }
    rig.publish_raw(&header(MessageType::DeleteAllModels, 0, 16));
    rig.frame(&block(0, 0));
    for slot in [5, 6] {
        assert_eq!(rig.node(slot, node::FLAGS) & node::STARTED, 0);
    }
    rig.mark_started(5);
    // {module id, slot | aux << 16, extra}: slot 5 with its id starts; slot 6
    // with another id and slot 9 (no node) fail.
    let mut start = header(MessageType::BulkStartModel, 3, 52);
    start.extend(record([SLOT5_MODULE, 5 | (1 << 16), u32::MAX]));
    start.extend(record([SLOT5_MODULE, 6, u32::MAX]));
    start.extend(record([22002, 9, u32::MAX]));
    rig.publish_raw(&start);
    rig.frame(&block(0, 0));
    assert_eq!(
        rig.node(5, node::FLAGS) & (node::ACTIVE | node::STARTED),
        node::ACTIVE
    );
    assert_eq!(rig.control(control::REJECTED), 1);
    let parameters = rig.image.layout().slot_parameters(6);
    assert_eq!(
        rig.soc.memory().peek(parameters).unwrap(),
        0.25f32.to_bits()
    );

    let reply = rig.receive().unwrap();
    let ResponseBody::BulkModelStart(started) = *ResponseFrame::decode(&reply).unwrap().body()
    else {
        panic!("bulk start")
    };
    assert_eq!(
        (started.core, started.started_mask, started.failed_mask),
        (0, 1 << 5, (1 << 6) | (1 << 9))
    );
    // Only slot 5 is active again: slots 6 and 7 output silence.
    let (a, b) = (1 << 20, 1 << 20);
    rig.frame(&block(a, b));
    rig.frame(&block(0, 0));
    let output = rig.frame(&block(0, 0));
    assert_eq!((output[0][2], output[0][3]), (a, 0));
    assert_eq!(
        words(
            &rig.soc
                .memory()
                .read_bytes(MESSAGE_WINDOW + 0x784, 4)
                .unwrap()
        )[0] >> 8
            & 0xff,
        3,
        "consumer byte names the last consumed slot"
    );
}

/// Review F3/F4: the SEC handler is reached only through the L1 vector table
/// that `init` selects with SYSCTL.IIVT; fault sources are unmasked and record
/// their vector.
#[test]
fn vectors_need_iivt_and_faults_are_recorded() {
    let mut rig = Rig::boot();
    assert_eq!(rig.machine.sysctl & SYSCTL_IIVT, SYSCTL_IIVT);
    let mut rom = rig.machine.clone();
    rom.sysctl = 0;
    rom.raise(InterruptBit::SECI);
    assert_eq!(
        rom.run(&rig.program, &mut rig.soc, None, 10),
        Err(MachineError::NoCode(ROM_IVT_BASE + 4 * 15))
    );
    // ILOPI (vector 4): an illegal-opcode probe failure becomes visible.
    rig.machine.raise(InterruptBit::new(4).unwrap());
    assert_eq!(
        rig.machine.run(&rig.program, &mut rig.soc, None, 100),
        Ok(Stop::Idle)
    );
    assert_eq!(rig.control(control::FAULT), 4);
}

/// Review F1: replies use word offsets on the byte-space slot pointer, so a
/// reply stays inside its 144-byte slot, also from the last slot of the ring
/// (the next bytes are the following ring's region).
#[test]
fn replies_never_write_past_their_slot() {
    let mut rig = Rig::boot();
    let incoming = RingLayout::incoming(RING);
    let base = MESSAGE_WINDOW + incoming.payload_offset as u32;
    let span = (usize::from(incoming.slot_count) + 1) * incoming.slot_bytes;
    for _ in 0..=incoming.slot_count {
        rig.soc
            .memory_mut()
            .load_bytes(base, &vec![0xa5; span])
            .unwrap();
        rig.send(MessageType::MeterRequest, 0, &[]);
        rig.frame(&block(0, 0));
        let slot = usize::from(rig.indices()[2]);
        let reply = rig.receive().unwrap();
        let start = slot * incoming.slot_bytes;
        let window = rig.soc.memory().read_bytes(base, span).unwrap();
        assert_eq!(&window[start..start + reply.len()], &reply[..]);
        for (offset, byte) in window.iter().enumerate() {
            if !(start..start + reply.len()).contains(&offset) {
                assert_eq!(*byte, 0xa5, "slot {slot} wrote byte {offset}");
            }
        }
    }
}

/// Review F2: outputs at and above full scale clip to 2^31 - 128 instead of
/// the all-ones `FIX` overflow result.
#[test]
fn outputs_clip_at_full_scale() {
    let mut rig = Rig::boot();
    let target = DeploymentTarget {
        core: DspCore::Core0,
        slot: ModuleSlot::try_from(5).unwrap(),
    };
    let gain = ParameterChange::from_template([0; 20], target, 0, 4.0);
    rig.arm().send_parameter(&gain).unwrap();
    rig.frame(&block(0, 0));
    let mut input = [[0; 8]; 32];
    let samples = [1 << 29, 1 << 30, -(1 << 30), -(1 << 29), 1 << 20];
    for (row, sample) in input.iter_mut().zip(samples) {
        row[0] = sample;
    }
    rig.frame(&input);
    rig.frame(&block(0, 0));
    let output = rig.frame(&block(0, 0));
    let column: Vec<i32> = output.iter().take(5).map(|row| row[2]).collect();
    let full = 0x7fff_ff80;
    assert_eq!(column, [full, full, -full, -full, 1 << 22]);
}

/// Review F8: boot starts both rings empty from the ARM-owned index bytes, so
/// stale slots are not replayed and replies go to the next free slot.
#[test]
fn boot_seeds_ring_indices_from_the_arm_bytes() {
    let mut rig = Rig::boot_with(|soc| {
        soc.memory_mut().load_bytes(INDICES, &[5, 0, 0, 9]).unwrap();
        // A stale request the old index state would have replayed.
        let lengths = MESSAGE_WINDOW + RingLayout::outgoing(RING).length_offset as u32;
        soc.memory_mut()
            .load_bytes(lengths + 4, &16u32.to_le_bytes())
            .unwrap();
    });
    assert_eq!(rig.indices(), [5, 5, 9, 9]);
    rig.send(MessageType::MeterRequest, 0, &[]);
    rig.frame(&block(0, 0));
    assert_eq!(rig.control(control::MESSAGES), 1);
    assert!(rig.receive().is_some());
    assert_eq!(rig.indices(), [6, 6, 10, 10]);
}

/// Review F9: a resident only serves a ring the ARM drains on its doorbell.
#[test]
fn ring_must_match_the_core_doorbell() {
    let build = |core, ring| {
        ResidentImage::build(&ResidentConfig {
            core,
            ring,
            ..config()
        })
    };
    assert!(matches!(
        build(SharcCore::Core1, CoreId::Dsp(DspCore::Core1)),
        Err(FirmwareError::RingPairing {
            core: SharcCore::Core1,
            ring: CoreId::Dsp(DspCore::Core1)
        })
    ));
    assert!(build(SharcCore::Core2, CoreId::Dsp(DspCore::Core1)).is_ok());
    assert!(build(SharcCore::Core2, CoreId::Control).is_err());
}
