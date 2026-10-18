//! Update check and download (`CloudUpdaterAPI` VA 0x0046a8ac): direct over Wi-Fi
//! (`CloudUpdaterWifi::sendRequest` 0x00473050) or through Cortex Control
//! (`CloudUpdaterUSB` 0x0044a928, UpdaterForward 61). Control is used whenever it is connected.
//!
//! Stock verifies the package only with `gzip -t`: no size, hash or signature check. That stays
//! the same here; [`crate::updater::install`] runs the stock installer on the result.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use super::paths;
use crate::cloud::api::CloudApi;
use crate::cloud::session::TokenStore;
use crate::cloud::{ClientSigner, SignError, SignedRequest, sign_request};
use crate::exec::{CancelToken, Command, CommandError, CommandRunner, FileError, Files};
use crate::http::{HttpClient, HttpError, HttpRequest, Method, Route, UpdaterOp};

/// Per-request timeouts (Wi-Fi rodata 0xa44544; USB 0x001c0188 table).
pub const CHECK_TIMEOUT: Duration = Duration::from_millis(60_000);
pub const CHANGELOG_TIMEOUT: Duration = Duration::from_millis(120_000);
pub const HEADER_TIMEOUT: Duration = Duration::from_millis(60_000);
/// USB download deadline, fixed at send time. Stock Wi-Fi instead aborts after 60 s without
/// data; a total cap is owned policy for both.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_millis(1_800_000);
/// Stock reruns a failed flow while attempts < 5, the error is not 105 and the device is online
/// or Control is connected (VA 0x0046bc94).
pub const MAX_ATTEMPTS: u32 = 5;

