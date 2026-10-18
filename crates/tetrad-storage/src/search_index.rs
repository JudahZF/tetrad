//! Stock search index files (`/media/p4/searchIndexUser`,
//! `/opt/neuraldsp/searchIndexFactory`).
//!
//! Wire layout, checked against CorOS 4.1.0 files:
//!
//! ```text
//! 1: bytes   text     lowercase names, each followed by '|'
//! 2: packed  suffixes suffix array of `text`, bytewise order
//! 3: packed  owners   owners[k] = entry index of the suffix at suffixes[k]
//! 4: repeated Entry { 1: uint32 folder_key, 2: string path }
//! ```
//!
//! Preset entries hold device `.pb` paths; capture entries hold the bare
//! 64-hex capture id. `folder_key` is [`folder_key`] of the entry's folder:
//! the setlist directory path, or `local_nc_root` for user captures. The
//! factory `setlists` XML uses the same value as its setlist `hash`.

use crate::{Error, Result};

/// FNV-1a 32 over the UTF-8 folder key; values below `0x2000` are raised by
/// `0x2000` (`0x00732508`). Matches every user entry and the factory setlist
/// hash of a CorOS 4.1.0 library.
pub fn folder_key(folder: &str) -> u32 {
    let hash = folder.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    if hash < 0x2000 { hash + 0x2000 } else { hash }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchEntry {
    /// Lowercase display name.
    pub name: String,
    pub folder_key: u32,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchIndex {
    pub entries: Vec<SearchEntry>,
    /// Derived on decode or encode; kept to answer queries without rebuilding.
    text: Vec<u8>,
    suffixes: Vec<u32>,
    owners: Vec<u32>,
}

impl SearchIndex {
    /// Builds the derived arrays for `entries`. Names are lowercased. A name may
    /// contain `|`: like stock, owners come from entry start offsets, not from
    /// the separator.
    pub fn build(entries: Vec<SearchEntry>) -> Result<Self> {
        let mut index = Self {
            entries,
            ..Self::default()
        };
        let mut starts = Vec::with_capacity(index.entries.len());
        for entry in &mut index.entries {
            entry.name = entry.name.to_lowercase();
            starts.push(index.text.len());
            index.text.extend_from_slice(entry.name.as_bytes());
            index.text.push(b'|');
        }
        let length = u32::try_from(index.text.len()).map_err(|_| Error::Limit)?;
        let mut suffixes: Vec<u32> = (0..length).collect();
        suffixes.sort_unstable_by(|&a, &b| index.text[a as usize..].cmp(&index.text[b as usize..]));
        index.owners = suffixes
            .iter()
            .map(|&position| {
                (starts.partition_point(|&start| start <= position as usize) - 1) as u32
            })
            .collect();
        index.suffixes = suffixes;
        Ok(index)
    }

    /// Decodes a stock index and rebuilds the derived arrays. Entry boundaries
    /// come from the stored owner array, so a name that contains `|` decodes.
    /// The stored suffix and owner arrays must equal the rebuilt ones, or the
    /// file is refused.
    pub fn decode(bytes: &[u8], max_entries: usize) -> Result<Self> {
        let mut text = None;
        let mut suffixes = Vec::new();
        let mut owners = Vec::new();
        let mut paths = Vec::new();
        let mut reader = Reader { bytes, offset: 0 };
        while let Some((field, value)) = reader.field()? {
            match (field, value) {
                (1, Value::Bytes(bytes)) => text = Some(bytes),
                (2, Value::Bytes(bytes)) => suffixes.extend(packed(bytes)?),
                (3, Value::Bytes(bytes)) => owners.extend(packed(bytes)?),
                (4, Value::Bytes(bytes)) => {
                    if paths.len() == max_entries {
                        return Err(Error::Limit);
                    }
                    paths.push(entry(bytes)?);
                }
                _ => return Err(Error::Format("unexpected search index field")),
            }
        }
        let text = text.unwrap_or_default();
        let inconsistent = || Error::Format("search index arrays are inconsistent");
        if suffixes.len() != text.len() || owners.len() != text.len() {
            return Err(inconsistent());
        }
        // The owner of each text position. Every entry is one run of equal
        // owners, in entry order, that ends with its '|' separator.
        let mut owner_at = vec![u32::MAX; text.len()];
        for (&position, &owner) in suffixes.iter().zip(&owners) {
            *owner_at
                .get_mut(position as usize)
                .ok_or_else(inconsistent)? = owner;
        }
        let mut names = Vec::with_capacity(paths.len());
        let mut start = 0;
        for end in 0..text.len() {
            if owner_at.get(end + 1) == Some(&owner_at[end]) {
                continue;
            }
            if owner_at[end] as usize != names.len() || text[end] != b'|' {
                return Err(inconsistent());
            }
            names.push(
                std::str::from_utf8(&text[start..end])
                    .map_err(|_| Error::Format("search index text is not UTF-8"))?,
            );
            start = end + 1;
        }
        if names.len() != paths.len() {
            return Err(Error::Format("search index names and entries differ"));
        }
        let entries = names
            .into_iter()
            .zip(paths)
            .map(|(name, (folder_key, path))| SearchEntry {
                name: name.to_owned(),
                folder_key,
                path,
            })
            .collect();
        let index = Self::build(entries)?;
        if index.text != text || index.suffixes != suffixes || index.owners != owners {
            return Err(inconsistent());
        }
        Ok(index)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.text.len() * 4);
        put_bytes(&mut out, 1, &self.text);
        put_bytes(&mut out, 2, &packed_bytes(&self.suffixes));
        put_bytes(&mut out, 3, &packed_bytes(&self.owners));
        for entry in &self.entries {
            let mut message = Vec::new();
            put_varint(&mut message, 1 << 3);
            put_varint(&mut message, u64::from(entry.folder_key));
            put_bytes(&mut message, 2, entry.path.as_bytes());
            put_bytes(&mut out, 4, &message);
        }
        out
    }

    /// Entries whose name contains `query` (case-insensitive), in name order.
    /// An empty query matches nothing.
    pub fn search(&self, query: &str) -> Vec<&SearchEntry> {
        let query = query.to_lowercase();
        if query.is_empty() || query.contains('|') {
            return Vec::new();
        }
        let query = query.as_bytes();
        let suffix = |k: usize| &self.text[self.suffixes[k] as usize..];
        let start = self
            .suffixes
            .partition_point(|&position| &self.text[position as usize..] < query);
        let mut owners: Vec<u32> = (start..self.suffixes.len())
            .take_while(|&k| suffix(k).starts_with(query))
            .map(|k| self.owners[k])
            .collect();
        owners.sort_unstable();
        owners.dedup();
        let mut found: Vec<&SearchEntry> = owners
            .into_iter()
            .map(|owner| &self.entries[owner as usize])
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }
}

/// The stock `searchRecentChanges*` file, each item `{1: folder_key, 2: path}`:
/// field 1 an MRU list of at most 10 used results (`0x007324cc`), field 2
/// entries added and field 3 entries removed since the index was built
/// (`0x00734d48`, `0x00734a7c`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchChanges {
    pub recent: Vec<(u32, String)>,
    pub added: Vec<(u32, String)>,
    pub removed: Vec<(u32, String)>,
}

