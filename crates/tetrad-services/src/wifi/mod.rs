//! Wi-Fi: interface bring-up, supplicant control, scan, connect, saved networks and DHCP, as the
//! ZenUI Wi-Fi worker does it (bring-up VAs 0x0088e1b8, 0x0088df3c, 0x008931b8, 0x00892dcc;
//! control wrapper 0x008895f0; connect 0x0088bce8, 0x0088d354; teardown 0x00889f0c).

pub mod config;
pub mod events;
pub mod psk;
pub mod scan;

use std::path::Path;

use thiserror::Error;

use crate::dhcp::{self, AppliedLease, DhcpError};
use crate::exec::{Command, CommandError, CommandRunner, ExitStatus, FileError, Files};
use events::{Association, Reaction, WifiAction, WifiEvent, WifiNotice, WifiStatus};
use psk::Psk;
use scan::{SavedNetwork, ScanEntry, Security};

pub const INTERFACE: &str = dhcp::INTERFACE;
pub const CONFIG_PATH: &str = "/media/p4/wpa_supplicant.conf";
/// The command socket the link connects to.
pub const CONTROL_SOCKET: &str = "/var/run/wpa_supplicant/wlan0";
/// Wait after `TERMINATE` before closing the links (`usleep(60000)`, VA 0x00889fcc).
pub const TERMINATE_WAIT_MS: u64 = 60;
/// Owned bound on `BSS <i>` requests per scan.
pub const MAX_BSS: u32 = 256;

