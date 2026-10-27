//! Owned (Tetrad): the Tetrad link over USB ([`tetrad_usb`], `docs/usb.md`). Requests from a
//! host tool arrive as Tetrad reports among the Cortex Control reports; each gets one reply.
//! A USB install stages a signed release as the `.part` files of the Tetrad updater and offers
//! it on the Tetrad Updates page, where the user starts the install on the unit.

use std::collections::{BTreeMap, VecDeque};

#[cfg(feature = "dev")]
use sha2::Digest;
use tetrad_dsp_host::transport::ControlPacket;
use tetrad_dsp_protocol::hid::HidReportFrame;
use tetrad_services::exec::Files;
use tetrad_services::updater::tetrad::{self, Manifest, TRUSTED_KEY};
use tetrad_ui::{Screen, SettingsPage};
use tetrad_usb::{
    Answer, FileSize, MAX_CHUNK_BYTES, Op, PROTOCOL, Reassembler, Reply, Report, Request,
};

use super::extensions::{Outcome, UsbReply};
use super::platform::{Platform, ServiceBackends};
use super::ui::UiInput;
use super::{Device, DeviceError, Effect, SendPolicy, Subsystem};

/// One install chunk and its header.
const MAX_REQUEST_BYTES: usize = MAX_CHUNK_BYTES + (8 << 10);
/// As the Cortex Control path (`0x6dfbf8`): no wait, 100 ms.
const HID_SEND: SendPolicy = SendPolicy::new(false, 100);
/// Reports per device loop pass, as Cortex Control (`control::outgoing::FRAMES_PER_PASS`): the
/// two links share the HID pipe.
pub const REPORTS_PER_PASS: usize = 12;

pub(super) struct UsbLink {
    reassembler: Reassembler,
    outgoing: VecDeque<Report>,
    staging: Option<Staging>,
}

impl Default for UsbLink {
    fn default() -> Self {
        Self {
            reassembler: Reassembler::new(MAX_REQUEST_BYTES),
            outgoing: VecDeque::new(),
            staging: None,
        }
    }
}

impl UsbLink {
    fn queue(&mut self, reply: &Reply, body: &[u8]) {
        (self.outgoing).extend(tetrad_usb::reports(0, &tetrad_usb::encode(reply, body)));
    }

    /// The answer of an extension to an [`Op::Extension`] request.
    pub(super) fn answer_extension(&mut self, reply: UsbReply) {
        let answer = match reply.result {
            Ok(value) => Answer::Extension { value },
            Err(message) => Answer::Error { message },
        };
        let reply = Reply {
            id: reply.request,
            answer,
        };
        self.queue(&reply, &[]);
    }
}

/// A USB install between `install-begin` and `install-ready`: bytes written per file.
struct Staging {
    manifest: Manifest,
    received: BTreeMap<String, u64>,
    /// The files came from the Remote page of a dev build and nobody signed them: their hashes
    /// are made at `ready`, and the user's touch on the unit is the approval.
    #[cfg(feature = "dev")]
    unsigned: bool,
}

impl<P: Platform> Device<P> {
    /// One Tetrad report from the host ([`tetrad_usb::is_tetrad`]).
    pub(super) fn usb_report(&mut self, report: &[u8]) {
        let message = match self.usb.reassembler.push(report) {
            Ok(Some((0, message))) => message,
            Ok(Some((channel, _))) => {
                self.notice(Subsystem::Usb, format!("no use for channel {channel}"));
                return;
            }
            Ok(None) => return,
            Err(error) => {
                self.notice(Subsystem::Usb, error.to_string());
                return;
            }
        };
        let (request, body) = match tetrad_usb::decode::<Request>(&message) {
            Ok(decoded) => decoded,
            Err(error) => {
                self.notice(Subsystem::Usb, error.to_string());
                return;
            }
        };
        let (answer, body) = match self.usb_answer(request.id, request.op, body) {
            Ok(Some(answer)) => answer,
            // An extension answers later (`UsbLink::answer_extension`).
            Ok(None) => return,
            Err(message) => (Answer::Error { message }, Vec::new()),
        };
        let reply = Reply {
            id: request.id,
            answer,
        };
        self.usb.queue(&reply, &body);
    }

