//! Remote (docs/remote.md): the Settings switch starts the server with a new PIN and is saved
//! in `remote.json`; page requests need the PIN; an upload lands in the IR saving destination
//! through the stock IR import. The server is a fake: tests push its calls.

mod common;

use common::{FakeRemote, Media, STOCK, Sim, boot_remote, light_preset, target};
use tetrad_app::device::platform::TouchSample;
use tetrad_app::device::remote::{MAX_WRONG_PINS, SETTINGS_FILE};
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Device, Event};
use tetrad_remote::{Call, Reply, Request};
use tetrad_services::fake::MemFiles;
use tetrad_storage::stock::ProductType;
use tetrad_ui::{Control, RemoteView, Screen, SettingsPage};

fn media() -> Media {
    Media::new([("Light", light_preset(&STOCK.specs(), 0))])
}

/// The WAV the page writes: an 18-byte IEEE-float `fmt` chunk and the data chunk at byte 38.
fn page_wav(samples: usize) -> Vec<u8> {
    let mut wav = b"RIFF".to_vec();
    wav.extend((38 + samples as u32 * 4).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(18_u32.to_le_bytes());
    wav.extend([3, 0, 1, 0]);
    wav.extend(48_000_u32.to_le_bytes());
    wav.extend(192_000_u32.to_le_bytes());
    wav.extend([4, 0, 32, 0, 0, 0]);
    wav.extend(b"data");
    wav.extend((samples as u32 * 4).to_le_bytes());
    wav.extend((0..samples).flat_map(|i| (i as f32 / samples as f32).to_le_bytes()));
    wav
}

fn remote(device: &Device<Sim>) -> &FakeRemote {
    device.platform().remote.as_ref().unwrap()
}

fn view(device: &Device<Sim>) -> RemoteView {
    device.bridges().ui.view().snapshot().remote.clone()
}

/// Taps the Remote switch of Settings → Tetrad → Remote.
fn tap_switch(device: &mut Device<Sim>) {
    tap(device, Control::RemoteToggle);
}

/// Taps one switch of Settings → Tetrad → Remote.
fn tap(device: &mut Device<Sim>, switch: Control) {
    let page = Screen::Settings(SettingsPage::Remote);
    device.push(Event::UiInput(UiInput::Show(page)));
    device.step();
    let tap = target(&device.bridges().ui, switch);
    for pressed in [true, false] {
        device.push(Event::UiInput(UiInput::Touch(TouchSample {
            pressed,
            ..tap
        })));
        device.step();
    }
}

/// Sends one page request and returns the device's reply.
fn ask(device: &mut Device<Sim>, request: Request) -> Reply {
    let id = remote(device).replies.len() as u64 + 1;
    device.push(Event::Remote(Call { id, request }));
    device.step();
    let replies = &remote(device).replies;
    let (replied, reply) = replies.last().expect("a reply").clone();
    assert_eq!(replied, id);
    reply
}

fn status(pin: &str) -> Request {
    Request::Status { pin: pin.into() }
}

#[test]
fn an_upload_needs_the_pin_and_lands_in_the_ir_saving_destination() {
    let files = MemFiles::default();
    let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
    assert_eq!(ask(&mut device, status("")), Reply::Off);

    tap_switch(&mut device);
    assert!(remote(&device).running);
    let shown = view(&device);
    assert!(shown.enabled);
    assert_eq!(shown.pin.as_deref(), Some("0001"));
    assert_eq!(
        files.get(SETTINGS_FILE).unwrap(),
        br#"{"enabled":true,"pin_in_menu":false}"#
    );

    assert_eq!(ask(&mut device, status("9999")), Reply::WrongPin);
    let Reply::Status { folder } = ask(&mut device, status("0001")) else {
        panic!("the status of the right PIN");
    };
    let upload = |pin: &str| Request::Upload {
        pin: pin.into(),
        name: "  V30   SM57 ".into(),
        wav: page_wav(1024),
    };
    assert_eq!(ask(&mut device, upload("1234")), Reply::WrongPin);
    let added = Reply::Added {
        name: "V30 SM57".into(),
        folder,
    };
    assert_eq!(ask(&mut device, upload("0001")), added);

    let library = device.state().library.as_ref().unwrap();
    let view_db = library.database.as_ref().unwrap().view();
    let ids = view_db.product_ids(ProductType::Ir).unwrap();
    let [id] = ids.as_slice() else {
        panic!("one IR: {ids:?}");
    };
    assert!(id.starts_with("CIR_"));
    let row = view_db.product(id).unwrap().unwrap();
    assert_eq!(row.raw_data.unwrap().len(), 1024 * 4);
    let snapshot = device.bridges().ui.view().snapshot();
    let toast = snapshot.toast.as_ref().map(|toast| toast.text.as_str());
    assert_eq!(toast, Some("IR added: V30 SM57"));
    // The upload came right after a touch: no notice dialog besides the toast.
    assert_eq!(snapshot.notice, None);
    assert_eq!(view(&device).status.as_deref(), Some("Added V30 SM57"));
}

/// The file manager needs the PIN, lists nothing without an extension that takes files, and refuses
/// a file for an extension that is not there.
#[test]
fn the_file_manager_needs_the_pin_and_an_extension_that_takes_files() {
    let files = MemFiles::default();
    let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
    tap_switch(&mut device);
    let list = |pin: &str| Request::Files { pin: pin.into() };
    let chunk = |pin: &str| Request::FileChunk {
        pin: pin.into(),
        id: "tetrad.nam".into(),
        name: "A.nam".into(),
        offset: 0,
        last: true,
        bytes: b"{}".to_vec(),
    };
    assert_eq!(ask(&mut device, list("9999")), Reply::WrongPin);
    assert_eq!(ask(&mut device, chunk("9999")), Reply::WrongPin);
    assert_eq!(
        ask(&mut device, list("0001")),
        Reply::Files {
            extensions: Vec::new()
        }
    );
    let Reply::Refused(reason) = ask(&mut device, chunk("0001")) else {
        panic!("a file for an extension that is not installed");
    };
    assert!(reason.contains("takes no files"), "{reason}");
    assert!(
        files
            .get("/media/p4/tetrad/assets/tetrad.nam/A.nam")
            .is_none()
    );
    let delete = Request::FileDelete {
        pin: "0001".into(),
        id: "tetrad.nam".into(),
        name: "A.nam".into(),
    };
    assert!(matches!(ask(&mut device, delete), Reply::Refused(_)));
}

/// After [`MAX_WRONG_PINS`] wrong PINs the unit makes a new PIN: the old one stops working.
#[test]
fn too_many_wrong_pins_make_a_new_pin() {
    let (mut device, _media) = boot_remote(media(), MemFiles::default(), FakeRemote::default());
    tap_switch(&mut device);
    for _ in 0..MAX_WRONG_PINS {
        assert_eq!(ask(&mut device, status("9999")), Reply::WrongPin);
    }
    assert_eq!(view(&device).pin.as_deref(), Some("0002"));
    assert_eq!(ask(&mut device, status("0001")), Reply::WrongPin);
    assert!(matches!(
        ask(&mut device, status("0002")),
        Reply::Status { .. }
    ));
}

/// The switch is saved: a boot with Remote on starts it again with a new PIN; off stops the
/// server and forgets the PIN.
#[test]
fn the_switch_is_saved_across_boots() {
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
    assert!(remote(&device).running);
    // The views follow at the next pass.
    device.step();
    assert_eq!(view(&device).pin.as_deref(), Some("0001"));

    tap_switch(&mut device);
    assert!(!remote(&device).running);
    assert_eq!(view(&device), RemoteView::default());
    assert_eq!(
        files.get(SETTINGS_FILE).unwrap(),
        br#"{"enabled":false,"pin_in_menu":false}"#
    );
    assert_eq!(ask(&mut device, status("0001")), Reply::Off);
}

/// A port that does not open leaves the switch on and saved, with the reason; the switch still
/// turns off and saves off.
#[test]
fn a_failed_start_keeps_the_switch_and_turns_off() {
    let files = MemFiles::default();
    let remote = FakeRemote {
        fail: Some("Port 80 is in use".into()),
        ..FakeRemote::default()
    };
    let (mut device, _media) = boot_remote(media(), files.clone(), remote);
    tap_switch(&mut device);
    let shown = view(&device);
    assert!(shown.enabled && shown.pin.is_none());
    assert_eq!(shown.status.as_deref(), Some("Port 80 is in use"));
    assert_eq!(
        files.get(SETTINGS_FILE).unwrap(),
        br#"{"enabled":true,"pin_in_menu":false}"#
    );
    assert_eq!(ask(&mut device, status("0001")), Reply::Off);

    tap_switch(&mut device);
    assert!(!view(&device).enabled);
    assert_eq!(
        files.get(SETTINGS_FILE).unwrap(),
        br#"{"enabled":false,"pin_in_menu":false}"#
    );
}

/// "Show PIN in menu" is saved with the Remote switch and leaves the server as it is; a boot
/// restores both.
#[test]
fn the_pin_in_the_menu_is_a_saved_switch() {
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
    device.step();
    assert!(
        !view(&device).pin_in_menu,
        "off in a file from before the switch"
    );

    tap(&mut device, Control::RemotePinInMenuToggle);
    let shown = view(&device);
    assert!(shown.pin_in_menu);
    assert_eq!(
        shown.pin.as_deref(),
        Some("0001"),
        "the server keeps its PIN"
    );
    let saved = br#"{"enabled":true,"pin_in_menu":true}"#;
    assert_eq!(files.get(SETTINGS_FILE).unwrap(), saved);

    let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
    device.step();
    assert!(view(&device).pin_in_menu);
}

/// Update through the Remote page (a dev build, `docs/remote.md`): unsigned files of the update
/// set, staged in chunks, checked, and offered on the unit.
#[cfg(feature = "dev")]
mod update {
    use super::*;

    const HOME: &str = "/media/p4/tetrad";

    /// A 32-bit little-endian ARM ELF header and `len` bytes in all.
    fn elf(len: usize) -> Vec<u8> {
        let mut bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 1;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(&[40, 0]);
        bytes
    }

    fn script() -> Vec<u8> {
        b"#!/bin/sh\necho hello\n".to_vec()
    }

    fn pin(device: &mut Device<Sim>) -> String {
        tap_switch(device);
        view(device).pin.expect("a PIN")
    }

    fn begin(pin: &str, files: &[(&str, usize)]) -> Request {
        Request::UpdateBegin {
            pin: pin.into(),
            files: files
                .iter()
                .map(|(p, n)| ((*p).into(), *n as u64))
                .collect(),
        }
    }

    /// Sends `bytes` of `path` in pieces of 128 bytes; every chunk is taken at its offset.
    fn send(device: &mut Device<Sim>, pin: &str, path: &str, bytes: &[u8]) {
        let mut offset = 0;
        for piece in bytes.chunks(128) {
            let request = Request::UpdateChunk {
                pin: pin.into(),
                path: path.into(),
                offset: offset as u64,
                bytes: piece.to_vec(),
            };
            offset += piece.len();
            let received = offset as u64;
            let reply = ask(device, request);
            assert_eq!(
                reply,
                Reply::UpdateReceived {
                    path: path.into(),
                    received
                }
            );
        }
    }

    /// A key that the PIN added gets the PIN by signing a challenge of the unit: once per
    /// challenge, only for a known key, only with a matching signature, and not when Remote is
    /// off.
    #[test]
    fn a_known_key_gets_the_pin_by_signing_a_challenge() {
        use base64::Engine;
        use ed25519_dalek::{Signer, SigningKey};
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let sign = |key: &SigningKey, nonce: &str| {
            let message = format!("{}{nonce}", tetrad_remote::AUTH_DOMAIN);
            b64(&key.sign(message.as_bytes()).to_bytes())
        };
        let files = MemFiles::default();
        let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
        // Off: no challenge.
        assert_eq!(ask(&mut device, Request::AuthNonce), Reply::Off);
        let pin = pin(&mut device);
        let (mine, other) = (
            SigningKey::from_bytes(&[3; 32]),
            SigningKey::from_bytes(&[4; 32]),
        );
        let mine_pub = b64(&mine.verifying_key().to_bytes());
        let other_pub = b64(&other.verifying_key().to_bytes());
        let auth = |key: &str, nonce: &str, signature: String| Request::AuthPin {
            key: key.into(),
            nonce: nonce.into(),
            signature,
        };
        let nonce = |device: &mut Device<Sim>| match ask(device, Request::AuthNonce) {
            Reply::AuthNonce { nonce } => nonce,
            other => panic!("a challenge: {other:?}"),
        };

        // Adding a key needs the PIN and a public key; adding it twice keeps one.
        let add = |pin: &str, key: &str| Request::AddKey {
            pin: pin.into(),
            key: key.into(),
        };
        assert_eq!(ask(&mut device, add("9999", &mine_pub)), Reply::WrongPin);
        let bad = ask(&mut device, add(&pin, "not a key"));
        assert!(refused(&bad).contains("public key"), "{bad:?}");
        assert_eq!(
            ask(&mut device, add(&pin, &mine_pub)),
            Reply::KeyAdded { keys: 1 }
        );
        assert_eq!(
            ask(&mut device, add(&pin, &mine_pub)),
            Reply::KeyAdded { keys: 1 }
        );
        let saved = files.get("/media/p4/tetrad/remote-keys").unwrap();
        assert_eq!(saved, format!("{mine_pub}\n").into_bytes());

        // The right key and signature: the PIN, and the challenge is used up.
        let n = nonce(&mut device);
        let good = auth(&mine_pub, &n, sign(&mine, &n));
        assert_eq!(
            ask(&mut device, good.clone()),
            Reply::AuthPin { pin: pin.clone() }
        );
        assert!(refused(&ask(&mut device, good)).contains("challenge"));
        // A key that is not known, a signature of another key or of another challenge.
        let n = nonce(&mut device);
        let unknown = ask(&mut device, auth(&other_pub, &n, sign(&other, &n)));
        assert!(refused(&unknown).contains("not known"), "{unknown:?}");
        let n = nonce(&mut device);
        let wrong = ask(&mut device, auth(&mine_pub, &n, sign(&other, &n)));
        assert!(refused(&wrong).contains("signature"), "{wrong:?}");
        let (n, m) = (nonce(&mut device), nonce(&mut device));
        let swapped = ask(&mut device, auth(&mine_pub, &n, sign(&mine, &m)));
        assert!(refused(&swapped).contains("signature"), "{swapped:?}");
        // The signature is of the challenge with the domain in front, not of the bare challenge.
        let n = nonce(&mut device);
        let bare = b64(&mine.sign(n.as_bytes()).to_bytes());
        let reply = ask(&mut device, auth(&mine_pub, &n, bare.clone()));
        assert!(refused(&reply).contains("signature"), "{reply:?}");
        // No more than four challenges wait: the oldest is gone.
        let first = nonce(&mut device);
        for _ in 0..4 {
            nonce(&mut device);
        }
        let gone = ask(&mut device, auth(&mine_pub, &first, sign(&mine, &first)));
        assert!(refused(&gone).contains("challenge"), "{gone:?}");
        // The log names the key, never the signature or the PIN.
        let Reply::Log { text } = ask(&mut device, Request::Log { pin: pin.clone() }) else {
            panic!("the log");
        };
        assert!(text.contains(&format!("key {}", &mine_pub[..8])), "{text}");
        assert!(!text.contains(&bare), "{text}");
        assert!(
            text.contains("got the PIN") && !text.contains("got the PIN:"),
            "{text}"
        );
    }

    /// The install call is behind the PIN, needs staged files like the button does, and starts
    /// the install of what `ready` offered.
    #[test]
    fn the_install_call_starts_the_staged_install() {
        let (mut device, _media) = boot_remote(media(), MemFiles::default(), FakeRemote::default());
        let pin = pin(&mut device);
        let install = |pin: &str| Request::UpdateInstall { pin: pin.into() };
        assert_eq!(ask(&mut device, install("9999")), Reply::WrongPin);
        // Nothing staged: refused, as the button is.
        let reply = ask(&mut device, install(&pin));
        assert!(refused(&reply).contains("no Tetrad update"), "{reply:?}");

        let binary = elf(200);
        ask(&mut device, begin(&pin, &[("bin/tetrad", 200)]));
        send(&mut device, &pin, "bin/tetrad", &binary);
        let ready = ask(&mut device, Request::UpdateReady { pin: pin.clone() });
        assert!(matches!(ready, Reply::UpdateStaged { .. }), "{ready:?}");
        assert_eq!(ask(&mut device, install(&pin)), Reply::UpdateInstalling);
        let Reply::Log { text } = ask(&mut device, Request::Log { pin }) else {
            panic!("the log");
        };
        assert!(text.contains("Remote: install started"), "{text}");
    }

    /// The log is behind the PIN and lists the notices, among them what the update did.
    #[test]
    fn the_log_lists_the_notices() {
        let (mut device, _media) = boot_remote(media(), MemFiles::default(), FakeRemote::default());
        let pin = pin(&mut device);
        assert_eq!(
            ask(&mut device, Request::Log { pin: "9999".into() }),
            Reply::WrongPin
        );
        ask(&mut device, begin(&pin, &[("bin/tetrad", 200)]));
        let Reply::Log { text } = ask(&mut device, Request::Log { pin }) else {
            panic!("the log");
        };
        let line = text
            .lines()
            .find(|line| line.contains("Remote: update of bin/tetrad"));
        assert!(line.is_some(), "{text}");
        assert!(text.ends_with('\n'));
    }

    fn refused(reply: &Reply) -> &str {
        match reply {
            Reply::Refused(reason) => reason,
            other => panic!("not refused: {other:?}"),
        }
    }

    /// The update steps need the PIN, and only the four update files are taken, each within its
    /// limit, once.
    #[test]
    fn an_update_needs_the_pin_and_takes_only_the_update_files() {
        let files = MemFiles::default();
        let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
        let pin = pin(&mut device);
        let wrong = Request::UpdateStatus { pin: "9999".into() };
        assert_eq!(ask(&mut device, wrong), Reply::WrongPin);
        let version = env!("CARGO_PKG_VERSION").to_owned();
        let status = ask(&mut device, Request::UpdateStatus { pin: pin.clone() });
        // No `launcher/select` on this fake unit.
        assert_eq!(
            status,
            Reply::UpdateStatus {
                version: version.clone(),
                select: None
            }
        );
        files.put(
            "/media/p4/tetrad/launcher/select",
            "5ef24effb50eb865412cc5115497a1c0\n",
        );
        let status = ask(&mut device, Request::UpdateStatus { pin: pin.clone() });
        let select = Some("5ef24effb50eb865412cc5115497a1c0".to_owned());
        assert_eq!(status, Reply::UpdateStatus { version, select });

        for (case, files) in [
            ("no files", vec![]),
            ("not an update file", vec![("launcher/select", 10)]),
            ("not an update file", vec![("../../etc/passwd", 10)]),
            ("not an update file", vec![("bin/tetrad.part", 10)]),
            ("size 0", vec![("bin/tetrad", 0)]),
            ("size 70000000", vec![("bin/tetrad", 70_000_000)]),
            ("size 300000", vec![("launcher/tetrad-launch.sh", 300_000)]),
            ("twice", vec![("bin/tetrad", 5), ("bin/tetrad", 5)]),
        ] {
            let reply = ask(&mut device, begin(&pin, &files));
            assert!(
                refused(&reply).contains(case.split(' ').next().unwrap()),
                "{case}: {reply:?}"
            );
        }
        // Nothing was started: a chunk and a ready have no staging.
        let chunk = Request::UpdateChunk {
            pin: pin.clone(),
            path: "bin/tetrad".into(),
            offset: 0,
            bytes: vec![1],
        };
        assert_eq!(refused(&ask(&mut device, chunk)), "no install-begin");
        let ready = ask(&mut device, Request::UpdateReady { pin });
        assert_eq!(refused(&ready), "no install-begin");
        assert_eq!(files.get(&format!("{HOME}/bin/tetrad.part")), None);
    }

    /// The binary alone: chunks at the right offsets, the `.part` file holds the bytes, and
    /// the update is offered as the running version with `+http`.
    #[test]
    fn a_binary_is_staged_in_chunks() {
        let files = MemFiles::default();
        let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
        let pin = pin(&mut device);
        let binary = elf(300);
        let begun = ask(&mut device, begin(&pin, &[("bin/tetrad", 300)]));
        let version = format!("{}+http", env!("CARGO_PKG_VERSION"));
        let expected = Reply::UpdateBegun {
            version: version.clone(),
            files: vec![("bin/tetrad".into(), 300)],
        };
        assert_eq!(begun, expected);
        // An offset that is not the bytes already here, and a file that was not announced.
        let chunk = |path: &str, offset: u64| Request::UpdateChunk {
            pin: pin.clone(),
            path: path.into(),
            offset,
            bytes: vec![1],
        };
        assert!(refused(&ask(&mut device, chunk("bin/tetrad", 5))).contains("offset"));
        assert!(
            refused(&ask(&mut device, chunk("launcher/tetrad-update.sh", 0))).contains("manifest")
        );
        send(&mut device, &pin, "bin/tetrad", &binary);
        assert_eq!(
            files.get(&format!("{HOME}/bin/tetrad.part")).unwrap(),
            binary
        );
        let staged = ask(&mut device, Request::UpdateReady { pin: pin.clone() });
        assert_eq!(staged, Reply::UpdateStaged { version });
        // The staging is over.
        let after = ask(&mut device, chunk("bin/tetrad", 300));
        assert_eq!(refused(&after), "no install-begin");
    }

    /// A binary with the scripts, and a script alone, stage the same way.
    #[test]
    fn scripts_stage_with_or_without_the_binary() {
        let files = MemFiles::default();
        let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
        let pin = pin(&mut device);
        let sh = script();
        let all = [
            ("bin/tetrad", elf(200)),
            ("launcher/tetrad-launch.sh", sh.clone()),
            ("launcher/tetrad-launcher.sh", sh.clone()),
            ("launcher/tetrad-update.sh", sh.clone()),
        ];
        let sizes: Vec<_> = all.iter().map(|(p, b)| (*p, b.len())).collect();
        assert!(matches!(
            ask(&mut device, begin(&pin, &sizes)),
            Reply::UpdateBegun { .. }
        ));
        for (path, bytes) in &all {
            send(&mut device, &pin, path, bytes);
        }
        let ready = ask(&mut device, Request::UpdateReady { pin: pin.clone() });
        assert!(matches!(ready, Reply::UpdateStaged { .. }), "{ready:?}");
        for (path, bytes) in &all {
            assert_eq!(&files.get(&format!("{HOME}/{path}.part")).unwrap(), bytes);
        }

        let only = [("launcher/tetrad-launcher.sh", sh.len())];
        assert!(matches!(
            ask(&mut device, begin(&pin, &only)),
            Reply::UpdateBegun { .. }
        ));
        send(&mut device, &pin, "launcher/tetrad-launcher.sh", &sh);
        let ready = ask(&mut device, Request::UpdateReady { pin });
        assert!(matches!(ready, Reply::UpdateStaged { .. }), "{ready:?}");
    }

    /// A file that is not an ARM executable never reaches the install, and an incomplete file
    /// ends the staging.
    #[test]
    fn a_wrong_binary_or_an_incomplete_file_is_refused() {
        let files = MemFiles::default();
        let (mut device, _media) = boot_remote(media(), files.clone(), FakeRemote::default());
        let pin = pin(&mut device);
        let mut x86 = elf(200);
        x86[18] = 62;
        assert!(matches!(
            ask(&mut device, begin(&pin, &[("bin/tetrad", 200)])),
            Reply::UpdateBegun { .. }
        ));
        send(&mut device, &pin, "bin/tetrad", &x86);
        let ready = ask(&mut device, Request::UpdateReady { pin: pin.clone() });
        assert!(refused(&ready).contains("ARM"), "{ready:?}");
        assert_eq!(files.get(&format!("{HOME}/bin/tetrad.part")), None);

        assert!(matches!(
            ask(&mut device, begin(&pin, &[("bin/tetrad", 200)])),
            Reply::UpdateBegun { .. }
        ));
        send(&mut device, &pin, "bin/tetrad", &elf(200)[..100]);
        let ready = ask(&mut device, Request::UpdateReady { pin });
        assert!(refused(&ready).contains("100"), "{ready:?}");
    }
}
