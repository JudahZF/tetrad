//! Remote (owned, Tetrad; docs/remote.md): the device side of the web page that uploads IRs
//! over Wi-Fi and hands out the support file (`super::support`). The platform runs the HTTP
//! server ([`RemoteBackend`]); this bridge keeps the switches (`remote.json`) and the PIN, and
//! imports each upload as a Cortex Control IR import does.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tetrad_library::stock::{IrImport, import_user_ir, saving_destination};
use tetrad_media::ir::IrLimits;
use tetrad_media::user::{MAX_IR_NAME_UNITS, simplify_name};
use tetrad_remote::{Call, Reply, Request, TabInfo};
use tetrad_storage::stock::{ProductType, StockView};
use tetrad_ui::{RemoteView, ToastView};

use super::platform::{Platform, ServiceBackends};
use super::{Device, Subsystem};

/// The switches. The PIN is never stored.
pub const SETTINGS_FILE: &str = "/media/p4/tetrad/remote.json";
const MAX_SETTINGS_BYTES: u64 = 4 * 1024;
/// Wrong PINs before the unit makes a new PIN.
pub const MAX_WRONG_PINS: u8 = 10;
/// The page sends 1,024 samples (4,142 bytes); the stock extractor keeps no more.
const WAV_LIMITS: IrLimits = IrLimits {
    max_asset_bytes: tetrad_remote::MAX_BODY,
    max_chunks: 4096,
};

/// The HTTP server of the platform. Its requests arrive as [`super::platform::Input::Remote`].
pub trait RemoteBackend {
    /// Opens the port. `Err` is the reason for the page, such as "Port 80 is in use".
    fn start(&mut self) -> Result<(), String>;
    fn stop(&mut self);
    fn reply(&mut self, id: u64, reply: Reply);
    /// A new random 4-digit PIN.
    fn new_pin(&mut self) -> Result<String, String>;
}

#[derive(Serialize, Deserialize)]
struct Stored {
    enabled: bool,
    /// Absent in files from before the switch: off.
    #[serde(default)]
    pin_in_menu: bool,
}

#[derive(Debug, Default)]
pub struct Remote {
    /// `remote.json` was read (at the first preset load).
    loaded: bool,
    /// The switch, as saved. It stays on when the port does not open, so the next boot (or
    /// the switch turned off and on) tries again.
    enabled: bool,
    /// The PIN shows as the first row of the ⋮ menu while the server runs.
    pin_in_menu: bool,
    /// `Some` while the server runs.
    pin: Option<String>,
    wrong: u8,
    status: Option<String>,
    toast: Option<ToastView>,
}

impl Remote {
    /// `address` is the Wi-Fi lease address, if any.
    pub fn view(&self, address: Option<&str>) -> RemoteView {
        RemoteView {
            enabled: self.enabled,
            pin_in_menu: self.pin_in_menu,
            address: address.map(|address| format!("http://{address}")),
            pin: self.pin.clone(),
            status: self.status.clone(),
        }
    }

    pub fn toast(&self) -> Option<ToastView> {
        self.toast.clone()
    }

    fn show_toast(&mut self, text: String) {
        let id = self.toast.as_ref().map_or(0, |toast| toast.id) + 1;
        self.toast = Some(ToastView { id, text });
    }
}

impl<P: Platform> Device<P> {
    /// Reads `remote.json` once and starts Remote when it says on. Called at the first preset
    /// load, as the extension store is: start-up never waits for it.
    pub(super) fn load_remote(&mut self) {
        if std::mem::replace(&mut self.bridges.remote.loaded, true) {
            return;
        }
        use tetrad_services::exec::Files;
        let path = Path::new(SETTINGS_FILE);
        let stored = (self.platform.services().files()).read_bounded(path, MAX_SETTINGS_BYTES);
        let off = Stored {
            enabled: false,
            pin_in_menu: false,
        };
        let stored = match stored {
            Ok(None) => off,
            Ok(Some(bytes)) => match serde_json::from_slice::<Stored>(&bytes) {
                Ok(stored) => stored,
                Err(error) => {
                    self.notice(Subsystem::Remote, format!("{SETTINGS_FILE}: {error}"));
                    off
                }
            },
            Err(error) => {
                self.notice(Subsystem::Remote, format!("{SETTINGS_FILE}: {error}"));
                off
            }
        };
        let remote = &mut self.bridges.remote;
        remote.enabled = stored.enabled;
        remote.pin_in_menu = stored.pin_in_menu;
        if stored.enabled {
            self.start_remote();
        }
    }

