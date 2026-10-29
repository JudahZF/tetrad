//! Remote Themes page (docs/remote.md, docs/themes.md): the list of every theme, theme files
//! saved from the browser with the checks of a file on the card, and choosing and deleting
//! themes as Settings → Appearance does.

mod common;

use std::path::Path;

use common::{FakeRemote, Media, STOCK, Sim, boot_remote, light_preset};
use serde_json::{Value, json};
use tetrad_app::device::appearance::{FILE, THEMES_DIR};
use tetrad_app::device::remote::SETTINGS_FILE;
use tetrad_app::device::{Device, Event};
use tetrad_remote::{Call, Reply, Request, ThemesOp};
use tetrad_services::exec::Files;
use tetrad_services::fake::MemFiles;
use tetrad_theme::{Rgb, ThemeId, ThemeSpec};

const PIN: &str = "0001";

/// A unit with Remote on (PIN 0001) over `files`.
fn boot(files: &MemFiles) -> Device<Sim> {
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    boot_remote(media, files.clone(), FakeRemote::default()).0
}

fn ask(device: &mut Device<Sim>, op: ThemesOp) -> Reply {
    let remote = device.platform().remote.as_ref().unwrap();
    let id = remote.replies.len() as u64 + 1;
    let request = Request::Themes {
        pin: PIN.into(),
        op,
    };
    device.push(Event::Remote(Call { id, request }));
    device.step();
    let remote = device.platform().remote.as_ref().unwrap();
    let (replied, reply) = remote.replies.last().expect("a reply").clone();
    assert_eq!(replied, id);
    reply
}

/// The list the op answers with; panics on a refusal.
fn list(device: &mut Device<Sim>, op: ThemesOp) -> Value {
    match ask(device, op) {
        Reply::Json(list) => list,
        other => panic!("a list, not {other:?}"),
    }
}

fn theme<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["themes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
}

fn save(value: Value) -> ThemesOp {
    ThemesOp::Save {
        json: serde_json::to_vec(&value).unwrap(),
    }
}

fn midnight() -> Value {
    json!({
        "version": 1, "id": "midnight", "name": "Midnight", "base": "slate",
        "colors": { "accent": "#7aa2f7" }
    })
}

fn stored_theme(files: &MemFiles) -> String {
    let stored: Value = serde_json::from_slice(&files.get(FILE).unwrap()).unwrap();
    stored["theme"].as_str().unwrap().to_owned()
}

/// The page draws its editor from this list: every built-in with its whole palette, the
/// chosen theme, and one field per palette role.
#[test]
fn the_list_has_the_builtins_the_active_theme_and_every_role() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    let list = list(&mut device, ThemesOp::List);
    assert_eq!(list["active"], "stock");
    let ids: Vec<_> = (list["themes"].as_array().unwrap().iter())
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["stock", "stage-contrast", "slate", "ember"]);
    let stock = theme(&list, "stock").unwrap();
    assert_eq!(stock["source"], "builtin");
    assert_eq!(stock["palette"]["accent"], "#45f862");
    // Every role of the palette is a field, and only those.
    let mut keys: Vec<_> = (list["fields"].as_array().unwrap().iter())
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    let mut roles: Vec<_> = stock["palette"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    roles.sort();
    assert_eq!(keys, roles);
}

/// A saved theme is a file the unit reads back: it shows on Settings → Appearance, can be
/// chosen (appearance.json keeps it), is saved again into the same file, and deleting it while
/// it shows brings back Stock.
#[test]
fn a_saved_theme_is_listed_chosen_replaced_and_deleted() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    let listed = list(&mut device, save(midnight()));
    let path = format!("{THEMES_DIR}/midnight.json");
    let written = ThemeSpec::from_file(&files.get(&path).unwrap()).unwrap();
    assert_eq!(written.colors.accent, Some(Rgb::hex(0x7aa2f7)));
    let row = theme(&listed, "file:midnight").unwrap();
    assert_eq!(
        (&row["source"], &row["base"]),
        (&json!("file"), &json!("slate"))
    );
    assert_eq!(row["colors"], json!({ "accent": "#7aa2f7" }));
    assert_eq!(row["error"], Value::Null);
    let on_unit = &device.bridges().ui.view().snapshot().appearance;
    assert!(
        on_unit
            .themes
            .iter()
            .any(|t| t.id == ThemeId::File("midnight".into()))
    );

    let chosen = list(
        &mut device,
        ThemesOp::Select {
            id: "file:midnight".into(),
        },
    );
    assert_eq!(chosen["active"], "file:midnight");
    assert_eq!(stored_theme(&files), "file:midnight");
    let shown = device.bridges().ui.view().snapshot().appearance.palette;
    assert_eq!(shown.accent, Rgb::hex(0x7aa2f7));

    // The same ID again replaces its file, whatever its name.
    let copied = format!("{THEMES_DIR}/copied.json");
    files.put(&copied, files.get(&path).unwrap());
    Files::remove(&mut files.clone(), Path::new(&path)).unwrap();
    let mut changed = midnight();
    changed["colors"]["accent"] = json!("#ff8800");
    list(&mut device, save(changed));
    assert_eq!(files.get(&path), None);
    let written = ThemeSpec::from_file(&files.get(&copied).unwrap()).unwrap();
    assert_eq!(written.colors.accent, Some(Rgb::hex(0xff8800)));
    let shown = device.bridges().ui.view().snapshot().appearance.palette;
    assert_eq!(
        shown.accent,
        Rgb::hex(0xff8800),
        "the unit shows the new colours"
    );

    let deleted = list(
        &mut device,
        ThemesOp::Delete {
            id: "file:midnight".into(),
        },
    );
    assert_eq!(files.get(&copied), None);
    assert_eq!(theme(&deleted, "file:midnight"), None);
    assert_eq!(deleted["active"], "stock");
    assert_eq!(stored_theme(&files), "stock");
}

/// The unit, not the page, decides: a theme that fails the checks of a file on the card is
/// refused with the reason and writes nothing. So are built-in IDs.
#[test]
fn a_theme_that_fails_the_checks_is_refused_and_writes_nothing() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    let mut unreadable = midnight();
    unreadable["colors"]["muted"] = json!("#303030");
    let Reply::Refused(reason) = ask(&mut device, save(unreadable)) else {
        panic!("refused");
    };
    assert!(reason.contains("muted on background"), "{reason}");
    let mut builtin = midnight();
    builtin["id"] = json!("slate");
    let Reply::Refused(reason) = ask(&mut device, save(builtin)) else {
        panic!("refused");
    };
    assert!(reason.contains("built-in"), "{reason}");
    assert_eq!(files.get(&format!("{THEMES_DIR}/midnight.json")), None);
    assert_eq!(files.get(&format!("{THEMES_DIR}/slate.json")), None);
}

#[test]
fn only_theme_files_can_be_deleted() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    for id in [
        "stock",
        "ext:judah/themes:judah.dusk-pack/dusk",
        "file:nothing",
    ] {
        let reply = ask(&mut device, ThemesOp::Delete { id: id.into() });
        assert!(matches!(reply, Reply::Refused(_)), "{id}: {reply:?}");
    }
}
