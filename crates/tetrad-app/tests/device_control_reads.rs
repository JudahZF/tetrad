//! Cortex Control READ replies of the State-backed model against the stock S7 capture, and the
//! pushes and pass costs that need the whole step loop, through the simulated device.

use std::fs;
use std::time::Instant;

use tetrad_app::device::Device;
use tetrad_control::codecs::{
    CloudProductMessage, ConnectionMessage, ControlMessage, CortexMessageType, FileMessage,
    KeepAliveMessage, MessageAction, ModelRepoMessage,
};
use tetrad_control::{Envelope, Limits};

mod common;

fn read() -> Option<tetrad_control::codecs::EnumValue<MessageAction>> {
    Some(MessageAction::Read.into())
}

/// One host message into the simulated device, then idle steps for the reply.
fn host(device: &mut Device<common::Sim>, message: impl Into<ControlMessage>) {
    let message = message.into();
    let envelope = Envelope {
        message_type: message.message_type() as u32,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: message
            .encode(tetrad_control::link::LinkLimits::default().payload)
            .unwrap(),
    };
    for report in envelope.reports(Limits::default()).unwrap() {
        device.push(tetrad_app::device::Event::Control(Box::new(report)));
        device.step();
    }
    (0..50).for_each(|_| _ = device.step());
}

/// Boots `media` with the stock capture markers and a fake serial, then runs the Cortex
/// Control handshake (keep-alive, ModelRepo READ, Connection).
fn connected(
    media: common::Media,
) -> (
    Device<common::Sim>,
    std::rc::Rc<std::cell::RefCell<common::Log>>,
    common::Media,
) {
    connected_with(media, |_| {})
}

/// [`connected`] with `setup` run on the platform before the boot.
fn connected_with(
    media: common::Media,
    setup: impl FnOnce(&mut common::Sim),
) -> (
    Device<common::Sim>,
    std::rc::Rc<std::cell::RefCell<common::Log>>,
    common::Media,
) {
    connected_in(common::STOCK, media, setup)
}

/// [`connected_with`] over the catalog of `fixture`.
fn connected_in(
    fixture: common::Fixture,
    media: common::Media,
    setup: impl FnOnce(&mut common::Sim),
) -> (
    Device<common::Sim>,
    std::rc::Rc<std::cell::RefCell<common::Log>>,
    common::Media,
) {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::platform::{Platform, ServiceBackends};
    let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(fixture, &media, &log);
    let files = sim.services().files();
    files.put(tetrad_media::factory::SERIAL_PATH, "TEST12345\n");
    for marker in [CAPTURE_CORRUPTION_CHECKED, CAPTURE_CONVERSION_FINISHED] {
        files.put(marker, "");
    }
    setup(&mut sim);
    let mut device = Device::start(sim).unwrap();
    host(
        &mut device,
        KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            ..Default::default()
        },
    );
    host(
        &mut device,
        ModelRepoMessage {
            action: read(),
            ..Default::default()
        },
    );
    host(
        &mut device,
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        },
    );
    assert!(device.bridges().control.connected());
    (device, log, media)
}

/// A preset sidecar as stock writes it.
fn sidecar(name: &str) -> String {
    format!(
        "{{\"author\": \"PlayerMusic\",\"authorId\": \"a1b2c3d4\",\"cloudId\": \"\",\"corosVersion\": \"3.1.0\",\"date\": \"2025-06-14T20:27:38Z\",\"description\": \"\",\"instrument_type\": \"guitar\",\"name\": \"{name}\",\"tags\": []}}"
    )
}

