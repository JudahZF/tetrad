//! One HTTP/1.1 exchange per connection: read the request, route it, write the response,
//! close. Bodies need `Content-Length`; chunked bodies are refused.

use std::io::{self, Read, Write};

use crate::api::{PIN_HEADER, Reply, Request};

const MAX_HEAD: usize = 8 * 1024;
const MAX_HEADERS: usize = 32;
/// The largest request body. A converted IR is 4,142 bytes.
pub const MAX_BODY: usize = 64 * 1024;

const PAGE: &str = include_str!("page.html");
/// Where a dev build puts the update part of the page ([`page`]).
const UPDATE_MARK: &str = "<!--update-->";
#[cfg(feature = "dev")]
const UPDATE_PART: &str = include_str!("update.html");

/// The page: in a dev build with its "Update Tetrad" part.
fn page() -> String {
    #[cfg(feature = "dev")]
    return PAGE.replace(UPDATE_MARK, UPDATE_PART);
    #[cfg(not(feature = "dev"))]
    PAGE.replace(UPDATE_MARK, "")
}

/// Inline script and style only; the page talks to the unit and nothing else.
const PAGE_POLICY: &str = "default-src 'none'; script-src 'unsafe-inline'; \
    style-src 'unsafe-inline'; connect-src 'self'; img-src data:; form-action 'none'; \
    frame-ancestors 'none'";

struct Head {
    method: String,
    path: String,
    query: String,
    host: Option<String>,
    pin: Option<String>,
    length: Option<usize>,
    chunked: bool,
}

