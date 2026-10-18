//! The Add device capture browser on the simulated device (r50, shots sn-4 … sn-7): the
//! library it lists (`factory.db` folders and captures, user captures, Downloads) and the
//! capture block a row builds (`onCaptureClicked` `0x386024` → `0x704530`). The sim has no
//! DSP receive buffers and no NC2 key, so no capture data loads; the block plan is checked.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{Log, Media, STOCK, Sim, light_preset};
use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Action, Device, Event};
use tetrad_engine::{Column, Row, Site, Slot};
use tetrad_media::factory::DeviceCipher;
use tetrad_storage::stock::{
    CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
};
use tetrad_ui::{
    CaptureFolder, CaptureOrigin, Control, DeviceGroup, GridPosition, InputEvent, ParamControl,
    PickerControl, Screen,
};

/// A factory V2 capture (NC2) in "Factory Captures V2" ▸ "Compressor" ▸ "4-Comp Custom".
const FACTORY_V2: &str = "00244c31936883e97c8b1aa1612e2260bf787eb0442adb3bfdd181ba1a2b83a6";
/// A user V1 capture ("My Captures").
const USER_V1: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// The sim catalog with both capture models; each has `file_name` at index 5.
fn fixture() -> common::Fixture {
    let params: String = (0..5)
        .map(|index| format!("<Parameter name='P{index}' type='float' defaultValue='0.5'/>"))
        .chain(["<Parameter name='file_name' type='string' defaultValue=''/>".into()])
        .collect();
    let stock = "<Model id='14001' name='Capture 2' blob='light'>\
<Parameter name='GAIN' type='float' min='0' max='1' defaultValue='0.5'/></Model>";
    let models = STOCK.models.replacen(
        stock,
        &format!(
            "<Model id='14000' name='Capture' blob='light'>{params}</Model>\
<Model id='14001' name='Capture 2' blob='light'>{params}</Model>"
        ),
        1,
    );
    assert_ne!(models, STOCK.models);
    let module = "<Module id='14000' name='Capture' blob_name='light' category='14' dm_heap='100' \
pm_heap='10' sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module></ModuleReport>";
    let report = STOCK.report.replacen("</ModuleReport>", module, 1);
    common::Fixture {
        models: Box::leak(models.into_boxed_str()),
        report: Box::leak(report.into_boxed_str()),
        ..STOCK
    }
}

/// "Light" at row 1, column 3; the factory tree with one V2 capture (as `0x6e86fc` builds it)
/// and one user V1 capture, metadata under the device cipher.
fn device() -> (Device<Sim>, Media) {
    let fixture = fixture();
    let media = Media::new([("Light", light_preset(&fixture.specs(), 2))]);
    let cipher = DeviceCipher::caller_secret(b"invented test secret");
    let ciphered = |json: &str| {
        let mut bytes = json.as_bytes().to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let factory = rusqlite::Connection::open(media.0.join("factory/factory.db")).unwrap();
    factory
        .execute_batch(
            "INSERT INTO folder VALUES (1, 2, 0, 'Factory Captures V1', 0, 1);
INSERT INTO folder VALUES (2, 2, 0, 'Factory Captures V2', 0, 1);
INSERT INTO folder VALUES (137, 2, 0, 'Compressor', 2, 1);
INSERT INTO folder VALUES (132, 2, 0, 'Amp', 2, 1);
INSERT INTO folder VALUES (170, 2, 0, '4-Comp Custom', 137, 1);",
        )
        .unwrap();
    let metadata = ciphered(
        r#"{"name":"4-Comp Custom 1","version":"2","deviceType":"compressor","date":"2025-04-29T10:00:00Z"}"#,
    );
    factory
        .execute(
            "INSERT INTO product VALUES (?1, 2, 1, x'0102', ?2)",
            rusqlite::params![FACTORY_V2, metadata],
        )
        .unwrap();
    for folder in [2, 137, 170] {
        factory
            .execute(
                "INSERT INTO folder_content VALUES (?1, ?2)",
                rusqlite::params![folder, FACTORY_V2],
            )
            .unwrap();
    }
    drop(factory);
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        write.put_product(&StockProduct {
            id: USER_V1.into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(vec![1; 8]),
            metadata: Some(ciphered(r#"{"name":"Plexi","version":"1"}"#)),
        })
    })
    .unwrap();
    drop(db);
    let log = Rc::new(RefCell::new(Log::default()));
    let mut sim = Sim::new(fixture, &media, &log);
    sim.media_cipher = Some(cipher);
    let files = sim.services().files().clone();
    for marker in [CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED] {
        files.put(marker, "");
    }
    (Device::start(sim).unwrap(), media)
}

fn activate(device: &mut Device<Sim>, control: Control) {
    let shown = (device.bridges().ui.view().hit_regions().iter())
        .any(|region| region.enabled && region.control == control);
    assert!(shown, "{control:?} is not on screen");
    device.push(Event::UiInput(UiInput::View(InputEvent::Activate(control))));
    (0..4).for_each(|_| _ = device.step());
}

fn block(device: &Device<Sim>, column: u8) -> Option<(u32, Option<String>)> {
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(column).unwrap(),
    };
    let current = device.state().current.as_ref()?;
    let block = current.deployment.layout().instance(Site::Slot(slot))?;
    let text = (block.texts.iter()).find(|text| text.index == 5)?;
    assert!(
        text.scenes.windows(2).all(|pair| pair[0] == pair[1]),
        "all scenes"
    );
    Some((block.model.0, text.scenes[0].clone()))
}

