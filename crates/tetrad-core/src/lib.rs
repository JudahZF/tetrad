//! Recovered behavioral primitives, separate from lossless preset storage.
//!
//! These contracts are static recovery plus synthetic reference checks. They
//! are not a complete importer, graph compiler or proof of SHARC/ARM rounding.

mod migration;
mod parameter;
mod parameter_import;
mod splitter;

pub mod comparison;
pub mod conversion;
pub mod performance;
pub mod workflow;

#[cfg(test)]
mod import_tests;

pub use migration::{MigrationFlags, SourceLayout};
pub use parameter::PowerMapping;
pub use parameter_import::{
    DefaultReason, ParameterControls, ParameterImportPlan, ParameterValue,
    ResolvedParameterMetadata, RuntimeParameter, SceneValueSource, StoredParameterScenes,
};
pub use splitter::{LegacySplitterPlan, ResolvedSplitterType, SplitterType};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("value must be finite")]
    NonFinite,
    #[error("power mapping needs finite min < max and exponent > 0")]
    InvalidMapping,
    #[error("graph position ({row}, {column}) is outside the 4 by 8 stored layout")]
    Position { row: usize, column: usize },
    #[error("mixer column {0} is outside the eight stored slots")]
    Mixer(usize),
    #[error("version metadata must contain a complete major/minor/patch triple")]
    PartialVersion,
    #[error("sidechain source count or decoded index exceeds the recovered graph bounds")]
    SidechainRange,
    #[error("parameter count {0} exceeds the recovered signed positional range")]
    ParameterCount(usize),
    #[error("destination parameter position {position} is outside count {count}")]
    ParameterPosition { position: usize, count: usize },
    #[error("stored parameter count is {actual}, but the import plan requires {expected}")]
    StoredParameterCount { expected: usize, actual: usize },
    #[error(
        "destination parameter {destination} requires a last stored parameter, but none exists"
    )]
    MissingLastParameter { destination: usize },
    #[error("source parameter position {position} is unavailable in count {count}")]
    SourceParameterPosition { position: usize, count: usize },
    #[error("model ID {0} has no recovered legacy splitter conversion")]
    UnsupportedSplitter(u32),
    #[error(
        "caller-resolved TYPE encoding is for {actual:?}, but conversion requires {expected:?}"
    )]
    SplitterTypeEncoding {
        expected: SplitterType,
        actual: SplitterType,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Stored graph rows in the 4 by 8 layout. The single owner for every crate.
pub const GRID_ROWS: usize = 4;

/// Stored identity, not a checksum of the serialized preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetId(pub u32);

impl PresetId {
    /// Applies the recovered identifier function. Callers select the identifier;
    /// a current file path is not necessarily the stock identity source.
    pub fn from_identifier(identifier: &str) -> Self {
        let mut hash = 0x811c_9dc5_u32;
        for byte in identifier.as_bytes() {
            hash ^= u32::from(*byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        if hash < 8192 {
            hash += 8192;
        }
        Self(hash)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const LEGACY: Self = Self {
        major: 1,
        minor: 0,
        patch: 3,
    };

    /// Absence and an explicit zero version are different states.
    pub fn from_fields(
        major: Option<u32>,
        minor: Option<u32>,
        patch: Option<u32>,
    ) -> Result<Option<Self>> {
        match (major, minor, patch) {
            (None, None, None) => Ok(None),
            (Some(major), Some(minor), Some(patch)) => Ok(Some(Self {
                major,
                minor,
                patch,
            })),
            _ => Err(Error::PartialVersion),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportVersions {
    pub created: Version,
    pub modified: Version,
    pub oldest_compatible: Version,
}

impl ImportVersions {
    /// The reviewed file-import path defaults all three versions together when
    /// created metadata is absent. Present created metadata needs complete peers.
    pub fn resolve(
        created: Option<Version>,
        modified: Option<Version>,
        oldest_compatible: Option<Version>,
    ) -> Result<Self> {
        match created {
            None => Ok(Self {
                created: Version::LEGACY,
                modified: Version::LEGACY,
                oldest_compatible: Version::LEGACY,
            }),
            Some(created) => Ok(Self {
                created,
                modified: modified.ok_or(Error::PartialVersion)?,
                oldest_compatible: oldest_compatible.ok_or(Error::PartialVersion)?,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matches_recovered_path_vector_without_claiming_universal_path_ids() {
        assert_eq!(
            PresetId::from_identifier("/opt/neuraldsp/Factory Library/12 String Bass.pb"),
            PresetId(0xefaf_eb7a)
        );
        assert_eq!(PresetId::from_identifier(""), PresetId(0x811c_9dc5));
        assert_eq!(PresetId::from_identifier("hello"), PresetId(0x4f9f_2cab));
    }

    #[test]
    fn version_absence_defaults_all_peers_but_partial_present_metadata_fails() {
        let newer = Version {
            major: 3,
            minor: 1,
            patch: 0,
        };
        let versions = ImportVersions::resolve(None, Some(newer), Some(newer)).unwrap();
        assert_eq!(versions.created, Version::LEGACY);
        assert_eq!(versions.modified, Version::LEGACY);
        assert_eq!(versions.oldest_compatible, Version::LEGACY);
        assert_eq!(
            Version::from_fields(None, Some(0), None),
            Err(Error::PartialVersion)
        );
        assert!(ImportVersions::resolve(Some(newer), None, Some(newer)).is_err());
        assert_eq!(
            Version::from_fields(Some(0), Some(0), Some(0)).unwrap(),
            Some(Version {
                major: 0,
                minor: 0,
                patch: 0
            })
        );
    }
}
