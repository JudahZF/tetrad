//! Response dispatch, diagnostics reduction and the send crash policy recovered
//! from ZenUI `DSPCommsARM` (dispatchers VAs `0x89d858` / `0x89cc60`, drain
//! `0x89ece0`, `sendMessage` `0x89c6fc`). The caller feeds raw incoming ring
//! payloads (for example from `DspTransport::try_receive`) and reports the
//! outcome of each physical send. Nothing here sleeps, polls or writes memory.

use std::fmt;

use tetrad_dsp_protocol::responses::{
    BufferResponse, BulkMeter, DebugText, DiagnosticsBlock, Dispatcher, HidReport, LooperStatus,
    MeterRecord, MetronomeStatus, MidiClockTempo, MidiRecord, ModuleLatency, OpaqueCalibration,
    ResponseBody, ResponseFrame, ResponseType, SelfTest, SlotStart, Soc2ArmDiagnostics, UsbMeter,
};
use tetrad_dsp_protocol::{CoreId, DspCore, Error as ProtocolError, MessageType};

/// Fourth consecutive send timeout on a core marks it dead (`0x89c6fc`).
pub const CRASH_TIMEOUT_COUNT: u8 = 4;
/// After an FPEx warning, the next five changed samples are swallowed (`0x89ece0`).
pub const FPEX_WARNING_HOLDOFF: u8 = 5;
/// `MidiClockCountAndTempo` always resets the timeout run, even after a failed send.
pub const ALWAYS_ACCEPTED_TYPE: u8 = MessageType::MidiClockCountAndTempo as u8;

