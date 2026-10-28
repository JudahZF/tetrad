//! Remote (owned, Tetrad; docs/remote.md): the Themes page: the themes of the unit, and theme files made in the browser.

use tetrad_remote::{Reply, ThemesOp};

use super::Device;
use super::platform::Platform;

impl<P: Platform> Device<P> {
    pub(super) fn remote_themes(&mut self, op: ThemesOp) -> Reply {
        let _ = op;
        Reply::Refused("not built yet".into())
    }
}
