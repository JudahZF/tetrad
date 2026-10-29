//! Bluetooth (docs/bluetooth.md): the Tetrad app's PIN permission. A bond keeps it only while its
//! saved key encrypts the link; a new pairing must give the PIN again. The BLE host is a fake:
//! tests push its events and read the Link replies it was asked to send.

mod common;

use common::{Media, STOCK, Sim, boot_bluetooth, light_preset};
use serde_json::{Value, json};
use tetrad_app::device::bluetooth::{BONDS_FILE, SETTINGS_FILE};
use tetrad_app::device::platform::Input;
use tetrad_app::device::services::{BackupDone, REMOTE_ARCHIVE, ServiceNotice};
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Device, Event};
use tetrad_ble::bond::{Bond, Kind};
use tetrad_ble::{Command, Event as Ble};
use tetrad_services::backup::Summary;
use tetrad_services::fake::MemFiles;
use tetrad_ui::{Control, InputEvent, Screen, SettingsPage};

const PHONE: [u8; 6] = [0x2b, 0xa0, 0xfa, 0x1b, 0x36, 0xac];

fn bond(ltk: u8, app: bool) -> Bond {
    Bond {
        address: PHONE,
        address_type: 0,
        kind: Kind::Host,
        irk: None,
        ltk: [ltk; 16],
        ediv: 0,
        rand: [0; 8],
        secure: true,
        name: Some("Phone".into()),
        notify: 0,
        digest: 0,
        app,
    }
}

/// A unit with Bluetooth on and one phone that gave the PIN on an earlier link.
fn boot() -> (Device<Sim>, MemFiles) {
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let bonds = serde_json::to_string(&vec![bond(1, true)]).unwrap();
    files.put(BONDS_FILE, bonds);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    let (device, _media) = boot_bluetooth(media, files.clone());
    (device, files)
}

fn push(device: &mut Device<Sim>, event: Ble) {
    device.push(Event::Bluetooth(event));
    device.step();
}

/// Sends one app request and returns the header of the reply.
fn ask(device: &mut Device<Sim>, header: Value) -> Value {
    exchange(device, header).0
}

/// Sends one app request and returns the header and the body of the reply.
fn exchange(device: &mut Device<Sim>, header: Value) -> (Value, Vec<u8>) {
    push(device, Ble::LinkMessage(tetrad_usb::encode(&header, &[])));
    let sent = &device.platform().bluetooth.as_ref().unwrap().sent;
    let Some(Command::LinkSend { message, .. }) = sent
        .iter()
        .rev()
        .find(|c| matches!(c, Command::LinkSend { .. }))
    else {
        panic!("no reply in {sent:?}");
    };
    let (header, body) = tetrad_usb::decode::<Value>(message).unwrap();
    (header, body.to_vec())
}

fn pin(device: &Device<Sim>) -> String {
    let view = &device.bridges().ui.view().snapshot().bluetooth;
    view.pin.clone().expect("a PIN while Bluetooth runs")
}

#[test]
fn a_remembered_pin_counts_only_after_the_saved_key_encrypts_the_link() {
    let (mut device, _files) = boot();
    assert!(device.platform().bluetooth.as_ref().unwrap().running);
    push(
        &mut device,
        Ble::HostConnected {
            address: PHONE,
            bonded: true,
        },
    );
    // The address alone proves nothing, and before encryption no reply goes out.
    let files = json!({"id": 1, "op": "files"});
    push(
        &mut device,
        Ble::LinkMessage(tetrad_usb::encode(&files, &[])),
    );
    let sent = &device.platform().bluetooth.as_ref().unwrap().sent;
    assert!(!sent.iter().any(|c| matches!(c, Command::LinkSend { .. })));
    push(
        &mut device,
        Ble::HostSecured {
            address: PHONE,
            paired: false,
            connection: 0,
        },
    );
    assert!(ask(&mut device, files)["extensions"].is_array());
}

#[test]
fn a_new_pairing_with_a_known_address_must_give_the_pin_again() {
    let (mut device, files) = boot();
    push(
        &mut device,
        Ble::HostConnected {
            address: PHONE,
            bonded: true,
        },
    );
    // Pairing again (the page is open) makes new keys for the same address.
    push(&mut device, Ble::Bonded(bond(2, false)));
    push(
        &mut device,
        Ble::HostSecured {
            address: PHONE,
            paired: true,
            connection: 0,
        },
    );
    let request = json!({"id": 1, "op": "files"});
    assert_eq!(
        ask(&mut device, request.clone())["error"],
        "Give the PIN first (auth)"
    );
    let saved: Vec<Bond> = serde_json::from_slice(&files.get(BONDS_FILE).unwrap()).unwrap();
    assert!(!saved[0].app);

    let pin = pin(&device);
    let auth = ask(&mut device, json!({"id": 2, "op": "auth", "pin": pin}));
    assert_eq!(auth["authorized"], true);
    assert!(ask(&mut device, request)["extensions"].is_array());
    let saved: Vec<Bond> = serde_json::from_slice(&files.get(BONDS_FILE).unwrap()).unwrap();
    assert!(saved[0].app);
}

