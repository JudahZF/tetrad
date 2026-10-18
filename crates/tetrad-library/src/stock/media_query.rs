//! Capture list queries for the Directory: the filter (manual M797, settings
//! `DEVICE_TYPES` / `VERSIONS` / `OWNED_BY_ME`) and the jump bar data of the
//! letter, date, device and gain sorts (`zenUI::JumpScrollbar*`). Callers pass
//! decrypted capture metadata JSON; nothing here reads the database.

use crate::{Error, Result};

/// Stock `deviceType` texts by device code (`0x7ecc18`); the device jump bar
/// shows one icon per code (`0x008050cc`: nine icons, `0x7dffe4(i)`).
pub const DEVICE_TYPES: [&str; 9] = [
    "default",
    "amp_head",
    "amp_combo",
    "amp_and_cab",
    "cab",
    "pedal",
    "fuzz",
    "compressor",
    "overdrive",
];
/// Gain jump bar labels "0" to "10" (`_INIT` list `0x15620dc`; ten ticks in
/// `0x008069e4`, tick `t` lit when the value is at least `t`).
pub const GAIN_POSITIONS: usize = 11;
/// Letter jump bar positions (`0x008054d8`, 0x22): 0..=8 show `#`, 9..=34
/// show `A`..`Z` (`0x00805420`).
pub const LETTER_POSITIONS: u8 = 35;
/// Date jump bar positions (`0x00807138`, 200).
pub const DATE_POSITIONS: u32 = 200;
/// Origin when the oldest date is not valid (`0x0080733c`: 2021-01-01).
const DEFAULT_ORIGIN_DAYS: i64 = 18_628;

/// The fields of one capture that the filter and the jump bars use
/// (metadata keys of `0x7ee62c` case 2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureFacts {
    pub id: String,
    pub name: String,
    /// Days since 1970-01-01 of `date` (`yyyy-MM-ddThh:mm:ssZ`); `None` when
    /// absent or not that form.
    pub date: Option<i64>,
    /// Index in [`DEVICE_TYPES`] of `deviceType`.
    pub device: Option<usize>,
    /// `gainType`; stock writes it only when it is not negative.
    pub gain: Option<i64>,
    /// The stock code (0..=5, `0x7ec714`) of `instrumentType`; `None` when absent or unknown.
    pub instrument: Option<i32>,
    pub author_id: String,
    pub version: String,
}

impl CaptureFacts {
    pub fn from_metadata(id: &str, json: &[u8]) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_slice(json).map_err(|_| Error::Sidecar)?;
        let object = value.as_object().ok_or(Error::Sidecar)?;
        let text = |key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };
        Ok(Self {
            id: id.to_owned(),
            name: text("name").to_owned(),
            date: days_of(text("date")),
            device: DEVICE_TYPES
                .iter()
                .position(|device| *device == text("deviceType")),
            gain: object
                .get("gainType")
                .and_then(serde_json::Value::as_i64)
                .filter(|gain| *gain >= 0),
            instrument: tetrad_capture::stored::instrument_type_code(text("instrumentType")),
            author_id: text("authorId").to_owned(),
            version: text("version").to_owned(),
        })
    }
}

/// The capture filter of the Directory. Inferred combination: each set group
/// must match (AND); inside a group any listed value matches (OR); an empty
/// or absent group matches every capture.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureFilter<'a> {
    /// `DEVICE_TYPES`: device codes (amp, cab, pedal, ...).
    pub device_types: Option<&'a [usize]>,
    /// `VERSIONS`: metadata `version` texts.
    pub versions: Option<&'a [&'a str]>,
    /// `OWNED_BY_ME` ("Made by Me"): the account user id when set.
    pub owned_by: Option<&'a str>,
}

impl CaptureFilter<'_> {
    pub fn matches(&self, capture: &CaptureFacts) -> bool {
        let devices = self.device_types.filter(|types| !types.is_empty());
        let versions = self.versions.filter(|versions| !versions.is_empty());
        devices.is_none_or(|types| capture.device.is_some_and(|device| types.contains(&device)))
            && versions.is_none_or(|versions| versions.contains(&capture.version.as_str()))
            && self
                .owned_by
                .is_none_or(|owner| !owner.is_empty() && capture.author_id == owner)
    }
}

/// The label of a letter bar position (`0x00805420`); `None` past the bar.
pub fn letter_label(position: u8) -> Option<char> {
    match position {
        0..9 => Some('#'),
        9..LETTER_POSITIONS => Some(char::from(b'A' + position - 9)),
        _ => None,
    }
}

/// The date bar of one list (`0x0080733c`): from the oldest date to the
/// newest one (or `today`), in days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateJumpBar {
    origin: i64,
    span: i64,
    /// The oldest date was not valid; position 0 then has no date.
    defaulted: bool,
}

impl DateJumpBar {
    pub fn new(oldest: Option<i64>, newest: Option<i64>, today: i64) -> Self {
        let origin = oldest.unwrap_or(DEFAULT_ORIGIN_DAYS);
        Self {
            origin,
            span: newest.unwrap_or(today) - origin,
            defaulted: oldest.is_none(),
        }
    }

    /// `0x008072b4`: `origin + lroundf(position / 200 * span)` days.
    pub fn date_at(&self, position: u32) -> Option<i64> {
        if self.defaulted && position == 0 {
            return None;
        }
        let offset = (position as f32 / DATE_POSITIONS as f32) * self.span as f32;
        Some(self.origin + offset.round() as i64)
    }
}

/// The first row for each jump bar position of a sorted capture list. Stock
/// keeps this mapping in the table client, which was not recovered; first row
/// whose key equals the position key is inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpTargets {
    /// `#` (index 0) and `A`..`Z` (1..=26), by the name's first character.
    pub letters: [Option<usize>; 27],
    pub devices: [Option<usize>; DEVICE_TYPES.len()],
    pub gains: [Option<usize>; GAIN_POSITIONS],
}

impl JumpTargets {
    pub fn new(rows: &[CaptureFacts]) -> Self {
        let mut targets = Self {
            letters: [None; 27],
            devices: [None; DEVICE_TYPES.len()],
            gains: [None; GAIN_POSITIONS],
        };
        for (row, capture) in rows.iter().enumerate() {
            let letter = capture
                .name
                .chars()
                .next()
                .map(|first| first.to_ascii_uppercase())
                .filter(char::is_ascii_uppercase)
                .map_or(0, |first| usize::from(first as u8 - b'A') + 1);
            targets.letters[letter].get_or_insert(row);
            if let Some(device) = capture.device {
                targets.devices[device].get_or_insert(row);
            }
            if let Some(gain) = capture.gain.and_then(|gain| usize::try_from(gain).ok())
                && let Some(target) = targets.gains.get_mut(gain)
            {
                target.get_or_insert(row);
            }
        }
        targets
    }
}

/// The row whose date is nearest to `day` (first on a tie); inferred like
/// [`JumpTargets`].
pub fn date_target(rows: &[CaptureFacts], day: i64) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(row, capture)| Some((row, (capture.date? - day).abs())))
        .min_by_key(|&(row, distance)| (distance, row))
        .map(|(row, _)| row)
}

/// Days since 1970-01-01 of a stock `yyyy-MM-ddThh:mm:ssZ` date.
fn days_of(date: &str) -> Option<i64> {
    let bytes = date.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let field = |range: std::ops::Range<usize>| date.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}
