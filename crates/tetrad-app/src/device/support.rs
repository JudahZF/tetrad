//! The support file (owned, Tetrad; docs/support.md): a zip with what a bug report needs,
//! downloaded from the Remote page. Each text entry is written field by field from typed
//! state, so no secret reaches it: no tokens, licence data, Wi-Fi secrets, account names,
//! Remote PIN, presets or captures. The serial goes in only as a short hash. The boot logs
//! and `launch.log` go in as they are (their last [`LOG_TAIL`] bytes), except that every copy
//! of the serial, a sign-in token or the account name is replaced.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::write::DeflateEncoder;
use sha2::{Digest, Sha256};
use tetrad_dsp_host::session::Clock;
use tetrad_engine::Site;
use tetrad_services::exec::Files;
use tetrad_sys::fs::FsSpace;
use tetrad_ui::{ExtensionState, InstalledExtension, WifiStatus};

use super::platform::{Platform, ServiceBackends};
use super::services::{LICENSABLE_PLUGINS, PluginStatus, utc_parts};
use super::{Device, State, Subsystem};

/// Stock's `tee` (`S80drvinit`) writes each boot's output, Tetrad's notices included, to
/// `<n>-mmcblk0p1.log` here. `n` is a log counter of its own, not `boot_count.txt`.
const BOOT_LOG_DIR: &str = "/media/p4/logs/system";
const BOOT_LOG_SUFFIX: &str = "-mmcblk0p1.log";
/// The newest boot logs: this boot and the ones before it.
const BOOT_LOGS: usize = 5;
/// The end of each log that goes in: enough for many boots of notices, small enough to build
/// within the Remote reply time.
pub const LOG_TAIL: u64 = 256 * 1024;
/// The launcher log and the copy it rotates out at 32 KiB (`tetrad-launch.sh`, `TD_HOME`).
const LAUNCH_LOGS: [&str; 2] = [
    "/media/p4/tetrad/launcher/launch.log.1",
    "/media/p4/tetrad/launcher/launch.log",
];
/// The md5 of the binary the launcher armed (`tetrad-launcher.sh arm`).
const SELECT_FILE: &str = "/media/p4/tetrad/launcher/select";
/// The unit has no RTC: until NTP or Cortex Control sets it, the clock starts in 1970.
const CLOCK_SET_AFTER: u64 = 1_704_067_200; // 2024-01-01
/// The newest extension log lines that go in.
const EXTENSION_LOG_LINES: usize = 40;

/// One file in the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub data: Vec<u8>,
}

/// What the collector needs beyond [`State`], read by the device at the same moment.
pub struct Context<'a> {
    /// The device clock of [`super::Notice::at`].
    pub now: Duration,
    pub wall: Option<SystemTime>,
    pub online: Option<bool>,
    pub signed_in: Option<bool>,
    pub wifi: WifiStatus,
    pub disk: Option<FsSpace>,
    pub extensions: &'a [InstalledExtension],
}

impl Context<'_> {
    fn clock(&self) -> Option<u64> {
        set_clock(self.wall)
    }
}

/// Unix seconds of a wall clock that NTP or Cortex Control has set.
pub fn set_clock(wall: Option<SystemTime>) -> Option<u64> {
    let seconds = wall?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    (seconds >= CLOCK_SET_AFTER).then_some(seconds)
}

impl<P: Platform> Device<P> {
    /// The support file for the Remote page: its download name and the zip.
    pub(super) fn support_file(&mut self) -> (String, Vec<u8>) {
        let wall = self.platform.wall_clock();
        let disk = self.platform.disk_space();
        let services = &self.bridges.services;
        let extensions = self.bridges.extensions.view(&self.state.community);
        let context = Context {
            now: self.clock.now(),
            wall,
            online: services.online(),
            signed_in: services.signed_in_now(),
            wifi: services.wifi_view().status,
            disk,
            extensions: &extensions.installed,
        };
        let files = self.platform.services().files();
        let entries = collect(&self.state, &context, files);
        let name = file_name(context.clock(), self.state.boot_count);
        let zip = zip(&entries, context.clock());
        let detail = format!(
            "Remote: support file {name} sent ({} KiB)",
            zip.len() / 1024
        );
        self.notice(Subsystem::Remote, detail);
        (name, zip)
    }
}