    /// The Remote page switches, saved in `remote.json`.
    pub(super) fn set_remote(&mut self, enabled: bool, pin_in_menu: bool) {
        let remote = &mut self.bridges.remote;
        if (enabled, pin_in_menu) == (remote.enabled, remote.pin_in_menu) {
            return;
        }
        let toggled = enabled != remote.enabled;
        remote.enabled = enabled;
        remote.pin_in_menu = pin_in_menu;
        match (toggled, enabled) {
            (true, true) => self.start_remote(),
            (true, false) => self.stop_remote(None),
            (false, _) => {}
        }
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let path = Path::new(SETTINGS_FILE);
        let stored = Stored {
            enabled,
            pin_in_menu,
        };
        let bytes = serde_json::to_vec(&stored).unwrap_or_default();
        let saved = (path.parent())
            .map_or(Ok(()), |dir| files.create_dirs(dir))
            .and_then(|()| files.write_atomic(path, &bytes));
        if let Err(error) = saved {
            self.notice(Subsystem::Remote, format!("Remote not saved: {error}"));
        }
    }

    fn start_remote(&mut self) {
        let started = match self.platform.remote() {
            Some(backend) => backend
                .start()
                .and_then(|()| backend.new_pin().inspect_err(|_| backend.stop())),
            None => Err("Remote does not run in this mode".to_owned()),
        };
        let remote = &mut self.bridges.remote;
        match started {
            Ok(pin) => {
                remote.pin = Some(pin);
                remote.wrong = 0;
                remote.status = None;
            }
            Err(reason) => {
                remote.status = Some(reason.clone());
                self.notice(Subsystem::Remote, format!("Remote not started: {reason}"));
            }
        }
    }

    fn stop_remote(&mut self, status: Option<String>) {
        if let Some(backend) = self.platform.remote() {
            backend.stop();
        }
        let remote = &mut self.bridges.remote;
        remote.pin = None;
        remote.status = status;
    }

    /// Answers one page request.
    pub(super) fn remote_call(&mut self, call: Call) {
        // A tab message gets its reply later, when the extension has answered.
        let Some(reply) = self.remote_reply(call.id, call.request) else {
            return;
        };
        if let Some(backend) = self.platform.remote() {
            backend.reply(call.id, reply);
        }
    }

    fn remote_reply(&mut self, call: u64, request: Request) -> Option<Reply> {
        let remote = &mut self.bridges.remote;
        let Some(pin) = remote.pin.as_deref() else {
            return Some(Reply::Off);
        };
        if pin != request.pin() {
            remote.wrong += 1;
            if remote.wrong >= MAX_WRONG_PINS {
                self.new_remote_pin();
            }
            return Some(Reply::WrongPin);
        }
        remote.wrong = 0;
        Some(match request {
            Request::Status { .. } => match self.ir_destination() {
                Ok((_, folder)) => Reply::Status { folder },
                Err(reason) => Reply::Refused(reason),
            },
            Request::Upload { name, wav, .. } => self.remote_upload(&name, &wav),
            Request::Files { .. } => self.remote_files(),
            Request::FileChunk {
                id,
                name,
                offset,
                last,
                bytes,
                ..
            } => self.remote_file_chunk(&id, &name, offset, last, &bytes),
            Request::FileDelete { id, name, .. } => self.remote_file_delete(&id, &name),
            Request::Support { .. } => {
                let (name, zip) = self.support_file();
                Reply::File { name, zip }
            }
            Request::Tabs { .. } => self.remote_tabs(),
            Request::TabPage { id, .. } => self.remote_tab_page(&id),
            Request::TabCall { id, message, .. } => {
                return self.remote_tab_call(call, &id, &message);
            }
        })
    }

