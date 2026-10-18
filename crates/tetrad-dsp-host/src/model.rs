//! Type-7 direct model copies into an explicitly supplied, core-specific window.
//! No mapping, relocation, image decoding or model activation is performed.

use crate::{MemoryError, SharedMemory};
use tetrad_dsp_protocol::{
    DeploymentTarget, DspCore,
    address::{Address, Bytes},
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModelError {
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error("direct model loading is not recovered for core {0:?}")]
    UnsupportedCore(DspCore),
    #[error("target core {requested:?} does not match window core {expected:?}")]
    CoreMismatch {
        expected: DspCore,
        requested: DspCore,
    },
    #[error("model window or destination interval exceeds 32-bit byte addressing")]
    AddressOverflow,
    #[error("empty model copy is not admitted")]
    EmptyModel,
    #[error("destination {address:#x}+{bytes} is outside window {base:#x}+{window}")]
    OutOfWindow {
        address: u32,
        bytes: usize,
        base: u32,
        window: usize,
    },
    #[error("model window is faulted after an ambiguous backend write")]
    Faulted,
}

/// Records a copy and publication barrier, NOT installation or execution.
/// The slot is caller metadata; type 7 does not publish it through a ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelWriteReceipt {
    pub target: DeploymentTarget,
    pub destination: Address<Bytes>,
    pub byte_length: usize,
}

/// Bounded replacement for the direct copy branch at ZenUI VA 0x51965c and
/// `externalTransfer` VA 0x529d88. The caller supplies the already mapped window
/// and byte-address origin, and optionally one external region ([`Self::with_external`]);
/// no bank aliases or arbitrary-address fallback are guessed.
pub struct ModelMemory<M> {
    core: DspCore,
    base: Address<Bytes>,
    memory: M,
    /// A second region for sections outside the L1 window (stock: a `/dev/mem` map of the
    /// physical address, 0x529e64).
    external: Option<(Address<Bytes>, M)>,
    faulted: bool,
}

impl<M: SharedMemory> ModelMemory<M> {
    pub fn new(core: DspCore, base: Address<Bytes>, memory: M) -> Result<Self, ModelError> {
        if !matches!(core, DspCore::Core0 | DspCore::Core1) {
            return Err(ModelError::UnsupportedCore(core));
        }
        let bytes = u64::try_from(memory.len()).map_err(|_| ModelError::AddressOverflow)?;
        if bytes > (1_u64 << 32) - u64::from(base.value()) {
            return Err(ModelError::AddressOverflow);
        }
        Ok(Self {
            core,
            base,
            memory,
            external: None,
            faulted: false,
        })
    }

    /// Adds the external region `base..base + memory.len()`: a copy that does not fit the L1
    /// window goes there when it fits it. Stock `externalTransfer` maps any physical address
    /// below a limit (0x529e64..0x529edc); the platform chooses a bounded region instead.
    pub fn with_external(mut self, base: Address<Bytes>, memory: M) -> Result<Self, ModelError> {
        let bytes = u64::try_from(memory.len()).map_err(|_| ModelError::AddressOverflow)?;
        if bytes > (1_u64 << 32) - u64::from(base.value()) {
            return Err(ModelError::AddressOverflow);
        }
        self.external = Some((base, memory));
        Ok(self)
    }

    pub fn core(&self) -> DspCore {
        self.core
    }

    pub fn base(&self) -> Address<Bytes> {
        self.base
    }

    pub fn memory(&self) -> &M {
        &self.memory
    }

    /// Does not revert any successful or partially failed copy.
    pub fn into_memory(self) -> M {
        self.memory
    }

    /// The L1 window memory and the external region, for a caller that rewraps both.
    pub fn into_parts(self) -> (M, Option<(Address<Bytes>, M)>) {
        (self.memory, self.external)
    }

    pub fn is_faulted(&self) -> bool {
        self.faulted
    }

    /// Copies exact bytes, with no ring header or notification. This is an owned
    /// bounds policy, not proof of legal stock placement or a complete LoadModel.
    pub fn load_model(
        &mut self,
        target: DeploymentTarget,
        destination: Address<Bytes>,
        bytes: &[u8],
    ) -> Result<ModelWriteReceipt, ModelError> {
        if self.faulted {
            return Err(ModelError::Faulted);
        }
        if target.core != self.core {
            return Err(ModelError::CoreMismatch {
                expected: self.core,
                requested: target.core,
            });
        }
        if bytes.is_empty() {
            return Err(ModelError::EmptyModel);
        }
        let length = u64::try_from(bytes.len()).map_err(|_| ModelError::AddressOverflow)?;
        if length > (1_u64 << 32) - u64::from(destination.value()) {
            return Err(ModelError::AddressOverflow);
        }
        let inside = |base: Address<Bytes>, window: usize| {
            let offset = destination.value().checked_sub(base.value())? as usize;
            offset
                .checked_add(bytes.len())
                .is_some_and(|end| end <= window)
                .then_some(offset)
        };
        let (offset, memory) = match inside(self.base, self.memory.len()) {
            Some(offset) => (offset, &mut self.memory),
            None => self
                .external
                .as_mut()
                .and_then(|(base, memory)| Some((inside(*base, memory.len())?, memory)))
                .ok_or(ModelError::OutOfWindow {
                    address: destination.value(),
                    bytes: bytes.len(),
                    base: self.base.value(),
                    window: self.memory.len(),
                })?,
        };
        if let Err(error) = memory.write(offset, bytes) {
            self.faulted = true;
            return Err(error.into());
        }
        memory.publish_barrier();
        Ok(ModelWriteReceipt {
            target,
            destination,
            byte_length: bytes.len(),
        })
    }
}

/// Direct-copy windows for transport cores 0 and 1. Stock `externalTransfer`
/// uses physical bases `0x28000000` and `0x28800000` with pre-mapped ranges
/// `+0x240000..0x270000`, `+0x2c0000..0x2f0000`, `+0x300000..0x320000` and
/// `+0x380000..0x3a0000` (VA 0x529d88). The platform maps them; addresses
/// here are the window-relative byte addresses that stock passes as `+4`.
pub struct CoreWindows<M> {
    core0: ModelMemory<M>,
    core1: ModelMemory<M>,
}

impl<M: SharedMemory> CoreWindows<M> {
    pub fn new(core0: ModelMemory<M>, core1: ModelMemory<M>) -> Result<Self, ModelError> {
        for (window, expected) in [(&core0, DspCore::Core0), (&core1, DspCore::Core1)] {
            if window.core != expected {
                return Err(ModelError::CoreMismatch {
                    expected,
                    requested: window.core,
                });
            }
        }
        Ok(Self { core0, core1 })
    }

    /// `None` for cores 2/3, which have no direct window.
    pub fn get_mut(&mut self, core: DspCore) -> Option<&mut ModelMemory<M>> {
        match core {
            DspCore::Core0 => Some(&mut self.core0),
            DspCore::Core1 => Some(&mut self.core1),
            DspCore::Core2 | DspCore::Core3 => None,
        }
    }

    pub fn into_inner(self) -> (ModelMemory<M>, ModelMemory<M>) {
        (self.core0, self.core1)
    }
}

#[cfg(test)]
mod tests;