/// Every entry of the support file.
pub fn collect(state: &State, context: &Context<'_>, files: &mut impl Files) -> Vec<Entry> {
    let text = |name: &str, body: String| Entry {
        name: name.to_owned(),
        data: body.into_bytes(),
    };
    let mut entries = vec![
        text("summary.txt", summary(state, context, files)),
        text("licences.txt", licences(state)),
        text("catalog.txt", catalog(state)),
        text("preset.txt", preset(state)),
        text("extensions.txt", extensions(context)),
        text("dsp.txt", dsp(state)),
        text("notices.txt", notices(state, context)),
    ];
    let logs = (boot_logs(files).into_iter().map(|n| {
        let path = format!("{BOOT_LOG_DIR}/{n}{BOOT_LOG_SUFFIX}");
        (path, format!("logs/boot-{n}.log"))
    }))
    .chain(LAUNCH_LOGS.map(|path| {
        let name = path.rsplit('/').next().unwrap_or(path);
        (path.to_owned(), format!("logs/{name}"))
    }));
    for (path, name) in logs {
        match files.read_tail(Path::new(&path), LOG_TAIL) {
            Ok(Some(data)) => entries.push(Entry { name, data }),
            Ok(None) => {}
            Err(error) => entries.push(text(&format!("{name}.error.txt"), error.to_string())),
        }
    }
    for entry in &mut entries {
        entry.data = private_out(state, std::mem::take(&mut entry.data));
    }
    entries
}

/// `data` without the private values the unit knows: the serial, the sign-in tokens and the
/// account names. Stock boot prints the serial (`SN_U:`, `SN_S:`, `SN_B:`), and a log line
/// of an extension could hold anything the unit knows.
pub(super) fn private_out(state: &State, mut data: Vec<u8>) -> Vec<u8> {
    let settings = state.settings.settings();
    let serial = state.info.serial.as_ref().map(|serial| serial.as_str());
    let secrets = [
        (serial.unwrap_or_default(), "<serial>"),
        (settings.tokens.access_token.expose(), "<token>"),
        (settings.tokens.refresh_token.expose(), "<token>"),
        (settings.account.username.as_str(), "<account>"),
        (settings.account.ui_username.as_str(), "<account>"),
    ];
    for (secret, mark) in secrets {
        data = redact(data, secret, mark);
    }
    data
}

