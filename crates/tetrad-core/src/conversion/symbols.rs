//! Metadata text parsing and the symbolic constant table of `_INIT_402`
//! (`0x9932c`), looked up by exact, case-sensitive name (`0x4e9a6c`).

/// All 43 recovered `std::map<QString,float>` entries.
pub const SYMBOLS: [(&str, f32); 43] = [
    ("MIN_EQ_DB", -12.0),
    ("MAX_EQ_DB", 12.0),
    ("MIN_EQ_FREQ", 20.0),
    ("MAX_EQ_FREQ", 20_000.0),
    ("MIN_EQ_Q", 0.1),
    ("MAX_EQ_Q", 10.0),
    ("MIN_REVERB_PREDELAY", 1.0),
    ("MAX_REVERB_PREDELAY", 100.0),
    ("MIN_REVERB_DECAY", 1.0),
    ("MAX_REVERB_DECAY", 10.0),
    ("MIN_REVERB_LOW_FREQ", 50.0),
    ("MAX_REVERB_LOW_FREQ", 900.0),
    ("MIN_REVERB_HIGH_FREQ", 1000.0),
    ("MAX_REVERB_HIGH_FREQ", 15_000.0),
    ("MIN_REVERB_GAIN", 0.0),
    ("MAX_REVERB_GAIN", 20.0),
    ("MIN_TUNER_DETUNE_FREQ", 400.0),
    ("MAX_TUNER_DETUNE_FREQ", 480.0),
    ("MIN_CABSIM_DB", -40.0),
    ("MAX_CABSIM_DB", 6.0),
    ("LOG_SKEW", 0.3),
    ("LIN_SKEW", 1.0),
    ("MIN_ADAPTIVE_GATE_EXPANSION_RANGE", 0.01),
    ("MAX_ADAPTIVE_GATE_EXPANSION_RANGE", 8.0),
    ("MIN_ADAPTIVE_GATE_RMS_RANGE", 0.1),
    ("MAX_ADAPTIVE_GATE_RMS_RANGE", 50.0),
    ("MIN_ADAPTIVE_GATE_THRESHOLD_RANGE", -96.0),
    ("MAX_ADAPTIVE_GATE_THRESHOLD_RANGE", 0.0),
    ("MIN_TEMPO", 40.0),
    ("MAX_TEMPO", 240.0),
    ("DEFAULT_TEMPO", 120.0),
    ("MIN_FXLOOP_OUT_GAIN_DB", -40.0),
    ("MAX_FXLOOP_OUT_GAIN_DB", 0.0),
    ("MIN_FXLOOP_IN_GAIN_DB", -40.0),
    ("MAX_FXLOOP_IN_GAIN_DB", 12.0),
    ("MIN_INPUT_TRIM", -40.0),
    ("MAX_INPUT_TRIM", 0.0),
    ("MIN_MIXER_DB", -40.0),
    ("MAX_MIXER_DB", 12.0),
    ("INPUT_GAIN_DEFAULT", f32::from_bits(0x3e2a_aaab)),
    ("MIN_OUTPUT_GAIN_DB", -40.0),
    ("MAX_OUTPUT_GAIN_DB", 12.0),
    ("OUTPUT_GAIN_DEFAULT", f32::from_bits(0x3f44_ec4f)),
];

pub fn symbol(name: &str) -> Option<f32> {
    SYMBOLS
        .iter()
        .find_map(|&(key, value)| (key == name).then_some(value))
}

/// `QString::toFloat`: C-locale decimal with surrounding whitespace, parsed as
/// double, refused beyond the float range, then narrowed. Owned policy: textual
/// infinities and NaN are refused because the pinned Qt spelling rules are not
/// recovered.
pub fn qt_float(text: &str) -> Option<f32> {
    let trimmed = text.trim_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.is_empty()
        || trimmed
            .bytes()
            .any(|b| b.is_ascii_alphabetic() && !matches!(b, b'e' | b'E'))
    {
        return None;
    }
    let value: f64 = trimmed.parse().ok()?;
    (value.abs() <= f64::from(f32::MAX)).then_some(value as f32)
}

/// Stock metadata number: `toFloat`, then the symbol table, else `fallback`.
pub fn metadata_float(text: &str, fallback: f32) -> f32 {
    qt_float(text).or_else(|| symbol(text)).unwrap_or(fallback)
}

/// pugixml `as_int`: leading whitespace, sign, decimal or `0x` hex, stops at
/// the first other character, 0 when no digits. Overflow saturates (owned:
/// the linked pugixml version is not pinned).
pub fn pugi_int(text: &str) -> i32 {
    let s = text.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = match s.get(..2) {
        Some("0x" | "0X") => (16, &s[2..]),
        _ => (10, s),
    };
    let mut value: i64 = 0;
    for digit in digits.chars().map_while(|c| c.to_digit(radix)) {
        value = (value * i64::from(radix) + i64::from(digit)).min(1 << 32);
    }
    let signed = if negative { -value } else { value };
    signed.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Boolean attributes compare the upper-cased text with `TRUE`. ASCII folding
/// matches Qt `toUpper` for every input that can equal `TRUE`.
pub fn xml_true(text: &str) -> bool {
    text.eq_ignore_ascii_case("TRUE")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_numbers_fall_back_through_symbols_then_defaults() {
        assert_eq!(metadata_float(" 0.6 ", 1.0), 0.6);
        assert_eq!(metadata_float("LIN_SKEW", 0.0), 1.0);
        assert_eq!(metadata_float("LOG_SKEW", 0.0), 0.3);
        assert_eq!(metadata_float("MIN_EQ_DB", 0.0), -12.0);
        assert_eq!(metadata_float("min_eq_db", 7.0), 7.0);
        assert_eq!(metadata_float("", 1.0), 1.0);
        assert_eq!(metadata_float("1e39", 2.0), 2.0);
        assert_eq!(qt_float("inf"), None);
        assert_eq!(
            symbol("OUTPUT_GAIN_DEFAULT").map(f32::to_bits),
            Some(0x3f44_ec4f)
        );
        assert_eq!(SYMBOLS.len(), 43);
    }

    #[test]
    fn pugixml_integers_and_booleans() {
        assert_eq!(pugi_int("1001"), 1001);
        assert_eq!(pugi_int("  0x10"), 16);
        assert_eq!(pugi_int("-3abc"), -3);
        assert_eq!(pugi_int("abc"), 0);
        assert_eq!(pugi_int("99999999999"), i32::MAX);
        assert!(xml_true("true") && xml_true("TRUE") && !xml_true("1"));
    }
}
