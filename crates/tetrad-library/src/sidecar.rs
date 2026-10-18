//! The stock `<preset>.json` sidecar beside each `<preset>.pb`.
//!
//! ZenUI (`0x0021eda0` → `0x007ee62c` case 0) always writes `name`,
//! `author`, `authorId`, `date` (`yyyy-MM-ddThh:mm:ssZ`, UTC),
//! `instrument_type` ([`Instrument::text`]), `cloudId`,
//! `corosVersion`, `description`, `creator`, any unknown keys, and `tags` (an
//! array). Its bundled JSON writer emits keys in sorted order as
//! `{"key": value,"key": value}`, without a final newline. Older sidecars may
//! lack `creator` or `description`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sidecar {
    /// All keys, including unknown ones, so a rewrite loses nothing.
    pub fields: Map<String, Value>,
}

/// The `creator` of a device save: the product (`0x497ce0`, by the board platform
/// `0x496ff8`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Creator {
    /// Quad Cortex (platforms 0 and 1).
    #[default]
    Quad,
    /// Quad Cortex mini (ATMA, platforms 2 and 3).
    QuadCortexMini,
}

impl Creator {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Quad => "quad",
            Self::QuadCortexMini => "quad_cortex_mini",
        }
    }
}

/// The fields of [`Sidecar::saved_preset`]. `author` and `author_id` are the saved preset's
/// own (`.pb` `+0x16a0`, `+0x16a4`) after the save's account fallback.
#[derive(Debug, Clone, Copy)]
pub struct PresetSave<'a> {
    pub name: &'a str,
    pub author: &'a str,
    pub author_id: &'a str,
    pub creator: Creator,
    pub now: SystemTime,
    pub coros_version: &'a str,
    pub instrument: Instrument,
    /// The saved preset's tags (`.pb` `+0x30`, `0x2e1024`).
    pub tags: &'a [String],
}

impl Sidecar {
    /// A sidecar with every save field empty and the instrument not set. Not the stock save
    /// rule ([`Self::saved_preset`]); the base for other records.
    pub fn new_preset(name: &str, now: SystemTime, coros_version: &str) -> Self {
        let mut sidecar = Self::default();
        for (key, value) in [
            ("author", ""),
            ("authorId", ""),
            ("cloudId", ""),
            ("corosVersion", coros_version),
            ("creator", ""),
            ("date", &utc_date(now)),
            ("description", ""),
            ("instrument_type", "instrument_not_set"),
            ("name", name),
        ] {
            sidecar.set_text(key, value);
        }
        sidecar
            .fields
            .insert("tags".into(), Value::Array(Vec::new()));
        sidecar
    }

    /// The sidecar of every stock preset save (Save, Save As, Edit Details of the loaded
    /// preset, the first save of a new preset): `0x5b7438` builds a new record (`0x7c4db8`
    /// over the empty record `0x1ec070`) and the folder replaces the slot's record whole
    /// (`0x1eb11c`). So `cloudId` and `description` are empty and unknown keys go; `creator`
    /// is the product; `date` is the save time (UTC).
    pub fn saved_preset(save: &PresetSave<'_>) -> Self {
        let mut sidecar = Self::new_preset(save.name, save.now, save.coros_version);
        sidecar.set_text("author", save.author);
        sidecar.set_text("authorId", save.author_id);
        sidecar.set_text("creator", save.creator.text());
        sidecar.set_instrument(save.instrument);
        let tags = save.tags.iter().cloned().map(Value::String).collect();
        sidecar.fields.insert("tags".into(), Value::Array(tags));
        sidecar
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        match serde_json::from_slice(bytes) {
            Ok(Value::Object(fields)) => Ok(Self { fields }),
            _ => Err(Error::Sidecar),
        }
    }

    /// Encodes in the stock layout. Values use compact JSON. Keys are sorted
    /// here, not by `Map`: a `serde_json/preserve_order` feature elsewhere in
    /// the build would make `Map` keep insertion order.
    pub fn encode(&self) -> Vec<u8> {
        let mut fields: Vec<_> = self.fields.iter().collect();
        fields.sort_unstable_by_key(|(key, _)| *key);
        let mut out = b"{".to_vec();
        for (index, (key, value)) in fields.into_iter().enumerate() {
            if index > 0 {
                out.push(b',');
            }
            // Serializing a string or a JSON value cannot fail.
            out.extend(serde_json::to_vec(key).expect("JSON key"));
            out.extend_from_slice(b": ");
            out.extend(serde_json::to_vec(value).expect("JSON value"));
        }
        out.push(b'}');
        out
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        self.fields.get(key).and_then(Value::as_str)
    }

    /// The strings of an array value such as `tags`; other items are left out.
    pub fn texts(&self, key: &str) -> Vec<&str> {
        self.fields
            .get(key)
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |items| {
                items.iter().filter_map(Value::as_str).collect()
            })
    }

    pub fn set_text(&mut self, key: &str, value: &str) {
        self.fields
            .insert(key.to_owned(), Value::String(value.to_owned()));
    }

    /// `instrument_type` as stock reads it (`0x7ec714`): an unknown or missing text is
    /// [`Instrument::NotSet`].
    pub fn instrument(&self) -> Instrument {
        self.text("instrument_type")
            .map_or(Instrument::NotSet, Instrument::from_text)
    }

    pub fn set_instrument(&mut self, instrument: Instrument) {
        self.set_text("instrument_type", instrument.text());
    }
}

/// The preferred instrument of a preset (record `+0x5c`): the PresetMetadataDialog picker,
/// ProductData `instrument` (has-bit `0x4000`), the sidecar `instrument_type`. The codes are
/// the stock enum, the same for presets and captures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Instrument {
    #[default]
    NotSet,
    Guitar,
    Bass,
    Synth,
    Vocal,
    Other,
}

impl Instrument {
    /// The sidecar text (`0x7ec4c4`).
    pub const fn text(self) -> &'static str {
        match self {
            Self::NotSet => "instrument_not_set",
            Self::Guitar => "guitar",
            Self::Bass => "bass",
            Self::Synth => "synth",
            Self::Vocal => "vocal",
            Self::Other => "other",
        }
    }

    /// `0x7ec714`: the five texts; anything else is not set.
    pub fn from_text(text: &str) -> Self {
        match text {
            "guitar" => Self::Guitar,
            "bass" => Self::Bass,
            "synth" => Self::Synth,
            "vocal" => Self::Vocal,
            "other" => Self::Other,
            _ => Self::NotSet,
        }
    }

    /// The stock code (0–5); other codes have no instrument (stock writes `unknown`).
    pub const fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            0 => Self::NotSet,
            1 => Self::Guitar,
            2 => Self::Bass,
            3 => Self::Synth,
            4 => Self::Vocal,
            5 => Self::Other,
            _ => return None,
        })
    }
}

/// Formats `yyyy-MM-ddThh:mm:ssZ` in UTC (Qt format string `0x01561d24`).
/// Times before 1970 clamp to the epoch.
pub fn utc_date(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest / 60 % 60,
        rest % 60
    )
}