    /// The tabs of the extensions that have one.
    fn remote_tabs(&self) -> Reply {
        let tabs = (self.bridges.extensions.remote_tabs().into_iter())
            .filter_map(|(_, installation)| {
                let tab = installation.remote.as_ref()?;
                Some(TabInfo {
                    id: installation.id.clone(),
                    title: tab.title.clone(),
                })
            })
            .collect();
        Reply::Tabs { tabs }
    }

    fn remote_tab_page(&self, id: &str) -> Reply {
        let page = (self.bridges.extensions.remote_tabs().into_iter())
            .find(|(_, installation)| installation.id == id)
            .and_then(|(_, installation)| installation.remote.as_ref())
            .map(|tab| tab.page.clone());
        match page {
            Some(html) => Reply::TabPage { html },
            None => Reply::Refused(format!("{id} has no tab")),
        }
    }

    /// Hands a message of a tab to its extension. `None`: the extension has it, and its answer
    /// ([`Self::answer_tab`]) is the reply to call `call`.
    fn remote_tab_call(&mut self, call: u64, id: &str, message: &[u8]) -> Option<Reply> {
        let message: serde_json::Value = match serde_json::from_slice(message) {
            Ok(message) => message,
            Err(_) => return Some(Reply::Refused("The message is not JSON".into())),
        };
        let mut refused = Some("Extensions are not running".to_owned());
        let _ = self.extensions(|extensions, backend, _| {
            match extensions.remote_message(id, call, message, backend) {
                Ok(outcome) => {
                    refused = None;
                    outcome
                }
                Err(reason) => {
                    refused = Some(reason);
                    super::extensions::Outcome::default()
                }
            }
        });
        refused.map(Reply::Refused)
    }

    /// The extension answered a tab message (or stopped): the reply to the page.
    pub(super) fn answer_tab(&mut self, reply: super::extensions::RemoteReply) {
        let answer = match reply.result {
            Ok(value) => Reply::TabReply {
                json: value.to_string(),
            },
            Err(reason) => Reply::Refused(reason),
        };
        if let Some(backend) = self.platform.remote() {
            backend.reply(reply.request, answer);
        }
    }

    /// The extensions that take asset files, and their files.
    fn remote_files(&mut self) -> Reply {
        use super::remote_files::list;
        let kinds = self.bridges.extensions.asset_kinds();
        let extensions = &self.bridges.extensions;
        let state = |key: &str, variant: &str| extensions.asset_state(key, variant);
        match list(self.platform.services().files(), &kinds, &state) {
            Ok(extensions) => Reply::Files { extensions },
            Err(reason) => Reply::Refused(reason),
        }
    }

    fn remote_file_chunk(
        &mut self,
        id: &str,
        name: &str,
        offset: u64,
        last: bool,
        bytes: &[u8],
    ) -> Reply {
        let kinds = self.bridges.extensions.asset_kinds();
        let files = self.platform.services().files();
        match super::remote_files::chunk(files, &kinds, id, name, offset, last, bytes) {
            Ok(reply @ Reply::FileStored { .. }) => {
                self.notice(Subsystem::Remote, format!("Remote: {name} stored for {id}"));
                self.bridges
                    .remote
                    .show_toast(format!("File added: {name}"));
                self.rescan_assets(&kinds, id);
                reply
            }
            Ok(reply) => reply,
            Err(reason) => {
                self.notice(
                    Subsystem::Remote,
                    format!("Remote: {name} not stored for {id}: {reason}"),
                );
                Reply::Refused(reason)
            }
        }
    }

    fn remote_file_delete(&mut self, id: &str, name: &str) -> Reply {
        let kinds = self.bridges.extensions.asset_kinds();
        let files = self.platform.services().files();
        match super::remote_files::delete(files, &kinds, id, name) {
            Ok(reply) => {
                self.notice(
                    Subsystem::Remote,
                    format!("Remote: {name} removed from {id}"),
                );
                self.rescan_assets(&kinds, id);
                reply
            }
            Err(reason) => Reply::Refused(reason),
        }
    }

