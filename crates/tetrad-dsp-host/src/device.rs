//! Hardware seams. `tetrad-sys` implements these for the real device; tests use in-memory fakes.
//!
//! This file is the contract between `tetrad-dsp-host` and `tetrad-sys`. Change it only in
//! coordination with both crates.

use std::time::Duration;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MemoryError {
    #[error("access {offset:#x}+{len} is outside the {window:#x}-byte window")]
    OutOfBounds {
        offset: usize,
        len: usize,
        window: usize,
    },
    #[error("offset {0:#x} is not 4-byte aligned")]
    Misaligned(usize),
}

/// Byte-addressed view of one physical window shared with the SHARC cores
/// (for example the message memory at `0x20088000`, length `0x1791c`).
/// Offsets are relative to the window base. Reads must observe the shared window,
/// not a stale compiler copy. The backend must provide coherent access through
/// suitable mapping attributes or cache maintenance; barriers alone are not enough.
pub trait SharedMemory {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError>;
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError>;
    /// One aligned 32-bit load; never split into byte accesses.
    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError>;
    /// One aligned 32-bit store; never split into byte accesses.
    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError>;
    /// Orders earlier shared-memory reads before later reads and writes. After
    /// observing a peer's published index/length, subsequent payload accesses
    /// must not precede that observation. This does not establish peer store order.
    fn acquire_barrier(&mut self);
    /// Orders every earlier shared-memory read and write before later writes,
    /// and makes earlier writes visible to the DSP before publication. In
    /// particular, payload reads must finish before a consumer acknowledgement
    /// allows the DSP to reuse a slot. A store-only barrier is insufficient.
    fn publish_barrier(&mut self);
}

/// One UIO interrupt line from a SHARC core.
pub trait Doorbell {
    type Error: std::error::Error + Send + Sync + 'static;
    /// Waits up to `timeout` and returns the interrupt count when one arrived.
    fn wait(&mut self, timeout: Duration) -> Result<Option<u32>, Self::Error>;
    /// Re-enables the interrupt (stock writes u32 1).
    fn rearm(&mut self) -> Result<(), Self::Error>;
}

/// SHARC core start/stop and entry-point control (stock requests `0x6200`, `0x6201`,
/// `0x6d11`, `0x6d12`). Core numbers are the stock 1-based SHARC core numbers.
pub trait CoreControl {
    type Error: std::error::Error + Send + Sync + 'static;
    fn start(&mut self, core: u8) -> Result<(), Self::Error>;
    fn stop(&mut self, core: u8) -> Result<(), Self::Error>;
    fn set_entry(&mut self, core: u8, address: u32) -> Result<(), Self::Error>;
}
