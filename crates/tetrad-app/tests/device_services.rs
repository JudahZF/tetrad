//! Services bridge over tetrad-services fakes: Wi-Fi to DHCP to internet, cloud tokens in
//! tetrad-settings, the signed update check, pair, unpair and licence activation. All tokens,
//! keys and licence bytes are synthetic.

mod common;

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use tetrad_app::device::platform::{ServiceBackends, ServiceIo};
use tetrad_app::device::services::{
    AfterUnpair, CloudRequest, JobError, Lane, Licences, PluginStatus, PresetDraft, ServiceJob,
    ServiceNotice, Services, UploadState, reset_files, run_job,
};
use tetrad_app::device::{DspCommand, Effect, PowerAction, Subsystem, Timer};
use tetrad_capture::v2::Nc2Input;
use tetrad_control::codecs::{ControlMessage, EnumValue, UpdaterState, UpdaterStatus};
use tetrad_dsp_protocol::requests::Request;
use tetrad_dsp_protocol::responses::UsbMeter;
use tetrad_dsp_protocol::tuner_looper::OpaqueCalibration;
use tetrad_media::factory::SERIAL_PATH;
use tetrad_services::cloud::signing::SigningMaterial;
use tetrad_services::exec::{NetError, Output};
use tetrad_services::fake::{MemFiles, ScriptedHttp, ScriptedNet, ScriptedRunner};
use tetrad_services::http::{HttpError, HttpRequest, Method};
use tetrad_services::updater::cloud::UpdateError;
use tetrad_services::updater::state::UpdaterStatus as FlagStatus;
use tetrad_services::wifi::{LinkError, SupplicantLink};
use tetrad_settings::device::{DeviceSettings, Hardware, ResetKind};
use tetrad_settings::tuner::TempoDefaults;
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_watchdog::power::RestartReason;

const CONNECTED: &str =
    "<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:01 completed [id=1 id_str=]";

/// Answers `LIST_NETWORKS` with one saved network, `BSS <i>` from `bss` (then `FAIL`) and
/// everything else with `OK`.
#[derive(Default)]
struct Link {
    sent: Vec<String>,
    bss: Vec<String>,
}

impl SupplicantLink for Link {
    fn request(&mut self, command: &str) -> Result<String, LinkError> {
        self.sent.push(command.to_owned());
        if let Some(index) = command.strip_prefix("BSS ") {
            let index: usize = index.parse().unwrap();
            return Ok(self
                .bss
                .get(index)
                .cloned()
                .unwrap_or_else(|| "FAIL".into()));
        }
        Ok(match command {
            "LIST_NETWORKS" => "network id / ssid / bssid / flags\n1\tHome\tany\t[CURRENT]\n",
            _ => "OK",
        }
        .to_owned())
    }
}

#[derive(Default)]
struct Backends {
    runner: ScriptedRunner,
    files: MemFiles,
    net: ScriptedNet,
    http: ScriptedHttp,
    link: Link,
}

impl ServiceBackends for Backends {
    type Runner = ScriptedRunner;
    type Files = MemFiles;
    type Net = ScriptedNet;
    type Http = ScriptedHttp;
    type Supplicant = Link;
    fn runner(&mut self) -> &mut ScriptedRunner {
        &mut self.runner
    }
    fn files(&mut self) -> &mut MemFiles {
        &mut self.files
    }
    fn net(&mut self) -> &mut ScriptedNet {
        &mut self.net
    }
    fn http(&mut self) -> &mut ScriptedHttp {
        &mut self.http
    }
    fn supplicant(&mut self) -> &mut Link {
        &mut self.link
    }
    fn lend(&mut self) -> Option<ServiceIo<'_, Self>> {
        Some(ServiceIo {
            runner: &mut self.runner,
            files: &mut self.files,
            net: &mut self.net,
            http: &mut self.http,
            supplicant: &mut self.link,
        })
    }
}

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

/// Made-up DSP replies: `0x11` to `0x35` (signing material) and `0x1f` to `0x4f` (client id).
fn signing_replies(services: &mut Services) {
    services.on_usb_meter_update(UsbMeter([0x5a; 16]));
    services.on_tuner_calibrate(OpaqueCalibration([0xa5; 32]));
}

/// A started check sends `0x35` and `0x4f`, both with the device serial as payload.
fn assert_signing_requests(effects: &[Effect]) {
    let sent: Vec<&Request> = effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Dsp(DspCommand::Request(request)) => Some(request),
            _ => None,
        })
        .collect();
    assert!(matches!(
        sent.as_slice(),
        [Request::SigningMaterial(_), Request::ClientId(_)]
    ));
}

/// The signing headers of the update check, in stock order (w4 §6).
const SIGNING_HEADERS: [&str; 4] = [
    "X-Cloud-Api-Client-Signature",
    "X-Cloud-Api-Client-Serial-Number",
    "X-Cloud-Api-Date",
    "X-Cloud-Api-Client-Id",
];

/// Stock set order of the signing headers for pair and unpair (0x00301ec0) and for licence
/// activation (0x001eed80, 0x001eea58).
const PAIR_ORDER: [&str; 4] = [
    "X-Cloud-Api-Date",
    "X-Cloud-Api-Client-Signature",
    "X-Cloud-Api-Client-Serial-Number",
    "X-Cloud-Api-Client-Id",
];
const LICENCE_ORDER: [&str; 4] = [
    "X-Cloud-Api-Client-Serial-Number",
    "X-Cloud-Api-Client-Id",
    "X-Cloud-Api-Client-Signature",
    "X-Cloud-Api-Date",
];

/// The signing headers are in `order`, with stock formats, and the bearer is present. The
/// signature and client id come from fake material, so only their shape is checked.
fn assert_signed(request: &HttpRequest, order: [&str; 4], unix_seconds: &str) {
    let names: Vec<&str> = request
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| SIGNING_HEADERS.contains(name))
        .collect();
    assert_eq!(names, order, "{request:?}");
    let signature = request
        .header_value("X-Cloud-Api-Client-Signature")
        .unwrap();
    assert!(
        signature.len() == 64
            && signature
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    );
    assert_eq!(request.header_value("X-Cloud-Api-Date"), Some(unix_seconds));
    assert_eq!(
        request.header_value("X-Cloud-Api-Client-Serial-Number"),
        Some("Q0000TEST")
    );
    let client_id = request.header_value("X-Cloud-Api-Client-Id").unwrap();
    assert!(client_id.len() == 44 && client_id.ends_with('='));
    assert!(
        request
            .header_value("Authorization")
            .is_some_and(|value| value.starts_with("Bearer "))
    );
}

fn token_reply() -> String {
    let access = jwt(r#"{"sub":"user-1","exp":4102444800}"#);
    format!(r#"{{"access_token":"{access}","refresh_token":"refresh-synthetic"}}"#)
}

/// Online with the wall clock at unix 0, a serial, then signed in and paired: requests 0 and
/// 1 are `user/signin` and the pair.
fn signed_in(services: &mut Services, b: &mut Backends, settings: &mut DeviceSettings) {
    internet_up(b);
    let linked = services.set_link(b, true, ms(0)).unwrap();
    settle(services, b, settings, linked, ms(0));
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    b.http.respond(200, token_reply());
    b.http.respond(200, "");
    b.http.respond(200, r#"{"username":"player"}"#);
    let started = services
        .sign_in(
            b,
            settings,
            "player@example.com",
            "password-synthetic",
            ms(1_000),
        )
        .unwrap();
    let started = settle(services, b, settings, started, ms(1_000));
    assert_signing_requests(&started);
    signing_replies(services);
    let paired = services.tick(b, settings, ms(1_060)).unwrap();
    settle(services, b, settings, paired, ms(1_060));
}

/// The tick's effects with its cloud jobs run, as the device does. Internet rechecks are
/// dropped: these tests script the network for the first probe only.
fn tick(
    services: &mut Services,
    b: &mut Backends,
    settings: &mut DeviceSettings,
    now: Duration,
) -> Vec<Effect> {
    let mut effects = services.tick(b, settings, now).unwrap();
    effects.retain(|effect| !matches!(effect, Effect::Service(ServiceJob::Internet(_))));
    settle(services, b, settings, effects, now)
}

fn settings() -> DeviceSettings {
    DeviceSettings::load(&MemSettings::default(), Hardware::QuadCortex)
        .unwrap()
        .0
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

fn ntp_reply(unix_seconds: u32) -> Vec<u8> {
    let mut reply = vec![0u8; 48];
    reply[0] = 0x24;
    reply[1] = 2;
    reply[40..44].copy_from_slice(&(unix_seconds + 2_208_988_800).to_be_bytes());
    reply
}

/// The next probe succeeds and NTP answers with the epoch.
fn internet_up(b: &mut Backends) {
    b.net.tcp.push_back(Ok(()));
    b.net
        .names
        .push_back(Ok(vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))]));
    b.net.udp.push_back(Ok(ntp_reply(0)));
    b.runner.expect_ok("date -u -s 1970-01-01 00:00:00");
}

/// Runs every service job as the inline `ServiceBackends::run_job` default does and feeds each
/// result back in device queue order, until no job is left. Returns the other effects.
fn settle(
    services: &mut Services,
    b: &mut Backends,
    settings: &mut DeviceSettings,
    effects: Vec<Effect>,
    now: Duration,
) -> Vec<Effect> {
    let mut queue = VecDeque::from(effects);
    let mut out = Vec::new();
    while let Some(effect) = queue.pop_front() {
        match effect {
            Effect::Service(job) => {
                let notice = run_job(b, job);
                queue.extend(services.on_notice(b, notice, settings, now).unwrap());
            }
            other => out.push(other),
        }
    }
    out
}

fn texts(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Notice(Subsystem::Services, text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Standard padded base64, as the cloud sends a product payload.
fn standard_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            out.push(match i <= chunk.len() {
                true => TABLE[(n >> (18 - 6 * i) & 63) as usize] as char,
                false => '=',
            });
        }
    }
    out
}

/// One backup record (`BackupData`, `0x306794`): the name in base64, uploaded at
/// 2025-08-29T17:06:00Z.
fn backup_data(name: &str, path: &str, key: &str) -> String {
    format!(
        r#"{{"userId":"u1","clientType":"quad_cortex","clientVersion":"4.1.0","backupName":"{}","backupUrl":"{path}","backupKey":"{key}","timestamp":1756487160}}"#,
        standard_base64(name.as_bytes())
    )
}

/// A `GET /backups/v1` reply.
fn backup_list(items: &[String]) -> String {
    format!(r#"{{"backups":[{}]}}"#, items.join(","))
}

/// A `generateBackupUrl` or `<backup>/update` reply (`0x30dce0`) for the backup at `path`.
fn backup_target(path: &str) -> String {
    format!(
        r#"{{"url":"https://uploads.example/b?sig=s","metadata":{}}}"#,
        backup_data("New", path, "k-new")
    )
}

/// Unsigned JWT with the given claims, as the cloud returns it.
fn jwt(claims: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in claims.as_bytes().chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    format!("e30.{out}.sig")
}

#[test]
fn association_runs_dhcp_job_then_lease_brings_internet_up() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();

    let effects = services
        .on_notice(
            &mut b,
            ServiceNotice::Supplicant(CONNECTED.into()),
            &mut settings,
            ms(0),
        )
        .unwrap();
    assert_eq!(texts(&effects), ["Wi-Fi associated"]);
    let job = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Service(job) => Some(job.clone()),
            _ => None,
        })
        .expect("association asks for a DHCP job");
    assert!(matches!(job, ServiceJob::Dhcp(_)));
    assert!(
        b.runner.calls.is_empty(),
        "DHCP never runs on the event path"
    );

    b.runner
        .expect(
            "udhcpc --now --quit -i wlan0",
            Ok(Output::code(0)
                .with_stdout("udhcpc: Lease of 192.0.2.40 obtained, lease time 600\n")),
        )
        .expect(
            "ifconfig wlan0",
            Ok(Output::code(0).with_stdout(
                "          inet addr:192.0.2.40  Bcast:192.0.2.255  Mask:255.255.255.0\n",
            )),
        )
        .expect(
            "route -n",
            Ok(Output::code(0).with_stdout(
                "0.0.0.0         192.0.2.1       0.0.0.0         UG    0      0        0 wlan0\n",
            )),
        );
    internet_up(&mut b);
    let done = run_job(&mut b, job);
    let effects = services
        .on_notice(&mut b, done, &mut settings, ms(10))
        .unwrap();
    let effects = settle(&mut services, &mut b, &mut settings, effects, ms(10));

    let lines = texts(&effects);
    assert_eq!(
        lines[..3],
        [
            "DHCP lease 192.0.2.40/24, lease time 600 s",
            "internet reachable",
            "time set from NTP"
        ]
    );
    assert_eq!(
        services.wifi().lease().map(|lease| lease.address),
        Some(Ipv4Addr::new(192, 0, 2, 40))
    );
    assert!(b.runner.is_done());
}

/// Cortex Control login (`loginFromControl` 0x00307e18): tokens without valid claims are
/// refused (0x00450158, nothing saved, no request); valid ones are saved and the pair follows,
/// which without a serial fails (0x00307540). Each failure answers GenericError
/// CloudLoginFailed with the login request ID (0x00238ce4). Synthetic tokens.
#[test]
fn control_login_saves_valid_tokens_and_answers_failures_with_its_request_id() {
    use tetrad_app::device::services::ControlLogin;
    use tetrad_control::codecs::{ControlMessage, GenericErrorCode, GenericErrorMessage};
    use tetrad_services::cloud::session::Tokens;
    use tetrad_services::exec::Secret;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    let mut login = |settings: &mut DeviceSettings, access: &str, request_id| {
        let login = ControlLogin {
            username: Some("player@example.com".into()),
            tokens: Tokens {
                access_token: Secret::new(access),
                refresh_token: Secret::new("refresh-synthetic"),
            },
            request_id: Some(request_id),
        };
        let started = services
            .login_from_control(&mut b, settings, login, ms(0))
            .unwrap();
        settle(&mut services, &mut b, settings, started, ms(0))
    };
    let failed = |id| {
        Effect::ControlNotify(Box::new(ControlMessage::GenericError(
            GenericErrorMessage {
                request_id: Some(id),
                error_code: Some(GenericErrorCode::CloudLoginFailed.into()),
            },
        )))
    };
    let effects = login(&mut settings, "not-a-token", 7);
    assert!(effects.contains(&failed(7)));
    assert!(settings.settings().tokens.access_token.is_empty());

    let access = jwt(r#"{"sub":"user-1","exp":4102444800}"#);
    let effects = login(&mut settings, &access, 8);
    assert_eq!(settings.settings().tokens.access_token.expose(), access);
    assert!(effects.contains(&failed(8)));
    assert!(
        texts(&effects).iter().any(|text| {
            text.starts_with("Failed to pair hardware after logging in from Control")
        })
    );
    assert!(b.http.requests.is_empty());
}

/// The serial skip rule (0x00307b04): without a serial no pair request goes out, the session
/// drops its tokens and AccountConfig keeps them (0x0030afb8). Logout then signs out at once.
#[test]
fn sign_in_without_serial_skips_the_pair_and_sign_out_still_blanks_tokens() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    let access = jwt(r#"{"sub":"user-1","exp":4102444800}"#);
    b.http.respond(
        200,
        format!(r#"{{"access_token":"{access}","refresh_token":"refresh-synthetic"}}"#),
    );

    let started = services
        .sign_in(
            &mut b,
            &mut settings,
            "player@example.com",
            "password-synthetic",
            ms(0),
        )
        .unwrap();
    let effects = settle(&mut services, &mut b, &mut settings, started, ms(0));

    let saved = settings.settings();
    assert_eq!(saved.tokens.access_token.expose(), access);
    assert_eq!(saved.tokens.refresh_token.expose(), "refresh-synthetic");
    assert_eq!(saved.account.username, "player@example.com");
    assert!(effects.contains(&Effect::StartTimer {
        timer: Timer::SettingsFlush,
        after: SettingsFileId::Account.save_delay(),
    }));
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::Dsp(_)))
    );
    assert_eq!(
        texts(&effects),
        ["Associating hardware with the account failed. Unit is unserialized."]
    );
    assert!(services.cloud().is_some_and(|api| !api.is_signed_in()));

    let started = services
        .unpair(&mut b, &mut settings, AfterUnpair::SignOut, ms(0))
        .unwrap();
    let effects = settle(&mut services, &mut b, &mut settings, started, ms(0));
    assert_eq!(
        texts(&effects),
        ["(Un)pairHardware() failed when logging out: no serial number"]
    );
    assert_eq!(b.http.requests.len(), 1, "only user/signin went out");
    let saved = settings.settings();
    assert!(saved.tokens.access_token.is_empty() && saved.tokens.refresh_token.is_empty());
    assert!(saved.account.username.is_empty());
}

/// Stock 0x0030cb1c → 0x00301ec0: sign-in success pairs with a signed POST, the bearer and an
/// empty body; success stores `last_paired_version` (0x00307784). Without DSP replies the
/// session drops its tokens and AccountConfig keeps them (0x0030afb8).
#[test]
fn sign_in_pairs_with_a_signed_request_or_drops_the_session() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);

    let pair = &b.http.requests[1];
    assert_eq!(pair.method, Method::Post);
    assert!(pair.url.ends_with("/hardware/v1/pair"), "{pair:?}");
    assert_signed(pair, PAIR_ORDER, "1");
    assert_eq!(pair.body.as_deref(), Some(&[][..]));
    assert_eq!(settings.settings().paired_version.0, "4.1.0");
    // 0x0030afb8 → 0x00301b94: the profile follows a successful pair.
    let profile = &b.http.requests[2];
    assert_eq!(profile.method, Method::Get);
    assert!(profile.url.ends_with("/user/profile"), "{profile:?}");
    assert!(services.cloud().is_some_and(|api| api.is_signed_in()));

    b.http.respond(200, token_reply());
    let started = services
        .sign_in(
            &mut b,
            &mut settings,
            "player@example.com",
            "password-synthetic",
            ms(5_000),
        )
        .unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(5_000));
    let early = services.tick(&mut b, &mut settings, ms(6_999)).unwrap();
    assert!(texts(&early).is_empty(), "{early:?}");
    let expired = services.tick(&mut b, &mut settings, ms(7_000)).unwrap();
    assert_eq!(
        texts(&expired),
        [
            "Associating hardware with the account failed. Network error? (not supported yet: no DSP signing reply)"
        ]
    );
    assert_eq!(
        b.http.requests.len(),
        4,
        "no pair request without the replies"
    );
    assert!(services.cloud().is_some_and(|api| !api.is_signed_in()));
    assert!(!settings.settings().tokens.access_token.is_empty());
}

/// Stock device logout (0x004190e0 → 0x00308250 → 0x0030b3c4): a signed DELETE without a body,
/// then `user/signout` and the token wipe even when the unpair fails.
#[test]
fn sign_out_unpairs_then_signs_out_whatever_the_unpair_result() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    b.http.respond(500, r#"{"error":3200}"#);
    b.http.respond(200, "");

    let started = services
        .unpair(&mut b, &mut settings, AfterUnpair::SignOut, ms(5_000))
        .unwrap();
    assert_signing_requests(&started);
    assert_eq!(
        b.http.requests.len(),
        3,
        "nothing goes out before the replies"
    );
    signing_replies(&mut services);
    let effects = tick(&mut services, &mut b, &mut settings, ms(5_060));

    let [unpair, sign_out] = &b.http.requests[3..] else {
        panic!("{:?}", b.http.requests);
    };
    assert_eq!(unpair.method, Method::Delete);
    assert!(unpair.url.ends_with("/hardware/v1/pair"), "{unpair:?}");
    assert_signed(unpair, PAIR_ORDER, "5");
    assert_eq!(unpair.body, None);
    assert!(sign_out.url.ends_with("/user/signout"), "{sign_out:?}");
    let notices = texts(&effects);
    assert_eq!(notices.len(), 1);
    assert!(notices[0].starts_with("(Un)pairHardware() failed when logging out: cloud error 3200"));
    let saved = settings.settings();
    assert!(saved.tokens.access_token.is_empty() && saved.account.username.is_empty());
}

/// Factory reset `0x004de79c`: the settings reset blanks `AccountConfig.ini` before the
/// unpair (`0x00308250`) runs; the session loaded at boot keeps the bearer for it. The Wi-Fi
/// networks (`0x00895264`) go only after the unpair ends, so its requests still had the link
/// (review-w6 F1).
#[test]
fn factory_reset_unpairs_with_the_link_up_then_forgets_networks() {
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut Services::new(), &mut b, &mut settings);
    // A reboot: a new bridge loads the saved tokens, then the reset wipes them.
    let mut services = Services::new();
    services.load_cloud(&mut settings).unwrap();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    settings.reset(ResetKind::Factory, &TempoDefaults::default());
    assert!(settings.settings().tokens.access_token.is_empty());

    services
        .unpair(&mut b, &mut settings, AfterUnpair::SignOut, ms(5_000))
        .unwrap();
    services.forget_all(&mut b).unwrap();
    let removed = |link: &Link| link.sent.iter().any(|sent| sent == "REMOVE_NETWORK all");
    assert!(!removed(&b.link), "networks stay while the unpair waits");
    signing_replies(&mut services);
    tick(&mut services, &mut b, &mut settings, ms(5_060));
    // The link-up also queued the update check in this batch.
    let unpair = (b.http.requests.iter())
        .find(|request| {
            request.url.ends_with("/hardware/v1/pair") && request.method == Method::Delete
        })
        .unwrap();
    assert_signed(unpair, PAIR_ORDER, "5");
    assert!(removed(&b.link));
}

/// Cortex Control logout (`0x003080b4` → `0x003070f4`): the unpair, then the local logout
/// only; stock sends no `user/signout` (review-w6 F8).
#[test]
fn control_logout_unpairs_without_user_signout() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let before = b.http.requests.len();
    services
        .unpair(&mut b, &mut settings, AfterUnpair::LocalLogout, ms(5_000))
        .unwrap();
    signing_replies(&mut services);
    tick(&mut services, &mut b, &mut settings, ms(5_060));
    let [unpair] = &b.http.requests[before..] else {
        panic!("{:?}", &b.http.requests[before..]);
    };
    assert_eq!(unpair.method, Method::Delete);
    let saved = settings.settings();
    assert!(saved.tokens.access_token.is_empty() && saved.account.username.is_empty());
}

/// Factory reset licence clear (`0x001f0c50`): `0x51` goes out empty and an apply in flight
/// is dropped, so its late `0x20` answer licenses nothing (review-w6 F3).
#[test]
fn licence_clear_sends_an_empty_0x51_and_drops_the_apply_in_flight() {
    let mut services = Services::new();
    let mut settings = settings();
    settings.update(|s| s.licenses.payload = vec![0x3c; 32].into());
    assert_eq!(services.apply_licences(&settings, ms(0)).len(), 1);
    let cleared = services.clear_licences();
    assert!(matches!(
        &cleared,
        Effect::Dsp(DspCommand::Request(Request::Blob51(body))) if body.as_bytes().is_empty()
    ));
    let mut licences = Licences::default();
    let mask = (1_u64 << 1 | 1 << 62).to_le_bytes();
    let (added, _) = services.on_tuner_timer(&mask, &mut settings, &mut licences, ms(10));
    assert!(added.is_empty());
    assert_eq!(licences.status(1), PluginStatus::Unlicensed);
}

/// Boot (0x001ed7b8 → 0x001f1050): `0x35` and `0x4f` with the serial go before the stored
/// licence `0x51`. S3 on the QC (2026-10-15): without them the DSP answered with no mask.
#[test]
fn boot_signs_before_it_applies_the_stored_licences() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    settings.update(|s| s.licenses.payload = vec![0x3c; 32].into());
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let requests: Vec<_> = services
        .boot_licences(&mut b.files, &settings, ms(0))
        .into_iter()
        .map(|effect| match effect {
            Effect::Dsp(DspCommand::Request(request)) => request,
            other => panic!("{other:?}"),
        })
        .collect();
    assert!(matches!(
        requests.as_slice(),
        [
            Request::SigningMaterial(_),
            Request::ClientId(_),
            Request::Blob51(_)
        ]
    ));
}

/// S2 on the QC (2026-10-15, owned): a `0x20` answer without the 8-byte mask keeps the
/// licences in memory and the stored `previous`/`new` sets; only a notice reports it.
#[test]
fn short_licence_answer_keeps_the_stored_licences() {
    let mut services = Services::new();
    let mut settings = settings();
    settings.update(|s| s.licenses.payload = vec![0x3c; 32].into());
    let mut licences = Licences::default();
    services.apply_licences(&settings, ms(0));
    let mask = (1_u64 << 1 | 1 << 3 | 1 << 62).to_le_bytes();
    services.on_tuner_timer(&mask, &mut settings, &mut licences, ms(10));
    assert_eq!(licences.status(1), PluginStatus::Active);
    let (before, stored) = (licences, settings.settings().licenses.clone());
    services.apply_licences(&settings, ms(20));
    let (added, effects) = services.on_tuner_timer(&[], &mut settings, &mut licences, ms(30));
    assert!(added.is_empty());
    assert_eq!(licences, before);
    assert_eq!(settings.settings().licenses, stored);
    assert!(
        matches!(effects.as_slice(), [Effect::Notice(..)]),
        "{effects:?}"
    );
}

/// Review r11 R3: the licence clear also drops an activation queued in the signing batch,
/// so it never runs after the reset with the old tokens.
#[test]
fn licence_clear_drops_a_queued_activation() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    services
        .refresh_licences(&mut b, &mut settings, ms(10_000))
        .unwrap();
    services.clear_licences();
    signing_replies(&mut services);
    tick(&mut services, &mut b, &mut settings, ms(10_060));
    assert!(
        !(b.http.requests.iter()).any(|request| request.url.ends_with("/licenses/activate")),
        "{:?}",
        b.http.requests
    );
}

/// Health r34 #5: the licence clear also drops a queued licence pre-pair (`0x003083a8`), so a
/// factory reset sends no pair and sets no licence step after it.
#[test]
fn licence_clear_drops_a_queued_licence_pre_pair() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    settings.update(|s| s.paired_version = Default::default());
    let before = b.http.requests.len();
    services
        .refresh_licences(&mut b, &mut settings, ms(10_000))
        .unwrap();
    services.clear_licences();
    signing_replies(&mut services);
    tick(&mut services, &mut b, &mut settings, ms(10_060));
    tick(&mut services, &mut b, &mut settings, ms(20_000));
    assert!(
        !(b.http.requests[before..].iter())
            .any(|r| r.url.ends_with("/hardware/v1/pair") || r.url.ends_with("/licenses/activate")),
        "{:?}",
        &b.http.requests[before..]
    );
}

/// The file-only reset steps: `0x004de3ec(1)` removes the test-mode, updater and debug files
/// and rewrites `presetsDirPath`; `0x00459014` writes boot count 0 (review-w6 F4).
#[test]
fn factory_file_steps_remove_flags_and_reset_the_boot_count() {
    let mut files = MemFiles::default();
    let kept = [
        "/opt/neuraldsp/productiontestmode",
        "/etc/updater_status",
        "/media/p4/coredump",
        "/opt/neuraldsp/allow_sshd",
    ];
    for path in kept {
        files.put(path, "1");
    }
    files.put("/opt/neuraldsp/boot_count.txt", "41");
    for step in ResetKind::Factory.effects() {
        reset_files(&mut files, *step).unwrap();
    }
    assert!(kept.iter().all(|path| files.get(path).is_none()));
    assert_eq!(files.get("/opt/neuraldsp/boot_count.txt").unwrap(), b"0");
    assert_eq!(
        files.get("/opt/neuraldsp/presetsDirPath").unwrap(),
        b"/media/p4/Presets"
    );
    // A settings reset keeps the debug files and the boot count.
    let mut files = MemFiles::default();
    files.put("/opt/neuraldsp/allow_sshd", "1");
    for step in ResetKind::Settings.effects() {
        reset_files(&mut files, *step).unwrap();
    }
    assert!(files.get("/opt/neuraldsp/allow_sshd").is_some());
    assert!(files.get("/opt/neuraldsp/boot_count.txt").is_none());
}

/// Stock 0x001f11d4 → 0x001f0194 → 0x001f2ccc → 0x00515640 → 0x001edd10: the signed
/// activation POST; a reply signed over its `activationCode` sends the decoded code to the DSP
/// in `0x50`, and the 32-byte `0x21` answer goes to PluginLicenses.ini. A forged reply ends the
/// activation and nothing goes to the DSP.
#[test]
fn licence_refresh_checks_the_reply_and_stores_the_dsp_answer() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let material = SigningMaterial::take(&mut [0x5a; 16]);
    let reply = |signature: &str| {
        format!(r#"{{"activationCode":"AQIDBA==","timestamp":10,"signature":"{signature}"}}"#)
    };
    let message = "quad_cortex-Q0000TEST-with-firmware-4.1.0-at-10-AQIDBA==";
    let to_dsp = |effects: &[Effect]| -> Vec<Vec<u8>> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Dsp(DspCommand::Request(Request::Blob50(body))) => {
                    Some(body.as_bytes().to_vec())
                }
                _ => None,
            })
            .collect()
    };
    b.http.respond(200, reply(&material.sign(message)));

    let started = services
        .refresh_licences(&mut b, &mut settings, ms(10_000))
        .unwrap();
    assert_signing_requests(&started);
    signing_replies(&mut services);
    let effects = tick(&mut services, &mut b, &mut settings, ms(10_060));
    let activate = &b.http.requests[3];
    assert_eq!(activate.method, Method::Post);
    assert!(
        activate.url.ends_with("/purchase/v1/licenses/activate"),
        "{activate:?}"
    );
    assert_signed(activate, LICENCE_ORDER, "10");
    assert_eq!(activate.body.as_deref(), Some(&[][..]));
    assert_eq!(activate.timeout, ms(40_000));
    assert_eq!(to_dsp(&effects), [vec![1, 2, 3, 4]]);

    let stored = services.on_midi_out_timer(&[0x3c; 32], &mut settings, ms(10_100));
    assert_eq!(*settings.settings().licenses.payload, [0x3c; 32]);
    assert!(stored.iter().any(|effect| matches!(
        effect,
        Effect::StartTimer {
            timer: Timer::SettingsFlush,
            ..
        }
    )));
    // 0x001f2390 → 0x001f0680 → 0x005155c4: the stored data goes to the DSP in `0x51`; the
    // 8-byte `0x20` answer is the licensed-plugin mask (0x001edec4).
    let applied: Vec<_> = stored
        .iter()
        .filter_map(|effect| match effect {
            Effect::Dsp(DspCommand::Request(Request::Blob51(body))) => Some(body.as_bytes()),
            _ => None,
        })
        .collect();
    assert_eq!(applied, [&[0x3c; 32][..]]);
    // 0x001f18c0 with a made-up mask: bits 1 (a known id), 2 (outside the 0xa17988 table) and
    // 62 (active). Status 2 (0x001ed368) needs both; `previous` and `new` are stored.
    let mut licences = Licences::stored(&settings);
    assert_eq!(licences.status(1), PluginStatus::Unlicensed);
    let mask = 1_u64 << 62 | 0b110;
    let (added, answered) = services.on_tuner_timer(
        &mask.to_le_bytes(),
        &mut settings,
        &mut licences,
        ms(10_200),
    );
    assert_eq!(added, [1, 2]);
    assert!(texts(&answered).is_empty(), "{answered:?}");
    assert_eq!(licences.status(1), PluginStatus::Active);
    assert_eq!(licences.status(2), PluginStatus::Unlicensed);
    let stored = &settings.settings().licenses;
    assert_eq!(
        (&stored.previous[..], &stored.new[..]),
        (&[1, 2][..], &[1, 2][..])
    );
    assert_eq!(
        Licences::stored(&settings).status(1),
        PluginStatus::Inactive
    );

    b.http.respond(200, reply(&material.sign("forged")));
    services
        .refresh_licences(&mut b, &mut settings, ms(20_000))
        .unwrap();
    signing_replies(&mut services);
    let effects = tick(&mut services, &mut b, &mut settings, ms(20_060));
    assert_eq!(
        texts(&effects),
        ["Failed to fetch license info: unexpected cloud reply: licence reply signature mismatch"]
    );
    assert!(to_dsp(&effects).is_empty());
}

