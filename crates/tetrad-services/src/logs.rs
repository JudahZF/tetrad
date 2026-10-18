//! The support log archive of SEND REPORT and Cortex Control Logs (44). Stock `0x487f88` runs
//! `tar -c -f /tmp/logs-<id>.tar /media/p4/logs/* /media/p4/ZenjackConfig.ini` and `gzip`
//! (string `0xa471b0`), then the upload PUTs the file (`0x6c0ab4`). O: Tetrad packs in memory
//! and redacts every member and member name first, so no token, key, serial, creator stamp or
//! Wi-Fi credential leaves the unit. The logs API key (`generateLogUrl`) is not a member and
//! stays an injected input of `CloudApi::log_upload_urls`.
//!
//! The redaction is two linear passes into zeroed buffers (review r25 Q3), but 32 MiB of logs
//! still take a while: run [`pack_logs`] on a worker, not the event loop.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use zeroize::Zeroizing;

use crate::backup::{BackupError, Packer};
use crate::exec::{FileError, Files};

/// The stock log set (`0x487f88`): the log tree and the settings file.
pub const LOG_DIR: &str = "/media/p4/logs";
pub const SETTINGS_FILE: &str = "/media/p4/ZenjackConfig.ini";
/// What a redacted value becomes.
pub const REDACTED: &[u8] = b"[redacted]";
/// O: the most input bytes one archive takes; a larger log set is refused.
pub const MAX_LOG_BYTES: u64 = 32 << 20;
/// O: shorter known values are not matched (they would hit ordinary text).
const MIN_SECRET_LEN: usize = 4;
/// O: the shortest base64 run of a known value taken from inside a longer base64 text.
const MIN_CORE_LEN: usize = 8;

/// Value names whose value is a secret (lowercase, letters and digits only), matched as a part
/// of the name: tokens, passwords, Wi-Fi credentials, the serial, the creator stamp, seeds,
/// keys, licences, cookies and sessions.
const SECRET_NAMES: [&str; 20] = [
    "token",
    "password",
    "passphrase",
    "psk",
    "wepkey",
    "ssid",
    "secret",
    "serial",
    "stamp",
    "seed",
    "cipher",
    "signature",
    "licence",
    "license",
    "credential",
    "cookie",
    "session",
    "creator",
    "authorization",
    "activationcode",
];
/// Secret only as the whole name: as a part of a name they are ordinary words (`author`,
/// `errorcode`).
const SECRET_WORDS: [&str; 2] = ["auth", "code"];