/// One supplicant control connection (tetrad-sys `supplicant::Transport` in production). Commands
/// can carry a derived PSK; implementations must not log them.
pub trait SupplicantLink {
    fn request(&mut self, command: &str) -> Result<String, LinkError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LinkError {
    #[error("supplicant request timed out")]
    Timeout,
    #[error("supplicant link failed: {0}")]
    Transport(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WifiError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("`{command}` ended with {status:?}")]
    Failed { command: String, status: ExitStatus },
    #[error(transparent)]
    File(#[from] FileError),
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(transparent)]
    Dhcp(#[from] DhcpError),
    /// The supplicant answered `FAIL…` (stock wrapper returns -1). The command is not echoed.
    #[error("supplicant refused `{0}`")]
    Refused(&'static str),
    #[error("supplicant gave an unexpected reply to `{0}`")]
    Unexpected(&'static str),
    /// Stock "Failed to find %s in scan results!": only scanned networks can be added.
    #[error("network is not in the scan results")]
    NotInScanResults,
    #[error("WPA passphrase must be 8 to 63 characters")]
    InvalidPassphrase,
    /// Stock cannot set WEP keys or EAP identities either; it would join without them.
    #[error("{0:?} networks are not supported")]
    Unsupported(Security),
}

/// Send `command` and apply the stock reply rules (VA 0x008895f0): `FAIL…` is an error,
/// surrounding quotes and whitespace are trimmed. `name` labels errors without the payload.
pub fn control(
    link: &mut impl SupplicantLink,
    name: &'static str,
    command: &str,
) -> Result<String, WifiError> {
    let reply = link.request(command)?;
    if reply.starts_with("FAIL") {
        return Err(WifiError::Refused(name));
    }
    Ok(reply.trim().trim_matches('"').to_owned())
}

fn expect_ok(
    link: &mut impl SupplicantLink,
    name: &'static str,
    command: &str,
) -> Result<(), WifiError> {
    match control(link, name, command)?.as_str() {
        "OK" => Ok(()),
        _ => Err(WifiError::Unexpected(name)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Read the interface MAC (`/sys/class/net/wlan0/address`, VA 0x0066c4b8).
pub fn mac_address(files: &mut impl Files) -> Result<Option<String>, FileError> {
    Ok(files
        .read(Path::new("/sys/class/net/wlan0/address"))?
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned()))
}

fn run(runner: &mut impl CommandRunner, program: &str, args: &[&str]) -> Result<(), WifiError> {
    let command = Command::new(program, args.iter().copied());
    let output = runner.run(&command)?;
    if !output.status.success() {
        return Err(WifiError::Failed {
            command: command.line(),
            status: output.status,
        });
    }
    Ok(())
}

/// Load the driver, raise the interface, prepare the saved config and start the supplicant in
/// the background. Each step needs exit 0 before the next, as stock.
pub fn bring_up(runner: &mut impl CommandRunner, files: &mut impl Files) -> Result<(), WifiError> {
    run(runner, "modprobe", &["wilc-spi"])?;
    run(runner, "ifconfig", &[INTERFACE, "up"])?;
    config::prepare(files)?;
    run(
        runner,
        "wpa_supplicant",
        &[
            "-B",
            "-Dnl80211",
            "-iwlan0",
            "-c/media/p4/wpa_supplicant.conf",
        ],
    )
}

/// Stock first requests on a new command link (VAs 0x0088f974, 0x00892dcc), then `SCAN`.
pub fn initialize(link: &mut impl SupplicantLink) -> Result<(), WifiError> {
    control(link, "INTERFACES", "INTERFACES")?;
    // EAP capability only informs the UI; a refusal is not fatal.
    let _ = control(link, "GET_CAPABILITY", "GET_CAPABILITY eap");
    control(link, "LIST_NETWORKS", "LIST_NETWORKS")?;
    expect_ok(link, "ENABLE_NETWORK", "ENABLE_NETWORK all")?;
    expect_ok(link, "SAVE_CONFIG", "SAVE_CONFIG")?;
    // Owned (O): a refused SCAN is not fatal. On the QC (S8, 2026-10-15) the supplicant refused
    // it right after ENABLE_NETWORK (it was already scanning); failing here made every service
    // retry start a second `wpa_supplicant` (exit 255) while the first one associated.
    let _ = expect_ok(link, "SCAN", "SCAN");
    Ok(())
}

pub fn saved_networks(link: &mut impl SupplicantLink) -> Result<Vec<SavedNetwork>, WifiError> {
    let reply = link.request("LIST_NETWORKS")?;
    if reply.starts_with("FAIL") {
        return Err(WifiError::Refused("LIST_NETWORKS"));
    }
    Ok(scan::parse_network_list(&reply))
}

/// `REMOVE_NETWORK <id>` (or `all`), then `SAVE_CONFIG`.
pub fn forget(link: &mut impl SupplicantLink, id: Option<u32>) -> Result<(), WifiError> {
    let target = id.map_or_else(|| "all".to_owned(), |id| id.to_string());
    expect_ok(link, "REMOVE_NETWORK", &format!("REMOVE_NETWORK {target}"))?;
    expect_ok(link, "SAVE_CONFIG", "SAVE_CONFIG")
}

/// `DISCONNECT`: leave the joined network; the supplicant events update the status.
pub fn disconnect(link: &mut impl SupplicantLink) -> Result<(), WifiError> {
    expect_ok(link, "DISCONNECT", "DISCONNECT")
}

/// A DHCP run the caller performs off the calling thread, then reports with
/// [`WifiService::dhcp_finished`]. A run that was stopped or superseded is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpRun {
    id: u32,
}

/// Result of a state-machine call: notices for the UI and at most one DHCP run to start.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Step {
    pub notices: Vec<WifiNotice>,
    pub dhcp: Option<DhcpRun>,
}

/// Wi-Fi state: the association state machine, the last scan, the lease and the DHCP run in
/// flight. It holds no backend; each executor method borrows the link and runner it needs, so
/// the platform keeps ownership of them.
#[derive(Debug)]
pub struct WifiService {
    association: Association,
    lease: Option<AppliedLease>,
    scan: Vec<ScanEntry>,
    dhcp_runs: u32,
    pending_dhcp: Option<DhcpRun>,
}

impl WifiService {
    /// Use [`events::STOCK_ASSOCIATION_TIMEOUT_MS`] for stock timing.
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            association: Association::new(timeout_ms),
            lease: None,
            scan: Vec::new(),
            dhcp_runs: 0,
            pending_dhcp: None,
        }
    }

    pub fn association(&self) -> &Association {
        &self.association
    }

    /// The lease applied by the last successful DHCP run, cleared when association is lost.
    pub fn lease(&self) -> Option<AppliedLease> {
        self.lease
    }

    /// The last scan, one entry per SSID.
    pub fn scan_entries(&self) -> &[ScanEntry] {
        &self.scan
    }

    /// Request a scan. Stock drops the request unless the state is Scanned, Disconnected,
    /// Rejected, TimedOut or Completed (VA 0x00890af0); then this returns `false`.
    pub fn scan(&self, link: &mut impl SupplicantLink) -> Result<bool, WifiError> {
        use WifiStatus::*;
        if !matches!(
            self.association.status(),
            Scanned | Disconnected | Rejected | TimedOut | Completed
        ) {
            return Ok(false);
        }
        expect_ok(link, "SCAN", "SCAN")?;
        Ok(true)
    }

    /// Read `BSS 0`, `BSS 1`, … until a refusal or an empty reply. Call after `ScanResults`.
    pub fn read_scan(&mut self, link: &mut impl SupplicantLink) -> Result<&[ScanEntry], WifiError> {
        let mut entries = Vec::new();
        for index in 0..MAX_BSS {
            let reply = link.request(&format!("BSS {index}"))?;
            if reply.trim().is_empty() || reply.starts_with("FAIL") {
                break;
            }
            if let Some(entry) = scan::parse_bss(&reply) {
                scan::push_unique(&mut entries, entry);
            }
        }
        self.scan = entries;
        Ok(&self.scan)
    }

    /// Join `ssid`. A saved network is re-enabled and keeps its stored PSK (stock ignores a new
    /// passphrase). A new one must be in the last scan; it is added with an SSID in hex and a
    /// PBKDF2 PSK, never the passphrase. Returns the network id; association follows as events.
    pub fn connect(
        &mut self,
        link: &mut impl SupplicantLink,
        ssid: &str,
        passphrase: Option<&str>,
        now_ms: u64,
    ) -> Result<u32, WifiError> {
        expect_ok(link, "DISABLE_NETWORK", "DISABLE_NETWORK all")?;
        let saved = saved_networks(link)?;
        let (id, added) = match saved.iter().find(|n| n.ssid == ssid) {
            Some(network) => (network.id, false),
            None => (self.add_network(link, ssid, passphrase)?, true),
        };
        for other in saved.iter().filter(|n| n.id != id) {
            expect_ok(
                link,
                "SET_NETWORK",
                &format!("SET_NETWORK {} priority 0", other.id),
            )?;
        }
        expect_ok(link, "SET_NETWORK", &format!("SET_NETWORK {id} priority 1"))?;
        expect_ok(link, "ENABLE_NETWORK", &format!("ENABLE_NETWORK {id}"))?;
        expect_ok(link, "SAVE_CONFIG", "SAVE_CONFIG")?;
        self.association.begin(id, added, now_ms);
        Ok(id)
    }

    /// Stock `SET_NETWORK` sequence for a scanned network (VA 0x0088d354).
    fn add_network(
        &self,
        link: &mut impl SupplicantLink,
        ssid: &str,
        passphrase: Option<&str>,
    ) -> Result<u32, WifiError> {
        let security = self
            .scan
            .iter()
            .find(|e| e.ssid == ssid)
            .ok_or(WifiError::NotInScanResults)?
            .security;
        let psk = match security {
            Security::Open => None,
            Security::WpaPsk | Security::Wpa2Psk => Some(
                passphrase
                    .and_then(|p| Psk::derive(p, ssid.as_bytes()))
                    .ok_or(WifiError::InvalidPassphrase)?,
            ),
            other => return Err(WifiError::Unsupported(other)),
        };
        let id: u32 = control(link, "ADD_NETWORK", "ADD_NETWORK")?
            .parse()
            .map_err(|_| WifiError::Unexpected("ADD_NETWORK"))?;
        let mut settings = vec![
            format!("ssid {}", hex(ssid.as_bytes())),
            "auth_alg OPEN".to_owned(),
        ];
        match &psk {
            None => settings.push("key_mgmt NONE".into()),
            Some(psk) => {
                let proto = if security == Security::Wpa2Psk {
                    "WPA2"
                } else {
                    "WPA"
                };
                settings.extend([
                    format!("proto {proto}"),
                    "key_mgmt WPA-PSK".into(),
                    "pairwise CCMP TKIP".into(),
                    "group TKIP CCMP WEP104 WEP40".into(),
                    format!("psk {}", psk.hex()),
                ]);
            }
        }
        for setting in settings {
            if let Err(error) =
                expect_ok(link, "SET_NETWORK", &format!("SET_NETWORK {id} {setting}"))
            {
                // Owned cleanup: do not leave a half-configured network behind.
                let _ = control(link, "REMOVE_NETWORK", &format!("REMOVE_NETWORK {id}"));
                return Err(error);
            }
        }
        Ok(id)
    }

    /// Feed one monitor datagram. Unknown events are ignored. Start the run in [`Step::dhcp`]
    /// on a worker, not on the thread that feeds events.
    pub fn on_event(
        &mut self,
        link: &mut impl SupplicantLink,
        runner: &mut impl CommandRunner,
        datagram: &str,
        now_ms: u64,
    ) -> Result<Step, WifiError> {
        match events::parse_event(datagram) {
            Some(event) => {
                let reaction = self.association.on_event(event, now_ms);
                self.execute(link, runner, reaction)
            }
            None => Ok(Step::default()),
        }
    }

    /// Expire an association attempt; call periodically (stock polls events every 60 ms).
    pub fn on_tick(
        &mut self,
        link: &mut impl SupplicantLink,
        runner: &mut impl CommandRunner,
        now_ms: u64,
    ) -> Result<Step, WifiError> {
        let reaction = self.association.on_tick(now_ms);
        self.execute(link, runner, reaction)
    }

    /// Report the result of the run in [`Step::dhcp`]. A stopped or superseded run gives an
    /// empty step. The next retry comes back in the returned step.
    pub fn dhcp_finished(
        &mut self,
        link: &mut impl SupplicantLink,
        runner: &mut impl CommandRunner,
        run: DhcpRun,
        result: Result<AppliedLease, DhcpError>,
    ) -> Result<Step, WifiError> {
        if self.pending_dhcp != Some(run) {
            return Ok(Step::default());
        }
        self.pending_dhcp = None;
        self.lease = result.as_ref().ok().copied();
        let reaction = self.association.on_dhcp(result.is_ok());
        self.execute(link, runner, reaction)
    }

    /// Run actions in order. `StartDhcp` is not run here; it becomes [`Step::dhcp`].
    fn execute(
        &mut self,
        link: &mut impl SupplicantLink,
        runner: &mut impl CommandRunner,
        reaction: Reaction,
    ) -> Result<Step, WifiError> {
        let mut step = Step {
            notices: reaction.notices,
            dhcp: None,
        };
        for action in reaction.actions {
            match action {
                WifiAction::StartDhcp => {
                    self.dhcp_runs += 1;
                    let run = DhcpRun { id: self.dhcp_runs };
                    self.pending_dhcp = Some(run);
                    step.dhcp = Some(run);
                }
                WifiAction::StopDhcp => self.stop_dhcp(runner)?,
                WifiAction::ResetInterface => {
                    run(runner, "ifconfig", &[INTERFACE, "down"])?;
                    run(runner, "ifconfig", &[INTERFACE, "up"])?;
                }
                WifiAction::DisableNetwork(id) => {
                    control(link, "DISABLE_NETWORK", &format!("DISABLE_NETWORK {id}"))?;
                    expect_ok(link, "SAVE_CONFIG", "SAVE_CONFIG")?;
                }
                WifiAction::RemoveNetwork(id) => forget(link, Some(id))?,
                WifiAction::DisableOthers(keep) => {
                    for other in saved_networks(link)?.iter().filter(|n| n.id != keep) {
                        control(
                            link,
                            "DISABLE_NETWORK",
                            &format!("DISABLE_NETWORK {}", other.id),
                        )?;
                    }
                    expect_ok(link, "SAVE_CONFIG", "SAVE_CONFIG")?;
                }
                WifiAction::Disconnect => expect_ok(link, "DISCONNECT", "DISCONNECT")?,
            }
        }
        Ok(step)
    }

    /// Stop DHCP and invalidate any run in flight, so its late result is ignored.
    fn stop_dhcp(&mut self, runner: &mut impl CommandRunner) -> Result<(), WifiError> {
        self.lease = None;
        self.pending_dhcp = None;
        dhcp::stop(runner)?;
        Ok(())
    }

    /// Stock teardown (VA 0x00889f0c): `DISABLE_NETWORK all`, `TERMINATE`. The caller waits
    /// [`TERMINATE_WAIT_MS`] and closes both links. No module unload is observed.
    pub fn shut_down(
        &mut self,
        link: &mut impl SupplicantLink,
        runner: &mut impl CommandRunner,
        now_ms: u64,
    ) -> Result<(), WifiError> {
        expect_ok(link, "DISABLE_NETWORK", "DISABLE_NETWORK all")?;
        expect_ok(link, "TERMINATE", "TERMINATE")?;
        let reaction = self.association.on_event(WifiEvent::Terminating, now_ms);
        if reaction.actions.contains(&WifiAction::StopDhcp) {
            self.stop_dhcp(runner)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Output;
    use crate::fake::ScriptedRunner;
    use std::collections::VecDeque;

    /// Answers by command prefix, in order, and records every command sent.
    #[derive(Default)]
    struct ScriptedLink {
        replies: VecDeque<(&'static str, &'static str)>,
        sent: Vec<String>,
    }

    impl SupplicantLink for ScriptedLink {
        fn request(&mut self, command: &str) -> Result<String, LinkError> {
            self.sent.push(command.to_owned());
            match self.replies.pop_front() {
                Some((prefix, reply)) if command.starts_with(prefix) => Ok(reply.into()),
                _ => Err(LinkError::Transport(format!("unexpected {command}"))),
            }
        }
    }

    /// S8 on the QC: a busy supplicant refuses the first SCAN; the start still succeeds.
    #[test]
    fn a_refused_first_scan_does_not_fail_initialize() {
        let mut link = ScriptedLink::default();
        link.replies.extend([
            ("INTERFACES", "wlan0\n"),
            ("GET_CAPABILITY", "PEAP"),
            ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
            ("ENABLE_NETWORK all", "OK"),
            ("SAVE_CONFIG", "OK"),
            ("SCAN", "FAIL-BUSY"),
        ]);
        assert_eq!(initialize(&mut link), Ok(()));
        assert_eq!(link.sent.last().map(String::as_str), Some("SCAN"));
    }

    fn service() -> WifiService {
        WifiService::new(events::STOCK_ASSOCIATION_TIMEOUT_MS)
    }

    #[test]
    fn new_wpa2_network_is_added_hashed_then_dhcp_runs() {
        let mut wifi = service();
        let mut link = ScriptedLink::default();
        link.replies.extend([
            ("BSS 0", "bssid=02:00:00:00:00:01\nfreq=2412\nlevel=-50\nflags=[WPA2-PSK-CCMP][ESS]\nssid=IEEE\n"),
            ("BSS 1", ""),
            ("DISABLE_NETWORK all", "OK"),
            ("LIST_NETWORKS", "network id / ssid / bssid / flags\n0\tHome\tany\t[DISABLED]\n"),
            ("ADD_NETWORK", "1\n"),
            ("SET_NETWORK 1 ssid", "OK"),
            ("SET_NETWORK 1 auth_alg", "OK"),
            ("SET_NETWORK 1 proto", "OK"),
            ("SET_NETWORK 1 key_mgmt", "OK"),
            ("SET_NETWORK 1 pairwise", "OK"),
            ("SET_NETWORK 1 group", "OK"),
            ("SET_NETWORK 1 psk", "OK"),
            ("SET_NETWORK 0 priority 0", "OK"),
            ("SET_NETWORK 1 priority 1", "OK"),
            ("ENABLE_NETWORK 1", "OK"),
            ("SAVE_CONFIG", "OK"),
        ]);
        wifi.read_scan(&mut link).unwrap();
        assert_eq!(wifi.connect(&mut link, "IEEE", Some("password"), 0), Ok(1));
        assert!(
            link.sent
                .contains(&"SET_NETWORK 1 ssid 49454545".to_owned())
        );
        assert!(link.sent.contains(
            &"SET_NETWORK 1 psk f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e".to_owned()
        ));
        assert!(!link.sent.iter().any(|c| c.contains("password")));

        link.replies.extend([
            (
                "LIST_NETWORKS",
                "network id / ssid / bssid / flags\n0\tHome\tany\t\n1\tIEEE\tany\t[CURRENT]\n",
            ),
            ("DISABLE_NETWORK 0", "OK"),
            ("SAVE_CONFIG", "OK"),
            ("DISCONNECT", "OK"),
        ]);
        let mut runner = ScriptedRunner::default();
        for _ in 0..4 {
            runner.expect("udhcpc --now --quit -i wlan0", Ok(Output::code(1)));
        }
        let step = wifi
            .on_event(&mut link, &mut runner, "<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:01 completed [id=1 id_str=]", 5)
            .unwrap();
        assert!(step.notices.contains(&WifiNotice::Association(true)));
        // The event path only asks for DHCP; the caller runs it on a worker.
        let mut next = step.dhcp;
        let mut dhcp_notices = Vec::new();
        while let Some(run) = next {
            let result = dhcp::acquire(&mut runner);
            let step = wifi
                .dhcp_finished(&mut link, &mut runner, run, result)
                .unwrap();
            dhcp_notices.extend(step.notices);
            next = step.dhcp;
        }
        assert_eq!(dhcp_notices, [WifiNotice::Dhcp(false)]);
        assert_eq!(link.sent.last().map(String::as_str), Some("DISCONNECT"));
    }

    #[test]
    fn unsupported_and_unscanned_networks_are_refused_before_adding() {
        let mut wifi = service();
        let mut link = ScriptedLink::default();
        link.replies.extend([
            (
                "BSS 0",
                "bssid=02:00:00:00:00:01\nlevel=-50\nflags=[WPA2-EAP-CCMP][ESS]\nssid=Corp\n",
            ),
            ("BSS 1", "FAIL"),
            ("DISABLE_NETWORK all", "OK"),
            ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
            ("DISABLE_NETWORK all", "OK"),
            ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
        ]);
        wifi.read_scan(&mut link).unwrap();
        assert_eq!(
            wifi.connect(&mut link, "Corp", None, 0),
            Err(WifiError::Unsupported(Security::Wpa2Eap))
        );
        assert_eq!(
            wifi.connect(&mut link, "Elsewhere", Some("password"), 0),
            Err(WifiError::NotInScanResults)
        );
        assert!(!link.sent.iter().any(|c| c.starts_with("ADD_NETWORK")));
    }

    #[test]
    fn temp_disabled_new_network_is_removed_so_corrected_password_is_added() {
        let mut wifi = service();
        let mut link = ScriptedLink::default();
        link.replies.extend([
            ("BSS 0", "bssid=02:00:00:00:00:01\nfreq=2412\nlevel=-50\nflags=[WPA2-PSK-CCMP][ESS]\nssid=IEEE\n"),
            ("BSS 1", ""),
            ("DISABLE_NETWORK all", "OK"),
            ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
            ("ADD_NETWORK", "1\n"),
            ("SET_NETWORK 1 ssid", "OK"),
            ("SET_NETWORK 1 auth_alg", "OK"),
            ("SET_NETWORK 1 proto", "OK"),
            ("SET_NETWORK 1 key_mgmt", "OK"),
            ("SET_NETWORK 1 pairwise", "OK"),
            ("SET_NETWORK 1 group", "OK"),
            ("SET_NETWORK 1 psk", "OK"),
            ("SET_NETWORK 1 priority 1", "OK"),
            ("ENABLE_NETWORK 1", "OK"),
            ("SAVE_CONFIG", "OK"),
        ]);
        wifi.read_scan(&mut link).unwrap();
        assert_eq!(wifi.connect(&mut link, "IEEE", Some("wrongpass"), 0), Ok(1));

        link.replies.extend([
            ("DISABLE_NETWORK 1", "OK"),
            ("SAVE_CONFIG", "OK"),
            ("REMOVE_NETWORK 1", "OK"),
            ("SAVE_CONFIG", "OK"),
        ]);
        let step = wifi
            .on_event(
                &mut link,
                &mut ScriptedRunner::default(),
                "<3>CTRL-EVENT-SSID-TEMP-DISABLED id=1 ssid=\"IEEE\" auth_failures=1 duration=10 reason=CONN_FAILED",
                1,
            )
            .unwrap();
        assert!(step.notices.contains(&WifiNotice::Rejected));
        assert_eq!(wifi.association().status(), WifiStatus::Disconnected);
        assert!(link.sent.contains(&"REMOVE_NETWORK 1".to_owned()));

        link.replies.extend([
            ("DISABLE_NETWORK all", "OK"),
            ("LIST_NETWORKS", "network id / ssid / bssid / flags\n"),
            ("ADD_NETWORK", "1\n"),
            ("SET_NETWORK 1 ssid", "OK"),
            ("SET_NETWORK 1 auth_alg", "OK"),
            ("SET_NETWORK 1 proto", "OK"),
            ("SET_NETWORK 1 key_mgmt", "OK"),
            ("SET_NETWORK 1 pairwise", "OK"),
            ("SET_NETWORK 1 group", "OK"),
            ("SET_NETWORK 1 psk", "OK"),
            ("SET_NETWORK 1 priority 1", "OK"),
            ("ENABLE_NETWORK 1", "OK"),
            ("SAVE_CONFIG", "OK"),
        ]);
        assert_eq!(wifi.connect(&mut link, "IEEE", Some("rightpass"), 2), Ok(1));
        let corrected = psk::Psk::derive("rightpass", b"IEEE").unwrap();
        assert!(
            link.sent
                .contains(&format!("SET_NETWORK 1 psk {}", corrected.hex()))
        );
    }
}
