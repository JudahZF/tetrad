//! WPA passphrase hashing. Stock never stores a plaintext password: it derives
//! `PKCS5_PBKDF2_HMAC_SHA1(passphrase, ssid, 4096, 32)` and saves 64 lowercase hex digits as an
//! unquoted `psk` (VAs 0x008958c8, 0x008956e8). This is the IEEE 802.11i passphrase mapping.

use std::fmt;

/// A derived PSK. `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct Psk(String);

impl Psk {
    /// `None` unless the passphrase is 8..=63 bytes, as WPA requires.
    pub fn derive(passphrase: &str, ssid: &[u8]) -> Option<Self> {
        if !(8..=63).contains(&passphrase.len()) {
            return None;
        }
        let mut key = [0u8; 32];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(passphrase.as_bytes(), ssid, 4096, &mut key);
        Some(Self(key.iter().map(|b| format!("{b:02x}")).collect()))
    }

    pub fn hex(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Psk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Psk(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // RFC 6070, 4096 rounds.
        let mut out = [0u8; 20];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(b"password", b"salt", 4096, &mut out);
        let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "4b007901b765489abead49d926f721d065a429c1");
        // IEEE 802.11i Annex H.4 passphrase mapping.
        assert_eq!(
            Psk::derive("password", b"IEEE").unwrap().hex(),
            "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
        );
        assert_eq!(Psk::derive("short", b"x"), None);
    }
}
