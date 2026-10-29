//! Remote's Backup page (docs/remote.md): a backup is packed on the bulk worker and read in
//! pieces; a backup that the page sends waits for RESTORE on the unit's Remote page. The
//! results of these jobs go to Remote only, and those of Cortex Control backups to Control only.
//! The worker is simulated: the tests answer its jobs.

mod common;

use common::{FakeRemote, Media, STOCK, Sim, boot_remote, light_preset, target};
use serde_json::Value;
use tetrad_app::device::platform::{Input, TouchSample};
use tetrad_app::device::services::{
    BackupDone, JobError, REMOTE_ARCHIVE, REMOTE_RESTORE, ServiceNotice,
};
use tetrad_app::device::ui::UiInput;
use tetrad_app::device::{Device, Event};
use tetrad_remote::{BackupOp, Call, Reply, Request};
use tetrad_services::backup::{BackupError, Summary};
use tetrad_services::fake::MemFiles;
use tetrad_ui::{Control, DialogButton, RemoteView, Screen, SettingsPage};

const PIN: &str = "0001";
/// A piece as the page sends it.
const PIECE: usize = 48 * 1024;

/// A device with Remote on (PIN [`PIN`]) and the bulk worker held: jobs wait for the test.
fn boot(files: &MemFiles) -> Device<Sim> {
    let media = Media::new([("Light", light_preset(&STOCK.specs(), 0))]);
    let (mut device, _media) = boot_remote(media, files.clone(), FakeRemote::default());
    tap(&mut device, Control::RemoteToggle);
    device.platform().worker.set(true);
    device
}

fn tap(device: &mut Device<Sim>, control: Control) {
    device.push(Event::UiInput(UiInput::Show(Screen::Settings(
        SettingsPage::Remote,
    ))));
    device.step();
    touch(device, control);
}

/// Touches `control` on the frame that shows now.
fn touch(device: &mut Device<Sim>, control: Control) {
    let tap = target(&device.bridges().ui, control);
    for pressed in [true, false] {
        device.push(Event::UiInput(UiInput::Touch(TouchSample {
            pressed,
            ..tap
        })));
        device.step();
    }
}

fn ask(device: &mut Device<Sim>, op: BackupOp) -> Reply {
    let remote = device.platform().remote.as_ref().unwrap();
    let id = remote.replies.len() as u64 + 1;
    let pin = PIN.into();
    device.push(Event::Remote(Call {
        id,
        request: Request::Backup { pin, op },
    }));
    device.step();
    let remote = device.platform().remote.as_ref().unwrap();
    let (replied, reply) = remote.replies.last().expect("a reply").clone();
    assert_eq!(replied, id);
    reply
}

/// The answer of `op`, which must be the status JSON.
fn status_after(device: &mut Device<Sim>, op: BackupOp) -> Value {
    match ask(device, op) {
        Reply::Json(status) => status,
        other => panic!("a status, not {other:?}"),
    }
}

fn state(device: &mut Device<Sim>) -> Value {
    status_after(device, BackupOp::Status)
}

fn refused(reply: Reply) -> String {
    match reply {
        Reply::Refused(reason) => reason,
        other => panic!("a refusal, not {other:?}"),
    }
}

fn view(device: &Device<Sim>) -> RemoteView {
    device.bridges().ui.view().snapshot().remote.clone()
}

/// Answers the job that the worker holds, then lets the device run its effects.
fn finish_job(device: &mut Device<Sim>, done: BackupDone) {
    end_job(device, Ok(done));
}

fn end_job(device: &mut Device<Sim>, result: Result<BackupDone, JobError>) {
    let notice = Input::Service(ServiceNotice::Backup(result));
    device.platform().inputs.borrow_mut().push(notice);
    for _ in 0..5 {
        device.step();
    }
}

fn created() -> BackupDone {
    BackupDone::Created {
        summary: Summary {
            files: 1,
            dirs: 0,
            bytes: 1,
        },
        notes: Vec::new(),
    }
}

/// A gzip header and then made-up bytes.
fn archive(len: usize) -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let mut bytes = vec![0x1f, 0x8b];
    bytes.extend((2..len).map(|_| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 24) as u8
    }));
    bytes
}