/// What the check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Available(UpdateOffer),
    /// No `url`, but a `message`: stock code 2, status "no new version".
    UpToDate(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOffer {
    pub version_id: String,
    pub url: String,
    pub changelog_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UpdateError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("server answered HTTP {0}")]
    Status(u16),
    #[error("update reply is not a JSON object")]
    BadReply,
    #[error("update reply has no {0}")]
    Missing(&'static str),
    #[error("update package failed `gzip -t`")]
    Integrity,
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Sign(#[from] SignError),
    #[error("token refresh failed before the update check")]
    Token,
}

impl UpdateError {
    /// The code stock shows through [`super::install::stock_failure_text`]: 3 bad JSON, 4 missing
    /// field or header, 105 integrity, HTTP status as is. Transport errors use 3 (owned).
    pub fn stock_code(&self) -> i32 {
        match self {
            Self::BadReply | Self::Http(_) | Self::Command(_) | Self::File(_) => 3,
            Self::Missing(_) => 4,
            Self::Integrity => 105,
            Self::Status(status) => i32::from(*status),
            Self::Sign(_) | Self::Token => -1,
        }
    }

    /// A cancelled download is never retried (owned).
    pub fn retryable(&self, attempts: u32, online: bool) -> bool {
        !matches!(self, Self::Integrity | Self::Http(HttpError::Cancelled))
            && attempts < MAX_ATTEMPTS
            && online
    }
}

/// Remove leftovers before a new check-and-download (VA 0x0046aa40).
pub fn clear_stale(files: &mut impl Files) -> Result<(), FileError> {
    for path in [
        paths::UPDATE_BIN,
        paths::UPDATE_TMP_BIN,
        paths::UPDATE_TMP_GZ,
    ] {
        files.remove(Path::new(path))?;
    }
    Ok(())
}

/// `restartUpdateDownload` (VA 0x004e7808): drop flags, progress files and the package.
pub fn reset(files: &mut impl Files) -> Result<(), FileError> {
    for path in [
        paths::DOWNLOADED_FLAG,
        paths::AVAILABLE_FLAG,
        paths::DOWNLOAD_FNAME,
        paths::DOWNLOAD_FSIZE,
    ] {
        files.remove(Path::new(path))?;
    }
    clear_stale(files)?;
    files.sync()
}

/// Release info for an `UpdaterMessage` status reply: the first line of `/media/p4/changelog`
/// is the version id, the rest the changelog (VAs 0x004e51c8, 0x004e54a8).
pub fn release_notes(files: &mut impl Files) -> Result<Option<(String, String)>, FileError> {
    Ok(files.read(Path::new(paths::CHANGELOG))?.map(|bytes| {
        let text = String::from_utf8_lossy(&bytes);
        let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
        (first.trim().to_owned(), rest.to_owned())
    }))
}

fn ok_status(status: u16) -> Result<(), UpdateError> {
    match status {
        200..=299 => Ok(()),
        other => Err(UpdateError::Status(other)),
    }
}

impl CloudApi {
    /// `https://cloud-api.<domain>/firmware/v1/latest`, or `/firmware/v1/suffix/<suffix>` for a
    /// signed-in account with a channel suffix (VA 0x0046c5f8).
    pub fn update_check_url(&self, suffix: Option<&str>) -> String {
        let path = match suffix.filter(|s| !s.is_empty() && self.is_signed_in()) {
            Some(suffix) => format!("/firmware/v1/suffix/{suffix}"),
            None => "/firmware/v1/latest".to_owned(),
        };
        format!("{}{path}", self.endpoint.updater_host())
    }

    /// GET the check URL with signed headers. Signed in: refresh first if due; not signed in:
    /// no bearer (VA 0x0046ad98).
    pub fn check_for_update(
        &mut self,
        http: &mut impl HttpClient,
        store: &mut impl TokenStore,
        signer: &mut impl ClientSigner,
        suffix: Option<&str>,
        now: i64,
    ) -> Result<CheckOutcome, UpdateError> {
        if self.is_signed_in() {
            self.ensure_fresh(http, store, now)
                .map_err(|_| UpdateError::Token)?;
        }
        let mut request = self.request(
            Method::Get,
            self.update_check_url(suffix),
            true,
            Route::Updater(UpdaterOp::Check),
        );
        request.timeout = CHECK_TIMEOUT;
        sign_request(
            &mut request,
            signer,
            &self.device,
            SignedRequest::UpdateCheck,
            now,
        )?;
        let response = http.send(&request)?;
        ok_status(response.status)?;
        let Ok(Value::Object(reply)) = serde_json::from_slice::<Value>(&response.body) else {
            return Err(UpdateError::BadReply);
        };
        let text = |key: &str| {
            reply
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        match (text("url"), text("message")) {
            (Some(url), _) => Ok(CheckOutcome::Available(UpdateOffer {
                version_id: text("versionId").unwrap_or_default(),
                url,
                changelog_url: text("changelogUrl"),
            })),
            (None, Some(message)) => Ok(CheckOutcome::UpToDate(message)),
            (None, None) => Err(UpdateError::Missing("url")),
        }
    }
}

/// Save the changelog (if any) and mark the update available (VA 0x0046f5c8).
pub fn publish_offer(
    http: &mut impl HttpClient,
    files: &mut impl Files,
    offer: &UpdateOffer,
) -> Result<(), UpdateError> {
    if let Some(url) = &offer.changelog_url {
        let request = HttpRequest::new(
            Method::Get,
            url.clone(),
            Route::Updater(UpdaterOp::DownloadChangelog),
            CHANGELOG_TIMEOUT,
        );
        let response = http.send(&request)?;
        if !response.is_success() {
            return Err(UpdateError::Missing("changelog"));
        }
        files.write(Path::new(paths::CHANGELOG), &response.body)?;
    }
    files.write(Path::new(paths::AVAILABLE_FLAG), b"")?;
    Ok(files.sync()?)
}

/// Size header, then the package straight into `update.bin`, then `gzip -t`, then the
/// downloaded flag (VAs 0x0046dd54, 0x00475504, 0x00472d64, 0x0046d7d8). The progress files
/// are removed at the end, as when the stock script finishes. `cancel` stops the transfer
/// (stock `0x13b87c` CancelDownload; review r14 S-2).
pub fn download_update(
    http: &mut impl HttpClient,
    files: &mut impl Files,
    runner: &mut impl CommandRunner,
    offer: &UpdateOffer,
    cancel: &CancelToken,
) -> Result<(), UpdateError> {
    let result = fetch_package(http, files, runner, offer, cancel);
    files.remove(Path::new(paths::DOWNLOAD_FNAME))?;
    files.remove(Path::new(paths::DOWNLOAD_FSIZE))?;
    result
}

fn fetch_package(
    http: &mut impl HttpClient,
    files: &mut impl Files,
    runner: &mut impl CommandRunner,
    offer: &UpdateOffer,
    cancel: &CancelToken,
) -> Result<(), UpdateError> {
    let header = HttpRequest::new(
        Method::Get,
        offer.url.clone(),
        Route::Updater(UpdaterOp::DownloadUpdateHeader),
        HEADER_TIMEOUT,
    )
    .cancel(cancel.clone());
    let length =
        content_length(&http.send(&header)?).ok_or(UpdateError::Missing("Content-Length"))?;
    let package = Path::new(paths::UPDATE_BIN);
    files.write(
        Path::new(paths::DOWNLOAD_FNAME),
        paths::UPDATE_BIN.as_bytes(),
    )?;
    files.write(
        Path::new(paths::DOWNLOAD_FSIZE),
        length.to_string().as_bytes(),
    )?;
    files.remove(package)?;
    files.sync()?;

    let request = HttpRequest::new(
        Method::Get,
        offer.url.clone(),
        Route::Updater(UpdaterOp::DownloadUpdate),
        DOWNLOAD_TIMEOUT,
    )
    .cancel(cancel.clone());
    // A partial package is useless: the next try starts over.
    let fetched = http
        .download(&request, package)
        .map_err(UpdateError::from)
        .and_then(ok_status);
    if let Err(error) = fetched {
        files.remove(package)?;
        return Err(error);
    }
    files.sync()?;
    let check = runner.run(&Command::new("gzip", ["-t", paths::UPDATE_BIN]))?;
    if !check.status.success() {
        files.remove(package)?;
        return Err(UpdateError::Integrity);
    }
    files.write(
        Path::new(paths::DOWNLOADED_FLAG),
        offer.version_id.as_bytes(),
    )?;
    files.remove(Path::new(paths::AVAILABLE_FLAG))?;
    Ok(files.sync()?)
}

/// The header reply is the JSON object Cortex Control returns, `{"Content-Length": n}`
/// (VA 0x0044c7e4); direct clients produce the same shape.
fn content_length(response: &crate::http::HttpResponse) -> Option<u64> {
    if !response.is_success() {
        return None;
    }
    let reply: Value = serde_json::from_slice(&response.body).ok()?;
    match reply.get("Content-Length")? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::api::tests::api;
    use crate::exec::Output;
    use crate::fake::{MemFiles, ScriptedHttp, ScriptedRunner};

    struct FixedSigner;

    impl ClientSigner for FixedSigner {
        fn sign(&mut self, _: &str) -> Result<String, SignError> {
            Ok("ab12".into())
        }
        fn client_id(&mut self) -> Result<String, SignError> {
            Ok("aWQ=".into())
        }
    }

    #[test]
    fn check_publish_and_download() {
        let (mut cloud, _, mut store) = api();
        let mut files = MemFiles::default();
        let mut http = ScriptedHttp::sharing(files.clone());
        http.respond(
            200,
            r#"{"url":"https://dl.example/u.bin","versionId":"4.2.0","changelogUrl":"https://dl.example/c.txt"}"#,
        );
        let CheckOutcome::Available(offer) = cloud
            .check_for_update(&mut http, &mut store, &mut FixedSigner, Some("beta"), 100)
            .unwrap()
        else {
            panic!("expected an offer");
        };
        let sent = &http.requests[0];
        // Not signed in: no suffix, no bearer.
        assert_eq!(
            sent.url,
            "https://cloud-api.neuraldsp.com/firmware/v1/latest"
        );
        assert_eq!(sent.header_value("Authorization"), None);
        assert_eq!(
            sent.header_value("X-Cloud-Api-Client-Signature"),
            Some("ab12")
        );

        http.respond(200, "4.2.0\nFixes\n");
        publish_offer(&mut http, &mut files, &offer).unwrap();
        assert_eq!(
            release_notes(&mut files).unwrap(),
            Some(("4.2.0".into(), "Fixes\n".into()))
        );

        http.respond(206, r#"{"Content-Length":"4"}"#);
        http.respond(200, vec![0x1f, 0x8b, 0, 0]);
        let mut runner = ScriptedRunner::default();
        runner.expect(
            "gzip -t /media/p4/UpdaterTemp/update.bin",
            Ok(Output::code(0)),
        );
        download_update(
            &mut http,
            &mut files,
            &mut runner,
            &offer,
            &CancelToken::default(),
        )
        .unwrap();
        assert_eq!(files.get(paths::DOWNLOADED_FLAG).unwrap(), b"4.2.0");
        assert!(files.get(paths::AVAILABLE_FLAG).is_none());
        assert!(files.get(paths::DOWNLOAD_FSIZE).is_none());
        assert_eq!(files.get(paths::UPDATE_BIN).unwrap().len(), 4);
    }

    #[test]
    fn no_update_and_failed_integrity() {
        let (mut cloud, _, mut store) = api();
        let mut files = MemFiles::default();
        let mut http = ScriptedHttp::sharing(files.clone());
        http.respond(200, r#"{"message":"Up to date"}"#);
        assert_eq!(
            cloud
                .check_for_update(&mut http, &mut store, &mut FixedSigner, None, 0)
                .unwrap(),
            CheckOutcome::UpToDate("Up to date".into())
        );
        http.respond(200, "[]");
        assert_eq!(
            cloud
                .check_for_update(&mut http, &mut store, &mut FixedSigner, None, 0)
                .unwrap_err()
                .stock_code(),
            3
        );

        let offer = UpdateOffer {
            version_id: "4.2.0".into(),
            url: "https://dl.example/u.bin".into(),
            changelog_url: None,
        };
        http.respond(200, r#"{"Content-Length":4}"#);
        http.respond(200, vec![1, 2, 3, 4]);
        let mut runner = ScriptedRunner::default();
        runner.expect(
            "gzip -t /media/p4/UpdaterTemp/update.bin",
            Ok(Output::code(1)),
        );
        let error = download_update(
            &mut http,
            &mut files,
            &mut runner,
            &offer,
            &CancelToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.stock_code(), 105);
        assert!(!error.retryable(1, true));
        assert!(files.get(paths::DOWNLOADED_FLAG).is_none());
        assert!(files.get(paths::UPDATE_BIN).is_none());
    }
}
