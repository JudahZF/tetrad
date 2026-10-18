//! Linux device backends (framebuffer, evdev, ioctls, tty, UIO, /dev/mem). The only crate that allows unsafe code.
//! Every `unsafe` block carries a `SAFETY:` comment.
//!
//! Nothing here touches a device on its own. A caller must name a node and call an explicit `open`, or pass
//! in a descriptor it already owns.

pub mod corectrl;
pub mod display;
pub mod fs;
pub mod gpio;
pub mod hal;
pub mod input;
pub mod ioctl;
pub mod jack;
pub mod lifecycle;
pub mod loader;
pub mod mem;
pub mod paths;
pub mod serial;
pub mod supplicant;
pub mod thread;
pub mod uio;

#[cfg(test)]
mod testing;
