//! The preset library.
//!
//! [`stock::StockLibrary`] reads and writes the stock CorOS library in place
//! (setlists, presets, favorites, recents, searches). [`PresetLibrary`] below is
//! the lossless editor over the Tetrad owned SQLite product format.
//!
//! For the owned format, storage opening, path/format gates, limits and the
//! conditional rawdata transaction are tetrad-storage's; this crate adds preset
//! parsing and sessions. Product types are caller-supplied opaque integers.
//! Wire bytes and opaque metadata remain authoritative; saving does not
//! validate signal graphs, migrate presets or update metadata/search indexes.
//!
//! ```no_run
//! use std::path::Path;
//! use tetrad_library::{Access, Limits, PresetLibrary};
//!
//! # fn main() -> tetrad_library::Result<()> {
//! // Created by tetrad-storage; 42 is a caller-selected owned type code.
//! let mut library = PresetLibrary::open_owned(
//!     Path::new("/explicit/owned.sqlite"), 42, Limits::default(), Access::ReadWrite,
//! )?;
//! let mut session = library.load("explicit-product-id")?;
//! session.edit(|document| document.rename("Verse"))?;
//! let outcomes = library.save(std::slice::from_mut(&mut session))?;
//! assert_eq!(outcomes.len(), 1);
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use tetrad_preset::PresetDocument;
use tetrad_storage::{Library, Product, RawDataReplacement};

pub mod sidecar;
pub mod stock;