/// Why a request got no device call.
#[derive(Debug)]
enum Fail {
    Status(u16, &'static str),
    Io(io::Error),
}

impl From<io::Error> for Fail {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Serves one connection. `device` answers the requests that need the device; the page and
/// every refusal are answered here.
pub fn serve_one<S: Read + Write>(
    stream: &mut S,
    device: impl FnOnce(Request) -> Reply,
) -> io::Result<()> {
    let request = read_request(stream).and_then(|(head, body)| route(head, body));
    match request {
        Ok(Route::Page) => respond(
            stream,
            200,
            "text/html; charset=utf-8",
            Some(PAGE_POLICY),
            page().as_bytes(),
        ),
        Ok(Route::Device(request)) => {
            let reply = device(request);
            #[cfg(feature = "dev")]
            if let Reply::Log { text } = &reply {
                return respond(
                    stream,
                    200,
                    "text/plain; charset=utf-8",
                    None,
                    text.as_bytes(),
                );
            }
            let (status, body) = reply.response();
            json(stream, status, &body)
        }
        Err(Fail::Status(status, error)) => {
            json(stream, status, &serde_json::json!({ "error": error }))
        }
        Err(Fail::Io(error)) => Err(error),
    }
}

enum Route {
    Page,
    Device(Request),
}

fn route(head: Head, body: Vec<u8>) -> Result<Route, Fail> {
    const PAGE_PATHS: [&str; 2] = ["/", "/index.html"];
    if !head.host.as_deref().is_some_and(ip_host) {
        return Err(Fail::Status(421, "open the page by the unit's IP address"));
    }
    let device = |pin: Option<String>| pin.ok_or(Fail::Status(401, "PIN required"));
    match (head.method.as_str(), head.path.as_str()) {
        ("GET", path) if PAGE_PATHS.contains(&path) => Ok(Route::Page),
        ("GET", "/api/status") => Ok(Route::Device(Request::Status {
            pin: device(head.pin)?,
        })),
        ("POST", "/api/irs") => {
            let pin = device(head.pin)?;
            let name = query_value(&head.query, "name")?
                .filter(|name| !name.trim().is_empty())
                .ok_or(Fail::Status(400, "name required"))?;
            Ok(Route::Device(Request::Upload {
                pin,
                name,
                wav: body,
            }))
        }
        #[cfg(feature = "dev")]
        ("GET", "/api/update") => Ok(Route::Device(Request::UpdateStatus {
            pin: device(head.pin)?,
        })),
        #[cfg(feature = "dev")]
        ("POST", "/api/update/begin") => {
            let pin = device(head.pin)?;
            let bad = || Fail::Status(400, r#"files: {"path": size, ...} required"#);
            let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| bad())?;
            let files = (value.get("files").and_then(serde_json::Value::as_object))
                .ok_or_else(bad)?
                .iter()
                .map(|(path, size)| Ok((path.clone(), size.as_u64().ok_or_else(bad)?)))
                .collect::<Result<_, Fail>>()?;
            Ok(Route::Device(Request::UpdateBegin { pin, files }))
        }
        #[cfg(feature = "dev")]
        ("POST", "/api/update/chunk") => {
            let pin = device(head.pin)?;
            let path =
                query_value(&head.query, "path")?.ok_or(Fail::Status(400, "path required"))?;
            let offset = query_value(&head.query, "offset")?
                .and_then(|offset| offset.parse().ok())
                .ok_or(Fail::Status(400, "offset required"))?;
            Ok(Route::Device(Request::UpdateChunk {
                pin,
                path,
                offset,
                bytes: body,
            }))
        }
        #[cfg(feature = "dev")]
        ("POST", "/api/update/ready") => Ok(Route::Device(Request::UpdateReady {
            pin: device(head.pin)?,
        })),
        #[cfg(feature = "dev")]
        ("POST", "/api/update/install") => Ok(Route::Device(Request::UpdateInstall {
            pin: device(head.pin)?,
        })),
        #[cfg(feature = "dev")]
        ("GET", "/api/auth/nonce") => Ok(Route::Device(Request::AuthNonce)),
        #[cfg(feature = "dev")]
        ("POST", "/api/auth/pin") => {
            let bad = || Fail::Status(400, r#"key, nonce and signature required"#);
            let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| bad())?;
            let text = |key: &str| {
                (value.get(key).and_then(serde_json::Value::as_str))
                    .map(str::to_owned)
                    .ok_or_else(bad)
            };
            Ok(Route::Device(Request::AuthPin {
                key: text("key")?,
                nonce: text("nonce")?,
                signature: text("signature")?,
            }))
        }
        #[cfg(feature = "dev")]
        ("POST", "/api/keys") => {
            let pin = device(head.pin)?;
            let bad = || Fail::Status(400, r#"key required"#);
            let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| bad())?;
            let key = (value.get("key").and_then(serde_json::Value::as_str)).ok_or_else(bad)?;
            Ok(Route::Device(Request::AddKey {
                pin,
                key: key.to_owned(),
            }))
        }
        #[cfg(feature = "dev")]
        ("GET", "/api/log") => Ok(Route::Device(Request::Log {
            pin: device(head.pin)?,
        })),
        #[cfg(feature = "dev")]
        (
            _,
            "/api/update"
            | "/api/update/begin"
            | "/api/update/chunk"
            | "/api/update/ready"
            | "/api/update/install"
            | "/api/auth/nonce"
            | "/api/auth/pin"
            | "/api/keys"
            | "/api/log",
        ) => Err(Fail::Status(405, "method not allowed")),
        ("GET", "/api/files") => Ok(Route::Device(Request::Files {
            pin: device(head.pin)?,
        })),
        ("POST", "/api/files/chunk") => {
            let pin = device(head.pin)?;
            let offset = query_value(&head.query, "offset")?
                .and_then(|value| value.parse().ok())
                .ok_or(Fail::Status(400, "offset required"))?;
            Ok(Route::Device(Request::FileChunk {
                pin,
                id: file_id(&head.query)?,
                name: file_name(&head.query)?,
                offset,
                last: query_value(&head.query, "last")?.as_deref() == Some("1"),
                bytes: body,
            }))
        }
        ("POST", "/api/files/delete") => Ok(Route::Device(Request::FileDelete {
            pin: device(head.pin)?,
            id: file_id(&head.query)?,
            name: file_name(&head.query)?,
        })),
        ("GET", "/api/tabs") => Ok(Route::Device(Request::Tabs {
            pin: device(head.pin)?,
        })),
        ("GET", "/api/tabs/page") => Ok(Route::Device(Request::TabPage {
            pin: device(head.pin)?,
            id: file_id(&head.query)?,
        })),
        ("POST", "/api/tabs/call") => Ok(Route::Device(Request::TabCall {
            pin: device(head.pin)?,
            id: file_id(&head.query)?,
            message: body,
        })),
        (
            _,
            "/api/status" | "/api/irs" | "/api/files" | "/api/files/chunk" | "/api/files/delete"
            | "/api/tabs" | "/api/tabs/page" | "/api/tabs/call",
        ) => Err(Fail::Status(405, "method not allowed")),
        (_, path) if PAGE_PATHS.contains(&path) => Err(Fail::Status(405, "method not allowed")),
        _ => Err(Fail::Status(404, "not found")),
    }
}

fn file_id(query: &str) -> Result<String, Fail> {
    query_value(query, "id")?
        .filter(|id| !id.is_empty())
        .ok_or(Fail::Status(400, "id required"))
}

fn file_name(query: &str) -> Result<String, Fail> {
    query_value(query, "name")?
        .filter(|name| !name.is_empty())
        .ok_or(Fail::Status(400, "name required"))
}

fn read_request(stream: &mut impl Read) -> Result<(Head, Vec<u8>), Fail> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0; 1024];
    let (head, start) = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(Fail::Status(400, "incomplete request"));
        }
        buffer.extend_from_slice(&chunk[..read]);
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(&buffer) {
            Ok(httparse::Status::Complete(length)) if length > MAX_HEAD => {
                return Err(Fail::Status(431, "request header too large"));
            }
            Ok(httparse::Status::Complete(length)) => break (head(&request)?, length),
            Ok(httparse::Status::Partial) if buffer.len() > MAX_HEAD => {
                return Err(Fail::Status(431, "request header too large"));
            }
            Ok(httparse::Status::Partial) => {}
            Err(httparse::Error::TooManyHeaders) => {
                return Err(Fail::Status(431, "too many headers"));
            }
            Err(_) => return Err(Fail::Status(400, "malformed request")),
        }
    };
    if head.chunked {
        return Err(Fail::Status(411, "chunked bodies are not supported"));
    }
    let length = match (head.method.as_str(), head.length) {
        (_, Some(length)) if length > MAX_BODY => {
            return Err(Fail::Status(413, "request body too large"));
        }
        (_, Some(length)) => length,
        ("POST", None) => return Err(Fail::Status(411, "Content-Length required")),
        (_, None) => 0,
    };
    let mut body = buffer.split_off(start.min(buffer.len()));
    body.truncate(length);
    let missing = (length - body.len()) as u64;
    stream.take(missing).read_to_end(&mut body)?;
    if body.len() < length {
        return Err(Fail::Status(400, "incomplete request body"));
    }
    Ok((head, body))
}

