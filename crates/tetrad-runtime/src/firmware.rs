//! Owned SHARC+ resident firmware: graph planning, generated resident code,
//! the SHARC side of the stock message rings, audio I/O setup and LDR images.
//! Everything here is generated or simulated offline; nothing touches hardware.

pub mod audio;
pub mod code;
pub mod graph;
pub mod image;
pub mod layout;
pub mod memory;
pub mod sim;
