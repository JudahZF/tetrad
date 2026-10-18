//! HTTP as an injected client. Each request names the Cortex Control forward message that carries
//! it over USB, so one request runs directly on Wi-Fi or through Product/Backups/Logs/
//! UpdaterForward (messages 29/30/31/61) when Cortex Control does the HTTP for the device.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use zeroize::Zeroize;

use crate::exec::{
    CancelToken, Command, CommandError, CommandRunner, ExitStatus, FileError, Files, Secret,
};

/// `HTTPOperation.Enum` values, as carried by `ProductForwardRequest.operation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get = 0,
    Put = 1,
    Post = 2,
    Delete = 3,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }
}

/// `BackupsForwardRequest.RequestType`. Stock 4.1.0 sends only `CreateOrUpdate` and
/// `Download` (`0x148e54`); the other backup calls are JSON calls ([`Route::Product`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupsOp {
    CreateOrUpdate = 0,
    Download = 1,
    Rename = 2,
    Delete = 3,
    List = 4,
    FetchUrl = 5,
    UploadStatus = 6,
}

/// `LogsForwardRequest.RequestType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogsOp {
    FetchUrls = 0,
    UploadLogs = 1,
    UploadCoredump = 2,
}

/// `UpdaterForwardRequest.RequestType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdaterOp {
    Check = 0,
    DownloadChangelog = 1,
    /// The reply body is a JSON object `{"Content-Length": n}`, as Cortex Control returns it
    /// (VA 0x0044c7e4). Direct clients must produce the same shape.
    DownloadUpdateHeader = 2,
    DownloadUpdate = 3,
}

/// The forward message that carries a request over USB. Direct clients ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// ProductForward (29): JSON API calls; carries the method.
    Product,
    /// BackupsForward (30): a backup body transfer only (upload `0x1458ac`, download
    /// `0x1417b4`), chunked, typed by operation.
    Backups(BackupsOp),
    /// LogsForward (31).
    Logs(LogsOp),
    /// UpdaterForward (61): chunked download.
    Updater(UpdaterOp),
}

#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    /// Values can hold bearer tokens; `Debug` prints names only.
    pub headers: Vec<(String, String)>,
    /// Can hold a password; `Debug` prints the length only.
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
    pub route: Route,
    /// Set: the transfer stops when the token is cancelled ([`HttpError::Cancelled`]).
    pub cancel: Option<CancelToken>,
    /// Set (a Directory upload, `0x725ba4`): the boundary of a `multipart/form-data` form whose
    /// one part is the JSON `body` ([`Self::form`], `0x80ccf8` case `+0x10 == 1`). The request
    /// then has no `Content-Type`: a direct client sends the form's own, and Cortex Control
    /// gets the JSON with `is_multipart` (`0x80c254`).
    pub multipart: Option<String>,
}

impl HttpRequest {
    pub fn new(method: Method, url: impl Into<String>, route: Route, timeout: Duration) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            timeout,
            route,
            cancel: None,
            multipart: None,
        }
    }

    pub fn cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// The `Content-Type` and the body of a [`Self::multipart`] request as a direct client sends
    /// them (U1, `0x80ccf8`): one part with the raw headers `Content-Type: application/json`
    /// and `Content-Disposition: form-data; name="productFile"; filename=""` (C), framed as
    /// `QHttpMultiPart` frames it (I: Qt's RFC 2046 layout). `None`: not multipart.
    pub fn form(&self) -> Option<(String, Vec<u8>)> {
        let boundary = self.multipart.as_deref()?;
        let json = self.body.as_deref().unwrap_or_default();
        let head = format!(
            "--{boundary}\r\nContent-Type: application/json\r\nContent-Disposition: form-data; \
             name=\"productFile\"; filename=\"\"\r\n\r\n"
        );
        let tail = format!("\r\n--{boundary}--\r\n");
        let form = [head.as_bytes(), json, tail.as_bytes()].concat();
        Some((format!("multipart/form-data; boundary={boundary}"), form))
    }

    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Header values (tokens, client id) and the body (passwords) are zeroed on drop.