/// Caller-observed result of one physical send. `TimedOut` is only the stock
/// failure (VA `0x54e0ac` returned 0): no free slot appeared within the caller's
/// timeout window, or a synchronous wait expired. An immediate nonblocking
/// `RingFull` without that wait is not a timeout and must not be recorded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Published,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendDisposition {
    Accepted,
    /// `first_in_run` mirrors the single stock timeout log per run.
    Timeout {
        consecutive: u8,
        first_in_run: bool,
    },
    /// Core is now dead; stock also dumps SEC registers and emits `dspCrashSignal`.
    Crashed,
    /// Stock returns failure before writing anything to a dead core.
    RefusedDead,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LinkState {
    pub consecutive_timeouts: u8,
    pub timeout_logged: bool,
    pub dead: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FpexTracker {
    stored: u32,
    holdoff: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashCause {
    /// Type `0x04` with status 0 on SOC2, or a failed bit in a type `0x0c` mask.
    ModuleStartFailure {
        core: u32,
        slot: u32,
    },
    SendTimeouts {
        core: CoreId,
    },
    /// No run-list-cleared acknowledgement after `DeleteAllModels`
    /// (VA 0x515890 calls the crash handler 0x89c680, which marks the core dead).
    RunListTimeout {
        core: DspCore,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    UnknownType,
    NotHandledByDispatcher,
    /// Type `0x0d` on SOC2 with a selector other than 2.
    SelectorMismatch,
    /// The body did not decode. Stock dispatchers (VAs 0x89d858, 0x89cc60)
    /// have no malformed state; the session ignores such a message.
    Malformed,
}

/// One drained stock signal or log line. Opaque payloads keep hidden `Debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Diagnostics {
        core: DspCore,
        block: DiagnosticsBlock,
    },
    ErrorFlagsChanged {
        core: DspCore,
        flags: u32,
    },
    FloatingPointException {
        core: DspCore,
        count: i32,
        slot: u32,
    },
    SelfTest {
        core: DspCore,
        result: SelfTest,
    },
    Soc2ArmDiagnostics(Soc2ArmDiagnostics),
    Meter(MeterRecord),
    BulkMeter(BulkMeter),
    LooperStatus(LooperStatus),
    MetronomeStatus(MetronomeStatus),
    ModelStart {
        core: u32,
        slot: u32,
        started: bool,
    },
    ModuleStartFailed {
        core: u32,
        slot: u32,
    },
    Crash(CrashCause),
    /// Unified allocation observation: SOC1 logs and emits bufferResponseSignal;
    /// SOC2 only logs. This variant does not claim identical stock signals.
    BufferAllocated(BufferResponse),
    ModuleLatency(ModuleLatency),
    DebugText {
        soc: Dispatcher,
        core_selector: u8,
        text: DebugText,
    },
    MidiClockTempo(MidiClockTempo),
    MidiReceived {
        record: MidiRecord,
        response_type: ResponseType,
    },
    HidReport(HidReport),
    UsbMeterUpdate(UsbMeter),
    UsbMeterInput {
        first: UsbMeter,
        second: UsbMeter,
    },
    UsbMeterOutput(UsbMeter),
    TunerCalibrate(OpaqueCalibration),
    /// Licence channel `0x20`; stock zeroes it after use (`memset` at `0x1edf70`).
    TunerTimer(KeyMaterial),
    /// Licence channel `0x21`.
    MidiOutTimer(KeyMaterial),
    StageOneComplete(u32),
    StageTwoComplete(u32, u32),
    SlaveBootBufferAddress(Vec<u8>),
    SlaveBootResponse(u32),
    Soc2SoundsReceived,
    /// Stock re-applies cached settings here; that follow-up is not modelled.
    Flag0f,
    Word1c(u32),
    /// ModelRepo key (stock controller `+0xf018`).
    Vector24(KeyMaterial),
    /// Blob key (stock controller `+0xf024`); feeds `model::BlobKey`.
    Vector25(KeyMaterial),
    Vector27(KeyMaterial),
    Ignored {
        response_type: u8,
        reason: IgnoreReason,
    },
}

impl Event {
    /// The variant name, for logs that must not print a payload.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Diagnostics { .. } => "Diagnostics",
            Self::ErrorFlagsChanged { .. } => "ErrorFlagsChanged",
            Self::FloatingPointException { .. } => "FloatingPointException",
            Self::SelfTest { .. } => "SelfTest",
            Self::Soc2ArmDiagnostics(_) => "Soc2ArmDiagnostics",
            Self::Meter(_) => "Meter",
            Self::BulkMeter(_) => "BulkMeter",
            Self::LooperStatus(_) => "LooperStatus",
            Self::MetronomeStatus(_) => "MetronomeStatus",
            Self::ModelStart { .. } => "ModelStart",
            Self::ModuleStartFailed { .. } => "ModuleStartFailed",
            Self::Crash(_) => "Crash",
            Self::BufferAllocated(_) => "BufferAllocated",
            Self::ModuleLatency(_) => "ModuleLatency",
            Self::DebugText { .. } => "DebugText",
            Self::MidiClockTempo(_) => "MidiClockTempo",
            Self::MidiReceived { .. } => "MidiReceived",
            Self::HidReport(_) => "HidReport",
            Self::UsbMeterUpdate(_) => "UsbMeterUpdate",
            Self::UsbMeterInput { .. } => "UsbMeterInput",
            Self::UsbMeterOutput(_) => "UsbMeterOutput",
            Self::TunerCalibrate(_) => "TunerCalibrate",
            Self::TunerTimer(_) => "TunerTimer",
            Self::MidiOutTimer(_) => "MidiOutTimer",
            Self::StageOneComplete(_) => "StageOneComplete",
            Self::StageTwoComplete(..) => "StageTwoComplete",
            Self::SlaveBootBufferAddress(_) => "SlaveBootBufferAddress",
            Self::SlaveBootResponse(_) => "SlaveBootResponse",
            Self::Soc2SoundsReceived => "Soc2SoundsReceived",
            Self::Flag0f => "Flag0f",
            Self::Word1c(_) => "Word1c",
            Self::Vector24(_) => "Vector24",
            Self::Vector25(_) => "Vector25",
            Self::Vector27(_) => "Vector27",
            Self::Ignored { .. } => "Ignored",
        }
    }
}