/// The QC library of the S7 capture at scale: 10 setlists, the factory plugin tree (12
/// plugins, 290 presets each, as `/opt/neuraldsp/Plugins` holds 3 460), 42 captures in the
/// user root and 389 downloaded captures.
fn qc_scale_media() -> common::Media {
    use tetrad_storage::media::{SETLIST_SLOTS, encode_setlist};
    let light = common::light_preset(&common::STOCK.specs(), 2);
    let media = common::Media::new([("Light", light.clone())]);
    let root = &media.0;
    let mut folders = format!("{}\n", common::SETLIST);
    for setlist in 0..9 {
        let name = format!("Setlist {setlist}");
        let dir = root.join("Presets").join(&name);
        fs::create_dir_all(&dir).unwrap();
        let mut slots = vec![None; SETLIST_SLOTS];
        for (index, slot) in slots.iter_mut().take(8).enumerate() {
            let preset = format!("Song {index}");
            fs::write(dir.join(format!("{preset}.pb")), light.to_bytes()).unwrap();
            fs::write(dir.join(format!("{preset}.json")), sidecar(&preset)).unwrap();
            *slot = Some(format!("/media/p4/Presets/{name}/{preset}.pb"));
        }
        fs::write(dir.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
        folders.push_str(&format!("/media/p4/Presets/{name}\n"));
    }
    fs::write(root.join("user_preset_folders"), folders).unwrap();
    for plugin in 0..12 {
        for folder in ["Neural DSP", "Artists/One", "Artists/Two"] {
            let dir = root.join(format!("factory/Plugins/Plugin {plugin}/{folder}"));
            fs::create_dir_all(&dir).unwrap();
            for preset in 0..97 {
                let name = format!("Preset {preset}");
                fs::write(dir.join(format!("{name}.pb")), b"").unwrap();
                fs::write(dir.join(format!("{name}.json")), sidecar(&name)).unwrap();
            }
        }
    }
    let db = rusqlite::Connection::open(root.join("zenui.db")).unwrap();
    db.execute_batch(
        "INSERT INTO folder VALUES (1, 2, 1, 'My Captures', 0, 0);
         INSERT INTO folder VALUES (2, 1, 1, 'My IRs', 0, 0);",
    )
    .unwrap();
    let capture = |n: usize| {
        format!(
            "{{\"authorId\": \"161cc91b\",\"authorName\": \"ToneJunkie\",\"cloudUUID\": \"uuid-{n}\",\"corosVersion\": \"\",\"date\": \"2023-07-28T20:31:14Z\",\"description\": \"A capture\",\"deviceType\": \"pedal\",\"gainType\": 3,\"instrumentType\": \"guitar\",\"name\": \"Capture {n}\",\"tags\": [],\"version\": \"1\"}}"
        )
    };
    for n in 0..42 {
        let id = format!("{n:060}");
        db.execute(
            "INSERT INTO product VALUES (?1, 2, 0, ?2, ?3)",
            rusqlite::params![id, vec![0u8; 10_000], capture(n).into_bytes()],
        )
        .unwrap();
        db.execute("INSERT INTO folder_content VALUES (1, ?1)", [&id])
            .unwrap();
    }
    for n in 0..389 {
        db.execute(
            "INSERT INTO downloads_product_nc VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                format!("cloud-{n}"),
                format!("{n:060}"),
                vec![0u8; 10_000],
                capture(n).into_bytes()
            ],
        )
        .unwrap();
    }
    media
}

/// The Cortex Control 4.1.0 connect burst after ModelRepo, as raw payloads (from the stock S7
/// trace): `(message type, payload)`. Most are `{action: READ}`.
const CONNECT_BURST: [(u32, &[u8]); 38] = [
    (35, &[0x08, 3]),
    (26, &[0x10, 1]),
    (58, &[0x08, 3]),
    (21, &[0x08, 3]),
    (3, &[0x08, 3]),
    (9, &[0x08, 3]),
    (24, &[0x08, 3]),
    (14, &[0x08, 3]),
    (38, &[0x08, 3]),
    (17, &[0x08, 3]),
    (4, &[0x08, 3, 0x58, 1]),
    (71, &[0x08, 3]),
    (46, &[0x08, 3, 0x42, 0]),
    (20, &[0x08, 3]),
    (20, &[0x08, 3, 0x28, 1]),
    (20, &[0x08, 3, 0x28, 2]),
    (20, &[0x08, 3, 0x18, 1]),
    (20, &[0x08, 3, 0x18, 1, 0x28, 1]),
    (20, &[0x08, 3, 0x18, 1, 0x28, 2]),
    (42, &[0x08, 3]),
    (15, &[0x08, 3]),
    (50, &[0x08, 3]),
    (54, &[0x08, 3]),
    (19, &[0x08, 3]),
    (33, &[0x08, 3]),
    (2, &[0x08, 3]),
    (34, &[0x08, 3]),
    (13, &[0x08, 3]),
    (57, &[0x08, 3]),
    (60, &[0x08, 3]),
    (32, &[0x08, 1, 0x10, 0]),
    (28, &[0x08, 3]),
    (32, &[0x08, 1, 0x10, 0]),
    (
        37,
        &[
            0x10, 2, 0x18, 0, 0x20, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ],
    ),
    (
        37,
        &[
            0x10, 3, 0x18, 1, 0x20, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ],
    ),
    (
        37,
        &[
            0x10, 4, 0x18, 2, 0x20, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ],
    ),
    (
        37,
        &[
            0x10, 5, 0x18, 3, 0x20, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ],
    ),
    (5, &[0x10, 6]),
];

/// One raw host message as HID reports in the device queue, with no step: a burst then
/// arrives in one gather, as on the QC.
fn queue_raw(device: &mut Device<common::Sim>, message_type: u32, payload: &[u8]) {
    let envelope = Envelope {
        message_type,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: payload.to_vec(),
    };
    for report in envelope.reports(Limits::default()).unwrap() {
        device.push(tetrad_app::device::Event::Control(Box::new(report)));
    }
}