#[derive(Debug, thiserror::Error)]
pub enum LogPackError {
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Archive(#[from] BackupError),
    #[error("the log set is larger than {MAX_LOG_BYTES} bytes")]
    TooLarge,
}

/// Secret values the caller knows (account tokens, the serial, the creator stamp, saved Wi-Fi
/// passphrases, key material). Each is removed in every form [`Secrets::add`] lists.
#[derive(Default)]
pub struct Secrets(Vec<Zeroizing<Vec<u8>>>);

impl Secrets {
    /// Adds `value` as bytes; as hex, also with `:` or space between bytes, and with a `0x`
    /// before each byte and `, `, `,` or space between; percent-encoded (every byte outside
    /// the unreserved set, or with `/` and `:` left plain as `QUrlQuery` does, or with space
    /// as `+` as a form does); JSON-escaped (`\"`, `\\`, `\u00xx`, `/` plain or `\/`); as
    /// base64 and base64url, padded or not; and as the base64 runs it makes inside a longer
    /// base64 text (the value at byte offset 0, 1 or 2 of a 3-byte group). Matching ignores
    /// ASCII case.
    pub fn add(&mut self, value: impl AsRef<[u8]>) {
        let value = value.as_ref();
        if value.len() < MIN_SECRET_LEN {
            return;
        }
        let hex = |separator: &str| {
            let pairs: Vec<String> = value.iter().map(|byte| format!("{byte:02x}")).collect();
            Zeroizing::new(pairs.join(separator).into_bytes())
        };
        let prefixed = |separator: &str| {
            let bytes: Vec<String> = value.iter().map(|byte| format!("0x{byte:02x}")).collect();
            Zeroizing::new(bytes.join(separator).into_bytes())
        };
        let mut forms = vec![
            Zeroizing::new(value.to_vec()),
            hex(""),
            hex(":"),
            hex(" "),
            prefixed(", "),
            prefixed(","),
            prefixed(" "),
            percent_encoded(value, b"", false),
            percent_encoded(value, b"/:", false),
            percent_encoded(value, b"", true),
            json_escaped(value, false),
            json_escaped(value, true),
        ];
        for engine in [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD] {
            forms.push(Zeroizing::new(engine.encode(value).into_bytes()));
        }
        for offset in 0..3 {
            let mut shifted = Zeroizing::new(vec![0; offset]);
            shifted.extend_from_slice(value);
            // The first group holds `offset` bytes of other text; only whole groups of the
            // value give the same characters wherever the value sits.
            let skip = if offset == 0 { 0 } else { 3 };
            let whole = (shifted.len() - skip) / 3 * 3;
            for engine in [STANDARD_NO_PAD, URL_SAFE_NO_PAD] {
                let core = engine.encode(&shifted[skip..skip + whole]);
                if core.len() >= MIN_CORE_LEN {
                    forms.push(Zeroizing::new(core.into_bytes()));
                }
            }
        }
        forms.sort_by(|a, b| a.as_slice().cmp(b.as_slice()));
        forms.dedup_by(|a, b| a.as_slice() == b.as_slice());
        self.0.extend(forms);
    }
}

/// `value` with every byte outside the URL unreserved set and `plain` as `%XX`; a space as
/// `+` when `space_plus`.
fn percent_encoded(value: &[u8], plain: &[u8], space_plus: bool) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(value.len() * 3));
    for &byte in value {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || plain.contains(&byte) {
            out.push(byte);
        } else if space_plus && byte == b' ' {
            out.push(b'+');
        } else {
            out.extend_from_slice(format!("%{byte:02X}").as_bytes());
        }
    }
    out
}

/// `value` as the text of a JSON string: `"` and `\` escaped, control bytes as `\u00xx`, `/`
/// as `\/` when `slash`; other bytes as they are.
fn json_escaped(value: &[u8], slash: bool) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(value.len() * 2));
    for &byte in value {
        match byte {
            b'"' | b'\\' => out.extend_from_slice(&[b'\\', byte]),
            b'/' if slash => out.extend_from_slice(b"\\/"),
            0..=0x1f => out.extend_from_slice(format!("\\u{byte:04x}").as_bytes()),
            _ => out.push(byte),
        }
    }
    out
}

/// A growing output that never leaves a plain copy behind: a full buffer moves into a larger
/// zeroed one and the old one is zeroed on drop (a `Vec` reallocation would not zero it).
struct Out(Zeroizing<Vec<u8>>);

impl Out {
    fn with_capacity(capacity: usize) -> Self {
        Self(Zeroizing::new(Vec::with_capacity(capacity)))
    }

    fn extend(&mut self, bytes: &[u8]) {
        let needed = self.0.len() + bytes.len();
        if needed > self.0.capacity() {
            let mut grown = Zeroizing::new(Vec::with_capacity(needed.max(self.0.capacity() * 2)));
            grown.extend_from_slice(&self.0);
            self.0 = grown;
        }
        self.0.extend_from_slice(bytes);
    }
}

/// The known forms by their ASCII-lowercase first byte, longest first.
struct Known<'a>(Vec<Vec<&'a [u8]>>);

impl<'a> Known<'a> {
    fn new(secrets: &'a Secrets) -> Self {
        let mut buckets = vec![Vec::new(); 256];
        for form in &secrets.0 {
            buckets[usize::from(form[0].to_ascii_lowercase())].push(form.as_slice());
        }
        for bucket in &mut buckets {
            bucket.sort_by_key(|form: &&[u8]| std::cmp::Reverse(form.len()));
        }
        Self(buckets)
    }

