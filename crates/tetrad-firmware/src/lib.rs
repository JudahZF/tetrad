//! The stock CorOS builds that Tetrad knows, and the values that belong to each build.
//!
//! Tetrad reads stock data at run time from the unit's own `/opt/neuraldsp/ZenUI` and
//! `/usr/bin/loadsharc_SC589`, by address. These addresses belong to one build. This crate
//! names each known build ([`Firmware`]), identifies it by the SHA-256 of both files
//! ([`Firmware::identify`]) and holds its [`Profile`]: the version string and every
//! build-specific address.
//!
//! Site VAs: the code names each ZenUI datum by its CorOS 4.1.0 VA. That VA is the stable id
//! of the read site. A profile maps site VAs to the VAs of its own build ([`ZenUiVas`]); for
//! 4.1.0 the map is the identity. A new build is data in this crate only: no driver file
//! changes.
//!
//! The running build: `tetrad run` identifies the files at start-up and calls [`select`].
//! Readers in other crates take values from [`running`]. Without a selection (host tools,
//! tests) the running build is [`Firmware::DEFAULT`].
//!
//! To add a build, see "Add a CorOS build" in `docs/architecture.md`.

use std::fmt;
use std::sync::OnceLock;

mod v4_1_0;
mod v4_1_1;

/// A stock CorOS build that Tetrad knows by its file hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Firmware {
    V4_1_0,
    /// Same driver data as 4.1.0 at moved VAs (`v4_1_1.rs`).
    V4_1_1,
}

/// SHA-256 of the two stock files that hold the build-specific addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// `/opt/neuraldsp/ZenUI`.
    pub zenui_sha256: [u8; 32],
    /// `/usr/bin/loadsharc_SC589`.
    pub loadsharc_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FirmwareError {
    #[error("stock ZenUI is not a known CorOS build")]
    UnknownZenUi,
    #[error("loadsharc_SC589 is not the {0} build")]
    LoadsharcMismatch(Firmware),
    /// The build is known, but some of its values are not. Tetrad must not run on it.
    #[error("{firmware} support is incomplete; open slots: {}", missing.join(", "))]
    Incomplete {
        firmware: Firmware,
        missing: Vec<&'static str>,
    },
    #[error("{running} is already the running build; cannot select {requested}")]
    AlreadySelected {
        running: Firmware,
        requested: Firmware,
    },
}

impl Firmware {
    pub const ALL: [Self; 2] = [Self::V4_1_0, Self::V4_1_1];
    /// The running build of host tools and tests, and of the device before [`select`].
    pub const DEFAULT: Self = Self::V4_1_0;

    pub const fn identity(self) -> &'static Identity {
        match self {
            Self::V4_1_0 => &v4_1_0::IDENTITY,
            Self::V4_1_1 => &v4_1_1::IDENTITY,
        }
    }

    /// The build of a ZenUI and a loadsharc, by their SHA-256 digests. The ZenUI selects the
    /// build; the loadsharc must be from the same build.
    pub fn identify(zenui_sha256: &[u8], loadsharc_sha256: &[u8]) -> Result<Self, FirmwareError> {
        let firmware = Self::ALL
            .into_iter()
            .find(|firmware| firmware.identity().zenui_sha256 == zenui_sha256)
            .ok_or(FirmwareError::UnknownZenUi)?;
        if firmware.identity().loadsharc_sha256 != loadsharc_sha256 {
            return Err(FirmwareError::LoadsharcMismatch(firmware));
        }
        Ok(firmware)
    }

    /// The complete build of a CorOS version string, as a CorOS update offer gives it (the
    /// first line of the release notes, for example `4.1.1`). `None`: Tetrad cannot run on
    /// that version yet, so stock ZenUI starts after it is installed. This is the one list of
    /// the versions that Tetrad supports.
    pub fn by_version(version: &str) -> Option<Self> {
        let version = version.trim();
        Self::ALL.into_iter().find(|firmware| {
            firmware
                .profile()
                .is_ok_and(|profile| profile.version == version)
        })
    }

    /// The values of this build. Fails while a slot of the build is open.
    pub fn profile(self) -> Result<&'static Profile, FirmwareError> {
        match self {
            Self::V4_1_0 => Ok(&V4_1_0),
            Self::V4_1_1 => V4_1_1.as_ref().ok_or_else(|| FirmwareError::Incomplete {
                firmware: self,
                missing: v4_1_1::DRAFT.missing(),
            }),
        }
    }
}

impl fmt::Display for Firmware {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::V4_1_0 => "CorOS 4.1.0",
            Self::V4_1_1 => "CorOS 4.1.1",
        })
    }
}

/// Every build-specific value of one complete build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub firmware: Firmware,
    /// The CorOS version string, as stock writes and reports it: preset stamps
    /// (`corosVersion`, `modified_version`, backup `creator_version`), Cortex Control
    /// (`coros_version`, version reply `zenos_git_hash`) and the cloud
    /// `X-Cloud-Api-Hardware-Source-Version` header.
    pub version: &'static str,
    pub zenui: ZenUi,
    pub loadsharc: LoadsharcLayout,
}

/// Build-specific places in `/opt/neuraldsp/ZenUI`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZenUi {
    /// Where the site VAs of the driver payloads, the table readers and the cipher seed
    /// (tetrad-dsp-host `zenui_reads`) are in this build.
    pub vas: ZenUiVas,
    /// File offsets that tetrad-media reads without the ELF headers.
    pub media: MediaOffsets,
}

