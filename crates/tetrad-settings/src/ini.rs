//! Qt 4.8.7 `QSettings::IniFormat` codec, Unix build, no INI codec set.
//!
//! Mirrors `qsettings.cpp`: `readIniLine`, `readIniFile`, `readIniSection`,
//! `iniUnescapedKey`, `iniUnescapedStringList`, `iniEscapedKey`,
//! `iniEscapedString`, `iniEscapedStringList`, `writeIniFile` and the
//! key bookkeeping of `QConfFile` (original key positions, removed keys).
//! Keys are case-sensitive, as on Unix.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::variant::Variant;

/// One stored value as Qt's INI layer keeps it, before type conversion.
///
/// The text is kept exactly as Qt unescapes it, so a value the app never
/// touches is written back unchanged.
#[derive(Clone, Debug, PartialEq)]
pub enum IniValue {
    /// One string. `@Variant(..)`, `@ByteArray(..)`, `@Invalid()` and `@@`
    /// stay encoded; [`Variant::from_ini`] decodes them. Text is UTF-8, so a
    /// lone UTF-16 surrogate becomes U+FFFD and a value that held one changes
    /// on save (Qt keeps the unit). Stock files do not contain one.
    Text(String),
    /// A comma list (`QStringList`, or a `QVariantList` without exactly one
    /// item). Items keep their `@` encoding.
    List(Vec<String>),
}

/// A recoverable syntax problem. Qt sets `QSettings::FormatError` and keeps
/// every key it could read; so does [`SettingsFile::parse`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatIssue {
    /// A `[section` header without `]` on its line. Byte offset of the line.
    UnclosedSection(usize),
    /// A non-comment line without `=`. Byte offset of the line.
    MissingEquals(usize),
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    value: IniValue,
    /// Order in the file Qt last read. `None` for keys set since then.
    position: Option<u32>,
}

/// One Qt INI settings file in memory, with the `QSettings` key API.
///
/// Keys are full paths such as `MidiSettings/Channel`; keys of the
/// `[General]` section have no group. Writing follows Qt: sections and keys
/// read from a file keep their file order, keys set later come first in their
/// section in `QString` order.
#[derive(Clone, Default, PartialEq)]
pub struct SettingsFile {
    entries: BTreeMap<String, Entry>,
    /// File positions of keys removed since the last save. Qt keeps them, so
    /// a key removed and set again in one save window keeps its place.
    removed: BTreeMap<String, u32>,
    dirty: bool,
}

/// Shows key names only: some files hold credentials.
impl std::fmt::Debug for SettingsFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsFile")
            .field("keys", &self.entries.keys().collect::<Vec<_>>())
            .field("dirty", &self.dirty)
            .finish()
    }
}

