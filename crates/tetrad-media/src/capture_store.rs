//! Capture store translation: re-encrypt one stored capture from one device
//! cipher to another (`0x006a60e8`).
//!
//! - Boot runs it over the downloads store (`0x006a6358`) and the user store
//!   (`0x006a68f4`) with target "local"; `tetrad-storage` owns the row loop and
//!   the `nc_encryption` flag.
//! - To local (`0x006b03d4`): decrypt with the mode-0 cipher (seed only),
//!   encrypt with the mode-1 cipher (seed and serial). To global
//!   (`0x006ae59c`): the reverse. Both build their contexts with
//!   `0x009f4fe0`. Callers pass the matching [`DeviceCipher`] pair.
//! - The metadata is always translated. The rawdata is translated only when
//!   the plain metadata JSON has `"version": "1"` (compare with `"1"` at
//!   VA 0x120a478). Other records keep their rawdata bytes.

use crate::factory::DeviceCipher;

/// The `version` text whose rawdata stock translates too.
pub const RAWDATA_TRANSLATED_VERSION: &str = "1";

/// Translates one record in place. Returns true when the rawdata changed too.
/// Metadata that is not a JSON object with a string `version` keeps its
/// rawdata, as stock does when the map lookup gives an empty string.
pub fn translate_capture(
    from: &DeviceCipher,
    to: &DeviceCipher,
    rawdata: &mut [u8],
    metadata: &mut [u8],
) -> bool {
    from.apply(metadata);
    let version_one = serde_json::from_slice::<serde_json::Value>(metadata)
        .ok()
        .and_then(|value| {
            value
                .get("version")
                .and_then(serde_json::Value::as_str)
                .map(|version| version == RAWDATA_TRANSLATED_VERSION)
        })
        .unwrap_or(false);
    to.apply(metadata);
    if version_one {
        from.apply(rawdata);
        to.apply(rawdata);
    }
    version_one
}