/// Sends `bytes` as the page does: pieces of 48 KiB, the last with `last`.
fn send(device: &mut Device<Sim>, name: &str, bytes: &[u8]) -> Reply {
    let mut reply = Reply::Busy;
    for (index, piece) in bytes.chunks(PIECE).enumerate() {
        let offset = (index * PIECE) as u64;
        reply = ask(
            device,
            BackupOp::Upload {
                name: name.into(),
                offset,
                last: offset as usize + piece.len() == bytes.len(),
                bytes: piece.to_vec(),
            },
        );
    }
    reply
}

/// Opens a Cortex Control backup receive and sends its one chunk: the restore job waits on the
/// worker.
fn start_control_restore(device: &mut Device<Sim>) {
    use tetrad_control::codecs::{LocalBackupMessage, MessageAction};
    common::connect_control(device);
    let update = || Some(MessageAction::Update.into());
    let open = LocalBackupMessage {
        action: update(),
        can_apply_backup: Some(true),
        ..Default::default()
    };
    let chunk = LocalBackupMessage {
        action: update(),
        backup_json: Some("{}".into()),
        is_last_chunk: Some(true),
        ..Default::default()
    };
    common::host_send(device, open);
    common::host_send(device, chunk);
}

/// Create packs on the worker, a second create waits for it, and the packed backup reads back
/// in pieces of at most 512 KiB that make the archive. Discard removes it.
#[test]
fn a_packed_backup_reads_back_in_pieces() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    assert_eq!(state(&mut device)["state"], "idle");
    assert_eq!(
        status_after(&mut device, BackupOp::Create)["state"],
        "creating"
    );
    let again = refused(ask(&mut device, BackupOp::Create));
    assert_eq!(again, "The unit is packing a backup");

    let packed = archive(1_200_000);
    files.put(REMOTE_ARCHIVE, packed.clone());
    finish_job(&mut device, created());
    let ready = state(&mut device);
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["size"], packed.len());
    assert_eq!(ready["name"], "QC-Backup-20261010-1234.tar.gz");

    let mut read = Vec::new();
    while read.len() < packed.len() {
        let offset = read.len() as u64;
        let Reply::Bytes(piece) = ask(&mut device, BackupOp::Read { offset }) else {
            panic!("a piece at {offset}");
        };
        assert!(!piece.is_empty() && piece.len() <= 512 * 1024);
        read.extend(piece);
    }
    assert_eq!(read, packed);
    let past = packed.len() as u64 + 1;
    refused(ask(&mut device, BackupOp::Read { offset: past }));

    assert_eq!(
        status_after(&mut device, BackupOp::Discard)["state"],
        "idle"
    );
    assert_eq!(files.get(REMOTE_ARCHIVE), None);
}

/// A piece that does not start where the received bytes end is refused, and the upload goes on
/// from the right offset; the complete gzip file shows on the unit's Remote page. A file that is
/// not gzip is refused at its last piece and removed.
#[test]
fn an_upload_follows_on_and_shows_on_the_unit() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    let sent = archive(100_000);
    let first = BackupOp::Upload {
        name: "My backup.tar.gz".into(),
        offset: 0,
        last: false,
        bytes: sent[..PIECE].to_vec(),
    };
    assert_eq!(status_after(&mut device, first)["received"], PIECE);
    let wrong = BackupOp::Upload {
        name: "My backup.tar.gz".into(),
        offset: 1000,
        last: true,
        bytes: sent[1000..].to_vec(),
    };
    let reason = refused(ask(&mut device, wrong));
    assert!(reason.starts_with("Out of order"), "{reason}");
    let rest = BackupOp::Upload {
        name: "My backup.tar.gz".into(),
        offset: PIECE as u64,
        last: true,
        bytes: sent[PIECE..].to_vec(),
    };
    assert_eq!(status_after(&mut device, rest)["state"], "received");
    assert_eq!(files.get(REMOTE_RESTORE), Some(sent));
    let shown = view(&device).restore.expect("the backup on the unit");
    assert_eq!(
        (shown.name.as_str(), shown.size.as_str()),
        ("My backup.tar.gz", "98 KB")
    );
    assert!(!shown.restoring);
    let reason = refused(ask(&mut device, BackupOp::Create));
    assert!(reason.contains("discard it first"), "{reason}");

    let reason = refused(send(&mut device, "notes.tar.gz", b"plain text"));
    assert!(reason.contains("not a Quad Cortex backup"), "{reason}");
    assert_eq!(state(&mut device)["state"], "idle");
    assert_eq!(files.get(REMOTE_RESTORE), None);
    assert_eq!(view(&device).restore, None);
}

