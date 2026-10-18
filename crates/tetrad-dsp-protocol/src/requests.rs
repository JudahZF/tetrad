//! Boot, global audio, model lifecycle, diagnostics and opaque-data request
//! codecs recovered from ZenUI `DSPController` and `DSPCommsARM`.
//!
//! Every writer stores the type at `+0`, a core byte at `+1` and the LE total at `+8` of the shared
//! 16-byte header; other fields are stored per type. Stock reuses a never-cleared buffer, so
//! unstored bytes hold stale data. [`RequestFrame::new`] zero-fills them (owned policy) and
//! [`RequestFrame::decode`] retains them. Floats travel as raw bits; these are wire codecs, not UI
//! range validators.
use std::fmt;

use zeroize::Zeroize;

use crate::tuner_looper::TextPrefix;
use crate::{CoreId, DeploymentTarget, DspCore, Error, MessageType, ModuleSlot, read_u32};

pub const HEADER_BYTES: usize = 16;
/// One control-core ring slot (144 bytes) bounds every fixed request here.
pub const MAX_BYTES: usize = 144;
/// Opaque bodies for `0x50`/`0x51` must fit one control-core slot. Stock
/// copies them unbounded into a 0x800 buffer and a 144-byte slot.
pub const OPAQUE_BODY_MAX: usize = MAX_BYTES - HEADER_BYTES;

/// Wait flag and timeout (ms) a recovered writer passes to `sendMessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendPolicy {
    pub wait: bool,
    pub timeout_ms: u32,
}

impl SendPolicy {
    pub const fn new(wait: bool, timeout_ms: u32) -> Self {
        Self { wait, timeout_ms }
    }
}

/// `0x0c` body (VA 0x51711c): 0.0 when not bypassed; when bypassed, 2.0 if
/// the caller's second flag is set, else 1.0. The second flag's meaning is
/// unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassState {
    Engaged,
    Bypassed,
    BypassedFlagged,
}

impl BypassState {
    pub const fn value_bits(self) -> u32 {
        match self {
            Self::Engaged => 0,
            Self::Bypassed => 0x3f80_0000,
            Self::BypassedFlagged => 0x4000_0000,
        }
    }

    pub fn from_bits(bits: u32) -> Result<Self, Error> {
        match bits {
            0 => Ok(Self::Engaged),
            0x3f80_0000 => Ok(Self::Bypassed),
            0x4000_0000 => Ok(Self::BypassedFlagged),
            _ => Err(Error::Invalid("model bypass value")),
        }
    }
}

/// Raw bytes for `0x50`/`0x51` (account data and `/opt/neuraldsp/licence`).
/// `Debug` shows only the length. Not `Copy`: the bytes are zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct OpaqueBody {
    bytes: [u8; OPAQUE_BODY_MAX],
    len: u8,
}

/// The empty body (factory reset `0x51`, VA 0x1f0c50).
impl Default for OpaqueBody {
    fn default() -> Self {
        Self {
            bytes: [0; OPAQUE_BODY_MAX],
            len: 0,
        }
    }
}

impl OpaqueBody {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl TryFrom<&[u8]> for OpaqueBody {
    type Error = Error;
    fn try_from(value: &[u8]) -> Result<Self, Error> {
        if value.len() > OPAQUE_BODY_MAX {
            return Err(Error::Limit("opaque request body"));
        }
        let mut bytes = [0; OPAQUE_BODY_MAX];
        bytes[..value.len()].copy_from_slice(value);
        Ok(Self {
            bytes,
            len: value.len() as u8,
        })
    }
}

impl Drop for OpaqueBody {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl fmt::Debug for OpaqueBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpaqueBody")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// Fields each recovered writer stores. Writer constants are implied by the
/// variant; numeric IDs, selectors and float bits stay raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    // Boot handshake (`DSPCommsARM`).
    /// `0x54`, VA 0x89c848: core 0, header only.
    DdrTestA,
    /// `0x55`, VA 0x89c848.
    DdrTestB,
    /// `0x57`, VA 0x89c848.
    DataiMessage,
    // `0x12`/`0x13`/`0x14` (SOC2 slave boot) are encoded only by
    // `tetrad_loader::soc2::SlaveBootRequest`; `decode` refuses them here.
    /// `0x53` (nominal `CalGateLevel`), VA 0x514bac "Preparing SOC2": byte
    /// `+1` is 0 but the message goes to the control core. Answered by
    /// response `0x23` (stage two complete).
    PrepareSoc2,