/// Owned secret bytes: boot-vector keys and the licence channels. `Debug` shows only the
/// length; never log the bytes. Every copy is zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyMaterial(Vec<u8>);

impl KeyMaterial {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for KeyMaterial {
    fn drop(&mut self) {
        self.0.fill(0);
        // Keeps the stores from being removed as dead before the free.
        std::hint::black_box(&self.0);
    }
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyMaterial({} bytes)", self.0.len())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReceiveError {
    #[error("malformed DSP response: {0}")]
    Protocol(#[from] ProtocolError),
}

/// Per-link crash policy plus the diagnostics comparators the drain keeps.
/// Construct one per comms session; stock never revives a dead core in place.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommsState {
    links: [LinkState; 5],
    diagnostics: [Option<DiagnosticsBlock>; 4],
    fpex: [FpexTracker; 4],
    error_flags: [u32; 4],
}

impl CommsState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn link(&self, core: CoreId) -> LinkState {
        self.links[usize::from(core.wire())]
    }

    /// Check before writing: stock refuses dead cores without touching the ring.
    pub fn can_send(&self, core: CoreId) -> bool {
        !self.link(core).dead
    }

    /// Boot-sequencer hook (`handleSlaveBootResponse`, VA `0x89c0bc`, marks cores 2..4).
    pub fn mark_dead(&mut self, core: CoreId) {
        self.links[usize::from(core.wire())].dead = true;
    }

    /// Exact `0x89c6fc` policy over a caller-supplied outcome.
    pub fn record_send(
        &mut self,
        core: CoreId,
        message_type: u8,
        outcome: SendOutcome,
    ) -> SendDisposition {
        let link = &mut self.links[usize::from(core.wire())];
        if link.dead {
            return SendDisposition::RefusedDead;
        }
        if outcome == SendOutcome::Published || message_type == ALWAYS_ACCEPTED_TYPE {
            link.consecutive_timeouts = 0;
            link.timeout_logged = false;
            return SendDisposition::Accepted;
        }
        let first_in_run = !link.timeout_logged;
        link.timeout_logged = true;
        link.consecutive_timeouts += 1;
        if link.consecutive_timeouts == CRASH_TIMEOUT_COUNT {
            link.dead = true;
            SendDisposition::Crashed
        } else {
            SendDisposition::Timeout {
                consecutive: link.consecutive_timeouts,
                first_in_run,
            }
        }
    }