impl SettingsFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses file bytes. Never fails: like Qt, bad lines are reported and
    /// skipped.
    pub fn parse(data: &[u8]) -> (Self, Vec<FormatIssue>) {
        let mut issues = Vec::new();
        // Section blocks in first-seen order; duplicates merge into the first.
        let mut blocks: Vec<(String, Vec<Line>)> = vec![(String::new(), Vec::new())];
        let mut current = 0;
        let mut pos = 0;
        while let Some(line) = read_line(data, &mut pos) {
            if data[line.start] != b'[' {
                blocks[current].1.push(line);
                continue;
            }
            let body = &data[line.start + 1..line.end];
            let raw = match body.iter().position(|&b| b == b']') {
                Some(close) => &body[..close],
                None => {
                    issues.push(FormatIssue::UnclosedSection(line.start));
                    body
                }
            };
            let raw = raw.trim_ascii();
            let name = if raw.eq_ignore_ascii_case(b"general") {
                String::new()
            } else if raw.eq_ignore_ascii_case(b"%general") {
                raw[1..].iter().map(|&b| char::from(b)).collect()
            } else {
                unescape_key(raw)
            };
            current = match blocks.iter().position(|(n, _)| *n == name) {
                Some(index) => index,
                None => {
                    blocks.push((name, Vec::new()));
                    blocks.len() - 1
                }
            };
        }

        let mut file = Self::new();
        let mut position = 0u32;
        for (section, lines) in blocks {
            for line in lines {
                let Some(equals) = line.equals else {
                    if data[line.start] != b';' {
                        issues.push(FormatIssue::MissingEquals(line.start));
                    }
                    continue;
                };
                let mut key_end = equals;
                while key_end > line.start && matches!(data[key_end - 1], b' ' | b'\t') {
                    key_end -= 1;
                }
                let key = unescape_key(&data[line.start..key_end]);
                let full = if section.is_empty() {
                    key
                } else {
                    format!("{section}/{key}")
                };
                let value = unescape_value(&data[equals + 1..line.end]);
                // QMap::insert keeps the first key object, so the first
                // position wins and the last value wins.
                file.entries
                    .entry(full)
                    .and_modify(|e| e.value = value.clone())
                    .or_insert(Entry {
                        value,
                        position: Some(position),
                    });
                position += 1;
            }
        }
        (file, issues)
    }

    /// Serializes like `writeIniFile` (LF line ends). Does not clear state;
    /// use [`Self::save`] after a successful write.
    pub fn to_bytes(&self) -> Vec<u8> {
        struct Section<'a> {
            name: &'a str,
            position: Option<u32>,
            keys: Vec<(&'a str, &'a Entry)>,
        }
        let mut sections: BTreeMap<&str, Section> = BTreeMap::new();
        for (full, entry) in &self.entries {
            let (name, key) = full.split_once('/').unwrap_or(("", full));
            let section = sections.entry(name).or_insert(Section {
                name,
                position: None,
                keys: Vec::new(),
            });
            // `-1 means infinity` for the section minimum.
            if let Some(p) = entry.position {
                section.position = Some(section.position.map_or(p, |s| s.min(p)));
            }
            section.keys.push((key, entry));
        }
        let mut sections: Vec<Section> = sections.into_values().collect();
        // QSettingsIniKey::operator< compares the int position (-1 first),
        // then the string.
        sections.sort_by(|a, b| qt_order(a.position, a.name, b.position, b.name));

        let mut out = Vec::new();
        for (index, section) in sections.iter_mut().enumerate() {
            if index != 0 {
                out.push(b'\n');
            }
            let mut header = Vec::new();
            escape_key(section.name, &mut header);
            if header.is_empty() {
                out.extend_from_slice(b"[General]");
            } else if header.eq_ignore_ascii_case(b"general") {
                out.extend_from_slice(b"[%General]");
            } else {
                out.push(b'[');
                out.extend_from_slice(&header);
                out.push(b']');
            }
            out.push(b'\n');
            section
                .keys
                .sort_by(|a, b| qt_order(a.1.position, a.0, b.1.position, b.0));
            for (key, entry) in &section.keys {
                escape_key(key, &mut out);
                out.push(b'=');
                match &entry.value {
                    IniValue::Text(text) => escape_value(text, &mut out),
                    IniValue::List(items) if items.is_empty() => {
                        out.extend_from_slice(b"@Invalid()");
                    }
                    IniValue::List(items) => {
                        for (i, item) in items.iter().enumerate() {
                            if i != 0 {
                                out.extend_from_slice(b", ");
                            }
                            escape_value(item, &mut out);
                        }
                    }
                }
                out.push(b'\n');
            }
        }
        out
    }

    /// Serializes and starts a new save window (like `QSettings::sync`
    /// after a good write): clears the dirty flag and removed-key positions.
    pub fn save(&mut self) -> Vec<u8> {
        let bytes = self.to_bytes();
        self.removed.clear();
        self.dirty = false;
        bytes
    }

    /// True when a key changed since the last [`Self::save`].
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn get(&self, key: &str) -> Option<&IniValue> {
        self.entries.get(&normalize_key(key)).map(|e| &e.value)
    }

    /// Decoded value, or `None` when the key is absent.
    pub fn value(&self, key: &str) -> Option<Result<Variant, crate::VariantError>> {
        self.get(key).map(Variant::from_ini)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(&normalize_key(key))
    }

    /// `QSettings::setValue`. Writing an equal value is a no-op.
    pub fn set(&mut self, key: &str, value: &Variant) {
        self.set_raw(key, value.to_ini());
    }

    pub fn set_raw(&mut self, key: &str, value: IniValue) {
        let key = normalize_key(key);
        if key.is_empty() {
            return;
        }
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.value != value {
                entry.value = value;
                self.dirty = true;
            }
            return;
        }
        let position = self.removed.remove(&key);
        self.entries.insert(key, Entry { value, position });
        self.dirty = true;
    }

    /// `QSettings::remove`: removes the key and every key below it. An empty
    /// key clears the file.
    pub fn remove(&mut self, key: &str) {
        let key = normalize_key(key);
        if key.is_empty() {
            self.clear();
            return;
        }
        let prefix = format!("{key}/");
        let doomed: Vec<String> = self
            .entries
            .keys()
            .filter(|k| **k == key || k.starts_with(&prefix))
            .cloned()
            .collect();
        for k in doomed {
            self.take(k);
        }
    }

    /// `QSettings::clear`.
    pub fn clear(&mut self) {
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for k in keys {
            self.take(k);
        }
    }

    fn take(&mut self, key: String) {
        if let Some(Entry {
            position: Some(p), ..
        }) = self.entries.remove(&key)
        {
            self.removed.insert(key, p);
        }
        self.dirty = true;
    }

    /// `QSettings::childKeys` of a group (`""` for the root).
    pub fn child_keys(&self, group: &str) -> Vec<String> {
        let mut keys: Vec<String> = self
            .children(group)
            .filter(|rest| !rest.contains('/'))
            .map(str::to_owned)
            .collect();
        keys.dedup();
        keys
    }

    /// `QSettings::childGroups` of a group (`""` for the root).
    pub fn child_groups(&self, group: &str) -> Vec<String> {
        let mut groups: Vec<String> = self
            .children(group)
            .filter_map(|rest| rest.split_once('/').map(|(g, _)| g.to_owned()))
            .collect();
        groups.dedup();
        groups
    }

    fn children<'a>(&'a self, group: &str) -> impl Iterator<Item = &'a str> {
        let group = normalize_key(group);
        let prefix = if group.is_empty() {
            group
        } else {
            format!("{group}/")
        };
        self.entries
            .keys()
            .filter_map(move |k| k.strip_prefix(prefix.as_str()))
    }

    /// Every key, sorted.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