    // Global audio (`DSPController`).
    /// `0x04`, VA 0x511bb0. Caller sends `10^((x*40-40)/20)`, 0 at x = 0.
    MasterVolumeChange { gain_bits: u32 },
    /// `0x05`, VA 0x511968, core 0. Stock inputs 1, 2, 4, 5.
    SetInputGain { input: u8, gain_bits: u32 },
    /// `0x06`, VA 0x511a38. Outputs 0..=5; caller sends `10^((x*52-40)/20)`.
    SetOutputGain { output: u8, gain_bits: u32 },
    /// `0x11`, VA 0x511ae4. Mix selectors 1..=9.
    SetHeadphoneMixLevel { mix: u8, level_bits: u32 },
    /// `0x21`, VA 0x5119d0.
    SetUsbReturnGain { gain_bits: u32 },
    /// `0x3a`, VA 0x511c7c. Body is `(float)flag`; stock outputs 0..=3.
    SetMasterVolumeAssignment { output: u8, value_bits: u32 },
    /// `0x19`, VA 0x511ee0. `+4` unstored.
    LimiterBypass { value: u32 },
    /// `0x33`, VA 0x513878. Value travels in `+2`; `+4` unstored.
    HeadphoneUsbFeedEnable { value: u8 },
    /// `0x39`, VA 0x514d9c. Stock sends core 2, then the control core.
    /// `+2` and `+4` unstored.
    SwitchUsbIns {
        destination: CoreId,
        value_bits: u32,
    },
    /// `0x20`, VA 0x876470. Stock sends core 0, then core 1. Body is
    /// `mute | (flag << 1)`; the flag's meaning is unresolved.
    MasterMute { core: DspCore, value: u32 },
    /// `0x56`, VA 0x5159c4. Outputs 0..=7. `+4` unstored.
    SetOutputCalibrationGains { output: u8, gain_bits: u32 },
    /// `0x59`, VA 0x515e00, core 0. Value in `+2`.
    PowerRailMute { value: u8 },
    /// `0x5a`, VA 0x87652c. Seventeen bytes; the value is the body byte.
    LibUsbConnect { value: u8 },
    /// `0x4c`, VA 0x515934. Stock sends one per DSP core.
    DynamicDelayCompensation { core: DspCore, value: u32 },
    /// `0x58`, VA 0x7d6500. `10^(dB/20)` for dB in -40..=12. `+2` unstored.
    SetHeadphoneOutputLevel { level_bits: u32 },

    // Model lifecycle.
    /// `0x02`, VA 0x517010 outside bulk mode: the float path of a single edit
    /// (`set_param` VA 0x523644 → 0x51e4d8). `+3` and `+12..16` unstored.
    /// Same bytes as [`crate::packet::ParameterChange`] with a zero template.
    ParameterChange {
        target: DeploymentTarget,
        index: u32,
        value_bits: u32,
    },
    /// `0x03`, VA 0x511318 (also 0x517228 with parameter 5).
    IntParameterChange {
        target: DeploymentTarget,
        parameter: u32,
        value: u32,
    },
    /// `0x0c`, VA 0x51711c. `+4` unstored.
    ModelBypass {
        target: DeploymentTarget,
        state: BypassState,
    },
    /// `0x1f`, VA 0x5112b4. Deploy sends word 0 to every DSP core.
    EnableAll { core: DspCore, value: u32 },
    /// `0x1e`, VA 0x51082c.
    DeleteAllModels { core: DspCore },
    /// `0x0f`, VA 0x8757b0. Body: id, constant 2, byte size.
    BufferAllocate { core: DspCore, id: u32, bytes: u32 },
    /// `0x10`, VA 0x87582c. Body: id, 0, 0.
    BufferFree { core: DspCore, id: u32 },
    /// `0x48`/`0x49`, VA 0x5156b8. Stock sends one per DSP core from a
    /// stack buffer. Source slot and core meanings are inferred.
    RouteToSidechainBuffer {
        core: DspCore,
        same_core: bool,
        channel: u8,
        source_slot: u32,
        source_core: u32,
    },
    /// `0x3e`, VA 0x511188, core 0. `+4`, `+12` unstored.
    BulkModelBypassSync,
    /// `0x17`, VA 0x51965c after a direct receive-buffer copy on core 0/1.
    ReceiveDataEnd {
        target: DeploymentTarget,
        buffer: u32,
        bytes: u32,
    },
    /// `0x0e`, vtable `+0x3c` VA 0x89b474: only while metering is enabled,
    /// without crash accounting.
    MeterRequest { core: CoreId, slot: u8, word: u32 },

    // Diagnostics and self-test.
    /// `0x0a`, VAs 0x50ea74 / 0x50ed68 (also to the control core).
    DiagnosticsRequest { core: CoreId },
    /// `0x1a`, VAs 0x50ea74 / 0x50eeac.
    DebugCommsRequest { core: CoreId },
    /// `0x1b`, VA 0x50edb4. Answered by response `0x0b`.
    ModuleLatencyRequest { target: DeploymentTarget },
    /// `0x40`, VA 0x50ee5c.
    ActivateSelfTestMode { core: CoreId, enabled: u32 },
    /// `0x41`, VA 0x50ee08.
    SelfTestDiagnosticRequest { core: CoreId, selector: u8 },
    /// `0x28`, VA 0x875f5c, core 2: 1 starts, 0 stops.
    TestSoc2Ddr { running: u32 },

    // Opaque data with cover names (not link tests or gate reads).
    /// `0x42`, VA 0x5152c4: device text to core 3.
    Text42(TextPrefix),
    /// `0x43` (nominal `TestSoc2`), VA 0x5153cc: text to core 3. The local backup asks with it for
    /// the creator stamp key (`0x3fc054`), answered by response `0x18`.
    StampKey(TextPrefix),
    /// `0x50`, VA 0x515640: account bytes to the control core.
    Blob50(OpaqueBody),
    /// `0x51`, VA 0x5155c4: account bytes or the licence file (VA 0x876810).
    Blob51(OpaqueBody),