impl Drop for HttpRequest {
    fn drop(&mut self) {
        for (_, value) in &mut self.headers {
            value.zeroize();
        }
        self.body.zeroize();
    }
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self
            .url
            .split_once('?')
            .map_or(self.url.as_str(), |(path, _)| path);
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &url)
            .field(
                "headers",
                &self.headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("body_len", &self.body.as_ref().map(Vec::len))
            .field("timeout", &self.timeout)
            .field("route", &self.route)
            .field("cancellable", &self.cancel.is_some())
            .field("multipart", &self.multipart.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HttpError {
    #[error("request timed out")]
    Timeout,
    #[error("request cancelled")]
    Cancelled,
    #[error("transport failed: {0}")]
    Transport(String),
    /// The forwarding host reported this `error_code` in its forward response.
    #[error("forwarded request failed with code {0}")]
    Forward(i32),
}

pub trait HttpClient {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, HttpError>;

    /// Write the response body to `dest`, replacing it, and return the HTTP status. Callers read
    /// progress from the file size, as stock does (VA 0x004e8824).
    fn download(&mut self, request: &HttpRequest, dest: &Path) -> Result<u16, HttpError>;
}

/// Direct HTTPS through the device's `curl` 7.40 (`/usr/bin/curl`). Owned policy: the stock app
/// uses `QNetworkAccessManager`; curl avoids a TLS stack in this crate.
///
/// The curl config goes through stdin (`-K -`), so tokens never appear in argv. Text bodies are
/// embedded in the config; binary bodies go through `scratch`, which is removed afterwards.
pub struct CurlHttp<R, F> {
    pub runner: R,
    pub files: F,
    pub scratch: PathBuf,
}

/// curl exit code for `--max-time` expiry.
const CURL_TIMEOUT: i32 = 28;

/// Largest header dump the size probe reads back. Real update headers are far smaller.
const PROBE_HEADER_LIMIT: u64 = 16 * 1024;

impl<R: CommandRunner, F: Files> CurlHttp<R, F> {
    fn config(&mut self, request: &HttpRequest) -> Result<(String, bool), HttpError> {
        let mut config = format!(
            "silent\nshow-error\nproto = \"=https\"\nproto-redir = \"=https\"\nurl = {}\nrequest = {}\nmax-time = {}\nwrite-out = \"\\n%{{http_code}}\"\n",
            quote(&request.url),
            quote(request.method.as_str()),
            request.timeout.as_secs_f64(),
        );
        for (name, value) in &request.headers {
            config += &format!("header = {}\n", quote(&format!("{name}: {value}")));
        }
        // Stock sets only the Content-Type; Qt 4.8.7 `post/put(req, QHttpMultiPart*)` adds
        // `MIME-Version: 1.0` when it is absent (lib `0x4fa44` → `0x4fb68`).
        let form = request.form().map(|(content_type, form)| {
            config += &format!(
                "header = {}\nheader = \"MIME-Version: 1.0\"\n",
                quote(&format!("Content-Type: {content_type}"))
            );
            form
        });
        let mut used_scratch = false;
        if let Some(body) = form.as_ref().or(request.body.as_ref()) {
            match std::str::from_utf8(body) {
                Ok(text) if !text.contains('\0') && !text.starts_with('@') => {
                    config += &format!("data-binary = {}\n", quote(text));
                }
                _ => {
                    self.files
                        .write(&self.scratch, body)
                        .map_err(|e| HttpError::Transport(e.to_string()))?;
                    used_scratch = true;
                    config += &format!(
                        "data-binary = {}\n",
                        quote(&format!("@{}", self.scratch.display()))
                    );
                }
            }
        }
        Ok((config, used_scratch))
    }

    /// Runs curl; a cancelled request kills it.
    fn curl(
        &mut self,
        config: String,
        used_scratch: bool,
        cancel: Option<&CancelToken>,
    ) -> Result<Vec<u8>, HttpError> {
        let command = Command::new("curl", ["-K", "-"]).stdin(Secret::new(config));
        let result = match cancel {
            Some(token) => self.runner.run_cancellable(&command, token),
            None => self.runner.run(&command),
        };
        if used_scratch {
            let _ = self.files.remove(&self.scratch);
        }
        let output = result.map_err(|e| match e {
            CommandError::Cancelled { .. } => HttpError::Cancelled,
            e => HttpError::Transport(e.to_string()),
        })?;
        match output.status {
            ExitStatus::Code(0) => Ok(output.stdout),
            ExitStatus::Code(CURL_TIMEOUT) => Err(HttpError::Timeout),
            status => Err(HttpError::Transport(format!("curl ended with {status:?}"))),
        }
    }

    /// Read and delete the header dump of a size probe. Oversized or missing dumps are errors.
    fn take_header_dump(&mut self, path: &Path) -> Result<String, HttpError> {
        let transport = |e: FileError| HttpError::Transport(e.to_string());
        let size = self.files.size(path).map_err(transport)?;
        let bytes = match size {
            Some(len) if len <= PROBE_HEADER_LIMIT => self.files.read(path).map_err(transport)?,
            _ => None,
        };
        let _ = self.files.remove(path);
        bytes
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .ok_or_else(|| HttpError::Transport("size probe headers missing or too large".into()))
    }
}

/// Split curl stdout into body and the trailing `\n%{http_code}`.
fn split_status(mut stdout: Vec<u8>) -> Result<(u16, Vec<u8>), HttpError> {
    let at = stdout
        .iter()
        .rposition(|&b| b == b'\n')
        .ok_or_else(|| HttpError::Transport("curl gave no status".into()))?;
    let status = std::str::from_utf8(&stdout[at + 1..])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|&code| code != 0)
        .ok_or_else(|| HttpError::Transport("curl gave no status".into()))?;
    stdout.truncate(at);
    Ok((status, stdout))
}

/// curl config string quoting: backslash escapes `\\`, `\"`, `\t`, `\n`, `\r`.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Total size from response headers: `Content-Range: bytes 0-0/<total>` for a 206, else the last
/// `Content-Length`.
fn header_total(headers: &str) -> Option<u64> {
    let value = |name: &str| {
        headers.lines().rev().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    value("Content-Range")
        .and_then(|range| range.rsplit_once('/')?.1.parse().ok())
        .or_else(|| value("Content-Length")?.parse().ok())
}

impl<R: CommandRunner, F: Files> HttpClient for CurlHttp<R, F> {
    fn send(&mut self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let (mut config, used_scratch) = self.config(request)?;
        let header_only = request.route == Route::Updater(UpdaterOp::DownloadUpdateHeader);
        let dump = self.scratch.with_extension("headers");
        if header_only {
            // Owned: stock Wi-Fi GETs and drops the body after the headers. A one-byte range
            // works with presigned GET URLs, which often refuse HEAD. The body goes to
            // /dev/null, so a server that ignores the range cannot fill memory.
            config += &format!(
                "range = \"0-0\"\noutput = \"/dev/null\"\ndump-header = {}\n",
                quote(&dump.display().to_string())
            );
        }
        let (status, body) =
            split_status(self.curl(config, used_scratch, request.cancel.as_ref())?)?;
        if !header_only {
            return Ok(HttpResponse { status, body });
        }
        if !body.is_empty() {
            return Err(HttpError::Transport("size probe returned a body".into()));
        }
        let headers = self.take_header_dump(&dump)?;
        let total = header_total(&headers)
            .ok_or_else(|| HttpError::Transport("no Content-Length".into()))?;
        Ok(HttpResponse {
            status,
            body: format!("{{\"Content-Length\":{total}}}").into_bytes(),
        })
    }

    fn download(&mut self, request: &HttpRequest, dest: &Path) -> Result<u16, HttpError> {
        let (mut config, used_scratch) = self.config(request)?;
        config += &format!("output = {}\n", quote(&dest.display().to_string()));
        let (status, _) =
            split_status(self.curl(config, used_scratch, request.cancel.as_ref())?)?;
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{MemFiles, ScriptedRunner};

    fn curl() -> CurlHttp<ScriptedRunner, MemFiles> {
        CurlHttp {
            runner: ScriptedRunner::default(),
            files: MemFiles::default(),
            scratch: "/tmp/tetrad-http-body".into(),
        }
    }

    #[test]
    fn curl_keeps_tokens_out_of_argv_and_splits_status() {
        let mut http = curl();
        http.runner.expect(
            "curl -K -",
            Ok(crate::exec::Output::code(0).with_stdout(b"{\"a\":1}\n401".to_vec())),
        );
        let request = HttpRequest::new(
            Method::Post,
            "https://cloud-api.example/api/v1/x",
            Route::Product,
            Duration::from_secs(30),
        )
        .header("Authorization", "Bearer tok\"en")
        .body(br#"{"k":"v\n"}"#.to_vec());
        let response = http.send(&request).unwrap();
        assert_eq!(response.status, 401);
        assert_eq!(response.body, br#"{"a":1}"#);
        let sent = &http.runner.calls[0];
        assert_eq!(sent.args, ["-K", "-"]);
        let config = String::from_utf8(sent.stdin.clone().unwrap().expose().to_vec()).unwrap();
        assert!(config.contains(r#"header = "Authorization: Bearer tok\"en""#));
        assert!(config.contains(r#"data-binary = "{\"k\":\"v\\n\"}""#));
        assert!(config.contains("request = \"POST\""));
        assert!(config.contains("proto = \"=https\"\n"));
        assert!(config.contains("proto-redir = \"=https\"\n"));
        assert!(
            !config.contains("MIME-Version"),
            "only a multipart request has it"
        );
        assert!(!format!("{request:?}").contains("tok"));
    }

    /// U1 (`0x80ccf8` case `+0x10 == 1`): the Wi-Fi upload is one `productFile` part with the
    /// JSON body and the form's own Content-Type; no other Content-Type goes out.
    #[test]
    fn upload_body_is_one_productfile_part() {
        let mut http = curl();
        http.runner.expect(
            "curl -K -",
            Ok(crate::exec::Output::code(0).with_stdout(b"{}\n200".to_vec())),
        );
        let mut request = HttpRequest::new(
            Method::Post,
            "https://cloud-api.example/api/v1/user/products",
            Route::Product,
            Duration::from_secs(30),
        )
        .header("X-Cloud-Api-Meta-Signature", "c2ln")
        .body(br#"{"name":"Lead"}"#.to_vec());
        request.multipart = Some("b-1".into());
        assert_eq!(
            request.form(),
            Some((
                "multipart/form-data; boundary=b-1".to_owned(),
                b"--b-1\r\nContent-Type: application/json\r\nContent-Disposition: form-data; \
                  name=\"productFile\"; filename=\"\"\r\n\r\n{\"name\":\"Lead\"}\r\n--b-1--\r\n"
                    .to_vec()
            ))
        );
        http.send(&request).unwrap();
        let sent = &http.runner.calls[0];
        let config = String::from_utf8(sent.stdin.clone().unwrap().expose().to_vec()).unwrap();
        let headers: Vec<_> = (config.lines())
            .filter(|l| l.starts_with("header"))
            .collect();
        assert_eq!(
            headers,
            [
                r#"header = "X-Cloud-Api-Meta-Signature: c2ln""#,
                r#"header = "Content-Type: multipart/form-data; boundary=b-1""#,
                // Review-r45 M3: Qt 4.8.7 `post(req, QHttpMultiPart*)` adds it (lib `0x4fb68`).
                r#"header = "MIME-Version: 1.0""#,
            ]
        );
        assert!(config.contains(
            r#"data-binary = "--b-1\r\nContent-Type: application/json\r\nContent-Disposition: form-data; name=\"productFile\"; filename=\"\"\r\n\r\n{\"name\":\"Lead\"}\r\n--b-1--\r\n""#
        ));
    }

    #[test]
    fn debug_omits_url_query() {
        let request = HttpRequest::new(
            Method::Get,
            "https://dl.example/u.bin?X-Amz-Signature=secret",
            Route::Updater(UpdaterOp::DownloadUpdate),
            Duration::from_secs(5),
        );
        let debug = format!("{request:?}");
        assert!(debug.contains("https://dl.example/u.bin"));
        assert!(!debug.contains("secret"));
    }

    fn probe_request() -> HttpRequest {
        HttpRequest::new(
            Method::Get,
            "https://dl.example/u.bin",
            Route::Updater(UpdaterOp::DownloadUpdateHeader),
            Duration::from_secs(60),
        )
    }

    #[test]
    fn curl_update_header_reports_total_size() {
        let mut http = curl();
        http.files.put(
            "/tmp/tetrad-http-body.headers",
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/123456\r\n\r\n",
        );
        http.runner.expect(
            "curl -K -",
            Ok(crate::exec::Output::code(0).with_stdout(b"\n206".to_vec())),
        );
        let response = http.send(&probe_request()).unwrap();
        assert_eq!(response.body, br#"{"Content-Length":123456}"#);
        let sent = &http.runner.calls[0];
        let config = String::from_utf8(sent.stdin.clone().unwrap().expose().to_vec()).unwrap();
        assert!(config.contains("output = \"/dev/null\""));
        assert!(config.contains("dump-header = \"/tmp/tetrad-http-body.headers\""));
        assert!(http.files.get("/tmp/tetrad-http-body.headers").is_none());
    }

    /// A server that ignores `Range` must not fill memory: the probe body goes to /dev/null, so
    /// any body reaching stdout, or an oversized header dump, is refused.
    #[test]
    fn size_probe_refuses_body_and_oversized_headers() {
        let mut http = curl();
        let huge = [vec![b'x'; 8 << 20], b"\n200".to_vec()].concat();
        http.runner.expect(
            "curl -K -",
            Ok(crate::exec::Output::code(0).with_stdout(huge)),
        );
        assert!(matches!(
            http.send(&probe_request()),
            Err(HttpError::Transport(_))
        ));

        http.files.put(
            "/tmp/tetrad-http-body.headers",
            vec![b'h'; PROBE_HEADER_LIMIT as usize + 1],
        );
        http.runner.expect(
            "curl -K -",
            Ok(crate::exec::Output::code(0).with_stdout(b"\n200".to_vec())),
        );
        assert!(matches!(
            http.send(&probe_request()),
            Err(HttpError::Transport(_))
        ));
        assert!(http.files.get("/tmp/tetrad-http-body.headers").is_none());
    }

    /// Review r14 S-2: a cancelled download kills curl (the runner's cancellable run) and
    /// reports `Cancelled`, not a transport failure that the updater would retry.
    #[test]
    fn cancelled_download_kills_curl() {
        let mut http = curl();
        http.runner.expect(
            "curl -K -",
            Ok(crate::exec::Output::code(0).with_stdout(b"\n200".to_vec())),
        );
        let token = CancelToken::default();
        let request = HttpRequest::new(
            Method::Get,
            "https://dl.example/u.bin",
            Route::Updater(UpdaterOp::DownloadUpdate),
            Duration::from_secs(60),
        )
        .cancel(token.clone());
        token.cancel();
        assert_eq!(
            http.download(&request, Path::new("/tmp/u.bin")),
            Err(HttpError::Cancelled)
        );
        assert!(http.runner.is_done());
    }

    #[test]
    fn curl_binary_body_uses_scratch_file_and_removes_it() {
        let mut http = curl();
        http.runner.expect(
            "curl -K -",
            Ok(crate::exec::Output::code(0).with_stdout(b"\n200".to_vec())),
        );
        let request = HttpRequest::new(
            Method::Put,
            "https://x.example/u",
            Route::Logs(LogsOp::UploadLogs),
            Duration::from_secs(5),
        )
        .body(vec![0u8, 159, 146]);
        assert_eq!(http.send(&request).unwrap().status, 200);
        assert!(http.files.get("/tmp/tetrad-http-body").is_none());
        http.runner
            .expect("curl -K -", Ok(crate::exec::Output::code(28)));
        assert_eq!(http.send(&request), Err(HttpError::Timeout));
    }
}