/// ZenUI rebuilds the index once added + removed reach this count.
pub const REBUILD_AFTER_CHANGES: usize = 51;
/// At boot ZenUI rebuilds the index when added + removed exceed this count,
/// or when the index or changes file does not load (`0x0073545c`).
pub const BOOT_REBUILD_ABOVE_CHANGES: usize = 12;

impl SearchChanges {
    pub fn decode(bytes: &[u8], max_entries: usize) -> Result<Self> {
        let mut changes = Self::default();
        let mut reader = Reader { bytes, offset: 0 };
        while let Some((field, value)) = reader.field()? {
            let list = match (field, &value) {
                (1, Value::Bytes(_)) => &mut changes.recent,
                (2, Value::Bytes(_)) => &mut changes.added,
                (3, Value::Bytes(_)) => &mut changes.removed,
                _ => return Err(Error::Format("unexpected search change field")),
            };
            if list.len() == max_entries {
                return Err(Error::Limit);
            }
            let Value::Bytes(bytes) = value else {
                unreachable!("matched above")
            };
            list.push(entry(bytes)?);
        }
        Ok(changes)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (field, list) in [(1, &self.recent), (2, &self.added), (3, &self.removed)] {
            for (key, path) in list {
                let mut message = Vec::new();
                put_varint(&mut message, 1 << 3);
                put_varint(&mut message, u64::from(*key));
                put_bytes(&mut message, 2, path.as_bytes());
                put_bytes(&mut out, field, &message);
            }
        }
        out
    }
}

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or(Error::Format("truncated varint"))?;
            self.offset += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(value);
            }
        }
        Err(Error::Format("varint overflow"))
    }

    fn field(&mut self) -> Result<Option<(u64, Value<'a>)>> {
        if self.offset == self.bytes.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            2 => {
                let length = usize::try_from(self.varint()?).map_err(|_| Error::Limit)?;
                let end = self
                    .offset
                    .checked_add(length)
                    .filter(|&end| end <= self.bytes.len())
                    .ok_or(Error::Format("truncated field"))?;
                let bytes = &self.bytes[self.offset..end];
                self.offset = end;
                Value::Bytes(bytes)
            }
            _ => return Err(Error::Format("unsupported wire type")),
        };
        Ok(Some((key >> 3, value)))
    }
}

fn packed(bytes: &[u8]) -> Result<Vec<u32>> {
    let mut reader = Reader { bytes, offset: 0 };
    let mut values = Vec::new();
    while reader.offset < bytes.len() {
        values.push(u32::try_from(reader.varint()?).map_err(|_| Error::Format("value overflow"))?);
    }
    Ok(values)
}

fn entry(bytes: &[u8]) -> Result<(u32, String)> {
    let mut reader = Reader { bytes, offset: 0 };
    let (mut key, mut path) = (0, String::new());
    while let Some((field, value)) = reader.field()? {
        match (field, value) {
            (1, Value::Varint(value)) => {
                key = u32::try_from(value).map_err(|_| Error::Format("key overflow"))?;
            }
            (2, Value::Bytes(bytes)) => {
                path = String::from_utf8(bytes.to_vec())
                    .map_err(|_| Error::Format("entry path is not UTF-8"))?;
            }
            _ => return Err(Error::Format("unexpected search entry field")),
        }
    }
    Ok((key, path))
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_bytes(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, field << 3 | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn packed_bytes(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 2);
    for &value in values {
        put_varint(&mut out, u64::from(value));
    }
    out
}
