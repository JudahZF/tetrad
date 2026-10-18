//! Global device settings: typed keys, defaults, ranges and the stock
//! persisted file format. Storage I/O is injected.
//!
//! - [`ini`]: Qt 4.8.7 `QSettings::IniFormat` codec. Keys the model does not
//!   know are kept and written back unchanged.
//! - [`variant`]: the `QVariant` subset inside `@Variant(..)` values.
//! - [`DeviceSettings`]: the four stock files plus a typed [`Settings`]
//!   snapshot; updates return [`SettingsEvent`]s; boot migrations; resets.
#![forbid(unsafe_code)]

pub mod account;
pub mod capture;
pub mod controls;
pub mod device;
pub mod general;
pub mod ini;
pub mod io;
pub mod legacy;
pub mod library;
pub mod midi;
pub mod permanent;
pub mod storage;
pub mod tuner;
mod typed;
pub mod variant;

pub use account::Secret;
pub use device::{
    DeviceSettings, Hardware, ResetEffect, ResetKind, Settings, SettingsEvent, StorageError,
};
pub use ini::{FormatIssue, IniValue, SettingsFile};
pub use storage::{SettingsFileId, SettingsStorage};
pub use typed::{LoadIssue, LoadIssueKind, SettingError};
pub use variant::{QtDateTime, Variant, VariantError};

#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod corpus_tests;
#[cfg(test)]
mod model_tests;