/// `QSettingsPrivate::normalizedKey`: drops empty path parts.
fn normalize_key(key: &str) -> String {
    let parts: Vec<&str> = key.split('/').filter(|p| !p.is_empty()).collect();
    parts.join("/")
}

fn qt_order(pa: Option<u32>, a: &str, pb: Option<u32>, b: &str) -> Ordering {
    let rank = |p: Option<u32>| p.map_or(-1, i64::from);
    rank(pa)
        .cmp(&rank(pb))
        .then_with(|| a.encode_utf16().cmp(b.encode_utf16()))
}

#[derive(Clone, Copy, Debug)]
struct Line {
    start: usize,
    end: usize,
    equals: Option<usize>,
}

/// `readIniLine`: one logical line. `;` starts a comment at line start and
/// ends the line elsewhere outside quotes; `\` escapes a line end.
fn read_line(data: &[u8], pos: &mut usize) -> Option<Line> {
    let len = data.len();
    let mut in_quotes = false;
    let mut equals = None;
    let mut start = *pos;
    while start < len && matches!(data[start], b'\t' | b'\n' | b'\r' | b' ') {
        start += 1;
    }
    let mut i = start;
    while i < len {
        let ch = data[i];
        i += 1;
        match ch {
            b'=' if !in_quotes && equals.is_none() => equals = Some(i - 1),
            b'\n' | b'\r' if i == start + 1 => start += 1,
            b'\n' | b'\r' if !in_quotes => {
                i -= 1;
                break;
            }
            b'\\' if i < len => {
                let next = data[i];
                i += 1;
                if i < len && matches!((next, data[i]), (b'\n', b'\r') | (b'\r', b'\n')) {
                    i += 1;
                }
            }
            b'"' => in_quotes = !in_quotes,
            b';' if i == start + 1 => {
                // Qt's comment loop only stops at '\n'.
                while i < len && data[i] != b'\n' {
                    i += 1;
                }
                start = i;
            }
            b';' if !in_quotes => {
                i -= 1;
                break;
            }
            _ => {}
        }
    }
    *pos = i;
    (i > start).then_some(Line {
        start,
        end: i,
        equals,
    })
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// `iniEscapedKey`: `/` becomes `\`, `[A-Za-z0-9_.-]` stay, other UTF-16
/// units become `%XX` or `%UXXXX`.
fn escape_key(key: &str, out: &mut Vec<u8>) {
    for unit in key.encode_utf16() {
        match unit {
            0x2F => out.push(b'\\'),
            u if u < 0x80 && (u as u8).is_ascii_alphanumeric()
                || matches!(u, 0x5F | 0x2D | 0x2E) =>
            {
                out.push(u as u8);
            }
            u if u <= 0xFF => {
                out.extend_from_slice(&[b'%', HEX[usize::from(u >> 4)], HEX[usize::from(u & 15)]])
            }
            u => {
                out.extend_from_slice(b"%U");
                for shift in [12, 8, 4, 0] {
                    out.push(HEX[usize::from((u >> shift) & 15)]);
                }
            }
        }
    }
}

/// `iniUnescapedKey`: bytes are Latin-1; `\` is `/`; `%XX` and `%UXXXX`.
fn unescape_key(raw: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let ch = raw[i];
        if ch == b'\\' {
            units.push(u16::from(b'/'));
            i += 1;
            continue;
        }
        if ch != b'%' || i == raw.len() - 1 {
            units.push(u16::from(ch));
            i += 1;
            continue;
        }
        let (first, digits) = if raw[i + 1] == b'U' {
            (i + 2, 4)
        } else {
            (i + 1, 2)
        };
        let parsed = raw
            .get(first..first + digits)
            .filter(|d| d.iter().all(u8::is_ascii_hexdigit))
            .and_then(|d| u16::from_str_radix(std::str::from_utf8(d).ok()?, 16).ok());
        match parsed {
            Some(unit) => {
                units.push(unit);
                i = first + digits;
            }
            None => {
                units.push(u16::from(b'%'));
                i += 1;
            }
        }
    }
    String::from_utf16_lossy(&units)
}

