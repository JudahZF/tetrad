//! Remote (owned, Tetrad; docs/remote.md): the device side of the web page that uploads IRs
//! over Wi-Fi. The platform runs the HTTP server ([`RemoteBackend`]); this bridge keeps the
//! on/off setting (`remote.json`) and the PIN, and imports each upload as a Cortex Control IR
//! import does.

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

/// The on/off setting. The PIN is never stored.
pub const SETTINGS_FILE: &str = "/media/p4/tetrad/remote.json";
const MAX_SETTINGS_BYTES: u64 = 4 * 1024;
/// A dev build: the public keys (base64, one per line) that may ask for the PIN.
#[cfg(feature = "dev")]
pub const KEYS_FILE: &str = "/media/p4/tetrad/remote-keys";
/// A dev build: how long a challenge for a key lasts, and how many wait at once.
#[cfg(feature = "dev")]
const NONCE_LIFE: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(feature = "dev")]
const MAX_NONCES: usize = 4;
#[cfg(feature = "dev")]
const MAX_KEYS: usize = 8;
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
    /// A dev build: a new random challenge of 32 hexadecimal digits, for a key that asks for the
    /// PIN.
    #[cfg(feature = "dev")]
    fn nonce(&mut self) -> Result<String, String>;
}

#[derive(Serialize, Deserialize)]
struct Stored {
    enabled: bool,
}

#[derive(Debug, Default)]
pub struct Remote {
    /// `remote.json` was read (at the first preset load).
    loaded: bool,
    /// The switch, as saved. It stays on when the port does not open, so the next boot (or
    /// the switch turned off and on) tries again.
    enabled: bool,
    /// `Some` while the server runs.
    pin: Option<String>,
    wrong: u8,
    status: Option<String>,
    toast: Option<ToastView>,
    /// Challenges that were given out, each with the time it was given (a challenge works once).
    #[cfg(feature = "dev")]
    nonces: Vec<(String, std::time::Duration)>,
}