/// Taps the capture row named `name` in the open browser.
fn pick(device: &mut Device<Sim>, name: &str) {
    let snapshot = device.bridges().ui.view().snapshot();
    let index = (snapshot.capture_library.captures.iter())
        .position(|capture| capture.name == name)
        .unwrap();
    activate(
        device,
        Control::Picker(PickerControl::Capture(index as u16)),
    );
}

/// Shot sn-4 (`0x841f60`): one tap on an empty slot of a row with an input opens the device
/// list; Neural Capture opens the browser with the Captures Library: the `factory.db` tree
/// (Factory Captures V1 / V2 and their subfolders) and "My Captures", the factory and user
/// captures with name, date and version. A factory V2 row adds a 14001 block (`0x704530`)
/// whose parameter 5 is key + name in every scene; a version "1" capture picked for that
/// block from its editor title replaces it with a 14000 block (the model follows the record).
#[test]
fn the_capture_browser_lists_the_library_and_builds_the_block_of_the_record() {
    let (mut device, _media) = device();
    let slot = GridPosition::new(0, 4).unwrap();
    activate(&mut device, Control::SelectCell(slot));
    let group = Control::Picker(PickerControl::Group(DeviceGroup::NeuralCapture));
    activate(&mut device, group);
    // The list is read on a worker thread from boot on.
    for _ in 0..500 {
        let library = &device.bridges().ui.view().snapshot().capture_library;
        if library.captures.len() == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        device.step();
    }
    let view = device.bridges().ui.view();
    assert!(view.capture_browser());
    let library = &view.snapshot().capture_library;
    let folders: Vec<_> = (library.folders.iter())
        .map(|folder| {
            let parent = folder
                .parent
                .map(|at| &*library.folders[usize::from(at)].name);
            (folder.name.as_str(), parent)
        })
        .collect();
    assert_eq!(
        folders,
        [
            ("Factory Captures V1", None),
            ("Factory Captures V2", None),
            ("Amp", Some("Factory Captures V2")),
            ("Compressor", Some("Factory Captures V2")),
            ("4-Comp Custom", Some("Compressor")),
            ("My Captures", None),
        ]
    );
    let captures: Vec<_> = (library.captures.iter())
        .map(|capture| (capture.name.as_str(), capture.origin, capture.nc2))
        .collect();
    assert_eq!(
        captures,
        [
            ("4-Comp Custom 1", CaptureOrigin::Factory, true),
            ("Plexi", CaptureOrigin::User, false),
        ]
    );
    assert_eq!(library.captures[0].date, "2025-04-29T10:00:00Z");
    assert_eq!(library.captures[0].folders, [1, 3, 4]);

    // Factory Captures V2 has subfolders: it is entered (shot sn-5), ← shows.
    let v2 = Control::Picker(PickerControl::CaptureFolder(CaptureFolder::Folder(1)));
    activate(&mut device, v2);
    let back = Control::Picker(PickerControl::CaptureBack);
    let shown = |device: &Device<Sim>, control| {
        (device.bridges().ui.view().hit_regions().iter()).any(|region| region.control == control)
    };
    assert!(shown(&device, back));
    pick(&mut device, "4-Comp Custom 1");
    assert_eq!(device.bridges().ui.view().screen(), Screen::Grid);
    let file_name = format!("{FACTORY_V2}4-Comp Custom 1");
    assert_eq!(block(&device, 4), Some((14_001, Some(file_name.clone()))));
    let snapshot = device.bridges().ui.view().snapshot();
    assert_eq!(
        snapshot.grid[4].label, "4-Comp Custom 1",
        "the capture name"
    );
    // `0x59b7bc` selects nothing, so no editor opens: the double ring of `nc-s1` is the
    // Capture 2.0 mark (`0x6966d4`), not a selection.
    assert_eq!(snapshot.selected_block, None);
    // The record's `deviceType` is in the block from its build (`0x2ed0e0`); the sim loads no
    // NC2 data, so it does not come from the load.
    assert_eq!(
        snapshot.captures[4],
        Some(tetrad_ui::CaptureDevice::Compressor)
    );
    // `0x59dd78`: the factory capture is the newest capture recent, and the browser's Recent
    // folder lists it.
    let recents = (device.state().library.as_ref().unwrap().presets)
        .items(tetrad_library::stock::ItemList::Recents(
            tetrad_storage::media::Collection::Captures,
        ))
        .unwrap();
    assert_eq!(recents[0].item, FACTORY_V2);
    let directory = &device.bridges().ui.view().snapshot().directory;
    assert_eq!(
        directory.capture_recents.first().map(String::as_str),
        Some(FACTORY_V2)
    );

    // The editor title (`onTitleAreaClicked`) opens the browser to replace the block.
    activate(&mut device, Control::SelectCell(slot));
    activate(&mut device, Control::Param(ParamControl::ChangeModel));
    assert!(device.bridges().ui.view().capture_browser());
    pick(&mut device, "Plexi");
    let plexi = format!("{USER_V1}Plexi");
    assert_eq!(block(&device, 4), Some((14_000, Some(plexi))));
    device.push(Event::Action(Action::Undo));
    (0..4).for_each(|_| _ = device.step());
    assert_eq!(block(&device, 4), Some((14_001, Some(file_name))));
}