/// Host probe of the connect burst at QC scale (A3): the whole burst arrives in one gather,
/// then the wall time and HID reports of each pass print. Not a gate (`cargo test --release
/// -- --ignored --nocapture`). The QC runs these paths about 100–150 times slower, so a pass
/// above about 0.4 ms here can block QC input for more than 50 ms.
#[test]
#[ignore]
fn probe_library_read_cost_at_qc_scale() {
    // The stock catalog when `$QC_CORPUS_DIR/plain` holds `ModelRepo.xml` and
    // `moduleReport.xml`; every blob is the sim Light blob.
    let fixture = match std::env::var_os("QC_CORPUS_DIR") {
        Some(root) => {
            let root = std::path::PathBuf::from(root).join("plain");
            let text =
                |name| -> &'static str { fs::read_to_string(root.join(name)).unwrap().leak() };
            common::Fixture {
                models: text("ModelRepo.xml"),
                report: text("moduleReport.xml"),
                blob: |_| common::light_blob(),
                diffs: &[],
            }
        }
        None => common::STOCK,
    };
    // The boot passes (the QC showed 813 ms at boot: apply 271, views 518).
    let boot = {
        let media = qc_scale_media();
        let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
        let mut device = Device::start(common::Sim::new(fixture, &media, &log)).unwrap();
        (0..3)
            .map(|pass| {
                let start = Instant::now();
                device.step();
                let units = device.last_units(std::time::Duration::from_micros(20));
                format!("boot pass {pass}: {:?} {units}", start.elapsed())
            })
            .collect::<Vec<_>>()
    };
    eprintln!("{}", boot.join("\n"));
    let (mut device, _log, _media) = connected_in(fixture, qc_scale_media(), |_| {});
    for round in 0..2 {
        for (message_type, payload) in CONNECT_BURST {
            queue_raw(&mut device, message_type, payload);
        }
        let mut total = std::time::Duration::ZERO;
        for pass in 0..200 {
            // Built reports: the sent and the waiting ones.
            let built = |device: &Device<common::Sim>| {
                let control = &device.bridges().control;
                control.hid_reports_sent() + control.reports_waiting() as u32
            };
            let before = built(&device);
            let start = Instant::now();
            device.step();
            let took = start.elapsed();
            total += took;
            let reports = built(&device) - before;
            if reports > 0 || took.as_micros() > 300 {
                eprintln!(
                    "round {round} pass {pass}: {took:?} reports={reports} {}",
                    device.last_units(std::time::Duration::from_micros(20))
                );
            }
        }
        eprintln!("round {round}: {total:?} in 200 passes");
    }
}

/// A3: at QC scale the connect-burst File and CloudProduct READs go out over many passes. A
/// pass sends at most one File folder, and a folder of N library reads takes at least
/// N / [`READS_PER_PASS`] passes, so no pass reads more than that (the QC held one pass for
/// 2.6 s). The replies keep the request order: the File reply ends before the CloudProduct
/// reply starts, and the Downloads captures follow their transfer state push.
#[test]
fn qc_scale_library_reads_spread_over_passes() {
    use tetrad_app::device::control::READS_PER_PASS;
    let (mut device, log, _media) = connected(qc_scale_media());
    for (message_type, payload) in [&CONNECT_BURST[10], &CONNECT_BURST[12]] {
        assert!(matches!(message_type, 4 | 46));
        queue_raw(&mut device, *message_type, payload);
    }
    // Map each message to the pass that queued its last report (sent and waiting reports).
    let reports = |device: &Device<common::Sim>| {
        let control = &device.bridges().control;
        control.hid_reports_sent() + control.reports_waiting() as u32
    };
    let (mark, start) = (log.borrow().requests.len(), reports(&device));
    let ends: Vec<u32> = (0..200)
        .map(|_| {
            device.step();
            reports(&device) - start
        })
        .collect();
    assert!(!device.bridges().control.spreading());
    while device.bridges().control.transmitting() {
        device.step();
    }
    let mut passes = vec![Vec::new(); ends.len()];
    for (sent, message) in sent_framed(&log.borrow(), mark) {
        passes[ends.partition_point(|&end| end < sent)].push(message);
    }
    for pass in &passes {
        let folders = (pass.iter())
            .filter(|message| matches!(message, ControlMessage::File(_)))
            .count();
        assert!(folders <= 1, "one folder per pass: {folders}");
    }
    let file_passes: Vec<usize> = (passes.iter().enumerate())
        .filter(|(_, pass)| pass.iter().any(|m| matches!(m, ControlMessage::File(_))))
        .map(|(index, _)| index)
        .collect();
    // The estimate, 10 setlists, `cloud-0-1`, the capture root, `1_q`, `cloud-2-1`, the IR
    // root and `2_q`.
    assert_eq!(file_passes.len(), 17);
    let downloads = (passes.iter())
        .position(|pass| {
            files(pass).iter().any(|file| {
                file.folder.as_ref().and_then(|f| f.key.as_deref()) == Some("cloud-2-1")
            })
        })
        .expect("the Downloads captures");
    let before = file_passes[file_passes.iter().position(|&p| p == downloads).unwrap() - 1];
    assert!(
        downloads - before >= 389_usize.div_ceil(READS_PER_PASS),
        "389 rows over {} passes",
        downloads - before
    );
    let [
        ..,
        ControlMessage::CloudProduct(push),
        ControlMessage::File(_),
    ] = &passes[downloads][..]
    else {
        panic!(
            "the transfer state push, then the folder: {:?}",
            passes[downloads]
        );
    };
    assert_eq!(push.cloud_transfer_state.as_ref().unwrap().items.len(), 389);
    let reply = (passes.iter())
        .position(|pass| {
            pass.iter().any(|m| {
                matches!(m, ControlMessage::CloudProduct(c)
                    if c.action == Some(MessageAction::Delete.into()))
            })
        })
        .expect("the CloudProduct READ reply");
    let last_file = *file_passes.last().unwrap();
    // 72 setlist presets, 42 captures and 389 Downloads rows.
    assert!(reply - last_file >= (72 + 42 + 389_usize).div_ceil(READS_PER_PASS));
    let [
        ControlMessage::CloudProduct(_),
        ControlMessage::CloudProduct(update),
    ] = &passes[reply][..]
    else {
        panic!("DELETE, then UPDATE: {:?}", passes[reply]);
    };
    // Every Downloads capture by cloud id; each library capture by key and by its cloud id,
    // which here is the id of a Downloads capture too.
    assert_eq!(
        update.cloud_transfer_state.as_ref().unwrap().items.len(),
        389 + 42
    );
}

