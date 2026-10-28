//! Remote (owned, Tetrad; docs/remote.md): the Footswitch page: the layouts of the Custom footswitch mode, edited as on the unit.

use tetrad_remote::{EditorOp, Reply};

use super::Device;
use super::platform::Platform;

impl<P: Platform> Device<P> {
    pub(super) fn remote_footswitch(&mut self, op: EditorOp) -> Reply {
        let _ = op;
        Reply::Refused("not built yet".into())
    }
}
