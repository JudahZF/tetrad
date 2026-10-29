use std::io::{self, Cursor, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use tetrad_remote::{
    BackupOp, Call, EditorOp, ExtensionFiles, ExtensionsOp, FileEntry, LibraryOp, MAX_BODY, Reply,
    Request, Server, TabInfo, ThemesOp, api_request, serve_one,
};

/// A connection: the request bytes in, the response bytes out.
struct Pipe {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Serves `request`; the device answers `reply`. Returns the status line, the whole response
/// and the request the device saw.
fn exchange(request: &[u8], reply: Reply) -> (String, String, Option<Request>) {
    let mut pipe = Pipe {
        input: Cursor::new(request.to_vec()),
        output: Vec::new(),
    };
    let mut seen = None;
    serve_one(&mut pipe, |request| {
        seen = Some(request);
        reply
    })
    .unwrap();
    let response = String::from_utf8(pipe.output).unwrap();
    let status = response.lines().next().unwrap_or_default().to_owned();
    (status, response, seen)
}

fn upload(head: &str, body: &[u8]) -> Vec<u8> {
    let mut request = head.as_bytes().to_vec();
    request.extend_from_slice(body);
    request
}

/// The page and the two device routes: the PIN, the decoded name and the body reach the
/// device; its reply becomes the status code and JSON body.
#[test]
fn routes_reach_the_device_with_pin_name_and_body() {
    let (status, response, seen) =
        exchange(b"GET / HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n", Reply::Off);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.contains("Content-Security-Policy: default-src 'none'"));
    assert!(response.contains("<title>Tetrad Remote</title>"));
    assert_eq!(seen, None);

    let status_request =
        b"GET /api/status HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 4821\r\n\r\n";
    let folder = Reply::Status {
        folder: "IRs".into(),
    };
    let (status, response, seen) = exchange(status_request, folder);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.ends_with(r#"{"folder":"IRs"}"#));
    assert_eq!(seen, Some(Request::Status { pin: "4821".into() }));

    let wav = [7_u8; 100];
    let head = "POST /api/irs?x=1&name=V30%20SM57+%C3%A9 HTTP/1.1\r\nHost: 192.168.1.20\r\n\
                x-tetrad-pin: 4821\r\nContent-Length: 100\r\n\r\n";
    let added = Reply::Added {
        name: "V30 SM57 é".into(),
        folder: "IRs".into(),
    };
    let (status, _, seen) = exchange(&upload(head, &wav), added);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        seen,
        Some(Request::Upload {
            pin: "4821".into(),
            name: "V30 SM57 é".into(),
            wav: wav.to_vec(),
        })
    );

    let (status, response, _) = exchange(status_request, Reply::WrongPin);
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert!(response.ends_with(r#"{"error":"Wrong PIN"}"#));
    let refused = Reply::Refused("the library is closed".into());
    assert_eq!(
        exchange(status_request, refused).0,
        "HTTP/1.1 422 Unprocessable Content"
    );
}

/// The support file needs the PIN like every device route, and comes back as the zip itself
/// with a download name, not as JSON.
#[test]
fn support_needs_the_pin_and_downloads_the_zip() {
    let (status, _, seen) = exchange(
        b"GET /api/support HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n",
        Reply::Off,
    );
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert_eq!(seen, None);

    let request = b"GET /api/support HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 4821\r\n\r\n";
    let (status, _, seen) = exchange(request, Reply::WrongPin);
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert_eq!(seen, Some(Request::Support { pin: "4821".into() }));

    let file = Reply::File {
        name: "tetrad-support-2026-10-27-2041.zip".into(),
        zip: b"PK zip bytes".to_vec(),
    };
    let (status, response, _) = exchange(request, file);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.contains("Content-Type: application/zip\r\n"));
    assert!(response.contains(
        "Content-Disposition: attachment; filename=\"tetrad-support-2026-10-27-2041.zip\"\r\n"
    ));
    assert!(response.contains("Cache-Control: no-store\r\n"));
    assert!(response.ends_with("\r\n\r\nPK zip bytes"));

    let post = b"POST /api/support HTTP/1.1\r\nHost: 192.168.1.20\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(
        exchange(post, Reply::Off).0,
        "HTTP/1.1 405 Method Not Allowed"
    );
}

/// Requests the server refuses itself never reach the device.
/// The file manager: list, a piece of a file and a removal reach the device with the PIN, the
/// decoded names and the body; the list becomes JSON for the page.
#[test]
fn the_file_routes_reach_the_device() {
    let list = Reply::Files {
        extensions: vec![ExtensionFiles {
            id: "tetrad.nam".into(),
            name: "Neural Amp Modeler A1".into(),
            label: "NAM model".into(),
            extensions: vec![".nam".into()],
            max_bytes: 3 << 20,
            files: vec![FileEntry {
                name: "Twin Custom2.nam".into(),
                size: 27324,
                state: "ready".into(),
                detail: String::new(),
            }],
        }],
    };
    let (status, response, seen) = exchange(
        b"GET /api/files HTTP/1.1\r\nHost: 10.0.0.41\r\nX-Tetrad-Pin: 4821\r\n\r\n",
        list,
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(seen, Some(Request::Files { pin: "4821".into() }));
    let json: serde_json::Value =
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let extension = &json["extensions"][0];
    assert_eq!(extension["id"], "tetrad.nam");
    assert_eq!(extension["extensions"][0], ".nam");
    assert_eq!(extension["maxBytes"], 3 << 20);
    assert_eq!(extension["files"][0]["size"], 27324);
    assert_eq!(extension["files"][0]["state"], "ready");

    let head = "POST /api/files/chunk?id=tetrad.nam&name=Twin%20Custom2.nam&offset=49152&last=1 \
                HTTP/1.1\r\nHost: 10.0.0.41\r\nx-tetrad-pin: 4821\r\nContent-Length: 3\r\n\r\n";
    let stored = Reply::FileStored {
        name: "Twin Custom2.nam".into(),
        size: 49155,
    };
    let (status, response, seen) = exchange(&upload(head, b"abc"), stored);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.ends_with(r#"{"name":"Twin Custom2.nam","size":49155}"#));
    assert_eq!(
        seen,
        Some(Request::FileChunk {
            pin: "4821".into(),
            id: "tetrad.nam".into(),
            name: "Twin Custom2.nam".into(),
            offset: 49152,
            last: true,
            bytes: b"abc".to_vec(),
        })
    );
    // Without `last=1` the piece is not the end.
    let head = head.replace("&last=1", "");
    let (_, _, seen) = exchange(&upload(&head, b"abc"), Reply::FileReceived { received: 3 });
    assert!(matches!(seen, Some(Request::FileChunk { last: false, .. })));

    let delete = b"POST /api/files/delete?id=tetrad.nam&name=A.nam HTTP/1.1\r\nHost: 10.0.0.41\r\n\
                   X-Tetrad-Pin: 4821\r\nContent-Length: 0\r\n\r\n";
    let (status, _, seen) = exchange(
        delete,
        Reply::FileDeleted {
            name: "A.nam".into(),
        },
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        seen,
        Some(Request::FileDelete {
            pin: "4821".into(),
            id: "tetrad.nam".into(),
            name: "A.nam".into(),
        })
    );
}

/// What the file routes refuse before the device is asked.
#[test]
fn bad_file_requests_are_refused_without_a_device_call() {
    let host = "Host: 10.0.0.41\r\nX-Tetrad-Pin: 4821\r\n";
    for (request, expected) in [
        // Offset, id and name are required; the offset is a number.
        (
            format!(
                "POST /api/files/chunk?id=a.b&name=A.nam HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"
            ),
            "HTTP/1.1 400 Bad Request",
        ),
        (
            format!(
                "POST /api/files/chunk?id=a.b&name=A.nam&offset=x HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"
            ),
            "HTTP/1.1 400 Bad Request",
        ),
        (
            format!(
                "POST /api/files/chunk?name=A.nam&offset=0 HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"
            ),
            "HTTP/1.1 400 Bad Request",
        ),
        (
            format!("POST /api/files/delete?id=a.b HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"),
            "HTTP/1.1 400 Bad Request",
        ),
        // The PIN comes first, and the method has to fit.
        (
            "GET /api/files HTTP/1.1\r\nHost: 10.0.0.41\r\n\r\n".to_owned(),
            "HTTP/1.1 401 Unauthorized",
        ),
        (
            format!("POST /api/files HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"),
            "HTTP/1.1 405 Method Not Allowed",
        ),
        (
            format!("GET /api/files/chunk HTTP/1.1\r\n{host}\r\n"),
            "HTTP/1.1 405 Method Not Allowed",
        ),
        // A piece is a request body: it needs Content-Length.
        (
            format!("POST /api/files/chunk?id=a.b&name=A.nam&offset=0 HTTP/1.1\r\n{host}\r\n"),
            "HTTP/1.1 411 Length Required",
        ),
    ] {
        let (status, _, seen) = exchange(request.as_bytes(), Reply::Off);
        assert_eq!(status, expected, "{request}");
        assert_eq!(seen, None, "{request}");
    }
}

/// The tab routes: the list, the page and a message reach the device with the PIN and the decoded
/// id; the answers become JSON, and the extension's own JSON text becomes `result`.
#[test]
fn the_tab_routes_reach_the_device() {
    let host = "Host: 10.0.0.41\r\nX-Tetrad-Pin: 4821\r\n";
    let tabs = Reply::Tabs {
        tabs: vec![TabInfo {
            id: "tetrad.nam".into(),
            title: "Models".into(),
        }],
    };
    let (status, response, seen) = exchange(
        format!("GET /api/tabs HTTP/1.1\r\n{host}\r\n").as_bytes(),
        tabs,
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.ends_with(r#"{"tabs":[{"id":"tetrad.nam","title":"Models"}]}"#));
    assert_eq!(seen, Some(Request::Tabs { pin: "4821".into() }));

    let page = Reply::TabPage {
        html: "<p>hi</p>".into(),
    };
    let (_, response, seen) = exchange(
        format!("GET /api/tabs/page?id=tetrad.nam HTTP/1.1\r\n{host}\r\n").as_bytes(),
        page,
    );
    assert!(response.ends_with(r#"{"html":"<p>hi</p>"}"#));
    assert_eq!(
        seen,
        Some(Request::TabPage {
            pin: "4821".into(),
            id: "tetrad.nam".into()
        })
    );

    let head =
        format!("POST /api/tabs/call?id=tetrad.nam HTTP/1.1\r\n{host}Content-Length: 11\r\n\r\n");
    let reply = Reply::TabReply {
        json: r#"{"models":["Twin"]}"#.into(),
    };
    let (status, response, seen) = exchange(&upload(&head, br#"{"op":"ls"}"#), reply);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(
        response.ends_with(r#"{"result":{"models":["Twin"]}}"#),
        "{response}"
    );
    assert_eq!(
        seen,
        Some(Request::TabCall {
            pin: "4821".into(),
            id: "tetrad.nam".into(),
            message: br#"{"op":"ls"}"#.to_vec(),
        })
    );
    // An extension that answers with text that is not JSON does not break the page.
    let bad = Reply::TabReply {
        json: "{oops".into(),
    };
    let (_, response, _) = exchange(&upload(&head, br#"{"op":"ls"}"#), bad);
    assert!(response.contains(r#""error""#), "{response}");
}

/// What the tab routes refuse before the device is asked.
#[test]
fn bad_tab_requests_are_refused_without_a_device_call() {
    let host = "Host: 10.0.0.41\r\nX-Tetrad-Pin: 4821\r\n";
    for (request, expected) in [
        (
            format!("GET /api/tabs/page HTTP/1.1\r\n{host}\r\n"),
            "HTTP/1.1 400 Bad Request",
        ),
        (
            format!("POST /api/tabs/call HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"),
            "HTTP/1.1 400 Bad Request",
        ),
        (
            format!("POST /api/tabs/call?id=a.b HTTP/1.1\r\n{host}\r\n"),
            "HTTP/1.1 411 Length Required",
        ),
        (
            "GET /api/tabs HTTP/1.1\r\nHost: 10.0.0.41\r\n\r\n".to_owned(),
            "HTTP/1.1 401 Unauthorized",
        ),
        (
            format!("POST /api/tabs HTTP/1.1\r\n{host}Content-Length: 0\r\n\r\n"),
            "HTTP/1.1 405 Method Not Allowed",
        ),
        (
            format!("GET /api/tabs/call?id=a.b HTTP/1.1\r\n{host}\r\n"),
            "HTTP/1.1 405 Method Not Allowed",
        ),
    ] {
        let (status, _, seen) = exchange(request.as_bytes(), Reply::Off);
        assert_eq!(status, expected, "{request}");
        assert_eq!(seen, None, "{request}");
    }
}

#[test]
fn bad_requests_are_refused_without_a_device_call() {
    let big = "a".repeat(8 * 1024);
    let cases: [(Vec<u8>, &str); 10] = [
        (b"GET /api/status HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n".to_vec(), "401"),
        (
            b"GET /api/status HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin:\r\n\r\n".to_vec(),
            "401",
        ),
        (
            b"POST /api/irs?name=a HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 1\r\n\r\n".to_vec(),
            "411",
        ),
        (
            format!(
                "POST /api/irs?name=a HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 1\r\nContent-Length: {}\r\n\r\n",
                MAX_BODY + 1
            )
            .into_bytes(),
            "413",
        ),
        (
            upload(
                "POST /api/irs?name=%20 HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 1\r\nContent-Length: 1\r\n\r\n",
                b"x",
            ),
            "400",
        ),
        (b"DELETE /api/irs HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n".to_vec(), "405"),
        (b"GET /secret HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n".to_vec(), "404"),
        // DNS rebinding: a name, or no Host at all.
        (b"GET / HTTP/1.1\r\nHost: qc.example:80\r\n\r\n".to_vec(), "421"),
        (b"GET / HTTP/1.0\r\n\r\n".to_vec(), "421"),
        // Over the 8 KiB limit, though one read completes the header.
        (
            format!("GET / HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Big: {big}\r\n\r\n").into_bytes(),
            "431",
        ),
    ];
    for (request, code) in cases {
        let (status, _, seen) = exchange(&request, Reply::Off);
        assert!(status.contains(code), "{status} for {request:?}");
        assert_eq!(seen, None, "{request:?}");
    }
}

/// The real thread over loopback: the call reaches `drain`, and `reply` answers the open
/// connection.
#[test]
fn server_round_trip_over_loopback() {
    let mut server = Server::start(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
    let address = server.local_addr();
    let client = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(
                b"GET /api/status HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 4821\r\n\r\n",
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut calls = Vec::new();
    while calls.is_empty() && Instant::now() < deadline {
        server.drain(&mut calls);
        std::thread::sleep(Duration::from_millis(5));
    }
    let [Call { id, request }] = calls.as_slice() else {
        panic!("one call, got {calls:?}");
    };
    assert_eq!(*request, Request::Status { pin: "4821".into() });
    server.reply(
        *id,
        Reply::Status {
            folder: "IRs".into(),
        },
    );
    let response = client.join().unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with(r#"{"folder":"IRs"}"#));
}

/// Stop closes the port even while a client holds the connection open: Remote turned off and
/// on again binds the same port.
#[test]
fn stop_frees_the_port_while_a_client_waits() {
    let server = Server::start(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
    let address = server.local_addr();
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(b"GET / HTTP/1.1\r\n").unwrap();
    // Time for the thread to take the connection and wait for the rest.
    std::thread::sleep(Duration::from_millis(300));
    let stopping = Instant::now();
    drop(server);
    // Bounded by the accept poll, not the 10 s connection deadline.
    assert!(
        stopping.elapsed() < Duration::from_secs(1),
        "{:?}",
        stopping.elapsed()
    );
    Server::start(address).expect("the port is free when drop returns");
    // The waiting client was closed, not left until its deadline.
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(client.read(&mut [0; 16]).unwrap(), 0);
}

/// A dev build takes an update in three steps; each reaches the device with the PIN, and the
/// reply of the device becomes the answer. Without the `dev` feature none of them exists.
#[cfg(feature = "dev")]
#[test]
fn the_update_routes_reach_the_device() {
    let head = |method: &str, path: &str, body: &str| {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 4821\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let pin = || "4821".to_owned();

    let reply = Reply::UpdateStatus {
        version: "0.1.0".into(),
        select: Some("5ef24effb50eb865412cc5115497a1c0".into()),
    };
    let (status, response, seen) = exchange(head("GET", "/api/update", "").as_bytes(), reply);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(
        response.ends_with(r#"{"select":"5ef24effb50eb865412cc5115497a1c0","version":"0.1.0"}"#)
    );
    assert_eq!(seen, Some(Request::UpdateStatus { pin: pin() }));

    let body = r#"{"files":{"bin/tetrad":7,"launcher/tetrad-update.sh":12}}"#;
    let begun = Reply::UpdateBegun {
        version: "1.0.0+http".into(),
        files: vec![("bin/tetrad".into(), 7)],
    };
    let (status, response, seen) =
        exchange(head("POST", "/api/update/begin", body).as_bytes(), begun);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(
        response.ends_with(r#"{"files":[{"path":"bin/tetrad","size":7}],"version":"1.0.0+http"}"#)
    );
    assert_eq!(
        seen,
        Some(Request::UpdateBegin {
            pin: pin(),
            files: vec![
                ("bin/tetrad".into(), 7),
                ("launcher/tetrad-update.sh".into(), 12)
            ],
        })
    );

    let path = "/api/update/chunk?path=launcher%2Ftetrad-update.sh&offset=4096";
    let received = Reply::UpdateReceived {
        path: "launcher/tetrad-update.sh".into(),
        received: 4100,
    };
    let (status, _, seen) = exchange(head("POST", path, "data").as_bytes(), received);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        seen,
        Some(Request::UpdateChunk {
            pin: pin(),
            path: "launcher/tetrad-update.sh".into(),
            offset: 4096,
            bytes: b"data".to_vec(),
        })
    );

    let staged = Reply::UpdateStaged {
        version: "1.0.0".into(),
    };
    let (status, _, seen) = exchange(head("POST", "/api/update/ready", "").as_bytes(), staged);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(seen, Some(Request::UpdateReady { pin: pin() }));

    let (status, response, seen) = exchange(
        head("POST", "/api/update/install", "").as_bytes(),
        Reply::UpdateInstalling,
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.ends_with(r#"{"installing":true}"#));
    assert_eq!(seen, Some(Request::UpdateInstall { pin: pin() }));

    // A refusal of the device is a 422 with its reason.
    let refused = Reply::Refused("no install-begin".into());
    let (status, response, _) = exchange(head("POST", "/api/update/ready", "").as_bytes(), refused);
    assert_eq!(status, "HTTP/1.1 422 Unprocessable Content");
    assert!(response.ends_with(r#"{"error":"no install-begin"}"#));
}

/// The update routes need the PIN, a file list as JSON and a numeric offset, and take no other method.
#[cfg(feature = "dev")]
#[test]
fn the_update_routes_check_their_input() {
    let case = |request: &str| exchange(request.as_bytes(), Reply::Off);
    let host = "Host: 192.168.1.20\r\n";
    let (status, _, seen) = case(&format!("GET /api/update HTTP/1.1\r\n{host}\r\n"));
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert_eq!(seen, None);
    let post = |path: &str, body: &str| {
        format!(
            "POST {path} HTTP/1.1\r\n{host}X-Tetrad-Pin: 1\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    for (path, body) in [
        ("/api/update/begin", "not json"),
        ("/api/update/begin", r#"{"manifest":"{}"}"#),
        ("/api/update/begin", r#"{"files":{"bin/tetrad":"big"}}"#),
        ("/api/update/begin", r#"{"files":["bin/tetrad"]}"#),
        ("/api/update/chunk?path=bin%2Ftetrad&offset=x", ""),
        ("/api/update/chunk?offset=0", ""),
    ] {
        let (status, _, seen) = case(&post(path, body));
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{path} {body}");
        assert_eq!(seen, None);
    }
    let (status, _, _) = case(&format!(
        "GET /api/update/ready HTTP/1.1\r\n{host}X-Tetrad-Pin: 1\r\n\r\n"
    ));
    assert_eq!(status, "HTTP/1.1 405 Method Not Allowed");
}

/// The page of a dev build has the update part; a release build has neither it nor the routes.
#[test]
fn only_a_dev_page_offers_the_update() {
    let (_, page, _) = exchange(b"GET / HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n", Reply::Off);
    assert_eq!(page.contains("Update Tetrad"), cfg!(feature = "dev"));
    assert!(!page.contains("<!--update-->"));
    if !cfg!(feature = "dev") {
        let request = b"GET /api/update HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 1\r\n\r\n";
        let (status, _, seen) = exchange(request, Reply::Off);
        assert_eq!(status, "HTTP/1.1 404 Not Found");
        assert_eq!(seen, None);
    }
}

/// The log of the unit is plain text, one notice per line, behind the PIN (a dev build).
#[cfg(feature = "dev")]
#[test]
fn the_log_is_plain_text_behind_the_pin() {
    let request = b"GET /api/log HTTP/1.1\r\nHost: 192.168.1.20\r\nX-Tetrad-Pin: 4821\r\n\r\n";
    let text = "    12.500 Remote: update of bin/tetrad\n    13.000 Usb: hello\n";
    let reply = Reply::Log { text: text.into() };
    let (status, response, seen) = exchange(request, reply);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.contains("Content-Type: text/plain; charset=utf-8"));
    assert!(response.ends_with(text));
    assert_eq!(seen, Some(Request::Log { pin: "4821".into() }));

    let (status, _, seen) = exchange(
        b"GET /api/log HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n",
        Reply::Off,
    );
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert_eq!(seen, None);
}

/// A key gets the PIN without one: the challenge and the signed answer need no PIN header, and
/// adding a key needs it.
#[cfg(feature = "dev")]
#[test]
fn the_key_routes_need_no_pin_except_to_add_a_key() {
    let host = "Host: 192.168.1.20\r\n";
    let (status, response, seen) = exchange(
        format!("GET /api/auth/nonce HTTP/1.1\r\n{host}\r\n").as_bytes(),
        Reply::AuthNonce {
            nonce: "ab12".into(),
        },
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.ends_with(r#"{"nonce":"ab12"}"#));
    assert_eq!(seen, Some(Request::AuthNonce));

    let body = r#"{"key":"S0VZ","nonce":"ab12","signature":"U0lH"}"#;
    let request = format!(
        "POST /api/auth/pin HTTP/1.1\r\n{host}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let (status, response, seen) =
        exchange(request.as_bytes(), Reply::AuthPin { pin: "4821".into() });
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.ends_with(r#"{"pin":"4821"}"#));
    assert_eq!(
        seen,
        Some(Request::AuthPin {
            key: "S0VZ".into(),
            nonce: "ab12".into(),
            signature: "U0lH".into(),
        })
    );

    let body = r#"{"key":"S0VZ"}"#;
    let add = |pin: &str| {
        format!(
            "POST /api/keys HTTP/1.1\r\n{host}{pin}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let (status, _, seen) = exchange(add("").as_bytes(), Reply::Off);
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert_eq!(seen, None);
    let (status, _, seen) = exchange(
        add("X-Tetrad-Pin: 4821\r\n").as_bytes(),
        Reply::KeyAdded { keys: 2 },
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        seen,
        Some(Request::AddKey {
            pin: "4821".into(),
            key: "S0VZ".into()
        })
    );
    // A request that is not JSON, or lacks a field, never reaches the device.
    for (path, body) in [
        ("/api/auth/pin", "no"),
        ("/api/auth/pin", r#"{"key":"x","nonce":"y"}"#),
    ] {
        let request = format!(
            "POST {path} HTTP/1.1\r\n{host}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (status, _, seen) = exchange(request.as_bytes(), Reply::Off);
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}");
        assert_eq!(seen, None);
    }
}

/// The routes of the IR library, backup, extension, theme and notice pages reach the device with
/// the PIN and their decoded parameters.
#[test]
fn the_page_routes_reach_the_device() {
    let host = "Host: 192.168.1.20\r\nX-Tetrad-Pin: 4821";
    let pin = || "4821".to_owned();
    let cases: Vec<(Vec<u8>, Request)> = vec![
        (
            format!("GET /api/notices HTTP/1.1\r\n{host}\r\n\r\n").into(),
            Request::Notices { pin: pin() },
        ),
        (
            format!("GET /api/library HTTP/1.1\r\n{host}\r\n\r\n").into(),
            Request::Library {
                pin: pin(),
                op: LibraryOp::List,
            },
        ),
        (
            upload(
                &format!(
                    "POST /api/library/rename?id=CIR_ab&name=V30+%C3%A9 HTTP/1.1\r\n{host}\r\n\
                     Content-Length: 0\r\n\r\n"
                ),
                b"",
            ),
            Request::Library {
                pin: pin(),
                op: LibraryOp::Rename {
                    id: "CIR_ab".into(),
                    name: "V30 é".into(),
                },
            },
        ),
        (
            upload(
                &format!(
                    "POST /api/library/link?id=CIR_ab&folder=7&linked=1 HTTP/1.1\r\n{host}\r\n\
                     Content-Length: 0\r\n\r\n"
                ),
                b"",
            ),
            Request::Library {
                pin: pin(),
                op: LibraryOp::Link {
                    id: "CIR_ab".into(),
                    folder: 7,
                    linked: true,
                },
            },
        ),
        (
            upload(
                &format!(
                    "POST /api/library/folder?name=Cabs HTTP/1.1\r\n{host}\r\n\
                     Content-Length: 0\r\n\r\n"
                ),
                b"",
            ),
            Request::Library {
                pin: pin(),
                op: LibraryOp::CreateFolder {
                    name: "Cabs".into(),
                    parent: None,
                },
            },
        ),
        (
            format!("GET /api/backup/file?offset=524288 HTTP/1.1\r\n{host}\r\n\r\n").into(),
            Request::Backup {
                pin: pin(),
                op: BackupOp::Read { offset: 524288 },
            },
        ),
        (
            upload(
                &format!(
                    "POST /api/backup/upload?name=qc.tar.gz&offset=0&last=1 HTTP/1.1\r\n\
                     {host}\r\nContent-Length: 3\r\n\r\n"
                ),
                b"abc",
            ),
            Request::Backup {
                pin: pin(),
                op: BackupOp::Upload {
                    name: "qc.tar.gz".into(),
                    offset: 0,
                    last: true,
                    bytes: b"abc".to_vec(),
                },
            },
        ),
        (
            upload(
                &format!(
                    "POST /api/extensions/command HTTP/1.1\r\n{host}\r\n\
                     Content-Length: 12\r\n\r\n"
                ),
                br#"{"op":"run"}"#,
            ),
            Request::Extensions {
                pin: pin(),
                op: ExtensionsOp::Command {
                    json: br#"{"op":"run"}"#.to_vec(),
                },
            },
        ),
        (
            format!("GET /api/footswitch HTTP/1.1\r\n{host}\r\n\r\n").into(),
            Request::Footswitch {
                pin: pin(),
                op: EditorOp::View,
            },
        ),
        (
            upload(
                &format!(
                    "POST /api/mappings/command HTTP/1.1\r\n{host}\r\n\
                     Content-Length: 2\r\n\r\n"
                ),
                b"{}",
            ),
            Request::Mappings {
                pin: pin(),
                op: EditorOp::Command {
                    json: b"{}".to_vec(),
                },
            },
        ),
        (
            upload(
                &format!(
                    "POST /api/themes/select?id=file%3Adusk HTTP/1.1\r\n{host}\r\n\
                     Content-Length: 0\r\n\r\n"
                ),
                b"",
            ),
            Request::Themes {
                pin: pin(),
                op: ThemesOp::Select {
                    id: "file:dusk".into(),
                },
            },
        ),
    ];
    for (request, expected) in cases {
        let (status, _, seen) = exchange(&request, Reply::Json(serde_json::json!({"ok": true})));
        assert_eq!(status, "HTTP/1.1 200 OK", "{expected:?}");
        assert_eq!(seen, Some(expected));
    }
}

/// A JSON reply is its own body; a piece of a download is written as raw bytes.
#[test]
fn json_and_byte_replies_are_written_as_they_are() {
    let host = "Host: 192.168.1.20\r\nX-Tetrad-Pin: 4821";
    let request = format!("GET /api/library HTTP/1.1\r\n{host}\r\n\r\n");
    let reply = Reply::Json(serde_json::json!({"irs": []}));
    let (_, response, _) = exchange(request.as_bytes(), reply);
    assert!(response.contains("Content-Type: application/json"));
    assert!(response.ends_with(r#"{"irs":[]}"#));

    let request = format!("GET /api/backup/file?offset=0 HTTP/1.1\r\n{host}\r\n\r\n");
    let (status, response, _) = exchange(request.as_bytes(), Reply::Bytes(b"piece".to_vec()));
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(response.contains("Content-Type: application/octet-stream\r\nContent-Length: 5\r\n"));
    assert!(response.ends_with("\r\n\r\npiece"));
}

/// Missing parameters, a wrong method, a missing PIN and an unknown path are answered without a
/// device call.
#[test]
fn bad_page_requests_are_refused_without_a_device_call() {
    let host = "Host: 192.168.1.20\r\nX-Tetrad-Pin: 4821";
    let post =
        |target: &str| format!("POST {target} HTTP/1.1\r\n{host}\r\nContent-Length: 0\r\n\r\n");
    let cases = [
        (post("/api/library/rename?id=CIR_ab"), "400"),
        (post("/api/library/link?id=CIR_ab&folder=x"), "400"),
        (post("/api/library/destination"), "400"),
        (post("/api/library/folder?name=Cabs&parent=up"), "400"),
        (post("/api/library/nothing"), "404"),
        (post("/api/library"), "405"),
        (
            format!("GET /api/library/delete?id=CIR_ab HTTP/1.1\r\n{host}\r\n\r\n"),
            "405",
        ),
        (
            format!("GET /api/backup/file HTTP/1.1\r\n{host}\r\n\r\n"),
            "400",
        ),
        (post("/api/backup/upload?offset=0"), "400"),
        (post("/api/backup"), "405"),
        (post("/api/themes/select"), "400"),
        (
            format!("GET /api/themes/save HTTP/1.1\r\n{host}\r\n\r\n"),
            "405",
        ),
        (
            format!("POST /api/notices HTTP/1.1\r\n{host}\r\nContent-Length: 0\r\n\r\n"),
            "405",
        ),
        (
            "GET /api/library HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n".to_owned(),
            "401",
        ),
    ];
    for (request, code) in cases {
        let (status, _, seen) = exchange(request.as_bytes(), Reply::Off);
        assert!(status.contains(code), "{request}: {status}");
        assert_eq!(seen, None, "{request}");
    }
}

/// Every page part is in the page, and no mark is left in it.
#[test]
fn the_page_has_every_part() {
    let (_, page, _) = exchange(b"GET / HTTP/1.1\r\nHost: 192.168.1.20\r\n\r\n", Reply::Off);
    for id in [
        "irs",
        "mappings",
        "footswitch",
        "backup",
        "themes",
        "notices",
        "extensions",
    ] {
        assert!(page.contains(&format!("<section id=\"{id}\"")), "{id}");
    }
    assert!(!page.contains("<!--parts-->"));
}

/// The Tetrad Link asks the same routes without HTTP (docs/bluetooth.md).
#[test]
fn api_requests_take_the_page_routes_without_http() {
    assert_eq!(
        api_request("POST", "/api/library/rename?id=7&name=V30+Hi", Vec::new()),
        Ok(Request::Library {
            pin: String::new(),
            op: LibraryOp::Rename {
                id: "7".into(),
                name: "V30 Hi".into(),
            },
        })
    );
    assert_eq!(
        api_request("POST", "/api/themes/save", b"{}".to_vec()),
        Ok(Request::Themes {
            pin: String::new(),
            op: ThemesOp::Save {
                json: b"{}".to_vec(),
            },
        })
    );
    assert_eq!(api_request("GET", "/", Vec::new()), Err((404, "not found")));
    assert_eq!(
        api_request("POST", "/api/notices", Vec::new()),
        Err((405, "method not allowed"))
    );
    // Dev-build routes stay on the page.
    for path in ["/api/log", "/api/update", "/api/auth/nonce"] {
        assert_eq!(
            api_request("GET", path, Vec::new()),
            Err((404, "not found"))
        );
    }
}
