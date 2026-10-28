//! Remote (owned, Tetrad; docs/remote.md): the Manage page of extensions: their state, switches, settings, updates and installs.

use tetrad_remote::{ExtensionsOp, Reply};

use super::Device;
use super::platform::Platform;

impl<P: Platform> Device<P> {
    pub(super) fn remote_extensions(&mut self, op: ExtensionsOp) -> Reply {
        let _ = op;
        Reply::Refused("not built yet".into())
    }
}
