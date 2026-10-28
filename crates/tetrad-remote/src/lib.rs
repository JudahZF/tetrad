//! Remote (docs/remote.md): the web page Tetrad serves on the LAN to upload IRs and to download
//! the support file (docs/support.md). This crate
//! reads HTTP, routes requests and runs the server thread. It does no library work: each
//! request that needs the device goes out as a [`Call`], and the device answers it with a
//! [`Reply`].
#![forbid(unsafe_code)]

mod api;
mod http;
mod server;

#[cfg(feature = "dev")]
pub use api::AUTH_DOMAIN;
pub use api::{Call, ExtensionFiles, FileEntry, PIN_HEADER, Reply, Request, TabInfo};
pub use http::{MAX_BODY, serve_one};
pub use server::Server;

/// The port of the page, so that the address alone opens it.
pub const PORT: u16 = 80;