/// `data` with every copy of `secret` replaced by `mark`. An empty value (signed out, no
/// serial) matches nothing.
fn redact(data: Vec<u8>, secret: &str, mark: &str) -> Vec<u8> {
    let secret = secret.as_bytes();
    if secret.is_empty() {
        return data;
    }
    let mut out = Vec::with_capacity(data.len());
    let mut rest = data.as_slice();
    while let Some(at) = (rest.windows(secret.len())).position(|window| window == secret) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(mark.as_bytes());
        rest = &rest[at + secret.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// The numbers of the newest [`BOOT_LOGS`] boot logs, oldest first. The watchdog logs beside
/// them (`<n>-watchdog-mmcblk0p1.log`) are left out.
fn boot_logs(files: &mut impl Files) -> Vec<u32> {
    let listed = files.list_dir(Path::new(BOOT_LOG_DIR)).ok().flatten();
    let mut numbers: Vec<u32> = (listed.into_iter().flatten())
        .filter(|entry| !entry.is_dir)
        .filter_map(|entry| entry.name.strip_suffix(BOOT_LOG_SUFFIX)?.parse().ok())
        .collect();
    numbers.sort_unstable();
    numbers.split_off(numbers.len().saturating_sub(BOOT_LOGS))
}

/// `tetrad-support-YYYY-MM-DD-HHMM.zip` (UTC), else by boot number without a clock.
pub fn file_name(clock: Option<u64>, boot: Option<u32>) -> String {
    match (clock, boot) {
        (Some(seconds), _) => {
            let [year, month, day, hour, minute, _] = utc_parts(seconds as i64);
            format!("tetrad-support-{year:04}-{month:02}-{day:02}-{hour:02}{minute:02}.zip")
        }
        (None, Some(boot)) => format!("tetrad-support-boot{boot}.zip"),
        (None, None) => "tetrad-support.zip".to_owned(),
    }
}

/// A short hash of the serial, so that two reports from one unit match without the serial.
fn unit_id(serial: &str) -> String {
    let digest = Sha256::digest(serial.as_bytes());
    digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `2026-10-27 20:41:07 UTC`.
pub fn utc(seconds: u64) -> String {
    let [year, month, day, hour, minute, second] = utc_parts(seconds as i64);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn summary(state: &State, context: &Context<'_>, files: &mut impl Files) -> String {
    let mut out = String::from("Tetrad support file (docs/support.md)\n\n");
    let mut line = |label: &str, value: &dyn std::fmt::Display| {
        let _ = writeln!(out, "{label:<16}{value}");
    };
    let created = context.clock().map_or("clock not set".to_owned(), utc);
    line("created", &created);
    line("tetrad", &env!("CARGO_PKG_VERSION"));
    let armed = (files
        .read_bounded(Path::new(SELECT_FILE), 64)
        .ok()
        .flatten())
    .map(|md5| String::from_utf8_lossy(&md5).trim().to_owned())
    .unwrap_or_else(|| "not armed".to_owned());
    line("binary md5", &armed);
    line("CorOS", &tetrad_firmware::running().version);
    line(
        "unit",
        &format!("{:?} ({:?})", state.product, state.platform),
    );
    let unit = (state.info.serial.as_ref())
        .map(|serial| unit_id(serial.as_str()))
        .unwrap_or_else(|| "no serial".to_owned());
    line("unit id", &unit);
    for (label, value) in [
        ("kernel", &state.info.kernel),
        ("u-boot", &state.info.uboot),
    ] {
        line(label, &value.as_deref().unwrap_or("unknown"));
    }
    let boot = (state.boot_count).map_or("unknown".to_owned(), |boot| boot.to_string());
    line("boot", &boot);
    line("uptime", &format!("{} s", context.now.as_secs()));
    line("wi-fi", &format!("{:?}", context.wifi));
    line("online", &yes_no(context.online));
    line("signed in", &yes_no(context.signed_in));
    line("cortex control", &yes_no(Some(state.control_connected)));
    let gib = |bytes: u64| bytes as f64 / f64::from(1 << 30);
    let disk = (context.disk).map_or("unknown".to_owned(), |disk| {
        format!(
            "{:.2} GiB free of {:.2} GiB",
            gib(disk.available),
            gib(disk.total)
        )
    });
    line("storage", &disk);
    let fault = (state.dsp_fault.as_ref()).map_or("none".to_owned(), |f| format!("{:?}", f.cause));
    line("DSP fault", &fault);
    let preset = (state.current.as_ref()).map_or("none".to_owned(), |current| {
        current
            .path
            .clone()
            .unwrap_or_else(|| "new preset (not saved)".to_owned())
    });
    line("preset", &preset);
    out
}

fn licences(state: &State) -> String {
    let mut out = String::from("plugin  status      models  example\n");
    let mut ids = LICENSABLE_PLUGINS;
    ids.sort_unstable();
    for id in ids {
        let models: Vec<&str> = (state.specs.iter())
            .flat_map(|specs| specs.models.values())
            .filter(|model| model.plugin_id == Some(id))
            .map(|model| model.name.as_str())
            .collect();
        let status = match state.licences.status(id) {
            PluginStatus::Unlicensed => "Unlicensed",
            PluginStatus::Inactive => "Inactive",
            PluginStatus::Active => "Active",
        };
        let example = models.first().copied().unwrap_or("-");
        let _ = writeln!(out, "{id:<8}{status:<12}{:<8}{example}", models.len());
    }
    let ids = |list: &[i32]| match list.is_empty() {
        true => "none".to_owned(),
        false => list
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
    };
    let licenses = &state.settings.settings().licenses;
    let bit = if state.licences.active() {
        "set"
    } else {
        "clear"
    };
    let _ = writeln!(out, "\nactivation bit (mask bit 62): {bit}");
    let _ = writeln!(out, "stored previous: {}", ids(&licenses.previous));
    let _ = writeln!(out, "new (not acknowledged): {}", ids(&licenses.new));
    let stored = if licenses.payload.is_empty() {
        "no"
    } else {
        "yes"
    };
    let _ = writeln!(out, "licence data stored: {stored}");
    out
}

fn catalog(state: &State) -> String {
    let Some(specs) = &state.specs else {
        return "the catalog is not loaded\n".to_owned();
    };
    let models = specs.models.values();
    let count = |keep: fn(&&tetrad_catalog::ModelSpec) -> bool| models.clone().filter(keep).count();
    let mut out = String::new();
    let _ = writeln!(out, "models            {}", specs.models.len());
    let _ = writeln!(out, "hidden            {}", count(|m| m.hidden));
    let _ = writeln!(out, "internal          {}", count(|m| m.internal));
    let _ = writeln!(
        out,
        "plugin models     {}",
        count(|m| m.plugin_id.is_some())
    );
    let _ = writeln!(
        out,
        "community models  {}",
        count(|m| m.community.is_some())
    );
    out
}

fn preset(state: &State) -> String {
    let Some(current) = state.current.as_ref() else {
        return "no preset is loaded\n".to_owned();
    };
    let mut out = String::new();
    let name = current.preset.name.as_deref().unwrap_or("");
    let _ = writeln!(
        out,
        "path   {}",
        current.path.as_deref().unwrap_or("(new preset)")
    );
    let _ = writeln!(out, "name   {name}");
    let scene = current.deployment.scene();
    let _ = writeln!(out, "scene  {}\n", scene.index() + 1);
    let _ = writeln!(out, "row  col  model   plugin  bypassed  name");
    for (site, block) in current.deployment.layout().sites() {
        let Site::Slot(slot) = site else { continue };
        let spec = state
            .specs
            .as_ref()
            .and_then(|specs| specs.model(block.model.0));
        let plugin =
            (spec.and_then(|spec| spec.plugin_id)).map_or("-".to_owned(), |id| id.to_string());
        let name = spec.map_or("(not in the catalog)", |spec| spec.name.as_str());
        let bypassed = if block.bypass.at(scene) { "yes" } else { "no" };
        let (row, column, model) = (slot.row.index() + 1, slot.column.index() + 1, block.model.0);
        let _ = writeln!(
            out,
            "{row:<5}{column:<5}{model:<8}{plugin:<8}{bypassed:<10}{name}"
        );
    }
    out
}

/// Installed extensions with their blocks, state and newest log lines. Their settings stay
/// out: an extension could keep a key there.
fn extensions(context: &Context<'_>) -> String {
    let mut out = String::new();
    for extension in context.extensions {
        let run = match &extension.state {
            ExtensionState::Disabled => "disabled".to_owned(),
            ExtensionState::Starting => "starting".to_owned(),
            ExtensionState::Running => "running".to_owned(),
            ExtensionState::Faulted(reason) => format!("faulted: {reason}"),
        };
        let _ = writeln!(
            out,
            "{} {} ({}, {})",
            extension.name, extension.version, extension.key, run
        );
        for block in &extension.blocks {
            let _ = writeln!(out, "  block {}: {}", block.name, block.detail);
        }
        let skip = extension.log.len().saturating_sub(EXTENSION_LOG_LINES);
        for line in &extension.log[skip..] {
            let _ = writeln!(out, "  log {line}");
        }
    }
    if context.extensions.is_empty() {
        out.push_str("no extensions installed\n");
    }
    out
}

fn dsp(state: &State) -> String {
    let mut out = String::new();
    let load = state
        .cpu
        .percent()
        .map_or("unknown".to_owned(), |percent| format!("{percent}%"));
    let _ = writeln!(out, "mean load of the 4 cores: {load}\n");
    let _ = writeln!(
        out,
        "core  cpu av/pk (%)        dm/pm/sd heap (%)    dropped  fp exc"
    );
    let cores = state.diagnostics.cores.iter().enumerate();
    for (index, core) in cores {
        let Some(core) = core else {
            let _ = writeln!(
                out,
                "{:<6}not read (open Settings → Diagnostics → DSP)",
                index + 1
            );
            continue;
        };
        let [a, b, c] = core.cpu;
        let [dm, pm, sd] = core.heap;
        let cpu = format!("{a:.1} {b:.1} {c:.1}");
        let heap = format!("{dm:.1} {pm:.1} {sd:.1}");
        let _ = writeln!(
            out,
            "{:<6}{cpu:<21}{heap:<21}{:<9}{}",
            index + 1,
            core.dropped_messages,
            core.fp_exceptions
        );
    }
    out
}

/// The notices in memory, oldest first, with the time of day once the clock is set.
fn notices(state: &State, context: &Context<'_>) -> String {
    let notices = &state.notices;
    let mut out = format!(
        "{} dropped before these; the last one repeated {} more times\n\n",
        notices.dropped, notices.repeats
    );
    for notice in notices.iter() {
        let ago = context.now.saturating_sub(notice.at).as_secs();
        let when = context
            .clock()
            .map_or(String::new(), |now| utc(now.saturating_sub(ago)) + "  ");
        let at = notice.at.as_secs_f64();
        let _ = writeln!(
            out,
            "{when}[+{at:.3} s] {:?}: {}",
            notice.subsystem, notice.detail
        );
    }
    out
}

/// A zip archive of `entries` (deflate, UTF-8 names, no zip64: the file stays far below
/// 4 GiB). `clock` stamps every entry; without one, 1980-01-01.
pub fn zip(entries: &[Entry], clock: Option<u64>) -> Vec<u8> {
    let (time, date) = clock.map_or((0, 0x21), |seconds| {
        let [year, month, day, hour, minute, second] = utc_parts(seconds as i64);
        let time = ((hour << 11) | (minute << 5) | (second / 2)) as u16;
        let date = (((year - 1980) << 9) | (month << 5) | day) as u16;
        (time, date)
    });
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let mut crc = flate2::Crc::new();
        crc.update(&entry.data);
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        let packed = (encoder.write_all(&entry.data))
            .and_then(|()| encoder.finish())
            .expect("deflate into a Vec cannot fail");
        let name = entry.name.as_bytes();
        // Version 2.0, bit 11 (UTF-8 name), method 8 (deflate).
        let fields = |out: &mut Vec<u8>| {
            for half in [20_u16, 0x0800, 8, time, date] {
                out.extend_from_slice(&half.to_le_bytes());
            }
            for word in [crc.sum(), packed.len() as u32, entry.data.len() as u32] {
                out.extend_from_slice(&word.to_le_bytes());
            }
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
        };
        let offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        fields(&mut out);
        out.extend_from_slice(name);
        out.extend_from_slice(&packed);

        central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        central.extend_from_slice(&20_u16.to_le_bytes());
        fields(&mut central);
        // Comment length, disk, internal and external attributes, then the local header.
        for half in [0_u16, 0, 0] {
            central.extend_from_slice(&half.to_le_bytes());
        }
        central.extend_from_slice(&0_u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let start = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    let count = entries.len() as u16;
    for half in [0_u16, 0, count, count] {
        out.extend_from_slice(&half.to_le_bytes());
    }
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use tetrad_hal::platform::{Platform as HalPlatform, Product};
    use tetrad_services::fake::MemFiles;
    use tetrad_settings::account::Secret;
    use tetrad_settings::device::{DeviceSettings, Hardware};
    use tetrad_settings::{SettingsFileId, SettingsStorage};

    use super::*;

    struct NoFiles;

    impl SettingsStorage for NoFiles {
        fn read(&self, _: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn write(&mut self, _: SettingsFileId, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn remove(&mut self, _: SettingsFileId) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn state() -> State {
        let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
        let mut state = State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings);
        state.boot_count = Some(10);
        state
    }

    fn context(extensions: &[InstalledExtension]) -> Context<'_> {
        Context {
            now: Duration::from_secs(60),
            wall: Some(UNIX_EPOCH + Duration::from_secs(1_793_133_667)),
            online: Some(true),
            signed_in: Some(true),
            wifi: WifiStatus::InternetConnected,
            disk: None,
            extensions,
        }
    }

    /// Reads every entry back: a small reader of the central directory, so the archive is
    /// checked as an unzip tool walks it (CRC included).
    fn unzip(zip: &[u8]) -> Vec<Entry> {
        let u16_at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]) as usize;
        let u32_at = |at: usize| u32::from_le_bytes(zip[at..at + 4].try_into().unwrap()) as usize;
        let end = zip.len() - 22;
        assert_eq!(u32_at(end), 0x0605_4b50);
        let (count, mut at) = (u16_at(end + 10), u32_at(end + 16));
        let mut entries = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(at), 0x0201_4b50);
            let (crc, packed, size) = (u32_at(at + 16), u32_at(at + 20), u32_at(at + 24));
            let name_len = u16_at(at + 28);
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            let local = u32_at(at + 42);
            assert_eq!(u32_at(local), 0x0403_4b50);
            let start = local + 30 + u16_at(local + 26) + u16_at(local + 28);
            let mut data = Vec::new();
            flate2::read::DeflateDecoder::new(&zip[start..start + packed])
                .read_to_end(&mut data)
                .unwrap();
            assert_eq!(data.len(), size);
            let mut check = flate2::Crc::new();
            check.update(&data);
            assert_eq!(check.sum() as usize, crc);
            entries.push(Entry { name, data });
            at += 46 + name_len;
        }
        entries
    }

    #[test]
    fn support_file_is_a_readable_zip() {
        let entries = vec![
            Entry {
                name: "summary.txt".into(),
                data: b"tetrad 0.1.0\n".repeat(50),
            },
            Entry {
                name: "logs/boot-é.log".into(),
                data: Vec::new(),
            },
        ];
        assert_eq!(unzip(&zip(&entries, Some(1_793_133_667))), entries);
        assert_eq!(unzip(&zip(&entries, None)), entries);
    }

    /// The promise of docs/support.md: secrets in the settings and the serial never reach the
    /// file, while the logs and the unit id do.
    #[test]
    fn support_file_holds_no_secret() {
        const MARKERS: [&str; 5] = [
            "MARKER-ACCESS",
            "MARKER-REFRESH",
            "MARKER-USER",
            "MARKER-LICENCE",
            "QC1234567",
        ];
        let mut state = state();
        state.settings.update(|s| {
            s.tokens.access_token = Secret::new(MARKERS[0]);
            s.tokens.refresh_token = Secret::new(MARKERS[1]);
            s.account.username = MARKERS[2].into();
            s.account.ui_username = MARKERS[2].into();
            s.licenses.payload = MARKERS[3].as_bytes().to_vec().into();
            s.licenses.previous = vec![5, 15];
        });
        state.info.serial = tetrad_media::factory::DeviceSerial::parse(MARKERS[4].as_bytes()).ok();
        assert!(state.info.serial.is_some());
        let mut files = MemFiles::default();
        files.put(
            "/media/p4/logs/system/902-mmcblk0p1.log",
            "SN_U: QC1234567\nlogin MARKER-USER MARKER-ACCESS\n[1.0s] Services: Wi-Fi associated\n",
        );
        let extensions = [];
        let zip = zip(&collect(&state, &context(&extensions), &mut files), None);
        let entries = unzip(&zip);
        for entry in &entries {
            let text = String::from_utf8_lossy(&entry.data);
            for marker in MARKERS {
                assert!(!text.contains(marker), "{} holds {marker}", entry.name);
            }
        }
        let text = |name: &str| {
            let entry = entries.iter().find(|entry| entry.name == name).unwrap();
            String::from_utf8_lossy(&entry.data).into_owned()
        };
        assert!(text("summary.txt").contains(&unit_id(MARKERS[4])));
        assert!(text("licences.txt").contains("stored previous: 5, 15"));
        let log = text("logs/boot-902.log");
        assert!(log.contains("SN_U: <serial>\nlogin <account> <token>\n"));
        assert!(log.contains("Wi-Fi associated"));
    }

    /// The Remote reply time and memory: a runaway log goes in as its end only, and only the
    /// newest [`BOOT_LOGS`] boot logs are read, by their own counter (not the boot count).
    #[test]
    fn boot_logs_are_tails_within_the_cap() {
        let state = state();
        let mut files = MemFiles::default();
        let mut big = vec![b'x'; 2 * LOG_TAIL as usize];
        big.extend_from_slice(b"last line\n");
        files.put("/media/p4/logs/system/903-mmcblk0p1.log", big);
        for n in [3, 899, 900, 901, 902] {
            files.put(
                &format!("/media/p4/logs/system/{n}-mmcblk0p1.log"),
                format!("{n}\n"),
            );
        }
        files.put(
            "/media/p4/logs/system/903-watchdog-mmcblk0p1.log",
            "watchdog\n",
        );
        files.put(
            "/media/p4/tetrad/launcher/launch.log",
            "boot selector: Tetrad\n",
        );
        let extensions = [];
        let entries = collect(&state, &context(&extensions), &mut files);
        let logs: Vec<&str> = (entries.iter())
            .map(|e| e.name.as_str())
            .filter(|name| name.starts_with("logs/"))
            .collect();
        assert_eq!(
            logs,
            [
                "logs/boot-899.log",
                "logs/boot-900.log",
                "logs/boot-901.log",
                "logs/boot-902.log",
                "logs/boot-903.log",
                "logs/launch.log",
            ]
        );
        let log = &entries
            .iter()
            .find(|e| e.name == "logs/boot-903.log")
            .unwrap()
            .data;
        assert_eq!(log.len() as u64, LOG_TAIL);
        assert!(log.ends_with(b"last line\n"));
    }
}
