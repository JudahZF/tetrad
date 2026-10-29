//! Remote IR library (docs/remote.md): the IRs page lists the user IRs with their folders and the
//! saving destination, and renames, deletes, links and makes folders through the touch UI's own
//! paths, so the Directory sees each change. The server is a fake: tests push its calls.

mod common;

use common::{FakeRemote, Media, STOCK, Sim, boot_remote, light_preset};
use serde_json::{Value, json};
use tetrad_app::device::remote::SETTINGS_FILE;
use tetrad_app::device::{Device, Event};
use tetrad_remote::{Call, LibraryOp, Reply, Request};
use tetrad_services::fake::MemFiles;
use tetrad_storage::stock::ProductType;

const PIN: &str = "0001";

/// A device with Remote on (PIN 0001) and no user IRs. The media folder lives as long as the
/// returned [`Media`].
fn device() -> (Device<Sim>, Media) {
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    boot_remote(media, files, FakeRemote::default())
}

fn remote(device: &Device<Sim>) -> &FakeRemote {
    device.platform().remote.as_ref().unwrap()
}

/// Sends one page request and returns the device's reply.
fn ask(device: &mut Device<Sim>, request: Request) -> Reply {
    let id = remote(device).replies.len() as u64 + 1;
    device.push(Event::Remote(Call { id, request }));
    device.step();
    let (replied, reply) = remote(device).replies.last().expect("a reply").clone();
    assert_eq!(replied, id);
    reply
}

/// One library request with the right PIN: the listing, or the reason of a refusal.
fn library(device: &mut Device<Sim>, op: LibraryOp) -> Result<Value, String> {
    let pin = PIN.into();
    match ask(device, Request::Library { pin, op }) {
        Reply::Json(listing) => Ok(listing),
        Reply::Refused(reason) => Err(reason),
        other => panic!("a listing or a refusal: {other:?}"),
    }
}

