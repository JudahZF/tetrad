//! w4 settings views through the UI bridge: `State::views` reaches the tetrad-ui snapshot, and
//! Connection, Updates and capture wizard input becomes the right bridge handoff. All names
//! and addresses are synthetic.

use std::collections::BTreeMap;
use std::time::Duration;

use tetrad_app::device::ui::views::SettingsViews;
use tetrad_app::device::ui::{Handoff, Request, Ui, UiInput};
use tetrad_app::device::{Action, State};
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_settings::device::{DeviceSettings, Hardware};
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_ui::{
    CaptureRequest, CaptureStep, Control, InputEvent, PowerControl, Screen, SettingControl,
    SettingsPage, UiCommand, UpdaterCommand, UpdaterState, UpdaterStatus, WifiControl, WifiNetwork,
    WifiStatus,
};

#[derive(Default)]
struct MemSettings(BTreeMap<SettingsFileId, Vec<u8>>);

impl SettingsStorage for MemSettings {
    fn read(&self, file: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        Ok(self.0.get(&file).cloned())
    }
    fn write(&mut self, file: SettingsFileId, bytes: &[u8]) -> std::io::Result<()> {
        self.0.insert(file, bytes.to_vec());
        Ok(())
    }
    fn remove(&mut self, file: SettingsFileId) -> std::io::Result<()> {
        self.0.remove(&file);
        Ok(())
    }
}

fn state(views: SettingsViews) -> State {
    let (settings, _) =
        DeviceSettings::load(&MemSettings::default(), Hardware::QuadCortex).unwrap();
    let mut state = State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings);
    state.views = views;
    state
}

fn activate(ui: &mut Ui, state: &State, control: Control) -> Vec<Request> {
    ui.handle(
        UiInput::View(InputEvent::Activate(control)),
        state,
        Duration::from_secs(1),
    )
}

fn handoffs(requests: &[Request]) -> Vec<&Handoff> {
    requests
        .iter()
        .filter_map(|request| match request {
            Request::Handoff(handoff) => Some(handoff),
            _ => None,
        })
        .collect()
}

/// Opens Settings › System › `page` (manual §10 group dropdown).
fn open_system(ui: &mut Ui, state: &State, page: SettingsPage) {
    let account = Screen::Settings(SettingsPage::MyAccount);
    ui.handle(UiInput::Show(account), state, Duration::ZERO);
    activate(ui, state, Control::Setting(SettingControl::Groups));
    activate(ui, state, Control::Choose(1));
    activate(ui, state, Control::Setting(SettingControl::Page(page)));
    assert_eq!(ui.view().screen(), Screen::Settings(page));
}

/// The projection reaches the snapshot without losing the running version, and an open
/// network in the scan list joins through the services bridge without a password.
#[test]
fn connection_page_shows_services_state_and_joins_through_services() {
    let mut views = SettingsViews::default();
    views.wifi.status = WifiStatus::InternetConnected;
    views.wifi.ip_address = Some("192.0.2.40".into());
    views.wifi.networks = vec![WifiNetwork {
        ssid: "Cafe".into(),
        signal: 2,
        secured: false,
        connected: false,
        saved: false,
        ..WifiNetwork::default()
    }];
    let state = state(views);
    let mut ui = Ui::new();
    ui.render(&state).unwrap();
    let snapshot = ui.view().snapshot();
    assert_eq!(snapshot.wifi.ip_address.as_deref(), Some("192.0.2.40"));
    assert!(!snapshot.updater.current_version.is_empty());

    open_system(&mut ui, &state, SettingsPage::Connection);
    let requests = activate(&mut ui, &state, Control::Wifi(WifiControl::Network(0)));
    assert_eq!(
        handoffs(&requests),
        [&Handoff::Services(UiCommand::WifiConnect {
            ssid: "Cafe".into(),
            password: None,
        })]
    );
}

/// Updates: offline, the stock page has no button; online, CHECK FOR UPDATES and the
/// downloaded UPDATE go to the services bridge.
#[test]
fn updates_page_buttons_follow_the_stock_state() {
    let mut views = SettingsViews::default();
    let mut ui = Ui::new();
    let check = Control::Updater(UpdaterCommand::Check);
    let offline = state(views.clone());
    open_system(&mut ui, &offline, SettingsPage::Updates);
    assert!(handoffs(&activate(&mut ui, &offline, check)).is_empty());

    views.updater.online = true;
    let online = state(views.clone());
    assert_eq!(
        handoffs(&activate(&mut ui, &online, check)),
        [&Handoff::Services(UiCommand::Updater {
            command: UpdaterCommand::Check
        })]
    );
    views.updater.state = UpdaterState::Idle;
    views.updater.status = UpdaterStatus::NewVersionDownloaded;
    let ready = state(views);
    let install = Control::Updater(UpdaterCommand::Install);
    assert_eq!(
        handoffs(&activate(&mut ui, &ready, install)),
        [&Handoff::Services(UiCommand::Updater {
            command: UpdaterCommand::Install
        })]
    );
}