/// Item 2 (r53): a device-side drop (here an injected `Action::DropBlock`; a touch drop comes
/// as `Effect::Action`, the same hook) sends Cortex Control the GridMove echo of
/// `Chain::dropSlot` (`0x59f620`) in the same pass, with no request ID.
#[test]
fn a_device_drop_sends_the_grid_move_echo() {
    use tetrad_app::device::actions::Action;
    use tetrad_control::codecs::{GridMoveElement, GridMoveMessage};
    use tetrad_engine::{Column, DropSide, Row, Slot};
    let (mut device, log, _media) = connected(s7_media().0);
    let slot = |column| Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(column).unwrap(),
    };
    let mark = log.borrow().requests.len();
    device.push(tetrad_app::device::Event::Action(Action::DropBlock {
        from: slot(2),
        to: slot(5),
        side: DropSide::Left,
    }));
    (0..20).for_each(|_| _ = device.step());
    let echo = GridMoveMessage {
        moves: vec![GridMoveElement {
            from_col: Some(2),
            to_col: Some(5),
            is_drop: Some(true),
            ..Default::default()
        }],
        ..Default::default()
    };
    let sent: Vec<_> = (sent_framed(&log.borrow(), mark).into_iter())
        .map(|(_, message)| message)
        .filter(|message| matches!(message, ControlMessage::GridMove(_)))
        .collect();
    assert_eq!(sent, [ControlMessage::GridMove(echo)]);
}

/// A3: the bring-up `SLOW` line names the costliest unit of each phase: the Control message
/// and action in `reduce`, or the READ part and folder of a spread reply (the effect kinds of
/// `apply` are in `probe.rs`).
#[test]
fn a_pass_names_its_costliest_units() {
    use std::time::Duration;
    let (mut device, _log, _media) = connected(qc_scale_media());
    queue_raw(&mut device, 35, &[0x08, 3]);
    device.step();
    let units = device.last_units(Duration::ZERO);
    assert!(
        units.contains("reduce: Control ModuleStats Read"),
        "{units}"
    );
    queue_raw(&mut device, 4, &[0x08, 3, 0x58, 1]);
    device.step();
    device.step();
    let units = device.last_units(Duration::ZERO);
    assert!(units.contains("reduce: File Read part estimate"), "{units}");
}

