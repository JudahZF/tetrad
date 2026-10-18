//! Scan results from `BSS <i>` replies (VA 0x0088c000) and `LIST_NETWORKS` rows (VA 0x0088cab4).

/// Stock auth codes (VA 0x00888064). Code 2 (shared WEP) and 3 (802.1X) are never set from
/// scan flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Open = 0,
    Wep = 1,
    WpaPsk = 4,
    WpaEap = 5,
    Wpa2Psk = 6,
    Wpa2Eap = 7,
}

impl Security {
    /// Case-sensitive substring tests in stock order; WEP only when nothing else matched.
    pub fn from_flags(flags: &str) -> Self {
        [
            ("[WPA2-EAP", Self::Wpa2Eap),
            ("[WPA-EAP", Self::WpaEap),
            ("[WPA2-PSK", Self::Wpa2Psk),
            ("[WPA-PSK", Self::WpaPsk),
        ]
        .into_iter()
        .find(|(needle, _)| flags.contains(needle))
        .map(|(_, security)| security)
        .unwrap_or(if flags.contains("WEP") {
            Self::Wep
        } else {
            Self::Open
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEntry {
    pub bssid: String,
    pub frequency_mhz: u32,
    pub signal_dbm: i32,
    pub security: Security,
    /// `\xNN`-decoded, then read as UTF-8 (lossy), as stock does.
    pub ssid: String,
}

impl ScanEntry {
    /// Stock percent: dBm clamped to -90..=-30, mapped linearly to 0..=100.
    pub fn signal_percent(&self) -> u8 {
        ((self.signal_dbm.clamp(-90, -30) + 90) * 100 / 60) as u8
    }

    /// Stock icon index 0..=6 (VA 0x005d2028): `floor((90 - |dBm|) / 60 * 7)`.
    pub fn signal_bars(&self) -> u8 {
        let distance = self.signal_dbm.unsigned_abs().clamp(30, 90);
        ((90 - distance) * 7 / 60).min(6) as u8
    }
}

/// Decode the supplicant's printable SSID form: `\xNN`, `\\`, `\"`, `\e`, `\n`, `\r`, `\t`.
pub fn decode_ssid(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let (byte, used) = match bytes[i + 1] {
                b'x' => match text
                    .get(i + 2..i + 4)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                {
                    Some(b) => (b, 4),
                    None => (b'\\', 1),
                },
                b'n' => (b'\n', 2),
                b'r' => (b'\r', 2),
                b't' => (b'\t', 2),
                b'e' => (0x1b, 2),
                other @ (b'\\' | b'"') => (other, 2),
                _ => (b'\\', 1),
            };
            out.push(byte);
            i += used;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One `BSS <i>` reply (`key=value` lines). `None` without a BSSID or SSID.
pub fn parse_bss(reply: &str) -> Option<ScanEntry> {
    let value = |key: &str| {
        reply
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
    };
    let ssid = decode_ssid(value("ssid")?);
    if ssid.is_empty() {
        return None;
    }
    Some(ScanEntry {
        bssid: value("bssid")?.into(),
        frequency_mhz: value("freq").and_then(|v| v.parse().ok()).unwrap_or(0),
        signal_dbm: value("level").and_then(|v| v.parse().ok()).unwrap_or(-100),
        security: Security::from_flags(value("flags").unwrap_or("")),
        ssid,
    })
}

/// Stock de-duplication: the first BSS seen for an SSID wins. No sorting.
pub fn push_unique(entries: &mut Vec<ScanEntry>, entry: ScanEntry) {
    if entries.iter().all(|e| e.ssid != entry.ssid) {
        entries.push(entry);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedNetwork {
    pub id: u32,
    /// Decoded like [`ScanEntry::ssid`].
    pub ssid: String,
    pub current: bool,
    pub disabled: bool,
}

/// Parse `network id / ssid / bssid / flags` rows; ids must be all digits.
pub fn parse_network_list(reply: &str) -> Vec<SavedNetwork> {
    reply
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let id = columns
                .next()
                .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))?
                .parse()
                .ok()?;
            let ssid = decode_ssid(columns.next()?);
            let flags = columns.nth(1).unwrap_or("");
            Some(SavedNetwork {
                id,
                ssid,
                current: flags.contains("[CURRENT]"),
                disabled: flags.contains("[DISABLED]"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bss_reply_security_and_signal() {
        let reply = "id=3\nbssid=02:00:00:00:00:01\nfreq=2412\nlevel=-48\n\
            flags=[WPA-PSK-CCMP][WPA2-PSK-CCMP][ESS]\nssid=Caf\\xc3\\xa9 \\\"x\\\"\n";
        let entry = parse_bss(reply).unwrap();
        assert_eq!(entry.ssid, "Café \"x\"");
        assert_eq!(entry.security, Security::Wpa2Psk);
        assert_eq!((entry.signal_percent(), entry.signal_bars()), (70, 4));
        assert_eq!(Security::from_flags("[WEP][ESS]"), Security::Wep);
        assert_eq!(Security::from_flags("[ESS]"), Security::Open);
        assert_eq!(parse_bss("bssid=02:00:00:00:00:02\nssid=\n"), None);
    }
}