fn head(request: &httparse::Request<'_, '_>) -> Result<Head, Fail> {
    let target = request.path.unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut head = Head {
        method: request.method.unwrap_or("").to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        host: None,
        pin: None,
        length: None,
        chunked: false,
    };
    for header in request.headers.iter() {
        let value = || std::str::from_utf8(header.value).map(str::trim);
        if header.name.eq_ignore_ascii_case("host") {
            head.host = value().ok().map(str::to_owned);
        } else if header.name.eq_ignore_ascii_case(PIN_HEADER) {
            let pin = value().map_err(|_| Fail::Status(400, "malformed PIN"))?;
            head.pin = (!pin.is_empty()).then(|| pin.to_owned());
        } else if header.name.eq_ignore_ascii_case("content-length") {
            let length = value().ok().and_then(|text| text.parse().ok());
            head.length = Some(length.ok_or(Fail::Status(400, "malformed Content-Length"))?);
        } else if header.name.eq_ignore_ascii_case("transfer-encoding") {
            head.chunked = true;
        }
    }
    Ok(head)
}

/// A `Host` that is an IPv4 address, with or without a port. A name could be a site that
/// resolves to the unit after its page loaded (DNS rebinding) and then asks it as its own.
fn ip_host(host: &str) -> bool {
    let address = (host.rsplit_once(':'))
        .filter(|(_, port)| port.parse::<u16>().is_ok())
        .map_or(host, |(address, _)| address);
    address.parse::<std::net::Ipv4Addr>().is_ok()
}

/// The decoded value of `key` in a query string (`%XX` escapes, `+` for a space).
fn query_value(query: &str, key: &str) -> Result<Option<String>, Fail> {
    let Some(raw) = (query.split('&'))
        .find_map(|pair| pair.split_once('=').filter(|(name, _)| *name == key))
        .map(|(_, value)| value)
    else {
        return Ok(None);
    };
    let malformed = Fail::Status(400, "malformed query");
    let mut bytes = Vec::with_capacity(raw.len());
    let mut input = raw.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'+' => b' ',
            b'%' => {
                let digits = [input.next(), input.next()];
                let hex = |digit: Option<u8>| (digit.map(char::from)?).to_digit(16);
                match digits.map(hex) {
                    [Some(high), Some(low)] => (high * 16 + low) as u8,
                    _ => return Err(malformed),
                }
            }
            byte => byte,
        });
    }
    String::from_utf8(bytes).map(Some).map_err(|_| malformed)
}

fn json(stream: &mut impl Write, status: u16, body: &serde_json::Value) -> io::Result<()> {
    let body = body.to_string();
    respond(stream, status, "application/json", None, body.as_bytes())
}

fn respond(
    stream: &mut impl Write,
    status: u16,
    content_type: &str,
    policy: Option<&str>,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n",
        body.len()
    );
    if let Some(policy) = policy {
        head.push_str(&format!("Content-Security-Policy: {policy}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}