/// A4: a connected Cortex Control at idle costs no frame. After the connect burst (the
/// subscriptions of the grid and I/O meters included), 10 s of keep-alives, row meter records,
/// SOC2 ARM blocks and the 1 s pushes render nothing on the grid (the QC rendered about 45
/// frames per 10 s), while the Grid meter (10/s) and CPULoad pushes still go to Control.
#[test]
fn a_connected_control_at_idle_costs_no_frame() {
    use std::time::Duration;
    use tetrad_app::device::Event;
    use tetrad_dsp_host::responses::Event as DspEvent;
    use tetrad_dsp_host::session::Clock;
    use tetrad_dsp_protocol::responses::{MeterRecord, Soc2ArmDiagnostics};
    use tetrad_dsp_protocol::{CoreId, DspCore};
    let (mut device, log, _media) = connected(s7_media().0);
    for (message_type, payload) in CONNECT_BURST {
        queue_raw(&mut device, message_type, payload);
    }
    // Sim time moves only while the device idles. Each second brings a host keep-alive, a
    // meter record per row and a SOC2 ARM block whose HID counter moved.
    let idle = |device: &mut Device<common::Sim>, seconds: u64| {
        let now = || device.platform().clock.now();
        let (until, mut next) = (now() + Duration::from_secs(seconds), now());
        while device.platform().clock.now() < until {
            if device.platform().clock.now() >= next {
                next += Duration::from_secs(1);
                queue_raw(device, 32, &[0x08, 1, 0x10, 0]);
                for row in 0..4 {
                    let meter = MeterRecord {
                        core: 0,
                        column: 1,
                        index: 0x401c + row,
                        value_bits: (-80.0_f32).to_bits(),
                    };
                    let core = CoreId::Dsp(DspCore::Core0);
                    device.push(Event::Dsp(core, DspEvent::Meter(meter)));
                }
                let mut block = [0_u8; 128];
                block[0] = 1;
                let sent = device.bridges().control.hid_reports_sent();
                block[0x78..0x7c].copy_from_slice(&sent.to_le_bytes());
                let block = DspEvent::Soc2ArmDiagnostics(Soc2ArmDiagnostics(block));
                device.push(Event::Dsp(CoreId::Control, block));
            }
            device.step();
        }
    };
    idle(&mut device, 5);
    assert!(device.bridges().control.grid_meters_subscribed());
    assert!(device.bridges().control.io_meters_subscribed());
    assert_eq!(device.state().screen, tetrad_ui::Screen::Grid);
    device.take_renders();
    let mark = log.borrow().requests.len();
    idle(&mut device, 10);
    assert_eq!(device.take_renders(), 0);
    let pushed: Vec<_> = (sent(&log.borrow(), mark).iter())
        .map(ControlMessage::message_type)
        .collect();
    for kind in [CortexMessageType::Grid, CortexMessageType::CPULoad] {
        assert!(pushed.contains(&kind), "{kind:?} pushes: {pushed:?}");
    }
}

/// The stock capture metadata keys (`0x7efc38` type 2) on a capture of the given hash.
fn capture_metadata(name: &str, cloud: &str) -> Vec<u8> {
    format!(
        "{{\"authorId\": \"161cc91b\",\"authorName\": \"ToneJunkie\",\"cloudUUID\": \"{cloud}\",\"date\": \"1970-01-01T00:01:37Z\",\"description\": \"A pedal\",\"deviceType\": \"pedal\",\"gainType\": 3,\"instrumentType\": \"guitar\",\"name\": \"{name}\",\"tags\": [\"#klon\"]}}"
    )
    .into_bytes()
}

/// A library like the S7 capture QC, small: "My Presets" with one preset that has a cloud id,
/// one capture in the user root and one downloaded capture.
fn s7_media() -> (common::Media, String) {
    let light = common::light_preset(&common::STOCK.specs(), 2);
    let media = common::Media::new([("Light", light)]);
    let sidecar = "{\"author\": \"PlayerMusic\",\"cloudId\": \"cloud-light\",\"name\": \"Light\"}";
    fs::write(media.0.join("Presets/My Presets/Light.json"), sidecar).unwrap();
    let db = rusqlite::Connection::open(media.0.join("zenui.db")).unwrap();
    db.execute_batch(
        "INSERT INTO folder VALUES (1, 2, 1, 'My Captures', 0, 0);
         INSERT INTO folder VALUES (2, 1, 1, 'My IRs', 0, 0);",
    )
    .unwrap();
    let hash = "00c05b0dcfeb".repeat(5);
    db.execute(
        "INSERT INTO product VALUES (?1, 2, 0, x'00', ?2)",
        rusqlite::params![hash, capture_metadata("Klon", "cloud-klon")],
    )
    .unwrap();
    db.execute("INSERT INTO folder_content VALUES (1, ?1)", [&hash])
        .unwrap();
    db.execute(
        "INSERT INTO downloads_product_nc VALUES ('row-id', 'dl-hash', x'00', ?1)",
        [capture_metadata("JTM", "cloud-jtm")],
    )
    .unwrap();
    (media, hash)
}

/// The messages the device sent from request `mark` on, inflated, in send order. Scene (13)
/// has its own codec and is left out.
fn sent(log: &common::Log, mark: usize) -> Vec<ControlMessage> {
    (sent_framed(log, mark).into_iter())
        .map(|(_, message)| message)
        .collect()
}

