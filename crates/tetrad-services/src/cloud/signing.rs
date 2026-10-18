//! Stock cloud request signing with SHARC-held material.
//!
//! The SHARC answers DSP request `0x35` (the serial) with 16 bytes of signing material and
//! request `0x4f` with a 32-byte client id. Both are run-time inputs: nothing here stores,
//! prints or derives them beyond the stock steps. Buffers are zeroed on drop; `Debug` shows
//! no bytes.

use std::fmt::{self, Write as _};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use super::{ClientSigner, SignError};

pub const SIGNING_MATERIAL_BYTES: usize = 16;
pub const CLIENT_ID_BYTES: usize = 32;
/// Stock rejects a reply whose timestamp is more than this from now (VA 0x004a10f8).
pub const REPLY_WINDOW_MS: u64 = 60_000;

/// Reply `0x11` payload: `A` = bytes 0–7, `B` = bytes 8–15, both big-endian (VA 0x004a1414).
pub struct SigningMaterial(Zeroizing<[u8; SIGNING_MATERIAL_BYTES]>);

/// Reply `0x1f` payload, sent as base64 in `X-Cloud-Api-Client-Id` (VA 0x00485aac).
pub struct ClientIdBytes(Zeroizing<[u8; CLIENT_ID_BYTES]>);

/// `SignatureHelper::validate` results; stock codes 0, 1 and 2 (VA 0x004a10f8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Validation {
    Valid,
    Mismatch,
    Stale,
}

impl SigningMaterial {
    /// Copies the reply bytes and zeroes `source`.
    pub fn take(source: &mut [u8; SIGNING_MATERIAL_BYTES]) -> Self {
        let material = Self(Zeroizing::new(*source));
        zeroize::Zeroize::zeroize(source);
        material
    }

    /// HMAC-SHA256 with key = decimal text of `B` over `message + "-" + decimal(A)`
    /// (VAs 0x004a171c–0x004a1814).
    fn mac(&self, message: &str) -> Hmac<Sha256> {
        let (a, b) = self.0.split_at(8);
        let half = |bytes: &[u8]| {
            Zeroizing::new(u64::from_be_bytes(bytes.try_into().expect("8-byte half")).to_string())
        };
        let mut mac =
            Hmac::<Sha256>::new_from_slice(half(b).as_bytes()).expect("HMAC takes any key length");
        mac.update(message.as_bytes());
        mac.update(b"-");
        mac.update(half(a).as_bytes());
        mac
    }

    /// The signature: 64 lowercase hex digits (VA 0x004a1970).
    pub fn sign(&self, message: &str) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.mac(message).finalize().into_bytes() {
            write!(hex, "{byte:02x}").expect("String write");
        }
        hex
    }

    /// Checks a server signature over `message`. `timestamp` is the reply's unix seconds (stock
    /// `QDateTime::fromTime_t`), `now_ms` the device clock. Stock compares with `QString ==`;
    /// here the digest compare is constant-time (owned). Only lowercase hex can match, as in
    /// stock.
    pub fn validate(
        &self,
        message: &str,
        signature: &str,
        timestamp: i64,
        now_ms: i64,
    ) -> Validation {
        if timestamp.saturating_mul(1000).abs_diff(now_ms) > REPLY_WINDOW_MS {
            return Validation::Stale;
        }
        match decode_hex(signature) {
            Some(digest) if self.mac(message).verify_slice(&digest).is_ok() => Validation::Valid,
            _ => Validation::Mismatch,
        }
    }
}

fn decode_hex(text: &str) -> Option<[u8; 32]> {
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let text = text.as_bytes();
    if text.len() != 64 {
        return None;
    }
    let mut digest = [0; 32];
    for (byte, pair) in digest.iter_mut().zip(text.chunks_exact(2)) {
        *byte = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(digest)
}

impl ClientIdBytes {
    /// Copies the reply bytes and zeroes `source`.
    pub fn take(source: &mut [u8; CLIENT_ID_BYTES]) -> Self {
        let id = Self(Zeroizing::new(*source));
        zeroize::Zeroize::zeroize(source);
        id
    }

    /// Qt `toBase64`: standard alphabet with `=` padding.
    pub fn to_base64(&self) -> String {
        STANDARD.encode(*self.0)
    }
}

impl fmt::Debug for SigningMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningMaterial(..)")
    }
}

