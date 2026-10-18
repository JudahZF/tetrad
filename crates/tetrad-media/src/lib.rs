//! Stock QC media: IR decoding (WAV, raw product words, device-bound factory
//! files), the factory IR catalog, IR Loader and cab block transfer plans with
//! the host-side cab IR blend, user IR products, metronome sounds and owned
//! offline convolution, and the capture store re-encryption. No filesystem or
//! device transport: callers inject bytes, key material and readers.

pub mod block;
pub mod cab;
pub mod capture_store;
pub mod catalog;
pub mod factory;
pub mod ir;
pub mod metronome;
pub mod owned_convolution;
mod proto;
pub mod user;