/// [`sent`], each message with the count of HID reports from `mark` to its last one.
fn sent_framed(log: &common::Log, mark: usize) -> Vec<(u32, ControlMessage)> {
    use std::io::Read;
    use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
    let mut reassembler = tetrad_control::Reassembler::new(Limits::default());
    let limits = tetrad_control::link::LinkLimits::default().payload;
    (log.requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x27 && sent.frame[2] == 0)
        .zip(1..)
        .filter_map(|(sent, reports)| {
            reassembler
                .push(&sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES])
                .unwrap()
                .map(|envelope| (reports, envelope))
        })
        .filter(|(_, envelope)| envelope.message_type != 13)
        .map(|(reports, mut envelope)| {
            if envelope.decompress != 0 {
                let mut plain = Vec::new();
                flate2::read::GzDecoder::new(&envelope.payload[..])
                    .read_to_end(&mut plain)
                    .unwrap();
                envelope.payload = plain;
                envelope.decompress = 0;
            }
            (
                reports,
                ControlMessage::decode(&envelope, limits).unwrap().message,
            )
        })
        .collect()
}

fn files(sent: &[ControlMessage]) -> Vec<&FileMessage> {
    (sent.iter())
        .filter_map(|message| match message {
            ControlMessage::File(file) => Some(file),
            _ => None,
        })
        .collect()
}

/// D3 (S7 capture `[50]`–`[92]`): a setlist lists all 256 slots, an empty one as `{index,
/// is_readonly}`; the default setlist and the `zenui.db` user roots are `is_user_default`; the
/// media trees are the library roots ("Captures Library" with every field, "IRs Library") with
/// the user roots `<id>_q` below them (keys only); a capture carries `author`, `gain`,
/// `cloud_id` and no `description`; a downloaded capture is keyed by its hash and its folder
/// sends no name or other flags; the estimate is the `0x24fb00` sum. D4: the Downloads
/// listing pushes its cloud ids as Downloaded.
#[test]
fn file_read_matches_the_stock_listing() {
    use tetrad_control::codecs::{CloudTransferItemState, ProductData};
    let (media, hash) = s7_media();
    let (mut device, log, _media) = connected(media);
    let mark = log.borrow().requests.len();
    host(
        &mut device,
        FileMessage {
            action: read(),
            omit_factory_content: Some(true),
            ..Default::default()
        },
    );
    let sent = sent(&log.borrow(), mark);
    let files = files(&sent);
    // Setlist presets 1, captures 1, Downloads captures 1, a third of 1 link.
    assert_eq!(files[0].user_content_estimate, Some(3));
    let folder = |key: &str| {
        (files.iter())
            .find_map(|file| {
                file.folder
                    .as_ref()
                    .filter(|f| f.key.as_deref() == Some(key))
            })
            .unwrap_or_else(|| panic!("no folder {key}"))
    };
    let setlist = folder(common::SETLIST);
    assert_eq!(
        (setlist.files.len(), setlist.is_user_default),
        (256, Some(true))
    );
    assert_eq!(setlist.files[0].name.as_deref(), Some("Light"));
    assert_eq!(
        setlist.files[1],
        ProductData {
            index: Some(1),
            is_readonly: Some(false),
            ..Default::default()
        }
    );

    let library = folder("local_nc_root");
    assert_eq!(
        (library.name.as_deref(), library.is_user_default),
        (Some("Captures Library"), Some(false))
    );
    let [capture] = &library.files[..] else {
        panic!("one capture: {library:?}");
    };
    assert_eq!(
        capture,
        &ProductData {
            key: Some(hash.clone()),
            name: Some("Klon".into()),
            author: Some("ToneJunkie".into()),
            author_id: Some("161cc91b".into()),
            tags: vec!["#klon".into()],
            gain: Some(3),
            cloud_id: Some("cloud-klon".into()),
            instrument: Some(1),
            device: capture.device,
            is_readonly: Some(false),
            version: Some("1".into()),
            date_ms_since_epoch: Some(97_000),
            ..Default::default()
        }
    );
    let user_root = folder("1_q");
    assert_eq!(
        (
            user_root.parent_key.as_deref(),
            user_root.name.as_deref(),
            user_root.is_user_default
        ),
        (Some("local_nc_root"), Some("My Captures"), Some(true))
    );
    assert_eq!(user_root.files[0].key, Some(hash));
    assert_eq!(user_root.files[0].name, None);
    assert_eq!(folder("local_ir_root").name.as_deref(), Some("IRs Library"));
    assert_eq!(folder("2_q").parent_key.as_deref(), Some("local_ir_root"));

    let downloads = folder("cloud-2-1");
    assert_eq!(
        (
            downloads.name.as_deref(),
            downloads.is_downloads,
            downloads.is_user_default,
            downloads.is_plugin
        ),
        (None, Some(true), None, None)
    );
    assert_eq!(downloads.files[0].key.as_deref(), Some("dl-hash"));
    assert_eq!(downloads.files[0].cloud_id.as_deref(), Some("cloud-jtm"));

    let pushes: Vec<_> = (sent.iter())
        .filter_map(|message| match message {
            ControlMessage::CloudProduct(push) => push.cloud_transfer_state.as_ref(),
            _ => None,
        })
        .collect();
    let [state] = &pushes[..] else {
        panic!("one transfer state push: {pushes:?}");
    };
    let items: Vec<_> = (state.items.iter())
        .map(|item| (item.id.as_deref().unwrap(), item.state))
        .collect();
    let downloaded = Some(CloudTransferItemState::Downloaded.into());
    assert_eq!(items, [("cloud-jtm", downloaded)]);
}

