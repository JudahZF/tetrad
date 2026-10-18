//! Wi-Fi, the link and the internet probe, and the Wi-Fi settings view.

use super::*;

impl Services {
    /// The link gained or lost association plus lease. Starts a probe job at once when up.
    pub fn set_link(
        &mut self,
        backends: &mut impl ServiceBackends,
        up: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        lend(backends)?;
        Ok(self.link_changed(up, now))
    }

    pub(super) fn link_changed(&mut self, up: bool, now: Duration) -> Vec<Effect> {
        let notices = self.internet.set_link(up, millis(now));
        let mut effects = self.internet_effects(&notices, now);
        effects.extend(self.internet_jobs(now));
        effects
    }

    pub(super) fn internet_jobs(&mut self, now: Duration) -> impl Iterator<Item = Effect> + use<> {
        self.internet
            .due(millis(now))
            .into_iter()
            .map(|work| Effect::Service(ServiceJob::Internet(work)))
    }

    /// The owned internet recovery ([`InternetManager::recovery_due`]) as Wi-Fi actions: a DHCP
    /// renewal, `REASSOCIATE`, then `REASSOCIATE` and the interface reset. The second
    /// `REASSOCIATE` ends a `DISCONNECT` that the stock DHCP rule sends after four failed runs
    /// (step 2 can cause one), so the supplicant connects again when the interface is up; it
    /// comes before the reset, because the supplicant refuses it while the interface is down.
    /// Never during a CorOS install or without a saved network. Each step is logged once, with
    /// the wlan0 counters when they can be read. No driver reload: stock never unloads
    /// `wilc-spi`, and a new supplicant would leave the attached monitor on the old socket.
    pub(super) fn recover_internet(
        &mut self,
        link: &mut impl SupplicantLink,
        runner: &mut impl CommandRunner,
        files: &mut impl Files,
        now: Duration,
    ) -> Vec<Effect> {
        if self.installing() || self.saved.is_empty() {
            return Vec::new();
        }
        let Some(Recovery { step, lost_for }) = self.internet.recovery_due(millis(now)) else {
            return Vec::new();
        };
        let actions = match step {
            RecoveryStep::RenewLease => vec![WifiAction::StartDhcp],
            RecoveryStep::Reassociate => vec![WifiAction::Reassociate],
            RecoveryStep::ResetInterface => {
                vec![WifiAction::Reassociate, WifiAction::ResetInterface]
            }
        };
        let counters = wifi::link_counters(files)
            .map(|counters| format!("; wlan0 {counters}"))
            .unwrap_or_default();
        let mut effects = vec![notice(format!(
            "internet lost for {} s: recovery {step}{counters}",
            lost_for.as_secs()
        ))];
        match self.wifi.recover(link, runner, actions) {
            Ok(wifi_step) => effects.extend(self.wifi_step(wifi_step, now)),
            Err(error) => effects.push(notice(format!("recovery {step} failed: {error}"))),
        }
        effects
    }

    /// Scan results are read when the supplicant reports them (stock `0x0088c000`); the saved
    /// list follows each scan and association change.
    pub(super) fn refresh_lists(
        &mut self,
        link: &mut impl wifi::SupplicantLink,
        notices: &[WifiNotice],
    ) -> Result<(), ServiceError> {
        let scanned = notices.contains(&WifiNotice::Status(LinkStatus::Scanned));
        if scanned {
            self.wifi.read_scan(link)?;
        }
        if scanned
            || notices
                .iter()
                .any(|notice| matches!(notice, WifiNotice::Association(_)))
        {
            self.saved = wifi::saved_networks(link)?;
        }
        Ok(())
    }