    /// The answer to one request, or `None` when it comes later.
    fn usb_answer(
        &mut self,
        id: u32,
        op: Op,
        body: &[u8],
    ) -> Result<Option<(Answer, Vec<u8>)>, String> {
        if let Op::Extension { extension, message } = op {
            if self.platform.extensions().is_none() {
                return Err("extensions do not run in this mode".into());
            }
            let mut refused = None;
            let effects = self.extensions(|extensions, backend, _| {
                (extensions.usb_message(&extension, id, message, backend)).unwrap_or_else(|e| {
                    refused = Some(e);
                    Outcome::default()
                })
            });
            self.apply(Ok::<_, DeviceError>(effects));
            return refused.map_or(Ok(None), Err);
        }
        Ok(Some(match op {
            Op::Extension { .. } => unreachable!("answered above"),
            Op::Hello => (
                Answer::Hello {
                    tetrad: env!("CARGO_PKG_VERSION").into(),
                    protocol: PROTOCOL,
                },
                Vec::new(),
            ),
            Op::Screenshot => {
                let rgb = self
                    .bridges
                    .ui
                    .render(&self.state)
                    .map_err(|e| e.to_string())?;
                let png = tetrad_ui::png(
                    tetrad_ui::WIDTH,
                    tetrad_ui::HEIGHT,
                    tetrad_ui::PngColour::Rgb,
                    rgb,
                )
                .ok_or("the UI frame is not a full RGB frame")?;
                let answer = Answer::Screenshot {
                    width: tetrad_ui::WIDTH,
                    height: tetrad_ui::HEIGHT,
                };
                (answer, png)
            }
            Op::Log => {
                let lines: Vec<String> = (self.state.notices.iter())
                    .map(|notice| {
                        let at = notice.at.as_secs_f64();
                        format!("{at:10.3} {:?}: {}", notice.subsystem, notice.detail)
                    })
                    .collect();
                let answer = Answer::Log { lines: lines.len() };
                (answer, lines.join("\n").into_bytes())
            }
            Op::InstallBegin {
                manifest,
                signature,
            } => {
                let (version, files) = self.stage_begin(&manifest, &signature)?;
                (Answer::InstallBegin { version, files }, Vec::new())
            }
            Op::InstallChunk { path, offset } => {
                let received = self.stage_chunk(&path, offset, body)?;
                (Answer::InstallChunk { path, received }, Vec::new())
            }
            Op::InstallReady => {
                let version = self.stage_ready()?;
                (Answer::InstallReady { version }, Vec::new())
            }
        }))
    }

    /// Sends up to `limit` queued reports; a failed send keeps its report first for the next
    /// pass. Without a DSP the queue waits. (O) Only while no Cortex Control report waits: the
    /// Control queue then holds no half-sent message, so a Tetrad report never lands inside a
    /// stock message on the wire, whatever the desktop receiver does with one.
    pub(super) fn send_usb_reports(&mut self, limit: usize) {
        if self.bridges.control.transmitting() {
            return;
        }
        let Some(dsp) = self.dsp.as_mut() else {
            return;
        };
        for _ in 0..limit {
            let Some(report) = self.usb.outgoing.front() else {
                return;
            };
            if let Err(error) = dsp.send(ControlPacket::Hid(&HidReportFrame::new(report)), HID_SEND)
            {
                self.notice(Subsystem::Usb, error.to_string());
                return;
            }
            self.usb.outgoing.pop_front();
        }
    }
}

/// The staging of a Tetrad install, for every way it can arrive: the USB link, and the Remote
/// page of a dev build. The files go to the `.part` files of the Tetrad updater, and the install
/// is offered on the Tetrad Updates page, where the user starts it on the unit.
impl<P: Platform> Device<P> {
    /// Checks the signature of `manifest` against the key of this build and starts a staging.
    /// Returns the version and the size of each file to send.
    pub(super) fn stage_begin(
        &mut self,
        manifest: &str,
        signature: &str,
    ) -> Result<(String, Vec<FileSize>), String> {
        // As every update install in bring-up (`RunOptions::refusal`), but `.part` files
        // only, so `--persist` allows it.
        if self.options.is_bringup() && !self.options.persist {
            return Err("staging writes p4: bring-up needs --persist".into());
        }
        (self.bridges.services.usb_staging_allowed()).map_err(|e| e.to_string())?;
        self.bridges.services.withdraw_usb_offer();
        let manifest =
            Manifest::verify(manifest.as_bytes(), signature.as_bytes(), TRUSTED_KEY, None)
                .map_err(|e| e.to_string())?;
        let files = self.platform.services().files();
        for path in manifest.files.keys() {
            let part = tetrad::part(path);
            if let Some(dir) = part.parent() {
                files.create_dirs(dir).map_err(|e| e.to_string())?;
            }
            files.remove(&part).map_err(|e| e.to_string())?;
        }
        let sizes = (manifest.files.iter())
            .map(|(path, entry)| FileSize {
                path: path.clone(),
                size: entry.size,
            })
            .collect();
        let version = manifest.version.to_string();
        let received = manifest
            .files
            .keys()
            .map(|path| (path.clone(), 0))
            .collect();
        self.usb.staging = Some(Staging {
            manifest,
            received,
            #[cfg(feature = "dev")]
            unsigned: false,
        });
        Ok((version, sizes))
    }

