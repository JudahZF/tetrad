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
