//! `scripts/dev-update.sh` (docs/development.md, "Your own updates"): the manifest and signature
//! it writes are the ones the unit checks with `Manifest::verify`, with the key it prints.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use tetrad_services::updater::tetrad::Manifest;

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/dev-update.sh");
const BUILD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/build-dev.sh");

fn run(args: &[&Path]) -> Option<String> {
    let output = Command::new("sh").arg(SCRIPT).args(args).output().ok()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8(output.stdout).unwrap())
}

#[test]
fn a_packed_update_verifies_with_the_printed_key_only() {
    if Command::new("openssl").arg("version").output().is_err() {
        eprintln!("skipped: no openssl");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tetrad-dev-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let (key, other) = (dir.join("key.pem"), dir.join("other.pem"));
    let public = |out: &str| {
        let line = out.lines().find(|l| l.starts_with("public key: "));
        line.unwrap().trim_start_matches("public key: ").to_owned()
    };
    let public_key = public(&run(&[Path::new("key"), &key]).unwrap());
    let other_key = public(&run(&[Path::new("key"), &other]).unwrap());
    assert_ne!(public_key, other_key);
    // The same key again: nothing is made, the same key is printed.
    assert_eq!(public(&run(&[Path::new("key"), &key]).unwrap()), public_key);

    let build = dir.join("build");
    fs::create_dir_all(&build).unwrap();
    fs::write(build.join("tetrad"), vec![7_u8; 1234]).unwrap();
    run(&[Path::new("pack"), &build, &key]).unwrap();

    let manifest = fs::read(build.join("tetrad-update.json")).unwrap();
    let signature = fs::read(build.join("tetrad-update.json.sig")).unwrap();
    let verified = Manifest::verify(&manifest, &signature, &public_key, None).unwrap();
    assert_eq!(verified.version.to_string(), env!("CARGO_PKG_VERSION"));
    assert_eq!(verified.files["bin/tetrad"].size, 1234);
    for (path, entry) in &verified.files {
        let name = path.rsplit('/').next().unwrap();
        let bytes = fs::read(build.join(name)).unwrap();
        assert_eq!(entry.size, bytes.len() as u64, "{path}");
    }
    // Another key, or a changed manifest, does not verify.
    assert!(Manifest::verify(&manifest, &signature, &other_key, None).is_err());
    let changed = String::from_utf8(manifest).unwrap().replace("1234", "1235");
    assert!(Manifest::verify(changed.as_bytes(), &signature, &public_key, None).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

/// The names in a tar.gz, in order.
fn listing(archive: &Path) -> Vec<String> {
    let output = Command::new("tar")
        .arg("-tzf")
        .arg(archive)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// `archive` makes one file: the binary alone, or with the three launcher scripts. The files
/// are flat (the Remote page takes them by name) and the contents are the build's.
#[test]
fn an_archive_has_the_binary_and_with_scripts_the_launchers() {
    let dir = std::env::temp_dir().join(format!("tetrad-dev-archive-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("tetrad");
    fs::write(&binary, vec![5_u8; 777]).unwrap();

    let default = dir.join("tetrad-update.tar.gz");
    run(&[Path::new("archive"), &binary]).unwrap();
    assert_eq!(listing(&default), ["tetrad"]);

    let all = dir.join("all.tar.gz");
    run(&[Path::new("archive"), &binary, Path::new("--scripts"), &all]).unwrap();
    let names = [
        "tetrad",
        "tetrad-launch.sh",
        "tetrad-launcher.sh",
        "tetrad-update.sh",
    ];
    assert_eq!(listing(&all), names);
    let out = dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&all)
        .arg("-C")
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read(out.join("tetrad")).unwrap(), vec![5_u8; 777]);
    let launcher = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/launcher/tetrad-launcher.sh"
    );
    assert_eq!(
        fs::read(out.join("tetrad-launcher.sh")).unwrap(),
        fs::read(launcher).unwrap()
    );
    fs::remove_dir_all(&dir).unwrap();
}

/// `build-dev.sh` runs `build-arm.sh` with `--features dev` on its `zigbuild` call only, and
/// leaves `tetrad-update.tar.gz` beside the binary.
#[test]
fn a_dev_build_ends_with_the_update_archive() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("tetrad-dev-build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let (bin, out) = (dir.join("bin"), dir.join("out"));
    fs::create_dir_all(&bin).unwrap();
    let calls = dir.join("calls");
    let write = |path: &Path, text: &str| {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    // The cargo-zigbuild of the environment records its arguments; build-arm.sh calls it as the
    // real one does, once for the version and once to build, and writes the binary.
    write(
        &bin.join("cargo-zigbuild"),
        &format!("#!/bin/sh\necho \"$*\" >>{}\n", calls.display()),
    );
    let build_arm = dir.join("build-arm.sh");
    write(
        &build_arm,
        "#!/bin/sh\ncargo-zigbuild --version\n\
         cargo-zigbuild zigbuild --locked --release -p tetrad-app --bin tetrad\n\
         mkdir -p \"$2\" && echo elf >\"$2/tetrad\"\n",
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let output = Command::new("sh")
        .arg(BUILD)
        .arg("--scripts")
        .arg(&out)
        .env("PATH", path)
        .env("TD_BUILD_ARM", &build_arm)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fs::read_to_string(&calls).unwrap();
    let lines: Vec<_> = calls.lines().collect();
    assert_eq!(
        lines,
        [
            "--version",
            "zigbuild --locked --release -p tetrad-app --bin tetrad --features dev"
        ]
    );
    assert_eq!(
        listing(&out.join("tetrad-update.tar.gz")),
        [
            "tetrad",
            "tetrad-launch.sh",
            "tetrad-launcher.sh",
            "tetrad-update.sh"
        ]
    );
    fs::remove_dir_all(&dir).unwrap();
}

/// Every POST of the scripts has a `Content-Length`, an empty one included (`ready`, `install`):
/// the Remote server answers 411 to a POST without it, as a unit did to the first `ready`.
#[test]
fn an_empty_post_has_a_content_length() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = vec![0; 2048];
        let read = stream.read(&mut head).unwrap();
        let head = String::from_utf8_lossy(&head[..read]).to_lowercase();
        let (status, body) = if head.starts_with("post /api/update/install ")
            && head.contains("content-length: 0")
            && head.contains("x-tetrad-pin: 1234")
        {
            ("200 OK", r#"{"installing":true}"#)
        } else {
            (
                "411 Length Required",
                r#"{"error":"Content-Length required"}"#,
            )
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    let output = Command::new("sh")
        .args([SCRIPT, "install", &address.to_string(), "1234"])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Reads one request: the head (lowercased) and the body, by its Content-Length.
#[cfg(feature = "dev")]
fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
    use std::io::Read;
    let mut data = Vec::new();
    let mut byte = [0; 1];
    while !data.ends_with(b"\r\n\r\n") {
        assert_eq!(stream.read(&mut byte).unwrap(), 1);
        data.push(byte[0]);
    }
    let head = String::from_utf8(data).unwrap().to_lowercase();
    let length = (head.lines())
        .find_map(|line| {
            line.strip_prefix("content-length: ")
                .map(|n| n.parse().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    (head, String::from_utf8(body).unwrap())
}

#[cfg(feature = "dev")]
fn respond(stream: &mut std::net::TcpStream, status: &str, body: &str) {
    use std::io::Write;
    let text = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(text.as_bytes()).unwrap();
}

/// `authorize` sends the public key with the PIN; `key:KEY.pem` in place of the PIN gets a
/// challenge, signs it with `openssl`, and the unit's own check accepts the signature over the
/// bytes the unit builds (`AUTH_DOMAIN` and the challenge), then the call goes with that PIN.
#[cfg(feature = "dev")]
#[test]
fn a_key_authorizes_and_gets_the_pin_from_the_unit() {
    use std::net::TcpListener;
    if Command::new("openssl").arg("version").output().is_err() {
        eprintln!("skipped: no openssl");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tetrad-dev-keyauth-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let key = dir.join("key.pem");
    let out = run(&[Path::new("key"), &key]).unwrap();
    let public = out
        .lines()
        .next()
        .unwrap()
        .trim_start_matches("public key: ")
        .to_owned();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let expected = public.clone();
    let server = std::thread::spawn(move || {
        const NONCE: &str = "00112233445566778899aabbccddeeff";
        // authorize: POST /api/keys with the PIN and the public key.
        let (mut stream, _) = listener.accept().unwrap();
        let (head, body) = read_request(&mut stream);
        assert!(head.starts_with("post /api/keys "), "{head}");
        assert!(head.contains("x-tetrad-pin: 4321"), "{head}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["key"], expected.as_str());
        respond(&mut stream, "200 OK", r#"{"keys":1}"#);
        // install with key:...: a challenge, the signed answer, then the call with the PIN.
        let (mut stream, _) = listener.accept().unwrap();
        let (head, _) = read_request(&mut stream);
        assert!(head.starts_with("get /api/auth/nonce "), "{head}");
        respond(&mut stream, "200 OK", &format!(r#"{{"nonce":"{NONCE}"}}"#));
        let (mut stream, _) = listener.accept().unwrap();
        let (head, body) = read_request(&mut stream);
        assert!(head.starts_with("post /api/auth/pin "), "{head}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["key"], expected.as_str());
        assert_eq!(value["nonce"], NONCE);
        let message = format!("{}{NONCE}", tetrad_remote::AUTH_DOMAIN);
        let verified = tetrad_services::updater::tetrad::verify_signature(
            value["key"].as_str().unwrap(),
            message.as_bytes(),
            value["signature"].as_str().unwrap().as_bytes(),
        );
        assert!(verified.is_ok(), "{verified:?}");
        respond(&mut stream, "200 OK", r#"{"pin":"4321"}"#);
        let (mut stream, _) = listener.accept().unwrap();
        let (head, _) = read_request(&mut stream);
        assert!(head.starts_with("post /api/update/install "), "{head}");
        assert!(head.contains("x-tetrad-pin: 4321"), "{head}");
        respond(&mut stream, "200 OK", r#"{"installing":true}"#);
    });
    let host = Path::new(&address);
    let pin = Path::new("4321");
    run(&[Path::new("authorize"), host, pin, &key]).unwrap();
    let by_key = format!("key:{}", key.display());
    run(&[Path::new("install"), host, Path::new(&by_key)]).unwrap();
    server.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

/// A request that the network drops (the connection ends with no answer) ends that attempt of a
/// push; the next one starts over from `begin` and cuts the file in pieces of another size, so
/// a drop that depends on where a piece starts does not repeat. A refusal of the unit is not
/// retried.
#[cfg(feature = "dev")]
#[test]
fn a_push_starts_over_with_other_pieces_when_one_is_dropped() {
    use std::net::TcpListener;
    let dir = std::env::temp_dir().join(format!("tetrad-dev-retry-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("tetrad");
    fs::write(&binary, vec![9_u8; 100_000]).unwrap();
    let archive = dir.join("u.tar.gz");
    run(&[Path::new("archive"), &binary, &archive]).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut dropped = false;
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            let (head, _) = read_request(&mut stream);
            let line = head.lines().next().unwrap().to_owned();
            seen.push(line.clone());
            if line.contains("offset=49152") && !dropped {
                dropped = true;
                continue; // the connection ends with no answer
            }
            let body = if line.contains("/api/update ") {
                r#"{"version":"0.1.0","select":null}"#
            } else {
                r#"{"ok":true}"#
            };
            respond(&mut stream, "200 OK", body);
            if line.contains("/api/update/ready") {
                return seen;
            }
        }
    });
    let output = Command::new("sh")
        .args([SCRIPT, "push"])
        .arg(&archive)
        .args([&address, "1234"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen = server.join().unwrap();
    let steps: Vec<&str> = seen
        .iter()
        .map(|line| line.split(' ').nth(1).unwrap())
        .collect();
    let begins = steps.iter().filter(|step| step.ends_with("/begin")).count();
    assert_eq!(begins, 2, "{steps:?}");
    let offsets: Vec<&str> = steps
        .iter()
        .filter_map(|s| s.split("offset=").nth(1))
        .collect();
    // First attempt: 0, then the dropped 49152. Second attempt: pieces of 40000.
    assert_eq!(offsets, ["0", "49152", "0", "40000", "80000"], "{steps:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("dropped"));
    fs::remove_dir_all(&dir).unwrap();
}
