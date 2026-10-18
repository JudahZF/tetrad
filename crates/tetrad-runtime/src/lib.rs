//! Injected offline supervisor, worker and owned resident models. No device backend.
#![forbid(unsafe_code)]

pub mod firmware;
pub mod gain_backend;
pub mod resident;
pub mod watchdog;
pub mod worker;
