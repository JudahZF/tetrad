//! AccountConfig.ini (`[General]` only) and PluginLicenses.ini.
//!
//! Account controller load `0x0030bce4`, identity save `0x002ff47c`
//! (followed by an immediate sync), token store load `0x004501bc`, save
//! `0x0044d3e4`, logout `0x002ff6e0` + `0x0044d590`. Licences:
//! `0x001f0d5c`, `0x001ef0fc` load; `0x001f2390`, `0x001f123c` save.
//! The legacy keys `token_expires` and `token_expires_datetime` are neither
//! read nor written by 4.1.0; they stay in the file untouched.

use zeroize::Zeroizing;

use crate::Variant;
use crate::typed::{KeyReader, KeyWriter};

/// A credential. `Debug` never shows the value.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() {
            "Secret(<empty>)"
        } else {
            "Secret(<redacted>)"
        })
    }
}

/// `username`, `ui_username`. Logout writes both as `""`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountIdentity {
    pub username: String,
    /// Display name.
    pub ui_username: String,
}

impl AccountIdentity {
    pub fn is_logged_out(&self) -> bool {
        self.username.is_empty()
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        Self {
            username: r.string("username", ""),
            ui_username: r.string("ui_username", ""),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.string("username", &self.username, &old.username);
        w.string("ui_username", &self.ui_username, &old.ui_username);
    }
}

/// Cognito JWTs. Stock wipes both when the access token does not parse
/// ("Invalid API access token loaded from settings file"); JWT checks
/// belong to the cloud client. Logout writes both as `""`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountTokens {
    pub access_token: Secret,
    pub refresh_token: Secret,
}

impl AccountTokens {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        Self {
            access_token: Secret(r.string("access_token", "")),
            refresh_token: Secret(r.string("refresh_token", "")),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.string(
            "access_token",
            self.access_token.expose(),
            old.access_token.expose(),
        );
        w.string(
            "refresh_token",
            self.refresh_token.expose(),
            old.refresh_token.expose(),
        );
    }
}

/// `last_paired_version`: firmware version of the last cloud pairing. Below
/// "3.0.0" (the default "1.0.0" included) and with a token, stock pairs
/// again (`0x002ff5b0`, `0x003083a8`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedVersion(pub String);

impl Default for PairedVersion {
    fn default() -> Self {
        Self("1.0.0".to_owned())
    }
}

impl PairedVersion {
    pub fn needs_pairing(&self) -> bool {
        crate::permanent::version_less(&self.0, "3.0.0")
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        Self(r.string("last_paired_version", "1.0.0"))
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.string("last_paired_version", &self.0, &old.0);
    }
}

/// `[Licenses]` of PluginLicenses.ini. Stock runs `chmod 0644` on the file
/// before each access. Factory reset deletes the file; logout keeps it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PluginLicenses {
    /// `licenses` (`@ByteArray`): encrypted, signed cloud licence payload. Zeroed on drop.
    pub payload: Zeroizing<Vec<u8>>,
    /// `new` (`QList<int>`): plugin ids licensed but not yet acknowledged.
    pub new: Vec<i32>,
    /// `previous` (`QList<int>`): licence set of the last refresh.
    pub previous: Vec<i32>,
}

impl std::fmt::Debug for PluginLicenses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginLicenses")
            .field("payload_len", &self.payload.len())
            .field("new", &self.new)
            .field("previous", &self.previous)
            .finish()
    }
}

impl PluginLicenses {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let payload = match r.variant("Licenses/licenses") {
            Some(Variant::ByteArray(bytes)) => bytes,
            Some(_) => {
                r.issue("Licenses/licenses", crate::LoadIssueKind::NotConvertible);
                Vec::new()
            }
            None => Vec::new(),
        };
        Self {
            payload: Zeroizing::new(payload),
            new: r.int_list("Licenses/new"),
            previous: r.int_list("Licenses/previous"),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.put("Licenses/licenses", &self.payload, &old.payload, |v| {
            Variant::ByteArray(v.to_vec())
        });
        w.put("Licenses/new", &self.new, &old.new, |v| {
            Variant::IntList(v.clone())
        });
        w.put("Licenses/previous", &self.previous, &old.previous, |v| {
            Variant::IntList(v.clone())
        });
    }
}
