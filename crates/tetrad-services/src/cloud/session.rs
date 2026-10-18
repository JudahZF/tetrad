//! Account tokens: JWT claims, refresh timing and the [`TokenStore`] boundary
//! (`ApiTokens`, VAs 0x0044da14, 0x0044e190, 0x0044ed80, 0x004501bc, 0x0044d3e4, 0x00450ab4).

use std::fmt;

use serde::Deserialize;
use thiserror::Error;

use crate::exec::Secret;

/// Group that marks Neural DSP staff accounts (VA 0x00451090).
pub const INTERNAL_GROUP: &str = "neural_internal";
/// Stock clears the tokens and logs out after this many failed refreshes in a row (VA 0x00450ab4).
pub const MAX_REFRESH_FAILURES: u32 = 5;

/// Access and refresh JWTs. `Debug` hides both.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: Secret,
    pub refresh_token: Secret,
}

impl Tokens {
    /// The access token as text, for the bearer header and claim parsing.
    pub fn access(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(self.access_token.expose())
    }
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tokens(<redacted>)")
    }
}

/// Claims ZenUI reads from the access token. Seconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub user_id: String,
    pub expires_at: i64,
    pub issued_at: Option<i64>,
    pub groups: Vec<String>,
}

#[derive(Deserialize)]
struct RawClaims {
    sub: Option<String>,
    exp: Option<i64>,
    iat: Option<i64>,
    #[serde(rename = "cognito:groups", default)]
    groups: Vec<String>,
}

impl Claims {
    /// Decode the payload part of a JWT without verifying it, as stock does. `None` when the
    /// token is malformed or lacks `sub` or `exp` ("Parsing JWT token failed", VA 0x00a41e90).
    pub fn parse(token: &str) -> Option<Self> {
        let payload = token.split('.').nth(1)?;
        let raw: RawClaims = serde_json::from_slice(&base64url_decode(payload)?).ok()?;
        Some(Self {
            user_id: raw.sub?,
            expires_at: raw.exp?,
            issued_at: raw.iat,
            groups: raw.groups,
        })
    }

    pub fn is_internal(&self) -> bool {
        self.groups.iter().any(|g| g == INTERNAL_GROUP)
    }

    /// How long before expiry a refresh starts. Owned policy (**O**), not traced to stock: half
    /// the token lifetime, clamped to 30 s–1 h. The lifetime is assumed one hour without `iat`.
    /// VA 0x0044ed80 only parses `exp` and `iat`.
    pub fn refresh_lead_secs(&self) -> i64 {
        let lifetime = self
            .issued_at
            .map_or(3_600, |iat| self.expires_at.saturating_sub(iat));
        (lifetime / 2).clamp(30, 3_600)
    }

    pub fn needs_refresh(&self, now_secs: i64) -> bool {
        now_secs >= self.expires_at.saturating_sub(self.refresh_lead_secs())
    }
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.trim_end_matches('=').bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("token store failed: {0}")]
pub struct TokenStoreError(pub String);

/// Persistent account state. tetrad-settings owns `/media/p4/AccountConfig.ini` (`[General]`
/// `access_token`, `refresh_token`, `username`); the app implements this trait over it, so the
/// cloud session never touches the file. Implementations must not log tokens.
pub trait TokenStore {
    /// Saved tokens; `None` when either is empty or missing.
    fn load(&mut self) -> Result<Option<Tokens>, TokenStoreError>;
    /// Save both tokens. `username` is `Some` after sign-in and `None` after a refresh, which
    /// keeps the saved name.
    fn store(&mut self, tokens: &Tokens, username: Option<&str>) -> Result<(), TokenStoreError>;
    /// Sign-out: blank both tokens and the username (stock logout writes them as `""`).
    fn clear(&mut self) -> Result<(), TokenStoreError>;
    /// The signed-in account name; `None` when empty.
    fn username(&mut self) -> Result<Option<String>, TokenStoreError>;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Unsigned test JWT with the given JSON claims.
    pub(crate) fn jwt(claims: &str) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in claims.as_bytes().chunks(3) {
            let n =
                chunk.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
            for i in 0..=chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            }
        }
        format!("e30.{out}.sig")
    }

    #[test]
    fn claims_and_refresh_lead() {
        let token =
            jwt(r#"{"sub":"u-1","exp":10000,"iat":6400,"cognito:groups":["neural_internal"]}"#);
        let claims = Claims::parse(&token).unwrap();
        assert_eq!(claims.user_id, "u-1");
        assert!(claims.is_internal());
        assert_eq!(claims.refresh_lead_secs(), 1_800);
        assert!(!claims.needs_refresh(8_199));
        assert!(claims.needs_refresh(8_200));
        assert_eq!(Claims::parse(&jwt(r#"{"sub":"u-1"}"#)), None);
    }
}
