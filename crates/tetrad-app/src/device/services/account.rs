//! The account: tokens, sign-in and sign-out, pairing and the signing batch.

use super::*;

/// [`TokenStore`] over the account group of `AccountConfig.ini`. Changes reach the file on
/// the next settings flush.
pub struct SettingsTokens<'a>(pub &'a mut DeviceSettings);

pub(super) fn token_text(secret: &Secret) -> Result<tetrad_settings::Secret, TokenStoreError> {
    std::str::from_utf8(secret.expose())
        .map(tetrad_settings::Secret::new)
        .map_err(|_| TokenStoreError("token is not UTF-8".into()))
}

impl TokenStore for SettingsTokens<'_> {
    fn load(&mut self) -> Result<Option<Tokens>, TokenStoreError> {
        let saved = &self.0.settings().tokens;
        Ok(
            (!saved.access_token.is_empty() && !saved.refresh_token.is_empty()).then(|| Tokens {
                access_token: Secret::new(saved.access_token.expose()),
                refresh_token: Secret::new(saved.refresh_token.expose()),
            }),
        )
    }

    fn store(&mut self, tokens: &Tokens, username: Option<&str>) -> Result<(), TokenStoreError> {
        let saved = AccountTokens {
            access_token: token_text(&tokens.access_token)?,
            refresh_token: token_text(&tokens.refresh_token)?,
        };
        self.0.update(|s| {
            s.tokens = saved;
            if let Some(name) = username {
                s.account.username = name.to_owned();
            }
        });
        Ok(())
    }

    /// Stock logout blanks `username` and `ui_username` too (`0x002ff6e0`).
    fn clear(&mut self) -> Result<(), TokenStoreError> {
        self.0.update(|s| {
            s.tokens = AccountTokens::default();
            s.account = AccountIdentity::default();
        });
        Ok(())
    }

    fn username(&mut self) -> Result<Option<String>, TokenStoreError> {
        let name = &self.0.settings().account.username;
        Ok((!name.is_empty()).then(|| name.clone()))
    }
}

/// What follows an unpair, whatever its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterUnpair {
    /// Device logout (0x004190e0) and factory reset (0x004de79c), both through 0x00308250:
    /// `user/signout`, then the token wipe (callback 0x0030b3c4).
    SignOut,
    /// Cortex Control logout (0x003080b4 → callback 0x003070f4): the local logout only
    /// (0x002ff6e0), no `user/signout`.
    LocalLogout,
}

/// Cortex Control CloudLogin CREATE (`loginFromControl` 0x00307e18): the account, its tokens
/// and the request ID a failure answers. It rides in [`Effect`] inside a [`Handoff`].
pub struct ControlLogin {
    pub username: Option<String>,
    pub tokens: Tokens,
    pub request_id: Option<u64>,
}

/// Token changes from the worker into `AccountConfig.ini` ([`SettingsTokens`]), then a flush.
pub(super) fn save_tokens(settings: &mut DeviceSettings, tokens: TokenLog) -> Vec<Effect> {
    let mut effects = Vec::new();
    let store = &mut SettingsTokens(settings);
    for change in tokens.0 {
        let written = match &change {
            Some((tokens, username)) => store.store(tokens, username.as_deref()),
            None => store.clear(),
        };
        if let Err(error) = written {
            effects.push(notice(format!("account tokens not saved: {error}")));
        }
    }
    effects.extend(flush(settings));
    effects
}

/// 0x00238ce4 → 0x00105d94: GenericError CloudLoginFailed with the login request ID.
pub(super) fn control_login_failed(request_id: Option<u64>) -> Effect {
    let message = GenericErrorMessage {
        request_id,
        error_code: Some(GenericErrorCode::CloudLoginFailed.into()),
    };
    Effect::ControlNotify(Box::new(message.into()))
}