#[test]
fn pairing_again_after_a_remembered_pin_must_give_the_pin_again() {
    let (mut device, files) = boot();
    push(
        &mut device,
        Ble::HostConnected {
            address: PHONE,
            bonded: true,
        },
    );
    push(
        &mut device,
        Ble::HostSecured {
            address: PHONE,
            paired: false,
            connection: 0,
        },
    );
    // The same connection pairs again: new keys.
    push(&mut device, Ble::Bonded(bond(3, false)));
    push(
        &mut device,
        Ble::HostSecured {
            address: PHONE,
            paired: true,
            connection: 0,
        },
    );
    let files_request = json!({"id": 1, "op": "files"});
    assert_eq!(
        ask(&mut device, files_request)["error"],
        "Give the PIN first (auth)"
    );
    let saved: Vec<Bond> = serde_json::from_slice(&files.get(BONDS_FILE).unwrap()).unwrap();
    assert!(!saved[0].app);
}

#[test]
fn a_bond_update_on_its_way_cannot_undo_forget() {
    let (mut device, files) = boot();
    let page = Screen::Settings(SettingsPage::Bluetooth);
    device.push(Event::UiInput(UiInput::Show(page)));
    device.step();
    let forget = Control::BluetoothForget(0);
    device.push(Event::UiInput(UiInput::View(InputEvent::Activate(forget))));
    device.step();
    let sent = &device.platform().bluetooth.as_ref().unwrap().sent;
    assert!(sent.contains(&Command::Forget(PHONE)));
    // The host changed the bond (a notification switch) before it got the FORGET.
    push(
        &mut device,
        Ble::Bonded(Bond {
            notify: 1,
            ..bond(1, true)
        }),
    );
    let saved: Vec<Bond> = serde_json::from_slice(&files.get(BONDS_FILE).unwrap()).unwrap();
    assert!(saved.is_empty());
    // A new pairing of the same phone has new keys.
    push(&mut device, Ble::Bonded(bond(4, false)));
    let saved: Vec<Bond> = serde_json::from_slice(&files.get(BONDS_FILE).unwrap()).unwrap();
    assert_eq!(saved.len(), 1);
}

#[test]
fn the_app_reaches_the_remote_api() {
    let (mut device, _files) = boot();
    push(
        &mut device,
        Ble::HostConnected {
            address: PHONE,
            bonded: true,
        },
    );
    push(
        &mut device,
        Ble::HostSecured {
            address: PHONE,
            paired: false,
            connection: 0,
        },
    );
    let notices = json!({"id": 1, "op": "remote", "method": "GET", "path": "/api/notices"});
    assert!(ask(&mut device, notices)["notices"].is_array());
    let page = json!({"id": 2, "op": "remote", "method": "GET", "path": "/"});
    assert_eq!(ask(&mut device, page)["error"], "not found (404)");
}

/// Remote is off: a backup that the app makes stays for its download while Bluetooth runs.
#[test]
fn the_app_downloads_a_backup_while_remote_is_off() {
    let (mut device, files) = boot();
    device.platform().worker.set(true);
    push(
        &mut device,
        Ble::HostConnected {
            address: PHONE,
            bonded: true,
        },
    );
    push(
        &mut device,
        Ble::HostSecured {
            address: PHONE,
            paired: false,
            connection: 0,
        },
    );
    let remote = |id: u64, method: &str, path: &str| json!({"id": id, "op": "remote", "method": method, "path": path});
    let created = ask(&mut device, remote(1, "POST", "/api/backup/create"));
    assert_eq!(created["state"], "creating");
    files.put(REMOTE_ARCHIVE, b"\x1f\x8bbackup".to_vec());
    let done = BackupDone::Created {
        summary: Summary {
            files: 1,
            dirs: 0,
            bytes: 1,
        },
        notes: Vec::new(),
    };
    let notice = Input::Service(ServiceNotice::Backup(Ok(done)));
    device.platform().inputs.borrow_mut().push(notice);
    for _ in 0..5 {
        device.step();
    }
    assert_eq!(
        ask(&mut device, remote(2, "GET", "/api/backup"))["state"],
        "ready"
    );
    let (_, body) = exchange(&mut device, remote(3, "GET", "/api/backup/file?offset=0"));
    assert_eq!(body, b"\x1f\x8bbackup");
}