/// D4 (S7 capture `[95]`, `[96]`): the CloudProduct READ answers DELETE, then UPDATE with the
/// transfer state map in key order: every Downloads row by cloud id and each library capture
/// and setlist preset with a cloud id by its key and by that id, all Downloaded.
#[test]
fn cloud_product_read_lists_every_stored_product_with_a_cloud_id() {
    use tetrad_control::codecs::CloudTransferItemState;
    let (media, hash) = s7_media();
    let (mut device, log, _media) = connected(media);
    let mark = log.borrow().requests.len();
    host(
        &mut device,
        CloudProductMessage {
            action: read(),
            cloud_transfer_state: Some(Default::default()),
            ..Default::default()
        },
    );
    let sent = sent(&log.borrow(), mark);
    let [
        ControlMessage::CloudProduct(delete),
        ControlMessage::CloudProduct(update),
    ] = &sent[..]
    else {
        panic!("DELETE and UPDATE: {sent:?}");
    };
    assert_eq!(delete.action, Some(MessageAction::Delete.into()));
    let items: Vec<_> = (update.cloud_transfer_state.as_ref().unwrap().items.iter())
        .map(|item| {
            assert_eq!(item.state, Some(CloudTransferItemState::Downloaded.into()));
            item.id.clone().unwrap()
        })
        .collect();
    let light = format!("{}/Light.pb", common::SETLIST);
    let mut expected = [
        light.as_str(),
        &hash,
        "cloud-jtm",
        "cloud-klon",
        "cloud-light",
    ];
    expected.sort_unstable();
    assert_eq!(items, expected);
}

