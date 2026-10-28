//! Remote (owned, Tetrad; docs/remote.md): the IR library page: the user IRs, their folders and the saving destination.

use tetrad_remote::{LibraryOp, Reply};

use super::Device;
use super::platform::Platform;

impl<P: Platform> Device<P> {
    pub(super) fn remote_library(&mut self, op: LibraryOp) -> Reply {
        let _ = op;
        Reply::Refused("not built yet".into())
    }
}