    /// Dispatches one raw incoming payload from `ring` and drains it immediately.
    /// A malformed known type returns an error and changes no state.
    pub fn receive(&mut self, ring: CoreId, bytes: &[u8]) -> Result<Vec<Event>, ReceiveError> {
        let frame = ResponseFrame::decode(bytes)?;
        let dispatcher = Dispatcher::for_ring(ring);
        let header = frame.header();
        let Some(kind) = frame.response_type() else {
            return Ok(vec![Event::Ignored {
                response_type: header.response_type,
                reason: IgnoreReason::UnknownType,
            }]);
        };
        if !kind.is_handled_by(dispatcher) {
            return Ok(vec![Event::Ignored {
                response_type: kind as u8,
                reason: IgnoreReason::NotHandledByDispatcher,
            }]);
        }
        let mut events = Vec::new();
        match frame.into_body() {
            ResponseBody::Diagnostics(block) => {
                let core = dispatcher.diagnostics_core(header.selector);
                self.reduce_diagnostics(core, block, &mut events);
            }
            ResponseBody::Meter(record) => events.push(Event::Meter(record)),
            ResponseBody::BufferResponse(record) => events.push(Event::BufferAllocated(record)),
            ResponseBody::ModelStart(record) => {
                if dispatcher == Dispatcher::Soc2 && record.status == 0 {
                    push_start_failure(&mut events, record.core, record.slot);
                } else {
                    events.push(Event::ModelStart {
                        core: record.core,
                        slot: record.slot,
                        started: record.status != 0,
                    });
                }
            }
            ResponseBody::SlaveBootBufferAddress(body) => {
                events.push(Event::SlaveBootBufferAddress(body.to_vec()));
            }
            ResponseBody::SlaveBootResponse(word) => events.push(Event::SlaveBootResponse(word)),
            ResponseBody::Midi(record) => events.push(Event::MidiReceived {
                record,
                response_type: kind,
            }),
            ResponseBody::Soc2ArmDiagnostics(block) => {
                events.push(Event::Soc2ArmDiagnostics(block))
            }
            ResponseBody::DebugText(text) => events.push(Event::DebugText {
                soc: dispatcher,
                core_selector: header.selector,
                text,
            }),
            ResponseBody::ModuleLatency(record) => events.push(Event::ModuleLatency(record)),
            ResponseBody::BulkModelStart(record) => {
                for (slot, start) in record.slots() {
                    match start {
                        SlotStart::Started => events.push(Event::ModelStart {
                            core: record.core,
                            slot,
                            started: true,
                        }),
                        SlotStart::Failed => push_start_failure(&mut events, record.core, slot),
                    }
                }
            }
            ResponseBody::BulkMeter(record) => {
                if dispatcher.accepts_bulk_meter(header.selector) {
                    events.push(Event::BulkMeter(record));
                } else {
                    events.push(Event::Ignored {
                        response_type: kind as u8,
                        reason: IgnoreReason::SelectorMismatch,
                    });
                }
            }
            ResponseBody::MidiClockTempo(record) => events.push(Event::MidiClockTempo(record)),
            ResponseBody::Flag0f => events.push(Event::Flag0f),
            ResponseBody::HidReport(report) => events.push(Event::HidReport(report)),
            ResponseBody::UsbMeterUpdate(meter) => events.push(Event::UsbMeterUpdate(meter)),
            ResponseBody::LooperStatus(status) => events.push(Event::LooperStatus(status)),
            ResponseBody::SelfTest(result) => events.push(Event::SelfTest {
                core: dispatcher.self_test_core(header.selector),
                result,
            }),
            ResponseBody::UsbMeterInput { first, second } => {
                events.push(Event::UsbMeterInput { first, second });
            }
            ResponseBody::UsbMeterOutput(meter) => events.push(Event::UsbMeterOutput(meter)),
            ResponseBody::MetronomeStatus(status) => events.push(Event::MetronomeStatus(status)),
            ResponseBody::Soc2SoundsReceived => events.push(Event::Soc2SoundsReceived),
            ResponseBody::Word1c(word) => events.push(Event::Word1c(word)),
            ResponseBody::TunerCalibrate(data) => events.push(Event::TunerCalibrate(data)),
            ResponseBody::TunerTimer(body) => {
                events.push(Event::TunerTimer(KeyMaterial(body.to_vec())))
            }
            ResponseBody::MidiOutTimer(body) => {
                events.push(Event::MidiOutTimer(KeyMaterial(body.to_vec())))
            }
            ResponseBody::StageOneComplete(word) => events.push(Event::StageOneComplete(word)),
            ResponseBody::StageTwoComplete(a, b) => events.push(Event::StageTwoComplete(a, b)),
            ResponseBody::Vector24(key) => {
                events.push(Event::Vector24(KeyMaterial(key.0.to_vec())))
            }
            ResponseBody::Vector25(key) => {
                events.push(Event::Vector25(KeyMaterial(key.0.to_vec())))
            }
            ResponseBody::Vector27(key) => {
                events.push(Event::Vector27(KeyMaterial(key.0.to_vec())))
            }
            ResponseBody::Unknown(_) => unreachable!("unknown types return early"),
        }
        Ok(events)
    }