/// Shot sn-1: an empty slot of a row without input opens nothing.
#[test]
fn an_empty_slot_of_a_row_without_input_opens_nothing() {
    let (mut device, _media) = device();
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(1, 4).unwrap()),
    );
    assert_eq!(device.bridges().ui.view().screen(), Screen::Grid);
}

/// Seen on the QC before a fix: a capture pick whose deploy failed left an undo frame
/// with no block. Stock (`0x59ec78`) pushes the frame only after the fit check passes; Tetrad
/// keeps the frame only when the deploy commits (`Action::Undoable`).
#[test]
fn a_pick_whose_deploy_fails_leaves_no_undo_frame() {
    let (mut device, _media) = device();
    let depths = device.state().history.depths();
    // The DSP never reads the run-list clear `0x1e`: the deploy fails.
    device
        .platform()
        .clock
        .hold
        .set(Some(|_, frame| frame[0] == 0x1e));
    activate(
        &mut device,
        Control::SelectCell(GridPosition::new(0, 4).unwrap()),
    );
    let group = Control::Picker(PickerControl::Group(DeviceGroup::NeuralCapture));
    activate(&mut device, group);
    for _ in 0..500 {
        if device
            .bridges()
            .ui
            .view()
            .snapshot()
            .capture_library
            .captures
            .len()
            == 2
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        device.step();
    }
    pick(&mut device, "Plexi");
    assert_eq!(block(&device, 4), None, "the deploy failed");
    assert_eq!(device.state().history.depths(), depths, "no orphan frame");
}
