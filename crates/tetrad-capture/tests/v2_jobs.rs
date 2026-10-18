//! V2 cloud-job controller and Capture2 file contracts (ZenUI VA 0x202fd8, 0x20068c,
//! 0x1fe630, 0x200ddc, 0x1fef08, 0x1201bc, 0x6f2b60). Synthetic data only.

use tetrad_capture::session::SaveInfo;
use tetrad_capture::v2::{
    CORRUPT_DOWNLOAD, Capture2File, HTTP_TIMEOUT_MS, JobReport, JobStatus, Nc2Controller,
    Nc2Effect, Nc2Error, Nc2Input, Nc2Remote, Nc2Request, Nc2Route, Nc2State, Nc2Update,
    POLL_RETRY_MS, Product, SETUP_METER_MS,
};

fn controller_in(state: Nc2State) -> Nc2Controller {
    let mut controller = Nc2Controller::new("example.invalid");
    controller.apply(Nc2Input::SetState(state as i32)).unwrap();
    controller
}

fn reply(status: &str, product: &str) -> Nc2Input {
    Nc2Input::JobReply(format!(r#"{{"status":"{status}","productId":"{product}"}}"#).into_bytes())
}

fn polling(state: Nc2State) -> Nc2Controller {
    let mut controller = controller_in(state);
    controller
        .apply(Nc2Input::SetJobId("j1".to_owned()))
        .unwrap();
    controller
}

fn product() -> Product {
    Product {
        name: "Plexi".to_owned(),
        device: 1,
        instrument: 0,
    }
}

#[test]
fn job_reply_parsing_is_as_lenient_as_stock() {
    let report = JobReport::parse(br#"{"status":"running","progress":12,"id":"j1"}"#).unwrap();
    assert_eq!(report.status, JobStatus::Running);
    assert_eq!(report.progress, 12);
    assert_eq!(report.product_id, "");
    // Non-integral progress and a non-string status read as 0 and empty (VA 0xe84e0).
    let report = JobReport::parse(br#"{"status":7,"progress":0.5}"#).unwrap();
    assert_eq!(report.status, JobStatus::Other(String::new()));
    assert_eq!(report.progress, 0);
    assert_eq!(JobReport::parse(b"not json"), Err(Nc2Error::Json));
}

#[test]
fn poll_download_audition_and_save_follow_the_stock_order() {
    let mut controller = controller_in(Nc2State::Training);
    assert_eq!(
        controller
            .apply(Nc2Input::SetJobId("j1".to_owned()))
            .unwrap(),
        [Nc2Effect::GetJob {
            url: "https://example.invalid/capture/v1/jobs/j1".to_owned(),
            timeout_ms: HTTP_TIMEOUT_MS
        }]
    );
    // One request in flight at a time.
    assert!(controller.apply(Nc2Input::PollDue).unwrap().is_empty());
    assert_eq!(
        controller.apply(reply("running", "")).unwrap(),
        [Nc2Effect::SchedulePoll {
            delay_ms: POLL_RETRY_MS
        }]
    );
    controller.apply(Nc2Input::PollDue).unwrap();
    assert_eq!(
        controller.apply(reply("success", "p9")).unwrap(),
        [Nc2Effect::DownloadProduct {
            product_id: "p9".to_owned()
        }]
    );
    let effects = controller
        .apply(Nc2Input::Downloaded(Some(product())))
        .unwrap();
    assert_eq!(effects[0], Nc2Effect::LoadAudition);
    assert_eq!(controller.state(), Nc2State::AuditionCapture);

    // Action CREATE saves the auditioned product and echoes its save_info (VA 0x1fef08).
    assert_eq!(
        controller.apply(Nc2Input::Save).unwrap(),
        [
            Nc2Effect::SaveProduct {
                product_id: "p9".to_owned()
            },
            Nc2Effect::Remote(Nc2Remote::SaveInfo(SaveInfo {
                name: "Plexi".to_owned(),
                device: 1,
                instrument: 0
            })),
            Nc2Effect::DropAudition,
        ]
    );
    // `newCaptureSaved()` (VA 0x9728ac) holds the saved mode until the next state change.
    assert!(controller.saved());
    assert_eq!(
        controller.apply(Nc2Input::Save),
        Err(Nc2Error::NothingToSave)
    );
    assert!(controller.saved());
    controller.apply(Nc2Input::StartFromScratch).unwrap();
    assert!(!controller.saved());
}

#[test]
fn failures_follow_the_stock_retry_and_corrupt_rules() {
    // Transport failures retry only in Training while online; invalid JSON is a failure.
    let mut controller = polling(Nc2State::Recording);
    let failed = Nc2Input::JobFailed { online: true };
    assert!(controller.apply(failed.clone()).unwrap().is_empty());
    let mut controller = polling(Nc2State::Training);
    assert!(
        controller
            .apply(Nc2Input::JobFailed { online: false })
            .unwrap()
            .is_empty()
    );
    controller.apply(Nc2Input::PollDue).unwrap();
    assert_eq!(
        controller
            .apply(Nc2Input::JobReply(b"not json".to_vec()))
            .unwrap(),
        [Nc2Effect::SchedulePoll {
            delay_ms: POLL_RETRY_MS
        }]
    );
    // error/stopped stop polling with no state change.
    controller.apply(Nc2Input::PollDue).unwrap();
    assert!(controller.apply(reply("error", "")).unwrap().is_empty());
    assert_eq!(controller.state(), Nc2State::Training);

    // A success without productId ends like a corrupt download: error 1, then Setup.
    let mut controller = polling(Nc2State::Training);
    let effects = controller.apply(reply("success", "")).unwrap();
    assert_eq!(
        effects[0],
        Nc2Effect::Remote(Nc2Remote::Error {
            id: 1,
            text: CORRUPT_DOWNLOAD.to_owned()
        })
    );
    assert_eq!(controller.state(), Nc2State::Setup);
}

#[test]
fn abort_deletes_the_cloud_product_and_drops_the_audition() {
    let mut controller = polling(Nc2State::Training);
    controller.apply(reply("success", "p9")).unwrap();
    controller
        .apply(Nc2Input::Downloaded(Some(product())))
        .unwrap();
    let effects = controller.apply(Nc2Input::Abort).unwrap();
    assert_eq!(
        effects[..2],
        [
            Nc2Effect::DeleteProduct {
                url: "https://example.invalid/api/v1/user/products/p9".to_owned(),
                timeout_ms: HTTP_TIMEOUT_MS
            },
            Nc2Effect::DropAudition
        ]
    );
    assert_eq!(controller.state(), Nc2State::Abort);
    assert!(effects.contains(&Nc2Effect::NcOutSel(false)));
}

/// VA 0x1201bc: a state change hides the A/B dialog (vtable +0x68 = 0x3b8320) only when
/// it exists and the change is not 4↔5. Stock never runs the NC2 close 0x203ca4 here.
#[test]
fn state_change_with_the_dialog_hides_it_but_does_not_close() {
    let mut controller = controller_in(Nc2State::AuditionDut);
    assert!(controller.apply(Nc2Input::DialogShown).unwrap().is_empty());
    assert_eq!(
        controller.apply(Nc2Input::SetState(5)).unwrap(),
        [
            Nc2Effect::Route(Nc2Route::Reset),
            Nc2Effect::MeterTimer(None),
            Nc2Effect::Route(Nc2Route::AuditionCapture),
            Nc2Effect::Remote(Nc2Remote::State(Nc2State::AuditionCapture)),
        ]
    );
    let effects = controller.apply(Nc2Input::SetState(1)).unwrap();
    assert_eq!(effects.first(), Some(&Nc2Effect::HideAbDialog));
    assert!(!effects.contains(&Nc2Effect::EnableNc2Preset(false)));
    assert!(!effects.contains(&Nc2Effect::Remote(Nc2Remote::State(Nc2State::Idle))));
    assert_eq!(controller.state(), Nc2State::Setup);
    assert_eq!(
        controller.apply(Nc2Input::SetState(7)),
        Err(Nc2Error::State(7))
    );
}

/// Review L1: a repeated remote state must not close (NC2 preset off) and re-enter.
#[test]
fn remote_state_equal_to_the_current_state_is_ignored() {
    let mut controller = controller_in(Nc2State::Recording);
    assert!(controller.apply(Nc2Input::SetState(2)).unwrap().is_empty());
    assert_eq!(controller.state(), Nc2State::Recording);
}

#[test]
fn setup_entry_follows_the_stock_order() {
    // VA 0x202fd8 case 1: 0x520498(3,0,0), SetNCOutSel(1,…), 0x1fec1c, 0x1fed6c, timer.
    // Idle → Setup with no A/B dialog: nothing precedes the entry (no NC2 preset off).
    let effects = Nc2Controller::new("example.invalid")
        .apply(Nc2Input::SetState(1))
        .unwrap();
    assert_eq!(
        effects,
        [
            Nc2Effect::Route(Nc2Route::Reset),
            Nc2Effect::MeterTimer(None),
            Nc2Effect::Route(Nc2Route::SetupPrepare),
            Nc2Effect::NcOutSel(true),
            Nc2Effect::Route(Nc2Route::Setup),
            Nc2Effect::Route(Nc2Route::AuditionDut),
            Nc2Effect::MeterTimer(Some(SETUP_METER_MS)),
            Nc2Effect::Remote(Nc2Remote::State(Nc2State::Setup)),
        ]
    );
}

#[test]
fn cab_bypass_applies_and_echoes_only_a_change() {
    let mut controller = controller_in(Nc2State::Setup);
    let set = |bypass, current| Nc2Input::CabsimBypass { bypass, current };
    assert!(controller.apply(set(true, true)).unwrap().is_empty());
    assert_eq!(
        controller.apply(set(true, false)).unwrap(),
        [
            Nc2Effect::CabsimBypass(true),
            Nc2Effect::Remote(Nc2Remote::CabsimBypass(true))
        ]
    );
}

#[test]
fn io_parameters_apply_once_per_change_and_levels_only_echo() {
    let mut controller = controller_in(Nc2State::Setup);
    let set = |index, value| Nc2Input::Parameter { index, value };
    assert_eq!(
        controller.apply(set(5, 0.25)).unwrap()[0],
        Nc2Effect::IoParameter {
            index: 5,
            value: 0.25
        }
    );
    assert!(controller.apply(set(5, 0.25)).unwrap().is_empty());
    assert!(
        !controller
            .apply(set(6, 0.5))
            .unwrap()
            .iter()
            .any(|e| matches!(e, Nc2Effect::IoParameter { .. }))
    );
    assert_eq!(controller.apply(set(15, 1.0)), Err(Nc2Error::Parameter(15)));
}

#[test]
fn update_handles_only_the_first_present_field_in_stock_order() {
    let update = Nc2Update {
        job_id: Some("j1".to_owned()),
        parameters: vec![(2, 1.0), (3, 0.0)],
        start_from_scratch: true,
        ..Nc2Update::default()
    };
    assert_eq!(
        update.request(),
        Some(Nc2Request::Input(Nc2Input::SetJobId("j1".to_owned())))
    );
    // state comes before open_dialog; open_dialog counts only when true (VA 0x1201bc).
    let update = Nc2Update {
        state: Some(2),
        open_dialog: true,
        ..Nc2Update::default()
    };
    assert_eq!(
        update.request(),
        Some(Nc2Request::Input(Nc2Input::SetState(2)))
    );
    let update = Nc2Update {
        open_dialog: true,
        job_id: Some("j1".to_owned()),
        ..Nc2Update::default()
    };
    assert_eq!(update.request(), Some(Nc2Request::OpenDialog));
    // cabsim (has-bit 8) comes before cabsim_bypass, save_info and the others.
    let update = Nc2Update {
        cabsim: true,
        cabsim_bypass: Some(true),
        save_info: Some(SaveInfo {
            name: String::new(),
            device: 0,
            instrument: 0,
        }),
        ..Nc2Update::default()
    };
    assert_eq!(update.request(), Some(Nc2Request::Cabsim));
    let update = Nc2Update {
        cabsim_bypass: Some(true),
        start_from_scratch: true,
        ..Nc2Update::default()
    };
    assert_eq!(update.request(), Some(Nc2Request::CabsimBypass(true)));
    assert_eq!(Nc2Update::default().request(), None);
}

#[test]
fn capture2_file_accepts_only_v0_1_0_and_prefixes_the_identity() {
    // version{minor 1}, metadata "m", identity 0x11223344, unknown field 9, data [1,2,3].
    let file = [
        0x0a, 0x02, 0x10, 0x01, 0x12, 0x01, b'm', 0x1d, 0x44, 0x33, 0x22, 0x11, 0x48, 0x05, 0x22,
        0x03, 1, 2, 3,
    ];
    let parsed = Capture2File::parse(&file).unwrap();
    assert_eq!(parsed.metadata, ["m"]);
    assert_eq!(parsed.sharc_blob(), [0x44, 0x33, 0x22, 0x11, 1, 2, 3]);
    let mut newer = file;
    newer[3] = 0x02;
    assert_eq!(Capture2File::parse(&newer), Err(Nc2Error::Version(0, 2, 0)));
    assert_eq!(
        Capture2File::parse(&file[..file.len() - 1]),
        Err(Nc2Error::Malformed)
    );
}