    pub(super) fn wifi_step(&mut self, step: Step, now: Duration) -> Vec<Effect> {
        let mut effects = Vec::new();
        for wifi_notice in &step.notices {
            match wifi_notice {
                WifiNotice::AuthenticationFailed => {
                    self.alert = Some(WifiAlert::AuthenticationFailed);
                }
                WifiNotice::Rejected => self.alert = Some(WifiAlert::ConnectionError),
                WifiNotice::Dhcp(false) => self.alert = Some(WifiAlert::NoIpAddress),
                WifiNotice::Association(true) | WifiNotice::Dhcp(true) => self.alert = None,
                _ => {}
            }
        }
        for wifi_notice in step.notices {
            let text = match wifi_notice {
                WifiNotice::Status(_) => continue,
                WifiNotice::Association(true) => "Wi-Fi associated".to_owned(),
                WifiNotice::Association(false) => {
                    effects.extend(self.link_changed(false, now));
                    "Wi-Fi association lost".to_owned()
                }
                WifiNotice::AuthenticationFailed => "Wi-Fi authentication failed".to_owned(),
                WifiNotice::Rejected => "Wi-Fi network rejected".to_owned(),
                WifiNotice::Dhcp(true) => {
                    let lease = self
                        .wifi
                        .lease()
                        .map(|l| {
                            let time = l
                                .lease_seconds
                                .map(|s| format!(", lease time {s} s"))
                                .unwrap_or_default();
                            format!("DHCP lease {}/{}{time}", l.address, l.prefix_len)
                        })
                        .unwrap_or_else(|| "DHCP lease".to_owned());
                    effects.push(notice(lease));
                    effects.extend(self.link_changed(true, now));
                    continue;
                }
                WifiNotice::Dhcp(false) => {
                    effects.extend(self.link_changed(false, now));
                    "DHCP failed".to_owned()
                }
                WifiNotice::RenewalFailed => {
                    "DHCP renewal failed; the association stays".to_owned()
                }
            };
            effects.push(notice(text));
        }
        effects.extend(step.dhcp.map(|run| Effect::Service(ServiceJob::Dhcp(run))));
        effects
    }

    pub(super) fn internet_effects(
        &mut self,
        notices: &[InternetNotice],
        now: Duration,
    ) -> Vec<Effect> {
        notices
            .iter()
            .filter_map(|internet_notice| {
                Some(notice(match internet_notice {
                    InternetNotice::Status(InternetStatus::Connected) => {
                        self.online = Some(true);
                        "internet reachable".to_owned()
                    }
                    InternetNotice::Status(InternetStatus::Disconnected) => {
                        self.online = Some(false);
                        "internet unreachable".to_owned()
                    }
                    InternetNotice::TimeSet(time) => {
                        self.wall = Some((time.unix_seconds, now));
                        "time set from NTP".to_owned()
                    }
                    InternetNotice::TimeFailed(error) => format!("NTP failed: {error}"),
                    // Stock starts an update check here (VA 0x004af488) unless downloading.
                    InternetNotice::FirstTimeSync => {
                        self.update_check_due = self.updater.state != UpdaterState::DownloadingData;
                        return None;
                    }
                }))
            })
            .collect()
    }

    /// Scan results arrive later as `CTRL-EVENT-SCAN-RESULTS`; then call [`Self::read_scan`].
    pub fn scan(&mut self, backends: &mut impl ServiceBackends) -> Result<bool, ServiceError> {
        Ok(self.wifi.scan(backends.supplicant())?)
    }

    pub fn read_scan(
        &mut self,
        backends: &mut impl ServiceBackends,
    ) -> Result<Vec<ScanEntry>, ServiceError> {
        Ok(self.wifi.read_scan(backends.supplicant())?.to_vec())
    }

    /// Join `ssid`; association follows as supplicant events. Never logs the passphrase.
    pub fn connect(
        &mut self,
        backends: &mut impl ServiceBackends,
        ssid: &str,
        passphrase: Option<&str>,
        now: Duration,
    ) -> Result<u32, ServiceError> {
        self.alert = None;
        self.joining = Some(ssid.to_owned());
        self.internet.stop_recovery();
        let link = backends.supplicant();
        let id = self.wifi.connect(link, ssid, passphrase, millis(now))?;
        self.saved = wifi::saved_networks(link)?;
        Ok(id)
    }

    pub fn saved_networks(
        &mut self,
        backends: &mut impl ServiceBackends,
    ) -> Result<Vec<SavedNetwork>, ServiceError> {
        Ok(wifi::saved_networks(backends.supplicant())?)
    }

    /// Reset step `RemoveWifiNetworks` (0x00895264). The factory reset queues its unpair
    /// first: while one waits for its DSP replies, the networks go when it ends (success or
    /// failure), so its requests still have the link. Owned order: stock posts
    /// `removeAllNetworks` in the same pass, behind the queued unpair (review-w6 F1).
    pub fn forget_all(&mut self, backends: &mut impl ServiceBackends) -> Result<(), ServiceError> {
        if self.unpairing > 0 {
            self.forget_after_unpair = true;
            return Ok(());
        }
        self.forget(backends, None)
    }

