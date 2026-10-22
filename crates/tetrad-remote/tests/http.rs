use std::io::{self, Cursor, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use tetrad_remote::{Call, MAX_BODY, Reply, Request, Server, serve_one};

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

/// Requests the server refuses itself never reach the device.
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