    /// The length of the longest known form at the start of `rest`.
    fn longest(&self, rest: &[u8]) -> Option<usize> {
        let first = rest.first()?;
        self.0[usize::from(first.to_ascii_lowercase())]
            .iter()
            .find(|form| {
                rest.get(..form.len())
                    .is_some_and(|w| w.eq_ignore_ascii_case(form))
            })
            .map(|form| form.len())
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    (haystack.get(from..)?)
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|at| at + from)
}

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-_.~+/=".contains(&byte)
}

fn secret_name(name: &[u8]) -> bool {
    let name: Vec<u8> = (name.iter())
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(u8::to_ascii_lowercase)
        .collect();
    let text = String::from_utf8_lossy(&name);
    SECRET_NAMES.iter().any(|secret| text.contains(secret))
        || SECRET_WORDS.contains(&text.as_ref())
        || (text.len() > 3 && text.ends_with("key"))
}

/// `data` with every secret replaced by [`REDACTED`]. First pass: the known [`Secrets`], the
/// longest form at each position. Second pass:
/// 1. PEM blocks (`-----BEGIN` … `-----END …-----`);
/// 2. `Bearer <token>`, `Basic <token>` and JWTs (`eyJ…` with two dots);
/// 3. the value after a secret name and `=` or `:` (INI, JSON, headers, query strings): quoted,
///    or bare to the line end when the name starts its line (an INI `name=value`), else up to a
///    `,`, `;`, `&`, `}` or the line end. Spaces do not end a bare value (review r26 S2: a
///    mid-line `Authorization: Bearer <token>` or a multi-word value is redacted whole).
///
/// Each pass is linear in the input.
pub fn redact(data: &[u8], secrets: &Secrets) -> Zeroizing<Vec<u8>> {
    let known = Known::new(secrets);
    let mut plain = Out::with_capacity(data.len() + data.len() / 4);
    let mut at = 0;
    while at < data.len() {
        match known.longest(&data[at..]) {
            Some(len) => {
                plain.extend(REDACTED);
                at += len;
            }
            None => {
                plain.extend(&data[at..=at]);
                at += 1;
            }
        }
    }
    redact_patterns(&plain.0)
}

fn redact_patterns(data: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut out = Out::with_capacity(data.len() + data.len() / 4);
    let mut index = 0;
    let mut line_start = true;
    while index < data.len() {
        let start = index;
        let rest = &data[start..];
        if rest.len() >= 10 && rest[..10].eq_ignore_ascii_case(b"-----BEGIN") {
            // No end: the rest is redacted, so a run of `BEGIN`s is scanned once.
            index = find(data, b"-----END", start)
                .and_then(|end| find(data, b"-----", end + 8))
                .map_or(data.len(), |dashes| dashes + 5);
            out.extend(REDACTED);
            line_start = false;
            continue;
        }
        let scheme = [&b"bearer "[..], b"basic "].into_iter().find(|scheme| {
            (rest.get(..scheme.len())).is_some_and(|word| word.eq_ignore_ascii_case(scheme))
        });
        if let Some(scheme) = scheme {
            let value = start + scheme.len();
            out.extend(&data[start..value]);
            out.extend(REDACTED);
            index = value + data[value..].iter().take_while(|&&b| token_byte(b)).count();
            line_start = false;
            continue;
        }
        if rest.starts_with(b"eyJ") && (start == 0 || !token_byte(data[start - 1])) {
            let end = start + rest.iter().take_while(|&&b| token_byte(b)).count();
            if data[start..end].iter().filter(|&&b| b == b'.').count() >= 2 {
                out.extend(REDACTED);
                index = end;
                line_start = false;
                continue;
            }
        }
        let byte = data[start];
        if byte.is_ascii_alphanumeric() {
            let name_end = start
                + (rest.iter())
                    .take_while(|&&b| b.is_ascii_alphanumeric() || b"_-/".contains(&b))
                    .count();
            out.extend(&data[start..name_end]);
            index = name_end;
            if secret_name(&data[start..name_end])
                && let Some((value, end)) = value_after(data, name_end, line_start)
            {
                out.extend(&data[name_end..value]);
                out.extend(REDACTED);
                index = end;
            }
            line_start = false;
            continue;
        }
        out.extend(&data[start..=start]);
        line_start = byte == b'\n' || (line_start && b" \t\"'".contains(&byte));
        index += 1;
    }
    out.0
}

