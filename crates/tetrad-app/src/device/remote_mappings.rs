//! Remote (owned, Tetrad; docs/remote.md): the Mappings page: the mappings of the loaded preset and the global mappings, edited as on the unit.

use tetrad_remote::{EditorOp, Reply};

use super::Device;
use super::platform::Platform;

impl<P: Platform> Device<P> {
    pub(super) fn remote_mappings(&mut self, op: EditorOp) -> Reply {
        let _ = op;
        Reply::Refused("not built yet".into())
    }
}
