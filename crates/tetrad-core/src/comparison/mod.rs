//! Bounded semantic comparison of caller-captured, typed admitted snapshots.
//!
//! No wire importer, live Qt handler, lifecycle integration or target FPSCR
//! parity is provided. Every recovered compared field has storage here, even
//! when only its offset and scalar width are known. Draft defaults are synthetic
//! zero fixtures, never firmware initialization. Comparison is directional.

//!
//! ```
//! use tetrad_core::comparison::{compare_parameter, Comparison, ComparisonPolicy,
//!     Parameter, ParameterFields, UnsupportedReason};
//! let policy = ComparisonPolicy::DefaultQtHandlerOrdinaryIeee;
//! let baseline = Parameter::new(ParameterFields::default())?;
//! let working = Parameter::new(ParameterFields {
//!     opaque_word256: 1, ..ParameterFields::default()
//! })?;
//! assert!(matches!(compare_parameter(&baseline, &working, policy)?,
//!     Comparison::Mismatch { .. }));
//! let stepped = Parameter::new(ParameterFields {
//!     type8: 4, ..ParameterFields::default()
//! })?;
//! assert_eq!(compare_parameter(&stepped, &stepped, policy).unwrap_err().reason,
//!     UnsupportedReason::Type4Stepping);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod arena;
mod compare;
mod domain;
mod finite_type4;
pub mod numeric;
mod snapshot;

pub use arena::{
    CompareError, ComparisonArena, ComparisonHandle, HandleError, InsertError, MAX_CAPTURES,
};
pub use compare::{
    Comparison, ComparisonPolicy, Unsupported, UnsupportedReason, VariantContext, compare,
    compare_model, compare_parameter, compare_variant,
};
pub use domain::{
    AdmissionError, AdmissionReason, MAX_BYTES, MAX_ITEMS, MAX_MODELS, MAX_PARAMETERS,
    MAX_PARAMETERS_TOTAL, MAX_ROW_MODELS, MAX_ROWS, MAX_SCALARS, MAX_SCENE_VALUES, MAX_TOTAL_UNITS,
    MAX_UTF16_UNITS, Path, PathSegment, Utf16,
};
pub use finite_type4::{OperandSide, Type4Refusal};
pub use numeric::{F32, F64};
pub use snapshot::*;