    /// A dev build: starts a staging of some of the update files with no signature (the Remote
    /// page, docs/remote.md). `sizes` has the path and the size of each file; only the update
    /// files are taken, each within its limit. Returns the version offered and the sizes.
    #[cfg(feature = "dev")]
    pub(super) fn stage_files_begin(
        &mut self,
        sizes: &[(String, u64)],
    ) -> Result<(String, Vec<FileSize>), String> {
        if self.options.is_bringup() && !self.options.persist {
            return Err("staging writes p4: bring-up needs --persist".into());
        }
        (self.bridges.services.usb_staging_allowed()).map_err(|e| e.to_string())?;
        let mut entries = BTreeMap::new();
        for (path, size) in sizes {
            let max =
                tetrad::max_bytes(path).ok_or_else(|| format!("{path} is not an update file"))?;
            if !(1..=max).contains(size) {
                return Err(format!("{path}: size {size} is not 1..={max}"));
            }
            let entry = tetrad::FileEntry {
                size: *size,
                sha256: String::new(),
            };
            if entries.insert(path.clone(), entry).is_some() {
                return Err(format!("{path} twice"));
            }
        }
        if entries.is_empty() {
            return Err("no files".into());
        }
        self.bridges.services.withdraw_usb_offer();
        let files = self.platform.services().files();
        for path in entries.keys() {
            let part = tetrad::part(path);
            if let Some(dir) = part.parent() {
                files.create_dirs(dir).map_err(|e| e.to_string())?;
            }
            files.remove(&part).map_err(|e| e.to_string())?;
        }
        let running = env!("CARGO_PKG_VERSION")
            .parse()
            .map_err(|_| "the version of this build is not semver")?;
        let manifest = tetrad::staged_manifest(&running, entries);
        let version = manifest.version.to_string();
        let sizes = (manifest.files.iter())
            .map(|(path, entry)| FileSize {
                path: path.clone(),
                size: entry.size,
            })
            .collect();
        let received = manifest
            .files
            .keys()
            .map(|path| (path.clone(), 0))
            .collect();
        self.usb.staging = Some(Staging {
            manifest,
            received,
            unsigned: true,
        });
        Ok((version, sizes))
    }

    /// Appends `body` to file `path` at `offset`, which must be the bytes already here. Returns
    /// the bytes of the file now.
    pub(super) fn stage_chunk(
        &mut self,
        path: &str,
        offset: u64,
        body: &[u8],
    ) -> Result<u64, String> {
        // An install that started since stage_begin owns the `.part` files.
        if let Err(error) = self.bridges.services.usb_staging_allowed() {
            self.usb.staging = None;
            return Err(error.to_string());
        }
        let staging = self.usb.staging.as_mut().ok_or("no install-begin")?;
        let size = (staging.manifest.files.get(path))
            .ok_or_else(|| format!("{path} is not in the manifest"))?
            .size;
        let received = staging.received.entry(path.to_owned()).or_default();
        if offset != *received {
            return Err(format!(
                "{path}: offset {offset}, but {received} bytes are here"
            ));
        }
        let end = offset + body.len() as u64;
        if body.len() > MAX_CHUNK_BYTES || end > size {
            return Err(format!(
                "{path}: a chunk to {end} does not fit {size} bytes"
            ));
        }
        (self
            .platform
            .services()
            .files()
            .append(&tetrad::part(path), body))
        .map_err(|e| e.to_string())?;
        *received = end;
        Ok(end)
    }

    /// Checks that every file is complete and offers the install on the Tetrad Updates page.
    /// Returns the version.
    pub(super) fn stage_ready(&mut self) -> Result<String, String> {
        #[cfg_attr(not(feature = "dev"), expect(unused_mut))]
        let mut staging = self.usb.staging.take().ok_or("no install-begin")?;
        let files = self.platform.services().files();
        for (path, entry) in &staging.manifest.files {
            let have = files.size(&tetrad::part(path)).map_err(|e| e.to_string())?;
            if have != Some(entry.size) {
                return Err(format!("{path}: {have:?} of {} bytes", entry.size));
            }
        }
        #[cfg(feature = "dev")]
        if staging.unsigned {
            // Nothing signed these files, so the unit makes the hashes that the install checks,
            // and a program that is not a 32-bit ARM executable never reaches the install.
            for (path, entry) in &mut staging.manifest.files {
                let max = tetrad::max_bytes(path).unwrap_or(0);
                let part = tetrad::part(path);
                let bytes = (files.read_bounded(&part, max))
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("{path}: gone"))?;
                if path == "bin/tetrad" && !is_arm_elf(&bytes) {
                    let _ = files.remove(&part);
                    return Err("bin/tetrad is not a 32-bit ARM executable".into());
                }
                entry.sha256 = (sha2::Sha256::digest(&bytes).iter())
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
            }
        }
        let version = staging.manifest.version.to_string();
        #[cfg(feature = "dev")]
        let via = if staging.unsigned { "Remote" } else { "USB" };
        #[cfg(not(feature = "dev"))]
        let via = "USB";
        let mut effects = (self
            .bridges
            .services
            .offer_staged_update(staging.manifest, via))
        .map_err(|e| e.to_string())?;
        // The user starts the install on the unit: show them where.
        let page = Screen::Settings(SettingsPage::TetradUpdates);
        effects.push(Effect::UiInput(UiInput::Show(page)));
        self.apply(Ok::<_, DeviceError>(effects));
        Ok(version)
    }
}

/// An ELF file of the Quad Cortex's CPU: 32-bit, little-endian, machine ARM (40).
#[cfg(feature = "dev")]
fn is_arm_elf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x7fELF")
        && bytes.get(4) == Some(&1)
        && bytes.get(5) == Some(&1)
        && bytes.get(18..20) == Some(&[40, 0])
}