impl fmt::Debug for ClientIdBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientIdBytes(..)")
    }
}

/// [`ClientSigner`] over one SHARC batch. Stock fetches both replies per batch and keeps no
/// cache, so build one per signed operation and drop it after.
#[derive(Debug)]
pub struct SharcSigner {
    pub material: SigningMaterial,
    pub client_id: ClientIdBytes,
}

impl ClientSigner for SharcSigner {
    fn sign(&mut self, message: &str) -> Result<String, SignError> {
        Ok(self.material.sign(message))
    }
    fn client_id(&mut self) -> Result<String, SignError> {
        Ok(self.client_id.to_base64())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Made-up reply bytes: `A` = 1, `B` = 2.
    pub(crate) fn signer() -> SharcSigner {
        let mut material = [0; SIGNING_MATERIAL_BYTES];
        material[7] = 1;
        material[15] = 2;
        SharcSigner {
            material: SigningMaterial::take(&mut material),
            client_id: ClientIdBytes::take(&mut [0xfb; CLIENT_ID_BYTES]),
        }
    }

    fn hmac_hex(key: &[u8], data: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(data);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// `A` and `B` are full 64-bit big-endian halves; key = decimal `B`, data gets `-A`.
    #[test]
    fn signs_with_big_endian_halves_as_decimal_text() {
        let mut bytes = [0; SIGNING_MATERIAL_BYTES];
        bytes[..8].copy_from_slice(&0x0102_0304_0506_0708_u64.to_be_bytes());
        bytes[8..].copy_from_slice(&u64::MAX.to_be_bytes());
        let material = SigningMaterial::take(&mut bytes);
        assert_eq!(bytes, [0; SIGNING_MATERIAL_BYTES], "source is zeroed");
        assert_eq!(
            material.sign("m"),
            hmac_hex(b"18446744073709551615", b"m-72623859790382856")
        );
        let signature = signer().material.sign("m");
        assert_eq!(signature, hmac_hex(b"2", b"m-1"));
        assert!(
            signature.len() == 64
                && signature
                    .bytes()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        );
    }

    #[test]
    fn validate_window_and_compare() {
        let material = signer().material;
        let good = material.sign("m");
        let now_ms = 1_700_000_000_000;
        let at = |ms: i64| material.validate("m", &good, 1_700_000_000, now_ms + ms);
        assert_eq!(at(60_000), Validation::Valid);
        assert_eq!(at(-60_000), Validation::Valid);
        assert_eq!(at(60_001), Validation::Stale);
        assert_eq!(at(-60_001), Validation::Stale);
        let validate = |sig: &str| material.validate("m", sig, 1_700_000_000, now_ms);
        assert_eq!(validate(&good.to_uppercase()), Validation::Mismatch);
        assert_eq!(validate(&good[..62]), Validation::Mismatch);
        assert_eq!(validate(&material.sign("n")), Validation::Mismatch);
    }

    #[test]
    fn client_id_is_padded_standard_base64() {
        let mut signer = signer();
        assert_eq!(
            signer.client_id().unwrap(),
            format!("{}+/s=", "+/v7".repeat(10))
        );
    }

    #[test]
    fn debug_prints_no_secret_bytes() {
        let signer = signer();
        let text = format!("{signer:?} {:?} {:?}", signer.material, signer.client_id);
        assert_eq!(
            text,
            "SharcSigner { material: SigningMaterial(..), client_id: ClientIdBytes(..) } \
             SigningMaterial(..) ClientIdBytes(..)"
        );
    }
}
