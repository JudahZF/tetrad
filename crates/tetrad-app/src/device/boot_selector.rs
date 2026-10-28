//! Settings › Tetrad › Boot Selector (owned, Tetrad; docs/launcher.md): the device side of the
//! switch. The launcher shows the boot selector unless [`NO_CHOOSER_FILE`] exists, the file
//! that `tetrad-launcher.sh chooser off` writes and `chooser on` removes.

use std::path::Path;

use tetrad_services::exec::Files;

use super::platform::{Platform, ServiceBackends};
use super::{Device, Subsystem};

/// `$TD_HOME/no-chooser` in tetrad-launch.sh (a host test compares them).
pub const NO_CHOOSER_FILE: &str = "/media/p4/tetrad/launcher/no-chooser";

#[derive(Debug, Default)]
pub struct BootSelector {
    /// [`NO_CHOOSER_FILE`] was read (at the first preset load).
    loaded: bool,
    /// The launcher shows the selector. `None` when the file could not be read.
    shown: Option<bool>,
}

impl BootSelector {
    pub fn view(&self) -> Option<bool> {
        self.shown
    }
}

impl<P: Platform> Device<P> {
    /// Reads the switch once, at the first preset load, as `load_remote` does.
    pub(super) fn load_boot_selector(&mut self) {
        if std::mem::replace(&mut self.bridges.boot_selector.loaded, true) {
            return;
        }
        let path = Path::new(NO_CHOOSER_FILE);
        match self.platform.services().files().size(path) {
            Ok(size) => self.bridges.boot_selector.shown = Some(size.is_none()),
            Err(error) => self.notice(Subsystem::Settings, format!("Boot selector: {error}")),
        }
    }

    /// The Boot Selector switch: removes [`NO_CHOOSER_FILE`] to show the selector, writes it to
    /// hide it. On a failure the switch keeps its old state.
    pub(super) fn set_boot_selector(&mut self, shown: bool) {
        if self.bridges.boot_selector.shown == Some(shown) {
            return;
        }
        let files = self.platform.services().files();
        let path = Path::new(NO_CHOOSER_FILE);
        let saved = if shown {
            files.remove(path).and_then(|()| files.sync())
        } else {
            (path.parent())
                .map_or(Ok(()), |dir| files.create_dirs(dir))
                .and_then(|()| files.write_atomic(path, b"turned off in Settings\n"))
        };
        match saved {
            Ok(()) => self.bridges.boot_selector.shown = Some(shown),
            Err(error) => {
                self.notice(
                    Subsystem::Settings,
                    format!("Boot selector not saved: {error}"),
                );
            }
        }
    }
}
