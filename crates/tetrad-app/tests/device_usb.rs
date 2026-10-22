//! The Tetrad link over USB (`device/usb.rs`, `docs/usb.md`): requests arrive as DSP HID
//! records, replies leave as `0x27` frames, and Cortex Control never sees Tetrad reports.

mod common;

use tetrad_dsp_protocol::hid::{HEADER_BYTES, REPORT_BYTES};
use tetrad_dsp_protocol::responses::{ResponseType, encode};
use tetrad_dsp_protocol::{CoreId, DspCore};
use tetrad_usb::{Answer, Op, Reassembler, Reply, Request};

fn ask(device: &mut tetrad_app::device::Device<common::Sim>, id: u32, op: Op) {
    let message = tetrad_usb::encode(&Request { id, op }, &[]);
    let rings = &device.platform().clock.rings;
    for report in tetrad_usb::reports(0, &message) {
        common::publish(
            &mut rings.0.borrow_mut(),
            CoreId::Dsp(DspCore::Core3),
            &encode(ResponseType::HidReport, 0, &report),
        );
    }
    (0..50).for_each(|_| _ = device.step());
}

/// The Tetrad replies in the `0x27` frames sent from request `mark` on, with their bodies.
fn replies(log: &common::Log, mark: usize) -> Vec<(Reply, Vec<u8>)> {
    let mut reassembler = Reassembler::new(1 << 20);
    (log.requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x27)
        .map(|sent| &sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES])
        .filter(|report| tetrad_usb::is_tetrad(report))
        .filter_map(|report| reassembler.push(report).unwrap())
        .map(|(_, message)| {
            let (reply, body) = tetrad_usb::decode::<Reply>(&message).unwrap();
            (reply, body.to_vec())
        })
        .collect()
}

/// Hello names the running Tetrad; Screenshot is a PNG of the UI; each reply keeps its id.
#[test]
fn hello_and_screenshot_are_answered() {
    let (mut device, log, _media) = common::boot(common::Media::new([]));
    let mark = log.borrow().requests.len();
    ask(&mut device, 1, Op::Hello);
    ask(&mut device, 2, Op::Screenshot);
    let replies = replies(&log.borrow(), mark);
    let [(hello, _), (screenshot, png)] = &replies[..] else {
        panic!("two replies: {replies:?}");
    };
    assert_eq!(
        hello,
        &Reply {
            id: 1,
            answer: Answer::Hello {
                tetrad: env!("CARGO_PKG_VERSION").into(),
                protocol: tetrad_usb::PROTOCOL,
            },
        }
    );
    assert_eq!(screenshot.id, 2);
    assert!(matches!(screenshot.answer, Answer::Screenshot { .. }));
    assert!(png.starts_with(b"\x89PNG"));
    // No Cortex Control frame: the link never saw the Tetrad reports.
    let stock = (log.borrow().requests[mark..].iter())
        .filter(|sent| sent.kind() == 0x27)
        .filter(|sent| {
            !tetrad_usb::is_tetrad(&sent.frame[HEADER_BYTES..HEADER_BYTES + REPORT_BYTES])
        })
        .count();
    assert_eq!(stock, 0);
}

/// An install needs the release key's signature; without one nothing is staged, and a chunk
/// is refused.
#[test]
fn an_unsigned_install_is_refused() {
    let (mut device, log, _media) = common::boot(common::Media::new([]));
    let mark = log.borrow().requests.len();
    ask(
        &mut device,
        5,
        Op::InstallBegin {
            manifest: r#"{"version":"9.9.9","files":{}}"#.into(),
            signature: "AAAA".into(),
        },
    );
    ask(
        &mut device,
        6,
        Op::InstallChunk {
            path: "bin/tetrad".into(),
            offset: 0,
        },
    );
    let replies = replies(&log.borrow(), mark);
    let [(begin, _), (chunk, _)] = &replies[..] else {
        panic!("two replies: {replies:?}");
    };
    let error = |reply: &Reply| match &reply.answer {
        Answer::Error { message } => message.clone(),
        other => panic!("not an error: {other:?}"),
    };
    assert!(error(begin).contains("tetrad-update.json.sig"), "{begin:?}");
    assert_eq!(error(chunk), "no install-begin");
}