    // Cloud request signing. The body is the device serial as 8-bit text;
    // `tuner_looper::TunerText35`/`TunerText4f` write the same bytes under their cover names.
    /// `0x35` (nominal `TunerMuteMessage`), VA 0x5151bc: serial to core 3. Answered by
    /// response `0x11`, the 16-byte signing material.
    SigningMaterial(TextPrefix),
    /// `0x4f` (nominal `TunerCalibrate`), VA 0x5154c8: serial to the control core. Answered
    /// by response `0x1f`, the 32-byte client id.
    ClientId(TextPrefix),
}

impl Request {
    pub const fn message_type(&self) -> MessageType {
        use MessageType as T;
        match self {
            Self::DdrTestA => T::DdrTestA,
            Self::DdrTestB => T::DdrTestB,
            Self::DataiMessage => T::DataiMessage,
            Self::PrepareSoc2 => T::CalGateLevel,
            Self::MasterVolumeChange { .. } => T::MasterVolumeChange,
            Self::SetInputGain { .. } => T::SetInputGain,
            Self::SetOutputGain { .. } => T::SetOutputGain,
            Self::SetHeadphoneMixLevel { .. } => T::SetHeadphoneMixLevel,
            Self::SetUsbReturnGain { .. } => T::SetUsbReturnGain,
            Self::SetMasterVolumeAssignment { .. } => T::SetMasterVolumeAssignment,
            Self::LimiterBypass { .. } => T::LimiterBypass,
            Self::HeadphoneUsbFeedEnable { .. } => T::HeadphoneUsbFeedEnable,
            Self::SwitchUsbIns { .. } => T::SwitchUsbIns,
            Self::MasterMute { .. } => T::MasterMute,
            Self::SetOutputCalibrationGains { .. } => T::SetOutputCalibrationGains,
            Self::PowerRailMute { .. } => T::PowerRailMute,
            Self::LibUsbConnect { .. } => T::LibUsbConnect,
            Self::DynamicDelayCompensation { .. } => T::DynamicDelayCompensation,
            Self::SetHeadphoneOutputLevel { .. } => T::SetHeadphoneOutputLevel,
            Self::ParameterChange { .. } => T::ParameterChange,
            Self::IntParameterChange { .. } => T::IntParameterChange,
            Self::ModelBypass { .. } => T::ModelBypass,
            Self::EnableAll { .. } => T::EnableAll,
            Self::DeleteAllModels { .. } => T::DeleteAllModels,
            Self::BufferAllocate { .. } => T::BufferAllocate,
            Self::BufferFree { .. } => T::BufferFree,
            Self::RouteToSidechainBuffer {
                same_core: false, ..
            } => T::RouteToSidechainBuffer,
            Self::RouteToSidechainBuffer { .. } => T::RouteToSidechainBufferSameCore,
            Self::BulkModelBypassSync => T::BulkModelBypassSync,
            Self::ReceiveDataEnd { .. } => T::ReceiveDataEnd,
            Self::MeterRequest { .. } => T::MeterRequest,
            Self::DiagnosticsRequest { .. } => T::DiagnosticsRequest,
            Self::DebugCommsRequest { .. } => T::DebugCommsRequest,
            Self::ModuleLatencyRequest { .. } => T::ModuleLatencyRequest,
            Self::ActivateSelfTestMode { .. } => T::ActivateSelfTestMode,
            Self::SelfTestDiagnosticRequest { .. } => T::SelfTestDiagnosticRequest,
            Self::TestSoc2Ddr { .. } => T::TestSoc2Ddr,
            Self::Text42(_) => T::TestLinkPort,
            Self::StampKey(_) => T::TestSoc2,
            Self::Blob50(_) => T::BulkStatusRead,
            Self::Blob51(_) => T::GetInputGateParam,
            Self::SigningMaterial(_) => T::TunerMuteMessage,
            Self::ClientId(_) => T::TunerCalibrate,
        }
    }