/// Capture calibration (manual §6): CONNECTION DIAGRAM steps back and START CAPTURE goes on,
/// both through the capture bridge.
#[test]
fn capture_calibration_buttons_hand_off_to_the_capture_bridge() {
    let mut views = SettingsViews::default();
    views.capture.step = CaptureStep::Calibration;
    let state = state(views);
    let mut ui = Ui::new();
    ui.handle(UiInput::Show(Screen::Capture), &state, Duration::ZERO);
    for request in [CaptureRequest::Back, CaptureRequest::StartCapture] {
        let requests = activate(&mut ui, &state, Control::Capture(request));
        assert_eq!(
            handoffs(&requests),
            [&Handoff::Capture(UiCommand::Capture { request })]
        );
    }
}

/// Mini POWER (`0x6a852c`): the bridge passes both edges and tetrad-ui acts on the release;
/// BE RIGHT BACK becomes the standby action (`0x6a82ac`).
#[test]
fn mini_power_release_opens_the_popup_and_be_right_back_requests_standby() {
    let (settings, _) = DeviceSettings::load(&MemSettings::default(), Hardware::Atma).unwrap();
    let mut state = State::new(Product::QuadCortex, HalPlatform::Atma, settings);
    state.splash_dismissed = true;
    let mut ui = Ui::new();
    ui.render(&state).unwrap();
    let power = |ui: &mut Ui, pressed| {
        ui.handle(UiInput::PowerButton { pressed }, &state, Duration::ZERO);
    };
    power(&mut ui, true);
    assert!(!ui.view().overlay_open());
    power(&mut ui, false);
    assert!(ui.view().overlay_open());
    let requests = activate(&mut ui, &state, Control::Power(PowerControl::BeRightBack));
    assert!(
        requests
            .iter()
            .any(|request| matches!(request, Request::Action(Action::Standby(true))))
    );
}