/// `iniEscapedString` with no codec: C escapes, `\xNN` for controls and
/// every unit >= 0x7F, quotes when the text has `;`, `,`, `=` or an edge
/// space.
fn escape_value(text: &str, out: &mut Vec<u8>) {
    let start = out.len();
    let mut needs_quotes = false;
    let mut escape_next_if_digit = false;
    for unit in text.encode_utf16() {
        if matches!(unit, 0x3B | 0x2C | 0x3D) {
            needs_quotes = true;
        }
        let is_hex = unit < 0x80 && (unit as u8).is_ascii_hexdigit();
        if escape_next_if_digit && is_hex {
            let _ = write!(Bytes(out), "\\x{unit:x}");
            continue;
        }
        escape_next_if_digit = false;
        match unit {
            0 => {
                out.extend_from_slice(b"\\0");
                escape_next_if_digit = true;
            }
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0C => out.extend_from_slice(b"\\f"),
            0x0A => out.extend_from_slice(b"\\n"),
            0x0D => out.extend_from_slice(b"\\r"),
            0x09 => out.extend_from_slice(b"\\t"),
            0x0B => out.extend_from_slice(b"\\v"),
            0x22 | 0x5C => out.extend_from_slice(&[b'\\', unit as u8]),
            u if u <= 0x1F || u >= 0x7F => {
                let _ = write!(Bytes(out), "\\x{u:x}");
                escape_next_if_digit = true;
            }
            u => out.push(u as u8),
        }
    }
    let edge_space = out.len() > start && (out[start] == b' ' || out.last() == Some(&b' '));
    if needs_quotes || edge_space {
        out.insert(start, b'"');
        out.push(b'"');
    }
}