    /// Transport core: the stock `sendMessage` core argument.
    pub const fn destination(&self) -> CoreId {
        const CORE0: CoreId = CoreId::Dsp(DspCore::Core0);
        const CORE1: CoreId = CoreId::Dsp(DspCore::Core1);
        match *self {
            Self::DdrTestA
            | Self::DdrTestB
            | Self::DataiMessage
            | Self::SetInputGain { .. }
            | Self::PowerRailMute { .. }
            | Self::BulkModelBypassSync => CORE0,
            Self::MasterVolumeChange { .. }
            | Self::SetOutputGain { .. }
            | Self::SetHeadphoneMixLevel { .. }
            | Self::SetUsbReturnGain { .. }
            | Self::SetMasterVolumeAssignment { .. }
            | Self::LimiterBypass { .. }
            | Self::HeadphoneUsbFeedEnable { .. }
            | Self::SetOutputCalibrationGains { .. }
            | Self::SetHeadphoneOutputLevel { .. } => CORE1,
            Self::TestSoc2Ddr { .. } => CoreId::Dsp(DspCore::Core2),
            Self::Text42(_) | Self::StampKey(_) | Self::SigningMaterial(_) => {
                CoreId::Dsp(DspCore::Core3)
            }
            Self::PrepareSoc2
            | Self::LibUsbConnect { .. }
            | Self::Blob50(_)
            | Self::Blob51(_)
            | Self::ClientId(_) => CoreId::Control,
            Self::SwitchUsbIns { destination, .. } => destination,
            Self::MasterMute { core, .. }
            | Self::DynamicDelayCompensation { core, .. }
            | Self::EnableAll { core, .. }
            | Self::DeleteAllModels { core }
            | Self::BufferAllocate { core, .. }
            | Self::BufferFree { core, .. }
            | Self::RouteToSidechainBuffer { core, .. } => CoreId::Dsp(core),
            Self::ParameterChange { target, .. }
            | Self::IntParameterChange { target, .. }
            | Self::ModelBypass { target, .. }
            | Self::ReceiveDataEnd { target, .. }
            | Self::ModuleLatencyRequest { target } => CoreId::Dsp(target.core),
            Self::MeterRequest { core, .. }
            | Self::DiagnosticsRequest { core }
            | Self::DebugCommsRequest { core }
            | Self::ActivateSelfTestMode { core, .. }
            | Self::SelfTestDiagnosticRequest { core, .. } => core,
        }
    }

    /// Byte `+1`. Equal to the destination except for `PrepareSoc2`.
    const fn header_core(&self) -> u8 {
        match self {
            Self::PrepareSoc2 => 0,
            _ => self.destination().wire(),
        }
    }

    /// The stock writer's wait flag and timeout.
    pub const fn send_policy(&self) -> SendPolicy {
        match self {
            Self::DdrTestA | Self::DdrTestB | Self::DataiMessage => SendPolicy::new(true, 500),
            Self::DeleteAllModels { .. } => SendPolicy::new(true, 500),
            Self::PrepareSoc2
            | Self::LibUsbConnect { .. }
            | Self::ActivateSelfTestMode { .. }
            | Self::Blob50(_)
            | Self::Blob51(_)
            | Self::ClientId(_) => SendPolicy::new(true, 1000),
            Self::RouteToSidechainBuffer { .. } | Self::ModuleLatencyRequest { .. } => {
                SendPolicy::new(false, 1000)
            }
            Self::MasterVolumeChange { .. }
            | Self::SetInputGain { .. }
            | Self::SetOutputGain { .. }
            | Self::SetHeadphoneMixLevel { .. }
            | Self::SetUsbReturnGain { .. }
            | Self::SetMasterVolumeAssignment { .. }
            | Self::LimiterBypass { .. }
            | Self::HeadphoneUsbFeedEnable { .. }
            | Self::MasterMute { .. }
            | Self::PowerRailMute { .. }
            | Self::DynamicDelayCompensation { .. }
            | Self::SetHeadphoneOutputLevel { .. }
            | Self::BufferAllocate { .. }
            | Self::BufferFree { .. }
            | Self::ReceiveDataEnd { .. } => SendPolicy::new(true, 100),
            Self::SwitchUsbIns { .. }
            | Self::SetOutputCalibrationGains { .. }
            | Self::ParameterChange { .. }
            | Self::IntParameterChange { .. }
            | Self::ModelBypass { .. }
            | Self::EnableAll { .. }
            | Self::BulkModelBypassSync
            | Self::MeterRequest { .. }
            | Self::DiagnosticsRequest { .. }
            | Self::DebugCommsRequest { .. }
            | Self::SelfTestDiagnosticRequest { .. }
            | Self::TestSoc2Ddr { .. }
            | Self::Text42(_)
            | Self::StampKey(_)
            | Self::SigningMaterial(_) => SendPolicy::new(false, 100),
        }
    }

    fn total_bytes(&self) -> usize {
        HEADER_BYTES
            + match self {
                Self::LibUsbConnect { .. } => 1,
                Self::BufferAllocate { .. } | Self::BufferFree { .. } => 12,
                Self::Text42(text)
                | Self::StampKey(text)
                | Self::SigningMaterial(text)
                | Self::ClientId(text) => text.as_bytes().len(),
                Self::Blob50(body) | Self::Blob51(body) => body.as_bytes().len(),
                Self::MasterVolumeChange { .. }
                | Self::SetInputGain { .. }
                | Self::SetOutputGain { .. }
                | Self::SetHeadphoneMixLevel { .. }
                | Self::SetUsbReturnGain { .. }
                | Self::SetMasterVolumeAssignment { .. }
                | Self::LimiterBypass { .. }
                | Self::SwitchUsbIns { .. }
                | Self::MasterMute { .. }
                | Self::SetOutputCalibrationGains { .. }
                | Self::DynamicDelayCompensation { .. }
                | Self::SetHeadphoneOutputLevel { .. }
                | Self::ParameterChange { .. }
                | Self::IntParameterChange { .. }
                | Self::ModelBypass { .. } => 4,
                _ => 0,
            }
    }

