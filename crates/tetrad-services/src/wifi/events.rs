//! Supplicant monitor events and the stock association state machine (worker event handler VA
//! 0x0088ecb0, status setter 0x00888928, timeouts 0x0088eb14 and 0x0088b69c, DHCP result
//! 0x0088b9f4).

/// Stock Wi-Fi status codes (name table VA 0x008886a8). Not IP or internet readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiStatus {
    Fatal = 0,
    Unknown = 1,
    NotRunning = 2,
    Obscure = 3,
    Running = 4,
    Disabled = 5,
    Authenticating = 6,
    Associating = 7,
    Associated = 8,
    FourWayHandshake = 9,
    GroupHandshake = 10,
    WaitForRegistrar = 11,
    Inactive = 12,
    WpsRunning = 13,
    Scanning = 14,
    Scanned = 15,
    Disconnected = 16,
    LostSignal = 17,
    Rejected = 18,
    TimedOut = 19,
    Completed = 20,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiEvent {
    ScanStarted,
    ScanResults,
    Disconnected {
        reason: Option<u16>,
    },
    Authenticating,
    Associating,
    Associated,
    Connected {
        network: Option<u32>,
    },
    Terminating,
    WpsTimeout,
    AssocReject,
    TempDisabled {
        network: Option<u32>,
        wrong_key: bool,
    },
}

/// Decode one monitor datagram. Strips `IFNAME=<if> ` and `<priority>`; credential requests
/// (`CTRL-REQ-`) and unknown events give `None`. Payloads are never kept.
pub fn parse_event(datagram: &str) -> Option<WifiEvent> {
    let text = datagram
        .strip_prefix("IFNAME=")
        .and_then(|rest| rest.split_once(' ').map(|(_, tail)| tail))
        .unwrap_or(datagram);
    let text = match text.strip_prefix('<') {
        Some(rest) => rest.split_once('>')?.1,
        None => text,
    };
    Some(if text.starts_with("CTRL-EVENT-SCAN-STARTED") {
        WifiEvent::ScanStarted
    } else if text.starts_with("CTRL-EVENT-SCAN-RESULTS") {
        WifiEvent::ScanResults
    } else if let Some(rest) = text.strip_prefix("CTRL-EVENT-DISCONNECTED") {
        WifiEvent::Disconnected {
            reason: field(rest, "reason="),
        }
    } else if text.starts_with("SME: Trying to authenticate") {
        WifiEvent::Authenticating
    } else if text.starts_with("Trying to associate") {
        WifiEvent::Associating
    } else if text.starts_with("Associated with") {
        WifiEvent::Associated
    } else if let Some(rest) = text.strip_prefix("CTRL-EVENT-CONNECTED") {
        // `- Connection to <bssid> completed [id=N id_str=…]`; id_str is opaque.
        WifiEvent::Connected {
            network: rest
                .split_once("[id=")
                .and_then(|(_, tail)| field(tail, "")),
        }
    } else if text.starts_with("CTRL-EVENT-TERMINATING") {
        WifiEvent::Terminating
    } else if text.starts_with("WPS-TIMEOUT") {
        WifiEvent::WpsTimeout
    } else if text.starts_with("CTRL-EVENT-ASSOC-REJECT") {
        WifiEvent::AssocReject
    } else if let Some(rest) = text.strip_prefix("CTRL-EVENT-SSID-TEMP-DISABLED") {
        // `id=N ssid="…" auth_failures=N duration=N reason=R`. The reason is the last field, so
        // searching from the end keeps SSID text from posing as one.
        WifiEvent::TempDisabled {
            network: field(rest, "id="),
            wrong_key: rest
                .rsplit_once(" reason=")
                .is_some_and(|(_, reason)| reason.trim() == "WRONG_KEY"),
        }
    } else {
        return None;
    })
}

/// First `key=value` token, parsed. A trailing `]` (connected id) is ignored.
fn field<T: std::str::FromStr>(tokens: &str, key: &str) -> Option<T> {
    tokens
        .split_whitespace()
        .find_map(|t| t.strip_prefix(key))
        .and_then(|v| v.trim_end_matches(']').parse().ok())
}