/// `fmt::Write` adapter so `write!` can append ASCII to a byte buffer.
struct Bytes<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for Bytes<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// `iniUnescapedStringList`: unescapes one value. An unquoted `,` makes it a
/// list. Bytes are Latin-1; `\xNN..` and `\ooo` give UTF-16 units.
fn unescape_value(raw: &[u8]) -> IniValue {
    #[derive(PartialEq)]
    enum State {
        SkipSpaces,
        Normal,
    }
    let mut list: Option<Vec<String>> = None;
    let mut current: Vec<u16> = Vec::new();
    let mut in_quotes = false;
    let mut quoted = false;
    let mut state = State::SkipSpaces;
    let mut i = 0;
    let to = raw.len();
    let chop = |s: &mut Vec<u16>| {
        while matches!(s.last(), Some(0x20 | 0x09)) {
            s.pop();
        }
    };
    while i < to {
        if state == State::SkipSpaces {
            while i < to && matches!(raw[i], b' ' | b'\t') {
                i += 1;
            }
            state = State::Normal;
            continue;
        }
        match raw[i] {
            b'\\' => {
                i += 1;
                if i >= to {
                    break;
                }
                let ch = raw[i];
                i += 1;
                let simple = match ch {
                    b'a' => Some(0x07),
                    b'b' => Some(0x08),
                    b'f' => Some(0x0C),
                    b'n' => Some(0x0A),
                    b'r' => Some(0x0D),
                    b't' => Some(0x09),
                    b'v' => Some(0x0B),
                    b'"' | b'?' | b'\'' | b'\\' => Some(u16::from(ch)),
                    _ => None,
                };
                if let Some(unit) = simple {
                    current.push(unit);
                } else if ch == b'x' {
                    if i < to && raw[i].is_ascii_hexdigit() {
                        let mut value: u32 = 0;
                        while i < to && raw[i].is_ascii_hexdigit() {
                            let digit = char::from(raw[i]).to_digit(16).unwrap_or(0);
                            value = value.wrapping_shl(4).wrapping_add(digit);
                            i += 1;
                        }
                        current.push(value as u16);
                    }
                } else if (b'0'..=b'7').contains(&ch) {
                    let mut value = u32::from(ch - b'0');
                    while i < to && (b'0'..=b'7').contains(&raw[i]) {
                        value = value.wrapping_shl(3).wrapping_add(u32::from(raw[i] - b'0'));
                        i += 1;
                    }
                    current.push(value as u16);
                } else if (ch == b'\n' || ch == b'\r')
                    && i < to
                    && matches!(raw[i], b'\n' | b'\r')
                    && raw[i] != ch
                {
                    i += 1;
                }
                // Any other escaped character is skipped.
            }
            b'"' => {
                i += 1;
                quoted = true;
                in_quotes = !in_quotes;
                if !in_quotes {
                    state = State::SkipSpaces;
                }
            }
            b',' if !in_quotes => {
                if !quoted {
                    chop(&mut current);
                }
                list.get_or_insert_with(Vec::new)
                    .push(String::from_utf16_lossy(&current));
                current.clear();
                quoted = false;
                i += 1;
                state = State::SkipSpaces;
            }
            _ => {
                let mut j = i + 1;
                while j < to && !matches!(raw[j], b'\\' | b'"' | b',') {
                    j += 1;
                }
                current.extend(raw[i..j].iter().map(|&b| u16::from(b)));
                i = j;
            }
        }
    }
    if !quoted {
        chop(&mut current);
    }
    let last = String::from_utf16_lossy(&current);
    match list {
        Some(mut items) => {
            items.push(last);
            IniValue::List(items)
        }
        None => IniValue::Text(last),
    }
}