/// DISCARD on the unit removes a received backup with no job. RESTORE asks first, then runs
/// the restore job, which holds the library. A failed job gives the library back and offers
/// RESTORE again with the reason; a good one removes the file and reloads.
#[test]
fn discard_and_restore_on_the_unit() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    send(&mut device, "QC.tar.gz", &archive(PIECE));
    tap(&mut device, Control::RemoteDiscard);
    assert_eq!(state(&mut device)["state"], "idle");
    assert_eq!(files.get(REMOTE_RESTORE), None);
    assert_eq!(view(&device).restore, None);

    send(&mut device, "QC.tar.gz", &archive(2 * PIECE));
    tap(&mut device, Control::RemoteRestore);
    assert!(
        device.state().library.is_some(),
        "nothing before the confirm"
    );
    touch(&mut device, Control::Dialog(DialogButton::Primary));
    assert!(view(&device).restore.is_some_and(|backup| backup.restoring));
    assert!(
        device.state().library.is_none(),
        "the restore holds the library"
    );
    assert_eq!(state(&mut device)["state"], "restoring");
    refused(ask(&mut device, BackupOp::Discard));

    end_job(&mut device, Err(JobError::Backup(BackupError::Empty)));
    let failed = state(&mut device);
    assert_eq!(failed["state"], "received");
    assert_eq!(failed["error"], BackupError::Empty.to_string());
    assert!(device.state().library.is_some(), "the library reopens");
    // The notice of the restore start (a Library notice after a touch opens its dialog).
    touch(&mut device, Control::Dialog(DialogButton::Primary));
    tap(&mut device, Control::RemoteRestore);
    touch(&mut device, Control::Dialog(DialogButton::Primary));
    finish_job(&mut device, BackupDone::Restored(3));
    assert_eq!(state(&mut device)["state"], "restored");
    assert!(device.state().library.is_some(), "the reload gives it back");
    assert_eq!(files.get(REMOTE_RESTORE), None);
    assert_eq!(view(&device).restore, None);
}

/// A RESTORE confirmed for one backup does not restore another that the page sent while the
/// question was open: the unit asks for a new touch.
#[test]
fn a_restore_confirmed_for_a_replaced_backup_does_nothing() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    send(&mut device, "First.tar.gz", &archive(PIECE));
    tap(&mut device, Control::RemoteRestore);
    send(&mut device, "Second.tar.gz", &archive(2 * PIECE));
    touch(&mut device, Control::Dialog(DialogButton::Primary));
    assert!(device.state().library.is_some(), "no restore runs");
    let after = state(&mut device);
    assert_eq!(
        (&after["state"], &after["name"]),
        (&"received".into(), &"Second.tar.gz".into())
    );
}

/// One backup at a time, and each result to its owner: while a Cortex Control restore runs, a
/// Remote pack is refused, and the Control result leaves the Backup page as it was; while a
/// Remote pack runs, a Control restore is refused, and the pack still ends on the Backup page.
#[test]
fn remote_and_control_backups_keep_their_own_results() {
    let files = MemFiles::default();
    let mut device = boot(&files);
    start_control_restore(&mut device);
    assert!(device.state().library.is_none(), "the Control restore runs");
    let reason = refused(ask(&mut device, BackupOp::Create));
    assert_eq!(reason, "a backup is in progress");
    finish_job(&mut device, BackupDone::Restored(3));
    let after = state(&mut device);
    assert_eq!(
        (&after["state"], &after["error"]),
        (&"idle".into(), &Value::Null)
    );
    assert!(device.state().library.is_some());

    assert_eq!(
        status_after(&mut device, BackupOp::Create)["state"],
        "creating"
    );
    start_control_restore(&mut device);
    assert!(device.state().library.is_some(), "Control is refused");
    files.put(REMOTE_ARCHIVE, archive(1000));
    finish_job(&mut device, created());
    assert_eq!(state(&mut device)["state"], "ready");
}
