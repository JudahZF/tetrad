//! ARM-side DSP manager over an injected shared-memory backend: ring
//! transport, response dispatch, model blob placement and relocation, and a
//! stock-ordered session (`session::DspSession`) for boot, global controls,
//! parameters, bypass, meters, model deploy and model data, and per-model
//! setup after start (`setup`).
#![forbid(unsafe_code)]

pub mod blob;

/// `$QC_CORPUS_DIR/<path>` for the corpus tests (README, "Corpus tests"), or `None` with a
/// note when the variable is not set.
#[cfg(test)]
pub(crate) fn corpus(path: &str) -> Option<std::path::PathBuf> {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR to read {path}");
        return None;
    };
    Some(std::path::Path::new(&dir).join(path))
}
pub mod device;
pub mod drain;
pub mod model;
pub mod responses;
pub mod session;
pub mod setup;
pub mod transport;

pub use device::{CoreControl, Doorbell, MemoryError, SharedMemory};

#[cfg(test)]
mod test_support;
