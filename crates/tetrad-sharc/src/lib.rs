//! Owned, restricted scalar SHARC ISA48 emitter and mathematical gain/FIR simulator.
//! Encoding evidence and scope are documented in this crate's README.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod address;
mod alu;
pub mod emit;
pub mod fir;
pub mod gain;
pub mod isa48;
pub mod ldr;
pub mod machine;
pub mod math;
pub mod register;
pub mod simulator;
pub mod visa;

pub use emit::{EmitError, IsaProgram, Label, OwnedCode};
pub use gain::compile_gain;
pub use isa48::{EncodeError, Instruction48, InstructionWord48};