    /// Owned admission of the shapes stock callers produce.
    fn validate(&self) -> Result<(), Error> {
        match *self {
            Self::SwitchUsbIns { destination, .. }
                if !matches!(destination, CoreId::Dsp(DspCore::Core2) | CoreId::Control) =>
            {
                Err(Error::Invalid("SwitchUsbIns destination"))
            }
            Self::MasterMute { core, .. } if !matches!(core, DspCore::Core0 | DspCore::Core1) => {
                Err(Error::Invalid("MasterMute core"))
            }
            Self::SetOutputCalibrationGains { output, .. } if output > 7 => {
                Err(Error::Invalid("calibration output"))
            }
            _ => Ok(()),
        }
    }

    /// Writes the stored fields over `bytes`, leaving unstored bytes untouched.
    fn write(&self, bytes: &mut [u8; MAX_BYTES]) {
        let total = self.total_bytes();
        bytes[0] = self.message_type() as u8;
        bytes[1] = self.header_core();
        bytes[8..12].copy_from_slice(&(total as u32).to_le_bytes());
        let mut put = |offset: usize, word: u32| {
            bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        };
        // (selector +2, word +4, body word +16)
        let (selector, word, body) = match *self {
            Self::DdrTestA
            | Self::DdrTestB
            | Self::DataiMessage
            | Self::PrepareSoc2
            | Self::DiagnosticsRequest { .. }
            | Self::DebugCommsRequest { .. }
            | Self::Text42(_)
            | Self::StampKey(_)
            | Self::SigningMaterial(_)
            | Self::ClientId(_)
            | Self::Blob50(_)
            | Self::Blob51(_) => (None, None, None),
            Self::MasterVolumeChange { gain_bits } | Self::SetUsbReturnGain { gain_bits } => {
                (Some(0), Some(0), Some(gain_bits))
            }
            Self::SetInputGain {
                input: selector,
                gain_bits: body,
            }
            | Self::SetOutputGain {
                output: selector,
                gain_bits: body,
            }
            | Self::SetHeadphoneMixLevel {
                mix: selector,
                level_bits: body,
            }
            | Self::SetMasterVolumeAssignment {
                output: selector,
                value_bits: body,
            } => (Some(selector), Some(0), Some(body)),
            Self::LimiterBypass { value } => (Some(0), None, Some(value)),
            Self::HeadphoneUsbFeedEnable { value } => (Some(value), None, None),
            Self::SwitchUsbIns { value_bits, .. } => (None, None, Some(value_bits)),
            Self::MasterMute { value, .. } | Self::DynamicDelayCompensation { value, .. } => {
                (Some(0), Some(0), Some(value))
            }
            Self::EnableAll { value, .. } => (Some(0), Some(value), None),
            Self::SetOutputCalibrationGains { output, gain_bits } => {
                (Some(output), None, Some(gain_bits))
            }
            Self::PowerRailMute { value } => (Some(value), Some(0), None),
            Self::LibUsbConnect { .. } => (Some(0), Some(0), None),
            Self::SetHeadphoneOutputLevel { level_bits } => (None, Some(0), Some(level_bits)),
            Self::ParameterChange {
                target,
                index: parameter,
                value_bits: value,
            }
            | Self::IntParameterChange {
                target,
                parameter,
                value,
            } => (Some(target.slot.index()), Some(parameter), Some(value)),
            Self::ModelBypass { target, state } => {
                (Some(target.slot.index()), None, Some(state.value_bits()))
            }
            Self::DeleteAllModels { .. } => (Some(0), Some(0), None),
            Self::BufferAllocate { id, bytes, .. } => {
                put(20, 2);
                put(24, bytes);
                (Some(0), Some(0), Some(id))
            }
            Self::BufferFree { id, .. } => {
                put(20, 0);
                put(24, 0);
                (Some(0), Some(0), Some(id))
            }
            Self::RouteToSidechainBuffer {
                channel,
                source_slot,
                source_core,
                ..
            } => {
                put(12, source_core);
                (Some(channel), Some(source_slot), None)
            }
            Self::BulkModelBypassSync => (Some(0), None, None),
            Self::ReceiveDataEnd {
                target,
                buffer,
                bytes,
            } => {
                put(12, bytes);
                (Some(target.slot.index()), Some(buffer), None)
            }
            Self::MeterRequest { slot, word, .. } => (Some(slot), Some(word), None),
            Self::ModuleLatencyRequest { target } => (Some(target.slot.index()), None, None),
            Self::ActivateSelfTestMode { enabled, .. } => (None, Some(enabled), None),
            Self::SelfTestDiagnosticRequest { selector, .. } => (Some(selector), None, None),
            Self::TestSoc2Ddr { running } => (Some(0), Some(running), None),
        };
        if let Some(word) = word {
            put(4, word);
        }
        if let Some(body) = body {
            put(16, body);
        }
        if let Some(selector) = selector {
            bytes[2] = selector;
        }
        match self {
            Self::Text42(text)
            | Self::StampKey(text)
            | Self::SigningMaterial(text)
            | Self::ClientId(text) => bytes[16..total].copy_from_slice(text.as_bytes()),
            Self::Blob50(body) | Self::Blob51(body) => {
                bytes[16..total].copy_from_slice(body.as_bytes())
            }
            Self::LibUsbConnect { value } => bytes[16] = *value,
            _ => {}
        }
    }