/// The value after a name at `at`: optional closing quote and spaces, `=` or `:`, spaces, then
/// a quoted value (without its quotes) or a bare one, to the line end when the name started
/// its line. Returns its start and end.
fn value_after(data: &[u8], mut at: usize, line_start: bool) -> Option<(usize, usize)> {
    let skip = |at: &mut usize, set: &[u8]| {
        while data.get(*at).is_some_and(|byte| set.contains(byte)) {
            *at += 1;
        }
    };
    skip(&mut at, b"\"' \t");
    if !matches!(data.get(at), Some(b'=' | b':')) {
        return None;
    }
    at += 1;
    skip(&mut at, b" \t");
    let quote = data.get(at).copied().filter(|byte| b"\"'".contains(byte));
    if quote.is_some() {
        at += 1;
    }
    let end = at
        + data[at..]
            .iter()
            .take_while(|&&byte| match quote {
                Some(quote) => byte != quote && byte != b'\n',
                None if line_start => !b"\r\n".contains(&byte),
                None => !b"\r\n,;&}".contains(&byte),
            })
            .count();
    Some((at, end))
}

/// The regular files under `dir`, depth first (symlinks are left out by `Files::list_dir`).
fn collect(files: &mut impl Files, dir: &Path, paths: &mut Vec<PathBuf>) -> Result<(), FileError> {
    for entry in files.list_dir(dir)?.unwrap_or_default() {
        let path = dir.join(&entry.name);
        if entry.is_dir {
            collect(files, &path, paths)?;
        } else {
            paths.push(path);
        }
    }
    Ok(())
}

