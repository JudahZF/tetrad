//! Licences: the stored plugin licences and the activation step.

use super::*;

/// How long a signing batch waits for the DSP replies to `0x35` and `0x4f`, and a licence
/// activation for the reply to `0x50`. Stock has no ARM-side timeout (owned).
pub(super) const SIGNING_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Licence activation waits this long after a fresh pair (0x001ee714).
pub(super) const LICENCE_DELAY_AFTER_PAIR: Duration = Duration::from_secs(2);

/// The serial text that skips pairing (0x00307b04, 0x00a29d44).
pub(super) const UNSERIALIZED: &str = "undefined";

/// Length of the DSP licence answer `0x21` that stock keeps (0x001edd10).
pub(super) const LICENCE_DATA_BYTES: usize = 32;

/// Plugin ids in the licensed-plugin mask of channel `0x20`; bits 62 and 63 are flags
/// (`+0x38`, `+0x39`, 0x001f18c0).
pub(super) const LICENCE_PLUGIN_BITS: u32 = 62;

/// The plugin ids the licence manager knows (table `0xa17988..0xa179b8`, 0x001ed1d4,
/// 0x001ed414). A licensed id outside it is never licensed.
pub const LICENSABLE_PLUGINS: [u32; 12] = [3, 10, 1, 11, 4, 5, 9, 12, 13, 21, 15, 20];

/// What a plugin block may do: the result of 0x001ed368.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    /// 0: not licensed. Deploys bypassed (0x00521c84).
    Unlicensed,
    /// 1: licensed, but the `0x20` answer did not set bit 62 ("No available activations
    /// remaining", 0x004b5b34). Deploys bypassed.
    Inactive,
    /// 2: licensed and bit 62 set. Runs with its stored bypass.
    Active,
}

/// The licence manager state (0x001f18c0): `+0x3c` licensed plugin ids (`previous` in
/// `PluginLicenses.ini`) and `+0x38`, bit 62 of the last `0x20` answer. Bit 63 (`+0x39`) has
/// no recovered reader and is not kept. `Debug` shows a count only: the ids come from the
/// licence mask.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Licences {
    /// Bit n: plugin id n (0..62) is licensed.
    pub(super) licensed: u64,
    pub(super) active: bool,
}

impl std::fmt::Debug for Licences {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Licences")
            .field("licensed", &self.licensed.count_ones())
            .finish_non_exhaustive()
    }
}

impl Licences {
    /// Boot state (0x001f1050): the `previous` set of `PluginLicenses.ini`, both flags clear,
    /// so every plugin is [`PluginStatus::Inactive`] at most until the DSP answers.
    pub fn stored(settings: &DeviceSettings) -> Self {
        Self {
            licensed: (settings.settings().licenses.previous.iter())
                .filter(|&&id| (0..LICENCE_PLUGIN_BITS as i32).contains(&id))
                .fold(0, |mask, &id| mask | 1 << id),
            ..Self::default()
        }
    }

    /// 0x001ed1d4: `plugin` is a known plugin id in the licensed set.
    pub fn is_licensed(&self, plugin: u32) -> bool {
        LICENSABLE_PLUGINS.contains(&plugin) && self.licensed >> plugin & 1 == 1
    }

    /// Bit 62 of the last `0x20` answer: licensed plugins may run.
    pub fn active(&self) -> bool {
        self.active
    }

    pub(super) fn ids(self) -> impl Iterator<Item = u32> {
        (0..LICENCE_PLUGIN_BITS).filter(move |id| self.licensed >> id & 1 == 1)
    }

    /// 0x001ed368.
    pub fn status(&self, plugin: u32) -> PluginStatus {
        match (self.is_licensed(plugin), self.active) {
            (false, _) => PluginStatus::Unlicensed,
            (true, false) => PluginStatus::Inactive,
            (true, true) => PluginStatus::Active,
        }
    }
}

/// Stored licence data on its way to the DSP (`0x51`; busy flag `+0x6c`, 0x001ef600).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LicenceApply {
    pub(super) sent: Duration,
    /// Another apply arrived meanwhile; stock queues it (`+0x70`, 0x001f0680).
    pub(super) again: bool,
}

/// A licence activation in flight. Stock starts no second one (busy flag `+0x30`, 0x001f11d4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LicenceStep {
    /// Queued in a signing batch, alone or behind its pair.
    Signing,
    /// Queued at this time, [`LICENCE_DELAY_AFTER_PAIR`] after a fresh pair.
    Due(Duration),
    /// `0x50` went out at this time with the decoded `activationCode`; `0x21` answers it.
    Dsp(Duration),
}

impl Services {
    /// Boot (0x001ed7b8): the licence manager runs a signing batch, `0x35` and `0x4f` with the
    /// serial, before it applies the stored data (0x001f1050). S3 on the QC (2026-10-15): an
    /// apply with no `0x35` before it got an answer with no mask. The replies find no open
    /// batch and are dropped (zeroed).
    pub fn boot_licences(
        &mut self,
        files: &mut impl Files,
        settings: &DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let mut effects = match Serial::read(files) {
            Ok(serial) => vec![
                Effect::Dsp(DspCommand::Request(Request::SigningMaterial(serial.prefix))),
                Effect::Dsp(DspCommand::Request(Request::ClientId(serial.prefix))),
            ],
            Err(error) => vec![notice(format!("licences at boot: {error}"))],
        };
        effects.extend(self.apply_licences(settings, now));
        effects
    }