    /// Reads the typed view: fixed fields from zero-padded storage, variable
    /// bodies from the exact frame. The caller checks sizes and constants.
    fn parse(
        message_type: MessageType,
        frame: &[u8],
        bytes: &[u8; MAX_BYTES],
    ) -> Result<Self, Error> {
        use MessageType as T;
        let dsp = || DspCore::try_from(bytes[1]);
        let any = || CoreId::try_from(bytes[1]);
        let target = || -> Result<DeploymentTarget, Error> {
            Ok(DeploymentTarget {
                core: dsp()?,
                slot: ModuleSlot::try_from(u32::from(bytes[2]))?,
            })
        };
        let selector = bytes[2];
        let word = read_u32(bytes, 4);
        let body = || read_u32(bytes, 16);
        let tail = &frame[HEADER_BYTES..];
        Ok(match message_type {
            T::DdrTestA => Self::DdrTestA,
            T::DdrTestB => Self::DdrTestB,
            T::DataiMessage => Self::DataiMessage,
            T::CalGateLevel => Self::PrepareSoc2,
            T::MasterVolumeChange => Self::MasterVolumeChange { gain_bits: body() },
            T::SetInputGain => Self::SetInputGain {
                input: selector,
                gain_bits: body(),
            },
            T::SetOutputGain => Self::SetOutputGain {
                output: selector,
                gain_bits: body(),
            },
            T::SetHeadphoneMixLevel => Self::SetHeadphoneMixLevel {
                mix: selector,
                level_bits: body(),
            },
            T::SetUsbReturnGain => Self::SetUsbReturnGain { gain_bits: body() },
            T::SetMasterVolumeAssignment => Self::SetMasterVolumeAssignment {
                output: selector,
                value_bits: body(),
            },
            T::LimiterBypass => Self::LimiterBypass { value: body() },
            T::HeadphoneUsbFeedEnable => Self::HeadphoneUsbFeedEnable { value: selector },
            T::SwitchUsbIns => Self::SwitchUsbIns {
                destination: any()?,
                value_bits: body(),
            },
            T::MasterMute => Self::MasterMute {
                core: dsp()?,
                value: body(),
            },
            T::SetOutputCalibrationGains => Self::SetOutputCalibrationGains {
                output: selector,
                gain_bits: body(),
            },
            T::PowerRailMute => Self::PowerRailMute { value: selector },
            T::LibUsbConnect => Self::LibUsbConnect {
                value: *tail.first().ok_or(Error::Invalid("request size"))?,
            },
            T::DynamicDelayCompensation => Self::DynamicDelayCompensation {
                core: dsp()?,
                value: body(),
            },
            T::SetHeadphoneOutputLevel => Self::SetHeadphoneOutputLevel { level_bits: body() },
            T::ParameterChange => Self::ParameterChange {
                target: target()?,
                index: word,
                value_bits: body(),
            },
            T::IntParameterChange => Self::IntParameterChange {
                target: target()?,
                parameter: word,
                value: body(),
            },
            T::ModelBypass => Self::ModelBypass {
                target: target()?,
                state: BypassState::from_bits(body())?,
            },
            T::EnableAll => Self::EnableAll {
                core: dsp()?,
                value: word,
            },
            T::DeleteAllModels => Self::DeleteAllModels { core: dsp()? },
            T::BufferAllocate => Self::BufferAllocate {
                core: dsp()?,
                id: body(),
                bytes: read_u32(bytes, 24),
            },
            T::BufferFree => Self::BufferFree {
                core: dsp()?,
                id: body(),
            },
            T::RouteToSidechainBuffer | T::RouteToSidechainBufferSameCore => {
                Self::RouteToSidechainBuffer {
                    core: dsp()?,
                    same_core: message_type == T::RouteToSidechainBufferSameCore,
                    channel: selector,
                    source_slot: word,
                    source_core: read_u32(bytes, 12),
                }
            }
            T::BulkModelBypassSync => Self::BulkModelBypassSync,
            T::ReceiveDataEnd => Self::ReceiveDataEnd {
                target: target()?,
                buffer: word,
                bytes: read_u32(bytes, 12),
            },
            T::MeterRequest => Self::MeterRequest {
                core: any()?,
                slot: selector,
                word,
            },
            T::DiagnosticsRequest => Self::DiagnosticsRequest { core: any()? },
            T::DebugCommsRequest => Self::DebugCommsRequest { core: any()? },
            T::ModuleLatencyRequest => Self::ModuleLatencyRequest { target: target()? },
            T::ActivateSelfTestMode => Self::ActivateSelfTestMode {
                core: any()?,
                enabled: word,
            },
            T::SelfTestDiagnosticRequest => Self::SelfTestDiagnosticRequest {
                core: any()?,
                selector,
            },
            T::TestSoc2Ddr => Self::TestSoc2Ddr { running: word },
            T::TestLinkPort => Self::Text42(TextPrefix::try_from(tail)?),
            T::TestSoc2 => Self::StampKey(TextPrefix::try_from(tail)?),
            T::BulkStatusRead => Self::Blob50(OpaqueBody::try_from(tail)?),
            T::GetInputGateParam => Self::Blob51(OpaqueBody::try_from(tail)?),
            T::TunerMuteMessage => Self::SigningMaterial(TextPrefix::try_from(tail)?),
            T::TunerCalibrate => Self::ClientId(TextPrefix::try_from(tail)?),
            _ => return Err(Error::Invalid("request type")),
        })
    }
}

/// One complete request as wire bytes plus its typed view. The wire bytes
/// can hold an [`OpaqueBody`], so they are zeroed on drop as well.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestFrame {
    request: Request,
    bytes: [u8; MAX_BYTES],
}