    /// The extension `id` reads its asset folder again.
    fn rescan_assets(&mut self, kinds: &[super::extensions::AssetKind], id: &str) {
        let Some(kind) = kinds.iter().find(|kind| kind.id == id) else {
            return;
        };
        let key = kind.key.clone();
        let _ = self.extensions(|extensions, backend, _| {
            extensions.rescan_assets(&key, backend);
            super::extensions::Outcome::default()
        });
    }

    /// After [`MAX_WRONG_PINS`] wrong PINs: the old PIN no longer works.
    fn new_remote_pin(&mut self) {
        let pin = self.platform.remote().map(|backend| backend.new_pin());
        let notice = format!("Remote: a new PIN after {MAX_WRONG_PINS} wrong PINs");
        self.notice(Subsystem::Remote, notice);
        match pin {
            Some(Ok(pin)) => {
                let remote = &mut self.bridges.remote;
                remote.pin = Some(pin);
                remote.wrong = 0;
                remote.status = Some(format!("New PIN after {MAX_WRONG_PINS} wrong PINs"));
            }
            Some(Err(reason)) => self.stop_remote(Some(format!("Remote stopped: {reason}"))),
            None => self.stop_remote(None),
        }
    }

    fn remote_upload(&mut self, name: &str, wav: &[u8]) -> Reply {
        let name = simplify_name(name);
        let units = name.encode_utf16().count();
        if !(1..=MAX_IR_NAME_UNITS).contains(&units) {
            return Reply::Refused(format!(
                "An IR name has 1 to {MAX_IR_NAME_UNITS} characters"
            ));
        }
        match self.import_remote_ir(&name, wav) {
            Ok(folder) => {
                // Not `Library`: a Library notice right after a touch opens a dialog, and the
                // toast already tells the player.
                self.notice(
                    Subsystem::Remote,
                    format!("Remote: IR {name} added to {folder}"),
                );
                self.refresh_directory_media();
                let remote = &mut self.bridges.remote;
                remote.status = Some(format!("Added {name}"));
                remote.show_toast(format!("IR added: {name}"));
                Reply::Added { name, folder }
            }
            Err(reason) => {
                self.notice(
                    Subsystem::Remote,
                    format!("Remote: IR {name} not stored: {reason}"),
                );
                self.bridges.remote.status = Some(format!("{name}: {reason}"));
                Reply::Refused(reason)
            }
        }
    }

    /// The stock IR import (`tetrad_library::stock::import_user_ir`) into the IR saving
    /// destination. Returns the folder name.
    fn import_remote_ir(&mut self, name: &str, wav: &[u8]) -> Result<String, String> {
        if !self.options.persist {
            return Err("this run does not write the library".into());
        }
        let date = (self.platform.wall_clock()).ok_or("no wall clock for the import date")?;
        let (folder, folder_name) = self.ir_destination()?;
        let cipher = (self.media.as_ref())
            .map(|media| media.cipher())
            .ok_or("no device media cipher")?;
        let db = (self.state.library.as_mut())
            .and_then(|library| library.database.as_mut())
            .ok_or("the library is closed")?;
        let import = IrImport {
            wav,
            name,
            date,
            cloud_id: "",
            coros_version: tetrad_firmware::running().version,
        };
        import_user_ir(db, folder, &import, cipher, WAV_LIMITS)
            .map_err(|error| error.to_string())?;
        Ok(folder_name)
    }

    /// The IR saving destination: its id and name.
    fn ir_destination(&self) -> Result<(i64, String), String> {
        let stored = &self
            .state
            .settings
            .settings()
            .library
            .download_destination_ir;
        let db = (self.state.library.as_ref())
            .and_then(|library| library.database.as_ref())
            .ok_or("the library is closed")?;
        folder_named(&db.view(), stored.as_deref()).map_err(|error| error.to_string())
    }
}

fn folder_named(
    view: &StockView<'_>,
    stored: Option<&str>,
) -> tetrad_library::Result<(i64, String)> {
    let folder = saving_destination(view, ProductType::Ir, stored)?;
    let name = (view.folders()?.into_iter())
        .find(|entry| entry.id == folder)
        .map(|entry| entry.name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "IRs".to_owned());
    Ok((folder, name))
}