/// Effects the state machine asks the executor to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiAction {
    /// `udhcpc --now --quit -i wlan0`.
    StartDhcp,
    /// `killall udhcpc`.
    StopDhcp,
    /// `ifconfig wlan0 down`, then `up`.
    ResetInterface,
    /// `DISABLE_NETWORK <id>` then `SAVE_CONFIG`.
    DisableNetwork(u32),
    /// `REMOVE_NETWORK <id>` then `SAVE_CONFIG`: a network added by this attempt timed out.
    RemoveNetwork(u32),
    /// `DISABLE_NETWORK` every other saved network (VA 0x0088a718).
    DisableOthers(u32),
    /// `DISCONNECT`.
    Disconnect,
}

/// Facts for the UI and the internet manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiNotice {
    Status(WifiStatus),
    /// Association true or false. True is not an IP lease.
    Association(bool),
    AuthenticationFailed,
    Rejected,
    /// DHCP finished with this result (stock signal at VA 0x0088b9f4).
    Dhcp(bool),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub actions: Vec<WifiAction>,
    pub notices: Vec<WifiNotice>,
}

/// Stock DHCP retry rule (VA 0x0088b9f4): retry while the failure counter is ≤ 2, then
/// `DISCONNECT`. Checking before incrementing gives the initial attempt plus three retries
/// (ordering inferred).
pub const DHCP_MAX_RETRIES: u8 = 2;
/// Connecting timer (+0xac, started on CONNECT, stopped on Associating) and associating timer
/// (+0xc4, started on Associating, restarted on ASSOC-REJECT). Both single-shot 30 s (VAs
/// 0x00891668, 0x008913a0).
pub const STOCK_ASSOCIATION_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Attempt {
    network: u32,
    /// The network was added for this attempt; a timeout removes it again.
    added: bool,
    connect_deadline_ms: Option<u64>,
    associating_deadline_ms: Option<u64>,
}

/// Association and DHCP sequencing for one interface. Times are injected milliseconds.
#[derive(Debug, Clone)]
pub struct Association {
    status: WifiStatus,
    attempt: Option<Attempt>,
    associated: bool,
    dhcp_failures: u8,
    timeout_ms: u64,
}

