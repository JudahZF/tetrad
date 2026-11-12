//! Remote Update channel (docs/remote.md, docs/updates.md): the app and the page read and set
//! the channel of Tetrad Updates through the same path as the buttons on the unit.

mod common;

use common::{FakeRemote, Media, STOCK, boot_remote, light_preset};
use serde_json::json;
use tetrad_app::device::Event;
use tetrad_app::device::remote::SETTINGS_FILE;
use tetrad_app::device::services::CHANNEL_FILE;
use tetrad_remote::{Call, Reply, Request, UpdatesOp};
use tetrad_services::fake::MemFiles;

/// No `updates.json` is Stable. Setting Nightly saves it and answers with the new channel.
#[test]
fn remote_reads_and_sets_the_update_channel() {
    let files = MemFiles::default();
    files.put(SETTINGS_FILE, r#"{"enabled":true}"#);
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    let mut device = boot_remote(media, files.clone(), FakeRemote::default()).0;
    let mut ask = |op| {
        let id = device.platform().remote.as_ref().unwrap().replies.len() as u64 + 1;
        let request = Request::Updates {
            pin: "0001".into(),
            op,
        };
        device.push(Event::Remote(Call { id, request }));
        device.step();
        let remote = device.platform().remote.as_ref().unwrap();
        remote.replies.last().expect("a reply").1.clone()
    };
    let version = env!("CARGO_PKG_VERSION");
    // A nightly build runs these tests with its own version.
    let nightly = version.contains("-nightly.");
    let view = |channel| {
        Reply::Json(json!({ "channel": channel, "version": version, "nightly": nightly }))
    };
    assert_eq!(ask(UpdatesOp::View), view("stable"));
    assert_eq!(
        ask(UpdatesOp::SetChannel { nightly: true }),
        view("nightly")
    );
    assert_eq!(
        files.get(CHANNEL_FILE).as_deref(),
        Some(&br#"{"channel":"nightly"}"#[..])
    );
    assert_eq!(
        ask(UpdatesOp::SetChannel { nightly: false }),
        view("stable")
    );
}