    /// Sends the stored licence data (`PluginLicenses.ini`) to the DSP: `0x51` with the bytes,
    /// `send(…, 1, 1000)` (0x001f0680 → 0x001ef600 → 0x001edad0 → 0x005155c4). Stock runs it at
    /// boot once the client id is in (0x001ed7b8 → 0x001f1050) and after new data is stored.
    /// No data: nothing to do. One in flight: this one queues behind it.
    pub fn apply_licences(&mut self, settings: &DeviceSettings, now: Duration) -> Vec<Effect> {
        let payload = &settings.settings().licenses.payload;
        if payload.is_empty() {
            return Vec::new();
        }
        if let Some(apply) = self.apply.as_mut() {
            apply.again = true;
            return Vec::new();
        }
        match OpaqueBody::try_from(payload.as_slice()) {
            Ok(body) => {
                self.apply = Some(LicenceApply {
                    sent: now,
                    again: false,
                });
                vec![Effect::Dsp(DspCommand::Request(Request::Blob51(body)))]
            }
            Err(_) => vec![notice(format!(
                "licence data not applied: {}",
                ServiceError::Unsupported("stored licence data longer than one DSP request (0x51)")
            ))],
        }
    }

    /// Factory reset licence clear (0x001f0c50): drops an activation or apply in flight and
    /// sends `0x51` empty. Stock sends it directly (0x005155c4), outside the apply busy flag, so
    /// its `0x20` answer changes nothing here (I). The caller clears `State::licences` and the
    /// DSP copy; the settings reset deletes `PluginLicenses.ini`. A queued activation leaves
    /// the signing batch too: stock clears the tokens at once (`0x2ff6e0` after `0x308250`),
    /// so its queued activation has no token (review r11 R3).
    ///
    /// The reset also drops the questions that would write into the library later (review r35
    /// G4, `drop_library_asks`); the reset removes the user data, so their notices are not
    /// kept.
    pub fn clear_licences(&mut self) -> Effect {
        self.drop_library_asks("factory reset");
        self.licence = None;
        self.apply = None;
        if let Some(batch) = self.batch.as_mut() {
            batch.jobs.retain(|job| !job.is_licence_call());
        }
        for pending in &mut self.cloud_queue {
            pending.calls.retain(|call| !call.is_licence_call());
        }
        Effect::Dsp(DspCommand::Request(Request::Blob51(OpaqueBody::default())))
    }

    /// Licence refresh, as Cortex Control License UPDATE starts it (0x00113094 → 0x001f0e80):
    /// online and signed in, one at a time (0x001f11d4). With `last_paired_version` below 3.0.0
    /// it pairs first (0x003083a8). Results arrive on later ticks as notices. The offline form
    /// of 0x001f0e80 (flag 0, caller 0x001f1034) is [`Self::apply_licences`].
    pub fn refresh_licences(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let io = lend(backends)?;
        if self.licence.is_some() {
            return Ok(Vec::new());
        }
        if self.online != Some(true) {
            return Err(ServiceError::NotReady("licence refresh needs the internet"));
        }
        if !self.signed_in(settings)? {
            return Err(ServiceError::NotReady(
                "licence refresh needs a signed-in account",
            ));
        }
        if settings.settings().paired_version.needs_pairing() {
            self.licence = Some(LicenceStep::Signing);
            return Ok(self.start_pair(io.files, PairFor::Licences, now));
        }
        Ok(self.start_licences(io.files, now))
    }

    pub(super) fn start_licences(&mut self, files: &mut impl Files, now: Duration) -> Vec<Effect> {
        match Serial::read(files) {
            Ok(serial) => {
                self.licence = Some(LicenceStep::Signing);
                self.queue_signed(serial, CloudCall::Licences, now)
            }
            Err(error) => self.licence_failed(error),
        }
    }

    /// 0x001f2714: the activation ends ("Failed to fetch license info"); nothing is stored.
    pub(super) fn licence_failed(&mut self, reason: impl Display) -> Vec<Effect> {
        self.licence = None;
        vec![notice(format!("Failed to fetch license info: {reason}"))]
    }

    pub(super) fn licence_timers(&mut self, files: &mut impl Files, now: Duration) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self
            .apply
            .take_if(|apply| now.saturating_sub(apply.sent) >= SIGNING_REPLY_TIMEOUT)
            .is_some()
        {
            let error = ServiceError::Unsupported("no DSP licence apply answer (0x20)");
            effects.push(notice(format!("licence data not applied: {error}")));
        }
        effects.extend(match self.licence {
            Some(LicenceStep::Due(at)) if now >= at => self.start_licences(files, now),
            Some(LicenceStep::Dsp(sent)) if now.saturating_sub(sent) >= SIGNING_REPLY_TIMEOUT => {
                self.licence_failed(ServiceError::Unsupported("no DSP licence answer (0x21)"))
            }
            _ => Vec::new(),
        });
        effects
    }
}
