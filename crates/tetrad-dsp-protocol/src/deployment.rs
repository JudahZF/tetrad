use crate::{DeploymentTarget, DspCore, MessageType};

pub const LOAD_MODEL_TYPE: u8 = MessageType::LoadModel as u8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredLoadPath {
    /// Type 7 bypasses the ordinary outgoing ring on cores 0/1.
    DirectMemory,
    /// VA 0x51965c falls through to generic framing on cores 2/3. The
    /// executor is `tetrad_dsp_host::session::DspSession::load_section`.
    GenericRing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedLoadModelPlan {
    pub target: DeploymentTarget,
    pub byte_length: usize,
    pub required_path: RequiredLoadPath,
}

/// Describes the stock path for one deployment request. Placement addresses
/// come from `tetrad_dsp_host::blob::place`, not from this plan.
pub fn plan_load_model(target: DeploymentTarget, byte_length: usize) -> UnsupportedLoadModelPlan {
    let required_path = match target.core {
        DspCore::Core0 | DspCore::Core1 => RequiredLoadPath::DirectMemory,
        DspCore::Core2 | DspCore::Core3 => RequiredLoadPath::GenericRing,
    };
    UnsupportedLoadModelPlan {
        target,
        byte_length,
        required_path,
    }
}