/// Who asked for the pair; it picks what happens after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PairFor {
    /// Sign-in success (0x0030cb1c → 0x0030afb8).
    SignIn,
    /// A Cortex Control login (0x00307e18 → 0x00307540), with the CloudLogin request ID a
    /// failure answers.
    Control(Option<u64>),
    /// Licence activation with `last_paired_version` below 3.0.0 (0x003083a8 → 0x001ee714).
    Licences,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PairFailure {
    /// No serial, or the text `undefined`: no request is sent (0x00307b04).
    #[error("no serial number")]
    Unserialized,
    #[error(transparent)]
    Failed(#[from] ServiceError),
}

/// One stock signing batch (0x004a0f30, 0x00485c78): the first queued job sends `0x35` and
/// `0x4f`; both replies sign every queued job once, then they are dropped.
pub(super) struct SigningBatch {
    pub(super) serial: String,
    pub(super) started: Duration,
    pub(super) jobs: Vec<CloudCall>,
    pub(super) material: Option<SigningMaterial>,
    pub(super) client_id: Option<ClientIdBytes>,
}

/// The device serial (`SERIAL_PATH`) and its DSP request payload.
pub(super) struct Serial {
    pub(super) text: String,
    pub(super) prefix: TextPrefix,
}

impl Serial {
    pub(super) fn read(files: &mut impl Files) -> Result<Self, ServiceError> {
        let raw = files
            .read(Path::new(SERIAL_PATH))?
            .ok_or(ServiceError::NotReady("device serial file is missing"))?;
        let serial = DeviceSerial::parse(&raw)
            .map_err(|_| ServiceError::NotReady("device serial file is malformed"))?;
        let prefix = TextPrefix::try_from(serial.as_str().as_bytes())
            .map_err(|_| ServiceError::NotReady("device serial is too long"))?;
        Ok(Self {
            text: serial.as_str().to_owned(),
            prefix,
        })
    }
}

impl Services {
    /// `user/signin`, then the signed pair (0x0030cb1c). Both run on the service worker.
    pub fn sign_in(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        username: &str,
        password: &str,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        lend(backends)?;
        let call = CloudCall::SignIn {
            username: username.to_owned(),
            password: Secret::new(password),
        };
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// Cortex Control CloudLogin CREATE (`loginFromControl` 0x00307e18): the tokens on the
    /// worker, then the signed pair and the profile. A failure answers GenericError
    /// CloudLoginFailed with the login request ID (0x00238ce4).
    pub fn login_from_control(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        login: ControlLogin,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        lend(backends)?;
        let ControlLogin {
            username,
            tokens,
            request_id,
        } = login;
        let call = CloudCall::ControlLogin {
            username,
            tokens,
            request_id,
        };
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// SystemTimeSync (0x001ba1b0): the Control clock, set on the internet worker while no sync
    /// has succeeded since start. Its result is an NTP result (0x004af488).
    pub fn set_clock_if_unset(&mut self, ms_since_epoch: u64) -> Option<Effect> {
        let work = self.internet.set_clock_if_unset(ms_since_epoch)?;
        Some(Effect::Service(ServiceJob::Internet(work)))
    }

    /// The signed unpair, then the logout `after` names whatever the unpair result (0x00308250,
    /// 0x003080b4). Without a serial the unpair fails at once and only the logout runs.
    pub fn unpair(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        after: AfterUnpair,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let io = lend(backends)?;
        self.load_cloud(settings)?;
        self.unpairing += 1;
        Ok(match Serial::read(io.files) {
            Ok(serial) if serial.text != UNSERIALIZED => {
                self.queue_signed(serial, CloudCall::Unpair(after), now)
            }
            _ => self.logout_after(settings, after, PairFailure::Unserialized, now),
        })
    }

    /// An unpair that never went out: its failure, then the logout alone.
    pub(super) fn logout_after(
        &mut self,
        settings: &mut DeviceSettings,
        after: AfterUnpair,
        failure: PairFailure,
        now: Duration,
    ) -> Vec<Effect> {
        let mut effects = vec![notice(format!(
            "(Un)pairHardware() failed when logging out: {failure}"
        ))];
        effects.extend(self.cloud_job(settings, vec![CloudCall::Logout(after)], None, now));
        effects
    }

    /// One unpair ended (0x0030b3c4, 0x003070f4). The networks a factory reset left for this
    /// point go once no unpair is left.
    pub(super) fn unpair_finished(&mut self, link: &mut impl SupplicantLink) -> Vec<Effect> {
        self.unpairing = self.unpairing.saturating_sub(1);
        if self.unpairing > 0 || !std::mem::take(&mut self.forget_after_unpair) {
            return Vec::new();
        }
        match self.forget_on(link, None) {
            Ok(()) => Vec::new(),
            Err(error) => vec![notice(format!("removing Wi-Fi networks: {error}"))],
        }
    }

    /// Adds `job` to the open batch, or opens one with the `0x35` and `0x4f` requests. A batch
    /// holds one update check: the newest request replaces a queued one (owned).
    pub(super) fn queue_signed(
        &mut self,
        serial: Serial,
        job: CloudCall,
        now: Duration,
    ) -> Vec<Effect> {
        if let Some(batch) = self.batch.as_mut() {
            if matches!(job, CloudCall::UpdateCheck { .. }) {
                batch
                    .jobs
                    .retain(|queued| !matches!(queued, CloudCall::UpdateCheck { .. }));
            }
            batch.jobs.push(job);
            return Vec::new();
        }
        self.batch = Some(SigningBatch {
            serial: serial.text,
            started: now,
            jobs: vec![job],
            material: None,
            client_id: None,
        });
        vec![
            Effect::Dsp(DspCommand::Request(Request::SigningMaterial(serial.prefix))),
            Effect::Dsp(DspCommand::Request(Request::ClientId(serial.prefix))),
        ]
    }

    /// Fails every job of a batch whose DSP replies did not arrive in time. Notices, not an
    /// error, so the other effects of the same tick survive.
    pub(super) fn expire_batch(
        &mut self,
        files: &mut impl Files,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(batch) = self
            .batch
            .take_if(|batch| now.saturating_sub(batch.started) >= SIGNING_REPLY_TIMEOUT)
        else {
            return Vec::new();
        };
        let no_reply = || ServiceError::Unsupported("no DSP signing reply");
        let mut effects = Vec::new();
        for job in batch.jobs {
            effects.extend(match job {
                CloudCall::UpdateCheck { .. } => self.checked(Err(ServiceError::Unsupported(
                    "signed update check: no DSP signing reply",
                ))),
                CloudCall::Pair(reason) => {
                    self.after_pair(files, reason, Err(no_reply().into()), now)
                }
                CloudCall::Unpair(after) => {
                    self.logout_after(settings, after, no_reply().into(), now)
                }
                CloudCall::Licences => self.licence_failed(no_reply()),
                other => unreachable!("{other:?} is not a signed request"),
            });
        }
        effects
    }

    /// Sends every queued job with the batch's signer once both replies are in. The signer
    /// travels with the job and is dropped at its end.
    pub(super) fn finish_batch(
        &mut self,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(SigningBatch {
            serial,
            jobs,
            material: Some(material),
            client_id: Some(client_id),
            ..
        }) = self
            .batch
            .take_if(|batch| batch.material.is_some() && batch.client_id.is_some())
        else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        if !self.installing()
            && jobs
                .iter()
                .any(|job| matches!(job, CloudCall::UpdateCheck { .. }))
        {
            effects.extend(self.report(UpdaterState::RequestingData));
        }
        let signer = SharcSigner {
            material,
            client_id,
        };
        effects.extend(self.cloud_job(settings, jobs, Some((serial, signer)), now));
        effects
    }

    /// Pair, unless the serial is missing or `undefined` (0x00307b04): then it fails at once.
    pub(super) fn start_pair(
        &mut self,
        files: &mut impl Files,
        reason: PairFor,
        now: Duration,
    ) -> Vec<Effect> {
        match Serial::read(files) {
            Ok(serial) if serial.text != UNSERIALIZED => {
                self.queue_signed(serial, CloudCall::Pair(reason), now)
            }
            _ => self.after_pair(files, reason, Err(PairFailure::Unserialized), now),
        }
    }

    pub(super) fn after_pair(
        &mut self,
        files: &mut impl Files,
        reason: PairFor,
        paired: Result<Vec<Effect>, PairFailure>,
        now: Duration,
    ) -> Vec<Effect> {
        match (reason, paired) {
            (PairFor::SignIn, Ok(effects)) => effects,
            // 0x0030afb8: the session drops its tokens (0x0044d590); AccountConfig keeps them.
            (PairFor::SignIn, Err(failure)) => {
                if let Some(api) = self.cloud.as_mut() {
                    api.forget_tokens();
                }
                let text = match &failure {
                    PairFailure::Unserialized => "Unit is unserialized.".to_owned(),
                    PairFailure::Failed(error) => format!("Network error? ({error})"),
                };
                vec![notice(format!(
                    "Associating hardware with the account failed. {text}"
                ))]
            }
            (PairFor::Control(_), Ok(effects)) => effects,
            // 0x00307540: the session drops its tokens (0x0044d590), then the login fails.
            (PairFor::Control(request_id), Err(failure)) => {
                if let Some(api) = self.cloud.as_mut() {
                    api.forget_tokens();
                }
                vec![
                    notice(format!(
                        "Failed to pair hardware after logging in from Control: {failure}"
                    )),
                    control_login_failed(request_id),
                ]
            }
            (PairFor::Licences, Ok(effects)) => {
                self.licence = Some(LicenceStep::Due(now + LICENCE_DELAY_AFTER_PAIR));
                effects
            }
            // 0x001ee714: activation goes on at once after a failed pair.
            (PairFor::Licences, Err(failure)) => {
                let mut effects = vec![notice(format!("Pair hardware failed: {failure}"))];
                effects.extend(self.start_licences(files, now));
                effects
            }
        }
    }
}