/// The map from site VAs (CorOS 4.1.0 ZenUI VAs) to the VAs of one build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZenUiVas {
    /// The build of the site VAs (CorOS 4.1.0): every VA maps to itself.
    Canonical,
    /// Moved ranges, sorted by [`Relocation::from`], without overlap. A site VA outside
    /// every range has no VA in this build.
    Relocated(&'static [Relocation]),
}

/// The site VAs `from..from + len` are at `to..to + len` in this build. One entry can move
/// a whole region (for example a shifted `.rodata` block) or one read site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relocation {
    pub from: u32,
    pub len: u32,
    pub to: u32,
}

impl ZenUiVas {
    /// The VA in this build of the site range `va..va + len`. `None` when no single
    /// relocation holds the whole range.
    pub fn relocate(self, va: u32, len: u32) -> Option<u32> {
        let table = match self {
            Self::Canonical => return Some(va),
            Self::Relocated(table) => table,
        };
        let entry = table[..table.partition_point(|entry| entry.from <= va)].last()?;
        let offset = va - entry.from;
        (u64::from(offset) + u64::from(len) <= u64::from(entry.len))
            .then(|| entry.to.checked_add(offset))
            .flatten()
    }
}

/// File offsets of stock tables in the ZenUI image (in 4.1.0: VA − `0x8000`, the first
/// `PT_LOAD` segment). tetrad-dsp-host reads the same cab and IR Loader tables by site VA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaOffsets {
    /// 57-byte NDSP seed of the device and capture ciphers (`FactoryIrSeed`).
    pub cipher_seed: usize,
    /// IR Loader reverb table (`ReverbTable::from_stock_zenui`).
    pub reverb_table: usize,
    /// Cab selector default IR.
    pub default_ir: usize,
    /// The three Petrucci room IRs, one after the other.
    pub petrucci_rooms: usize,
    /// Mono cab reverb table.
    pub mono_reverb: usize,
    /// Stereo cab reverb table.
    pub stereo_reverb: usize,
}

/// Build-specific values of the booter in `/usr/bin/loadsharc_SC589`. The SC589 windows and
/// core-local bases are hardware facts and stay in tetrad-loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadsharcLayout {
    pub core1: BooterCore,
    pub core2: BooterCore,
}

/// Per-core constants of loadsharc `main` (4.1.0 `0x87e4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BooterCore {
    /// ELF VA of the booter config words and section table.
    pub config_va: u32,
    /// L2 address of the application-phase mailbox (four words).
    pub app_mailbox: u32,
}

/// The values of a build that is not complete yet. Each `None` is an open slot; the build
/// is refused until every slot is filled.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Draft {
    pub version: Option<&'static str>,
    pub zenui_vas: Option<&'static [Relocation]>,
    pub media: Option<MediaOffsets>,
    pub loadsharc: Option<LoadsharcLayout>,
}

impl Draft {
    const fn profile(&self, firmware: Firmware) -> Option<Profile> {
        match (self.version, self.zenui_vas, self.media, self.loadsharc) {
            (Some(version), Some(vas), Some(media), Some(loadsharc)) => Some(Profile {
                firmware,
                version,
                zenui: ZenUi {
                    vas: ZenUiVas::Relocated(vas),
                    media,
                },
                loadsharc,
            }),
            _ => None,
        }
    }

    fn missing(&self) -> Vec<&'static str> {
        [
            ("version", self.version.is_none()),
            ("zenui_vas", self.zenui_vas.is_none()),
            ("media", self.media.is_none()),
            ("loadsharc", self.loadsharc.is_none()),
        ]
        .into_iter()
        .filter_map(|(slot, open)| open.then_some(slot))
        .collect()
    }
}

/// A SHA-256 digest from its 64 hex digits (the `sha256sum` output).
const fn sha256(hex: &str) -> [u8; 32] {
    const fn nibble(digit: u8) -> u8 {
        match digit {
            b'0'..=b'9' => digit - b'0',
            b'a'..=b'f' => digit - b'a' + 10,
            _ => panic!("SHA-256 hex must be lowercase hex digits"),
        }
    }
    let hex = hex.as_bytes();
    assert!(hex.len() == 64, "SHA-256 hex must have 64 digits");
    let mut digest = [0; 32];
    let mut index = 0;
    while index < 32 {
        digest[index] = nibble(hex[2 * index]) << 4 | nibble(hex[2 * index + 1]);
        index += 1;
    }
    digest
}

static V4_1_0: Profile = v4_1_0::PROFILE;
static V4_1_1: Option<Profile> = v4_1_1::DRAFT.profile(Firmware::V4_1_1);
static RUNNING: OnceLock<&'static Profile> = OnceLock::new();

/// Makes `profile` the running build. Call it once at start-up, before any reader runs, and
/// only after tetrad-dsp-host `check_zenui_vas` passed for it. Fails when [`running`] already
/// gave another build.
pub fn select(profile: &'static Profile) -> Result<(), FirmwareError> {
    let running = RUNNING.get_or_init(|| profile);
    if running.firmware == profile.firmware {
        Ok(())
    } else {
        Err(FirmwareError::AlreadySelected {
            running: running.firmware,
            requested: profile.firmware,
        })
    }
}

/// The running build: the one [`select`] chose, else [`Firmware::DEFAULT`]. The first call
/// fixes the build for the process.
pub fn running() -> &'static Profile {
    RUNNING.get_or_init(|| &V4_1_0)
}

#[cfg(test)]
mod tests;