impl Association {
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            status: WifiStatus::Unknown,
            attempt: None,
            associated: false,
            dhcp_failures: 0,
            timeout_ms,
        }
    }

    pub fn status(&self) -> WifiStatus {
        self.status
    }

    pub fn is_associated(&self) -> bool {
        self.associated
    }

    /// The executor has enabled `network`; wait for its Connected event.
    pub fn begin(&mut self, network: u32, added: bool, now_ms: u64) {
        self.attempt = Some(Attempt {
            network,
            added,
            connect_deadline_ms: Some(now_ms.saturating_add(self.timeout_ms)),
            associating_deadline_ms: None,
        });
    }

    fn arm_associating_timer(&mut self, now_ms: u64) -> bool {
        let deadline = now_ms.saturating_add(self.timeout_ms);
        self.attempt
            .as_mut()
            .map(|a| a.associating_deadline_ms = Some(deadline))
            .is_some()
    }

    fn set_status(&mut self, status: WifiStatus, reaction: &mut Reaction) {
        if self.status != status {
            self.status = status;
            reaction.notices.push(WifiNotice::Status(status));
        }
    }

    fn lose_association(&mut self, reaction: &mut Reaction) {
        if self.associated {
            self.associated = false;
            reaction.actions.push(WifiAction::StopDhcp);
            reaction.notices.push(WifiNotice::Association(false));
        }
    }

    pub fn on_event(&mut self, event: WifiEvent, now_ms: u64) -> Reaction {
        let mut reaction = self.on_tick(now_ms);
        let r = &mut reaction;
        match event {
            WifiEvent::ScanStarted => self.set_status(WifiStatus::Scanning, r),
            WifiEvent::ScanResults => self.set_status(WifiStatus::Scanned, r),
            WifiEvent::Authenticating => self.set_status(WifiStatus::Authenticating, r),
            WifiEvent::Associating => {
                if let Some(attempt) = self.attempt.as_mut() {
                    attempt.connect_deadline_ms = None;
                }
                self.arm_associating_timer(now_ms);
                self.set_status(WifiStatus::Associating, r);
            }
            WifiEvent::Associated => {
                if let Some(attempt) = self.attempt.as_mut() {
                    attempt.connect_deadline_ms = None;
                    attempt.associating_deadline_ms = None;
                }
                self.set_status(WifiStatus::Associated, r);
            }
            WifiEvent::WpsTimeout => self.set_status(WifiStatus::TimedOut, r),
            // During an attempt a disconnect is only an observation: switching networks makes
            // the supplicant drop the old one first. It stops the associating timer only.
            WifiEvent::Disconnected { .. } => {
                if let Some(attempt) = self.attempt.as_mut() {
                    attempt.associating_deadline_ms = None;
                }
                self.set_status(WifiStatus::Disconnected, r);
                self.lose_association(r);
            }
            WifiEvent::Connected { network } => {
                let expected = self.attempt.map(|a| a.network);
                if expected.is_some() && network != expected {
                    return reaction;
                }
                self.attempt = None;
                self.set_status(WifiStatus::Completed, r);
                if let Some(id) = network {
                    r.actions.push(WifiAction::DisableOthers(id));
                }
                if !self.associated {
                    self.associated = true;
                    self.dhcp_failures = 0;
                    r.notices.push(WifiNotice::Association(true));
                    r.actions.push(WifiAction::StartDhcp);
                }
            }
            WifiEvent::Terminating => {
                self.attempt = None;
                self.set_status(WifiStatus::NotRunning, r);
                self.lose_association(r);
            }
            // Stock resets the interface and restarts the associating timer.
            WifiEvent::AssocReject => {
                if self.arm_associating_timer(now_ms) {
                    r.actions.push(WifiAction::ResetInterface);
                }
            }
            WifiEvent::TempDisabled {
                wrong_key: true, ..
            } => {
                self.attempt = None;
                r.notices.push(WifiNotice::AuthenticationFailed);
                self.set_status(WifiStatus::Disconnected, r);
            }
            // Stock also removes a network this attempt added (0x0088b484), then goes Disconnected.
            WifiEvent::TempDisabled { network, .. } => {
                let attempt = self.attempt.take();
                if let Some(id) = network.or(attempt.map(|a| a.network)) {
                    r.actions.push(WifiAction::DisableNetwork(id));
                    if attempt.is_some_and(|a| a.added && a.network == id) {
                        r.actions.push(WifiAction::RemoveNetwork(id));
                    }
                }
                r.notices.push(WifiNotice::Rejected);
                self.set_status(WifiStatus::Disconnected, r);
            }
        }
        reaction
    }

    /// Expire the attempt (VAs 0x0088eb14, 0x0088b69c): status Rejected, disable the network,
    /// remove it if this attempt added it. Only the connecting timer resets the interface.
    pub fn on_tick(&mut self, now_ms: u64) -> Reaction {
        let mut reaction = Reaction::default();
        let Some(attempt) = self.attempt else {
            return reaction;
        };
        let connect_expired = attempt.connect_deadline_ms.is_some_and(|d| now_ms >= d);
        let associating_expired = attempt.associating_deadline_ms.is_some_and(|d| now_ms >= d);
        if !connect_expired && !associating_expired {
            return reaction;
        }
        self.attempt = None;
        self.set_status(WifiStatus::Rejected, &mut reaction);
        reaction.notices.push(WifiNotice::Rejected);
        reaction
            .actions
            .push(WifiAction::DisableNetwork(attempt.network));
        if attempt.added {
            reaction
                .actions
                .push(WifiAction::RemoveNetwork(attempt.network));
        }
        if connect_expired {
            reaction.actions.push(WifiAction::ResetInterface);
        }
        reaction
    }

    /// Feed the result of a [`WifiAction::StartDhcp`]. Stock reports `Dhcp` on success or after
    /// the final failure only; earlier failures retry silently.
    pub fn on_dhcp(&mut self, ok: bool) -> Reaction {
        let mut reaction = Reaction::default();
        if !self.associated {
            return reaction;
        }
        if ok {
            self.dhcp_failures = 0;
            reaction.notices.push(WifiNotice::Dhcp(true));
        } else if self.dhcp_failures <= DHCP_MAX_RETRIES {
            self.dhcp_failures += 1;
            reaction.actions.push(WifiAction::StartDhcp);
        } else {
            reaction.notices.push(WifiNotice::Dhcp(false));
            reaction.actions.push(WifiAction::Disconnect);
        }
        reaction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stock_event_shapes() {
        assert_eq!(
            parse_event(
                "<3>CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:00:01 completed [id=4 id_str=]"
            ),
            Some(WifiEvent::Connected { network: Some(4) })
        );
        assert_eq!(
            parse_event(
                "IFNAME=wlan0 <3>CTRL-EVENT-DISCONNECTED bssid=02:00:00:00:00:01 reason=3 locally_generated=1"
            ),
            Some(WifiEvent::Disconnected { reason: Some(3) })
        );
        assert_eq!(
            parse_event(
                r#"<3>CTRL-EVENT-SSID-TEMP-DISABLED id=1 ssid="a reason=WRONG_KEY" auth_failures=1 duration=10 reason=CONN_FAILED"#
            ),
            Some(WifiEvent::TempDisabled {
                network: Some(1),
                wrong_key: false
            })
        );
        assert_eq!(
            parse_event("<3>CTRL-REQ-PASSWORD-1:Password needed for SSID x"),
            None
        );
    }

    #[test]
    fn connect_then_dhcp_retries_three_times_then_disconnects() {
        let mut assoc = Association::new(STOCK_ASSOCIATION_TIMEOUT_MS);
        assoc.begin(2, false, 0);
        // A disconnect from the old network does not cancel the attempt.
        let r = assoc.on_event(WifiEvent::Disconnected { reason: Some(3) }, 10);
        assert!(r.actions.is_empty());
        // Another network's Connected is ignored.
        assert_eq!(
            assoc.on_event(WifiEvent::Connected { network: Some(1) }, 20),
            Reaction::default()
        );
        let r = assoc.on_event(WifiEvent::Connected { network: Some(2) }, 30);
        assert_eq!(
            r.actions,
            [WifiAction::DisableOthers(2), WifiAction::StartDhcp]
        );
        assert!(r.notices.contains(&WifiNotice::Association(true)));
        for _ in 0..3 {
            let r = assoc.on_dhcp(false);
            assert_eq!(r.actions, [WifiAction::StartDhcp]);
            assert!(r.notices.is_empty());
        }
        let r = assoc.on_dhcp(false);
        assert_eq!(r.actions, [WifiAction::Disconnect]);
        assert_eq!(r.notices, [WifiNotice::Dhcp(false)]);
    }

    #[test]
    fn associating_timer_rejects_without_reset_after_connecting_timer_stops() {
        let mut assoc = Association::new(1_000);
        assoc.begin(7, true, 0);
        assert!(
            assoc
                .on_event(WifiEvent::Associating, 500)
                .actions
                .is_empty()
        );
        // Associating stopped the connecting timer, so its 1000 ms deadline passes quietly.
        assert_eq!(assoc.on_tick(1_000), Reaction::default());
        let r = assoc.on_event(WifiEvent::AssocReject, 900);
        assert_eq!(r.actions, [WifiAction::ResetInterface]);
        assert_eq!(assoc.on_tick(1_899), Reaction::default());
        let r = assoc.on_tick(1_900);
        assert_eq!(
            r.actions,
            [WifiAction::DisableNetwork(7), WifiAction::RemoveNetwork(7)]
        );
        assert_eq!(assoc.status(), WifiStatus::Rejected);
    }

    #[test]
    fn connecting_timeout_rejects_and_resets_interface() {
        let mut assoc = Association::new(1_000);
        assoc.begin(7, true, 0);
        assert_eq!(assoc.on_tick(999), Reaction::default());
        let r = assoc.on_tick(1_000);
        assert_eq!(
            r.actions,
            [
                WifiAction::DisableNetwork(7),
                WifiAction::RemoveNetwork(7),
                WifiAction::ResetInterface
            ]
        );
        assert_eq!(assoc.status(), WifiStatus::Rejected);
    }

    #[test]
    fn associated_stops_both_timers() {
        let mut assoc = Association::new(1_000);
        assoc.begin(2, false, 0);
        assoc.on_event(WifiEvent::Associating, 100);
        assoc.on_event(WifiEvent::Associated, 200);
        assert_eq!(assoc.on_tick(60_000), Reaction::default());
        assert_eq!(assoc.status(), WifiStatus::Associated);
    }

    #[test]
    fn wrong_key_keeps_new_network_saved_and_ends_attempt() {
        let mut assoc = Association::new(1_000);
        assoc.begin(7, true, 0);
        let r = assoc.on_event(
            WifiEvent::TempDisabled {
                network: Some(7),
                wrong_key: true,
            },
            10,
        );
        assert!(r.actions.is_empty());
        assert_eq!(
            r.notices,
            [
                WifiNotice::AuthenticationFailed,
                WifiNotice::Status(WifiStatus::Disconnected)
            ]
        );
        assert_eq!(assoc.on_tick(60_000), Reaction::default());
    }
}
