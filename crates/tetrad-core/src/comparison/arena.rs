//! Immutable ownership of already-admitted local comparison projections.
//!
//! This prototype stores the actual `tetrad_core::comparison::AdmittedSnapshot`
//! type. It cannot establish complete runtime capture, producer coherence,
//! constructor provenance, Qt bindings, lifecycle facts or dirty state.
//! Logical limits do not bound spare capacity, allocation overhead or handle clones.

#![forbid(unsafe_code)]
#![deny(warnings)]

use crate::comparison::{AdmittedSnapshot, Comparison, ComparisonPolicy, Unsupported};
use std::{error::Error, fmt, sync::Arc};

/// Private prototype limit, unrelated to stock firmware or timing requirements.
pub const MAX_CAPTURES: usize = 16;

/// Identity of one insertion, never semantic equality of its content.
/// Clones retain the same identity witness, even after the arena is dropped.
#[derive(Debug, Clone)]
pub struct ComparisonHandle {
    arena: Arc<()>,
    slot: usize,
}

impl PartialEq for ComparisonHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.arena, &other.arena) && self.slot == other.slot
    }
}

impl Eq for ComparisonHandle {}

/// Append-only owned projections. Each default construction has a fresh witness.
/// The arena has no clone, replacement, deletion, eviction or mutable getter.
#[derive(Debug)]
pub struct ComparisonArena {
    identity: Arc<()>,
    slots: Vec<AdmittedSnapshot>,
}

impl Default for ComparisonArena {
    fn default() -> Self {
        Self {
            identity: Arc::new(()),
            slots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertError {
    CapacityReached,
}

impl fmt::Display for InsertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "comparison arena capacity of {MAX_CAPTURES} reached")
    }
}

impl Error for InsertError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleError {
    ForeignArena,
    /// Defensive lookup failure; safe public construction cannot produce this.
    MissingSlot,
}

impl fmt::Display for HandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignArena => f.write_str("handle belongs to another comparison arena"),
            Self::MissingSlot => f.write_str("handle has no retained slot"),
        }
    }
}

impl Error for HandleError {}

/// Invalid identities are separate from the comparator's exact refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareError {
    LhsHandle(HandleError),
    RhsHandle(HandleError),
    Unsupported(Unsupported),
}

impl fmt::Display for CompareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LhsHandle(error) => write!(f, "lhs handle: {error}"),
            Self::RhsHandle(error) => write!(f, "rhs handle: {error}"),
            Self::Unsupported(error) => write!(f, "{error}"),
        }
    }
}

impl Error for CompareError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::LhsHandle(error) | Self::RhsHandle(error) => Some(error),
            Self::Unsupported(error) => Some(error),
        }
    }
}

impl ComparisonArena {
    /// Moves a previously admitted projection into a new immutable identity.
    /// At capacity the input is dropped and all existing entries remain intact.
    /// Admission and external capture facts remain the caller's responsibility.
    pub fn insert(&mut self, preset: AdmittedSnapshot) -> Result<ComparisonHandle, InsertError> {
        if self.slots.len() >= MAX_CAPTURES {
            return Err(InsertError::CapacityReached);
        }
        let slot = self.slots.len();
        self.slots.push(preset);
        Ok(ComparisonHandle {
            arena: Arc::clone(&self.identity),
            slot,
        })
    }

    /// Checks the witness before looking up the slot; returns only a shared ref.
    pub fn get(&self, handle: &ComparisonHandle) -> Result<&AdmittedSnapshot, HandleError> {
        if !Arc::ptr_eq(&self.identity, &handle.arena) {
            return Err(HandleError::ForeignArena);
        }
        self.slots.get(handle.slot).ok_or(HandleError::MissingSlot)
    }

    /// Resolves lhs, then rhs, then invokes the actual directional comparator.
    /// Even identical handles undergo comparison with the explicit policy.
    /// No lifecycle metadata, admission, wire comparison or dirty effect is made.
    pub fn compare(
        &self,
        lhs: &ComparisonHandle,
        rhs: &ComparisonHandle,
        policy: ComparisonPolicy,
    ) -> Result<Comparison, CompareError> {
        let lhs = self.get(lhs).map_err(CompareError::LhsHandle)?;
        let rhs = self.get(rhs).map_err(CompareError::RhsHandle)?;
        crate::comparison::compare(lhs, rhs, policy).map_err(CompareError::Unsupported)
    }
}