    /// Keep both SOC caches. Duplicate sibling diagnostics notifications are
    /// omitted, but both FPEx comparators must run on each SOC diagnostics drain.
    fn reduce_diagnostics(
        &mut self,
        core: DspCore,
        block: DiagnosticsBlock,
        events: &mut Vec<Event>,
    ) {
        let index = core as usize;
        self.diagnostics[index] = Some(block);
        events.push(Event::Diagnostics { core, block });
        if block.error_flags() != self.error_flags[index] {
            self.error_flags[index] = block.error_flags();
            events.push(Event::ErrorFlagsChanged {
                core,
                flags: block.error_flags(),
            });
        }
        let pair = match core {
            DspCore::Core0 | DspCore::Core1 => [DspCore::Core0, DspCore::Core1],
            DspCore::Core2 | DspCore::Core3 => [DspCore::Core2, DspCore::Core3],
        };
        for core in pair {
            if let Some(block) = self.diagnostics[core as usize] {
                self.reduce_fpex(core, block, events);
            }
        }
    }

    fn reduce_fpex(&mut self, core: DspCore, block: DiagnosticsBlock, events: &mut Vec<Event>) {
        let tracker = &mut self.fpex[core as usize];
        let count = block.fpex_count() as u32;
        if count != tracker.stored {
            if tracker.holdoff == 0 {
                tracker.stored = count;
                tracker.holdoff = FPEX_WARNING_HOLDOFF;
                events.push(Event::FloatingPointException {
                    core,
                    count: block.fpex_count(),
                    slot: block.fpex_slot(),
                });
            } else {
                tracker.holdoff -= 1;
            }
        }
    }
}

fn push_start_failure(events: &mut Vec<Event>, core: u32, slot: u32) {
    events.push(Event::ModuleStartFailed { core, slot });
    events.push(Event::Crash(CrashCause::ModuleStartFailure { core, slot }));
}

