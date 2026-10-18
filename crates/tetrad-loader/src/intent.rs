//! `/dev/corectrl` scalar requests as data (loadsharc `0x8ac0`..`0x8bc8`). `plan_core_pair`
//! is the older inert pairing; executable boot sequences are in [`crate::plan`].
use crate::{Error, RawLdr, Signature};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LoaderCore {
    Core1 = 1,
    Core2 = 2,
}

impl LoaderCore {
    pub const fn signature(self) -> Signature {
        match self {
            Self::Core1 => Signature::Core1,
            Self::Core2 => Signature::Core2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreAction {
    Start,
    Stop,
    SetEntry(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalarRequest {
    pub request: u32,
    pub argument: u32,
}

pub fn core_control_intent(core: LoaderCore, action: CoreAction) -> ScalarRequest {
    match action {
        CoreAction::Start => ScalarRequest {
            request: 0x6200,
            argument: core as u32,
        },
        CoreAction::Stop => ScalarRequest {
            request: 0x6201,
            argument: core as u32,
        },
        CoreAction::SetEntry(entry) => ScalarRequest {
            request: match core {
                LoaderCore::Core1 => 0x6d11,
                LoaderCore::Core2 => 0x6d12,
            },
            argument: entry,
        },
    }
}

#[derive(Debug)]
pub struct CoreLoadIntent<'image, 'bytes> {
    pub core: LoaderCore,
    pub image: &'image RawLdr<'bytes>,
    pub stop: ScalarRequest,
    pub set_entry: ScalarRequest,
    pub start: ScalarRequest,
}

#[derive(Debug)]
pub struct LoaderIntent<'image, 'bytes> {
    /// Core 2 before core 1. Per-core data is not an executable boot sequence.
    pub loads: [CoreLoadIntent<'image, 'bytes>; 2],
}

pub fn plan_core_pair<'image, 'bytes>(
    core1: &'image RawLdr<'bytes>,
    entry1: u32,
    core2: &'image RawLdr<'bytes>,
    entry2: u32,
) -> Result<LoaderIntent<'image, 'bytes>, Error> {
    core1.validate_signature(Signature::Core1)?;
    core2.validate_signature(Signature::Core2)?;
    let loads = [
        (LoaderCore::Core2, core2, entry2),
        (LoaderCore::Core1, core1, entry1),
    ]
    .map(|(core, image, entry)| CoreLoadIntent {
        core,
        image,
        stop: core_control_intent(core, CoreAction::Stop),
        set_entry: core_control_intent(core, CoreAction::SetEntry(entry)),
        start: core_control_intent(core, CoreAction::Start),
    });
    Ok(LoaderIntent { loads })
}