impl RequestFrame {
    /// Zero-fills unstored bytes (owned policy; stock reuses a stale buffer).
    pub fn new(request: Request) -> Result<Self, Error> {
        request.validate()?;
        let mut bytes = [0; MAX_BYTES];
        request.write(&mut bytes);
        Ok(Self { request, bytes })
    }

    /// Strict decode: exact per-type size, matching `+8`, and every writer
    /// constant. Unstored bytes are retained.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if !(HEADER_BYTES..=MAX_BYTES).contains(&bytes.len())
            || read_u32(bytes, 8) as usize != bytes.len()
        {
            return Err(Error::Invalid("request size"));
        }
        let message_type = MessageType::try_from(bytes[0])?;
        let mut retained = [0; MAX_BYTES];
        retained[..bytes.len()].copy_from_slice(bytes);
        let request = Request::parse(message_type, bytes, &retained)?;
        request.validate()?;
        if request.total_bytes() != bytes.len() {
            return Err(Error::Invalid("request exact size"));
        }
        let mut checked = retained;
        request.write(&mut checked);
        if checked != retained {
            return Err(Error::Invalid("request writer constants"));
        }
        Ok(Self {
            request,
            bytes: retained,
        })
    }

    pub fn request(&self) -> Request {
        self.request.clone()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.request.total_bytes()]
    }
}

impl Drop for RequestFrame {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

impl fmt::Debug for RequestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Header only: opaque and text bodies stay out of logs.
        f.debug_struct("RequestFrame")
            .field("request", &self.request)
            .field("header", &&self.bytes[..HEADER_BYTES])
            .finish_non_exhaustive()
    }
}

/// Largest DSP-core message: one 2048-byte outgoing slot. Stock bulk writers
/// never fragment (VAs 0x511084, 0x511188, 0x510894).
pub const BULK_MAX_BYTES: usize = 2048;
/// Stock flushes a core's parameter queue when it reaches 169 records
/// (VA 0x517010); `16 + 169 * 12` is the largest fit in one slot.
pub const BULK_PARAMETER_RECORDS_MAX: usize = 169;
pub const BULK_BYPASS_RECORDS_MAX: usize = (BULK_MAX_BYTES - HEADER_BYTES) / 8;
pub const BULK_START_RECORDS_MAX: usize = 30;
/// Every bulk writer and the `0x3e` sync use wait 0 / timeout 100.
pub const BULK_SEND_POLICY: SendPolicy = SendPolicy::new(false, 100);

/// One `0x3d` record `{u32 slot, f32 value}` (VA 0x511188).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BypassRecord {
    pub slot: ModuleSlot,
    pub state: BypassState,
}

/// One `0x1c` record (VA 0x879100), one per occupied slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartRecord {
    /// ModelRepo model ID. Stock replaces IDs 22000..=22004 with a word from
    /// the uploaded-data table filled through the generic sender (VA 0x878aa0).
    pub model: u32,
    pub slot: ModuleSlot,
    /// Compiler table `+0x19c` (VA 0x87a314): the first byte of the plan
    /// node's byte vector (VA 0x87fc2c), stored per slot by VA 0x87b904;
    /// routers get their node's byte. Meaning unresolved.
    pub layout_word: u16,
    /// The model's ModelRepo `plugin_id` attribute (artist pack id, parser VA
    /// 0x4e9ed8 → model `+0xf8`), or -1 when absent or not a grid block.
    pub block_word: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkRequest {
    /// `0x1d`, VA 0x511084. Records `{u32 slot, u32 index, f32 value}`.
    Parameters {
        core: DspCore,
        mode: crate::packet::BulkMode,
        records: Vec<crate::packet::BulkRecord>,
    },
    /// `0x3d`, VA 0x511188. `+4` unstored.
    Bypass {
        core: DspCore,
        records: Vec<BypassRecord>,
    },
    /// `0x1c`, VA 0x510894.
    Start {
        core: DspCore,
        records: Vec<StartRecord>,
    },
}

impl BulkRequest {
    pub fn core(&self) -> DspCore {
        match self {
            Self::Parameters { core, .. }
            | Self::Bypass { core, .. }
            | Self::Start { core, .. } => *core,
        }
    }
}

/// A validated bulk message. Header `+2` is 0 and `+12` is the record count.
#[derive(Clone, PartialEq, Eq)]
pub struct BulkFrame {
    request: BulkRequest,
    bytes: Vec<u8>,
}

