//! Remote (owned, Tetrad; docs/remote.md): the Backup page: a backup of the user data to download, and a backup to restore after a touch on the unit.

use tetrad_remote::{BackupOp, Reply};

use super::Device;
use super::platform::Platform;

impl<P: Platform> Device<P> {
    pub(super) fn remote_backup(&mut self, op: BackupOp) -> Reply {
        let _ = op;
        Reply::Refused("not built yet".into())
    }
}
