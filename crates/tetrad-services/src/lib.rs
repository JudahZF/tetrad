//! System service executors: Wi-Fi association, DHCP, time, internet probes,
//! updater installation and cloud clients. Commands, files, sockets and HTTP are injected
//! ([`exec`], [`http`]); this crate does no device I/O on its own. [`system`] holds the std
//! backends that do, and [`fake`] the scripted ones for offline tests.
#![forbid(unsafe_code)]

pub mod backup;
pub mod bootenv;
pub mod cloud;
pub mod dhcp;
pub mod exec;
pub mod fake;
pub mod http;
pub mod internet;
pub mod logs;
#[cfg(unix)]
pub mod system;
pub mod updater;
pub mod wifi;