pub use tetrad_storage::ReplaceOutcome as SaveOutcome;
pub use tetrad_storage::media::{Collection, MediaRoot};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Path/format gates, limits, identity, permission, freshness and SQLite errors.
    #[error(transparent)]
    Storage(#[from] tetrad_storage::Error),
    #[error(transparent)]
    Preset(#[from] tetrad_preset::Error),
    /// IR import or legacy IR decoding (`tetrad_media::user`).
    #[error(transparent)]
    Media(#[from] tetrad_media::user::UserIrError),
    #[error("product type {actual} differs from the caller-selected preset type {expected}")]
    ProductType { actual: i64, expected: i64 },
    #[error("product has NULL preset data: {0}")]
    PresetDataMissing(String),
    #[error("session was loaded by a different library handle")]
    WrongLibrary,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("preset sidecar is not a JSON object")]
    Sidecar,
    /// `0x45fa44`: a cloud product that fails its folder check (vtable `+0x98`).
    #[error("Downloaded product {name} ({id}) is corrupt: {reason}")]
    CorruptDownload {
        name: String,
        id: String,
        reason: String,
    },
    #[error("setlist does not exist: {0}")]
    SetlistMissing(String),
    #[error("setlist already exists: {0}")]
    SetlistExists(String),
    #[error("all 12 user setlists are in use")]
    SetlistsFull,
    #[error("setlist still holds presets: {0}")]
    SetlistNotEmpty(String),
    /// The setlist directory holds entries that no slot of it names, for
    /// example a preset file that another setlist still uses.
    #[error("setlist {setlist} holds other files: {files:?}")]
    SetlistHasFiles { setlist: String, files: Vec<String> },
    #[error("preset file does not exist: {0}")]
    PresetMissing(String),
    #[error("setlist slot {0} is outside 0..256")]
    SlotRange(usize),
    #[error("setlist slot {0} is occupied")]
    SlotOccupied(usize),
    #[error("setlist slot {0} is empty")]
    SlotEmpty(usize),
    #[error("setlist slot {0} changed after the download started")]
    SlotChanged(usize),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// The library has no factory root, so it has no factory presets (review r35 G5).
    #[error("unsupported: no factory root configured")]
    NoFactoryRoot,
    /// A converted legacy capture does not read back as its old file (`stock::conversion`).
    #[error("legacy capture conversion: {0}")]
    LegacyCapture(String),
    /// Another item holds the name (capture edit `0x6901c8`: "Name already exists").
    #[error("{0} already exists. Please choose a different name.")]
    NameTaken(String),
    /// A two-setlist change stopped and could not be undone: `orphans` are preset files that
    /// no slot names now (review r30 C3).
    #[error("{error}; these presets are in no setlist now: {}", orphans.join(", "))]
    SwapIncomplete {
        error: Box<Error>,
        orphans: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Its `max_batch_operations` also bounds sessions per save.
    pub storage: tetrad_storage::Limits,
    /// Bounds the combined candidate wire bytes retained until commit.
    pub max_save_bytes: usize,
    pub preset: tetrad_preset::Limits,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            storage: tetrad_storage::Limits {
                max_batch_operations: 128,
                ..Default::default()
            },
            max_save_bytes: 64 * 1024 * 1024,
            preset: tetrad_preset::Limits::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

/// Opens existing owned storage; creation/import/folder management stay in
/// tetrad-storage. The caller must control the database's parent directory.
pub struct PresetLibrary {
    storage: Library,
    limits: Limits,
    access: Access,
    preset_type: i64,
    origin: Arc<()>,
}

impl PresetLibrary {
    pub fn open_owned(
        path: &Path,
        preset_type: i64,
        limits: Limits,
        access: Access,
    ) -> Result<Self> {
        if limits.max_save_bytes == 0 {
            return Err(tetrad_storage::Error::Limit.into());
        }
        let storage = Library::open_owned(path, limits.storage, access == Access::ReadOnly)?;
        Ok(Self {
            storage,
            limits,
            access,
            preset_type,
            origin: Arc::new(()),
        })
    }

    /// Parses only rawdata. NULL is not an empty protobuf, and metadata is never
    /// interpreted. Diagnostics remain an explicit PresetDocument operation.
    pub fn load(&self, id: &str) -> Result<PresetSession> {
        let original = self
            .storage
            .product(id)?
            .ok_or_else(|| tetrad_storage::Error::ProductMissing(id.to_owned()))?;
        if original.product_type != self.preset_type {
            return Err(Error::ProductType {
                actual: original.product_type,
                expected: self.preset_type,
            });
        }
        let raw_data = original
            .data
            .raw_data
            .as_deref()
            .ok_or_else(|| Error::PresetDataMissing(id.to_owned()))?;
        let document = PresetDocument::parse(raw_data, self.limits.preset)?;
        Ok(PresetSession {
            original,
            document,
            limits: self.limits,
            origin: Arc::clone(&self.origin),
        })
    }

    /// Saves one or more sessions through tetrad-storage's conditional rawdata
    /// replacement: one immediate transaction, exact loaded-snapshot checks and
    /// no UPDATE for no-op saves.
    ///
    /// Success advances each session's baseline only after commit. On any error,
    /// both the database and session baselines stay unchanged, and edits remain
    /// available for export/retry. Folder memberships are never replaced.
    pub fn save(&mut self, sessions: &mut [PresetSession]) -> Result<Vec<SaveOutcome>> {
        // Refuse before serializing a batch that storage would refuse anyway.
        if self.access == Access::ReadOnly {
            return Err(tetrad_storage::Error::ReadOnly.into());
        }
        if sessions.len() > self.limits.storage.max_batch_operations {
            return Err(tetrad_storage::Error::Limit.into());
        }
        let mut candidates = Vec::with_capacity(sessions.len());
        let mut bytes = 0_usize;
        for session in sessions.iter() {
            if !Arc::ptr_eq(&self.origin, &session.origin) {
                return Err(Error::WrongLibrary);
            }
            let candidate = session.document.to_bytes();
            bytes = bytes
                .checked_add(candidate.len())
                .ok_or(tetrad_storage::Error::Limit)?;
            if bytes > self.limits.max_save_bytes {
                return Err(tetrad_storage::Error::Limit.into());
            }
            candidates.push(candidate);
        }
        let replacements: Vec<_> = sessions
            .iter()
            .zip(&candidates)
            .map(|(session, raw_data)| RawDataReplacement {
                expected: &session.original,
                raw_data,
            })
            .collect();
        let outcomes = self.storage.replace_raw_data(&replacements)?;
        for (session, candidate) in sessions.iter_mut().zip(candidates) {
            session.original.data.raw_data = Some(candidate);
        }
        Ok(outcomes)
    }
}

/// A handle-bound optimistic snapshot with a staged lossless document.
/// Dropping a session has no database effect.
pub struct PresetSession {
    original: Product,
    document: PresetDocument,
    limits: Limits,
    origin: Arc<()>,
}

impl PresetSession {
    pub fn id(&self) -> &str {
        &self.original.id
    }

    pub fn document(&self) -> &PresetDocument {
        &self.document
    }

    pub fn metadata(&self) -> Option<&[u8]> {
        self.original.data.metadata.as_deref()
    }

    pub fn read_only(&self) -> bool {
        self.original.read_only
    }

    /// Stages a group of edits atomically. A failed closure or bounded reparse
    /// retains the previous document. No semantic migration is implicit.
    pub fn edit(
        &mut self,
        edits: impl FnOnce(&mut PresetDocument) -> tetrad_preset::Result<()>,
    ) -> Result<()> {
        if self.original.read_only {
            return Err(tetrad_storage::Error::ReadOnly.into());
        }
        let mut candidate = self.document.clone();
        edits(&mut candidate)?;
        let bytes = candidate.to_bytes();
        self.limits
            .storage
            .check_product_data(Some(&bytes), self.metadata())?;
        self.document = PresetDocument::parse(&bytes, self.limits.preset)?;
        Ok(())
    }
}

#[cfg(test)]
mod corpus_tests;
#[cfg(test)]
mod stock_tests;
#[cfg(test)]
mod tests;