    /// Leave the joined network (stock cell tap `0x5d4834`).
    pub fn disconnect(&mut self, backends: &mut impl ServiceBackends) -> Result<(), ServiceError> {
        self.internet.stop_recovery();
        Ok(wifi::disconnect(backends.supplicant())?)
    }

    /// Forget one saved network, or all with `None`.
    pub fn forget(
        &mut self,
        backends: &mut impl ServiceBackends,
        id: Option<u32>,
    ) -> Result<(), ServiceError> {
        Ok(self.forget_on(backends.supplicant(), id)?)
    }

    pub(super) fn forget_on(
        &mut self,
        link: &mut impl SupplicantLink,
        id: Option<u32>,
    ) -> Result<(), WifiError> {
        self.internet.stop_recovery();
        wifi::forget(link, id)?;
        self.saved = wifi::saved_networks(link)?;
        Ok(())
    }

    /// The Connection page: link status, the last scan with saved and joined flags, and the
    /// lease. A joined network missing from the scan is listed with no signal (I).
    pub fn wifi_view(&self) -> WifiView {
        use tetrad_ui::WifiStatus as View;
        let association = self.wifi.association();
        // Stock title order (`0x5d3f70`): module failure; joined → "Internet Connected" or
        // "Network check..." ("Internet Connection Failed" after a failed check, `0x5d56ac`);
        // association in progress → "Connecting..." (`0x5d45e4`); no scan entries → "No network
        // found"; scanning → "Scanning..."; else "No Internet Connection". The joined test is
        // the association, not the supplicant state: a background scan moves that state off
        // COMPLETED while the link stays up (seen on the QC, 2026-10-16).
        let status = match association.status() {
            LinkStatus::Fatal => View::ModuleFailed,
            _ if association.is_associated() => match (self.wifi.lease(), self.online) {
                (Some(_), Some(true)) => View::InternetConnected,
                (Some(_), Some(false)) => View::ConnectionFailed,
                _ => View::NetworkCheck,
            },
            LinkStatus::Authenticating
            | LinkStatus::Associating
            | LinkStatus::Associated
            | LinkStatus::FourWayHandshake
            | LinkStatus::GroupHandshake => View::Connecting,
            _ if self.wifi.scan_entries().is_empty() => View::NoNetwork,
            LinkStatus::Scanning => View::Scanning,
            _ => View::NoInternet,
        };
        let joined = |ssid: &str| {
            association.is_associated()
                && self
                    .saved
                    .iter()
                    .any(|saved| saved.current && saved.ssid == ssid)
        };
        let saved = |ssid: &str| self.saved.iter().any(|saved| saved.ssid == ssid);
        let mut networks: Vec<WifiNetwork> = self
            .wifi
            .scan_entries()
            .iter()
            .map(|entry| WifiNetwork {
                ssid: entry.ssid.clone(),
                signal: entry.signal_bars(),
                // Stock lock and password rule (`0x5d2028`): auth code > 2, WPA only.
                secured: !matches!(entry.security, Security::Open | Security::Wep),
                connected: joined(&entry.ssid),
                saved: saved(&entry.ssid),
                bssid: entry.bssid.clone(),
                frequency_mhz: entry.frequency_mhz,
                rssi_dbm: entry.signal_dbm.clamp(-90, -30),
            })
            .collect();
        for current in self.saved.iter().filter(|network| joined(&network.ssid)) {
            if networks.iter().all(|network| network.ssid != current.ssid) {
                networks.push(WifiNetwork {
                    ssid: current.ssid.clone(),
                    signal: 0,
                    secured: true,
                    connected: true,
                    saved: true,
                    rssi_dbm: -90,
                    ..WifiNetwork::default()
                });
            }
        }
        // Stock lists the joined network first, the rest in scan order (`0x53d730`).
        networks.sort_by_key(|network| !network.connected);
        WifiView {
            status,
            networks,
            ip_address: self.wifi.lease().map(|lease| lease.address.to_string()),
            alert: self.alert,
            joining: self.joining.clone(),
        }
    }
}