#[test]
fn update_check_without_new_version_reports_status_three() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    // After the link: the automatic first check (no serial yet) opens no batch.
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    b.http.respond(200, r#"{"message":"You are up to date"}"#);

    let started = services
        .check_for_update(&mut b, &mut settings, None, false, ms(1_000))
        .unwrap();
    assert_signing_requests(&started);
    assert!(b.http.requests.is_empty(), "no HTTP before the replies");

    signing_replies(&mut services);
    let effects = tick(&mut services, &mut b, &mut settings, ms(1_060));

    let request = &b.http.requests[0];
    assert!(request.url.ends_with("/firmware/v1/latest"), "{request:?}");
    let signing: Vec<&str> = request
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| SIGNING_HEADERS.contains(name))
        .collect();
    assert_eq!(signing, SIGNING_HEADERS);
    assert_eq!(
        request.header_value("X-Cloud-Api-Client-Serial-Number"),
        Some("Q0000TEST")
    );
    let reports: Vec<_> = started
        .iter()
        .chain(&effects)
        .filter_map(|effect| match effect {
            Effect::UpdaterReport(message) => Some((message.state, message.status)),
            _ => None,
        })
        .collect();
    assert_eq!(
        reports,
        [
            (Some(EnumValue::Known(UpdaterState::RequestingData)), None),
            (Some(EnumValue::Known(UpdaterState::Idle)), None),
            (
                None,
                Some(EnumValue::Known(UpdaterStatus::NoNewVersionAvailable))
            ),
        ]
    );
    assert_eq!(texts(&effects), ["no new version: You are up to date"]);
    assert_eq!(services.updater().status, FlagStatus::NoNewVersionAvailable);
}

#[test]
fn signing_replies_missing_time_out_to_unsupported() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    b.files.put(SERIAL_PATH, "Q0000TEST\n");

    services
        .check_for_update(&mut b, &mut settings, None, false, ms(1_000))
        .unwrap();
    let early = services.tick(&mut b, &mut settings, ms(2_999)).unwrap();
    assert!(texts(&early).is_empty(), "{early:?}");
    assert!(b.http.requests.is_empty());

    let expired = services.tick(&mut b, &mut settings, ms(3_000)).unwrap();
    assert_eq!(
        texts(&expired),
        ["update check failed: not supported yet: signed update check: no DSP signing reply"]
    );
    assert!(b.http.requests.is_empty());
}

/// Review w2c-io R4: the probe (up to 40 s) and NTP (up to 30 s) never run on the event loop.
/// The link change only hands out a probe job.
#[test]
fn failed_probe_reports_nothing_and_skips_time_sync() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();

    let effects = services.set_link(&mut b, true, ms(0)).unwrap();
    assert!(b.net.log.is_empty(), "no network call on the event loop");
    assert!(matches!(
        effects.as_slice(),
        [Effect::Service(ServiceJob::Internet(_))]
    ));
    let effects = settle(&mut services, &mut b, &mut settings, effects, ms(0));

    assert!(effects.is_empty(), "already disconnected: no status change");
    assert!(
        b.net.log.iter().all(|line| !line.starts_with("resolve")),
        "NTP must not run without a probe: {:?}",
        b.net.log
    );
}

#[test]
fn recheck_waits_for_interval_then_reports_lost_internet() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));

    let early = services.tick(&mut b, &mut settings, ms(1_000)).unwrap();
    assert!(
        !early
            .iter()
            .any(|effect| matches!(effect, Effect::Service(_))),
        "no probe before the recheck time"
    );

    let late = services.tick(&mut b, &mut settings, ms(5_000)).unwrap();
    let late = settle(&mut services, &mut b, &mut settings, late, ms(5_000));
    assert!(texts(&late).contains(&"internet unreachable".to_owned()));
}

/// One service tick with every job it starts run to the end (probes included); the notice
/// lines of the internet recovery.
fn recovery_tick(
    services: &mut Services,
    b: &mut Backends,
    settings: &mut DeviceSettings,
    now: u64,
) -> Vec<String> {
    let effects = services.tick(b, settings, ms(now)).unwrap();
    let effects = settle(services, b, settings, effects, ms(now));
    texts(&effects)
        .into_iter()
        .filter(|line| line.contains("recovery") || line.contains("renewal"))
        .collect()
}

/// Association with a saved network, a lease and a connected probe at 0 ms; the probe at 5 s
/// finds the internet lost while the link stays associated (CorOS 4.1.0, 2026-10-17).
fn internet_lost() -> (Services, Backends, DeviceSettings) {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    let effects = services
        .on_notice(
            &mut b,
            ServiceNotice::Supplicant(CONNECTED.into()),
            &mut settings,
            ms(0),
        )
        .unwrap();
    b.runner
        .expect(
            "udhcpc --now --quit -i wlan0",
            Ok(Output::code(0)
                .with_stdout("udhcpc: Lease of 192.0.2.40 obtained, lease time 600\n")),
        )
        .expect(
            "ifconfig wlan0",
            Ok(Output::code(0).with_stdout(
                "          inet addr:192.0.2.40  Bcast:192.0.2.255  Mask:255.255.255.0\n",
            )),
        )
        .expect("route -n", Ok(Output::code(0)));
    internet_up(&mut b);
    settle(&mut services, &mut b, &mut settings, effects, ms(0));
    let effects = services.tick(&mut b, &mut settings, ms(5_000)).unwrap();
    let lines = texts(&settle(
        &mut services,
        &mut b,
        &mut settings,
        effects,
        ms(5_000),
    ));
    assert!(
        lines.contains(&"internet unreachable".to_owned()),
        "{lines:?}"
    );
    assert!(b.runner.is_done());
    (services, b, settings)
}

/// The recovery steps as Wi-Fi actions: a DHCP renewal 60 s after the loss (its failure keeps
/// the association: no `DISCONNECT`), `REASSOCIATE` 60 s later, then `REASSOCIATE` and the
/// interface reset, again every 10 min. Each step is logged once with the wlan0 counters. A
/// connected probe ends it.
#[test]
fn a_lost_internet_runs_the_recovery_steps_as_wifi_actions() {
    let (mut services, mut b, mut settings) = internet_lost();
    let stats = "/sys/class/net/wlan0/statistics";
    for (name, value) in [
        ("rx_packets", "10"),
        ("tx_packets", "20"),
        ("rx_errors", "0"),
        ("tx_errors", "3"),
        ("rx_dropped", "0"),
        ("tx_dropped", "1"),
    ] {
        b.files
            .put(&format!("{stats}/{name}"), format!("{value}\n"));
    }
    assert!(recovery_tick(&mut services, &mut b, &mut settings, 64_999).is_empty());

    b.runner
        .expect("udhcpc --now --quit -i wlan0", Ok(Output::code(1)));
    assert_eq!(
        recovery_tick(&mut services, &mut b, &mut settings, 65_000),
        [
            "internet lost for 60 s: recovery step 1 (renew the DHCP lease); wlan0 rx_packets 10 \
             tx_packets 20 rx_errors 0 tx_errors 3 rx_dropped 0 tx_dropped 1",
            "DHCP renewal failed; the association stays",
        ]
    );
    assert!(!b.link.sent.contains(&"DISCONNECT".to_owned()));
    assert!(recovery_tick(&mut services, &mut b, &mut settings, 124_999).is_empty());

    let lines = recovery_tick(&mut services, &mut b, &mut settings, 125_000);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("internet lost for 120 s: recovery step 2 (reassociate);"));
    assert_eq!(b.link.sent.last().map(String::as_str), Some("REASSOCIATE"));
    b.link.sent.clear();

    for (now, lost) in [(185_000, 180), (785_000, 780)] {
        b.runner
            .expect_ok("ifconfig wlan0 down")
            .expect_ok("ifconfig wlan0 up");
        let lines = recovery_tick(&mut services, &mut b, &mut settings, now);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with(&format!(
                "internet lost for {lost} s: recovery step 3 (reset the interface);"
            )),
            "{lines:?}"
        );
        assert!(b.runner.is_done(), "at {now}");
        assert_eq!(b.link.sent.last().map(String::as_str), Some("REASSOCIATE"));
        b.link.sent.clear();
        assert!(recovery_tick(&mut services, &mut b, &mut settings, now + 1).is_empty());
    }

    b.net.tcp.push_back(Ok(()));
    let effects = services.tick(&mut b, &mut settings, ms(790_000)).unwrap();
    let lines = texts(&settle(
        &mut services,
        &mut b,
        &mut settings,
        effects,
        ms(790_000),
    ));
    assert!(
        lines.contains(&"internet reachable".to_owned()),
        "{lines:?}"
    );
    assert!(recovery_tick(&mut services, &mut b, &mut settings, 10_000_000).is_empty());
}

/// No recovery step during a CorOS install (the step waits for its end), and none after the
/// user leaves the network.
#[test]
fn the_recovery_waits_for_an_install_and_ends_on_a_user_disconnect() {
    let (mut services, mut b, mut settings) = internet_lost();
    services
        .on_notice(
            &mut b,
            ServiceNotice::Download(Ok(())),
            &mut settings,
            ms(6_000),
        )
        .unwrap();
    services.install().unwrap();
    assert!(recovery_tick(&mut services, &mut b, &mut settings, 65_000).is_empty());

    services
        .on_notice(
            &mut b,
            ServiceNotice::Install(Err(JobError::WorkerStopped)),
            &mut settings,
            ms(70_000),
        )
        .unwrap();
    b.runner
        .expect("udhcpc --now --quit -i wlan0", Ok(Output::code(1)));
    let lines = recovery_tick(&mut services, &mut b, &mut settings, 70_001);
    assert!(lines[0].contains("recovery step 1"), "{lines:?}");

    services.disconnect(&mut b).unwrap();
    assert!(recovery_tick(&mut services, &mut b, &mut settings, 10_000_000).is_empty());
    assert!(b.runner.is_done());
}

#[test]
fn ntp_failure_is_reported_after_a_reachable_probe() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    b.net.tcp.push_back(Ok(()));
    b.net.names.push_back(Err(NetError::NoAddress));

    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    let lines = texts(&settle(&mut services, &mut b, &mut settings, linked, ms(0)));

    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], "internet reachable");
    assert!(lines[1].starts_with("NTP failed:"), "{lines:?}");
}