/// I/O page meters (`0x4acacc`): the poll timer runs only while the I/O page shows; each tick
/// sends the eight words to their cores; replies map as stock (`0x354468`, `0x4937a8`,
/// `0x4936cc`, `0x49324c`) and reach the snapshot.
#[test]
fn io_meters_poll_on_the_io_page_and_reach_the_snapshot() {
    use tetrad_app::device::meters::{self, POLL};
    use tetrad_app::device::{DspCommand, Effect, Timer, actions};
    use tetrad_dsp_protocol::CoreId;
    use tetrad_dsp_protocol::requests::Request;
    use tetrad_dsp_protocol::responses::BulkMeter;
    use tetrad_dsp_protocol::tuner_looper::MeterRecord;

    let mut state = state(SettingsViews::default());
    let shown = actions::reduce(
        Action::ScreenShown(Screen::Io(tetrad_ui::IoTab::Input1)),
        &mut state,
    );
    let start = Effect::StartTimer {
        timer: Timer::IoMeters,
        after: POLL,
    };
    assert!(shown.unwrap().contains(&start));
    let sent: Vec<_> = meters::requests()
        .map(|effect| match effect {
            Effect::Dsp(DspCommand::Request(Request::MeterRequest {
                core,
                slot: 0,
                word,
            })) => (core.wire(), word),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        sent,
        [
            (0, 0x4000),
            (0, 0x4001),
            (0, 0x4002),
            (0, 0x4003),
            (1, 0x4023),
            (1, 0x4022),
            (1, 0x4021),
            (CoreId::Control.wire(), 0x4024)
        ]
    );

    let record = MeterRecord {
        core: 0,
        column: 0,
        index: 0x4002,
        value_bits: (-12.0f32).to_bits(),
    };
    assert!(meters::on_meter(&mut state.io_meters, &record));
    let bulk = |first, second, values: &[f32]| {
        let mut value_bits = [0; 16];
        for (bits, value) in value_bits.iter_mut().zip(values) {
            *bits = value.to_bits();
        }
        BulkMeter {
            first,
            second,
            count: values.len() as u32,
            value_bits,
        }
    };
    let outputs = [-1.0, -2.0, -3.0, -4.0, -5.0, -6.0, -7.0, -8.0];
    assert!(meters::on_bulk(
        &mut state.io_meters,
        &bulk(1, 0x4023, &outputs)
    ));
    assert!(meters::on_bulk(
        &mut state.io_meters,
        &bulk(1, 0x4022, &[3.0, 0.0, -1.0, 6.0, 0.0, 0.0, 0.0, 0.0])
    ));
    let usb: Vec<f32> = (0..16).map(|index| -(index as f32)).collect();
    assert!(meters::on_bulk(
        &mut state.io_meters,
        &bulk(4, 0x4024, &usb)
    ));
    assert!(!meters::on_bulk(
        &mut state.io_meters,
        &bulk(4, 0x4023, &outputs)
    ));

    let mut ui = Ui::new();
    ui.render(&state).unwrap();
    let shown = ui.view().snapshot().io_meters;
    assert_eq!(shown.returns, [-12.0, -40.0]);
    assert_eq!(
        (shown.outputs, shown.sends, shown.headphones),
        ([-1.0, -2.0, -3.0, -4.0], [-5.0, -6.0], [-7.0, -8.0])
    );
    assert_eq!(shown.limiters, [-3.0, 0.0, 0.0, -6.0]);
    assert_eq!(
        (shown.usb_out[0], shown.usb_in[0]),
        ([0.0, -1.0], [-8.0, -9.0])
    );

    let left = actions::reduce(Action::ScreenShown(Screen::Grid), &mut state).unwrap();
    assert!(left.contains(&Effect::StopTimer(Timer::IoMeters)));
}

/// Device Information (`0x40d27c`) and DSP Diagnostics (`0x364ba0`) read `State::info` and
/// `State::diagnostics`: firmware words print as hex, the classic QC shows its board rows,
/// and a core without a block stays empty. All values are made up.
#[test]
fn support_pages_project_device_information_and_dsp_diagnostics() {
    use tetrad_dsp_protocol::DspCore;
    use tetrad_dsp_protocol::responses::DiagnosticsBlock;
    use tetrad_hal::zencoder::FirmwareVersion;
    let mut state = state(SettingsViews::default());
    state.info.serial = Some(tetrad_media::factory::DeviceSerial::parse(b"ZZ0000001\n").unwrap());
    state.info.mac = Some("02:00:00:00:00:01".into());
    state.info.uboot = Some("U-Boot 2000.01 (test)".into());
    state.info.wireless = Some("0123456789abcdef0123456789abcdef".into());
    state.info.zenjack = [Some(FirmwareVersion(0x1a2b)), None];
    let mut words = [0_u32; 16];
    (words[0], words[3]) = (7, 2);
    for (index, value) in [(5, 12.5_f32), (9, 40.0), (11, 3.25), (10, 1.5)] {
        words[index] = value.to_bits();
    }
    words[6] = 76_796; // DM 50 %
    state
        .diagnostics
        .record(DspCore::Core1, &DiagnosticsBlock { words });
    let mut ui = Ui::new();
    let _ = ui.build(&state);
    let snapshot = ui.view().snapshot();
    let about = &snapshot.about;
    assert_eq!(about.serial.as_deref(), Some("ZZ0000001"));
    assert_eq!(about.mac.as_deref(), Some("02:00:00:00:00:01"));
    assert_eq!(about.uboot.as_deref(), Some("U-Boot 2000.01 (test)"));
    assert_eq!(about.kernel, None);
    assert_eq!(about.wireless_fw.as_deref(), state.info.wireless.as_deref());
    assert!(about.zen_boards);
    assert_eq!(about.zenjack_app.as_deref(), Some("1a2b"));
    assert_eq!(about.zenjack_bootloader, None);
    let [core0, core1, ..] = snapshot.dsp_diagnostics;
    assert_eq!(core0, None);
    let core1 = core1.expect("core 2 reported");
    assert_eq!((core1.dropped_messages, core1.fp_exceptions), (7, 2));
    assert_eq!((core1.cpu, core1.emdma), ([12.5, 40.0, 3.25], 1.5));
    assert_eq!(core1.heap[0], 50.0);
}

/// The NC2 A/B dialog buttons hand off to the NC2 controller in stock terms: CAPTURE and
/// REFERENCE enter states 5 and 4 (`0x3d7424`), RESTART CAPTURE discards (`0x3d8000` →
/// `0x203ee4`), SAVE NEURAL CAPTURE saves (`0x3d8018` → `0x1fef08`) and ✕ closes.
#[test]
fn nc2_dialog_buttons_hand_off_to_the_nc2_controller() {
    use tetrad_capture::v2::Nc2Input;
    use tetrad_ui::{AuditionSource, Nc2Dialog, Nc2Request as N};
    let mut views = SettingsViews::default();
    views.capture.nc2 = Some(Nc2Dialog::Ready);
    let state = state(views);
    let mut ui = Ui::new();
    let _ = ui.build(&state);
    for (request, input) in [
        (N::Audition(AuditionSource::Capture), Nc2Input::SetState(5)),
        (
            N::Audition(AuditionSource::Reference),
            Nc2Input::SetState(4),
        ),
        (N::Restart, Nc2Input::Discard),
        (N::Save, Nc2Input::Save),
        (N::Close, Nc2Input::Close),
    ] {
        let requests = activate(&mut ui, &state, Control::Nc2(request));
        assert_eq!(handoffs(&requests), [&Handoff::Nc2(input)], "{request:?}");
    }
}