impl fmt::Display for SendDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accepted => f.write_str("accepted"),
            Self::Timeout { consecutive, .. } => {
                write!(f, "timeout {consecutive}/{CRASH_TIMEOUT_COUNT}")
            }
            Self::Crashed => f.write_str("core marked dead after consecutive timeouts"),
            Self::RefusedDead => f.write_str("refused: core is dead"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::FakeMemory;
    use crate::transport::{DspTransport, MESSAGE_MEMORY_BYTES, RingLayout};
    use tetrad_dsp_protocol::responses::encode;

    const CORE0: CoreId = CoreId::Dsp(DspCore::Core0);
    const CORE2: CoreId = CoreId::Dsp(DspCore::Core2);

    fn le_words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }

    fn diagnostics(selector: u8, fpex: i32, slot: u32, flags: u32) -> Vec<u8> {
        let mut words = [0_u32; 16];
        words[3] = fpex as u32;
        words[4] = slot;
        words[12] = flags;
        encode(ResponseType::Diagnostics, selector, &le_words(&words))
    }

    /// Review w5 R7: licence channel bytes (`0x20`, `0x21`) never reach `Debug`; logs print
    /// the kind.
    #[test]
    fn licence_channel_bodies_are_redacted() {
        let mut state = CommsState::new();
        for kind in [ResponseType::TunerTimer, ResponseType::MidiOutTimer] {
            let events = state.receive(CORE2, &encode(kind, 0, &[0x5a; 8])).unwrap();
            let [event] = &events[..] else {
                panic!("{events:?}")
            };
            assert!(matches!(event.kind(), "TunerTimer" | "MidiOutTimer"));
            assert!(format!("{event:?}").ends_with("(KeyMaterial(8 bytes))"));
        }
    }

    #[test]
    fn four_consecutive_timeouts_mark_the_core_dead_and_tempo_sends_always_reset() {
        let mut state = CommsState::new();
        let parameter = MessageType::ParameterChange as u8;
        assert_eq!(
            state.record_send(CORE0, parameter, SendOutcome::TimedOut),
            SendDisposition::Timeout {
                consecutive: 1,
                first_in_run: true
            }
        );
        assert_eq!(
            state.record_send(CORE0, parameter, SendOutcome::TimedOut),
            SendDisposition::Timeout {
                consecutive: 2,
                first_in_run: false
            }
        );
        assert_eq!(
            state.record_send(CORE0, parameter, SendOutcome::Published),
            SendDisposition::Accepted
        );
        assert_eq!(state.link(CORE0), LinkState::default());
        for expected in 1..CRASH_TIMEOUT_COUNT {
            assert_eq!(
                state.record_send(CORE0, parameter, SendOutcome::TimedOut),
                SendDisposition::Timeout {
                    consecutive: expected,
                    first_in_run: expected == 1
                }
            );
            assert!(state.can_send(CORE0));
        }
        assert_eq!(
            state.record_send(CORE0, parameter, SendOutcome::TimedOut),
            SendDisposition::Crashed
        );
        assert!(!state.can_send(CORE0));
        assert_eq!(
            state.record_send(CORE0, parameter, SendOutcome::Published),
            SendDisposition::RefusedDead
        );
        assert!(state.can_send(CoreId::Control));

        let mut tempo = CommsState::new();
        for _ in 0..3 {
            tempo.record_send(CoreId::Control, parameter, SendOutcome::TimedOut);
        }
        assert_eq!(
            tempo.record_send(CoreId::Control, ALWAYS_ACCEPTED_TYPE, SendOutcome::TimedOut),
            SendDisposition::Accepted
        );
        assert_eq!(tempo.link(CoreId::Control), LinkState::default());
        tempo.mark_dead(CORE2);
        assert_eq!(
            tempo.record_send(CORE2, ALWAYS_ACCEPTED_TYPE, SendOutcome::Published),
            SendDisposition::RefusedDead
        );
        assert_eq!(
            SendDisposition::Timeout {
                consecutive: 2,
                first_in_run: false
            }
            .to_string(),
            "timeout 2/4"
        );
    }

    #[test]
    fn sibling_diagnostics_advance_soc1_fpex_holdoff() {
        sibling_fpex_holdoff(CORE0, DspCore::Core0);
    }

    #[test]
    fn sibling_diagnostics_advance_soc2_fpex_holdoff() {
        sibling_fpex_holdoff(CORE2, DspCore::Core2);
    }

    fn sibling_fpex_holdoff(ring: CoreId, core: DspCore) {
        let mut state = CommsState::new();
        let warnings = |events: Vec<Event>| {
            events
                .into_iter()
                .filter(|event| matches!(event, Event::FloatingPointException { .. }))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            warnings(state.receive(ring, &diagnostics(0, 1, 9, 0)).unwrap()),
            [Event::FloatingPointException {
                core,
                count: 1,
                slot: 9
            }]
        );
        assert!(warnings(state.receive(ring, &diagnostics(0, 2, 9, 0)).unwrap()).is_empty());
        for _ in 0..4 {
            assert!(warnings(state.receive(ring, &diagnostics(1, 0, 0, 0)).unwrap()).is_empty());
        }
        assert_eq!(
            warnings(state.receive(ring, &diagnostics(1, 0, 0, 0)).unwrap()),
            [Event::FloatingPointException {
                core,
                count: 2,
                slot: 9
            }]
        );
        // Reported counts consume no holdoff, even on sibling drains.
        for _ in 0..6 {
            assert!(warnings(state.receive(ring, &diagnostics(1, 0, 0, 0)).unwrap()).is_empty());
        }
        assert!(warnings(state.receive(ring, &diagnostics(0, 0, 0, 0)).unwrap()).is_empty());
        for _ in 0..4 {
            assert!(warnings(state.receive(ring, &diagnostics(1, 0, 0, 0)).unwrap()).is_empty());
        }
        assert_eq!(
            warnings(state.receive(ring, &diagnostics(1, 0, 0, 0)).unwrap()),
            [Event::FloatingPointException {
                core,
                count: 0,
                slot: 0
            }]
        );
    }

    #[test]
    fn diagnostics_route_by_dispatcher_and_rate_limit_fpex_warnings() {
        let mut state = CommsState::new();
        let events = state.receive(CORE0, &diagnostics(0, 0, 0, 0)).unwrap();
        assert!(matches!(
            events[..],
            [Event::Diagnostics {
                core: DspCore::Core0,
                ..
            }]
        ));

        let events = state
            .receive(CoreId::Control, &diagnostics(5, 1, 9, 0x10))
            .unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0],
            Event::Diagnostics {
                core: DspCore::Core3,
                ..
            }
        ));
        assert_eq!(
            events[1],
            Event::ErrorFlagsChanged {
                core: DspCore::Core3,
                flags: 0x10
            }
        );
        assert_eq!(
            events[2],
            Event::FloatingPointException {
                core: DspCore::Core3,
                count: 1,
                slot: 9
            }
        );

        // Same flags, five more changed FPEx samples: all swallowed by the holdoff.
        for count in 2..=6 {
            let events = state
                .receive(CoreId::Control, &diagnostics(5, count, 9, 0x10))
                .unwrap();
            assert_eq!(events.len(), 1, "sample {count}");
        }
        let events = state
            .receive(CoreId::Control, &diagnostics(5, 7, 3, 0x10))
            .unwrap();
        assert_eq!(
            events[1],
            Event::FloatingPointException {
                core: DspCore::Core3,
                count: 7,
                slot: 3
            }
        );
        let events = state
            .receive(CoreId::Control, &diagnostics(5, 7, 3, 0x10))
            .unwrap();
        assert_eq!(events.len(), 1);

        // Core 2 keeps its own comparators and reads selector 0 on SOC2.
        let events = state.receive(CORE2, &diagnostics(0, -1, 2, 0)).unwrap();
        assert_eq!(
            events[1],
            Event::FloatingPointException {
                core: DspCore::Core2,
                count: -1,
                slot: 2
            }
        );
        let events = state.receive(CORE2, &diagnostics(2, 0, 0, 0x10)).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn module_start_failures_raise_crash_only_where_stock_does() {
        let mut state = CommsState::new();
        let single = encode(ResponseType::ModelStart, 0, &le_words(&[1, 4, 0]));
        assert_eq!(
            state.receive(CORE0, &single).unwrap(),
            [Event::ModelStart {
                core: 1,
                slot: 4,
                started: false
            }]
        );
        assert_eq!(
            state.receive(CORE2, &single).unwrap(),
            [
                Event::ModuleStartFailed { core: 1, slot: 4 },
                Event::Crash(CrashCause::ModuleStartFailure { core: 1, slot: 4 })
            ]
        );
        let started = encode(ResponseType::ModelStart, 0, &le_words(&[3, 29, 7]));
        assert_eq!(
            state.receive(CORE2, &started).unwrap(),
            [Event::ModelStart {
                core: 3,
                slot: 29,
                started: true
            }]
        );

        let bulk = encode(
            ResponseType::BulkModelStart,
            0,
            &le_words(&[2, 0b10, 0b11 | (1 << 30)]),
        );
        assert_eq!(
            state.receive(CORE0, &bulk).unwrap(),
            [
                Event::ModuleStartFailed { core: 2, slot: 0 },
                Event::Crash(CrashCause::ModuleStartFailure { core: 2, slot: 0 }),
                Event::ModelStart {
                    core: 2,
                    slot: 1,
                    started: true
                },
            ]
        );
        assert!(
            state.can_send(CORE2),
            "crash signals do not mark links dead"
        );
    }

    #[test]
    fn dispatcher_tables_selector_gates_and_malformed_packets() {
        let mut state = CommsState::new();
        let looper = encode(ResponseType::LooperStatus, 0, &[0; 88]);
        assert_eq!(
            state.receive(CORE0, &looper).unwrap(),
            [Event::Ignored {
                response_type: 0x12,
                reason: IgnoreReason::NotHandledByDispatcher
            }]
        );
        assert!(matches!(
            state.receive(CoreId::Control, &looper).unwrap()[..],
            [Event::LooperStatus(_)]
        ));
        let boot = encode(ResponseType::SlaveBootResponse, 0, &le_words(&[1]));
        assert_eq!(
            state.receive(CORE0, &boot).unwrap(),
            [Event::SlaveBootResponse(1)]
        );
        assert_eq!(
            state.receive(CORE2, &boot).unwrap(),
            [Event::Ignored {
                response_type: 0x06,
                reason: IgnoreReason::NotHandledByDispatcher
            }]
        );

        let mut bulk = le_words(&[1, 2, 1]);
        bulk.extend([0; 64]);
        let soc2_wrong = encode(ResponseType::BulkMeter, 0, &bulk);
        assert_eq!(
            state.receive(CORE2, &soc2_wrong).unwrap(),
            [Event::Ignored {
                response_type: 0x0d,
                reason: IgnoreReason::SelectorMismatch
            }]
        );
        assert!(matches!(
            state.receive(CORE0, &soc2_wrong).unwrap()[..],
            [Event::BulkMeter(_)]
        ));
        assert!(matches!(
            state
                .receive(CORE2, &encode(ResponseType::BulkMeter, 2, &bulk))
                .unwrap()[..],
            [Event::BulkMeter(_)]
        ));

        let self_test = encode(ResponseType::SelfTest, 2, &le_words(&[9, 8, 7, 6]));
        assert!(matches!(
            state.receive(CORE2, &self_test).unwrap()[..],
            [Event::SelfTest {
                core: DspCore::Core2,
                ..
            }]
        ));
        assert!(matches!(
            state.receive(CORE0, &self_test).unwrap()[..],
            [Event::SelfTest {
                core: DspCore::Core1,
                ..
            }]
        ));

        let midi = encode(
            ResponseType::MidiFromType07,
            0,
            &le_words(&[0xf8_0000, 0, 0]),
        );
        assert!(matches!(
            state.receive(CORE2, &midi).unwrap()[..],
            [Event::MidiReceived {
                response_type: ResponseType::MidiFromType07,
                ..
            }]
        ));
        assert_eq!(
            state.receive(CORE0, &[0x42, 0, 0, 0, 9, 0, 0, 0]).unwrap(),
            [Event::Ignored {
                response_type: 0x42,
                reason: IgnoreReason::UnknownType
            }]
        );

        let before = state.clone();
        let mut truncated = diagnostics(0, 1, 1, 1);
        truncated.truncate(70);
        assert!(state.receive(CORE0, &truncated).is_err());
        assert_eq!(
            state.receive(CORE0, &[0x12]),
            Err(ReceiveError::Protocol(ProtocolError::Invalid(
                "response header"
            )))
        );
        assert_eq!(state, before);
    }

    #[test]
    fn transport_receive_feeds_the_dispatcher_with_an_incoming_hid_report() {
        let layout = RingLayout::incoming(CoreId::Control);
        let packet = encode(ResponseType::HidReport, 0, &[0x5a; 128]);
        assert_eq!(packet.len(), 136);
        let mut memory = FakeMemory::new(MESSAGE_MEMORY_BYTES);
        let slot = 1;
        let payload = layout.payload_offset + slot * layout.slot_bytes;
        memory.bytes[payload..payload + packet.len()].copy_from_slice(&packet);
        memory.bytes[layout.length_offset + slot * 4..layout.length_offset + slot * 4 + 4]
            .copy_from_slice(&(packet.len() as u32).to_le_bytes());
        memory.bytes[layout.producer_offset] = slot as u8;
        let mut transport = DspTransport::new(memory).unwrap();
        let received = transport.try_receive(CoreId::Control).unwrap().unwrap();
        let mut state = CommsState::new();
        let events = state.receive(CoreId::Control, &received).unwrap();
        assert_eq!(events, [Event::HidReport(HidReport([0x5a; 128]))]);
        assert!(!format!("{events:?}").contains("90"));
        assert_eq!(transport.try_receive(CoreId::Control).unwrap(), None);
    }
}