/// Message 60 uses distinct fields, changed values and release notes; progress comes from files.
#[test]
fn updater_notifications_use_stock_shapes_and_only_changed_values() {
    use tetrad_control::codecs::{MessageAction, UpdaterMessage};
    use tetrad_services::updater::paths;
    let messages = |effects: Vec<Effect>| {
        effects
            .into_iter()
            .filter_map(|effect| {
                if let Effect::UpdaterReport(message) = effect {
                    Some(message)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    let base = UpdaterMessage {
        action: Some(MessageAction::Update.into()),
        request_id: Some(0),
        ..Default::default()
    };
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    b.http.respond(200, r#"{"url":"https://example.invalid/update.bin","versionId":"4.2.0","changelogUrl":"https://example.invalid/notes"}"#);
    b.http.respond(200, "4.\t2.0\r\nFix input switching\n");
    let mut reports = messages(
        services
            .check_for_update(&mut b, &mut settings, None, false, ms(1_000))
            .unwrap(),
    );
    signing_replies(&mut services);
    reports.extend(messages(tick(
        &mut services,
        &mut b,
        &mut settings,
        ms(1_060),
    )));
    let available = UpdaterMessage {
        status: Some(UpdaterStatus::NewVersionAvailableToDownload.into()),
        new_version_id: Some("4.2.0".into()),
        changelog: Some("Fix input switching\n".into()),
        ..base.clone()
    };
    assert_eq!(reports.last(), Some(&available));
    assert!(
        reports[..reports.len() - 1]
            .iter()
            .all(|m| m.state.is_some() && m.status.is_none() && m.new_version_id.is_none())
    );
    let started = messages(services.download().unwrap());
    assert_eq!(
        started,
        [UpdaterMessage {
            state: Some(UpdaterState::DownloadingData.into()),
            ..base.clone()
        }]
    );
    b.files.put(paths::DOWNLOAD_FNAME, paths::UPDATE_BIN);
    b.files.put(paths::DOWNLOAD_FSIZE, "200");
    b.files.put(paths::UPDATE_BIN, vec![0u8; 100]);
    let progress = messages(services.tick(&mut b, &mut settings, ms(1001)).unwrap());
    assert_eq!(
        progress,
        [UpdaterMessage {
            download_progress: Some(47.5),
            ..base.clone()
        }]
    );
    assert!(messages(services.tick(&mut b, &mut settings, ms(1002)).unwrap()).is_empty());
    let done = messages(
        services
            .on_notice(
                &mut b,
                ServiceNotice::Download(Ok(())),
                &mut settings,
                ms(1003),
            )
            .unwrap(),
    );
    assert_eq!(
        done,
        [
            UpdaterMessage {
                state: Some(UpdaterState::Idle.into()),
                ..base.clone()
            },
            UpdaterMessage {
                status: Some(UpdaterStatus::NewVersionDownloaded.into()),
                ..available
            },
        ]
    );
    assert!(
        messages(
            services
                .on_notice(
                    &mut b,
                    ServiceNotice::Download(Ok(())),
                    &mut settings,
                    ms(1004)
                )
                .unwrap()
        )
        .is_empty()
    );
    messages(services.install().unwrap());
    b.files.put(paths::STATUS_FILE, "UPDATER_STEP_1\n");
    assert_eq!(
        messages(services.tick(&mut b, &mut settings, ms(1005)).unwrap()),
        [UpdaterMessage {
            installation_progress: Some((100.0_f64 / 14.0) as f32),
            ..base.clone()
        }]
    );
    b.files.put(paths::STATUS_FILE, "UPDATER_STEP_7\n");
    assert_eq!(
        messages(services.tick(&mut b, &mut settings, ms(1005)).unwrap()),
        [UpdaterMessage {
            installation_progress: Some(50.0),
            ..base
        }]
    );
    assert!(messages(services.tick(&mut b, &mut settings, ms(1006)).unwrap()).is_empty());
}

fn bss(bssid: &str, ssid: &str, level: i32, flags: &str) -> String {
    format!("bssid={bssid}\nfreq=2412\nlevel={level}\nflags={flags}\nssid={ssid}\n")
}

/// w4 settings views: scan results are read when the supplicant reports them, saved and
/// joined flags come from `LIST_NETWORKS`, and a failed lease shows the stock
/// "Failed to obtain IP address" until the next attempt.
#[test]
fn connection_page_follows_scan_association_and_lease() {
    use tetrad_ui::{WifiAlert, WifiStatus};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    b.link.bss = vec![
        bss("02:00:00:00:00:01", "Home", -40, "[WPA2-PSK-CCMP][ESS]"),
        bss("02:00:00:00:00:02", "Cafe", -85, "[ESS]"),
    ];
    let scanned = ServiceNotice::Supplicant("<3>CTRL-EVENT-SCAN-RESULTS ".into());
    services
        .on_notice(&mut b, scanned, &mut settings, ms(0))
        .unwrap();
    let view = services.wifi_view();
    assert_eq!(view.status, WifiStatus::NoInternet);
    let summary: Vec<_> = view
        .networks
        .iter()
        .map(|n| (n.ssid.as_str(), n.signal, n.secured, n.saved, n.connected))
        .collect();
    assert_eq!(
        summary,
        [
            ("Home", 5, true, true, false),
            ("Cafe", 0, false, false, false)
        ]
    );

    let effects = services
        .on_notice(
            &mut b,
            ServiceNotice::Supplicant(CONNECTED.into()),
            &mut settings,
            ms(10),
        )
        .unwrap();
    let view = services.wifi_view();
    assert_eq!(view.status, WifiStatus::NetworkCheck);
    assert!(view.networks[0].connected);
    // A background scan moves the supplicant state off COMPLETED; the joined link keeps its
    // title (the QC showed "No Internet Connection" while online, 2026-10-16).
    let scanning = ServiceNotice::Supplicant("<3>CTRL-EVENT-SCAN-STARTED ".into());
    services
        .on_notice(&mut b, scanning, &mut settings, ms(11))
        .unwrap();
    assert_eq!(services.wifi_view().status, WifiStatus::NetworkCheck);
    // Stock retries DHCP before it reports the failure; every run fails here.
    let dhcp = |effects: Vec<Effect>| {
        effects.into_iter().find_map(|effect| match effect {
            Effect::Service(job) => Some(job),
            _ => None,
        })
    };
    let mut job = dhcp(effects);
    while let Some(run) = job {
        assert_eq!(services.wifi_view().alert, None);
        let effects = services
            .on_notice(&mut b, run.lost(), &mut settings, ms(20))
            .unwrap();
        job = dhcp(effects);
    }
    assert_eq!(services.wifi_view().alert, Some(WifiAlert::NoIpAddress));
    services.connect(&mut b, "Home", None, ms(30)).unwrap();
    assert_eq!(services.wifi_view().alert, None);
}

/// Backups page: `GET /backups/v1` fills the list (item shape inferred); DELETE goes to the
/// listed server path and lists again.
#[test]
fn backups_list_and_delete_by_server_path() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    // With a serial the session keeps its tokens; the pair stays queued.
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    b.http.respond(200, token_reply());
    let started = services
        .sign_in(
            &mut b,
            &mut settings,
            "player@example.com",
            "password-synthetic",
            ms(0),
        )
        .unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(0));
    // `0x30eb7c`: an item without every `BackupData` member and one from another client
    // type (`0x45bb04`) are left out.
    let other_client = backup_data("Native", "/backups/v1/n1", "k-n1")
        .replace("\"quad_cortex\"", "\"native_plugin\"");
    let list = backup_list(&[
        backup_data("My Rig", "/backups/v1/a1", "k-a1"),
        backup_data("Old", "/backups/v1/b2", "k-b2"),
        r#"{"backupUrl":"/backups/v1/nameless"}"#.to_owned(),
        other_client,
    ]);
    b.http.respond(200, list);
    let listed = services
        .refresh_backups(&mut b, &mut settings, ms(1_000))
        .unwrap();
    settle(&mut services, &mut b, &mut settings, listed, ms(1_000));
    let view = services.backup_view();
    let titles: Vec<_> = view.items.iter().map(|item| item.title.as_str()).collect();
    assert_eq!(titles, ["My Rig", "Old"]);
    assert_eq!(view.items[0].detail, "2025-08-29T17:06:00Z");

    b.http.respond(200, "{}");
    b.http.respond(
        200,
        backup_list(&[backup_data("Old", "/backups/v1/b2", "k-b2")]),
    );
    let id = view.items[0].id;
    let deleted = services
        .on_ui(
            &mut b,
            tetrad_ui::UiCommand::DeleteBackup { id },
            &mut settings,
            ms(2_000),
        )
        .unwrap();
    settle(&mut services, &mut b, &mut settings, deleted, ms(2_000));
    let delete = &b.http.requests[b.http.requests.len() - 2];
    assert!(delete.url.ends_with("/backups/v1/a1"), "{delete:?}");
    assert_eq!(services.backups().len(), 1);
}

/// Cloud backup create: pack (`0x004774d8`) on the bulk lane, `generateBackupUrl`
/// (`0x00303300`) with the `BackupEditDialog` name, the upload, a list refresh. Rename
/// (`0x3fb0f4` → `0x30d054`): `POST <path>/rename` with the new name, then the list. Restore:
/// the backup's url (`0x0030d4b0`), the body, `0x00406844` on the bulk lane, then a restart
/// (review r18 F1: no full reload yet).
#[test]
fn cloud_backup_create_then_restore() {
    let mut services = Services::new();
    let mut b = Backends::default();
    b.http = ScriptedHttp::sharing(b.files.clone());
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    b.http.respond(200, token_reply());
    let started = services
        .sign_in(&mut b, &mut settings, "player@example.com", "pw", ms(0))
        .unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(0));
    b.files.put("/media/p4/Presets/User/a.pb", vec![1u8, 2, 3]);
    b.files.put("/media/p4/zenui.db", "db");

    let create = tetrad_ui::UiCommand::CreateBackup {
        name: "GigRig".into(),
    };
    let packing = services
        .on_ui(&mut b, create, &mut settings, ms(1_000))
        .unwrap();
    assert!(matches!(packing.as_slice(), [Effect::Service(job)] if job.lane() == Lane::Bulk));
    assert_eq!(
        services.backup_view().busy,
        Some(("Creating backup".to_owned(), None))
    );
    b.http.respond(200, backup_target("/backups/v1/c3"));
    b.http.respond(200, "");
    b.http.respond(200, r#"{"status":"completed"}"#);
    b.http.respond(
        200,
        backup_list(&[backup_data("New", "/backups/v1/c3", "k-c3")]),
    );
    let created = settle(&mut services, &mut b, &mut settings, packing, ms(1_000));
    assert!(texts(&created).contains(&"Backup created".to_owned()));
    let sent = &b.http.requests[b.http.requests.len() - 4..];
    assert!(
        sent[0]
            .url
            .ends_with("/backups/v1/generateBackupUrl/GigRig"),
        "{:?}",
        sent[0]
    );
    // re-r34 §3 G1/G2: the PUT body is the cloud backup file (`0x47a1c8`, no stamp), sent as
    // JSON (`0x30b73c`); G3: then one `<backupUrl>/status` GET (`0x30a83c`).
    assert_eq!(sent[1].method, Method::Put);
    let header = |key: &str| {
        (sent[1].headers.iter())
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("Content-Type"), Some("application/json"));
    let file = sent[1].body.clone().unwrap();
    let wrapper: serde_json::Value = serde_json::from_slice(&file).unwrap();
    assert_eq!(wrapper["type"], "backup");
    assert_eq!(wrapper["creator"], "quad");
    assert_eq!(wrapper["author"], "player@example.com");
    let archive =
        tetrad_services::backup::local::decode_cloud(std::str::from_utf8(&file).unwrap(), "4.1.0")
            .unwrap();
    assert!(archive.starts_with(&[0x1f, 0x8b]));
    assert_eq!(sent[2].method, Method::Get);
    assert!(
        sent[2].url.ends_with("/backups/v1/c3/status"),
        "{:?}",
        sent[2]
    );
    assert!(
        b.files
            .get("/media/p4/tetrad_backup_upload.tar.gz")
            .is_none()
    );
    assert_eq!(services.backup_view().busy, None);

    let id = services.backup_view().items[0].id;
    b.http.respond(200, "{}");
    b.http.respond(
        200,
        backup_list(&[backup_data("Studio", "/backups/v1/c3", "k-c3")]),
    );
    let rename = tetrad_ui::UiCommand::RenameBackup {
        id,
        name: "Studio".into(),
    };
    let started = (services.on_ui(&mut b, rename, &mut settings, ms(1_500))).unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(1_500));
    let sent = &b.http.requests[b.http.requests.len() - 2..];
    assert_eq!(sent[0].method, Method::Post);
    assert!(
        sent[0].url.ends_with("/backups/v1/c3/rename"),
        "{:?}",
        sent[0]
    );
    let body: serde_json::Value = serde_json::from_slice(sent[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body, serde_json::json!({"name": "Studio"}));
    assert_eq!(services.backup_view().items[0].title, "Studio");
    assert_eq!(
        services.backup_view().items[0].id,
        id,
        "the id follows backupKey"
    );

    b.files
        .put("/media/p4/Presets/User/b.pb", "made after the backup");
    // Review r23 K8: the backup's hardware is the `creator` of this reply (`0x407908`); the
    // list item above has none.
    b.http.respond(
        200,
        r#"{"url":"https://downloads.example/b?sig=t","creator":"atma"}"#,
    );
    // The body is the file this create uploaded (`0x407908` → `0x4072fc(.., 0, ..)`).
    b.http.respond(200, file);
    let id = services.backup_view().items[0].id;
    let restore = tetrad_ui::UiCommand::RestoreBackup { id };
    let started = services
        .on_ui(&mut b, restore, &mut settings, ms(2_000))
        .unwrap();
    let restored = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert!(!restored.contains(&Effect::LibraryReopen));
    // `0x3fcf64`: the device reloads in place; its first flush converts the Atma levels.
    assert_eq!(restored.last(), Some(&Effect::RestoreReload));
    assert!(b.files.get("/media/p4/Presets/User/b.pb").is_none());
    assert_eq!(
        b.files.get("/media/p4/Presets/User/a.pb").unwrap(),
        [1, 2, 3]
    );
    assert!(
        b.http
            .requests
            .last()
            .unwrap()
            .url
            .starts_with("https://downloads.example/")
    );
}

/// A signed-in bridge with CREATE "Rig" out (the pack job).
fn backup_started() -> (Services, Backends, DeviceSettings, Vec<Effect>) {
    let (mut services, mut b, mut settings) = (Services::new(), Backends::default(), settings());
    b.http = ScriptedHttp::sharing(b.files.clone());
    signed_in(&mut services, &mut b, &mut settings);
    b.files.put("/media/p4/zenui.db", "db");
    let create = tetrad_ui::UiCommand::CreateBackup { name: "Rig".into() };
    let packing = services
        .on_ui(&mut b, create, &mut settings, ms(2_000))
        .unwrap();
    (services, b, settings, packing)
}

/// re-r34 §3 G3 (`0x30a83c` → `0x3084fc` → `0x30e5c0`): after the PUT the create polls
/// `<backupUrl>/status` once a second; "pending" waits, "completed" ends it well, an error on
/// `/status` also ends it well, and 60 s without "completed" fails it.
#[test]
fn a_backup_create_polls_its_status_until_completed_or_60_s() {
    let statuses = |b: &Backends| {
        (b.http.requests.iter())
            .filter(|request| request.url.ends_with("/backups/v1/c3/status"))
            .count()
    };
    let (mut services, mut b, mut settings, packing) = backup_started();
    b.http.respond(200, backup_target("/backups/v1/c3"));
    b.http.respond(200, "");
    b.http.respond(200, r#"{"status":"pending"}"#);
    let waiting = settle(&mut services, &mut b, &mut settings, packing, ms(2_000));
    assert!(!texts(&waiting).contains(&"Backup created".to_owned()));
    assert_eq!(statuses(&b), 1);
    assert!(tick(&mut services, &mut b, &mut settings, ms(2_500)).is_empty());
    assert_eq!(statuses(&b), 1, "the next GET waits one second");
    b.http.respond(200, r#"{"status":"completed"}"#);
    b.http.respond(200, r#"{"backups":[]}"#);
    let created = tick(&mut services, &mut b, &mut settings, ms(3_000));
    assert!(
        texts(&created).contains(&"Backup created".to_owned()),
        "{created:?}"
    );
    assert_eq!(statuses(&b), 2);

    // An HTTP error on `/status` ends as a good create ("Backup upload status error!").
    let (mut services, mut b, mut settings, packing) = backup_started();
    b.http.respond(200, backup_target("/backups/v1/c3"));
    b.http.respond(200, "");
    b.http.respond(503, "");
    b.http.respond(200, r#"{"backups":[]}"#);
    let created = settle(&mut services, &mut b, &mut settings, packing, ms(2_000));
    assert!(
        texts(&created).contains(&"Backup created".to_owned()),
        "{created:?}"
    );

    // Pending for more than 60 s from the PUT reply: the create fails, no more GETs.
    let (mut services, mut b, mut settings, packing) = backup_started();
    b.http.respond(200, backup_target("/backups/v1/c3"));
    b.http.respond(200, "");
    b.http.respond(200, r#"{"status":"pending"}"#);
    settle(&mut services, &mut b, &mut settings, packing, ms(2_000));
    let before = statuses(&b);
    b.http.respond(200, r#"{"status":"pending"}"#);
    tick(&mut services, &mut b, &mut settings, ms(30_000));
    let failed = tick(&mut services, &mut b, &mut settings, ms(62_001));
    assert!(
        texts(&failed).contains(&"backup failed: Backup upload validation timeout!".to_owned()),
        "{failed:?}"
    );
    assert_eq!(statuses(&b), before + 1);
    assert_eq!(services.backup_view().busy, None);
}

/// Review r33 F2 / re-r34 §3: backup ⋮ Update of a listed backup packs, asks `GET
/// <backupUrl>/update` (empty body), PUTs the backup file and polls `/status`; it never sends
/// a DELETE. It ends as "Backup updated" with no Control answer (review r35 G7). A failed PUT
/// removes the archive and sends nothing more. An id that is not listed sends nothing.
#[test]
fn a_backup_update_puts_a_new_file_and_never_deletes() {
    let (mut services, mut b, mut settings) = (Services::new(), Backends::default(), settings());
    b.http = ScriptedHttp::sharing(b.files.clone());
    signed_in(&mut services, &mut b, &mut settings);
    b.files.put("/media/p4/zenui.db", "db");
    b.http.respond(
        200,
        backup_list(&[backup_data("Gig", "/backups/v1/g1", "k-g1")]),
    );
    let listed = services
        .refresh_backups(&mut b, &mut settings, ms(2_000))
        .unwrap();
    settle(&mut services, &mut b, &mut settings, listed, ms(2_000));
    let id = services.backup_view().items[0].id;
    let sent = b.http.requests.len();
    let unlisted = tetrad_ui::UiCommand::UpdateBackup { id: id ^ 1 };
    assert!(
        services
            .on_ui(&mut b, unlisted, &mut settings, ms(2_000))
            .is_err()
    );
    assert_eq!(b.http.requests.len(), sent);

    b.http.respond(200, backup_target("/backups/v1/g1"));
    b.http.respond(200, "");
    b.http.respond(200, r#"{"status":"completed"}"#);
    b.http.respond(
        200,
        backup_list(&[backup_data("Gig", "/backups/v1/g1", "k-g1")]),
    );
    let update = tetrad_ui::UiCommand::UpdateBackup { id };
    let packing = services
        .on_ui(&mut b, update, &mut settings, ms(3_000))
        .unwrap();
    let done = settle(&mut services, &mut b, &mut settings, packing, ms(3_000));
    // Review r35 G7: an update is no create; Control 41 has no update to answer.
    let ends = texts(&done);
    assert!(ends.contains(&"Backup updated".to_owned()), "{done:?}");
    assert!(!ends.contains(&"Backup created".to_owned()), "{done:?}");
    assert!(
        !(done.iter()).any(|effect| matches!(effect, Effect::CloudBackupDone(_))),
        "{done:?}"
    );
    let calls = &b.http.requests[sent..];
    let urls: Vec<_> = calls.iter().map(|r| (r.method, r.url.as_str())).collect();
    assert_eq!(
        urls,
        [
            (
                Method::Get,
                "https://cloud-api.neuraldsp.com/backups/v1/g1/update"
            ),
            (Method::Put, "https://uploads.example/b?sig=s"),
            (
                Method::Get,
                "https://cloud-api.neuraldsp.com/backups/v1/g1/status"
            ),
            (Method::Get, "https://cloud-api.neuraldsp.com/backups/v1"),
        ]
    );
    assert!(calls[0].body.is_none());
    let file: serde_json::Value = serde_json::from_slice(calls[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(file["type"], "backup");

    let sent = b.http.requests.len();
    b.http.respond(200, backup_target("/backups/v1/g1"));
    b.http.respond(500, "");
    let update = tetrad_ui::UiCommand::UpdateBackup { id };
    let packing = services
        .on_ui(&mut b, update, &mut settings, ms(4_000))
        .unwrap();
    let done = settle(&mut services, &mut b, &mut settings, packing, ms(4_000));
    assert!(
        texts(&done)
            .iter()
            .any(|text| text.starts_with("backup failed")),
        "{done:?}"
    );
    assert_eq!(
        b.http.requests.len(),
        sent + 2,
        "no call after the failed PUT"
    );
    assert!(
        b.files
            .get("/media/p4/tetrad_backup_upload.tar.gz")
            .is_none()
    );
    assert!(!(b.http.requests.iter()).any(|request| request.method == Method::Delete));
}

/// Health r34 #4: a clean-up that fails after a good PUT is a notice; the create goes on to
/// its status poll and ends well, and no second upload goes out.
#[test]
fn a_failed_backup_clean_up_does_not_hide_a_good_upload() {
    let (mut services, mut b, mut settings, packing) = backup_started();
    b.files.fail_remove("/media/p4/tetrad_backup_upload.tar.gz");
    b.http.respond(200, backup_target("/backups/v1/c3"));
    b.http.respond(200, "");
    b.http.respond(200, r#"{"status":"completed"}"#);
    b.http.respond(200, r#"{"backups":[]}"#);
    let created = settle(&mut services, &mut b, &mut settings, packing, ms(2_000));
    let texts = texts(&created);
    assert!(texts.contains(&"Backup created".to_owned()), "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|text| text.starts_with("backup upload clean-up:")),
        "{texts:?}"
    );
    let puts = (b.http.requests.iter()).filter(|request| request.method == Method::Put);
    assert_eq!(puts.count(), 1);
}

/// r23-control MR 1: a Cortex Control CloudBackup (41) create runs under Control's name
/// (`createBackup(name, …)`, `0x150260`) and answers Control once, at its upload; the list
/// refresh after it answers nothing. A Backups page create never answers Control.
#[test]
fn a_control_cloud_backup_answers_control_when_its_flow_ends() {
    use tetrad_app::device::control::{CloudBackupDone, CloudBackupRequest};
    let mut services = Services::new();
    let mut b = Backends::default();
    b.http = ScriptedHttp::sharing(b.files.clone());
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    b.http.respond(200, token_reply());
    let started = services
        .sign_in(&mut b, &mut settings, "player@example.com", "pw", ms(0))
        .unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(0));
    b.files.put("/media/p4/zenui.db", "db");
    let answers = |effects: &[Effect]| {
        (effects.iter())
            .filter(|effect| matches!(effect, Effect::CloudBackupDone(_)))
            .cloned()
            .collect::<Vec<_>>()
    };
    let upload = |b: &mut Backends| {
        b.http.respond(200, backup_target("/backups/v1/c3"));
        b.http.respond(200, "");
        b.http.respond(200, r#"{"status":"completed"}"#);
        b.http.respond(200, r#"{"backups":[]}"#);
    };

    upload(&mut b);
    let request = CloudBackupRequest::Create {
        name: "Stage-Rig".into(),
    };
    let started = services.control_cloud_backup(&mut b, &mut settings, request, ms(1_000));
    let created = settle(&mut services, &mut b, &mut settings, started, ms(1_000));
    assert_eq!(
        answers(&created),
        [Effect::CloudBackupDone(CloudBackupDone::Done)]
    );
    let target = &b.http.requests[b.http.requests.len() - 4];
    assert!(
        target.url.ends_with("/generateBackupUrl/Stage-Rig"),
        "{target:?}"
    );

    upload(&mut b);
    let create = tetrad_ui::UiCommand::CreateBackup {
        name: String::new(),
    };
    let started = services
        .on_ui(&mut b, create, &mut settings, ms(2_000))
        .unwrap();
    let created = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert!(texts(&created).contains(&"Backup created".to_owned()));
    assert!(answers(&created).is_empty());
}

/// Review r39 K11 (C: `0x150c08`, `0x150ab8`, `0x152218`): a Cortex Control 41 rename lists
/// first, finds its `key` in that list, posts the rename, then lists again and answers with
/// the new list.
#[test]
fn a_control_backup_rename_lists_before_and_after_the_post() {
    use tetrad_app::device::control::{CloudBackupDone, CloudBackupRequest};
    let mut services = Services::new();
    let mut b = Backends::default();
    b.http = ScriptedHttp::sharing(b.files.clone());
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let answers = |effects: &[Effect]| {
        (effects.iter())
            .filter_map(|effect| match effect {
                Effect::CloudBackupDone(done) => Some(done.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let rename = |services: &mut Services, b: &mut Backends, settings: &mut _, at| {
        let mark = b.http.requests.len();
        let request = CloudBackupRequest::Rename {
            key: "/backups/v1/c3".into(),
            name: "Studio".into(),
        };
        let started = services.control_cloud_backup(b, settings, request, ms(at));
        let done = settle(services, b, settings, started, ms(at));
        let sent: Vec<_> = (b.http.requests[mark..].iter())
            .map(|request| match (request.method, request.url.as_str()) {
                (Method::Get, url) if url.ends_with("/backups/v1") => "list",
                (Method::Post, url) if url.ends_with("/backups/v1/c3/rename") => "rename",
                _ => "other",
            })
            .collect();
        (answers(&done), sent)
    };
    let listed = |name| backup_list(&[backup_data(name, "/backups/v1/c3", "k-c3")]);

    b.http.respond(200, listed("Old"));
    b.http.respond(200, "{}");
    b.http.respond(200, listed("Studio"));
    let (done, sent) = rename(&mut services, &mut b, &mut settings, 1_000);
    assert_eq!(done, [CloudBackupDone::Done]);
    assert_eq!(sent, ["list", "rename", "list"]);
    assert_eq!(services.backups()[0].title, "Studio");

    // The first list fails → 15 (`0x150c08`); nothing is posted.
    b.http.respond(500, "{}");
    let (done, sent) = rename(&mut services, &mut b, &mut settings, 2_000);
    assert!(
        matches!(done[..], [CloudBackupDone::ListFailed(_)]),
        "{done:?}"
    );
    assert_eq!(sent, ["list"]);
    // The key is not in the new list → 7.
    b.http.respond(200, backup_list(&[]));
    let (done, sent) = rename(&mut services, &mut b, &mut settings, 3_000);
    assert_eq!(
        (done, sent),
        (vec![CloudBackupDone::NotFound], vec!["list"])
    );
    // The POST fails → 6 (`0x150ab8`).
    b.http.respond(200, listed("Studio"));
    b.http.respond(500, "{}");
    let (done, sent) = rename(&mut services, &mut b, &mut settings, 4_000);
    assert!(matches!(done[..], [CloudBackupDone::Failed(_)]), "{done:?}");
    assert_eq!(sent, ["list", "rename"]);
    // The list after a good POST fails → 15 (C, `0x152218`), with no list.
    b.http.respond(200, listed("Studio"));
    b.http.respond(200, "{}");
    b.http.respond(500, "{}");
    let (done, sent) = rename(&mut services, &mut b, &mut settings, 5_000);
    assert!(
        matches!(done[..], [CloudBackupDone::ListFailed(_)]),
        "{done:?}"
    );
    assert_eq!(sent, ["list", "rename", "list"]);
}

/// `0x0071d538` → `0x0045fa44`: a product that fails its check is reported as corrupt (code
/// -20) and not handed on. I: Tetrad's check is an empty payload.
#[test]
fn an_empty_nc2_product_is_corrupt() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    b.http.respond(200, "");
    b.http.respond(200, r#"{"name":"Clean"}"#);
    let request = CloudRequest::Nc2Download("p-1".into());
    let started = services.cloud_request(&mut settings, request, ms(2_000));
    let done = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert!(done.contains(&Effect::Nc2(Nc2Input::Downloaded(None))));
    assert!(
        texts(&done)
            .iter()
            .any(|text| text.starts_with("Downloaded product Clean (p-1) is corrupt.")),
        "{done:?}"
    );
}

/// Gap #18: a cloud request is a service job, never HTTP on the event loop. While a job holds
/// the session, the next request waits and goes out when the reply comes back.
#[test]
fn cloud_requests_run_on_the_worker_one_job_at_a_time() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    // With a serial the pair waits for the DSP, so the session keeps its tokens.
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    b.http.respond(200, token_reply());
    b.http.respond(
        200,
        backup_list(&[backup_data("My Rig", "/backups/v1/a1", "k-a1")]),
    );

    let signing_in = services
        .sign_in(
            &mut b,
            &mut settings,
            "player@example.com",
            "password-synthetic",
            ms(0),
        )
        .unwrap();
    let listing = services
        .refresh_backups(&mut b, &mut settings, ms(0))
        .unwrap();
    assert!(b.http.requests.is_empty(), "no HTTP on the event loop");
    assert!(
        listing.is_empty(),
        "the second request waits for the session"
    );
    let [Effect::Service(job @ ServiceJob::Cloud(_))] = signing_in.as_slice() else {
        panic!("{signing_in:?}");
    };

    let reply = run_job(&mut b, job.clone());
    let next = services
        .on_notice(&mut b, reply, &mut settings, ms(10))
        .unwrap();
    assert_eq!(b.http.requests.len(), 1);
    assert!(!settings.settings().tokens.access_token.is_empty());
    assert_signing_requests(&next);
    settle(&mut services, &mut b, &mut settings, next, ms(10));
    assert!(b.http.requests[1].url.contains("/backups/v1"));
    assert_eq!(services.backup_view().items.len(), 1);
}

/// NC2 job poll (`0x0020068c`) on the worker: the reply body goes to the controller as
/// `JobReply`; a transport failure as `JobFailed` with the internet state (`0x00200ddc`).
#[test]
fn nc2_job_poll_answers_the_controller() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let url = "https://cloud.example/capture/v1/jobs/job-1".to_owned();
    let nc2 = |effects: Vec<Effect>| -> Vec<Nc2Input> {
        effects
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::Nc2(input) => Some(input),
                _ => None,
            })
            .collect()
    };

    b.http.respond(200, r#"{"status":"running"}"#);
    let polled =
        services.cloud_request(&mut settings, CloudRequest::Nc2Job(url.clone()), ms(2_000));
    let polled = settle(&mut services, &mut b, &mut settings, polled, ms(2_000));
    assert_eq!(
        nc2(polled),
        [Nc2Input::JobReply(br#"{"status":"running"}"#.to_vec())]
    );
    let sent = b.http.requests.last().unwrap();
    assert_eq!(
        (sent.method, sent.url.as_str()),
        (Method::Get, url.as_str())
    );

    b.http.fail(HttpError::Timeout);
    let failed = services.cloud_request(&mut settings, CloudRequest::Nc2Job(url), ms(3_000));
    let failed = settle(&mut services, &mut b, &mut settings, failed, ms(3_000));
    assert_eq!(nc2(failed), [Nc2Input::JobFailed { online: true }]);
}

/// `startUpdate` (`0x004e63a4`): state Updating, then the input mask cleared
/// (`0x001ec83c(0x3f)`, which locks Cortex Control through `0x16ad60`) and the outputs muted
/// (`0x00876470(1, 1)`), then the installer. A failed run gives all back (owned).
#[test]
fn install_masks_inputs_and_mutes_before_the_script() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    services
        .on_notice(
            &mut b,
            ServiceNotice::Download(Ok(())),
            &mut settings,
            ms(0),
        )
        .unwrap();

    let effects = services.install().unwrap();
    let [
        Effect::UpdaterReport(state),
        Effect::ControlNotify(lock),
        Effect::Dsp(DspCommand::MasterMute(3)),
        Effect::Service(ServiceJob::Install),
    ] = effects.as_slice()
    else {
        panic!("{effects:?}");
    };
    assert_eq!(state.state, Some(EnumValue::Known(UpdaterState::Updating)));
    assert_eq!(control_lock(lock), Some(true));
    assert!(services.inputs_masked());
    assert!(services.install().unwrap().is_empty(), "one run at a time");

    let failed = services
        .on_notice(
            &mut b,
            ServiceNotice::Install(Err(JobError::WorkerStopped)),
            &mut settings,
            ms(10),
        )
        .unwrap();
    assert!(failed.contains(&Effect::Dsp(DspCommand::MasterMute(0))));
    let unlock = failed.iter().find_map(|effect| match effect {
        Effect::ControlNotify(message) => control_lock(message),
        _ => None,
    });
    assert_eq!(unlock, Some(false));
    assert!(!services.inputs_masked());
}

/// r21-ui MR 1: a Directory upload reaches services. Signed out it is refused like
/// `upload_product`; signed in it names what is still missing (r31-upload: a preset is built
/// on the device), and no request goes out.
#[test]
fn a_directory_upload_names_what_the_builder_still_needs() {
    use tetrad_ui::{UiCommand, UploadItem};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    let upload = || UiCommand::Upload {
        items: vec![
            UploadItem::Preset("/media/p4/Presets/My Presets/Lead.pb".into()),
            UploadItem::Capture("c1".into()),
        ],
    };
    let signed_out = services.on_ui(&mut b, upload(), &mut settings, ms(1_000));
    assert!(
        matches!(&signed_out, Err(error) if error.to_string().contains("signed-in")),
        "{signed_out:?}"
    );

    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    b.http.respond(200, token_reply());
    let started =
        (services.sign_in(&mut b, &mut settings, "player@example.com", "pw", ms(0))).unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(0));
    let sent = b.http.requests.len();
    let effects = services
        .on_ui(&mut b, upload(), &mut settings, ms(1_000))
        .unwrap();
    let [text] = texts(&effects).try_into().unwrap();
    assert!(text.starts_with("Upload not sent (2 item(s))"), "{text}");
    assert!(
        text.contains("Device::prepare_upload") && text.contains("one user capture at a time"),
        "{text}"
    );
    // re-r44 §1: stock 4.1.0 has no IR upload (replaces review r21 MR 1's IR text).
    let irs = UiCommand::Upload {
        items: vec![UploadItem::Ir("CIR_1".into())],
    };
    let refused = services.on_ui(&mut b, irs, &mut settings, ms(1_000));
    assert!(
        matches!(&refused, Err(error) if error.to_string().contains("no IR upload")),
        "{refused:?}"
    );
    assert_eq!(b.http.requests.len(), sent);
}

/// A preset upload as the device builds it (`Device::prepare_upload`); made-up values.
fn preset_draft() -> tetrad_app::device::services::PresetDraft {
    use tetrad_services::cloud::services::{ProductKind, ProductUpload};
    tetrad_app::device::services::PresetDraft {
        path: "/media/p4/Presets/My Presets/Lead.pb".into(),
        // A file that stays the same while the test runs (review r37 J4 checks it).
        file: tetrad_app::device::services::PresetFile::read(std::env::current_exe().unwrap()),
        upload: ProductUpload {
            kind: ProductKind::Preset,
            cloud_id: None,
            request_id: "0f0e0d0c-0000-4000-8000-000000000001".into(),
            name: "Lead".into(),
            author_username: String::new(),
            author_id: String::new(),
            creator: "quad".into(),
            creator_version: "4.1.0".into(),
            version: String::new(),
            date: "2026-10-12T10:00:00Z".into(),
            tags: Vec::new(),
            description: String::new(),
            instrument_type: Some("guitar".into()),
            dependencies: Vec::new(),
            factory_dependencies: vec!["f".repeat(64)],
            metadata: serde_json::Map::new(),
            payload: b"made-up payload".to_vec(),
            image: Some(b"made-up png".to_vec()),
            meta_signature: Vec::new(),
            boundary: "boundary-synthetic".into(),
        },
        captures: Vec::new(),
        from_control: false,
    }
}

/// The JSON body of a sent request.
fn body(request: &HttpRequest) -> serde_json::Value {
    serde_json::from_slice(request.body.as_deref().unwrap()).unwrap()
}

/// r31-upload, `0x1db6ac`: a free name (`0x719794`, empty `product_id`) posts a new preset
/// with the grid image (`image_type`, `image`, `image_hash` over the base64 text, `0x725020`)
/// and the factory capture hashes. An empty author takes the account's name (`0x73d818`).
/// The good reply asks the device for the sidecar `cloudId` write and nothing else. The
/// transfer state map (`0x7db73c`) holds the path as Uploading (4) while the upload runs and
/// Uploaded (5) after the good reply (`0x1d5944`); a failed upload leaves NotUploaded (3).
#[test]
fn a_preset_upload_with_a_free_name_posts_its_image_and_factory_captures() {
    use tetrad_services::cloud::services::product_digest;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let sent = b.http.requests.len();
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, r#"{"id":"cloud-9"}"#);
    let started =
        (services.upload_preset(&mut b, &mut settings, preset_draft(), ms(2_000))).unwrap();
    let lead = "/media/p4/Presets/My Presets/Lead.pb".to_owned();
    assert_eq!(
        services.upload_states(),
        [(lead.clone(), UploadState::Uploading)]
    );
    let effects = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(
        services.upload_states(),
        [(lead.clone(), UploadState::Uploaded)]
    );
    let [check, post] = &b.http.requests[sent..] else {
        panic!("{:?}", b.http.requests);
    };
    assert!(
        check
            .url
            .ends_with("/user/products/exists?name=Lead&type=preset"),
        "{}",
        check.url
    );
    assert_eq!(post.method, Method::Post);
    assert!(post.url.ends_with("/user/products"), "{}", post.url);
    let body = body(post);
    let image = body["image"].as_str().unwrap();
    assert!(!image.is_empty() && body["image_type"] == "image/png");
    assert_eq!(body["image_hash"], product_digest(image.as_bytes()));
    assert_eq!(
        body["dependencies"]["factory"],
        serde_json::json!([{ "hash": "f".repeat(64) }])
    );
    let account = &settings.settings().account.username;
    assert!(!account.is_empty() && body["author_username"] == account.as_str());
    let written = (effects.iter()).filter(|effect| matches!(effect, Effect::PresetUploaded { .. }));
    assert_eq!(
        written.collect::<Vec<_>>(),
        [&Effect::PresetUploaded {
            path: "/media/p4/Presets/My Presets/Lead.pb".into(),
            name: "Lead".into(),
            cloud_id: "cloud-9".into(),
        }]
    );
    assert!(services.upload_question().is_none());
    let rhythm = "/media/p4/Presets/My Presets/Rhythm.pb".to_owned();
    let draft = PresetDraft {
        path: rhythm.clone(),
        ..preset_draft()
    };
    // No reply: the upload fails.
    let failed = (services.upload_preset(&mut b, &mut settings, draft, ms(2_000))).unwrap();
    settle(&mut services, &mut b, &mut settings, failed, ms(2_000));
    assert_eq!(
        services.upload_states(),
        [
            (lead, UploadState::Uploaded),
            (rhythm, UploadState::NotUploaded)
        ]
    );
}

/// r31-upload: a name already on the account opens the overwrite dialog (`0x46485c`) and
/// sends nothing more. CANCEL ends the upload with nothing sent; OVERWRITE updates the
/// account's preset with the id of the name check (`0x1e138c`: `PUT user/products/<id>`).
#[test]
fn a_taken_preset_name_asks_then_puts_to_the_name_check_id() {
    use tetrad_ui::UiCommand;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let answer = |overwrite| UiCommand::UploadOverwrite { overwrite };
    let sent = b.http.requests.len();
    b.http.respond(200, r#"{"product_id":"cloud-3"}"#);
    let started =
        (services.upload_preset(&mut b, &mut settings, preset_draft(), ms(2_000))).unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(services.upload_question(), Some("Lead"));
    assert_eq!(b.http.requests.len(), sent + 1, "only the name check");
    let cancelled = services
        .on_ui(&mut b, answer(false), &mut settings, ms(2_100))
        .unwrap();
    assert!(settle(&mut services, &mut b, &mut settings, cancelled, ms(2_100)).is_empty());
    assert!(services.upload_question().is_none());
    assert_eq!(b.http.requests.len(), sent + 1, "CANCEL sends nothing");
    assert!(services.take_upload_ends().is_empty());

    b.http.respond(200, r#"{"product_id":"cloud-3"}"#);
    b.http.respond(200, "{}");
    let started =
        (services.upload_preset(&mut b, &mut settings, preset_draft(), ms(3_000))).unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(3_000));
    let overwrite = services
        .on_ui(&mut b, answer(true), &mut settings, ms(3_100))
        .unwrap();
    let effects = settle(&mut services, &mut b, &mut settings, overwrite, ms(3_100));
    let put = b.http.requests.last().unwrap();
    assert_eq!(put.method, Method::Put);
    assert!(put.url.ends_with("/user/products/cloud-3"), "{}", put.url);
    assert!(effects.contains(&Effect::PresetUploaded {
        path: "/media/p4/Presets/My Presets/Lead.pb".into(),
        name: "Lead".into(),
        cloud_id: "cloud-3".into(),
    }));
}

/// Review r31 D1: a new UPLOAD while the overwrite question waits (its dialog may be gone)
/// ends that question as CANCEL (no request, no end) and runs. D2: `upload_busy` holds while
/// the name check or the upload is out, not while the question waits.
#[test]
fn a_new_upload_replaces_a_waiting_overwrite_question() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    b.http.respond(200, r#"{"product_id":"cloud-3"}"#);
    let started =
        (services.upload_preset(&mut b, &mut settings, preset_draft(), ms(2_000))).unwrap();
    assert!(services.upload_busy(), "the name check is out");
    settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(services.upload_question(), Some("Lead"));
    assert!(!services.upload_busy(), "the question waits for the user");

    let mut other = preset_draft();
    other.path = "/media/p4/Presets/My Presets/Solo.pb".into();
    other.upload.name = "Solo".into();
    let sent = b.http.requests.len();
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, r#"{"id":"cloud-4"}"#);
    let started = (services.upload_preset(&mut b, &mut settings, other, ms(3_000))).unwrap();
    assert!(services.upload_question().is_none());
    let effects = settle(&mut services, &mut b, &mut settings, started, ms(3_000));
    let [check, post] = &b.http.requests[sent..] else {
        panic!("{:?}", &b.http.requests[sent..]);
    };
    assert!(check.url.contains("name=Solo"), "{}", check.url);
    assert_eq!(post.method, Method::Post, "no PUT for the old question");
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::PresetUploaded { name, .. } if name == "Solo"
    )));
    assert!(
        services.take_upload_ends().is_empty(),
        "the old question ends with no result"
    );
    assert!(!services.upload_busy());
}

/// r31-upload, user data first: a failed upload reply asks for no write and ends with its
/// reason for the result dialog.
#[test]
fn a_failed_preset_upload_writes_nothing() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(500, r#"{"error":3200}"#);
    let started =
        (services.upload_preset(&mut b, &mut settings, preset_draft(), ms(2_000))).unwrap();
    let effects = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert!(
        !(effects.iter()).any(|effect| matches!(effect, Effect::PresetUploaded { .. })),
        "{effects:?}"
    );
    let [end] = services.take_upload_ends().try_into().unwrap();
    assert_eq!(end.name, "Lead");
    assert!(end.result.is_err(), "{end:?}");
}

/// A user capture upload as `Device::prepare_capture_upload` builds it (made-up record,
/// payload and signature); the account's own (empty `authorId`).
fn capture_draft() -> tetrad_app::device::services::CaptureDraft {
    use tetrad_services::cloud::services::{ProductKind, ProductUpload};
    tetrad_app::device::services::CaptureDraft {
        hash: "c".repeat(64),
        origin: tetrad_app::device::services::CaptureOrigin::User,
        upload: ProductUpload {
            kind: ProductKind::NeuralCapture,
            cloud_id: None,
            request_id: "0f0e0d0c-0000-4000-8000-000000000002".into(),
            name: "Amp".into(),
            author_username: "player".into(),
            author_id: String::new(),
            creator: "quad".into(),
            creator_version: "4.1.0".into(),
            version: "1".into(),
            date: "2026-10-12T10:00:00Z".into(),
            tags: vec!["clean".into()],
            description: String::new(),
            instrument_type: None,
            dependencies: Vec::new(),
            factory_dependencies: Vec::new(),
            metadata: serde_json::Map::new(),
            payload: b"made-up capture payload".to_vec(),
            image: None,
            meta_signature: b"made-up signature".to_vec(),
            boundary: "boundary-synthetic".into(),
        },
        from_control: false,
    }
}

/// The `cloudUUID` writes the effects ask the device for.
fn capture_writes(effects: &[Effect]) -> Vec<&Effect> {
    (effects.iter())
        .filter(|effect| matches!(effect, Effect::CaptureUploaded { .. }))
        .collect()
}

/// The search reply of a capture on the account (`0x7bcac8`: `author` true).
const MY_CAPTURE: &str = r#"{"existing":[{"cloud_id":"c-1","author":true}]}"#;

/// re-r34 §1, `0x1db0e0`: the account's capture that is not on the cloud under a free name
/// [3] is posted (`0x1e1a40(.., 0, ..)`): the hash search, the `neural_capture` name check,
/// then `POST user/products` with the capture's payload and meta-signature. The good reply
/// asks the device for the `cloudUUID` write ([`Effect::CaptureUploaded`]) and nothing else.
#[test]
fn a_new_capture_upload_posts_then_asks_for_the_cloud_uuid_write() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let sent = b.http.requests.len();
    b.http.respond(200, r#"{"existing":[]}"#);
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, r#"{"id":"cloud-c"}"#);
    let draft = capture_draft();
    let started = services.upload_capture(&mut b, &mut settings, draft.clone(), ms(2_000));
    let effects = settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    let [search, check, post] = &b.http.requests[sent..] else {
        panic!("{:?}", &b.http.requests[sent..]);
    };
    let hash = &draft.hash;
    assert!(
        search
            .url
            .ends_with(&format!("/products/exists?hash={hash}"))
    );
    assert!(
        (check.url).ends_with("/user/products/exists?name=Amp&type=neural_capture"),
        "{}",
        check.url
    );
    assert_eq!(post.method, Method::Post);
    assert!(post.url.ends_with("/api/v1/user/products"), "{}", post.url);
    let sent_body = body(post);
    assert_eq!(sent_body["type"], "neural_capture");
    assert_eq!(sent_body["name"], "Amp");
    assert_eq!(sent_body["author_id"], "user-1", "the account's id");
    assert_eq!(
        sent_body["payload"],
        standard_base64(b"made-up capture payload")
    );
    assert_eq!(
        post.header_value("X-Cloud-Api-Meta-Signature"),
        Some(standard_base64(b"made-up signature").as_str())
    );
    assert_eq!(
        capture_writes(&effects),
        [&Effect::CaptureUploaded {
            hash: hash.clone(),
            name: "Amp".into(),
            cloud_id: "cloud-c".into(),
        }]
    );
    assert!(services.capture_upload_question().is_none());
}

/// re-r34 §1 steps 2 and 7: the account's capture under the found id with other tags [8]
/// asks "Neural Capture %1 already exists in your account" (`0x465178` mode 0) and sends
/// nothing more. CANCEL (`0x1d5a84`) sends nothing and ends with no result; OVERWRITE
/// (`0x1e2924`) puts to the found id. The same name and tags [7] put at once.
#[test]
fn a_capture_on_the_account_asks_before_it_puts_to_the_found_id() {
    use tetrad_ui::UiCommand;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let record = |tags: &str| {
        format!(r#"{{"id":"c-1","type":"neural_capture","name":"Amp","tags":{tags}}}"#)
    };
    let check = |b: &mut Backends, tags: &str| {
        b.http.respond(200, MY_CAPTURE);
        b.http.respond(200, r#"{"product_id":"c-1"}"#);
        b.http.respond(200, record(tags));
    };
    let answer = |overwrite| UiCommand::CaptureUploadOverwrite { overwrite };
    let put_to_c1 = Effect::CaptureUploaded {
        hash: "c".repeat(64),
        name: "Amp".into(),
        cloud_id: "c-1".into(),
    };
    let sent = b.http.requests.len();
    check(&mut b, r#"["lead"]"#);
    let started = services.upload_capture(&mut b, &mut settings, capture_draft(), ms(2_000));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    assert_eq!(services.capture_upload_question(), Some("Amp"));
    assert_eq!(b.http.requests.len(), sent + 3, "only the check");
    let cancelled = services.on_ui(&mut b, answer(false), &mut settings, ms(2_100));
    let effects = settle(
        &mut services,
        &mut b,
        &mut settings,
        cancelled.unwrap(),
        ms(2_100),
    );
    assert!(effects.is_empty(), "{effects:?}");
    assert!(services.capture_upload_question().is_none());
    assert_eq!(b.http.requests.len(), sent + 3, "CANCEL sends nothing");
    assert!(services.take_upload_ends().is_empty());

    check(&mut b, r#"["lead"]"#);
    b.http.respond(200, "{}");
    let started = services.upload_capture(&mut b, &mut settings, capture_draft(), ms(3_000));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(3_000),
    );
    let overwrite = services.on_ui(&mut b, answer(true), &mut settings, ms(3_100));
    let effects = settle(
        &mut services,
        &mut b,
        &mut settings,
        overwrite.unwrap(),
        ms(3_100),
    );
    let put = b.http.requests.last().unwrap();
    assert_eq!(put.method, Method::Put);
    assert!(put.url.ends_with("/user/products/c-1"), "{}", put.url);
    assert_eq!(capture_writes(&effects), [&put_to_c1]);

    check(&mut b, r#"["clean"]"#);
    b.http.respond(200, "{}");
    let started = services.upload_capture(&mut b, &mut settings, capture_draft(), ms(4_000));
    let effects = settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(4_000),
    );
    assert!(
        services.capture_upload_question().is_none(),
        "[7] does not ask"
    );
    assert_eq!(b.http.requests.last().unwrap().method, Method::Put);
    assert_eq!(capture_writes(&effects), [&put_to_c1]);
}

/// `0x1e1fe0`: a record of another author [0] is refused before any request, a name on
/// another product [4] after the check, each with its stock dialog (`0x464ce4`, `0x464e30`
/// mode 0). A failed upload reply asks for no write. None of them asks for a write.
#[test]
fn a_refused_or_failed_capture_upload_asks_for_no_write() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let mut run = |b: &mut Backends, draft, now| {
        let started = services
            .upload_capture(b, &mut settings, draft, now)
            .unwrap();
        let effects = settle(&mut services, b, &mut settings, started, now);
        assert!(capture_writes(&effects).is_empty(), "{effects:?}");
        let [end] = services.take_upload_ends().try_into().unwrap();
        end
    };
    let sent = b.http.requests.len();
    let mut other = capture_draft();
    other.upload.author_id = "someone-else".into();
    let end = run(&mut b, other, ms(2_000));
    assert_eq!(b.http.requests.len(), sent, "[0] sends nothing");
    assert_eq!(
        end.dialog(),
        (
            "Failed to upload Neural Capture",
            "Uploading other users' Neural Captures is not permitted.".into()
        )
    );

    b.http.respond(200, r#"{"existing":[]}"#);
    b.http.respond(200, r#"{"product_id":"other"}"#);
    let (title, text) = run(&mut b, capture_draft(), ms(3_000)).dialog();
    assert_eq!(title, "Neural Capture name already exists");
    assert!(text.starts_with("A Neural Capture named Amp already exists on your account."));

    b.http.respond(200, r#"{"existing":[]}"#);
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(500, r#"{"error":3200}"#);
    let end = run(&mut b, capture_draft(), ms(4_000));
    assert_eq!(b.http.requests.last().unwrap().method, Method::Post);
    assert!(end.result.is_err(), "{end:?}");
    assert_eq!(end.dialog().0, "Failed to upload Neural Capture");
}

/// A user capture of a preset walk: `capture_draft` with this hash, name and author.
fn walk_capture(
    tail: &str,
    name: &str,
    author_id: &str,
) -> tetrad_app::device::services::CaptureDraft {
    let mut draft = capture_draft();
    draft.hash = format!("{tail:0>64}");
    draft.upload.name = name.into();
    draft.upload.author_id = author_id.into();
    draft
}

/// `preset_draft` with these user captures, in walk order (each check from its record).
fn walk_preset(
    captures: Vec<tetrad_app::device::services::CaptureDraft>,
) -> tetrad_app::device::services::PresetDraft {
    use tetrad_app::device::services::WalkCapture;
    use tetrad_services::cloud::products::CaptureCheck;
    let captures = (captures.into_iter())
        .map(|draft| WalkCapture {
            check: CaptureCheck {
                hash: draft.hash,
                name: draft.upload.name.clone(),
                tags: draft.upload.tags.clone(),
                author_id: draft.upload.author_id.clone(),
            },
            origin: draft.origin,
            upload: Ok(Box::new(draft.upload)),
        })
        .collect();
    tetrad_app::device::services::PresetDraft {
        captures,
        ..preset_draft()
    }
}

/// Runs a preset upload through its replies: the effects and the requests it sent.
fn run_walk(
    services: &mut Services,
    b: &mut Backends,
    settings: &mut DeviceSettings,
    draft: tetrad_app::device::services::PresetDraft,
    now: Duration,
) -> (Vec<Effect>, Vec<HttpRequest>) {
    let sent = b.http.requests.len();
    let started = services.upload_preset(b, settings, draft, now).unwrap();
    let effects = settle(services, b, settings, started, now);
    (effects, b.http.requests[sent..].to_vec())
}

/// r37-services, the preset walk (`0x7414c0` → `0x7419b8` → `0x73d818`): each user capture is
/// checked in walk order with `0x6f94ec` mode 0, so another author's capture is looked up by
/// hash [1] instead of refused. A new capture [3] goes out first (POST) and its good reply asks
/// for its `cloudUUID` write with no result dialog of its own; [1] and [7] send nothing. The
/// preset then names all of them in `dependencies.product`: the found ones from the back of
/// their list, then the uploaded one with its reply id.
#[test]
fn a_preset_walk_uploads_new_captures_first_then_names_them() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let other = walk_capture("a1", "Theirs", "someone-else");
    let new = walk_capture("b2", "New", "");
    let same = walk_capture("c3", "Same", "");
    let replies = [
        r#"{"product_id":""}"#,
        r#"{"existing":[{"cloud_id":"c-other","visible":true}]}"#,
        r#"{"existing":[]}"#,
        r#"{"product_id":""}"#,
        r#"{"existing":[{"cloud_id":"c-same","author":true}]}"#,
        r#"{"product_id":"c-same"}"#,
        r#"{"id":"c-same","type":"neural_capture","name":"Same","tags":["clean"]}"#,
        r#"{"id":"cloud-new"}"#,
        r#"{"id":"cloud-preset"}"#,
    ];
    for reply in replies {
        b.http.respond(200, reply);
    }
    let draft = walk_preset(vec![other.clone(), new.clone(), same.clone()]);
    let (effects, requests) = run_walk(&mut services, &mut b, &mut settings, draft, ms(2_000));
    let routes: Vec<(Method, &str)> = (requests.iter())
        .map(|request| {
            (
                request.method,
                request.url.rsplit("/api/v1/").next().unwrap(),
            )
        })
        .collect();
    let search = |draft: &tetrad_app::device::services::CaptureDraft| {
        format!("products/exists?hash={}", draft.hash)
    };
    assert_eq!(
        routes,
        [
            (Method::Get, "user/products/exists?name=Lead&type=preset"),
            (Method::Get, search(&other).as_str()),
            (Method::Get, search(&new).as_str()),
            (
                Method::Get,
                "user/products/exists?name=New&type=neural_capture"
            ),
            (Method::Get, search(&same).as_str()),
            (
                Method::Get,
                "user/products/exists?name=Same&type=neural_capture"
            ),
            (Method::Get, "products/c-same"),
            (Method::Post, "user/products"),
            (Method::Post, "user/products"),
        ]
    );
    assert_eq!(body(&requests[7])["name"], "New");
    let dependency = |draft: &tetrad_app::device::services::CaptureDraft, id: &str| serde_json::json!({ "hash": draft.hash, "id": id });
    assert_eq!(
        body(&requests[8])["dependencies"]["product"],
        serde_json::json!([
            dependency(&same, "c-same"),
            dependency(&other, "c-other"),
            dependency(&new, "cloud-new"),
        ])
    );
    assert_eq!(
        capture_writes(&effects),
        [&Effect::CaptureUploaded {
            hash: new.hash.clone(),
            name: "New".into(),
            cloud_id: "cloud-new".into(),
        }]
    );
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::PresetUploaded { cloud_id, .. } if cloud_id == "cloud-preset"
    )));
    services.capture_written(&new.hash, "New".into(), Ok("cloud-new".into()));
    assert!(
        services.take_upload_ends().is_empty(),
        "the walk's capture write shows no dialog"
    );
    services.capture_written(&new.hash, "New".into(), Ok("cloud-new".into()));
    assert_eq!(
        services.take_upload_ends().len(),
        1,
        "a capture upload of its own shows one"
    );
}

/// r37-services, statuses 5 and 8 of the walk (`0x73d818` → `0x465178` mode 0): the capture
/// question asks and nothing more is sent. CANCEL (`0x73f150`) sends no capture and names the
/// found id; OVERWRITE (`0x742d28`) puts the capture to the found id first. A new upload while
/// the question waits ends the waiting one with no request (review r31 D1).
#[test]
fn a_preset_walk_asks_before_it_overwrites_a_capture_on_the_account() {
    use tetrad_ui::UiCommand;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let mine = walk_capture("d4", "Mine", "");
    let ask = |b: &mut Backends| {
        b.http.respond(200, r#"{"product_id":""}"#);
        b.http
            .respond(200, r#"{"existing":[{"cloud_id":"c-mine","author":true}]}"#);
        b.http.respond(200, r#"{"product_id":""}"#);
    };
    let answer = |overwrite| UiCommand::CaptureUploadOverwrite { overwrite };
    let named = serde_json::json!([{ "hash": mine.hash, "id": "c-mine" }]);
    for overwrite in [false, true] {
        ask(&mut b);
        let draft = walk_preset(vec![mine.clone()]);
        let (_, requests) = run_walk(&mut services, &mut b, &mut settings, draft, ms(2_000));
        assert_eq!(requests.len(), 3, "the checks only");
        assert_eq!(services.capture_upload_question(), Some("Mine"));
        assert!(!services.upload_busy() && services.upload_question().is_none());
        if overwrite {
            b.http.respond(200, "{}");
        }
        b.http.respond(200, r#"{"id":"cloud-preset"}"#);
        let sent = b.http.requests.len();
        let answered = services.on_ui(&mut b, answer(overwrite), &mut settings, ms(2_100));
        let effects = settle(
            &mut services,
            &mut b,
            &mut settings,
            answered.unwrap(),
            ms(2_100),
        );
        let requests = &b.http.requests[sent..];
        let preset = requests.last().unwrap();
        assert_eq!(body(preset)["dependencies"]["product"], named);
        match overwrite {
            false => {
                assert_eq!(requests.len(), 1, "CANCEL sends no capture");
                assert!(capture_writes(&effects).is_empty());
            }
            true => {
                assert_eq!(requests[0].method, Method::Put);
                assert!(requests[0].url.ends_with("/user/products/c-mine"));
                assert_eq!(capture_writes(&effects).len(), 1);
            }
        }
        assert!(services.capture_upload_question().is_none());
    }

    ask(&mut b);
    let draft = walk_preset(vec![mine.clone()]);
    run_walk(&mut services, &mut b, &mut settings, draft, ms(3_000));
    let mut other = preset_draft();
    other.path = "/media/p4/Presets/My Presets/Solo.pb".into();
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, r#"{"id":"cloud-solo"}"#);
    let (_, requests) = run_walk(&mut services, &mut b, &mut settings, other, ms(3_100));
    assert_eq!(requests.len(), 2, "no request for the old question");
    assert!(services.capture_upload_question().is_none());
}

/// r37-services, walk end `0x73e4cc`: a capture that is no longer available [0] or whose
/// name another product has [4] refuses the preset with the stock texts, before any upload.
/// A failed status check (`0x73bf08`) or capture upload (`0x1e4d10`) ends the upload: the
/// preset is not sent and nothing asks for a write.
#[test]
fn a_preset_walk_refuses_unavailable_or_taken_captures() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let gone = walk_capture("e5", "Gone", "someone-else");
    let taken = walk_capture("f6", "Taken", "");
    for reply in [
        r#"{"product_id":""}"#,
        r#"{"existing":[]}"#,
        r#"{"existing":[]}"#,
        r#"{"product_id":"other"}"#,
    ] {
        b.http.respond(200, reply);
    }
    let draft = walk_preset(vec![gone, taken.clone()]);
    let (effects, requests) = run_walk(&mut services, &mut b, &mut settings, draft, ms(2_000));
    assert_eq!(requests.len(), 4, "the checks only");
    assert!(capture_writes(&effects).is_empty());
    let [end] = services.take_upload_ends().try_into().unwrap();
    assert_eq!(
        end.dialog(),
        (
            "Failed to upload Preset",
            "This Preset could not be uploaded as one or more Neural Captures are no longer \
             available. This Preset could not be uploaded as one or more Neural Captures with \
             the same name already exist on your Cortex Cloud account. Please rename the \
             existing Capture(s) or the Capture(s) you are uploading."
                .into()
        )
    );

    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(500, r#"{"error":3200}"#);
    let draft = walk_preset(vec![taken.clone()]);
    let (_, requests) = run_walk(&mut services, &mut b, &mut settings, draft, ms(3_000));
    assert_eq!(requests.len(), 2, "a failed check ends the walk");
    let [end] = services.take_upload_ends().try_into().unwrap();
    assert!(end.result.is_err() && end.name == "Lead", "{end:?}");

    for reply in [
        r#"{"product_id":""}"#,
        r#"{"existing":[]}"#,
        r#"{"product_id":""}"#,
    ] {
        b.http.respond(200, reply);
    }
    b.http.respond(500, r#"{"error":3200}"#);
    let draft = walk_preset(vec![taken]);
    let (effects, requests) = run_walk(&mut services, &mut b, &mut settings, draft, ms(4_000));
    assert_eq!(requests.last().unwrap().method, Method::Post);
    assert_eq!(requests.len(), 4, "the preset is not sent");
    assert!(capture_writes(&effects).is_empty());
    assert!(
        !(effects.iter()).any(|effect| matches!(effect, Effect::PresetUploaded { .. })),
        "{effects:?}"
    );
    let [end] = services.take_upload_ends().try_into().unwrap();
    assert!(end.result.is_err(), "{end:?}");
}

/// Review r36 H5: an upload reply completes only the upload of its own flow, whatever the
/// product kind. Here a capture's own upload [5] waits, a preset walk's new capture goes out,
/// then OVERWRITE puts the waiting capture while the walk's reply is still due: each reply
/// asks for the `cloudUUID` write of its own capture with its own id.
#[test]
fn an_upload_reply_completes_only_its_own_upload() {
    use tetrad_ui::UiCommand;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let asked = walk_capture("a8", "Asked", "");
    b.http.respond(200, MY_CAPTURE);
    b.http.respond(200, r#"{"product_id":""}"#);
    let started = services.upload_capture(&mut b, &mut settings, asked.clone(), ms(2_000));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    assert_eq!(services.capture_upload_question(), Some("Asked"));

    let walked = walk_capture("b9", "Walked", "");
    for reply in [
        r#"{"product_id":""}"#,
        r#"{"existing":[]}"#,
        r#"{"product_id":""}"#,
    ] {
        b.http.respond(200, reply);
    }
    let draft = walk_preset(vec![walked.clone()]);
    let mut effects = services
        .upload_preset(&mut b, &mut settings, draft, ms(3_000))
        .unwrap();
    // The preset name check and the walk's check run; the walk's POST stays out.
    for _ in 0..2 {
        let at = effects.iter().position(|e| matches!(e, Effect::Service(_)));
        let Effect::Service(job) = effects.remove(at.unwrap()) else {
            unreachable!()
        };
        let notice = run_job(&mut b, job);
        effects = services
            .on_notice(&mut b, notice, &mut settings, ms(3_000))
            .unwrap();
    }
    let overwrite = UiCommand::CaptureUploadOverwrite { overwrite: true };
    let queued = services.on_ui(&mut b, overwrite, &mut settings, ms(3_100));
    effects.extend(queued.unwrap());
    for reply in [r#"{"id":"cloud-walked"}"#, "{}", r#"{"id":"cloud-preset"}"#] {
        b.http.respond(200, reply);
    }
    let effects = settle(&mut services, &mut b, &mut settings, effects, ms(3_100));
    let write = |draft: &tetrad_app::device::services::CaptureDraft, cloud_id: &str| {
        Effect::CaptureUploaded {
            hash: draft.hash.clone(),
            name: draft.upload.name.clone(),
            cloud_id: cloud_id.into(),
        }
    };
    assert_eq!(
        capture_writes(&effects),
        [&write(&walked, "cloud-walked"), &write(&asked, "c-1")]
    );
    let preset = body(b.http.requests.last().unwrap());
    assert_eq!(
        preset["dependencies"]["product"],
        serde_json::json!([{ "hash": walked.hash, "id": "cloud-walked" }])
    );
}

/// Review r36 H4: "Uploading %1, please wait." (`0x785dcc`) also shows while a capture's own
/// check or upload is out, with "Neural Captures" (`0x785dcc`: not a preset folder); not
/// while its question waits.
#[test]
fn a_capture_upload_shows_the_uploading_status() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    b.http.respond(200, MY_CAPTURE);
    b.http.respond(200, r#"{"product_id":""}"#);
    let started = services.upload_capture(&mut b, &mut settings, capture_draft(), ms(2_000));
    assert_eq!(services.uploading(), Some("Neural Captures"));
    assert!(services.upload_busy());
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    assert_eq!(services.capture_upload_question(), Some("Amp"));
    assert_eq!(services.uploading(), None, "the question waits");
    b.http.respond(200, r#"{"product_id":""}"#);
    let started = services.upload_preset(&mut b, &mut settings, preset_draft(), ms(3_000));
    assert_eq!(services.uploading(), Some("Presets"));
    started.unwrap();
}

/// Review r37 J3 (re-r38 §1): the walk uploads the account's own Downloads capture as a user
/// capture (status 3: one POST) and the preset names it by the reply id, but no `cloudUUID`
/// write is asked for (`0x7dd13c` → `0x2146a4` returns 2: the Downloads row keeps no cloud
/// id). Another author's Downloads capture is only named (status 1), never posted.
#[test]
fn a_preset_walk_uploads_an_own_downloads_capture_and_writes_nothing_local() {
    use tetrad_app::device::services::{CaptureDraft, CaptureOrigin};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let downloaded = |tail, name, author| CaptureDraft {
        origin: CaptureOrigin::Downloads,
        ..walk_capture(tail, name, author)
    };
    let theirs = downloaded("d1", "Theirs", "someone-else");
    let own = downloaded("d2", "Fetched", "");
    for reply in [
        r#"{"product_id":""}"#,
        r#"{"existing":[{"cloud_id":"c-theirs","visible":true}]}"#,
        r#"{"existing":[]}"#,
        r#"{"product_id":""}"#,
        r#"{"id":"cloud-own"}"#,
        r#"{"id":"cloud-preset"}"#,
    ] {
        b.http.respond(200, reply);
    }
    let mut draft = walk_preset(vec![theirs.clone(), own.clone()]);
    // Review r39 K9: a capture the walk only names needs no upload body.
    draft.captures[0].upload = Err("made-up: the body does not build".into());
    let (effects, requests) = run_walk(&mut services, &mut b, &mut settings, draft, ms(2_000));
    let posts: Vec<&HttpRequest> = (requests.iter())
        .filter(|request| request.method == Method::Post)
        .collect();
    let [capture, preset] = posts.as_slice() else {
        panic!("{requests:?}");
    };
    assert_eq!(body(capture)["name"], "Fetched");
    let dependency =
        |draft: &CaptureDraft, id: &str| serde_json::json!({ "hash": draft.hash, "id": id });
    assert_eq!(
        body(preset)["dependencies"]["product"],
        serde_json::json!([
            dependency(&theirs, "c-theirs"),
            dependency(&own, "cloud-own")
        ])
    );
    assert!(capture_writes(&effects).is_empty(), "{effects:?}");
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::PresetUploaded { cloud_id, .. } if cloud_id == "cloud-preset"
    )));

    // A capture the walk must upload (status 3) whose body did not build refuses the preset
    // there, with nothing sent.
    services.take_upload_ends();
    for reply in [
        r#"{"product_id":""}"#,
        r#"{"existing":[]}"#,
        r#"{"product_id":""}"#,
    ] {
        b.http.respond(200, reply);
    }
    let mut draft = walk_preset(vec![own]);
    draft.captures[0].upload = Err("made-up: the body does not build".into());
    let (_, requests) = run_walk(&mut services, &mut b, &mut settings, draft, ms(3_000));
    assert!(
        requests.iter().all(|request| request.method == Method::Get),
        "{requests:?}"
    );
    let [end] = <[_; 1]>::try_from(services.take_upload_ends()).unwrap();
    assert_eq!(
        end.result,
        Err("Neural Capture Fetched: made-up: the body does not build".into())
    );
}

/// Review r39 K3 (`0x463d2c` takes the Control flag per call; re-r38 §3): each waiting
/// question keeps the route of its own upload, and an answer reaches only the question it
/// names ([`Services::answer_upload`]) from that question's side. Control's question goes to
/// Control first.
#[test]
fn an_upload_answer_reaches_only_its_own_question_from_its_side() {
    use tetrad_app::device::services::CaptureDraft;
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    b.http.respond(200, MY_CAPTURE);
    b.http.respond(200, r#"{"product_id":"c-1"}"#);
    let record = r#"{"id":"c-1","type":"neural_capture","name":"Amp","tags":["lead"]}"#;
    b.http.respond(200, record);
    let control = CaptureDraft {
        from_control: true,
        ..capture_draft()
    };
    let started = services.upload_capture(&mut b, &mut settings, control, ms(2_000));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    b.http.respond(200, r#"{"product_id":"p-lead"}"#);
    let started = services.upload_preset(&mut b, &mut settings, preset_draft(), ms(2_000));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    let routes: Vec<_> = (services.upload_prompts())
        .map(|question| {
            (
                question.kind,
                question.name.to_owned(),
                question.from_control,
            )
        })
        .collect();
    assert_eq!(
        routes,
        [
            (ProductKind::Preset, "Lead".to_owned(), false),
            (ProductKind::NeuralCapture, "Amp".to_owned(), true)
        ]
    );
    let prompt = services.upload_prompt();
    assert_eq!(
        prompt.map(|question| question.name),
        Some("Amp"),
        "Control first"
    );
    let serial = |services: &Services, kind| {
        let mut prompts = services.upload_prompts();
        prompts
            .find(|question| question.kind == kind)
            .unwrap()
            .serial
    };
    let lead = serial(&services, ProductKind::Preset);
    let amp = serial(&services, ProductKind::NeuralCapture);
    let sent = b.http.requests.len();
    for (question, from_control) in [(amp, false), (lead, true), (lead.max(amp) + 1, true)] {
        let ignored = services.answer_upload(
            &mut b,
            &mut settings,
            question,
            from_control,
            true,
            ms(3_000),
        );
        settle(
            &mut services,
            &mut b,
            &mut settings,
            ignored.unwrap(),
            ms(3_000),
        );
    }
    assert_eq!(
        b.http.requests.len(),
        sent,
        "another side or question sends nothing"
    );
    assert_eq!(services.upload_prompts().count(), 2);
    b.http.respond(200, "{}");
    let answered = services.answer_upload(&mut b, &mut settings, amp, true, true, ms(3_100));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        answered.unwrap(),
        ms(3_100),
    );
    let put = b.http.requests.last().unwrap();
    assert_eq!(put.method, Method::Put);
    assert!(put.url.ends_with("/user/products/c-1"), "{}", put.url);
    assert_eq!(
        services.upload_question(),
        Some("Lead"),
        "the device question still waits"
    );
}

/// Review r37 J4 (`0x73d818` starts with `0x7650ec`; O texts): the walk checks the preset
/// file before each request and before the `cloudId` write. Deleted while the capture question
/// waits: CANCEL sends no preset. Replaced (staged and renamed, as the library writes) while
/// the preset request is out: the good reply asks for no `cloudId` write.
#[test]
fn a_preset_removed_or_replaced_during_its_upload_is_not_sent_or_written() {
    use tetrad_app::device::services::{PresetDraft, PresetFile};
    use tetrad_ui::UiCommand;
    let media = common::Media::new([("Lead", common::light_preset(&common::STOCK.specs(), 0))]);
    let pb = media.0.join("Presets/My Presets/Lead.pb");
    let bytes = std::fs::read(&pb).unwrap();
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);

    for reply in [r#"{"product_id":""}"#, MY_CAPTURE, r#"{"product_id":""}"#] {
        b.http.respond(200, reply);
    }
    let draft = PresetDraft {
        file: PresetFile::read(pb.clone()),
        ..walk_preset(vec![walk_capture("a1", "Same", "")])
    };
    run_walk(&mut services, &mut b, &mut settings, draft, ms(2_000));
    assert_eq!(services.capture_upload_question(), Some("Same"));
    std::fs::remove_file(&pb).unwrap();
    let sent = b.http.requests.len();
    let cancel = UiCommand::CaptureUploadOverwrite { overwrite: false };
    let effects = services.on_ui(&mut b, cancel, &mut settings, ms(3_000));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        effects.unwrap(),
        ms(3_000),
    );
    assert_eq!(b.http.requests.len(), sent, "no preset request");
    let [end] = <[_; 1]>::try_from(services.take_upload_ends()).unwrap();
    assert!(
        (end.result.as_ref()).is_err_and(|reason| reason.contains("removed or replaced")),
        "{end:?}"
    );

    std::fs::write(&pb, &bytes).unwrap();
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, r#"{"id":"cloud-9"}"#);
    let draft = PresetDraft {
        file: PresetFile::read(pb.clone()),
        ..preset_draft()
    };
    let started = (services.upload_preset(&mut b, &mut settings, draft, ms(4_000))).unwrap();
    let check = started.into_iter().find_map(|effect| match effect {
        Effect::Service(job) => Some(job),
        _ => None,
    });
    let notice = run_job(&mut b, check.unwrap());
    let post = services.on_notice(&mut b, notice, &mut settings, ms(4_000));
    let staged = pb.with_extension("staged");
    std::fs::write(&staged, &bytes).unwrap();
    std::fs::rename(&staged, &pb).unwrap();
    let effects = settle(
        &mut services,
        &mut b,
        &mut settings,
        post.unwrap(),
        ms(4_000),
    );
    assert_eq!(b.http.requests.last().unwrap().method, Method::Post);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::PresetUploaded { .. })),
        "{effects:?}"
    );
    let [end] = <[_; 1]>::try_from(services.take_upload_ends()).unwrap();
    assert!(
        (end.result.as_ref()).is_err_and(|reason| reason.contains("cloud id was not saved")),
        "{end:?}"
    );
}

/// A batch question: its text, its batch flag and the progress while it waits.
type Asked = (String, bool, Option<(usize, usize)>);

/// Plays the device for a capture batch (`Device::upload_next`, the `cloudUUID` writes): when
/// no item runs, the next one from `drafts` goes to its single upload; a write ends with
/// `capture_written`; a waiting question takes the next of `answers`. Returns the other
/// effects and, per question, its text, batch flag and the progress then.
fn drive_batch(
    services: &mut Services,
    b: &mut Backends,
    settings: &mut DeviceSettings,
    drafts: &[tetrad_app::device::services::CaptureDraft],
    answers: &mut VecDeque<bool>,
    now: Duration,
) -> (Vec<Effect>, Vec<Asked>) {
    let (mut out, mut asked, mut pending) = (Vec::new(), Vec::new(), Vec::new());
    loop {
        if let Some(item) = services.batch_next() {
            let key = item.key;
            let draft = drafts.iter().find(|draft| draft.hash == key).unwrap();
            pending.extend(
                services
                    .upload_capture(b, settings, draft.clone(), now)
                    .unwrap(),
            );
            services.batch_item_started();
            continue;
        }
        if pending.is_empty() {
            let Some(question) = services.upload_prompt() else {
                break;
            };
            asked.push((question.text(), question.batch, services.upload_progress()));
            let overwrite = answers.pop_front().expect("an answer for each question");
            let answer = tetrad_ui::UiCommand::CaptureUploadOverwrite { overwrite };
            pending = services.on_ui(b, answer, settings, now).unwrap();
            continue;
        }
        for effect in settle(services, b, settings, std::mem::take(&mut pending), now) {
            match effect {
                Effect::CaptureUploaded {
                    hash,
                    name,
                    cloud_id,
                } => services.capture_written(&hash, name, Ok(cloud_id)),
                other => out.push(other),
            }
        }
    }
    (out, asked)
}

/// The search, name check and record replies of a capture on the account under `id` with
/// other tags (status 8).
fn respond_ask(b: &mut Backends, id: &str, name: &str) {
    b.http.respond(
        200,
        format!(r#"{{"existing":[{{"cloud_id":"{id}","author":true}}]}}"#),
    );
    b.http.respond(200, format!(r#"{{"product_id":"{id}"}}"#));
    let record = format!(r#"{{"id":"{id}","type":"neural_capture","name":"{name}","tags":[]}}"#);
    b.http.respond(200, record);
}

/// re-r38 §2 (`0x785dcc` → `0x847934`): a batch runs its captures one at a time in selection
/// order. Each is on the account with other tags [8]; the first asks with the batch text
/// (`0x465178` with `0x843c90`) while progress counts it (1 of 3), and its OVERWRITE
/// (`af3c` = 1) answers the others at once: three PUTs in order, one question. A good item
/// ends with no dialog of its own (`batch`); the batch ends with success.
#[test]
fn a_batch_upload_runs_in_order_and_one_overwrite_answer_serves_all() {
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let drafts = [("a1", "Amp A"), ("b2", "Amp B"), ("c3", "Amp C")]
        .map(|(tail, name)| walk_capture(tail, name, ""));
    for (draft, id) in drafts.iter().zip(["c-a", "c-b", "c-c"]) {
        respond_ask(&mut b, id, &draft.upload.name);
        b.http.respond(200, "{}");
    }
    let keys = drafts.iter().map(|draft| draft.hash.clone()).collect();
    let kind = ProductKind::NeuralCapture;
    (services.start_batch(&mut b, &mut settings, kind, keys, false, ms(2_000))).unwrap();
    assert_eq!(services.upload_progress(), Some((0, 3)));
    let sent = b.http.requests.len();
    let mut answers = VecDeque::from([true]);
    let (_, asked) = drive_batch(
        &mut services,
        &mut b,
        &mut settings,
        &drafts,
        &mut answers,
        ms(2_000),
    );
    let text = "Some of the neural captures you selected already exist in your account. Would \
                you like to overwrite them?";
    assert_eq!(asked, [(text.to_owned(), true, Some((1, 3)))]);
    let puts: Vec<&str> = (b.http.requests[sent..].iter())
        .filter(|request| request.method == Method::Put)
        .map(|request| request.url.rsplit('/').next().unwrap())
        .collect();
    assert_eq!(puts, ["c-a", "c-b", "c-c"]);
    let ends = services.take_upload_ends();
    assert!(
        ends.len() == 3 && ends.iter().all(|end| end.batch && end.result.is_ok()),
        "{ends:?}"
    );
    assert_eq!(services.upload_progress(), None);
    let end = services.last_batch_end().unwrap();
    assert!(
        end.success && end.serial == 1 && end.kind == kind,
        "{end:?}"
    );
}

/// re-r44 §2 API: the running batch as Cortex Control BulkOperation (57) reads it: one serial
/// for the whole batch (the `BatchEnd` serial it gets), the kind (`operation_type`), handled of
/// total (1-based) and the source folder (a preset batch's setlist; captures: the user store).
#[test]
fn upload_batch_reports_the_running_batch() {
    use tetrad_app::device::services::{BatchOperation, BatchView};
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    assert_eq!(services.upload_batch(), None);
    let paths = ["Lead", "Rhythm"].map(|name| format!("/media/p4/Presets/My Presets/{name}.pb"));
    let kind = ProductKind::Preset;
    (services.start_batch(&mut b, &mut settings, kind, paths.into(), false, ms(2_000))).unwrap();
    let view = |handled| BatchView {
        serial: 1,
        operation: BatchOperation::Upload,
        kind,
        handled,
        total: 2,
        source: Some("/media/p4/Presets/My Presets".into()),
    };
    assert_eq!(services.upload_batch(), Some(view(0)));
    services.batch_next().unwrap();
    services.batch_item_started();
    assert_eq!(services.upload_batch(), Some(view(1)));
    services.cancel_upload_batch();
    assert_eq!(services.batch_next(), None);
    assert_eq!(services.upload_batch(), None);
    assert_eq!(services.last_batch_end().unwrap().serial, 1);

    let kind = ProductKind::NeuralCapture;
    let keys = vec!["a1".into(), "b2".into(), "c3".into()];
    (services.start_batch(&mut b, &mut settings, kind, keys, true, ms(3_000))).unwrap();
    let expected = BatchView {
        serial: 2,
        operation: BatchOperation::Upload,
        kind,
        handled: 0,
        total: 3,
        source: None,
    };
    assert_eq!(services.upload_batch(), Some(expected));
}

/// re-r38 §2 step 5: the batch goes on after a refused or failed item (`0x847cf8` reads no
/// result): each item that ends inside Services asks the device for the next one
/// (`Effect::UploadNext`). Another author's captures [0] show the batch not-author text once
/// (`af40`), names on other products [4] the batch name text once (`af41`); a failed check
/// ends its own item with a dialog; the last capture is posted.
#[test]
fn a_batch_goes_on_after_a_failed_item_and_shows_each_notice_once() {
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let drafts = [
        ("a1", "Theirs A", "someone-else"),
        ("b2", "Theirs B", "someone-else"),
        ("c3", "Taken C", ""),
        ("d4", "Taken D", ""),
        ("e5", "Broken", ""),
        ("f6", "New", ""),
    ]
    .map(|(tail, name, author)| walk_capture(tail, name, author));
    for _taken in 0..2 {
        b.http.respond(200, r#"{"existing":[]}"#);
        b.http.respond(200, r#"{"product_id":"c-other"}"#);
    }
    b.http.respond(500, r#"{"error":3200}"#);
    b.http.respond(200, r#"{"existing":[]}"#);
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, r#"{"id":"cloud-new"}"#);
    let keys = drafts.iter().map(|draft| draft.hash.clone()).collect();
    let kind = ProductKind::NeuralCapture;
    (services.start_batch(&mut b, &mut settings, kind, keys, false, ms(2_000))).unwrap();
    let sent = b.http.requests.len();
    let (effects, asked) = drive_batch(
        &mut services,
        &mut b,
        &mut settings,
        &drafts,
        &mut VecDeque::new(),
        ms(2_000),
    );
    assert!(asked.is_empty());
    let next = (effects.iter()).filter(|effect| **effect == Effect::UploadNext);
    assert_eq!(next.count(), 5, "items 1 to 5 end inside Services");
    let posts = (b.http.requests[sent..].iter()).filter(|request| request.method == Method::Post);
    assert_eq!(posts.count(), 1);
    let ends: Vec<(String, String)> = (services.take_upload_ends().iter())
        .map(|end| (end.name.clone(), end.dialog().1))
        .collect();
    let [not_author, name_taken, broken, new] = ends.as_slice() else {
        panic!("{ends:?}");
    };
    assert_eq!(
        not_author,
        &(
            "Theirs A".to_owned(),
            "Some of the Neural Captures you selected were not created by you and will not be \
             uploaded."
                .to_owned()
        )
    );
    assert_eq!(
        name_taken,
        &(
            "Taken C".to_owned(),
            "Some of the Neural Captures you selected have names that already exist on your \
             account and will not be uploaded."
                .to_owned()
        )
    );
    assert!(
        broken.0 == "Broken" && broken.1.contains("upload check failed"),
        "{broken:?}"
    );
    assert_eq!(new.0, "New");
    assert!(services.last_batch_end().unwrap().success);
}

/// re-r38 §2 step 8: the device Cancel (`0x780390` → `0x843f2c`, `UiCommand::CancelUpload`)
/// or Control BulkOperation (57) DELETE sets the stop flag (`+0x7c`): the item in flight
/// completes, the batch ends before the next one with `success: false`.
#[test]
fn a_cancelled_batch_stops_before_the_next_item() {
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let drafts =
        [("a1", "First"), ("b2", "Second")].map(|(tail, name)| walk_capture(tail, name, ""));
    b.http.respond(200, r#"{"existing":[]}"#);
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, r#"{"id":"cloud-first"}"#);
    let keys = drafts.iter().map(|draft| draft.hash.clone()).collect();
    let kind = ProductKind::NeuralCapture;
    (services.start_batch(&mut b, &mut settings, kind, keys, true, ms(2_000))).unwrap();
    let first = services.batch_next().unwrap().key;
    let draft = drafts
        .iter()
        .find(|draft| draft.hash == first)
        .unwrap()
        .clone();
    let started = services.upload_capture(&mut b, &mut settings, draft, ms(2_000));
    services.batch_item_started();
    let cancel = tetrad_ui::UiCommand::CancelUpload;
    assert_eq!(
        services
            .on_ui(&mut b, cancel, &mut settings, ms(2_000))
            .unwrap(),
        []
    );
    let sent = b.http.requests.len();
    let (_, asked) = drive_batch(
        &mut services,
        &mut b,
        &mut settings,
        &drafts,
        &mut VecDeque::new(),
        ms(2_000),
    );
    assert!(
        asked.is_empty() && b.http.requests.len() == sent,
        "nothing more yet"
    );
    let effects = settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    let [
        Effect::CaptureUploaded {
            hash,
            name,
            cloud_id,
        },
    ] = capture_writes(&effects)[..]
    else {
        panic!("{effects:?}");
    };
    services.capture_written(hash, name.clone(), Ok(cloud_id.clone()));
    assert_eq!(services.batch_next(), None);
    assert_eq!(b.http.requests.len(), sent + 3, "only the first item");
    let end = services.last_batch_end().unwrap();
    assert!(!end.success && end.from_control, "{end:?}");
    assert_eq!(services.upload_progress(), None);
}

/// re-r38 §2 step 5 for presets (`0x848308`, `0x46485c` with `0x843c90`): the first name on
/// the account asks with the batch texts and SKIP; SKIP (`af3c` 2) ends that item and
/// answers the next one at once. Nothing but the two name checks is sent.
#[test]
fn a_preset_batch_asks_once_and_skip_serves_the_rest() {
    use tetrad_app::device::services::PresetDraft;
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let drafts = ["Lead", "Rhythm"].map(|name| {
        let mut draft = PresetDraft {
            path: format!("/media/p4/Presets/My Presets/{name}.pb"),
            ..preset_draft()
        };
        draft.upload.name = name.into();
        draft
    });
    b.http.respond(200, r#"{"product_id":"p-lead"}"#);
    b.http.respond(200, r#"{"product_id":"p-rhythm"}"#);
    let paths = drafts.iter().map(|draft| draft.path.clone()).collect();
    let kind = ProductKind::Preset;
    (services.start_batch(&mut b, &mut settings, kind, paths, false, ms(2_000))).unwrap();
    let sent = b.http.requests.len();
    let (mut effects, mut asked) = (Vec::new(), 0);
    for draft in &drafts {
        let path = services.batch_next().unwrap().key;
        assert_eq!(path, draft.path, "selection order");
        let started = services.upload_preset(&mut b, &mut settings, draft.clone(), ms(2_000));
        services.batch_item_started();
        effects.extend(settle(
            &mut services,
            &mut b,
            &mut settings,
            started.unwrap(),
            ms(2_000),
        ));
        if let Some(question) = services.upload_prompt() {
            asked += 1;
            assert_eq!(
                (
                    question.kind,
                    question.title(),
                    question.text(),
                    question.buttons()
                ),
                (
                    kind,
                    "Overwrite Existing Presets?".to_owned(),
                    "Some of the Presets you are trying to upload already exist in your account."
                        .to_owned(),
                    ("OVERWRITE", "SKIP")
                )
            );
            let skip = tetrad_ui::UiCommand::UploadOverwrite { overwrite: false };
            effects.extend(
                services
                    .on_ui(&mut b, skip, &mut settings, ms(2_000))
                    .unwrap(),
            );
        }
    }
    assert_eq!(asked, 1, "one question for the batch");
    assert_eq!(b.http.requests.len(), sent + 2, "the two name checks");
    let next = (effects.iter()).filter(|effect| **effect == Effect::UploadNext);
    assert_eq!(next.count(), 2, "each skipped item asks for the next");
    assert_eq!(services.batch_next(), None);
    assert!(services.last_batch_end().unwrap().success);
}

/// re-r38 §2 through the device (Control 46 UPLOAD of three capture keys, `0xf3060` →
/// `0x785dcc(.., fromControl 1)`): the first two captures are another author's, so each ends
/// inside Services and `Effect::UploadNext` starts the next; the third is posted, its good
/// reply writes its `cloudUUID`, and the batch ends with success.
#[test]
fn a_control_batch_runs_each_capture_through_the_device() {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
    use tetrad_control::codecs::{CloudProductMessage, MessageAction};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let keys = ["a1", "b2", "c3"].map(|tail| format!("{tail:0>64}"));
    let media = common::Media::new([("Light", common::light_preset(&common::STOCK.specs(), 0))]);
    let cipher = DeviceCipher::caller_secret(b"invented test secret");
    let sealed = |plain: &[u8]| {
        let mut bytes = plain.to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        let root = write.boot_maintenance()?.0;
        for (key, author) in keys.iter().zip(["someone-else", "someone-else", ""]) {
            let metadata =
                serde_json::json!({"name": format!("Amp {}", &key[62..]), "authorId": author});
            write.put_product(&StockProduct {
                id: key.clone(),
                product_type: ProductType::Capture,
                read_only: false,
                raw_data: Some(sealed(&common::sound_v1_capture())),
                metadata: Some(sealed(metadata.to_string().as_bytes())),
            })?;
            write.link(root, key)?;
        }
        Ok(())
    })
    .unwrap();
    drop(db);
    let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher.clone());
    let files = sim.services().files().clone();
    for marker in [CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED] {
        files.put(marker, "");
    }
    let cloud = sim.cloud.clone();
    let mut device = common::start_online(sim);
    device.set_capture_seed(DeviceCipher::caller_secret(b"invented seed-only secret"));
    let mut routes = cloud.borrow_mut();
    routes.route(
        Method::Get,
        "/products/exists?hash=",
        200,
        r#"{"existing":[]}"#,
    );
    let free = r#"{"product_id":""}"#;
    routes.route(Method::Get, "/user/products/exists?name=", 200, free);
    routes.route(Method::Post, "/user/products", 200, r#"{"id":"cloud-c3"}"#);
    drop(routes);
    common::connect_control(&mut device);
    let upload = CloudProductMessage {
        action: Some(MessageAction::Upload.into()),
        product_keys: keys.to_vec(),
        product_type: Some(2),
        ..Default::default()
    };
    common::host_send(&mut device, upload);
    for _ in 0..100 {
        if device.bridges().services.last_batch_end().is_some() {
            break;
        }
        device.step();
    }
    let end = device.bridges().services.last_batch_end().unwrap();
    assert!(end.success && end.from_control, "{end:?}");
    assert_eq!(cloud.borrow().sent(Method::Post, "/user/products").len(), 1);
    let db = device.state().library.as_ref().unwrap().database.as_ref();
    let mut metadata = db
        .unwrap()
        .view()
        .product(&keys[2])
        .unwrap()
        .unwrap()
        .metadata
        .unwrap();
    cipher.apply(&mut metadata);
    let metadata: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    assert_eq!(metadata["cloudUUID"], "cloud-c3");
}

/// A cloud capture download of `name` that waits for the question (re-r34 §2 status 5/8).
fn capture_ask(
    name: &str,
) -> (
    tetrad_ui::CaptureQuestion,
    tetrad_app::device::services::CloudDownload,
) {
    use tetrad_app::device::services::{CloudDownload, DownloadOrigin};
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    let product = serde_json::json!({"id": format!("cloud-{name}"), "name": name,
        "type": "neural_capture"});
    let download = CloudDownload {
        product: CloudProduct::from_product(&product).unwrap(),
        payload: ProductPayload::Capture {
            id: "c".repeat(64),
            record: b"made-up record".to_vec(),
            metadata: b"{}".to_vec(),
        },
        origin: DownloadOrigin::Cloud,
    };
    let question = tetrad_ui::CaptureQuestion {
        name: name.into(),
        existing: "Kept".into(),
    };
    (question, download)
}

/// Review r35 G8 (re-r34 §2, `0x465178` mode 1) through `Services::on_ui`: a second question
/// while one waits is not stored and leaves the first (O); CANCEL (`0x1d5ac8`) stores nothing
/// and ends the question; OVERWRITE (`0x1d8f48`) sends the waiting download to the store once,
/// as `CaptureOverwrite`.
#[test]
fn the_capture_download_question_answers_once_through_on_ui() {
    use tetrad_app::device::services::DownloadOrigin;
    use tetrad_ui::UiCommand;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    let mut answer = |services: &mut Services, overwrite| {
        let command = UiCommand::CaptureOverwrite { overwrite };
        services
            .on_ui(&mut b, command, &mut settings, ms(1_000))
            .unwrap()
    };
    let (question, download) = capture_ask("Renamed");
    assert!(services.ask_capture(question.clone(), download.clone()));
    let (other, other_download) = capture_ask("Other");
    assert!(
        !services.ask_capture(other, other_download),
        "one question at a time"
    );
    assert_eq!(services.capture_question(), Some(&question));
    assert!(
        answer(&mut services, false).is_empty(),
        "CANCEL stores nothing"
    );
    assert_eq!(services.capture_question(), None);

    assert!(services.ask_capture(question, download.clone()));
    let stored = answer(&mut services, true);
    let [Effect::StoreDownload(stored)] = stored.as_slice() else {
        panic!("one store: {stored:?}");
    };
    assert_eq!(stored.origin, DownloadOrigin::CaptureOverwrite);
    assert_eq!(stored.product, download.product);
    assert_eq!(services.capture_question(), None);
    assert!(answer(&mut services, true).is_empty(), "taken once");
}

/// Review r35 G4 (I, `0x4de79c`): the factory reset's licence clear also ends the waiting
/// cloud capture question and capture upload question (also a preset walk's), so no later
/// OVERWRITE writes into the reset library or replaces an account capture.
#[test]
fn a_factory_reset_ends_the_waiting_capture_questions() {
    use tetrad_ui::UiCommand;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let (question, download) = capture_ask("Renamed");
    assert!(services.ask_capture(question, download));
    b.http.respond(200, MY_CAPTURE);
    b.http.respond(200, r#"{"product_id":""}"#);
    let started = services.upload_capture(&mut b, &mut settings, capture_draft(), ms(2_000));
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    assert_eq!(services.capture_upload_question(), Some("Amp"), "[5] asks");
    b.http.respond(200, r#"{"product_id":""}"#);
    b.http.respond(200, MY_CAPTURE);
    b.http.respond(200, r#"{"product_id":""}"#);
    let walk = walk_preset(vec![walk_capture("a7", "Walked", "")]);
    run_walk(&mut services, &mut b, &mut settings, walk, ms(2_500));

    services.clear_licences();
    assert_eq!(services.capture_question(), None);
    assert_eq!(
        services.capture_upload_question(),
        None,
        "both questions end"
    );
    let sent = b.http.requests.len();
    for command in [
        UiCommand::CaptureOverwrite { overwrite: true },
        UiCommand::CaptureUploadOverwrite { overwrite: true },
    ] {
        let effects = services.on_ui(&mut b, command, &mut settings, ms(3_000));
        assert!(effects.unwrap().is_empty());
    }
    assert_eq!(b.http.requests.len(), sent);
}

/// Review r36 H8 (review r35 G4, the restore path): a capture upload question that waits when
/// a backup restore starts ends there (`prepare_service_job`), so a later OVERWRITE sends
/// nothing into the account. The capture is the device's own user capture, uploaded through
/// Control 46 UPLOAD (type 2), and the cloud holds it under the account with other tags [8].
#[test]
fn a_restore_start_ends_the_waiting_capture_upload_question() {
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::platform::{Platform as _, ServiceBackends as _};
    use tetrad_control::codecs::{CloudProductMessage, MessageAction};
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let hash = "c".repeat(64);
    let media = common::Media::new([("Light", common::light_preset(&common::STOCK.specs(), 0))]);
    let cipher = DeviceCipher::caller_secret(b"invented test secret");
    let sealed = |plain: &[u8]| {
        let mut bytes = plain.to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Local)?;
        let root = write.boot_maintenance()?.0;
        write.put_product(&StockProduct {
            id: hash.clone(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(sealed(&common::sound_v1_capture())),
            metadata: Some(sealed(br#"{"name":"Amp","tags":["clean"]}"#)),
        })?;
        write.link(root, &hash)
    })
    .unwrap();
    drop(db);
    let log = std::rc::Rc::new(std::cell::RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    sim.media_cipher = Some(cipher.clone());
    let files = sim.services().files().clone();
    for marker in [CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED] {
        files.put(marker, "");
    }
    let cloud = sim.cloud.clone();
    let mut device = common::start_online(sim);
    device.set_capture_seed(DeviceCipher::caller_secret(b"invented seed-only secret"));
    let mut routes = cloud.borrow_mut();
    routes.route(Method::Get, "/products/exists?hash=", 200, MY_CAPTURE);
    routes.route(
        Method::Get,
        "/user/products/exists?name=",
        200,
        r#"{"product_id":"c-1"}"#,
    );
    let record = r#"{"id":"c-1","type":"neural_capture","name":"Amp","tags":["lead"]}"#;
    routes.route(Method::Get, "/products/c-1", 200, record);
    routes.route(Method::Put, "/user/products/c-1", 200, "{}");
    drop(routes);
    common::connect_control(&mut device);
    let upload = CloudProductMessage {
        action: Some(MessageAction::Upload.into()),
        product_keys: vec![hash.clone()],
        product_type: Some(2),
        ..Default::default()
    };
    common::host_send(&mut device, upload);
    (0..3).for_each(|_| _ = device.step());
    assert_eq!(
        device.bridges().services.capture_upload_question(),
        Some("Amp")
    );

    start_control_restore(&mut device);
    assert_eq!(device.bridges().services.capture_upload_question(), None);
    device.platform().worker.set(false);
    let overwrite = tetrad_ui::Control::Dialog(tetrad_ui::DialogButton::Primary);
    let tap = tetrad_ui::InputEvent::Activate(overwrite);
    let tap = tetrad_app::device::ui::UiInput::View(tap);
    device.push(tetrad_app::device::Event::UiInput(tap));
    (0..3).for_each(|_| _ = device.step());
    assert!(
        cloud
            .borrow()
            .sent(Method::Put, "/user/products/c-1")
            .is_empty()
    );
}

/// A booted device whose setlist holds `presets`, each with a stock sidecar (made-up
/// record values), and the device path of each.
fn upload_device(
    presets: Vec<(&str, tetrad_preset::Preset)>,
) -> (
    tetrad_app::device::Device<common::Sim>,
    Vec<String>,
    common::Media,
) {
    let names: Vec<String> = presets.iter().map(|(name, _)| (*name).to_owned()).collect();
    let media = common::Media::new(presets);
    for name in &names {
        let sidecar = serde_json::json!({
            "author": "", "authorId": "", "cloudId": "", "corosVersion": "4.1.0",
            "creator": "", "date": "2026-10-12T10:00:00Z", "description": "made up",
            "instrument_type": "guitar", "name": format!("{name} Record"), "tags": ["clean"],
        });
        let file = media.0.join(format!("Presets/My Presets/{name}.json"));
        std::fs::write(file, sidecar.to_string()).unwrap();
    }
    // The factory setlist the upload checks first (an empty one; health r34 #3).
    let factory = media.0.join("factory/Factory Library");
    std::fs::create_dir_all(&factory).unwrap();
    let empty = tetrad_storage::media::encode_setlist(&vec![None; 256]).unwrap();
    std::fs::write(factory.join("setlist"), empty).unwrap();
    let paths = (names.iter())
        .map(|name| format!("{}/{name}.pb", common::SETLIST))
        .collect();
    let (device, _log, media) = common::boot(media);
    (device, paths, media)
}

/// A light preset with one capture block (14000) whose parameter 5 is `key` then `name`.
fn capture_preset(key: &str, name: &str) -> tetrad_preset::Preset {
    use tetrad_preset::{Model, ParamValue};
    let mut preset = common::light_preset(&common::STOCK.specs(), 0);
    let mut values = vec![ParamValue::Float(0.0); 5];
    values.push(ParamValue::String(format!("{key}{name}")));
    preset.chains[0].models.push(Model::block(14_000, values));
    preset
}

/// r31-upload: the device builds a preset upload from the library (`0x21e000`, `0x725020`):
/// the record from the sidecar, the payload with the record name and the hash of the path
/// (`0x2e119c`, `0x2e1244`) packed as `payload.pb`, the grid PNG (`0x691040`, 800 × 384) and
/// a version 4 request id (`0x80a778`). No capture: no dependency.
#[test]
fn a_preset_upload_is_built_from_the_library() {
    use tetrad_services::cloud::products::ProductPayload;
    use tetrad_services::cloud::services::ProductKind;
    let specs = common::STOCK.specs();
    let (mut device, paths, media) = upload_device(vec![("Lead", common::light_preset(&specs, 0))]);
    let draft = device.prepare_upload(&paths[0]).unwrap();
    let upload = &draft.upload;
    assert_eq!(draft.path, paths[0]);
    assert_eq!(
        (upload.name.as_str(), upload.creator.as_str()),
        ("Lead Record", "quad")
    );
    assert_eq!(upload.tags, ["clean"]);
    assert_eq!(upload.instrument_type.as_deref(), Some("guitar"));
    assert_eq!(upload.date, "2026-10-12T10:00:00Z");
    assert!(upload.factory_dependencies.is_empty() && upload.dependencies.is_empty());
    let id = upload.request_id.as_bytes();
    assert!(
        id.len() == 36 && id[14] == b'4' && b"89ab".contains(&id[19]),
        "{}",
        upload.request_id
    );
    let png = upload.image.as_deref().unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(
        &png[16..24],
        [800u32.to_be_bytes(), 384u32.to_be_bytes()].concat()
    );
    let ProductPayload::Preset(stored) =
        ProductPayload::unpack(ProductKind::Preset, upload.payload.clone()).unwrap()
    else {
        panic!("not a preset payload");
    };
    let summary = tetrad_preset::PresetDocument::parse(&stored, tetrad_preset::Limits::default())
        .unwrap()
        .summary()
        .unwrap();
    assert_eq!(summary.name.as_deref(), Some("Lead Record"));
    assert_eq!(
        summary.hash,
        Some(tetrad_core::PresetId::from_identifier(&paths[0]).0)
    );

    // Review r31 D4: a date that is not `yyyy-MM-ddThh:mm:ssZ` is a null `QDateTime`
    // (`0x7efc38`), so `created` / `updated` are empty (`0x725020`).
    let file = media.0.join("Presets/My Presets/Lead.json");
    let mut sidecar: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    sidecar["date"] = "2026-10-12T10:00:00.250+02:00".into();
    std::fs::write(&file, sidecar.to_string()).unwrap();
    assert_eq!(device.prepare_upload(&paths[0]).unwrap().upload.date, "");
}

/// r31-upload walk (`0x73cef8`, `0x7414c0`): a factory capture is a `dependencies.factory`
/// hash. A user or Downloads capture (review r37 J3) is built for its own upload; here each
/// row holds no capture, so the preset is refused with the capture's name. A capture in no
/// store is missing (`0x73e4cc` text).
#[test]
fn the_upload_walk_takes_factory_captures_and_refuses_the_others() {
    let [factory, mine, gone, cloud] = ["f1", "a2", "c3", "d4"].map(|tail| format!("{tail:0>64}"));
    let (mut device, paths, media) = upload_device(vec![
        ("Factory", capture_preset(&factory, "Factory NC")),
        ("Mine", capture_preset(&mine, "My NC")),
        ("Gone", capture_preset(&gone, "Lost NC")),
        ("Fetched", capture_preset(&cloud, "Cloud NC")),
    ]);
    for (db, key) in [("factory/factory.db", &factory), ("zenui.db", &mine)] {
        let insert = "INSERT INTO product (id, type, readonly, rawdata, metadata) \
                      VALUES (?1, 2, 0, x'00', x'00')";
        let connection = rusqlite::Connection::open(media.0.join(db)).unwrap();
        connection.execute(insert, [key]).unwrap();
    }
    let insert = "INSERT INTO downloads_product_nc (id, hash, rawdata, metadata) \
                  VALUES ('cloud-5', ?1, x'00', x'00')";
    let connection = rusqlite::Connection::open(media.0.join("zenui.db")).unwrap();
    connection.execute(insert, [&cloud]).unwrap();
    let draft = device.prepare_upload(&paths[0]).unwrap();
    assert_eq!(draft.upload.factory_dependencies, [factory]);

    for (path, start) in [
        (&paths[1], "Neural Capture My NC: "),
        (&paths[3], "Neural Capture Cloud NC: "),
    ] {
        let refused = device.prepare_upload(path).unwrap_err();
        assert!(refused.reason.starts_with(start), "{}", refused.reason);
    }

    let missing = device.prepare_upload(&paths[2]).unwrap_err();
    assert_eq!(
        missing.reason,
        "This Preset could not be uploaded as one or more Neural Captures are missing."
    );
}

/// Health r34 #3: the upload's factory checks fail closed. A factory preset list that does not
/// read refuses every preset upload, and a `factory.db` that does not open refuses a preset
/// with a capture, both before any request.
#[test]
fn an_upload_refuses_when_the_factory_lists_do_not_read() {
    let specs = common::STOCK.specs();
    let key = format!("{:0>64}", "e5");
    let (mut device, paths, media) = upload_device(vec![
        ("Lead", common::light_preset(&specs, 0)),
        ("Captured", capture_preset(&key, "Some NC")),
    ]);
    let setlist = media.0.join("factory/Factory Library/setlist");
    let good = std::fs::read(&setlist).unwrap();
    std::fs::write(&setlist, b"not a setlist").unwrap();
    let refused = device.prepare_upload(&paths[0]).unwrap_err();
    assert!(
        refused
            .reason
            .starts_with("the factory preset list does not read"),
        "{}",
        refused.reason
    );
    std::fs::write(&setlist, good).unwrap();
    std::fs::remove_file(media.0.join("factory/factory.db")).unwrap();
    assert!(
        device.prepare_upload(&paths[0]).is_ok(),
        "no capture, no factory.db read"
    );
    let refused = device.prepare_upload(&paths[1]).unwrap_err();
    assert!(
        refused.reason.starts_with("factory.db does not read"),
        "{}",
        refused.reason
    );
}

/// Review r31 D3: UPLOAD checks the clock and the account before it builds the preset
/// (`0x1db6ac` checks the transfer state first): with no account, even a preset that is not in
/// the library is refused for the account, and no result or request follows.
#[test]
fn an_upload_without_an_account_builds_nothing() {
    use tetrad_ui::UploadItem;
    let specs = common::STOCK.specs();
    let (mut device, _paths, _media) =
        upload_device(vec![("Lead", common::light_preset(&specs, 0))]);
    let gone = format!("{}/Gone.pb", common::SETLIST);
    let refused = device.upload(vec![UploadItem::Preset(gone)], Duration::ZERO);
    assert!(
        (refused.as_ref()).is_err_and(|error| !error.to_string().contains("Gone")),
        "refused before the build: {refused:?}"
    );
    device.step();
    let snapshot = device.bridges().ui.view().snapshot();
    assert!(
        (snapshot.notice.as_ref()).is_none_or(|notice| notice.title != "Failed to upload Preset"),
        "no upload result: {:?}",
        snapshot.notice
    );
}

/// re-r44 §1 (C `0x1e8a70`, `0x848b60`, `0x704c94`): stock 4.1.0 has no IR upload. An IR-only
/// list is refused with its own text before any request, also when the cloud is ready.
#[test]
fn an_ir_upload_is_refused_with_no_request() {
    use tetrad_dsp_host::session::Clock as _;
    use tetrad_ui::UploadItem;
    let (mut device, _media) = common::boot_online(common::Media::new([]));
    let cloud = device.platform().cloud.clone();
    let sent = cloud.borrow().requests.len();
    let now = device.platform().clock.now();
    let irs = vec![
        UploadItem::Ir("CIR_1".into()),
        UploadItem::Ir("CIR_2".into()),
    ];
    let error = device.upload(irs, now).unwrap_err().to_string();
    assert!(
        error.contains("Cortex Cloud has no IR upload in CorOS 4.1.0"),
        "{error}"
    );
    (0..20).for_each(|_| _ = device.step());
    assert_eq!(cloud.borrow().requests.len(), sent, "no request");
}

/// r31-upload, user data first: a good reply writes only the sidecar `cloudId` (`0x1d5944`):
/// the `.pb` and every other sidecar key stay. The result dialog shows the upload.
#[test]
fn a_good_upload_reply_writes_only_the_sidecar_cloud_id() {
    let specs = common::STOCK.specs();
    let (mut device, paths, media) = upload_device(vec![("Lead", common::light_preset(&specs, 0))]);
    let file = |extension: &str| media.0.join(format!("Presets/My Presets/Lead.{extension}"));
    let read_json = |path| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    };
    let (pb, mut before) = (std::fs::read(file("pb")).unwrap(), read_json(file("json")));
    device
        .preset_uploaded(&paths[0], "Lead Record".into(), "cloud-9".into())
        .unwrap();
    let mut after = read_json(file("json"));
    assert_eq!(after["cloudId"], "cloud-9");
    before["cloudId"] = serde_json::Value::Null;
    after["cloudId"] = serde_json::Value::Null;
    assert_eq!(after, before);
    assert_eq!(std::fs::read(file("pb")).unwrap(), pb);
    device.step();
    let notice = device
        .bridges()
        .ui
        .view()
        .snapshot()
        .notice
        .clone()
        .unwrap();
    assert_eq!(notice.title, "Preset uploaded");
}

/// RestartDownloadViaControl (`0x4e4cc0` → `0x4e7808`): the update check and the download
/// go through Cortex Control (`CloudUpdaterUSB`, forward 61), never through the device's own
/// HTTP. Here the Control bridge is gone, so both fail at once with that reason.
#[test]
fn restart_download_via_control_uses_the_control_transport() {
    use tetrad_app::device::control::Control;
    use tetrad_app::device::services::{ControlTransport, DownloadJob};
    use tetrad_services::exec::CancelToken;
    use tetrad_services::updater::cloud::UpdateOffer;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    let sent = b.http.requests.len();
    let control = ControlTransport::new(Control::new().forward_link());

    let started = services
        .restart_download_over_control(&mut b, &mut settings, control.clone(), ms(1_000))
        .unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(1_000));
    signing_replies(&mut services);
    let checked = tick(&mut services, &mut b, &mut settings, ms(1_060));
    assert!(
        texts(&checked)
            .iter()
            .any(|text| text.contains("Cortex Control")),
        "{checked:?}"
    );
    assert_eq!(
        services.updater().state,
        tetrad_services::updater::state::UpdaterState::Failed
    );

    let job = DownloadJob {
        offer: UpdateOffer {
            version_id: "4.2.0".into(),
            url: "https://example.invalid/update.bin".into(),
            changelog_url: None,
        },
        cancel: CancelToken::default(),
        via_control: Some(control),
    };
    let ServiceNotice::Download(result) = run_job(&mut b, ServiceJob::Download(job)) else {
        panic!("a download result");
    };
    assert!(result.unwrap_err().to_string().contains("Cortex Control"));
    assert_eq!(
        b.http.requests.len(),
        sent,
        "the device's HTTP sends nothing"
    );
}

/// The synthetic key-exchange vector of [`ControlHost`]'s link transform.
const HOST_KEY: &[u8] = b"fake key-exchange vector";

/// Cortex Control as the cloud route (re-r44 §3): a connected link with the transform set,
/// whose KeepAlive says `online` or not, and the forward messages that went out through it.
struct ControlHost {
    control: tetrad_app::device::control::Control,
    state: tetrad_app::device::State,
    sent: Vec<ControlMessage>,
}

impl ControlHost {
    fn new(online: bool) -> Self {
        use tetrad_control::codecs::{ConnectionMessage, MessageAction, ModelRepoMessage};
        use tetrad_hal::platform::{Platform, Product};
        let state =
            tetrad_app::device::State::new(Product::QuadCortex, Platform::QcOriginal, settings());
        let control = tetrad_app::device::control::Control::new();
        let mut host = Self {
            control,
            state,
            sent: Vec::new(),
        };
        host.control.set_model_repo(b"<Models/>").unwrap();
        let cipher = tetrad_app::device::control::CommsCipher::derive(HOST_KEY);
        host.control.set_transform(Some(cipher));
        host.keep_alive(online);
        host.send(ModelRepoMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        });
        host.send(ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        });
        assert!(host.control.connected());
        host
    }

    fn keep_alive(&mut self, online: bool) {
        use tetrad_control::codecs::{KeepAliveMessage, MessageAction};
        self.send(KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            is_online: Some(online),
            ..Default::default()
        });
    }

    /// One host message in; the device's replies are dropped.
    fn send(&mut self, message: impl Into<ControlMessage>) {
        use tetrad_control::link::LinkLimits;
        let message = message.into();
        let envelope = tetrad_control::Envelope {
            message_type: message.message_type() as u32,
            transform: 0,
            decompress: 0,
            opaque: [0; 2],
            payload: message.encode(LinkLimits::default().payload).unwrap(),
        };
        for report in envelope.reports(tetrad_control::Limits::default()).unwrap() {
            (self.control)
                .on_report(&report, &mut self.state, Duration::ZERO)
                .unwrap();
        }
    }

    /// Runs `work` on its own thread with fresh backends, as the service worker does, and
    /// answers each forward call with the next of `answers` until `work` ends. A backup body
    /// is answered after its last chunk. Forwards carry the bearer, so each must be sealed.
    fn run<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut Backends) -> R + Send + 'static,
        answers: &mut VecDeque<Vec<u8>>,
    ) -> R {
        use tetrad_control::codecs::{
            BackupsForwardMessage, BackupsForwardResponse, CodecError, MessageAction,
            ProductForwardMessage, ProductForwardResponse,
        };
        use tetrad_control::link::LinkLimits;
        use tetrad_control::pipeline::Transform as _;
        let cipher = tetrad_app::device::control::CommsCipher::derive(HOST_KEY);
        let worker = std::thread::spawn(move || work(&mut Backends::default()));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut reassembler = tetrad_control::Reassembler::new(tetrad_control::Limits::default());
        while !worker.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker did not end"
            );
            for effect in (self.control.tick(&mut self.state, Duration::ZERO)).unwrap() {
                let Effect::ControlReport(report) = effect else {
                    continue;
                };
                let Some(mut envelope) = reassembler.push(&report[..]).unwrap() else {
                    continue;
                };
                assert_eq!(envelope.decompress, 0, "the host accepts no compression");
                let sealed = envelope.transform != 0;
                if sealed {
                    envelope.payload = cipher.decode(&envelope.payload).unwrap();
                    envelope.transform = 0;
                }
                let message = match ControlMessage::decode(&envelope, LinkLimits::default().payload)
                {
                    Err(CodecError::UnsupportedMessageType(_)) => continue,
                    decoded => decoded.unwrap().message,
                };
                let mut answer = || Some(answers.pop_front().expect("an answer for each call"));
                let update = Some(MessageAction::Update.into());
                let reply: Option<ControlMessage> = match &message {
                    ControlMessage::ProductForward(sent) => Some(
                        ProductForwardMessage {
                            action: update,
                            cloud_request_id: sent.cloud_request_id,
                            response: Some(ProductForwardResponse {
                                payload: answer(),
                                error_code: None,
                            }),
                            ..Default::default()
                        }
                        .into(),
                    ),
                    ControlMessage::BackupsForward(sent) => {
                        let last = sent.request.as_ref().and_then(|r| r.is_last_chunk);
                        (last == Some(true)).then(|| {
                            BackupsForwardMessage {
                                action: update,
                                backups_request_id: sent.backups_request_id,
                                response: Some(BackupsForwardResponse {
                                    payload: answer(),
                                    is_last_chunk: Some(true),
                                    error_code: None,
                                }),
                                ..Default::default()
                            }
                            .into()
                        })
                    }
                    _ => continue,
                };
                assert!(sealed, "a forward goes out sealed");
                self.sent.push(message);
                if let Some(reply) = reply {
                    self.send(reply);
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap()
    }
}

/// [`settle`] with each service job on a worker thread whose calls Cortex Control carries
/// ([`ControlHost::run`]): the worker's own HTTP sends nothing.
fn settle_over(
    host: &mut ControlHost,
    services: &mut Services,
    b: &mut Backends,
    settings: &mut DeviceSettings,
    effects: Vec<Effect>,
    answers: &[&str],
    now: Duration,
) -> Vec<Effect> {
    let mut answers = answers
        .iter()
        .map(|answer| answer.as_bytes().to_vec())
        .collect();
    let mut queue = VecDeque::from(effects);
    let mut out = Vec::new();
    while let Some(effect) = queue.pop_front() {
        match effect {
            Effect::Service(job) => {
                let run = move |b: &mut Backends| (run_job(b, job), b.http.requests.len());
                let (notice, direct) = host.run(run, &mut answers);
                assert_eq!(direct, 0, "the device's own HTTP sends nothing");
                queue.extend(services.on_notice(b, notice, settings, now).unwrap());
            }
            other => out.push(other),
        }
    }
    assert!(answers.is_empty(), "unused answers: {answers:?}");
    out
}

/// The value of header `name` of a forwarded request.
fn forward_header<'a>(
    headers: &'a [tetrad_control::codecs::ProductForwardHeaderPair],
    name: &str,
) -> Option<&'a str> {
    (headers.iter())
        .find(|pair| pair.name.as_deref() == Some(name))
        .and_then(|pair| pair.value.as_deref())
}

/// The ProductForward (29) requests the host got, in order.
fn product_forwards(host: &ControlHost) -> Vec<tetrad_control::codecs::ProductForwardRequest> {
    (host.sent.iter())
        .map(|message| match message {
            ControlMessage::ProductForward(sent) => sent.request.clone().unwrap(),
            other => panic!("not a ProductForward: {other:?}"),
        })
        .collect()
}

/// re-r44 §3 (C `0x80da7c`, `0x80c254`): with Cortex Control connected and its KeepAlive online,
/// a cloud call goes as ProductForward 29 with the `Hardware-Source` headers (here the backup
/// list, which Tetrad once sent as BackupsForward 30), and the device's own HTTP sends nothing.
/// KeepAlive `is_online` false: the same call goes over Wi-Fi with the `Client` headers. No
/// token is in a URL or a notice.
#[test]
fn cloud_calls_use_cortex_control_when_it_is_connected_and_online() {
    use tetrad_control::codecs::HttpOperation;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let mut host = ControlHost::new(true);
    services.set_control_route(host.control.cloud_route());
    let listed = (services.refresh_backups(&mut b, &mut settings, ms(2_000))).unwrap();
    let answers = [r#"{"backups":[]}"#];
    let effects = settle_over(
        &mut host,
        &mut services,
        &mut b,
        &mut settings,
        listed,
        &answers,
        ms(2_000),
    );
    let [request] = &product_forwards(&host)[..] else {
        panic!("one ProductForward: {:?}", host.sent);
    };
    let header = |name| forward_header(&request.headers, name);
    assert_eq!(
        (
            request.operation,
            request.url.as_deref(),
            request.timeout,
            request.is_multipart
        ),
        (
            Some(HttpOperation::Get.into()),
            Some("https://cloud-api.neuraldsp.com/backups/v1"),
            Some(30_000),
            None
        )
    );
    assert_eq!(
        header("X-Cloud-Api-Hardware-Source-Type"),
        Some("quad_cortex")
    );
    assert_eq!(header("X-Cloud-Api-Hardware-Source-Version"), Some("4.1.0"));
    assert_eq!(header("X-Cloud-Api-Client-Type"), None);
    assert_eq!(header("Content-Type"), Some("application/json"));
    assert!(header("Authorization").is_some_and(|value| value.starts_with("Bearer e30.")));
    assert!(
        !request.url.as_deref().unwrap().contains("e30."),
        "no token in the URL"
    );
    assert!(texts(&effects).iter().all(|text| !text.contains("e30.")));

    host.keep_alive(false);
    assert_eq!(host.control.cloud_route(), None);
    services.set_control_route(host.control.cloud_route());
    b.http.respond(200, r#"{"backups":[]}"#);
    let sent = b.http.requests.len();
    let listed = (services.refresh_backups(&mut b, &mut settings, ms(3_000))).unwrap();
    settle(&mut services, &mut b, &mut settings, listed, ms(3_000));
    let [wifi] = &b.http.requests[sent..] else {
        panic!("one Wi-Fi request");
    };
    assert_eq!(
        wifi.header_value("X-Cloud-Api-Client-Type"),
        Some("quad_cortex")
    );
    assert_eq!(host.sent.len(), 1, "Cortex Control carries nothing more");
}

/// re-r44 §3 and U1 (`0x80c254` with `+0x10` = 1): over Cortex Control the capture upload POST
/// is a ProductForward 29 with `is_multipart`, no Content-Type, the meta-signature and the JSON
/// body as its payload; the checks before it are plain JSON calls.
#[test]
fn a_capture_upload_over_control_is_multipart() {
    use tetrad_control::codecs::HttpOperation;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let mut host = ControlHost::new(true);
    services.set_control_route(host.control.cloud_route());
    let draft = walk_capture("a1", "Amp A", "");
    let started = (services.upload_capture(&mut b, &mut settings, draft, ms(2_000))).unwrap();
    let answers = [
        r#"{"existing":[]}"#,
        r#"{"product_id":""}"#,
        r#"{"id":"cloud-new"}"#,
    ];
    let effects = settle_over(
        &mut host,
        &mut services,
        &mut b,
        &mut settings,
        started,
        &answers,
        ms(2_000),
    );
    let requests = product_forwards(&host);
    let [checks @ .., upload] = &requests[..] else {
        panic!("no requests");
    };
    assert_eq!(checks.len(), 2);
    for check in checks {
        let content = forward_header(&check.headers, "Content-Type");
        assert_eq!(
            (check.is_multipart, content),
            (None, Some("application/json"))
        );
    }
    assert_eq!(
        (upload.operation, upload.url.as_deref(), upload.is_multipart),
        (
            Some(HttpOperation::Post.into()),
            Some("https://cloud-api.neuraldsp.com/api/v1/user/products"),
            Some(true)
        )
    );
    assert_eq!(forward_header(&upload.headers, "Content-Type"), None);
    assert!(forward_header(&upload.headers, "X-Cloud-Api-Meta-Signature").is_some());
    let body: serde_json::Value =
        serde_json::from_slice(upload.payload.as_deref().unwrap()).unwrap();
    assert_eq!(
        (&body["name"], &body["type"]),
        (&"Amp A".into(), &"neural_capture".into())
    );
    assert!(
        matches!(capture_writes(&effects)[..], [Effect::CaptureUploaded { cloud_id, .. }]
            if cloud_id == "cloud-new"),
        "{effects:?}"
    );
}

/// re-r44 §3 (C `0x1458ac`, `0x1417b4`, `0x148e54`): over Cortex Control a backup body goes as
/// BackupsForward 30. The upload is CreateOrUpdate in chunks of at most 150 000 bytes with one
/// id, `is_last_chunk` and the Hardware-Source headers on the last only, 600 000 ms; the
/// download is type 1, 600 000 ms. The device's own HTTP sends nothing.
#[test]
fn a_backup_body_over_control_goes_as_30_in_150000_byte_chunks() {
    use tetrad_app::device::services::{BackupDone, BackupJob};
    use tetrad_control::codecs::BackupsForwardRequestType as Type;
    use tetrad_services::backup::DOWNLOADED_BACKUP;
    use tetrad_services::backup::local::{LocalHeader, encode};
    use tetrad_services::cloud::services::BackupUrl;
    let mut host = ControlHost::new(true);
    let route = host.control.cloud_route();
    let archive: Vec<u8> = (0..320_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let header = LocalHeader {
        creator: "quad".into(),
        creator_version: "4.1.0".into(),
        name: "QC_test".into(),
        author: String::new(),
        author_id: String::new(),
        created: "2026-10-13T00:00:00Z".into(),
    };
    let file = encode(&archive, &header, None).into_bytes();
    let job = ServiceJob::Backup(BackupJob::Upload {
        archive: "/media/p4/upload.tar.gz".into(),
        target: BackupUrl {
            url: "https://bucket.example/put".into(),
            metadata: serde_json::Value::Null,
        },
        header: Box::new(header),
        via_control: route.clone(),
    });
    let upload = move |b: &mut Backends| {
        b.files.put("/media/p4/upload.tar.gz", archive);
        (run_job(b, job), b.http.requests.len())
    };
    let (notice, direct) = host.run(upload, &mut VecDeque::from([Vec::new()]));
    assert!(
        matches!(
            notice,
            ServiceNotice::Backup(Ok(BackupDone::Uploaded { .. }))
        ),
        "{notice:?}"
    );
    assert_eq!(direct, 0);
    let chunks: Vec<_> = (std::mem::take(&mut host.sent).into_iter())
        .map(|message| match message {
            ControlMessage::BackupsForward(sent) => {
                (sent.backups_request_id, sent.request.unwrap())
            }
            other => panic!("not a BackupsForward: {other:?}"),
        })
        .collect();
    let sizes: Vec<_> = (chunks.iter())
        .map(|(_, chunk)| chunk.payload.as_ref().map_or(0, Vec::len))
        .collect();
    assert_eq!(sizes, [150_000, 150_000, file.len() - 300_000]);
    let lasts: Vec<_> = chunks
        .iter()
        .map(|(_, chunk)| chunk.is_last_chunk)
        .collect();
    assert_eq!(lasts, [Some(false), Some(false), Some(true)]);
    assert!(chunks.iter().all(|(id, chunk)| *id == chunks[0].0
        && chunk.request_type == Some(Type::CreateOrUpdate.into())
        && chunk.timeout == Some(600_000)));
    let sent: Vec<u8> = (chunks.iter())
        .flat_map(|(_, chunk)| chunk.payload.clone().unwrap())
        .collect();
    assert_eq!(sent, file);
    // Review r47 N2 (C `0x1458ac`): the header list goes with the last chunk only.
    let names: Vec<Vec<_>> = (chunks.iter())
        .map(|(_, chunk)| (chunk.headers.iter()).map(|pair| pair.name.clone().unwrap()))
        .map(Iterator::collect)
        .collect();
    assert_eq!(
        names,
        [
            vec![],
            vec![],
            vec![
                "X-Cloud-Api-Hardware-Source-Type",
                "X-Cloud-Api-Hardware-Source-Version",
                "Content-Transfer-Encoding"
            ]
        ]
    );

    let job = ServiceJob::Backup(BackupJob::Fetch {
        url: "https://bucket.example/get".into(),
        via_control: route,
    });
    let fetch = move |b: &mut Backends| {
        let notice = run_job(b, job);
        (
            notice,
            b.files.get(DOWNLOADED_BACKUP),
            b.http.requests.len(),
        )
    };
    let answers = &mut VecDeque::from([vec![0x1f, 0x8b, 8, 0]]);
    let (notice, body, direct) = host.run(fetch, answers);
    assert!(
        matches!(notice, ServiceNotice::Backup(Ok(BackupDone::Fetched))),
        "{notice:?}"
    );
    assert_eq!((body.is_some(), direct), (true, 0));
    let [ControlMessage::BackupsForward(sent)] = &host.sent[..] else {
        panic!("one BackupsForward: {:?}", host.sent);
    };
    let request = sent.request.as_ref().unwrap();
    assert_eq!(
        (request.request_type, request.timeout, request.is_last_chunk),
        (Some(Type::Download.into()), Some(600_000), Some(true))
    );
    // Review-r45 M2: `0x1417b4` sends the `0x1458ac` header list (same identity, same order).
    assert_eq!(request.headers, chunks[2].1.headers);
}

/// Review-r21 H1, H9: RestartDownloadViaControl runs the stock reset (`0x4e7808`) before the
/// check: the flags and the package go and status returns to None, so no install can start
/// before the new check ends. During an install it is refused and queues no check.
#[test]
fn restart_download_via_control_resets_first_and_waits_for_an_install() {
    use tetrad_app::device::control::Control;
    use tetrad_app::device::services::ControlTransport;
    use tetrad_services::updater::paths::{DOWNLOADED_FLAG, UPDATE_BIN};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    let downloaded = |services: &mut Services, b: &mut Backends, settings: &mut DeviceSettings| {
        services
            .on_notice(b, ServiceNotice::Download(Ok(())), settings, ms(0))
            .unwrap();
        b.files.put(DOWNLOADED_FLAG, "4.2.0");
        b.files.put(UPDATE_BIN, "package");
    };
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let control = ControlTransport::new(Control::new().forward_link());
    downloaded(&mut services, &mut b, &mut settings);

    let restarted = services
        .restart_download_over_control(&mut b, &mut settings, control.clone(), ms(1_000))
        .unwrap();
    let statuses: Vec<_> = (restarted.iter())
        .filter_map(|effect| match effect {
            Effect::UpdaterReport(message) => message.status,
            _ => None,
        })
        .collect();
    assert_eq!(statuses, [EnumValue::Known(UpdaterStatus::None)]);
    assert_eq!(b.files.get(DOWNLOADED_FLAG), None);
    assert_eq!(b.files.get(UPDATE_BIN), None);
    assert!(
        services.install().is_err(),
        "no install before the new check"
    );
    signing_replies(&mut services);
    tick(&mut services, &mut b, &mut settings, ms(1_060));

    downloaded(&mut services, &mut b, &mut settings);
    services.install().unwrap();
    let refused = services.restart_download_over_control(&mut b, &mut settings, control, ms(2_000));
    assert!(refused.is_err(), "{refused:?}");
    assert!(b.files.get(UPDATE_BIN).is_some(), "the package stays");
    signing_replies(&mut services);
    let ticked = tick(&mut services, &mut b, &mut settings, ms(2_060));
    assert!(texts(&ticked).is_empty(), "{ticked:?}");
    assert_eq!(
        services.updater().state,
        tetrad_services::updater::state::UpdaterState::Updating
    );
}

/// Review-r21 H3: a check result that lands during an install keeps the state Updating (no
/// state report) and starts no download.
#[test]
fn a_check_result_during_an_install_keeps_the_install_state() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    services
        .on_notice(
            &mut b,
            ServiceNotice::Download(Ok(())),
            &mut settings,
            ms(0),
        )
        .unwrap();
    services
        .check_for_update(&mut b, &mut settings, None, true, ms(1_000))
        .unwrap();
    services.install().unwrap();
    b.http.respond(
        200,
        r#"{"url":"https://example.invalid/update.bin","versionId":"4.3.0"}"#,
    );

    signing_replies(&mut services);
    let checked = tick(&mut services, &mut b, &mut settings, ms(1_060));
    assert!(!b.http.requests.is_empty(), "the check ran");
    assert!(
        !checked.iter().any(|effect| matches!(
            effect,
            Effect::UpdaterReport(message) if message.state.is_some()
        )),
        "{checked:?}"
    );
    assert_eq!(
        services.updater().state,
        tetrad_services::updater::state::UpdaterState::Updating
    );
}

fn control_lock(message: &ControlMessage) -> Option<bool> {
    match message {
        ControlMessage::GeneralSettings(settings) => settings.lock_screen_and_volume_knob,
        _ => None,
    }
}

/// The Tetrad version guard: the offered version (the first line of the release notes; the
/// `versionId` is opaque) makes UPDATE ask first unless Tetrad has a complete profile for it.
#[test]
fn update_asks_first_for_a_version_without_a_tetrad_profile() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    for (at, notes, asks) in [
        (1_000, "4.1.1\n<h2>CorOS 4.1.1</h2>\n", false),
        (5_000, "4.2.0\n<h2>CorOS 4.2.0</h2>\n", true),
    ] {
        b.http.respond(
            200,
            r#"{"url":"https://example.invalid/update.bin","versionId":"G_mw","changelogUrl":"https://example.invalid/notes"}"#,
        );
        b.http.respond(200, notes);
        services
            .check_for_update(&mut b, &mut settings, None, false, ms(at))
            .unwrap();
        signing_replies(&mut services);
        tick(&mut services, &mut b, &mut settings, ms(at + 60));
        let view = services.updater_view();
        assert_eq!(view.available_version.as_deref(), Some(&notes[..5]));
        assert_eq!(view.stock_after_install, asks, "{notes}");
    }
}

/// Download retry (`0x0046bc94`): a failed try runs again while fewer than five ran and the
/// internet is up; an integrity failure (105) never does. Cancel (`0x13b87c`) sets the job's
/// stop flag, reports Idle at once and drops the late result; the offer stays.
#[test]
fn update_download_retries_then_cancel_drops_the_late_result() {
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    internet_up(&mut b);
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let linked = services.set_link(&mut b, true, ms(0)).unwrap();
    settle(&mut services, &mut b, &mut settings, linked, ms(0));
    b.http.respond(
        200,
        r#"{"url":"https://example.invalid/update.bin","versionId":"4.2.0"}"#,
    );
    services
        .check_for_update(&mut b, &mut settings, None, false, ms(1_000))
        .unwrap();
    signing_replies(&mut services);
    tick(&mut services, &mut b, &mut settings, ms(1_060));
    let download = |effects: &[Effect]| {
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Service(ServiceJob::Download(_))))
    };
    let mut result = |services: &mut Services, b: &mut Backends, result| {
        services
            .on_notice(b, ServiceNotice::Download(result), &mut settings, ms(2_000))
            .unwrap()
    };

    assert!(download(&services.download().unwrap()));
    let failed = Err(JobError::Update(UpdateError::Status(503)));
    assert!(download(&result(&mut services, &mut b, failed)));
    let corrupt = Err(JobError::Update(UpdateError::Integrity));
    assert!(!download(&result(&mut services, &mut b, corrupt)));
    assert_eq!(
        services.updater().state,
        tetrad_services::updater::state::UpdaterState::Failed
    );

    let started = services.download().unwrap();
    let Some(Effect::Service(ServiceJob::Download(job))) = started
        .into_iter()
        .find(|effect| matches!(effect, Effect::Service(ServiceJob::Download(_))))
    else {
        panic!("a download job");
    };
    assert_eq!(ServiceJob::Download(job.clone()).lane(), Lane::Bulk);
    let cancelled = services.cancel_download();
    assert!(
        job.cancel.is_cancelled(),
        "the worker's curl is killed (S-2)"
    );
    assert!(cancelled.iter().any(|effect| matches!(
        effect,
        Effect::UpdaterReport(message) if message.state == Some(EnumValue::Known(UpdaterState::Idle))
    )));
    assert!(result(&mut services, &mut b, Ok(())).is_empty());
    assert_eq!(
        services.updater().status,
        FlagStatus::NewVersionAvailableToDownload
    );
    assert!(services.cancel_download().is_empty(), "nothing to cancel");
}

/// Device Information (r16-ui MR 3, `0x40d27c`): the wlan0 MAC (`0x66c4b8`), the `uname`
/// fields (`0x18928`), the first "U-Boot " line of `/dev/mtd0` (`0x18a18`) and the board
/// firmware come through the platform seams; a missing file leaves its row empty. All values
/// are invented.
#[test]
fn device_information_reads_its_sources_and_leaves_missing_rows_empty() {
    use tetrad_app::device::info::{DeviceInfo, MAC_PATH, UBOOT_PATH};
    use tetrad_hal::hal::{Board, FirmwareKind, Query, Reading, RecordingHal};
    use tetrad_hal::zencoder::FirmwareVersion;
    let mut files = MemFiles::default();
    files.put(MAC_PATH, "02:00:00:00:00:01\n");
    for (field, value) in [
        ("ostype", "Linux"),
        ("hostname", "qc"),
        ("osrelease", "4.0.0-test"),
        ("version", "#1 PREEMPT"),
    ] {
        files.put(&format!("/proc/sys/kernel/{field}"), format!("{value}\n"));
    }
    files.put(
        UBOOT_PATH,
        &b"\x00\xffjunk\nU-Boot 2099.01 (test)\nU-Boot 2\n"[..],
    );
    let mut info = DeviceInfo::read(&mut files, None);
    assert_eq!(info.mac.as_deref(), Some("02:00:00:00:00:01"));
    assert_eq!(
        info.kernel.as_deref(),
        Some("Linux qc 4.0.0-test #1 PREEMPT armv7l ")
    );
    assert_eq!(info.uboot.as_deref(), Some("U-Boot 2099.01 (test)"));
    let mut hal = RecordingHal::default();
    let query = Query::Firmware(Board::Zenjack, FirmwareKind::Application);
    hal.set_reading(query, Reading::Firmware(FirmwareVersion::from_word(0x123)));
    info.read_boards(&mut hal);
    assert_eq!(
        info.zenjack[0]
            .map(|version| version.to_string())
            .as_deref(),
        Some("123")
    );
    assert_eq!(info.zencoder, [None, None]);
    let empty = DeviceInfo::read(&mut MemFiles::default(), None);
    assert_eq!((empty.mac, empty.kernel, empty.uboot), (None, None, None));
}

/// The Control backup flow (r16-services MR-4, r18-control): a create asks the DSP for the
/// stamp key (`0x43` → `0x18`), packs the user data into the stock backup file (`0x47a1c8`),
/// and that file
/// restores through `0x406844`. A good restore answers Control, then the device reloads in
/// place (`0x3fcf64`); a failed one reopens the library. Synthetic data only.
#[test]
fn control_backup_creates_and_restores_the_local_backup_file() {
    use tetrad_app::device::services::{ControlBackup, ControlBackupDone};
    use tetrad_services::backup::local::{LocalHeader, decode};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    // Stock puts the stamp after 873 archive bytes, so the data must pack larger than that.
    let noise: Vec<u8> = (0..4_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    b.files.put("/media/p4/zenui.db", noise);
    b.files.put("/media/p4/Presets/User/a.pb", vec![1u8, 2, 3]);
    let header = LocalHeader {
        creator: "quad".into(),
        creator_version: "4.1.0".into(),
        name: "QC_test".into(),
        author: String::new(),
        author_id: String::new(),
        created: "2026-10-11T00:00:00Z".into(),
    };
    // The stamp holds the serial, so a create needs the serial file. The key is made up.
    let create = ControlBackup::Create(header);
    assert!(
        services
            .control_backup(&mut b, create.clone(), ms(0))
            .is_err()
    );
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let asking = services
        .control_backup(&mut b, create.clone(), ms(0))
        .unwrap();
    assert!(matches!(
        asking.as_slice(),
        [Effect::Dsp(DspCommand::Request(Request::StampKey(text)))]
            if text.as_bytes() == b"386260441"
    ));
    let packing = services.on_usb_meter_output(UsbMeter([0x5a; 16]));
    assert!(matches!(packing.as_slice(), [Effect::Service(job)] if job.lane() == Lane::Bulk));
    assert!(
        services
            .on_usb_meter_output(UsbMeter([0x5a; 16]))
            .is_empty()
    );
    let created = settle(&mut services, &mut b, &mut settings, packing, ms(0));
    let [Effect::ControlBackupDone(ControlBackupDone::Created { json })] = created.as_slice()
    else {
        panic!("{created:?}");
    };
    assert!(decode(json, "4.1.0").is_ok());
    assert!(
        b.files
            .get("/media/p4/tetrad_local_backup.tar.gz")
            .is_none()
    );

    b.files
        .put("/media/p4/Presets/User/a.pb", "changed after the backup");
    let restore = ControlBackup::Restore { json: json.clone() };
    let restoring = services.control_backup(&mut b, restore, ms(0)).unwrap();
    let restored = settle(&mut services, &mut b, &mut settings, restoring, ms(0));
    assert!(!restored.contains(&Effect::LibraryReopen));
    assert!(matches!(
        restored.as_slice(),
        [
            ..,
            Effect::ControlBackupDone(ControlBackupDone::Restored { .. }),
            Effect::RestoreReload,
        ]
    ));
    assert_eq!(
        b.files.get("/media/p4/Presets/User/a.pb"),
        Some(vec![1, 2, 3])
    );

    // Another backup waits for the device reload after a good restore.
    let again = ControlBackup::Restore { json: json.clone() };
    assert!(services.control_backup(&mut b, again, ms(0)).is_err());
    let mut services = Services::new();
    let broken = ControlBackup::Restore { json: "{}".into() };
    let restoring = services.control_backup(&mut b, broken, ms(0)).unwrap();
    let failed = settle(&mut services, &mut b, &mut settings, restoring, ms(0));
    assert!(failed.contains(&Effect::LibraryReopen));
    assert!(matches!(
        failed.last(),
        Some(Effect::ControlBackupDone(ControlBackupDone::Failed(_)))
    ));
    assert_eq!(services.backup_view().busy, None);

    // Without the stamp key the create fails after 2 s (stock waits forever) and frees the
    // flow; a late key starts nothing.
    let mut services = Services::new();
    services.control_backup(&mut b, create, ms(0)).unwrap();
    assert!(
        services
            .tick(&mut b, &mut settings, ms(1_999))
            .unwrap()
            .is_empty()
    );
    let late = services.tick(&mut b, &mut settings, ms(2_000)).unwrap();
    assert!(matches!(
        late.as_slice(),
        [Effect::ControlBackupDone(ControlBackupDone::Failed(_))]
    ));
    assert_eq!(services.backup_view().busy, None);
    assert!(
        services
            .on_usb_meter_output(UsbMeter([0x5a; 16]))
            .is_empty()
    );
}

/// Review r19 F5: when the `zenui.db` stage cannot be made, the create fails and packs
/// nothing. It never packs the live file, which the device can write during the pack.
#[test]
fn a_backup_whose_database_copy_fails_packs_nothing() {
    use tetrad_app::device::services::{BackupJob, ControlBackup, ControlBackupDone, Stage};
    use tetrad_services::backup::local::LocalHeader;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    b.files.put("/media/p4/zenui.db", vec![7u8; 4_000]);
    b.files.put(SERIAL_PATH, "Q0000TEST\n");
    let create = ControlBackup::Create(LocalHeader {
        creator: "quad".into(),
        creator_version: "4.1.0".into(),
        name: "QC_test".into(),
        author: String::new(),
        author_id: String::new(),
        created: "2026-10-12T00:00:00Z".into(),
    });
    services.control_backup(&mut b, create, ms(0)).unwrap();
    let mut packing = services.on_usb_meter_output(UsbMeter([0x5a; 16]));
    let scratch = std::env::temp_dir().join(format!("tetrad-r19-f5-{}", std::process::id()));
    let [Effect::Service(ServiceJob::Backup(BackupJob::CreateLocal { stage, .. }))] =
        packing.as_mut_slice()
    else {
        panic!("{packing:?}");
    };
    // The device plans a stage of a database file that is not there.
    *stage = Some(Box::new(Stage {
        database: scratch.join("zenui.db"),
        dir: scratch.join("BackupTmpDir"),
        keys: Err("no ciphers in this test".into()),
    }));
    let done = settle(&mut services, &mut b, &mut settings, packing, ms(0));
    let failed = done.iter().find_map(|effect| match effect {
        Effect::ControlBackupDone(ControlBackupDone::Failed(reason)) => Some(reason),
        _ => None,
    });
    assert!(
        failed.is_some_and(|reason| reason.contains("zenui.db copy failed")),
        "{done:?}"
    );
    assert!(
        b.files
            .get("/media/p4/tetrad_local_backup.tar.gz")
            .is_none()
    );
    assert!(
        !scratch.join("BackupTmpDir").exists(),
        "the failed stage is removed"
    );
    std::fs::remove_dir(&scratch).unwrap();
}

/// Review r18 F9: `Debug` of a finished backup job shows the length of the backup text, never
/// the text (it carries the base64 creator stamp).
#[test]
fn backup_done_debug_hides_the_backup_text() {
    use tetrad_app::device::services::BackupDone;
    let done = BackupDone::LocalCreated {
        json: "synthetic stamp-bearing text".into(),
        notes: Vec::new(),
    };
    let shown = format!("{done:?}");
    assert!(!shown.contains("stamp-bearing"), "{shown}");
}

/// Opens a Cortex Control backup receive and sends one last chunk: the device starts the
/// restore job on the (simulated) worker, which the test then answers.
fn start_control_restore(device: &mut tetrad_app::device::Device<common::Sim>) {
    start_control_restore_of(device, "{}");
}

/// [`start_control_restore`] of the backup file text `json`.
fn start_control_restore_of(device: &mut tetrad_app::device::Device<common::Sim>, json: &str) {
    use tetrad_control::codecs::{LocalBackupMessage, MessageAction};
    let update = || Some(MessageAction::Update.into());
    device.platform().worker.set(true);
    for message in [
        LocalBackupMessage {
            action: update(),
            can_apply_backup: Some(true),
            ..Default::default()
        },
        LocalBackupMessage {
            action: update(),
            backup_json: Some(json.into()),
            is_last_chunk: Some(true),
            ..Default::default()
        },
    ] {
        common::host_send(device, message);
    }
}

/// `applied_backup` of each LocalBackup message the device sent from request `mark` on.
fn local_backup_replies(log: &common::Log, mark: usize) -> Vec<Option<bool>> {
    use tetrad_control::codecs::ControlMessage;
    // A restore also sends Scene (13); `control_sent` checks and skips it.
    (common::control_sent(log, mark).into_iter())
        .filter_map(|message| match message {
            ControlMessage::LocalBackup(reply) => Some(reply.applied_backup),
            _ => None,
        })
        .collect()
}

/// Answers the restore job, then runs one simulated second of steps; returns a requested
/// power action.
fn finish_restore(
    device: &mut tetrad_app::device::Device<common::Sim>,
    result: Result<tetrad_app::device::services::BackupDone, JobError>,
) -> Option<PowerAction> {
    use tetrad_app::device::platform::Input;
    use tetrad_dsp_host::session::Clock;
    let notice = ServiceNotice::Backup(result);
    (device.platform().inputs.borrow_mut()).push(Input::Service(notice));
    (0..20).find_map(|_| {
        device
            .platform()
            .clock
            .clone()
            .sleep(Duration::from_millis(50));
        device.step()
    })
}

/// r26-media, the download queue client (Control 45): queue pages (`0x71aa28`,
/// `inDownloadedQueue=true`), each item through `0x722464`, then `products/<id>/download`
/// (JSON `payload`, base64) and `products/<id>` (`0x7240b0`); the payload is unpacked by its
/// folder form (`+0x9c`: a preset is the first archive member) and handed to the device as
/// `Effect::StoreDownload`. A product whose payload is empty is named, not stored. B10: pages
/// ask by number and size, never by `productId`; a full page (50 rows, `0x433b3c`) is followed
/// by the next page; a repeated row is not downloaded twice.
#[test]
fn the_download_queue_client_hands_each_product_to_the_device() {
    use tetrad_app::device::services::CloudDownload;
    use tetrad_services::cloud::products::ProductPayload;
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let archive = tetrad_services::backup::pack(&[("payload.pb", b"preset bytes")]).unwrap();
    let payload = standard_base64(&archive);
    let rows = |ids: std::ops::RangeInclusive<u32>| {
        let items: Vec<String> =
            (ids.map(|i| format!(r#"{{"productId":"p-{i}","type":"preset"}}"#))).collect();
        format!(r#"{{"data":[{}]}}"#, items.join(","))
    };
    b.http.respond(200, rows(1..=50));
    b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
    b.http
        .respond(200, r#"{"id":"p-1","name":"Lead.pb","type":"preset"}"#);
    for _ in 2..=50 {
        b.http.respond(200, r#"{"payload":""}"#);
    }
    b.http.respond(200, rows(50..=51));
    b.http.respond(200, r#"{"payload":""}"#);
    let sent = b.http.requests.len();
    let started = services
        .process_downloads_queue(&mut b, &mut settings, ProductKind::Preset, ms(2_000))
        .unwrap();
    let effects = settle(&mut services, &mut b, &mut settings, started, ms(2_000));

    let urls: Vec<&str> = b.http.requests[sent..]
        .iter()
        .map(|r| r.url.as_str())
        .collect();
    assert!(
        urls[0].contains(
            "/search/v1/for/preset?inDownloadedQueue=true&searchOver=name&size=50&page=0&"
        ),
        "{}",
        urls[0]
    );
    assert!(urls[1].ends_with("products/p-1/download") && urls[2].ends_with("products/p-1"));
    assert!(urls[3].ends_with("products/p-2/download"), "{urls:?}");
    assert!(urls[52].contains("&size=50&page=1&"), "{}", urls[52]);
    assert!(urls[53].ends_with("products/p-51/download"), "{urls:?}");
    assert_eq!(
        urls.len(),
        54,
        "p-50 is downloaded once; a short page ends the walk"
    );
    let stored: Vec<&CloudDownload> = (effects.iter())
        .filter_map(|effect| match effect {
            Effect::StoreDownload(download) => Some(&**download),
            _ => None,
        })
        .collect();
    let [download] = stored.as_slice() else {
        panic!("one product to store: {effects:?}");
    };
    assert_eq!(
        (download.product.id.as_str(), download.product.name.as_str()),
        ("p-1", "Lead")
    );
    assert_eq!(
        download.payload,
        ProductPayload::Preset(b"preset bytes".to_vec())
    );
    assert!(
        texts(&effects)
            .iter()
            .any(|text| text.contains("p-2 not downloaded")),
        "{effects:?}"
    );
}

/// Directory cloud folder list (r28-ui data): the first page is page 0 of 50 in name order
/// (`ownedByMe=true` for a CLOUD folder), the exact request of the stock device log, with no
/// `productId` (B10: the server refuses an empty one); a full page (50 rows, `0x433b3c`) lets
/// a fetchMore ask page 1 (`0x433358`); a short page ends it, and a further fetchMore sends
/// nothing. Review r27 R3: a page is full by its raw
/// rows, so a refused row (unknown `type`) on a full page does not end the paging. r28-ui
/// MR 1: the Directory sends each page as `UiCommand::BrowseCloud` through `Services::on_ui`.
#[test]
fn a_cloud_folder_lists_pages_until_a_short_page() {
    use tetrad_services::cloud::services::{BrowseScope, ProductKind};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let rows = |ids: std::ops::RangeInclusive<u32>| {
        let items: Vec<String> =
            (ids.map(|i| format!(r#"{{"productId":"p-{i}","type":"preset"}}"#))).collect();
        format!(r#"{{"data":[{}]}}"#, items.join(","))
    };
    let (kind, scope) = (ProductKind::Preset, BrowseScope::OwnedByMe);
    let mut browse = |b: &mut Backends, refresh: bool| {
        let command = tetrad_ui::UiCommand::BrowseCloud {
            kind: tetrad_ui::ItemKind::Preset,
            scope: tetrad_ui::CloudScope::Cloud,
            refresh,
        };
        let started = (services.on_ui(b, command, &mut settings, ms(2_000))).unwrap();
        settle(&mut services, b, &mut settings, started, ms(2_000));
    };
    let unknown = r#"{"productId":"p-5","type":"preset"}"#;
    b.http.respond(
        200,
        rows(1..=50).replacen(unknown, r#"{"productId":"p-5","type":"song"}"#, 1),
    );
    b.http.respond(200, rows(50..=51));
    let sent = b.http.requests.len();
    browse(&mut b, true);
    browse(&mut b, false);
    browse(&mut b, false);
    let urls: Vec<&str> = (b.http.requests[sent..].iter())
        .map(|r| r.url.as_str())
        .collect();
    assert_eq!(urls.len(), 2, "{urls:?}");
    assert!(
        urls[0].ends_with(
            "/search/v1/for/preset?ownedByMe=true&searchOver=name&size=50&page=0\
             &sort=name&sortDir=ascending"
        ),
        "{}",
        urls[0]
    );
    assert!(urls[1].contains("&size=50&page=1&"), "{}", urls[1]);
    let folder = services.cloud_folder(kind, scope).unwrap();
    assert_eq!(
        folder.rows.len(),
        50,
        "the refused row is left out; the repeated row is listed once"
    );
    assert_eq!(folder.rows[49].id, "p-51");
    assert!(!folder.more && !folder.fetching && folder.error.is_none());
}

/// Review r28 A2: a Directory download takes its origin from the listing: a CLOUD row is
/// stored with no queue `DELETE` after it, a DOWNLOADS row leaves the queue. A CLOUD IR row
/// downloads too (r33: the device stores it into the IR library, `0x79992c` → `0x799134`).
#[test]
fn a_directory_download_takes_its_origin_from_the_listing() {
    use tetrad_app::device::services::DownloadOrigin;
    use tetrad_ui::{CloudScope, ItemKind, UiCommand};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let mut run = |b: &mut Backends, command: UiCommand| {
        let started = services.on_ui(b, command, &mut settings, ms(2_000))?;
        Ok::<_, tetrad_app::device::services::ServiceError>(settle(
            &mut services,
            b,
            &mut settings,
            started,
            ms(2_000),
        ))
    };
    let list = |kind, scope| UiCommand::BrowseCloud {
        kind,
        scope,
        refresh: true,
    };
    b.http
        .respond(200, r#"{"data":[{"productId":"p-1","type":"preset"}]}"#);
    b.http
        .respond(200, r#"{"data":[{"productId":"q-1","type":"preset"}]}"#);
    b.http.respond(
        200,
        r#"{"data":[{"productId":"i-1","type":"impulse_response"}]}"#,
    );
    run(&mut b, list(ItemKind::Preset, CloudScope::Cloud)).unwrap();
    run(&mut b, list(ItemKind::Preset, CloudScope::Downloads)).unwrap();
    run(&mut b, list(ItemKind::Ir, CloudScope::Cloud)).unwrap();
    let mut origin = |b: &mut Backends, id: &str| {
        let archive = tetrad_services::backup::pack(&[("payload.pb", b"\x12\x03Amp")]);
        let payload = standard_base64(&archive.unwrap());
        b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
        b.http.respond(
            200,
            format!(r#"{{"id":"{id}","name":"Amp","type":"preset"}}"#),
        );
        let effects = run(b, UiCommand::DownloadProduct { id: id.into() }).unwrap();
        effects.into_iter().find_map(|effect| match effect {
            Effect::StoreDownload(download) => Some(download.origin),
            _ => None,
        })
    };
    assert_eq!(origin(&mut b, "p-1"), Some(DownloadOrigin::Cloud));
    assert_eq!(origin(&mut b, "q-1"), Some(DownloadOrigin::Queue));
    b.http.respond(200, r#"{"payload":"UklGRg=="}"#);
    b.http.respond(
        200,
        r#"{"id":"i-1","name":"Room","type":"impulse_response"}"#,
    );
    let ir = run(&mut b, UiCommand::DownloadProduct { id: "i-1".into() }).unwrap();
    let ir = ir.into_iter().find_map(|effect| match effect {
        Effect::StoreDownload(download) => Some((download.origin, download.payload)),
        _ => None,
    });
    let raw = tetrad_services::cloud::products::ProductPayload::Raw(b"RIFF".to_vec());
    assert_eq!(ir, Some((DownloadOrigin::Cloud, raw)));
}

/// Review r28 A7, review r29 B11: stock keeps a transfer state per product id and starts no
/// second download while one is out (`0x7db480`, `0x1d9954`, `0x79992c`). A row tap during
/// a bulk DOWNLOAD of the same product (one `DownloadProduct` per row) sends one download;
/// the id is free again when the download ends.
#[test]
fn a_second_download_of_a_product_in_flight_is_ignored() {
    use tetrad_ui::UiCommand;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let reply = |b: &mut Backends| {
        let archive = tetrad_services::backup::pack(&[("payload.pb", b"\x12\x03Amp")]);
        let payload = standard_base64(&archive.unwrap());
        b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
        b.http
            .respond(200, r#"{"id":"p-1","name":"Amp","type":"preset"}"#);
    };
    let tap = || UiCommand::DownloadProduct { id: "p-1".into() };
    let stores = |effects: &[Effect]| {
        (effects.iter())
            .filter(|effect| matches!(effect, Effect::StoreDownload(_)))
            .count()
    };
    reply(&mut b);
    let sent = b.http.requests.len();
    let mut started = (services.on_ui(&mut b, tap(), &mut settings, ms(2_000))).unwrap();
    assert_eq!(services.downloads_in_flight(), ["p-1"]);
    started.extend((services.on_ui(&mut b, tap(), &mut settings, ms(2_000))).unwrap());
    let done = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(stores(&done), 1);
    assert_eq!(
        b.http.requests.len(),
        sent + 2,
        "one download and its product"
    );
    assert!(services.downloads_in_flight().is_empty());

    reply(&mut b);
    let started = (services.on_ui(&mut b, tap(), &mut settings, ms(3_000))).unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(3_000));
    assert_eq!(stores(&done), 1, "a download after the end goes out again");
}

/// re-r31 §5: a preset run (`0x1dd61c` per item) sends one download at a time, in order, each
/// for its slot; the next goes out only when the device stored the last one. The first failed
/// download, or a failed store, stops the rest: no request for a later item.
#[test]
fn preset_downloads_fill_the_paste_slots_in_order_and_stop_at_the_first_error() {
    use tetrad_app::device::services::{BatchOperation, DownloadOrigin};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let reply = |b: &mut Backends, id: &str| {
        let archive = tetrad_services::backup::pack(&[("payload.pb", b"\x12\x03Amp")]);
        let payload = standard_base64(&archive.unwrap());
        b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
        b.http.respond(
            200,
            format!(r#"{{"id":"{id}","name":"Amp","type":"preset"}}"#),
        );
    };
    let slot = |effects: &[Effect]| {
        effects.iter().find_map(|effect| match effect {
            Effect::StoreDownload(download) => Some(download.origin.clone()),
            _ => None,
        })
    };
    let into = |slot| {
        Some(DownloadOrigin::Slot {
            setlist: "Live".into(),
            slot,
            replace: None,
        })
    };
    let items = |ids: &[&str]| -> Vec<(String, usize)> {
        ids.iter().map(|id| (*id).to_owned()).zip(2..).collect()
    };

    reply(&mut b, "p-1");
    let sent = b.http.requests.len();
    let run = items(&["p-1", "p-2", "p-3", "p-4"]);
    let started =
        (services.download_presets(&mut b, &mut settings, "Live".into(), run, ms(2_000))).unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(slot(&done), into(2));
    assert_eq!(b.http.requests.len(), sent + 2, "one item at a time");
    // A9: more than one item is a run (`0x786ac4`) for the BulkOperation (57) pushes.
    let run = services.bulk_run().unwrap();
    let live = BatchOperation::Download {
        setlist: Some("Live".into()),
    };
    assert_eq!((run.operation, run.handled, run.total), (live, 1, 4));
    let second = vec![("q-1".into(), 9)];
    let busy = services.download_presets(&mut b, &mut settings, "Live".into(), second, ms(2_000));
    assert!(busy.is_err(), "one run at a time");

    reply(&mut b, "p-2");
    let next = services.preset_stored(&mut settings, true, ms(2_000));
    let done = settle(&mut services, &mut b, &mut settings, next, ms(2_000));
    assert_eq!(slot(&done), into(3));

    b.http.respond(500, r#"{"message":"made up"}"#);
    let sent = b.http.requests.len();
    let next = services.preset_stored(&mut settings, true, ms(2_000));
    let done = settle(&mut services, &mut b, &mut settings, next, ms(2_000));
    assert_eq!(slot(&done), None);
    assert_eq!(b.http.requests.len(), sent + 1, "no request for p-4");
    assert!(
        texts(&done)
            .iter()
            .any(|text| text.contains("stopped after 2 of 4")),
        "{:?}",
        texts(&done)
    );
    assert_eq!(services.bulk_run(), None);
    assert!(!services.last_batch_end().unwrap().success);

    reply(&mut b, "q-1");
    let run = items(&["q-1", "q-2", "q-3"]);
    let started =
        (services.download_presets(&mut b, &mut settings, "Live".into(), run, ms(3_000))).unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(3_000));
    assert_eq!(slot(&done), into(2));
    let sent = b.http.requests.len();
    let stopped = services.preset_stored(&mut settings, false, ms(3_000));
    assert!(
        texts(&stopped)
            .iter()
            .any(|text| text.contains("stopped after 0 of 3"))
    );
    assert_eq!(b.http.requests.len(), sent, "a failed store stops the run");
    assert!(services.downloads_in_flight().is_empty());
}

/// `0x742990` → `0x740b84` → `0x74012c`: a slot preset's Neural Capture dependencies download
/// first, one at a time from the back of `dependencies.product` (a missing, hidden or id-less
/// entry is skipped with a notice; an id once), each handed to the device store before the
/// next; the preset goes to its store after the last. A failed dependency download stops the
/// run and the preset is never stored (`0x73bf8c`).
#[test]
fn a_slot_preset_downloads_its_capture_dependencies_first() {
    use tetrad_app::device::services::DownloadOrigin;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let payload = |b: &mut Backends, files: &[(&str, &[u8])]| {
        let archive = tetrad_services::backup::pack(files).unwrap();
        let payload = standard_base64(&archive);
        b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
    };
    let preset = |b: &mut Backends, id: &str| {
        payload(b, &[("payload.pb", b"\x12\x03Amp")]);
        let entry = |id: &str, flags: &str| {
            format!(r#"{{"hash":"h-{id}","id":"{id}","name":"NC {id}",{flags}}}"#)
        };
        let product = [
            entry("c-1", r#""visible":true"#),
            entry("c-2", r#""visible":true,"missing":true"#),
            entry("c-3", r#""visible":false"#),
            entry("", r#""visible":true"#),
            entry("c-4", r#""visible":true"#),
            entry("c-1", r#""visible":true"#),
        ];
        let reply = format!(
            r#"{{"id":"{id}","name":"Amp","type":"preset","dependencies":{{"product":[{}]}}}}"#,
            product.join(",")
        );
        b.http.respond(200, reply);
    };
    let capture = |b: &mut Backends, id: &str| {
        payload(b, &[(&format!("h-{id}.cns"), b"rec"), ("meta.json", b"{}")]);
        let reply = format!(r#"{{"id":"{id}","name":"NC","type":"neural_capture"}}"#);
        b.http.respond(200, reply);
    };
    let stored = |effects: &[Effect]| -> Vec<(String, DownloadOrigin)> {
        (effects.iter())
            .filter_map(|effect| match effect {
                Effect::StoreDownload(download) => {
                    Some((download.product.id.clone(), download.origin.clone()))
                }
                _ => None,
            })
            .collect()
    };
    // Review r33 F1: the origin carries the key the preset names.
    let dependency = |id: &str| {
        let (preset, hash) = ("p-1".to_owned(), format!("h-{id}"));
        (id.to_owned(), DownloadOrigin::Dependency { preset, hash })
    };
    let run = |id: &str, slot| vec![(id.to_owned(), slot)];

    preset(&mut b, "p-1");
    capture(&mut b, "c-1");
    let started = (services.download_presets(
        &mut b,
        &mut settings,
        "Live".into(),
        run("p-1", 2),
        ms(2_000),
    ))
    .unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(
        stored(&done),
        [dependency("c-1")],
        "the back first; the preset waits"
    );
    let asked = &b.http.requests[b.http.requests.len() - 2];
    assert!(asked.url.ends_with("/products/c-1/download"), "{asked:?}");
    for skipped in [
        "NC c-2 (h-c-2) is missing",
        "NC c-3 (h-c-3) is not available",
    ] {
        assert!(
            texts(&done).iter().any(|text| text.contains(skipped)),
            "{skipped}: {:?}",
            texts(&done)
        );
    }

    capture(&mut b, "c-4");
    let next = services.next_dependency(&mut settings, ms(2_000));
    let done = settle(&mut services, &mut b, &mut settings, next, ms(2_000));
    assert_eq!(stored(&done), [dependency("c-4")]);
    let sent = b.http.requests.len();
    let last = services.next_dependency(&mut settings, ms(2_000));
    let slot = DownloadOrigin::Slot {
        setlist: "Live".into(),
        slot: 2,
        replace: None,
    };
    assert_eq!(stored(&last), [("p-1".to_owned(), slot)]);
    assert_eq!(b.http.requests.len(), sent, "c-1 downloads once");
    let end = services.preset_stored(&mut settings, true, ms(2_000));
    assert!(
        texts(&end)
            .iter()
            .any(|text| text.contains("1 of 1 stored"))
    );

    preset(&mut b, "p-2");
    b.http.respond(500, r#"{"message":"made up"}"#);
    let started = (services.download_presets(
        &mut b,
        &mut settings,
        "Live".into(),
        run("p-2", 3),
        ms(3_000),
    ))
    .unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(3_000));
    assert!(stored(&done).is_empty(), "{:?}", stored(&done));
    assert!(
        texts(&done)
            .iter()
            .any(|text| text.contains("stopped after 0 of 1")),
        "{:?}",
        texts(&done)
    );
    assert!(
        services
            .next_dependency(&mut settings, ms(3_000))
            .is_empty()
    );
    assert!(services.downloads_in_flight().is_empty());
}

/// re-r34 §4 (`0x7406c0` → `0x74012c` with the captures Downloads folder): a queue preset
/// (a DOWNLOADS row) fetches its captures first, from the back, each into Downloads
/// (`QueueDependency`); the preset goes to the store last as a queue product. A failed capture
/// download stores no preset (`0x73bf8c`).
#[test]
fn a_queue_preset_downloads_its_captures_into_downloads_first() {
    use tetrad_app::device::services::DownloadOrigin;
    use tetrad_services::cloud::services::{BrowseScope, ProductKind};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let payload = |b: &mut Backends, files: &[(&str, &[u8])]| {
        let archive = tetrad_services::backup::pack(files).unwrap();
        b.http.respond(
            200,
            format!(r#"{{"payload":"{}"}}"#, standard_base64(&archive)),
        );
    };
    let preset = |b: &mut Backends, id: &str| {
        payload(b, &[("payload.pb", b"\x12\x03Amp")]);
        b.http.respond(
            200,
            format!(
                r#"{{"id":"{id}","name":"Amp","type":"preset","dependencies":{{"product":[{{"hash":"h-c-1","id":"c-1","visible":true}},{{"hash":"h-c-2","id":"c-2","visible":true}}]}}}}"#
            ),
        );
    };
    let capture = |b: &mut Backends, id: &str| {
        payload(b, &[(&format!("h-{id}.cns"), b"rec"), ("meta.json", b"{}")]);
        b.http.respond(
            200,
            format!(r#"{{"id":"{id}","name":"NC","type":"neural_capture"}}"#),
        );
    };
    let stored = |effects: &[Effect]| -> Vec<(String, DownloadOrigin)> {
        (effects.iter())
            .filter_map(|effect| match effect {
                Effect::StoreDownload(download) => {
                    Some((download.product.id.clone(), download.origin.clone()))
                }
                _ => None,
            })
            .collect()
    };
    let dependency = |id: &str| {
        let (preset, hash) = ("p-1".to_owned(), format!("h-{id}"));
        (
            id.to_owned(),
            DownloadOrigin::QueueDependency { preset, hash },
        )
    };
    // The DOWNLOADS listing makes the row a queue product.
    b.http
        .respond(200, r#"{"data":[{"productId":"p-1","type":"preset"}]}"#);
    let (kind, scope) = (
        tetrad_ui::ItemKind::Preset,
        tetrad_ui::CloudScope::Downloads,
    );
    let browse = tetrad_ui::UiCommand::BrowseCloud {
        kind,
        scope,
        refresh: true,
    };
    let listed = services
        .on_ui(&mut b, browse, &mut settings, ms(2_000))
        .unwrap();
    settle(&mut services, &mut b, &mut settings, listed, ms(2_000));
    assert!(
        services
            .cloud_folder(ProductKind::Preset, BrowseScope::DownloadQueue)
            .is_some()
    );

    preset(&mut b, "p-1");
    capture(&mut b, "c-2");
    let started = services
        .download_product(&mut b, &mut settings, "p-1".into(), ms(2_000))
        .unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(
        stored(&done),
        [dependency("c-2")],
        "the back first; the preset waits"
    );
    capture(&mut b, "c-1");
    let next = services.next_queue_dependency(&mut settings, ms(2_000));
    let done = settle(&mut services, &mut b, &mut settings, next, ms(2_000));
    assert_eq!(stored(&done), [dependency("c-1")]);
    let last = services.next_queue_dependency(&mut settings, ms(2_000));
    assert_eq!(stored(&last), [("p-1".to_owned(), DownloadOrigin::Queue)]);

    preset(&mut b, "p-1");
    b.http.respond(500, r#"{"message":"made up"}"#);
    let started = services
        .download_product(&mut b, &mut settings, "p-1".into(), ms(3_000))
        .unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(3_000));
    assert!(stored(&done).is_empty(), "{:?}", stored(&done));
    assert!(
        texts(&done)
            .iter()
            .any(|text| text == "cloud preset p-1 not stored: a capture download failed"),
        "{:?}",
        texts(&done)
    );
    assert!(
        services
            .next_queue_dependency(&mut settings, ms(3_000))
            .is_empty()
    );
}

/// re-r31 §6, Cortex Control 46 DELETE (`0x1d916c` → cloud folder `vt+0x4c` = `0x7180bc`):
/// exactly one `DELETE user/products/<id>` for exactly the given id; the listed CLOUD row goes
/// and the other stays. An empty id sends nothing.
#[test]
fn a_control_delete_sends_one_delete_for_exactly_its_id() {
    use tetrad_services::cloud::services::{BrowseScope, ProductKind};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let (kind, scope) = (ProductKind::Preset, BrowseScope::OwnedByMe);
    b.http.respond(
        200,
        r#"{"data":[{"productId":"p-1","type":"preset"},{"productId":"p-2","type":"preset"}]}"#,
    );
    let listed =
        (services.browse_cloud(&mut b, &mut settings, kind, scope, true, ms(2_000))).unwrap();
    settle(&mut services, &mut b, &mut settings, listed, ms(2_000));
    let sent = b.http.requests.len();
    b.http.respond(200, "{}");
    let one = vec!["p-1".to_owned()];
    let started = (services.control_delete(&mut b, &mut settings, kind, one, ms(2_000))).unwrap();
    let done = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    let requests = &b.http.requests[sent..];
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].method, Method::Delete);
    assert!(
        requests[0].url.ends_with("/api/v1/user/products/p-1"),
        "{}",
        requests[0].url
    );
    let rows: Vec<_> = (services.cloud_folder(kind, scope).unwrap().rows.iter())
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(rows, ["p-2"]);
    assert!(texts(&done).contains(&"cloud product p-1 deleted from the account".to_owned()));
    for ids in [vec![], vec![String::new()]] {
        let empty = services.control_delete(&mut b, &mut settings, kind, ids, ms(2_000));
        assert!(empty.is_err());
    }
    assert_eq!(b.http.requests.len(), sent + 1);
}

/// Runs the queued service jobs of `effects` one level deep: each job's reply, and the
/// effects it gives (the next job of a run among them).
fn one_job(
    services: &mut Services,
    b: &mut Backends,
    settings: &mut DeviceSettings,
    effects: Vec<Effect>,
) -> Vec<Effect> {
    let mut out = Vec::new();
    for effect in effects {
        if let Effect::Service(job) = effect {
            let notice = run_job(b, job);
            out.extend(services.on_notice(b, notice, settings, ms(2_000)).unwrap());
        }
    }
    out
}

/// A9 (`0x785b74` → `0x84c614` → `0x84be40`, C): a Control DELETE of more ids is one run, one
/// `DELETE user/products/<id>` at a time in order; the reply (here a failure) starts the next.
/// The run view counts the item now starting; the stop flag ends the run after the item in
/// flight (`success: false`); a second run waits.
#[test]
fn a_control_delete_of_more_ids_runs_one_request_at_a_time() {
    use tetrad_app::device::services::{BatchOperation, BatchView};
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let sent = b.http.requests.len();
    let kind = ProductKind::Preset;
    let ids = ["p-1", "p-2", "p-3"].map(String::from).to_vec();
    let started = services.control_delete(&mut b, &mut settings, kind, ids.clone(), ms(2_000));
    let view = |handled| BatchView {
        serial: 1,
        operation: BatchOperation::Delete,
        kind,
        handled,
        total: 3,
        source: None,
    };
    assert_eq!(services.bulk_run(), Some(view(1)));
    let second = services.control_delete(&mut b, &mut settings, kind, ids, ms(2_000));
    assert!(second.is_err(), "one run at a time: {second:?}");
    b.http.respond(500, "{}");
    let next = one_job(&mut services, &mut b, &mut settings, started.unwrap());
    assert_eq!(services.bulk_run(), Some(view(2)));
    assert!(services.cancel_bulk_run());
    b.http.respond(200, "{}");
    let after = one_job(&mut services, &mut b, &mut settings, next);
    assert!(
        after
            .iter()
            .all(|effect| !matches!(effect, Effect::Service(_)))
    );
    assert_eq!(services.bulk_run(), None);
    let end = services.last_batch_end().unwrap();
    assert_eq!(
        (end.serial, end.success, end.from_control),
        (1, false, true)
    );
    let urls: Vec<_> = (b.http.requests[sent..].iter())
        .map(|request| (request.method, request.url.rsplit('/').next().unwrap()))
        .collect();
    assert_eq!(urls, [(Method::Delete, "p-1"), (Method::Delete, "p-2")]);
}

/// A9 (`0x786264` → `0x847620` → `0x845370`, C): a Control DOWNLOAD of more capture keys is
/// one run. Tetrad queues the downloads at once; the worker runs them in order, so the run
/// view counts the first one not ended as the item now starting. The stop flag drops the
/// downloads still queued; the one in flight ends the run (`success: false`).
#[test]
fn a_control_download_of_more_keys_is_one_run_that_stops_its_queue() {
    use tetrad_app::device::services::{BatchOperation, BatchView};
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let sent = b.http.requests.len();
    let kind = ProductKind::NeuralCapture;
    let ids = ["c-1", "c-2", "c-3"].map(String::from).to_vec();
    let started = services.control_downloads(&mut b, &mut settings, kind, ids, ms(2_000));
    let view = |handled| BatchView {
        serial: 1,
        operation: BatchOperation::Download { setlist: None },
        kind,
        handled,
        total: 3,
        source: None,
    };
    assert_eq!(services.bulk_run(), Some(view(1)));
    // No reply: the first download fails and ends; the second goes out.
    let next = one_job(&mut services, &mut b, &mut settings, started.unwrap());
    assert_eq!(services.bulk_run(), Some(view(2)));
    assert!(services.cancel_bulk_run());
    assert_eq!(services.downloads_in_flight(), ["c-2"]);
    assert_eq!(services.bulk_run(), Some(view(2)));
    one_job(&mut services, &mut b, &mut settings, next);
    assert_eq!(services.bulk_run(), None);
    let end = services.last_batch_end().unwrap();
    assert_eq!((end.serial, end.success), (1, false));
    assert_eq!(b.http.requests.len(), sent + 2, "c-3 never went out");
}

/// CloudProduct (46) DOWNLOAD of one key (`0xf3f24` → `0x1e8a70`): a capture goes the way of
/// a Directory capture (`0x1d9954`, origin `Cloud`: the NC saving destination, re-r34 §2),
/// not through the download queue. A preset (setlist slot copy `0x1dd61c`) is refused before
/// any request.
#[test]
fn a_control_cloud_download_takes_one_capture_as_a_directory_capture() {
    use tetrad_app::device::services::DownloadOrigin;
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let sent = b.http.requests.len();
    let kind = ProductKind::Preset;
    let refused = services.control_download(&mut b, &mut settings, kind, "x".into(), ms(2_000));
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(b.http.requests.len(), sent);
    assert!(services.downloads_in_flight().is_empty());

    let archive = tetrad_services::backup::pack(&[("h-1.cns", b"record"), ("h-1.json", b"{}")]);
    let payload = standard_base64(&archive.unwrap());
    b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
    b.http
        .respond(200, r#"{"id":"c-1","name":"Cab","type":"neural_capture"}"#);
    let started = services.control_download(
        &mut b,
        &mut settings,
        ProductKind::NeuralCapture,
        "c-1".into(),
        ms(2_000),
    );
    let done = settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    let origin = done.into_iter().find_map(|effect| match effect {
        Effect::StoreDownload(download) => Some(download.origin),
        _ => None,
    });
    assert_eq!(origin, Some(DownloadOrigin::Cloud));
    assert!(
        b.http.requests[sent]
            .url
            .ends_with("/products/c-1/download")
    );
}

/// Census K2 (`0x464e30(name, 1)`, C): a cloud capture whose name another user capture holds
/// gets the single text in a single download. In a Multi Select download (`0x843c90` true)
/// the batch text shows once per batch (`DAT_0154af41`, cleared at each batch start
/// `0x464830`). A Control multi-key DOWNLOAD is a batch too (I); it takes captures and IRs.
#[test]
fn a_capture_download_batch_says_name_taken_once() {
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    let batch = "Some of the Neural Captures you selected have names that already exist on \
                 your Quad Cortex and will not be downloaded.";
    let single = "A Neural Capture named Amp already exists on your Quad Cortex. The capture \
                  must be renamed either on cloud or locally on your Quad Cortex.";
    let started =
        services.download_products(&mut b, &mut settings, ids(&["c-1", "c-2"]), ms(2_000));
    assert_eq!(services.downloads_in_flight(), ["c-1", "c-2"]);
    assert_eq!(
        services.capture_name_taken("c-1", "Amp").as_deref(),
        Some(batch)
    );
    assert_eq!(
        services.capture_name_taken("c-2", "Amp"),
        None,
        "once per batch"
    );
    assert_eq!(
        services.capture_name_taken("c-9", "Amp").as_deref(),
        Some(single)
    );
    // No reply: both downloads fail and end. A single download of a batch id is single.
    settle(
        &mut services,
        &mut b,
        &mut settings,
        started.unwrap(),
        ms(2_000),
    );
    assert!(services.downloads_in_flight().is_empty());
    let tap = services.download_product(&mut b, &mut settings, "c-2".into(), ms(2_000));
    assert!(tap.is_ok(), "{tap:?}");
    assert_eq!(
        services.capture_name_taken("c-2", "Amp").as_deref(),
        Some(single)
    );
    // Review r47 N4: the ended batch is over at the next cloud request, so a later store of
    // one of its ids (an OVERWRITE answer) gets the single text again.
    assert_eq!(
        services.capture_name_taken("c-1", "Amp").as_deref(),
        Some(single)
    );

    let kind = ProductKind::NeuralCapture;
    let again = services.control_downloads(&mut b, &mut settings, kind, ids(&["c-3"]), ms(2_000));
    assert!(again.is_ok(), "{again:?}");
    assert_eq!(
        services.capture_name_taken("c-3", "Amp").as_deref(),
        Some(batch)
    );
    assert_eq!(
        services.capture_name_taken("c-1", "Amp").as_deref(),
        Some(single)
    );
    let sent = b.http.requests.len();
    for (kind, ids) in [(ProductKind::Preset, ids(&["p-1"])), (kind, Vec::new())] {
        let refused = services.control_downloads(&mut b, &mut settings, kind, ids, ms(2_000));
        assert!(refused.is_err(), "{refused:?}");
    }
    assert_eq!(b.http.requests.len(), sent);
}

/// Review r28 A8, review r29 B3: the cloud counter moves with each folder change (a page
/// asked, a page landed), and a read leaves it as it is, so a view rebuilds only on a change.
#[test]
fn the_cloud_counter_moves_only_with_a_folder_change() {
    use tetrad_services::cloud::services::{BrowseScope, ProductKind};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let (kind, scope) = (ProductKind::Preset, BrowseScope::OwnedByMe);
    let before = services.cloud_changes();
    b.http
        .respond(200, r#"{"data":[{"productId":"p-1","type":"preset"}]}"#);
    let started =
        (services.browse_cloud(&mut b, &mut settings, kind, scope, true, ms(2_000))).unwrap();
    let asked = services.cloud_changes();
    assert_ne!(asked, before);
    settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    let landed = services.cloud_changes();
    assert_ne!(landed, asked);
    assert_eq!(services.cloud_folder(kind, scope).unwrap().rows.len(), 1);
    services.tick(&mut b, &mut settings, ms(2_100)).unwrap();
    assert_eq!(services.cloud_changes(), landed);
}

/// Review r28 A5: a refused folder request (here: no account) still leaves the folder with
/// its reason, so the Directory stops its busy ring and can say why.
#[test]
fn a_refused_cloud_folder_request_keeps_its_reason() {
    use tetrad_services::cloud::services::{BrowseScope, ProductKind};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    let command = tetrad_ui::UiCommand::BrowseCloud {
        kind: tetrad_ui::ItemKind::Capture,
        scope: tetrad_ui::CloudScope::Downloads,
        refresh: true,
    };
    assert!(
        services
            .on_ui(&mut b, command, &mut settings, ms(2_000))
            .is_err()
    );
    let folder = services.cloud_folder(ProductKind::NeuralCapture, BrowseScope::DownloadQueue);
    let folder = folder.expect("the refused folder exists");
    assert!(folder.rows.is_empty() && !folder.fetching && folder.error.is_some());
}

/// Review r27 R4: a refresh while a folder page is out sends nothing, so the old page cannot
/// land in the fresh list.
#[test]
fn a_cloud_folder_refresh_waits_for_the_page_out() {
    use tetrad_services::cloud::services::{BrowseScope, ProductKind};
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let (kind, scope) = (ProductKind::Preset, BrowseScope::OwnedByMe);
    b.http
        .respond(200, r#"{"data":[{"productId":"p-1","type":"preset"}]}"#);
    let sent = b.http.requests.len();
    let mut started =
        (services.browse_cloud(&mut b, &mut settings, kind, scope, true, ms(2_000))).unwrap();
    let again = services.browse_cloud(&mut b, &mut settings, kind, scope, true, ms(2_000));
    started.extend(again.unwrap());
    settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    assert_eq!(b.http.requests.len(), sent + 1);
    let folder = services.cloud_folder(kind, scope).unwrap();
    assert_eq!(folder.rows.len(), 1);
    assert!(!folder.fetching && folder.error.is_none());
}

/// NC editor DOWNLOAD (`0x3c892c`): `GET products/exists?hash=<hash>` (`0x7bcac8`), then the
/// first result's cloud id is downloaded when it is visible or the account's own (`0x3c5ba4`);
/// a hidden capture of another author is not downloaded and the answer is a Services notice.
/// Review r27 R1: the download carries the editor's hash, so its job ends on the device store;
/// a failure ends it here, and a refused request ends it at once.
#[test]
fn the_nc_editor_download_finds_the_capture_by_hash() {
    use tetrad_app::device::services::{CaptureDownloadEnd, DownloadOutcome};
    use tetrad_services::cloud::products::ProductPayload;
    let failed = |hash: &str| CaptureDownloadEnd {
        hash: hash.into(),
        outcome: DownloadOutcome::Failed,
    };
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let download = |services: &mut Services, b: &mut Backends, settings: &mut DeviceSettings| {
        let command = tetrad_ui::UiCommand::DownloadCapture { hash: "h-1".into() };
        let started = (services.on_ui(b, command, settings, ms(2_000))).unwrap();
        settle(services, b, settings, started, ms(2_000))
    };
    let archive = tetrad_services::backup::pack(&[("h-1.cns", b"record"), ("h-1.json", b"{}")]);
    let payload = standard_base64(&archive.unwrap());
    let found = r#"{"existing":[{"cloud_id":"c-1","hash":"h-1","visible":false,"author":true}]}"#;
    b.http.respond(200, found);
    b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
    b.http
        .respond(200, r#"{"id":"c-1","name":"Amp","type":"neural_capture"}"#);
    let sent = b.http.requests.len();
    let effects = download(&mut services, &mut b, &mut settings);
    let urls: Vec<&str> = (b.http.requests[sent..].iter())
        .map(|r| r.url.as_str())
        .collect();
    assert!(urls[0].ends_with("/products/exists?hash=h-1"), "{urls:?}");
    assert!(urls[1].ends_with("/products/c-1/download") && urls[2].ends_with("/products/c-1"));
    let stored = effects.iter().find_map(|effect| match effect {
        Effect::StoreDownload(download) => Some(download),
        _ => None,
    });
    let stored = stored.expect("the capture goes to the device");
    assert_eq!(stored.product.id, "c-1");
    assert!(matches!(&stored.payload, ProductPayload::Capture { id, .. } if id == "h-1"));
    assert_eq!(
        stored.origin,
        tetrad_app::device::services::DownloadOrigin::Editor("h-1".into())
    );
    assert!(services.take_capture_download_ends().is_empty());

    b.http.respond(
        200,
        r#"{"existing":[{"cloud_id":"c-2","visible":false,"author":false}]}"#,
    );
    let sent = b.http.requests.len();
    let effects = download(&mut services, &mut b, &mut settings);
    assert_eq!(
        b.http.requests.len(),
        sent + 1,
        "no download of a hidden capture"
    );
    assert!(
        texts(&effects)
            .iter()
            .any(|text| text.contains("h-1 not downloaded")),
        "{effects:?}"
    );
    assert_eq!(services.take_capture_download_ends(), [failed("h-1")]);

    let command = tetrad_ui::UiCommand::DownloadCapture {
        hash: String::new(),
    };
    assert!(
        services
            .on_ui(&mut b, command, &mut settings, ms(2_000))
            .is_err()
    );
    assert_eq!(services.take_capture_download_ends(), [failed("")]);
}

/// Review r26 S8: a queued product that the cloud no longer has (HTTP 404 on its download)
/// leaves the download queue (`0x1e48d4` → `0x45ef6c` → `0x1e4db4`), so it does not stay
/// first in the queue at every walk.
#[test]
fn a_queued_product_gone_from_the_cloud_leaves_the_queue() {
    use tetrad_services::cloud::services::ProductKind;
    use tetrad_services::http::Method;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    b.http.respond(
        200,
        r#"{"data":[{"productId":"p-9","type":"neural_capture"}]}"#,
    );
    b.http.respond(404, r#"{"error":3101}"#);
    b.http.respond(200, "");
    let sent = b.http.requests.len();
    let started = services
        .process_downloads_queue(&mut b, &mut settings, ProductKind::NeuralCapture, ms(2_000))
        .unwrap();
    settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    let last = b.http.requests.last().unwrap();
    assert_eq!(b.http.requests.len(), sent + 3);
    assert_eq!(last.method, Method::Delete);
    assert!(
        last.url.ends_with("user/downloaded_products_queue/p-9"),
        "{}",
        last.url
    );
}

/// Review r27 R8: a queue walk starts no download that could take its held payloads over
/// `DOWNLOAD_QUEUE_BYTES`: with 12 MiB IRs, two are held (24 MiB) and the third stays queued.
#[test]
fn a_queue_walk_holds_at_most_its_byte_bound() {
    use tetrad_app::device::services::DOWNLOAD_QUEUE_BYTES;
    use tetrad_services::cloud::services::ProductKind;
    let mut services = Services::new();
    let mut b = Backends::default();
    let mut settings = settings();
    signed_in(&mut services, &mut b, &mut settings);
    let items: Vec<String> = (1..=3)
        .map(|i| format!(r#"{{"productId":"p-{i}","type":"impulse_response"}}"#))
        .collect();
    b.http
        .respond(200, format!(r#"{{"data":[{}]}}"#, items.join(",")));
    let payload = standard_base64(&vec![0; 12 << 20]);
    for i in 1..=2 {
        b.http.respond(200, format!(r#"{{"payload":"{payload}"}}"#));
        b.http.respond(
            200,
            format!(r#"{{"id":"p-{i}","name":"Cab","type":"impulse_response"}}"#),
        );
    }
    let started = services
        .process_downloads_queue(
            &mut b,
            &mut settings,
            ProductKind::ImpulseResponse,
            ms(2_000),
        )
        .unwrap();
    let effects = settle(&mut services, &mut b, &mut settings, started, ms(2_000));
    let held: usize = (effects.iter())
        .filter_map(|effect| match effect {
            Effect::StoreDownload(download) => Some(download.payload.size()),
            _ => None,
        })
        .sum();
    assert_eq!(held, 24 << 20);
    assert!(held <= DOWNLOAD_QUEUE_BYTES);
    assert!(
        !(b.http.requests.iter()).any(|r| r.url.ends_with("products/p-3/download")),
        "p-3 stays queued"
    );
}

/// `0x6fffe4`: a capture upload's meta-signature is the seed-only capture cipher (secret slot
/// 0) over the hex SHA-256 of the capture metadata, not the seed-and-serial cipher. The seed
/// and serial are made up.
#[test]
fn capture_meta_signature_uses_the_seed_only_cipher() {
    use tetrad_app::device::services::capture_meta_signature;
    use tetrad_media::factory::{DeviceCipher, DeviceSerial, FactoryIrSeed};
    use tetrad_services::cloud::services::product_digest;
    let seed = FactoryIrSeed::new(&[0x42; 57]).unwrap();
    let metadata = br#"{"name":"My Amp"}"#;
    let signature = capture_meta_signature(&DeviceCipher::seed(&seed), metadata);
    let digest = product_digest(metadata).into_bytes();
    let mut plain = signature.clone();
    DeviceCipher::seed(&seed).apply(&mut plain);
    assert_eq!(plain, digest);
    let serial = DeviceSerial::parse(b"Q0000TEST").unwrap();
    let mut other = signature;
    DeviceCipher::seed_and_serial(&seed, &serial).apply(&mut other);
    assert_ne!(other, digest);
}

/// Stock `0x3fcf64` re-reads the restored settings but keeps this device's `AtmaVolume` and
/// `customName`. Before its restart Tetrad writes only those two from memory into the restored
/// `ZenjackConfig.ini`; every other value stays as restored.
#[test]
fn a_restore_keeps_only_the_device_volume_and_name() {
    use tetrad_app::device::services::keep_device_values_after_restore;
    use tetrad_settings::controls::AtmaVolume;
    let mut storage = MemSettings::default();
    let mut restored = settings();
    restored.update(|s| {
        s.general.custom_name = Some("Backup QC".into());
        s.display.dimmed_preset = true;
    });
    restored.flush(&mut storage).unwrap();
    let mut memory = settings();
    memory.update(|s| {
        s.general.custom_name = Some("Stage QC".into());
        s.atma_volume = AtmaVolume::new(0.8).unwrap();
        s.display.dimmed_preset = false;
    });
    keep_device_values_after_restore(&mut storage, memory.settings(), Hardware::QuadCortex)
        .unwrap();
    let (after, _) = DeviceSettings::load(&storage, Hardware::QuadCortex).unwrap();
    let after = after.settings();
    assert_eq!(after.general.custom_name.as_deref(), Some("Stage QC"));
    assert_eq!(after.atma_volume, memory.settings().atma_volume);
    assert!(after.display.dimmed_preset, "the restored value stays");
}

/// `0x3fcf64` in place (review r18 F1, F3): while a restore job extracts, the library is off
/// the device and the settings flush waits. A good extract answers Control (`applied_backup`)
/// and reloads with no restart: the settings come from the restored files (a change made
/// during the extract is gone; nothing in memory was written over them), the library is back
/// with `zenui.db` open, preset 0 of the Factory Library plays (`0x5b37f0`) with no undo frame
/// left (`0x647ac0`), and settings flushes run again.
#[test]
fn a_restore_reloads_in_place_from_the_restored_files() {
    use tetrad_app::device::actions::Action;
    use tetrad_app::device::services::BackupDone;
    use tetrad_app::device::{Event, FACTORY_SETLIST};
    use tetrad_dsp_host::session::Clock;
    use tetrad_storage::media::{SETLIST_SLOTS, encode_setlist};
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let factory = media.0.join("factory/Factory Library");
    std::fs::create_dir_all(&factory).unwrap();
    let one = common::light_preset(&specs, 4);
    std::fs::write(factory.join("One.pb"), one.to_bytes()).unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[0] = Some(format!("{FACTORY_SETLIST}/One.pb"));
    std::fs::write(factory.join("setlist"), encode_setlist(&slots).unwrap()).unwrap();
    let (mut device, log, _media) = common::boot(media);
    common::connect_control(&mut device);
    let before = device.state().settings.settings().general.bypass_save_mode;
    start_control_restore(&mut device);
    assert!(
        device.state().library.is_none(),
        "the library is off the device"
    );
    device.push(Event::Action(Action::ChangeSetting {
        id: tetrad_ui::SettingId::SceneBypassBehavior,
        value: tetrad_ui::SettingValue::Choice(2),
    }));
    let writes = log.borrow().settings_writes.len();
    // Past the 250 ms `ZenjackConfig.ini` save delay, inside the Control keep-alive.
    let sleep = |device: &tetrad_app::device::Device<common::Sim>| {
        device
            .platform()
            .clock
            .clone()
            .sleep(Duration::from_secs(1));
    };
    sleep(&device);
    assert_eq!((0..20).find_map(|_| device.step()), None);
    assert_eq!(
        log.borrow().settings_writes.len(),
        writes,
        "the flush waits"
    );
    assert_ne!(
        device.state().settings.settings().general.bypass_save_mode,
        before
    );

    let mark = log.borrow().requests.len();
    let action = finish_restore(&mut device, Ok(BackupDone::Restored(3)));
    assert_eq!(action, None, "no restart");
    assert!(
        local_backup_replies(&log.borrow(), mark).contains(&Some(true)),
        "Control hears of the restore"
    );
    let state = device.state();
    assert_eq!(
        state.settings.settings().general.bypass_save_mode,
        before,
        "the restored file's value"
    );
    let library = state.library.as_ref();
    assert!(library.is_some_and(|library| library.database.is_some()));
    let last = &state.settings.settings().last_preset;
    assert_eq!(
        (last.setlist.as_deref(), last.preset_index),
        (Some(FACTORY_SETLIST), 0)
    );
    let path = state.current.as_ref().unwrap().path.clone();
    assert_eq!(path, Some(format!("{FACTORY_SETLIST}/One.pb")));
    assert_eq!(state.history.frame_types(), (vec![], vec![]));

    // Flushes run again.
    device.push(Event::Action(Action::ChangeSetting {
        id: tetrad_ui::SettingId::SceneBypassBehavior,
        value: tetrad_ui::SettingValue::Choice(2),
    }));
    let writes = log.borrow().settings_writes.len();
    assert_eq!(device.step(), None);
    sleep(&device);
    assert_eq!((0..20).find_map(|_| device.step()), None);
    assert!(log.borrow().settings_writes[writes..].contains(&SettingsFileId::Zenjack));
}

/// `0x3fcf64` step 7 (`0x6a6358(db, 1)`, `0x6a68f4(db, 1)`): a restored `zenui.db` whose user
/// capture store is flagged global (a backup from another unit) is re-keyed by the reload, with
/// no restart: the store is flagged local and the row's metadata moves from the seed-only
/// cipher to this device's cipher (made-up secrets).
#[test]
fn a_restore_reload_moves_a_global_capture_store_to_this_device() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::media::{CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED};
    use tetrad_app::device::platform::Platform as _;
    use tetrad_app::device::services::BackupDone;
    use tetrad_media::factory::DeviceCipher;
    use tetrad_storage::stock::{
        CaptureEncryption, CaptureStore, DbLimits, ProductType, StockDb, StockProduct,
    };
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    let local = DeviceCipher::caller_secret(b"invented device secret");
    sim.media_cipher = Some(local.clone());
    let files = sim.services().files().clone();
    files.put(CAPTURE_CORRUPTION_CHECKED, "");
    files.put(CAPTURE_CONVERSION_FINISHED, "");
    let mut device = tetrad_app::device::Device::start(sim).unwrap();
    let seed = DeviceCipher::caller_secret(b"invented seed-only secret");
    device.set_capture_seed(seed.clone());
    common::connect_control(&mut device);
    start_control_restore(&mut device);

    // The extract left a database from another unit (`zenui.db` is closed meanwhile).
    let sealed = |cipher: &DeviceCipher| {
        let mut bytes = br#"{"name":"Other"}"#.to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let mut db = StockDb::open(&media.0.join("zenui.db"), DbLimits::default(), false).unwrap();
    db.transaction(|write| {
        write.set_capture_encryption(CaptureStore::User, CaptureEncryption::Global)?;
        write.put_product(&StockProduct {
            id: "other".into(),
            product_type: ProductType::Capture,
            read_only: false,
            raw_data: Some(vec![1, 2, 3]),
            metadata: Some(sealed(&seed)),
        })
    })
    .unwrap();
    drop(db);
    assert_eq!(
        finish_restore(&mut device, Ok(BackupDone::Restored(3))),
        None
    );

    let view = (device.state().library.as_ref())
        .and_then(|library| library.database.as_ref())
        .unwrap()
        .view();
    assert_eq!(
        view.capture_encryption(CaptureStore::User).unwrap(),
        CaptureEncryption::Local
    );
    let metadata = view.product("other").unwrap().and_then(|row| row.metadata);
    assert_eq!(metadata, Some(sealed(&local)));
}

/// `0x480bcc`: a backup from an Atma restored on a Quad Cortex gets its input, return and
/// capture dialog levels moved from −12..30 dB to −12..60 dB; a missing key counts as 0 dB of
/// the source. A Quad Cortex backup on an Atma clamps at +30 dB. Equal hardware writes nothing.
#[test]
fn a_backup_from_other_hardware_moves_its_levels_to_this_range() {
    use tetrad_app::device::services::convert_backup_gains;
    use tetrad_settings::{SettingsFile, Variant};
    let level = |storage: &MemSettings, key: &str| {
        let bytes = storage.read(SettingsFileId::Zenjack).unwrap().unwrap();
        let file = SettingsFile::parse(&bytes).0;
        file.value(key).unwrap().unwrap().to_f32().unwrap()
    };
    let mut file = SettingsFile::new();
    file.set("ConnectorsSettings/100", &Variant::Float(1.0));
    let mut storage = MemSettings::default();
    storage
        .write(SettingsFileId::Zenjack, &file.save())
        .unwrap();

    convert_backup_gains(&mut storage, Hardware::QuadCortex, Hardware::QuadCortex).unwrap();
    assert_eq!(level(&storage, "ConnectorsSettings/100"), 1.0);
    assert!(
        SettingsFile::parse(&storage.0[&SettingsFileId::Zenjack])
            .0
            .value("ConnectorsSettings/200")
            .is_none()
    );

    convert_backup_gains(&mut storage, Hardware::Atma, Hardware::QuadCortex).unwrap();
    assert!((level(&storage, "ConnectorsSettings/100") - 42.0 / 72.0).abs() < 1e-6);
    assert!((level(&storage, "NeuralCaptureDialogSettings/InLevel") - 12.0 / 72.0).abs() < 1e-6);

    let mut file = SettingsFile::new();
    file.set("ConnectorsSettings/500", &Variant::Float(1.0));
    storage
        .write(SettingsFileId::Zenjack, &file.save())
        .unwrap();
    convert_backup_gains(&mut storage, Hardware::QuadCortex, Hardware::Atma).unwrap();
    assert_eq!(level(&storage, "ConnectorsSettings/500"), 1.0);
}

/// The restore reload (`0x3fcf64`): a good extract sets the capture markers (`0x7038a8`,
/// `0x6f6218`, `0x6f5fa0`) and the reload runs the boot media steps on them: the legacy
/// conversion writes its marker again; the capture translation and the corruption check wait
/// (the flag stays, the check marker stays missing), as the sim has no ZenUI seed or device
/// cipher. The backup's hardware (`creator` "atma") converts the levels in the reload's first
/// flush (review r23 K9), once: a later change flushes as usual.
#[test]
fn a_restore_sets_the_capture_markers_and_converts_the_levels_once() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use tetrad_app::device::Event;
    use tetrad_app::device::actions::Action;
    use tetrad_app::device::media::{
        CAPTURE_CONVERSION_FINISHED, CAPTURE_CORRUPTION_CHECKED, CAPTURE_TRANSLATION_FLAG,
    };
    use tetrad_app::device::platform::Platform as _;
    use tetrad_app::device::services::BackupDone;
    use tetrad_dsp_host::session::Clock;
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let log = Rc::new(RefCell::new(common::Log::default()));
    let mut sim = common::Sim::new(common::STOCK, &media, &log);
    let files = sim.services().files().clone();
    files.put(CAPTURE_CORRUPTION_CHECKED, "");
    files.put(CAPTURE_CONVERSION_FINISHED, "");
    let mut device = tetrad_app::device::Device::start(sim).unwrap();
    common::connect_control(&mut device);
    // No settings flush is pending when the restore starts.
    device
        .platform()
        .clock
        .clone()
        .sleep(Duration::from_secs(1));
    assert_eq!((0..20).find_map(|_| device.step()), None);
    start_control_restore_of(&mut device, r#"{"creator": "atma", "payload": "x"}"#);
    let writes = log.borrow().settings_writes.len();
    let action = finish_restore(&mut device, Ok(BackupDone::Restored(3)));
    assert_eq!(action, None, "no restart");
    assert_eq!(
        log.borrow().settings_writes[writes..],
        [SettingsFileId::Zenjack],
        "one conversion write"
    );
    assert_eq!(files.get(CAPTURE_TRANSLATION_FLAG), Some(Vec::new()));
    assert_eq!(files.get(CAPTURE_CORRUPTION_CHECKED), None);
    assert_eq!(files.get(CAPTURE_CONVERSION_FINISHED), Some(Vec::new()));

    device.push(Event::Action(Action::ChangeSetting {
        id: tetrad_ui::SettingId::SceneBypassBehavior,
        value: tetrad_ui::SettingValue::Choice(1),
    }));
    assert_eq!(device.step(), None);
    device
        .platform()
        .clock
        .clone()
        .sleep(Duration::from_secs(1));
    assert_eq!((0..20).find_map(|_| device.step()), None);
    let written = &log.borrow().settings_writes[writes..];
    assert_eq!(written, [SettingsFileId::Zenjack; 2]);
}

/// Review r18 F7: a failed restore gives the library back with `zenui.db` open again. When
/// the extract left a file that does not open (two tries), the device restarts instead of
/// running the rest of the session without its library.
#[test]
fn a_failed_restore_reopens_the_library_or_restarts() {
    use tetrad_services::backup::BackupError;
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, _log, media) = common::boot(media);
    common::connect_control(&mut device);
    let corrupt = || Err(JobError::Backup(BackupError::Corrupt("synthetic".into())));

    start_control_restore(&mut device);
    assert_eq!(finish_restore(&mut device, corrupt()), None);
    let library = device.state().library.as_ref();
    assert!(library.is_some_and(|library| library.database.is_some()));

    start_control_restore(&mut device);
    std::fs::write(media.0.join("zenui.db"), b"not a database").unwrap();
    let action = finish_restore(&mut device, corrupt());
    assert_eq!(action, Some(PowerAction::Restart(RestartReason::Direct)));
    assert!(device.state().library.is_some());
}

/// Review r19 F3: while a restore extracts, nothing stops the process with the user data
/// half written. The POWER key is dropped (r20-ui hold: the user presses again after the
/// restore). A restart from another source, here an install job result, is held and runs when
/// the restore ends: after a failed extract, and after the reload of a good one.
#[test]
fn power_requests_wait_for_the_restore_extract() {
    use tetrad_app::device::platform::Input;
    use tetrad_dsp_host::session::Clock;
    use tetrad_services::backup::BackupError;
    use tetrad_services::updater::install::InstallOutcome;
    let specs = common::STOCK.specs();
    let media = common::Media::new([("Light", common::light_preset(&specs, 0))]);
    let (mut device, _log, _media) = common::boot(media);
    common::connect_control(&mut device);
    start_control_restore(&mut device);

    let installed = Input::Service(ServiceNotice::Install(Ok(InstallOutcome::Installed)));
    for input in [Input::Power(PowerAction::PowerOff), installed] {
        (device.platform().inputs.borrow_mut()).push(input);
    }
    let mut clock = device.platform().clock.clone();
    let during = (0..20).find_map(|_| {
        clock.sleep(Duration::from_millis(50));
        device.step()
    });
    assert_eq!(during, None, "no power action during the extract");

    let corrupt = Err(JobError::Backup(BackupError::Corrupt("synthetic".into())));
    assert_eq!(
        finish_restore(&mut device, corrupt),
        Some(PowerAction::Restart(RestartReason::Direct)),
        "the held restart runs after the failed restore"
    );
    let library = device.state().library.as_ref();
    assert!(library.is_some_and(|library| library.database.is_some()));

    start_control_restore(&mut device);
    let installed = Input::Service(ServiceNotice::Install(Ok(InstallOutcome::Installed)));
    (device.platform().inputs.borrow_mut()).push(installed);
    assert_eq!((0..5).find_map(|_| device.step()), None);
    assert_eq!(
        finish_restore(
            &mut device,
            Ok(tetrad_app::device::services::BackupDone::Restored(3))
        ),
        Some(PowerAction::Restart(RestartReason::Direct)),
        "the held restart runs after the reload"
    );
    let library = device.state().library.as_ref();
    assert!(library.is_some_and(|library| library.database.is_some()));
}

/// Tetrad Updates: CHECK TETRAD finds a newer release, UPDATE TETRAD installs and arms it and
/// restarts the unit. While it runs, a CorOS install is refused.
#[test]
fn a_tetrad_update_installs_arms_and_restarts() {
    use tetrad_extensions::catalog::sha256_hex;
    use tetrad_services::updater::tetrad::{FileEntry, Manifest, Offer};
    use tetrad_ui::{TetradUpdateCommand, TetradUpdateState, UiCommand};
    let mut services = Services::new();
    let mut b = Backends::default();
    b.http = ScriptedHttp::sharing(b.files.clone());
    let mut settings = settings();
    let command = |command| UiCommand::TetradUpdate { command };

    // CHECK TETRAD goes to the worker; its signed manifest is tested in tetrad-services, so
    // the check result comes in directly here.
    let check = (services.on_ui(
        &mut b,
        command(TetradUpdateCommand::Check),
        &mut settings,
        ms(0),
    ))
    .unwrap();
    assert_eq!(check, [Effect::Service(ServiceJob::TetradCheck)]);
    let entry = |body: &[u8]| FileEntry {
        size: body.len() as u64,
        sha256: sha256_hex(body),
    };
    let files = [
        ("bin/tetrad", entry(b"elf")),
        ("launcher/tetrad-launch.sh", entry(b"sh")),
        ("launcher/tetrad-launcher.sh", entry(b"sh")),
        ("launcher/tetrad-update.sh", entry(b"sh")),
    ];
    let offer = Offer {
        tag: "v99.0.0".into(),
        manifest: Manifest {
            version: "99.0.0".parse().unwrap(),
            files: files.map(|(path, entry)| (path.to_owned(), entry)).into(),
        },
    };
    let checked = ServiceNotice::TetradChecked(Ok(Some(offer)));
    services
        .on_notice(&mut b, checked, &mut settings, ms(0))
        .unwrap();
    assert_eq!(
        services.tetrad_update_view().state,
        TetradUpdateState::Available {
            version: "99.0.0".into()
        }
    );

    let install = services
        .on_ui(
            &mut b,
            command(TetradUpdateCommand::Install),
            &mut settings,
            ms(10),
        )
        .unwrap();
    services
        .on_notice(
            &mut b,
            ServiceNotice::Download(Ok(())),
            &mut settings,
            ms(10),
        )
        .unwrap();
    assert!(
        services.install().is_err(),
        "no CorOS install during a Tetrad install"
    );
    assert_eq!(
        services.download().unwrap_err().to_string(),
        "a Tetrad update is installing"
    );

    for body in ["elf", "sh", "sh", "sh"] {
        b.http.respond(200, body);
    }
    let parts = [
        "bin/tetrad",
        "launcher/tetrad-launch.sh",
        "launcher/tetrad-launcher.sh",
        "launcher/tetrad-update.sh",
    ]
    .map(|path| format!("/media/p4/tetrad/{path}.part"));
    b.runner
        .expect_ok(&format!("chmod 755 {}", parts.join(" ")));
    for part in &parts[1..] {
        b.runner.expect_ok(&format!("sh -n {part}"));
    }
    b.runner
        .expect_ok("/media/p4/tetrad/bin/tetrad.part run --check")
        .expect_ok("sh /media/p4/tetrad/launcher/tetrad-launcher.sh arm");
    let done = settle(&mut services, &mut b, &mut settings, install, ms(20));
    assert!(b.runner.is_done(), "{:?}", b.runner.lines());
    assert!(done.contains(&Effect::Power(PowerAction::Restart(RestartReason::Direct))));
    assert_eq!(
        b.files.get("/media/p4/tetrad/bin/tetrad").as_deref(),
        Some(&b"elf"[..])
    );
}