/// D6, D13 (S7 capture `[33]`, IOSettings): GeneralSettings sends `power_option` WakeUp
/// (Standby while Be Right Back runs), the database default download destinations (`1_q`
/// captures, `2_q` IRs), no false bypass rows, and the `/media/p4` space of the platform: the
/// capture's sizes give its fields 17/18 (25, 27 GiB) and the `storage_info` kB, with the
/// item limits and this library's counts. IOSettings has the USB port plugged, both EXP
/// `calibrating` flags, and the jack `plugged` flags of the HAL word: output 2 needs 6 and 7,
/// outputs 4, 5, 1 have none, and a READ that lists output 2 gets false (`0x2683cc`).
#[test]
fn settings_and_io_reads_match_the_stock_capture() {
    use tetrad_control::codecs::{
        GeneralSettingsMessage, GlobalBypassRows, IOSettingsMessage, OutputPortSettings,
        PortSettings, PowerOptions, StorageInfo, StorageInfoEntity, StorageInfoType,
    };
    use tetrad_hal::zenjack::{Jack, JackStatus};
    let (media, _) = s7_media();
    let plugged = [Jack::Input1, Jack::Output6, Jack::Output7, Jack::Headphones];
    let (mut device, log, _media) = connected_with(media, |sim| {
        sim.disk_space = Some(tetrad_sys::fs::FsSpace {
            total: 28_371_548 << 10,
            available: 26_682_928 << 10,
        });
        sim.inject_hal(tetrad_hal::hal::Event::Jacks(JackStatus {
            changed: 0,
            // Input 1 by its second switch bit only (`0x67ab94` bit 2).
            plugged: plugged.iter().map(|jack| jack.mask()).sum::<u16>() & !1,
        }));
    });
    let mark = log.borrow().requests.len();
    let general_read = GeneralSettingsMessage {
        action: read(),
        ..Default::default()
    };
    host(&mut device, general_read.clone());
    host(
        &mut device,
        IOSettingsMessage {
            action: read(),
            ..Default::default()
        },
    );
    let output_2 = PortSettings {
        out_port: vec![OutputPortSettings {
            output_port_id: Some(2),
            ..Default::default()
        }],
        ..Default::default()
    };
    host(
        &mut device,
        IOSettingsMessage {
            action: read(),
            settings: Some(output_2),
            ..Default::default()
        },
    );
    let sent = sent(&log.borrow(), mark);
    let [
        ControlMessage::GeneralSettings(general),
        ControlMessage::IOSettings(io),
        ControlMessage::IOSettings(asked),
    ] = &sent[..]
    else {
        panic!("three replies: {sent:?}");
    };
    assert_eq!(general.power_option, Some(PowerOptions::WakeUp.into()));
    let destinations: Vec<_> = (general.download_destination_folders.iter())
        .map(|folder| (folder.key.as_deref().unwrap(), folder.folder_type.unwrap()))
        .collect();
    assert_eq!(destinations, [("1_q", 2), ("2_q", 1)]);
    assert_eq!(general.global_bypass_cab, Some(GlobalBypassRows::default()));
    assert_eq!(general.available_disk_space, Some(25.0));
    assert_eq!(general.total_disk_space, Some(27.0));
    let entity = |kind, maximum, current| StorageInfoEntity {
        entity_type: kind,
        size_maximum: Some(maximum),
        size_current: current,
    };
    let captures = Some(StorageInfoType::NeuralCaptures.into());
    let irs = Some(StorageInfoType::ImpulseResponses.into());
    assert_eq!(
        general.storage_info,
        Some(StorageInfo {
            total_storage_size_kb: Some(28_371_548),
            free_storage_size_kb: Some(26_682_928),
            storage_entities: vec![
                entity(None, 3072, Some(1)),
                entity(captures, 2048, Some(1)),
                entity(irs, 2048, None),
            ],
        })
    );

    let ports = io.settings.as_ref().unwrap();
    assert_eq!(ports.usb_port.as_ref().unwrap().plugged, Some(true));
    assert!(
        ports
            .exp_port
            .iter()
            .all(|port| port.calibrating == Some(false))
    );
    let inputs: Vec<_> = (ports.in_port.iter())
        .map(|port| (port.input_port_id.unwrap(), port.plugged))
        .collect();
    assert_eq!(
        inputs,
        [
            (1, Some(true)),
            (2, Some(false)),
            (4, Some(false)),
            (5, Some(false))
        ]
    );
    let outputs: Vec<_> = (ports.out_port.iter())
        .map(|port| (port.output_port_id.unwrap(), port.plugged))
        .collect();
    let (on, off) = (Some(true), Some(false));
    assert_eq!(
        outputs,
        [
            (4, None),
            (5, None),
            (1, None),
            (6, on),
            (7, on),
            (2, on),
            (8, off),
            (9, off)
        ]
    );
    assert_eq!(ports.hp_port.as_ref().unwrap().plugged, on);
    let asked = asked.settings.as_ref().unwrap();
    assert_eq!(asked.out_port.len(), 1);
    assert_eq!(asked.out_port[0].plugged, off);

    // Be Right Back from Control (`power_option` Standby): the READ reports it.
    host(
        &mut device,
        GeneralSettingsMessage {
            action: Some(MessageAction::Update.into()),
            power_option: Some(PowerOptions::Standby.into()),
            ..Default::default()
        },
    );
    let mark = log.borrow().requests.len();
    host(&mut device, general_read);
    let standby = self::sent(&log.borrow(), mark);
    let [ControlMessage::GeneralSettings(general)] = &standby[..] else {
        panic!("one reply: {standby:?}");
    };
    assert_eq!(general.power_option, Some(PowerOptions::Standby.into()));
}

/// D8 (S7 capture `[121]`): the Looper READ writes `quantize_enabled`, a presence field
/// (`+0xb882`, writers `0x513df0`/`0x513dbc`/`0x513e24`): false without a Looper X.
#[test]
fn looper_read_writes_quantize_enabled() {
    use tetrad_control::codecs::LooperMessage;
    use tetrad_dsp_protocol::responses::{ResponseType, encode};
    use tetrad_dsp_protocol::{CoreId, DspCore};
    let (mut device, log, _media) = connected(common::Media::new([(
        "Light",
        common::light_preset(&common::STOCK.specs(), 2),
    )]));
    let rings = &device.platform().clock.rings;
    common::publish(
        &mut rings.0.borrow_mut(),
        CoreId::Dsp(DspCore::Core3),
        &encode(ResponseType::LooperStatus, 0, &[0; 88]),
    );
    (0..50).for_each(|_| _ = device.step());
    let mark = log.borrow().requests.len();
    host(
        &mut device,
        LooperMessage {
            action: read(),
            ..Default::default()
        },
    );
    let sent = sent(&log.borrow(), mark);
    let replies: Vec<_> = (sent.iter())
        .filter_map(|message| match message {
            ControlMessage::Looper(reply) if reply.status.is_some() => Some(reply),
            _ => None,
        })
        .collect();
    let [reply] = &replies[..] else {
        panic!("one Looper reply: {sent:?}");
    };
    assert_eq!(reply.quantize_enabled, Some(false));
}