impl Remote {
    /// `address` is the Wi-Fi lease address, if any.
    pub fn view(&self, address: Option<&str>) -> RemoteView {
        RemoteView {
            enabled: self.enabled,
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
        let enabled = match stored {
            Ok(None) => false,
            Ok(Some(bytes)) => match serde_json::from_slice::<Stored>(&bytes) {
                Ok(stored) => stored.enabled,
                Err(error) => {
                    self.notice(Subsystem::Remote, format!("{SETTINGS_FILE}: {error}"));
                    false
                }
            },
            Err(error) => {
                self.notice(Subsystem::Remote, format!("{SETTINGS_FILE}: {error}"));
                false
            }
        };
        self.bridges.remote.enabled = enabled;
        if enabled {
            self.start_remote();
        }
    }

    /// The Remote page switch, saved in `remote.json`.
    pub(super) fn set_remote(&mut self, enabled: bool) {
        if enabled == self.bridges.remote.enabled {
            return;
        }
        self.bridges.remote.enabled = enabled;
        if enabled {
            self.start_remote();
        } else {
            self.stop_remote(None);
        }
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let path = Path::new(SETTINGS_FILE);
        let bytes = serde_json::to_vec(&Stored { enabled }).unwrap_or_default();
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
        // A key asks for the PIN, so these two cannot have one.
        #[cfg(feature = "dev")]
        match &request {
            Request::AuthNonce => return Some(self.remote_auth_nonce()),
            Request::AuthPin {
                key,
                nonce,
                signature,
            } => return Some(self.remote_auth_pin(key, nonce, signature)),
            _ => {}
        }
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
            #[cfg(feature = "dev")]
            Request::UpdateStatus { .. } => Reply::UpdateStatus {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                select: self.armed_md5(),
            },
            #[cfg(feature = "dev")]
            Request::UpdateBegin { files, .. } => self.remote_update_begin(&files),
            #[cfg(feature = "dev")]
            Request::UpdateInstall { .. } => self.remote_update_install(),
            #[cfg(feature = "dev")]
            Request::AddKey { key, .. } => self.remote_add_key(&key),
            // Answered before the PIN check.
            #[cfg(feature = "dev")]
            Request::AuthNonce | Request::AuthPin { .. } => unreachable!("answered above"),
            #[cfg(feature = "dev")]
            Request::Log { .. } => Reply::Log {
                text: self.remote_log(),
            },
            #[cfg(feature = "dev")]
            Request::UpdateChunk {
                path,
                offset,
                bytes,
                ..
            } => match self.stage_chunk(&path, offset, &bytes) {
                Ok(received) => Reply::UpdateReceived { path, received },
                Err(reason) => self.remote_update_refused(reason),
            },
            #[cfg(feature = "dev")]
            Request::UpdateReady { .. } => match self.stage_ready() {
                Ok(version) => {
                    let notice = format!("Remote: Tetrad {version} staged; start it on the unit");
                    self.notice(Subsystem::Remote, notice);
                    Reply::UpdateStaged { version }
                }
                Err(reason) => self.remote_update_refused(reason),
            },
            Request::Tabs { .. } => self.remote_tabs(),
            Request::TabPage { id, .. } => self.remote_tab_page(&id),
            Request::TabCall { id, message, .. } => {
                return self.remote_tab_call(call, &id, &message);
            }
        })
    }

    /// A dev build: starts a staging of the update files that the page names
    /// ([`Device::stage_files_begin`]).
    #[cfg(feature = "dev")]
    fn remote_update_begin(&mut self, sizes: &[(String, u64)]) -> Reply {
        match self.stage_files_begin(sizes) {
            Ok((version, files)) => {
                let names: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
                let notice = format!("Remote: update of {} ({version})", names.join(", "));
                self.notice(Subsystem::Remote, notice);
                let files = files.into_iter().map(|f| (f.path, f.size)).collect();
                Reply::UpdateBegun { version, files }
            }
            Err(reason) => self.remote_update_refused(reason),
        }
    }

    /// A challenge for a key that asks for the PIN. Old ones expire, and no more than
    /// [`MAX_NONCES`] wait: the oldest goes.
    #[cfg(feature = "dev")]
    fn remote_auth_nonce(&mut self) -> Reply {
        use tetrad_dsp_host::session::Clock;
        if self.bridges.remote.pin.is_none() {
            return Reply::Off;
        }
        let now = self.clock.now();
        let nonce = match self.platform.remote().map(|backend| backend.nonce()) {
            Some(Ok(nonce)) => nonce,
            Some(Err(reason)) => return Reply::Refused(reason),
            None => return Reply::Off,
        };
        let nonces = &mut self.bridges.remote.nonces;
        nonces.retain(|(_, at)| now.saturating_sub(*at) < NONCE_LIFE);
        while nonces.len() >= MAX_NONCES {
            nonces.remove(0);
        }
        nonces.push((nonce.clone(), now));
        Reply::AuthNonce { nonce }
    }

    /// The PIN for a known key that signed a challenge of this unit. The challenge is used up by
    /// the attempt, right or wrong.
    #[cfg(feature = "dev")]
    fn remote_auth_pin(&mut self, key: &str, nonce: &str, signature: &str) -> Reply {
        use tetrad_dsp_host::session::Clock;
        let Some(pin) = self.bridges.remote.pin.clone() else {
            return Reply::Off;
        };
        let now = self.clock.now();
        let nonces = &mut self.bridges.remote.nonces;
        let fresh = (nonces.iter())
            .position(|(known, at)| known == nonce && now.saturating_sub(*at) < NONCE_LIFE)
            .is_some();
        nonces.retain(|(known, _)| known != nonce);
        let short: String = key.chars().take(8).collect();
        let verdict = if !fresh {
            Err("the challenge is unknown or too old")
        } else if !self.remote_keys().iter().any(|known| known == key.trim()) {
            Err("this key is not known to the unit")
        } else {
            let message = format!("{}{nonce}", tetrad_remote::AUTH_DOMAIN);
            tetrad_services::updater::tetrad::verify_signature(
                key,
                message.as_bytes(),
                signature.as_bytes(),
            )
            .map_err(|_| "the signature does not match")
        };
        match verdict {
            Ok(()) => {
                self.notice(
                    Subsystem::Remote,
                    format!("Remote: key {short}… got the PIN"),
                );
                Reply::AuthPin { pin }
            }
            Err(reason) => {
                let notice = format!("Remote: key {short}… refused: {reason}");
                self.notice(Subsystem::Remote, notice);
                Reply::Refused(reason.to_owned())
            }
        }
    }

    /// The public keys in [`KEYS_FILE`].
    #[cfg(feature = "dev")]
    fn remote_keys(&mut self) -> Vec<String> {
        use tetrad_services::exec::Files;
        let path = Path::new(KEYS_FILE);
        let bytes = (self.platform.services().files().read_bounded(path, 4096))
            .ok()
            .flatten()
            .unwrap_or_default();
        (String::from_utf8_lossy(&bytes).lines())
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// Takes a public key for the PIN requests. The PIN already proved who asks.
    #[cfg(feature = "dev")]
    fn remote_add_key(&mut self, key: &str) -> Reply {
        use tetrad_services::exec::Files;
        let key = key.trim();
        if !tetrad_services::updater::tetrad::is_public_key(key) {
            return Reply::Refused("not a base64 Ed25519 public key".into());
        }
        let mut keys = self.remote_keys();
        if !keys.iter().any(|known| known == key) {
            if keys.len() >= MAX_KEYS {
                return Reply::Refused(format!(
                    "{MAX_KEYS} keys are known; remove one on the card"
                ));
            }
            keys.push(key.to_owned());
            let files = self.platform.services().files();
            let path = Path::new(KEYS_FILE);
            let saved = (path.parent())
                .map_or(Ok(()), |dir| files.create_dirs(dir))
                .and_then(|()| files.write_atomic(path, (keys.join("\n") + "\n").as_bytes()));
            if let Err(error) = saved {
                return Reply::Refused(format!("the key is not saved: {error}"));
            }
            let short: String = key.chars().take(8).collect();
            self.notice(Subsystem::Remote, format!("Remote: key {short}… added"));
        }
        Reply::KeyAdded { keys: keys.len() }
    }

    /// The md5 in `launcher/select`: the binary that the launcher starts.
    #[cfg(feature = "dev")]
    fn armed_md5(&mut self) -> Option<String> {
        use tetrad_services::exec::Files;
        let path = Path::new(tetrad_services::updater::tetrad::HOME).join("launcher/select");
        let bytes = (self.platform.services().files().read_bounded(&path, 64)).ok()??;
        let text = String::from_utf8(bytes).ok()?;
        Some(text.trim().to_owned())
    }

    /// A dev build: UPDATE TETRAD without the touch, for the files that `ready` offered. The
    /// same command as the button, so it has the same refusals (no offer, a CorOS update).
    #[cfg(feature = "dev")]
    fn remote_update_install(&mut self) -> Reply {
        let command = tetrad_ui::UiCommand::TetradUpdate {
            command: tetrad_ui::TetradUpdateCommand::Install,
        };
        use tetrad_dsp_host::session::Clock;
        let now = self.clock.now();
        match self.handoff(super::ui::Handoff::Services(command), now) {
            Ok(effects) => {
                self.notice(Subsystem::Remote, "Remote: install started".to_owned());
                self.apply(Ok::<_, super::DeviceError>(effects));
                Reply::UpdateInstalling
            }
            Err(error) => self.remote_update_refused(error.to_string()),
        }
    }

    /// The recent notices as `usb.rs` lists them: the time, the subsystem and the text.
    #[cfg(feature = "dev")]
    fn remote_log(&self) -> String {
        let lines: Vec<String> = (self.state.notices.iter())
            .map(|notice| {
                let at = notice.at.as_secs_f64();
                format!("{at:10.3} {:?}: {}", notice.subsystem, notice.detail)
            })
            .collect();
        lines.join("\n") + "\n"
    }

    #[cfg(feature = "dev")]
    fn remote_update_refused(&mut self, reason: String) -> Reply {
        self.notice(
            Subsystem::Remote,
            format!("Remote: update refused: {reason}"),
        );
        Reply::Refused(reason)
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