/// Uploads an IR as the page does and returns its id. The samples depend on the name: the id is
/// the MD5 of the WAV, so equal samples would replace the earlier IR.
fn upload(device: &mut Device<Sim>, name: &str) -> String {
    let samples = 1024_u32;
    let offset = name.len() as f32;
    let mut wav = b"RIFF".to_vec();
    wav.extend((38 + samples * 4).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(18_u32.to_le_bytes());
    wav.extend([3, 0, 1, 0]);
    wav.extend(48_000_u32.to_le_bytes());
    wav.extend(192_000_u32.to_le_bytes());
    wav.extend([4, 0, 32, 0, 0, 0]);
    wav.extend(b"data");
    wav.extend((samples * 4).to_le_bytes());
    wav.extend((0..samples).flat_map(|i| ((i as f32 + offset) / 2048.0).to_le_bytes()));
    let request = Request::Upload {
        pin: PIN.into(),
        name: name.into(),
        wav,
    };
    assert!(matches!(ask(device, request), Reply::Added { .. }));
    let listing = library(device, LibraryOp::List).unwrap();
    let irs = listing["irs"].as_array().unwrap();
    let ir = irs
        .iter()
        .find(|ir| ir["name"] == name)
        .expect("the new IR");
    ir["id"].as_str().unwrap().to_owned()
}

fn root(listing: &Value) -> i64 {
    let folders = listing["folders"].as_array().unwrap();
    assert_eq!(folders[0]["root"], true, "the root comes first");
    folders[0]["id"].as_i64().unwrap()
}

fn new_folder(device: &mut Device<Sim>, name: &str, parent: Option<i64>) -> i64 {
    let op = LibraryOp::CreateFolder {
        name: name.into(),
        parent,
    };
    let listing = library(device, op).unwrap();
    let folders = listing["folders"].as_array().unwrap();
    let folder = folders.iter().find(|folder| folder["name"] == name);
    folder.expect("the new folder")["id"].as_i64().unwrap()
}

/// The names in the touch UI's media picker: it follows each change of the page.
fn picker_names(device: &Device<Sim>) -> Vec<String> {
    let media = device.bridges().ui.media();
    let user = media.iter().filter(|entry| entry.value.starts_with("CIR_"));
    user.map(|entry| entry.name.clone()).collect()
}

/// The listing holds the user IRs (no factory IRs) with name, import date and folders, the
/// writable IR folders root first, and the saving destination.
#[test]
fn the_listing_shows_user_irs_their_folders_and_the_destination() {
    let (mut device, _media) = device();
    let empty = library(&mut device, LibraryOp::List).unwrap();
    assert_eq!(empty["irs"], json!([]));
    let root = root(&empty);
    assert_eq!(empty["destination"], root);
    assert_eq!(empty["folders"][0]["name"], "My IRs");

    let id = upload(&mut device, "V30 SM57");
    let cabs = new_folder(&mut device, "Cabs", None);
    let listing = library(&mut device, LibraryOp::List).unwrap();
    assert_eq!(
        listing["irs"],
        json!([{
            "id": id,
            "name": "V30 SM57",
            "date": "2026-10-10T12:34:56Z",
            "folders": [root],
        }])
    );
    let cabs_row = json!({ "id": cabs, "name": "Cabs", "parent": root, "root": false });
    assert_eq!(listing["folders"][1], cabs_row);
}

/// Link and unlink write `folder_content` as the Directory's ADD TO FOLDER / REMOVE does; the
/// root keeps every IR, and an IR or folder that is not there is refused.
#[test]
fn an_ir_joins_and_leaves_folders_but_stays_in_the_root() {
    let (mut device, _media) = device();
    let id = upload(&mut device, "V30 SM57");
    let root = root(&library(&mut device, LibraryOp::List).unwrap());
    let cabs = new_folder(&mut device, "Cabs", None);
    let link = |folder, linked| LibraryOp::Link {
        id: id.clone(),
        folder,
        linked,
    };

    let linked = library(&mut device, link(cabs, true)).unwrap();
    assert_eq!(linked["irs"][0]["folders"], json!([root, cabs]));
    let unlinked = library(&mut device, link(cabs, false)).unwrap();
    assert_eq!(unlinked["irs"][0]["folders"], json!([root]));

    let refused = library(&mut device, link(root, false)).unwrap_err();
    assert!(refused.contains("root"), "{refused}");
    assert!(library(&mut device, link(9999, true)).is_err());
    // A writable folder of another type (here the captures root) takes no IR.
    let captures = (device.state().library.as_ref())
        .and_then(|library| library.database.as_ref())
        .map(|db| {
            db.view()
                .root_folder(ProductType::Capture)
                .unwrap()
                .unwrap()
        })
        .unwrap();
    let refused = library(&mut device, link(captures, true)).unwrap_err();
    assert!(refused.contains("not in the library"), "{refused}");
    let unknown = LibraryOp::Link {
        id: "CIR_unknown".into(),
        folder: cabs,
        linked: true,
    };
    assert!(library(&mut device, unknown).is_err());
    let listing = library(&mut device, LibraryOp::List).unwrap();
    assert_eq!(listing["irs"][0]["folders"], json!([root]));
}

/// A rename follows the IR name rule (4 to 60 characters, spaces simplified) and reaches the
/// touch UI's media picker.
#[test]
fn a_rename_follows_the_name_rule_and_reaches_the_picker() {
    let (mut device, _media) = device();
    let id = upload(&mut device, "V30 SM57");
    let rename = |name: &str| LibraryOp::Rename {
        id: id.clone(),
        name: name.into(),
    };

    let renamed = library(&mut device, rename("  Greenback   121 ")).unwrap();
    assert_eq!(renamed["irs"][0]["name"], "Greenback 121");
    assert_eq!(picker_names(&device), ["Greenback 121"]);

    for bad in ["abc", &"x".repeat(61)] {
        let refused = library(&mut device, rename(bad)).unwrap_err();
        assert!(refused.contains("4 to 60"), "{refused}");
    }
    assert_eq!(picker_names(&device), ["Greenback 121"]);
}

/// A delete removes the IR from the library, its folders and the touch UI's media picker.
#[test]
fn a_delete_removes_the_ir_everywhere() {
    let (mut device, _media) = device();
    let kept = upload(&mut device, "V30 SM57");
    let gone = upload(&mut device, "Greenback 121");
    let cabs = new_folder(&mut device, "Cabs", None);
    let link = LibraryOp::Link {
        id: gone.clone(),
        folder: cabs,
        linked: true,
    };
    library(&mut device, link).unwrap();

    let listing = library(&mut device, LibraryOp::Delete { id: gone.clone() }).unwrap();
    let irs = listing["irs"].as_array().unwrap();
    assert_eq!(irs.len(), 1);
    assert_eq!(irs[0]["id"], kept.as_str());
    assert_eq!(picker_names(&device), ["V30 SM57"]);
    assert!(library(&mut device, LibraryOp::Delete { id: gone }).is_err());
}

/// The saving destination is the stock setting: the status line and the next upload follow it.
/// Only a writable IR folder can be one.
#[test]
fn the_destination_sets_where_uploads_go() {
    let (mut device, _media) = device();
    let cabs = new_folder(&mut device, "Cabs", None);
    let nested = new_folder(&mut device, "Nested", Some(cabs));

    let listing = library(&mut device, LibraryOp::Destination { folder: nested }).unwrap();
    assert_eq!(listing["destination"], nested);
    let status = Request::Status { pin: PIN.into() };
    let Reply::Status { folder } = ask(&mut device, status) else {
        panic!("the status");
    };
    assert_eq!(folder, "Nested");
    let id = upload(&mut device, "V30 SM57");
    let listing = library(&mut device, LibraryOp::List).unwrap();
    let root = root(&listing);
    assert_eq!(listing["irs"][0]["id"], id.as_str());
    assert_eq!(listing["irs"][0]["folders"], json!([root, nested]));

    assert!(library(&mut device, LibraryOp::Destination { folder: 9999 }).is_err());
    let listing = library(&mut device, LibraryOp::List).unwrap();
    assert_eq!(listing["destination"], nested);
}

/// Folder names follow the touch keyboard rule; the folder count is capped as on the unit.
#[test]
fn a_new_folder_follows_the_name_rule_and_the_cap() {
    let (mut device, _media) = device();
    let create = |name: &str| LibraryOp::CreateFolder {
        name: name.into(),
        parent: None,
    };
    let refused = library(&mut device, create("abc")).unwrap_err();
    assert!(refused.contains("4 to 60"), "{refused}");

    // The root counts as one of the 30.
    for n in 1..30 {
        library(&mut device, create(&format!("Folder {n}"))).unwrap();
    }
    let refused = library(&mut device, create("Folder 30")).unwrap_err();
    assert!(refused.contains("30 IR folders"), "{refused}");
}

/// Every library request needs the PIN; a wrong PIN changes nothing.
#[test]
fn a_wrong_pin_changes_nothing() {
    let (mut device, _media) = device();
    let id = upload(&mut device, "V30 SM57");
    let before = library(&mut device, LibraryOp::List).unwrap();
    let root = root(&before);
    let ops = [
        LibraryOp::List,
        LibraryOp::Rename {
            id: id.clone(),
            name: "Greenback 121".into(),
        },
        LibraryOp::Delete { id: id.clone() },
        LibraryOp::Link {
            id,
            folder: root,
            linked: false,
        },
        LibraryOp::Destination { folder: root },
        LibraryOp::CreateFolder {
            name: "Cabs".into(),
            parent: None,
        },
    ];
    for op in ops {
        let pin = "9999".into();
        assert_eq!(
            ask(&mut device, Request::Library { pin, op }),
            Reply::WrongPin
        );
    }
    assert_eq!(library(&mut device, LibraryOp::List).unwrap(), before);
}