/// The stock log set as one in-memory `.tar.gz` (members named as GNU tar stores absolute
/// paths, without the leading `/`). Each member and its name pass through [`redact`]; one
/// member at a time is held in plain form, read with the bound that is left.
pub fn pack_logs(files: &mut impl Files, secrets: &Secrets) -> Result<Vec<u8>, LogPackError> {
    let mut paths = Vec::new();
    collect(files, Path::new(LOG_DIR), &mut paths)?;
    paths.push(PathBuf::from(SETTINGS_FILE));
    let mut packer = Packer::default();
    let mut left = MAX_LOG_BYTES;
    for path in paths {
        let Some(bytes) = files.read_bounded(&path, left)? else {
            continue;
        };
        let bytes = Zeroizing::new(bytes);
        left = left
            .checked_sub(bytes.len() as u64)
            .ok_or(LogPackError::TooLarge)?;
        let path = path.display().to_string();
        let name = redact(path.trim_start_matches('/').as_bytes(), secrets);
        packer.add(&String::from_utf8_lossy(&name), &redact(&bytes, secrets))?;
    }
    Ok(packer.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::MemFiles;

    fn archive_text(archive: &[u8]) -> String {
        let mut plain = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(archive), &mut plain).unwrap();
        String::from_utf8_lossy(&plain).into_owned()
    }

    fn redacted(text: &str, values: &[&[u8]]) -> String {
        let mut secrets = Secrets::default();
        for value in values {
            secrets.add(value);
        }
        String::from_utf8_lossy(&redact(text.as_bytes(), &secrets)).into_owned()
    }

    /// Each secret kind leaves the archive: known tokens, serial and creator stamp (also as
    /// hex and base64), a JWT, a bearer header, Wi-Fi credentials and keys by name (INI, JSON,
    /// query string), and a PEM key. Ordinary log text and settings stay.
    #[test]
    fn the_log_archive_redacts_each_secret_kind() {
        let serial = "Q2-SYNTH-0042";
        let stamp = [0xa5u8; 21];
        let refresh = "synthetic-refresh-value";
        let mut secrets = Secrets::default();
        for value in [serial.as_bytes(), &stamp, refresh.as_bytes()] {
            secrets.add(value);
        }
        let stamp_hex: String = stamp.iter().map(|byte| format!("{byte:02x}")).collect();
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1In0.c2ln";
        let log = format!(
            "boot ok, unit {serial}\nstamp {stamp_hex}\nrefresh {refresh}\nreply {jwt} end\n\
             Authorization: Bearer opaque.bearer-value\n{{\"access_token\": \"tok-1\", \
             \"name\": \"Crunch\"}}\nGET /x?api_key=k-123&page=2\npsk=\"wifi pass\" ssid=Home\n\
             -----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\nsigning_key: 0011\n"
        );
        let mut files = MemFiles::default();
        files.put(&format!("{LOG_DIR}/System/zenui.log"), log.as_str());
        files.put(
            SETTINGS_FILE,
            "[General]\ncustomName=My QC\n[Wifi]\npassphrase=hunter22\n",
        );

        let text = archive_text(&pack_logs(&mut files, &secrets).unwrap());
        for secret in [
            serial,
            &stamp_hex,
            refresh,
            jwt,
            "opaque.bearer-value",
            "tok-1",
            "k-123",
            "wifi pass",
            "Home",
            "AAAA",
            "0011",
            "hunter22",
        ] {
            assert!(!text.contains(secret), "{secret} left in the archive");
        }
        for kept in [
            "media/p4/logs/System/zenui.log",
            "media/p4/ZenjackConfig.ini",
            "boot ok, unit [redacted]",
            "\"name\": \"Crunch\"",
            "page=2",
            "customName=My QC",
        ] {
            assert!(text.contains(kept), "{kept} missing");
        }
    }

    /// Review r25 Q2 (a): a log file named with a known value keeps its content, not the name.
    #[test]
    fn member_names_lose_known_values() {
        let serial = "Q2-SYNTH-0042";
        let mut secrets = Secrets::default();
        secrets.add(serial);
        let mut files = MemFiles::default();
        files.put(&format!("{LOG_DIR}/crash-{serial}.log"), "crash dump");
        let text = archive_text(&pack_logs(&mut files, &secrets).unwrap());
        assert!(!text.contains(serial));
        assert!(text.contains("media/p4/logs/crash-[redacted].log"));
        assert!(text.contains("crash dump"));
    }

    /// Review r25 Q2 (b): activation codes, licences, cookies, sessions and `auth`/`code`
    /// values; `author` alone is an ordinary name.
    #[test]
    fn licence_cookie_session_and_code_values_are_redacted() {
        let text = redacted(
            "{\"activationCode\": \"AC-1\"}\nlicence=LIC-2\nCookie: sid=CK-3; path=/\n\
             sessionId=SS-4\nauth=AU-5\ncode=CD-6\nauthorName=Kept Name\n",
            &[],
        );
        for secret in ["AC-1", "LIC-2", "CK-3", "SS-4", "AU-5", "CD-6"] {
            assert!(!text.contains(secret), "{secret} left in {text}");
        }
        assert!(text.contains("authorName=Kept Name"));
    }

    /// Review r25 Q2 (c): QSettings writes INI values with inner spaces unquoted, so a value
    /// after a name that starts its line runs to the line end; a query string still stops at `&`.
    #[test]
    fn an_ini_value_is_redacted_to_the_line_end() {
        let text = redacted(
            "[Wifi]\npassphrase=my wifi pass\r\nnext=1\nGET /x?token=t-1&page=2\n",
            &[],
        );
        assert!(
            !text.contains("wifi pass") && !text.contains("t-1"),
            "{text}"
        );
        assert!(text.contains("next=1") && text.contains("page=2"), "{text}");
    }

    /// Review r26 S2: an auth scheme after a header name that does not start its line keeps
    /// no part of its token.
    #[test]
    fn a_mid_line_auth_header_is_redacted_whole() {
        let text = redacted(
            "t=1 Authorization: Bearer opaque-1
t=2 Proxy-Authorization: Basic b64-2
next
",
            &[],
        );
        assert!(
            !text.contains("opaque-1") && !text.contains("b64-2"),
            "{text}"
        );
        assert!(text.ends_with("\nnext\n"), "{text}");
    }

    /// Review r26 S2: a bare multi-word value after a secret name in mid-line loses every
    /// word, not only the first.
    #[test]
    fn a_mid_line_multi_word_value_is_redacted_whole() {
        let text = redacted(
            "wifi: psk=my wifi pass
ssid shown
",
            &[],
        );
        assert!(!text.contains("wifi pass"), "{text}");
        assert!(text.contains("ssid shown"), "{text}");
    }

    /// Review r25 Q2 (d): a known value inside a longer base64 text (offsets 1 and 2), as
    /// base64url, percent-encoded (any hex case) and as hex with `:` or space separators.
    #[test]
    fn encoded_forms_of_a_known_value_are_redacted() {
        let value = b"synthetic-refresh-value";
        let binary = [0xfb_u8, 0xef, 0xbe, 0xff, 0x3f, 0xa5];
        let password = b"pa ss/w+rd";
        let mut log = String::new();
        let mut cores = Vec::new();
        for (offset, prefix) in [(1, &b"x"[..]), (2, b"xy")] {
            let embedded = STANDARD.encode([prefix, value, b"tail"].concat());
            let skip = 3 - offset;
            let core = STANDARD_NO_PAD.encode(&value[skip..skip + (value.len() - skip) / 3 * 3]);
            assert!(embedded.contains(&core));
            log.push_str(&format!("blob {embedded}\n"));
            cores.push(core);
        }
        let url = URL_SAFE_NO_PAD.encode(binary);
        assert!(url.contains(['-', '_']));
        let colons = "FB:EF:BE:FF:3F:A5";
        let spaced = "fb ef be ff 3f a5";
        let percent = "pa%20ss%2fw%2Brd";
        // Review r26 S5: `QUrlQuery` (`/` and `:` plain), form (space as `+`), JSON escapes
        // and `0x` hex.
        let query = "pa%20ss/w%2Brd";
        let plus = "pa+ss%2Fw%2Brd";
        let quoted = br#"q"t\x/y"#;
        let json = r#"q\"t\\x\/y"#;
        let json_plain = r#"q\"t\\x/y"#;
        let control = b"line\x01feed";
        let unicode = r"line\u0001feed";
        let prefixed = "0xfb, 0xef, 0xbe, 0xff, 0x3f, 0xa5";
        log.push_str(&format!(
            "url {url}\ndump {colons}\ndump {spaced}\nq {percent}\nq {query}\nq {plus}\n\
             j {json}\nj {json_plain}\nj {unicode}\nc {prefixed}\n"
        ));
        let text = redacted(&log, &[value, &binary, password, quoted, control]);
        for form in cores.iter().map(String::as_str).chain([
            url.as_str(),
            colons,
            spaced,
            percent,
            query,
            plus,
            json,
            json_plain,
            unicode,
            prefixed,
        ]) {
            assert!(!text.contains(form), "{form} left in {text}");
        }
    }

    /// Review r25 Q3: a known value on every line of a log of [`MAX_LOG_BYTES`] goes in one
    /// linear pass, in seconds even in a debug build (the old splice per match moved about
    /// 10^13 bytes and stalled).
    #[test]
    fn a_large_log_with_many_matches_is_redacted_in_linear_time() {
        let serial = "Q2-SYNTH-0042";
        let mut secrets = Secrets::default();
        secrets.add(serial);
        let line = format!("2026-10-12 12:00:00 unit {serial} heartbeat ok\n");
        let log = line.repeat(MAX_LOG_BYTES as usize / line.len());
        let started = std::time::Instant::now();
        let out = redact(log.as_bytes(), &secrets);
        let elapsed = started.elapsed();
        assert!(!out.windows(serial.len()).any(|w| w == serial.as_bytes()));
        assert!(elapsed < std::time::Duration::from_secs(30), "{elapsed:?}");
    }
}