impl BulkFrame {
    pub fn new(request: BulkRequest) -> Result<Self, Error> {
        let core = request.core();
        let mut bytes = vec![0; HEADER_BYTES];
        let (message_type, count, word) = match &request {
            BulkRequest::Parameters {
                mode,
                records: list,
                ..
            } => {
                let limits = Self::parameter_limits();
                let bytes =
                    crate::packet::encode_bulk([0; HEADER_BYTES], core, *mode, list, limits)?;
                return Ok(Self { request, bytes });
            }
            BulkRequest::Bypass { records, .. } => {
                if records.len() > BULK_BYPASS_RECORDS_MAX {
                    return Err(Error::Limit("bulk bypass records"));
                }
                for record in records {
                    bytes.extend_from_slice(&u32::from(record.slot.index()).to_le_bytes());
                    bytes.extend_from_slice(&record.state.value_bits().to_le_bytes());
                }
                (MessageType::BulkModelBypass, records.len(), None)
            }
            BulkRequest::Start { records, .. } => {
                if records.len() > BULK_START_RECORDS_MAX {
                    return Err(Error::Limit("bulk start records"));
                }
                for record in records {
                    bytes.extend_from_slice(&record.model.to_le_bytes());
                    bytes.extend_from_slice(&u16::from(record.slot.index()).to_le_bytes());
                    bytes.extend_from_slice(&record.layout_word.to_le_bytes());
                    bytes.extend_from_slice(&record.block_word.to_le_bytes());
                }
                (MessageType::BulkStartModel, records.len(), Some(0))
            }
        };
        bytes[0] = message_type as u8;
        bytes[1] = core as u8;
        if let Some(word) = word {
            bytes[4..8].copy_from_slice(&u32::to_le_bytes(word));
        }
        let total = bytes.len() as u32;
        bytes[8..12].copy_from_slice(&total.to_le_bytes());
        bytes[12..16].copy_from_slice(&(count as u32).to_le_bytes());
        Ok(Self { request, bytes })
    }

    /// Strict decode: type, core, `+2 == 0`, count and total must agree.
    /// `+3` (and `+4` for bypass) are retained unstored bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_BYTES || bytes.len() > BULK_MAX_BYTES || bytes[2] != 0 {
            return Err(Error::Invalid("bulk header"));
        }
        let core = DspCore::try_from(bytes[1])?;
        let records = &bytes[HEADER_BYTES..];
        let count = read_u32(bytes, 12) as usize;
        let width = |width: usize, max: usize| {
            if count > max
                || records.len() != count * width
                || read_u32(bytes, 8) as usize != bytes.len()
            {
                Err(Error::Invalid("bulk count/length"))
            } else {
                Ok(records.chunks_exact(width))
            }
        };
        let request = match MessageType::try_from(bytes[0])? {
            MessageType::BulkParameterChange => {
                let packet =
                    crate::packet::BulkParameterChange::decode(bytes, Self::parameter_limits())?;
                let mode = match packet.mode_word() {
                    8 => crate::packet::BulkMode::Value8,
                    16 => crate::packet::BulkMode::Value16,
                    _ => return Err(Error::Invalid("bulk parameter mode")),
                };
                BulkRequest::Parameters {
                    core,
                    mode,
                    records: packet.records().collect(),
                }
            }
            MessageType::BulkModelBypass => BulkRequest::Bypass {
                core,
                records: width(8, BULK_BYPASS_RECORDS_MAX)?
                    .map(|record| {
                        Ok(BypassRecord {
                            slot: ModuleSlot::try_from(read_u32(record, 0))?,
                            state: BypassState::from_bits(read_u32(record, 4))?,
                        })
                    })
                    .collect::<Result<_, Error>>()?,
            },
            MessageType::BulkStartModel => {
                if read_u32(bytes, 4) != 0 {
                    return Err(Error::Invalid("bulk start word"));
                }
                BulkRequest::Start {
                    core,
                    records: width(12, BULK_START_RECORDS_MAX)?
                        .map(|record| {
                            Ok(StartRecord {
                                model: read_u32(record, 0),
                                slot: ModuleSlot::try_from(u32::from(u16::from_le_bytes([
                                    record[4], record[5],
                                ])))?,
                                layout_word: u16::from_le_bytes([record[6], record[7]]),
                                block_word: read_u32(record, 8) as i32,
                            })
                        })
                        .collect::<Result<_, Error>>()?,
                }
            }
            _ => return Err(Error::Invalid("bulk type")),
        };
        Ok(Self {
            request,
            bytes: bytes.to_vec(),
        })
    }

    pub fn request(&self) -> &BulkRequest {
        &self.request
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn destination(&self) -> CoreId {
        CoreId::Dsp(self.request.core())
    }

    fn parameter_limits() -> crate::CodecLimits {
        crate::CodecLimits {
            max_message_bytes: BULK_MAX_BYTES,
            max_records: BULK_PARAMETER_RECORDS_MAX,
        }
    }
}

impl fmt::Debug for BulkFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BulkFrame")
            .field("request", &self.request)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests;
