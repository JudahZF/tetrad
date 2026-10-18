//! Directory uploads: presets with their capture walk, and user captures.

use super::*;

/// re-r44 §1 (C): stock 4.1.0 has no IR upload. `0x708a8c` serves presets only, the IR cloud
/// folder's add slots return 2 (`0x704c94`, `0x704c64`), and the library add (`0x1e8a70`), the
/// batch (`0x848b60`) and Control 46 (`0xf3060`) send nothing for an IR.
pub const NO_IR_UPLOAD: &str = "Cortex Cloud has no IR upload in CorOS 4.1.0";

impl Services {
    /// The checks of a new preset upload of `path`, before the device builds it (review r31
    /// D3: `0x1db6ac` checks the transfer state first). `Ok(false)`: the same preset is out
    /// (transfer state 4, `0x7db480`), so nothing happens. Another preset out is refused (O:
    /// one at a time). An upload that still waits for an answer (the overwrite dialog or the
    /// walk's capture question) does not count: the new upload replaces it with no further
    /// request and no write when it starts ([`Self::upload_preset`]; its dialog may be gone,
    /// review r31 D1). A refused start leaves it waiting (review r41 L3). Then the clock and
    /// the account ([`Self::upload_ready`]).
    pub(in crate::device) fn upload_gate(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        path: &str,
        now: Duration,
    ) -> Result<bool, ServiceError> {
        if let Some(upload) = (self.preset_upload.as_ref()).filter(|upload| !upload.asks()) {
            return match upload.draft().path == path {
                true => Ok(false),
                false => Err(ServiceError::NotReady("a preset upload is in progress")),
            };
        }
        self.upload_ready(backends, settings, now)?;
        Ok(true)
    }

    /// A request of a preset or capture upload is out (review r31 D2, review r36 H4):
    /// [`Self::uploading`] is `Some`.
    pub fn upload_busy(&self) -> bool {
        self.uploading().is_some()
    }

    /// The `%1` of "Uploading %1, please wait." (`0x785dcc`: "Presets" for a preset folder,
    /// else "Neural Captures"; hidden by the done callback `0x7839a4`) while a request of an
    /// upload is out or a batch runs; `None` while none is out or a question waits. A preset
    /// upload first (O). I: the capture's own upload (`0x1db0e0`) shows the same status.
    pub fn uploading(&self) -> Option<&'static str> {
        if (self.preset_upload.as_ref()).is_some_and(|upload| !upload.asks()) {
            return Some("Presets");
        }
        if let Some(CaptureUpload::Checking(_) | CaptureUpload::Sending(_)) = self.capture_upload {
            return Some("Neural Captures");
        }
        match &self.upload_batch {
            Some(batch) if self.upload_prompt().is_none() => match batch.kind {
                ProductKind::Preset => Some("Presets"),
                _ => Some("Neural Captures"),
            },
            _ => None,
        }
    }

    /// (handled, total) of the running batch (`0x847934` signal `progress(handled, total, 0)`:
    /// handled counts the item now starting); `None`: no batch.
    pub fn upload_progress(&self) -> Option<(usize, usize)> {
        (self.upload_batch.as_ref()).map(|batch| (batch.handled, batch.total))
    }

    /// The running batch upload (re-r44 §2) as [`Self::bulk_run`] reports it; `None`: no
    /// batch.
    pub fn upload_batch(&self) -> Option<BatchView> {
        let batch = self.upload_batch.as_ref()?;
        Some(BatchView {
            serial: batch.serial,
            operation: BatchOperation::Upload,
            kind: batch.kind,
            handled: batch.handled,
            total: batch.total,
            source: batch.source.clone(),
        })
    }

    /// The end of the last run, a batch upload or any [`Self::bulk_run`]
    /// (`actionsFinished(target, !cancel, fromControl)`, re-r38 §2 step 4). A reader keeps the
    /// last `serial` it handled.
    pub fn last_batch_end(&self) -> Option<BatchEnd> {
        self.batch_end
    }

    /// Control BulkOperation (57) DELETE (`0xed8d0`) or the device Cancel (`0x843f2c`) sets the
    /// stop flag `+0x7c`: the item in flight completes and the batch ends before the next one
    /// with `success: false`. `false`: no batch runs.
    pub fn cancel_upload_batch(&mut self) -> bool {
        (self.upload_batch.as_mut())
            .map(|batch| batch.cancel = true)
            .is_some()
    }

    /// The upload side of the transfer state map `0x7db468` (CloudProduct 46 READ), by local
    /// key (a preset path, a capture key), in key order. C: a Directory upload sets 4,
    /// Uploading, at its start (`0x1db6ac` presets, `0x1db0e0` captures), 5, Uploaded, after a
    /// good reply (`0x1d5944`) and 3, NotUploaded, on a refusal, a failure or a cancel
    /// (`0x1e1fe0`, `0x1d5a84`, `0x1e4cbc`). Tetrad: an upload that ended without a good reply
    /// is NotUploaded. I: the walk's own capture uploads (`0x1e1a40`) are not listed.
    pub fn upload_states(&self) -> Vec<(String, UploadState)> {
        let in_flight = |key: &str| {
            (self.preset_upload.as_ref()).is_some_and(|upload| upload.draft().path == key)
                || (self.capture_upload.as_ref()).is_some_and(|upload| upload.draft().hash == key)
        };
        (self.upload_states.iter())
            .map(|(key, state)| {
                let state = match state {
                    UploadState::Uploading if !in_flight(key) => UploadState::NotUploaded,
                    state => *state,
                };
                (key.clone(), state)
            })
            .collect()
    }

    /// The upload questions that wait for their answers: the preset upload's (the overwrite
    /// dialog `0x46485c`, or its walk's capture question `0x465178` mode 0), then the capture
    /// upload's own (`0x465178` mode 0). In a batch they have the batch texts and SKIP
    /// (`0x843c90`). Each has the route of its own upload (review r39 K3: `0x463d2c` takes the
    /// Control flag per call): from Cortex Control it goes to Control only (re-r38 §3).
    pub fn upload_prompts(&self) -> impl Iterator<Item = UploadQuestion<'_>> {
        let batch = self.upload_batch.is_some();
        let preset = match &self.preset_upload {
            Some(PresetUpload::Overwrite {
                draft, question, ..
            }) => Some(UploadQuestion {
                kind: ProductKind::Preset,
                name: &draft.upload.name,
                batch,
                from_control: draft.from_control,
                serial: *question,
            }),
            Some(PresetUpload::Walk(walk)) => match (&walk.step, walk.ask.last()) {
                (WalkStep::Asking(question), Some((capture, _))) => Some(UploadQuestion {
                    kind: ProductKind::NeuralCapture,
                    name: &capture.check.name,
                    batch,
                    from_control: walk.draft.from_control,
                    serial: *question,
                }),
                _ => None,
            },
            _ => None,
        };
        let capture = match &self.capture_upload {
            Some(CaptureUpload::Asking {
                draft, question, ..
            }) => Some(UploadQuestion {
                kind: ProductKind::NeuralCapture,
                name: &draft.upload.name,
                batch,
                from_control: draft.from_control,
                serial: *question,
            }),
            _ => None,
        };
        preset.into_iter().chain(capture)
    }

    /// The waiting upload question for Cortex Control first (`from_control`), else the
    /// device's ([`Self::upload_prompts`]).
    pub fn upload_prompt(&self) -> Option<UploadQuestion<'_>> {
        let mut prompts = self.upload_prompts();
        let first = prompts.next()?;
        Some(match first.from_control {
            true => first,
            false => (prompts.find(|question| question.from_control)).unwrap_or(first),
        })
    }

    /// The answer to the waiting upload question `question` ([`UploadQuestion::serial`]) from
    /// the side that asks it: `from_control` must be the question's route (re-r38 §3; review
    /// r39 K3). An answer to a question that no longer waits, or from the other side, changes
    /// nothing.
    pub fn answer_upload(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        question: u64,
        from_control: bool,
        overwrite: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let asked = (self.upload_prompts())
            .find(|asked| asked.serial == question && asked.from_control == from_control);
        let Some(asked) = asked else {
            return Ok(vec![notice(format!(
                "upload answer for question {question} ignored: it does not wait"
            ))]);
        };
        let own_capture = matches!(
            self.capture_upload,
            Some(CaptureUpload::Asking { question: waiting, .. }) if waiting == question
        );
        match (asked.kind, own_capture) {
            (ProductKind::Preset, _) => self.answer_overwrite(backends, settings, overwrite, now),
            (_, true) => self.answer_own_capture(backends, settings, overwrite, now),
            (_, false) => self.answer_walk(backends, settings, overwrite, now),
        }
    }

    /// A new question's serial ([`UploadQuestion::serial`]).
    fn next_question(&mut self) -> u64 {
        self.upload_questions += 1;
        self.upload_questions
    }

    /// A Directory preset upload (`0x1db6ac`) of a draft the device built
    /// (`super::Device::prepare_upload`) after `Self::upload_gate`: the stock author fix
    /// (`0x73d818`), then the name check (`0x719794`). A free name posts a new product; a name
    /// on the account opens the overwrite dialog (`0x46485c`, [`Self::upload_question`]). The
    /// end: [`Self::take_upload_ends`].
    pub fn upload_preset(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        mut draft: PresetDraft,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if !self.upload_gate(backends, settings, &draft.path, now)? {
            return Ok(Vec::new());
        }
        // `0x73d818`: an empty author or an author id that is not a UUID (`0x73c0c4`) takes
        // the account's user name and id (an empty id is the account's in the body). O: stock
        // also saves this into the sidecar before the upload; Tetrad changes no user file
        // before a good reply.
        let upload = &mut draft.upload;
        if upload.author_username.is_empty() || !is_uuid(&upload.author_id) {
            upload.author_username = settings.settings().account.username.clone();
            upload.author_id.clear();
        }
        let call = CloudCall::NameCheck(upload.name.clone());
        (self.upload_states).insert(draft.path.clone(), UploadState::Uploading);
        // A question that still waits ends here, with nothing sent (review r31 D1, r41 L3).
        self.preset_upload = Some(PresetUpload::NameCheck(Box::new(draft)));
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// The name check reply (`0x1da9d0` → `0x691040` → `0x1e0e58`): free → the upload; on the
    /// account → the overwrite dialog with that product's id (in a batch with an answer, that
    /// answer at once, `0x46485c`: `af3c` 1 OVERWRITE, 2 SKIP); a failure ends the upload
    /// (`0x1e4c1c`: `done(0)`).
    pub(super) fn name_checked(
        &mut self,
        settings: &mut DeviceSettings,
        result: Result<Option<String>, ServiceError>,
        now: Duration,
    ) -> Vec<Effect> {
        let checking =
            (self.preset_upload).take_if(|upload| matches!(upload, PresetUpload::NameCheck(_)));
        let Some(PresetUpload::NameCheck(mut draft)) = checking else {
            return Vec::new();
        };
        match result {
            Ok(None) => self.start_walk(settings, draft, now),
            Ok(Some(cloud_id)) => match self.batch_answer() {
                Some(true) => {
                    draft.upload.cloud_id = Some(cloud_id);
                    self.start_walk(settings, draft, now)
                }
                Some(false) => {
                    self.item_ended();
                    Vec::new()
                }
                None => {
                    let question = self.next_question();
                    self.preset_upload = Some(PresetUpload::Overwrite {
                        draft,
                        cloud_id,
                        question,
                    });
                    Vec::new()
                }
            },
            Err(error) => {
                let name = draft.upload.name.clone();
                let log = notice(format!("upload of {name}: the name check failed: {error}"));
                self.end_upload(ProductKind::Preset, name, Err(error.to_string()));
                vec![log]
            }
        }
    }

    /// The preset request (`0x1e27f4`), after the J4 check: a preset file that changed since
    /// the draft ends the upload with nothing sent.
    pub(super) fn send_upload(
        &mut self,
        settings: &mut DeviceSettings,
        draft: Box<PresetDraft>,
        now: Duration,
    ) -> Vec<Effect> {
        if !draft.file.unchanged() {
            let name = draft.upload.name;
            let log = notice(format!("upload of {name} ended: {PRESET_CHANGED}"));
            self.end_upload(ProductKind::Preset, name, Err(PRESET_CHANGED.into()));
            return vec![log];
        }
        let call = CloudCall::Upload {
            flow: UploadFlow::Preset,
            upload: Box::new(draft.upload.clone()),
        };
        self.preset_upload = Some(PresetUpload::Sending(draft));
        self.cloud_job(settings, vec![call], None, now)
    }

    /// The walk after a free name or OVERWRITE (`0x1e08e4` → `0x7414c0`): each user capture
    /// is checked on the cloud, one at a time, in walk order. A preset without user captures
    /// goes out at once.
    pub(super) fn start_walk(
        &mut self,
        settings: &mut DeviceSettings,
        draft: Box<PresetDraft>,
        now: Duration,
    ) -> Vec<Effect> {
        if draft.captures.is_empty() {
            return self.send_upload(settings, draft, now);
        }
        let walk = CaptureWalk {
            draft: *draft,
            step: WalkStep::Checking,
            found: Vec::new(),
            create: Vec::new(),
            ask: Vec::new(),
            unavailable: Vec::new(),
            taken: Vec::new(),
        };
        self.check_next(settings, Box::new(walk), now)
    }

    /// The status check of the next capture (`0x74102c` → `0x6f94ec` flag 0, mode 0), else
    /// the walk end.
    pub(super) fn check_next(
        &mut self,
        settings: &mut DeviceSettings,
        mut walk: Box<CaptureWalk>,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(capture) = walk.draft.captures.first() else {
            return self.walk_end(settings, walk, now);
        };
        let call = CloudCall::WalkStatus(capture.check.clone());
        walk.step = WalkStep::Checking;
        self.preset_upload = Some(PresetUpload::Walk(walk));
        self.cloud_job(settings, vec![call], None, now)
    }

    /// The walk's status handler `0x7419b8`: 1 and 7 name the found id, 3 is uploaded first,
    /// 5 and 8 ask, 0 and 2, 4 and 6 refuse the preset at the walk end. A failed check ends
    /// the upload (`0x73bf08`: `done(0)`).
    pub(super) fn walk_checked(
        &mut self,
        settings: &mut DeviceSettings,
        result: Result<CaptureUploadStatus, ServiceError>,
        now: Duration,
    ) -> Vec<Effect> {
        let checking = (self.preset_upload).take_if(
            |upload| matches!(upload, PresetUpload::Walk(walk) if walk.step == WalkStep::Checking),
        );
        let Some(PresetUpload::Walk(mut walk)) = checking else {
            return Vec::new();
        };
        let capture = walk.draft.captures.remove(0);
        let name = capture.check.name.clone();
        match result {
            Ok(
                CaptureUploadStatus::Visible { cloud_id }
                | CaptureUploadStatus::Update { cloud_id },
            ) => (walk.found).push(UploadDependency {
                hash: capture.check.hash,
                id: cloud_id,
            }),
            Ok(CaptureUploadStatus::New) => walk.create.push(capture),
            Ok(CaptureUploadStatus::Ask { cloud_id }) => walk.ask.push((capture, cloud_id)),
            Ok(CaptureUploadStatus::NotAuthor) => walk.unavailable.push(name),
            Ok(CaptureUploadStatus::NameTaken) => walk.taken.push(name),
            Err(error) => {
                let reason = format!("The upload check of Neural Capture {name} failed: {error}");
                return self.walk_failed(walk, reason);
            }
        }
        self.check_next(settings, walk, now)
    }

    /// Ends the walk's preset upload with `reason` for the result dialog; nothing more is
    /// sent and nothing is written for the preset.
    pub(super) fn walk_failed(&mut self, walk: Box<CaptureWalk>, reason: String) -> Vec<Effect> {
        let name = walk.draft.upload.name;
        let log = notice(format!("upload of {name} refused: {reason}"));
        self.end_upload(ProductKind::Preset, name, Err(reason));
        vec![log]
    }

    /// Walk end `0x73e4cc`: a capture that is no longer available or whose name another
    /// product has refuses the preset (O: one dialog with both stock texts, in stock order;
    /// stock shows one dialog each). Else `0x73d818`: the found captures go into the record,
    /// from the back of the list.
    pub(super) fn walk_end(
        &mut self,
        settings: &mut DeviceSettings,
        mut walk: Box<CaptureWalk>,
        now: Duration,
    ) -> Vec<Effect> {
        let texts = [(UNAVAILABLE, &walk.unavailable), (TAKEN, &walk.taken)];
        let reasons: Vec<&str> = (texts.into_iter())
            .filter(|(_, names)| !names.is_empty())
            .map(|(text, _)| text)
            .collect();
        if !reasons.is_empty() {
            let reason = reasons.join(" ");
            return self.walk_failed(walk, reason);
        }
        let found = std::mem::take(&mut walk.found);
        walk.draft
            .upload
            .dependencies
            .extend(found.into_iter().rev());
        self.walk_next(settings, walk, now)
    }

    /// `0x73d818` again. It first checks that the preset still lives (`0x7650ec`; review r37
    /// J4: [`PresetFile::unchanged`], else the upload ends with nothing more sent). Then the
    /// next new capture is uploaded (from the back of the list), else the next capture question
    /// asks (in a batch with an answer, that answer at once: `0x465178` reads `af3c` as the
    /// preset question does), else the preset goes out with its dependencies.
    pub(super) fn walk_next(
        &mut self,
        settings: &mut DeviceSettings,
        mut walk: Box<CaptureWalk>,
        now: Duration,
    ) -> Vec<Effect> {
        if !walk.draft.file.unchanged() {
            return self.walk_failed(walk, PRESET_CHANGED.into());
        }
        if let Some(capture) = walk.create.pop() {
            return self.walk_upload(settings, walk, capture, None, now);
        }
        if let Some(overwrite) = self.batch_answer()
            && let Some((capture, cloud_id)) = walk.ask.pop()
        {
            return self.walk_answered(settings, walk, capture, cloud_id, overwrite, now);
        }
        if !walk.ask.is_empty() {
            walk.step = WalkStep::Asking(self.next_question());
            self.preset_upload = Some(PresetUpload::Walk(walk));
            return Vec::new();
        }
        self.send_upload(settings, Box::new(walk.draft), now)
    }

    /// One capture of the walk goes out (`0x1e1a40`: create 0 posts, update 1 puts to the
    /// draft's cloud id), after the J4 check (an OVERWRITE answer may come much later). A
    /// capture whose upload did not build refuses the preset here (review r39 K9), with
    /// nothing sent.
    pub(super) fn walk_upload(
        &mut self,
        settings: &mut DeviceSettings,
        mut walk: Box<CaptureWalk>,
        capture: WalkCapture,
        cloud_id: Option<String>,
        now: Duration,
    ) -> Vec<Effect> {
        if !walk.draft.file.unchanged() {
            return self.walk_failed(walk, PRESET_CHANGED.into());
        }
        let mut upload = match capture.upload {
            Ok(upload) => upload,
            Err(reason) => {
                let reason = format!("Neural Capture {}: {reason}", capture.check.name);
                return self.walk_failed(walk, reason);
            }
        };
        upload.cloud_id = cloud_id;
        let call = CloudCall::Upload {
            flow: UploadFlow::Preset,
            upload,
        };
        walk.step = WalkStep::Uploading(capture.check.hash, capture.origin);
        self.preset_upload = Some(PresetUpload::Walk(walk));
        self.cloud_job(settings, vec![call], None, now)
    }

    /// The reply to a walk capture upload. Good (`0x1d5944`, `0x73f36c`): the device writes
    /// a user capture's `cloudUUID` ([`Effect::CaptureUploaded`], no result dialog of its own:
    /// [`Self::capture_written`]; a Downloads row keeps no cloud id, re-r38 §1: `0x7dd13c` →
    /// `0x2146a4` returns 2), the record names it, and the walk goes on. Failed (`0x1e4d10`:
    /// `done(0)`): the preset upload ends; the preset is not sent.
    pub(super) fn walk_uploaded(
        &mut self,
        settings: &mut DeviceSettings,
        name: String,
        result: Result<String, ServiceError>,
        now: Duration,
    ) -> Vec<Effect> {
        let uploading = (self.preset_upload).take_if(|upload| {
            matches!(upload, PresetUpload::Walk(walk) if matches!(walk.step, WalkStep::Uploading(..)))
        });
        let Some(PresetUpload::Walk(mut walk)) = uploading else {
            return Vec::new();
        };
        let WalkStep::Uploading(hash, origin) =
            std::mem::replace(&mut walk.step, WalkStep::Checking)
        else {
            return Vec::new();
        };
        let cloud_id = match result {
            Ok(cloud_id) => cloud_id,
            Err(error) => {
                let reason = format!("Neural Capture {name} could not be uploaded: {error}");
                return self.walk_failed(walk, reason);
            }
        };
        (walk.draft.upload.dependencies).push(UploadDependency {
            hash: hash.clone(),
            id: cloud_id.clone(),
        });
        let log = notice(format!("Uploaded {name} (cloud id {cloud_id})"));
        let write = (origin == CaptureOrigin::User).then(|| {
            self.walk_writes.push(hash.clone());
            Effect::CaptureUploaded {
                hash,
                name,
                cloud_id,
            }
        });
        std::iter::once(log)
            .chain(write)
            .chain(self.walk_next(settings, walk, now))
            .collect()
    }

    /// The device's end of a capture `cloudUUID` write ([`Effect::CaptureUploaded`]): the
    /// capture upload's result dialog. A capture a preset walk uploaded has none: the preset
    /// upload ends for both (`0x73f36c` goes on with the walk).
    pub fn capture_written(&mut self, hash: &str, name: String, result: Result<String, String>) {
        match self.walk_writes.iter().position(|walked| walked == hash) {
            Some(index) => _ = self.walk_writes.remove(index),
            None => self.end_upload(ProductKind::NeuralCapture, name, result),
        }
    }

    /// The overwrite dialog's answer: OVERWRITE updates the account's preset with that name
    /// (`0x1e138c`: `PUT user/products/<the name check id>`, so the update id comes from the
    /// name check, not from the sidecar); CANCEL or SKIP ends the upload with nothing sent (I:
    /// the no callback `0x1d5b50` shows nothing). In a batch the answer serves the rest of it
    /// (`0x463264` stores 1, `0x4631fc` 2).
    pub(super) fn answer_overwrite(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        overwrite: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let asking =
            (self.preset_upload).take_if(|upload| matches!(upload, PresetUpload::Overwrite { .. }));
        let Some(PresetUpload::Overwrite {
            mut draft,
            cloud_id,
            ..
        }) = asking
        else {
            return Ok(Vec::new());
        };
        self.store_answer(overwrite);
        if !overwrite {
            self.item_ended();
            return Ok(Vec::new());
        }
        if let Err(error) = self.upload_ready(backends, settings, now) {
            let name = draft.upload.name.clone();
            self.end_upload(ProductKind::Preset, name, Err(error.to_string()));
            return Err(error);
        }
        draft.upload.cloud_id = Some(cloud_id);
        Ok(self.start_walk(settings, draft, now))
    }

    /// The preset name the overwrite dialog asks about (`0x46485c`), while it waits.
    pub fn upload_question(&self) -> Option<&str> {
        match &self.preset_upload {
            Some(PresetUpload::Overwrite { draft, .. }) => Some(&draft.upload.name),
            _ => None,
        }
    }

    /// Ends a preset or capture upload for the result dialog: a refusal before the network,
    /// a failed request, or the device's end after its write ([`Effect::PresetUploaded`],
    /// [`Effect::CaptureUploaded`]). In a batch the next item may start.
    pub(in crate::device) fn end_upload(
        &mut self,
        kind: ProductKind,
        name: String,
        result: Result<String, String>,
    ) {
        self.push_end(kind, name, result, None);
    }

    fn push_end(
        &mut self,
        kind: ProductKind,
        name: String,
        result: Result<String, String>,
        title: Option<&'static str>,
    ) {
        self.upload_ends.push(UploadEnd {
            kind,
            name,
            result,
            title,
            batch: self.upload_batch.is_some(),
        });
        self.item_ended();
    }

    /// The upload of `flow` whose call the worker lost or that will not run ends as a
    /// failure; one waiting for an answer is not on the worker and stays.
    pub(super) fn upload_lost(&mut self, flow: UploadFlow, reason: &str) {
        let name = match flow {
            UploadFlow::Capture => (self.capture_upload)
                .take_if(|upload| !matches!(upload, CaptureUpload::Asking { .. }))
                .map(|upload| upload.draft().upload.name.clone()),
            UploadFlow::Preset => (self.preset_upload)
                .take_if(|upload| !upload.asks())
                .map(|upload| upload.draft().upload.name.clone()),
        };
        if let Some(name) = name {
            self.end_upload(flow.kind(), name, Err(reason.to_owned()));
        }
    }

    /// The upload of `flow` whose request is out, taken: its write target (the capture key
    /// or the preset path). Review r37 J4: `Err` when the preset file changed since the
    /// draft, so its `cloudId` goes nowhere.
    pub(super) fn take_sending(&mut self, flow: UploadFlow) -> Option<Result<String, String>> {
        match flow {
            UploadFlow::Capture => match self.capture_upload.take()? {
                CaptureUpload::Sending(draft) => Some(Ok(draft.hash)),
                waiting => {
                    self.capture_upload = Some(waiting);
                    None
                }
            },
            UploadFlow::Preset => match self.preset_upload.take()? {
                PresetUpload::Sending(draft) if !draft.file.unchanged() => Some(Err(draft.path)),
                PresetUpload::Sending(draft) => Some(Ok(draft.path)),
                waiting => {
                    self.preset_upload = Some(waiting);
                    None
                }
            },
        }
    }

    /// The checks of a new capture upload of `hash`, before the device builds it, as
    /// [`Self::upload_gate`]: the same key out → `Ok(false)`, nothing (transfer state 4,
    /// `0x7db480`); another capture out → refused (O: one at a time); a question that still
    /// waits does not count: it ends as CANCEL when the new upload starts
    /// ([`Self::upload_capture`]; its dialog may be gone, as review r31 D1), and a refused
    /// start leaves it waiting (review r41 L3); then the clock and the account.
    pub(in crate::device) fn capture_upload_gate(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        hash: &str,
        now: Duration,
    ) -> Result<bool, ServiceError> {
        let asks = |upload: &&CaptureUpload| matches!(upload, CaptureUpload::Asking { .. });
        if let Some(upload) = (self.capture_upload.as_ref()).filter(|upload| !asks(upload)) {
            return match upload.draft().hash == hash {
                true => Ok(false),
                false => Err(ServiceError::NotReady("a capture upload is in progress")),
            };
        }
        self.upload_ready(backends, settings, now)?;
        Ok(true)
    }

    /// A user capture upload from the Directory or Control (`0x1db0e0`) of a draft the device
    /// built (`super::Device::prepare_capture_upload`) after `capture_upload_gate`: the status
    /// check (`CloudApi::capture_upload_status`, `0x6f94ec`) on the cloud worker, then the
    /// status handler `0x1e1fe0` (`capture_checked`). The end: [`Self::take_upload_ends`].
    pub fn upload_capture(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        draft: CaptureDraft,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if !self.capture_upload_gate(backends, settings, &draft.hash, now)? {
            return Ok(Vec::new());
        }
        let upload = &draft.upload;
        let call = CloudCall::CaptureStatus(CaptureCheck {
            hash: draft.hash.clone(),
            name: upload.name.clone(),
            tags: upload.tags.clone(),
            author_id: upload.author_id.clone(),
        });
        (self.upload_states).insert(draft.hash.clone(), UploadState::Uploading);
        // A question that still waits ends here, with nothing sent (review r31 D1, r41 L3).
        self.capture_upload = Some(CaptureUpload::Checking(Box::new(draft)));
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// The status handler `0x1e1fe0`: 3 posts, 7 puts to the found id, 5 and 8 ask
    /// ([`Self::capture_upload_question`]); 0, 1, 2 and 4, 6 end with the stock refusal
    /// dialogs (`0x464ce4`, `0x464e30` mode 0); a failed check ends the upload. In a batch
    /// (`0x843c90`): 5 and 8 take a stored answer at once; the two refusals show their batch
    /// text the first time only (`af40`, `af41`), later items end with no dialog.
    pub(super) fn capture_checked(
        &mut self,
        settings: &mut DeviceSettings,
        result: Result<CaptureUploadStatus, ServiceError>,
        now: Duration,
    ) -> Vec<Effect> {
        let checking =
            (self.capture_upload).take_if(|upload| matches!(upload, CaptureUpload::Checking(_)));
        let Some(CaptureUpload::Checking(mut draft)) = checking else {
            return Vec::new();
        };
        let name = draft.upload.name.clone();
        let (title, reason) = match result {
            Ok(CaptureUploadStatus::New) => return self.send_capture(settings, draft, now),
            Ok(CaptureUploadStatus::Update { cloud_id }) => {
                draft.upload.cloud_id = Some(cloud_id);
                return self.send_capture(settings, draft, now);
            }
            Ok(CaptureUploadStatus::Ask { cloud_id }) => match self.batch_answer() {
                Some(true) => {
                    draft.upload.cloud_id = Some(cloud_id);
                    return self.send_capture(settings, draft, now);
                }
                Some(false) => {
                    self.item_ended();
                    return Vec::new();
                }
                None => {
                    let question = self.next_question();
                    self.capture_upload = Some(CaptureUpload::Asking {
                        draft,
                        cloud_id,
                        question,
                    });
                    return Vec::new();
                }
            },
            Ok(CaptureUploadStatus::NotAuthor | CaptureUploadStatus::Visible { .. }) => {
                let text = match self.upload_batch.as_mut().map(|batch| &mut batch.answers) {
                    Some(answers) if answers.not_author_shown => return self.skipped(name),
                    Some(answers) => {
                        answers.not_author_shown = true;
                        "Some of the Neural Captures you selected were not created by you and \
                         will not be uploaded."
                    }
                    None => "Uploading other users' Neural Captures is not permitted.",
                };
                ("Failed to upload Neural Capture", text.to_owned())
            }
            Ok(CaptureUploadStatus::NameTaken) => {
                let text = match self.upload_batch.as_mut().map(|batch| &mut batch.answers) {
                    Some(answers) if answers.name_taken_shown => return self.skipped(name),
                    Some(answers) => {
                        answers.name_taken_shown = true;
                        "Some of the Neural Captures you selected have names that already exist \
                         on your account and will not be uploaded."
                            .to_owned()
                    }
                    None => format!(
                        "A Neural Capture named {name} already exists on your account. Please \
                         rename the existing capture or the capture you are uploading."
                    ),
                };
                ("Neural Capture name already exists", text)
            }
            Err(error) => (
                "Failed to upload Neural Capture",
                format!("The upload check failed: {error}"),
            ),
        };
        let log = notice(format!("upload of {name} refused: {reason}"));
        self.push_end(ProductKind::NeuralCapture, name, Err(reason), Some(title));
        vec![log]
    }

    /// A batch item refused with a notice the batch showed already: no dialog (C).
    fn skipped(&mut self, name: String) -> Vec<Effect> {
        self.item_ended();
        vec![notice(format!("upload of {name} skipped"))]
    }

    pub(super) fn send_capture(
        &mut self,
        settings: &mut DeviceSettings,
        draft: Box<CaptureDraft>,
        now: Duration,
    ) -> Vec<Effect> {
        let call = CloudCall::Upload {
            flow: UploadFlow::Capture,
            upload: Box::new(draft.upload.clone()),
        };
        self.capture_upload = Some(CaptureUpload::Sending(draft));
        self.cloud_job(settings, vec![call], None, now)
    }

    /// The capture name "Neural Capture %1 already exists in your account. Would you like to
    /// overwrite it?" asks about (`0x465178` mode 0, statuses 5 and 8), while it waits: the
    /// capture's own upload first, else a preset walk's.
    pub fn capture_upload_question(&self) -> Option<&str> {
        match (&self.capture_upload, &self.preset_upload) {
            (Some(CaptureUpload::Asking { draft, .. }), _) => Some(&draft.upload.name),
            (_, Some(PresetUpload::Walk(walk))) if matches!(walk.step, WalkStep::Asking(_)) => {
                (walk.ask.last()).map(|(capture, _)| capture.check.name.as_str())
            }
            _ => None,
        }
    }

    /// The capture's own upload question answered: OVERWRITE (`0x1e2924`) puts the upload to
    /// the found cloud id, replacing the account's capture; CANCEL or SKIP (`0x1d5a84`: state
    /// 3, `done(0)`) sends nothing and shows nothing. In a batch the answer serves the rest of
    /// it.
    fn answer_own_capture(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        overwrite: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let asking =
            (self.capture_upload).take_if(|upload| matches!(upload, CaptureUpload::Asking { .. }));
        let Some(CaptureUpload::Asking {
            mut draft,
            cloud_id,
            ..
        }) = asking
        else {
            return Ok(Vec::new());
        };
        self.store_answer(overwrite);
        if !overwrite {
            self.item_ended();
            return Ok(Vec::new());
        }
        if let Err(error) = self.upload_ready(backends, settings, now) {
            let name = draft.upload.name.clone();
            self.end_upload(ProductKind::NeuralCapture, name, Err(error.to_string()));
            return Err(error);
        }
        draft.upload.cloud_id = Some(cloud_id);
        Ok(self.send_capture(settings, draft, now))
    }

    /// The walk's capture question answered (in a batch the answer serves the rest of it,
    /// [`Self::walk_answered`]).
    pub(super) fn answer_walk(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        overwrite: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let asking = (self.preset_upload).take_if(|upload| {
            matches!(upload, PresetUpload::Walk(walk) if matches!(walk.step, WalkStep::Asking(_)))
        });
        let Some(PresetUpload::Walk(mut walk)) = asking else {
            return Ok(Vec::new());
        };
        let Some((capture, cloud_id)) = walk.ask.pop() else {
            return Ok(self.walk_next(settings, walk, now));
        };
        self.store_answer(overwrite);
        if overwrite && let Err(error) = self.upload_ready(backends, settings, now) {
            let name = walk.draft.upload.name;
            self.end_upload(ProductKind::Preset, name, Err(error.to_string()));
            return Err(error);
        }
        Ok(self.walk_answered(settings, walk, capture, cloud_id, overwrite, now))
    }

    /// OVERWRITE (`0x742d28`): the capture is put to the found id, then named by its reply id.
    /// CANCEL or SKIP (`0x73f150`): the preset names the found id and the capture is not sent.
    fn walk_answered(
        &mut self,
        settings: &mut DeviceSettings,
        mut walk: Box<CaptureWalk>,
        capture: WalkCapture,
        cloud_id: String,
        overwrite: bool,
        now: Duration,
    ) -> Vec<Effect> {
        if !overwrite {
            (walk.draft.upload.dependencies).push(UploadDependency {
                hash: capture.check.hash,
                id: cloud_id,
            });
            return self.walk_next(settings, walk, now);
        }
        self.walk_upload(settings, walk, capture, Some(cloud_id), now)
    }

    /// The preset uploads that ended since the last call, oldest first.
    pub fn take_upload_ends(&mut self) -> Vec<UploadEnd> {
        std::mem::take(&mut self.upload_ends)
    }

    /// Starts a batch of `keys` (preset paths or capture keys, selection order) after the
    /// checks of `0x785dcc`: an upload in flight refuses it (O: one at a time; a question that
    /// still waits does not count and ends when the batch starts, as review r31 D1; a refused
    /// batch leaves it waiting, review r41 L3), then the clock and the account (stock: signed
    /// out → return, no text). `0x848934`/`0x848710` reset the answers (`0x464830`). The
    /// device then runs the items (`super::Device::upload_next`).
    pub fn start_batch(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        kind: ProductKind,
        keys: Vec<String>,
        from_control: bool,
        now: Duration,
    ) -> Result<(), ServiceError> {
        let asks = |upload: &CaptureUpload| matches!(upload, CaptureUpload::Asking { .. });
        if (self.preset_upload.as_ref()).is_some_and(|upload| !upload.asks())
            || (self.capture_upload.as_ref()).is_some_and(|upload| !asks(upload))
        {
            return Err(ServiceError::NotReady("an upload is in progress"));
        }
        if self.bulk_running() {
            return Err(ServiceError::NotReady(RUN_IN_PROGRESS));
        }
        self.upload_ready(backends, settings, now)?;
        (self.preset_upload).take_if(|upload| upload.asks());
        (self.capture_upload).take_if(|upload| asks(upload));
        let source = match kind {
            ProductKind::Preset => (keys.first().and_then(|path| path.rsplit_once('/')))
                .map(|(folder, _)| folder.to_owned()),
            _ => None,
        };
        let serial = self.next_run(keys.len(), from_control).serial;
        self.upload_batch = Some(UploadBatch {
            serial,
            kind,
            source,
            total: keys.len(),
            keys: keys.into(),
            handled: 0,
            from_control,
            cancel: false,
            running: false,
            next_asked: false,
            answers: BatchAnswers::default(),
        });
        Ok(())
    }

    /// A batch runs: a new single upload is refused (O; stock `0x785dcc` returns while one
    /// runs, `+0x55`).
    pub fn upload_batch_running(&self) -> bool {
        self.upload_batch.is_some()
    }

    /// The next batch item for the device to build (`0x847934`, `0x848308`): with the stop
    /// flag set or no key left the batch ends (`actionsFinished(!cancel)`); else the first key
    /// in selection order, and `handled` counts it. `None`: no batch, an item runs, or the
    /// batch just ended ([`Self::last_batch_end`]).
    pub fn batch_next(&mut self) -> Option<BatchItem> {
        let batch = self.upload_batch.as_mut()?;
        batch.next_asked = false;
        if batch.running {
            return None;
        }
        match batch.keys.pop_front().filter(|_| !batch.cancel) {
            Some(key) => {
                batch.handled += 1;
                batch.running = true;
                Some(BatchItem {
                    kind: batch.kind,
                    key,
                    from_control: batch.from_control,
                })
            }
            None => {
                self.end_batch();
                None
            }
        }
    }

    /// After the device started a batch item: it runs while its upload is in flight; an item
    /// that ended at once (a refusal) lets the next one start.
    pub fn batch_item_started(&mut self) {
        let in_flight = self.preset_upload.is_some() || self.capture_upload.is_some();
        if let Some(batch) = &mut self.upload_batch {
            batch.running &= in_flight;
        }
    }

    /// Ends the batch now (`actionsFinished`, the done callback `0x7839a4`): `success` is
    /// `!cancel`.
    pub(super) fn end_batch(&mut self) {
        let Some(batch) = self.upload_batch.take() else {
            return;
        };
        self.batch_end = Some(BatchEnd {
            serial: batch.serial,
            kind: batch.kind,
            success: !batch.cancel,
            from_control: batch.from_control,
        });
    }

    /// The batch item in flight ended (good, refused, failed or skipped; `0x847cf8` does not
    /// read its result).
    fn item_ended(&mut self) {
        if let Some(batch) = &mut self.upload_batch {
            batch.running = false;
        }
    }

    /// The stored OVERWRITE / SKIP answer of the running batch (`DAT_0154af3c`).
    fn batch_answer(&self) -> Option<bool> {
        self.upload_batch.as_ref()?.answers.overwrite
    }

    fn store_answer(&mut self, overwrite: bool) {
        if let Some(batch) = &mut self.upload_batch {
            batch.answers.overwrite = Some(overwrite);
        }
    }

    /// [`Effect::UploadNext`] once after a batch item ended inside Services (a reply, an
    /// answer, a dropped call); the device's own ends start the next item directly.
    pub(super) fn batch_due(&mut self) -> Option<Effect> {
        let batch = self.upload_batch.as_mut()?;
        (!batch.running && !batch.next_asked).then(|| {
            batch.next_asked = true;
            Effect::UploadNext
        })
    }

    /// An upload needs the clock (`created`, the token refresh) and a signed-in account.
    pub(super) fn upload_ready(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Result<(), ServiceError> {
        self.unix_seconds(now)?;
        lend(backends)?;
        match self.signed_in(settings)? {
            true => Ok(()),
            false => Err(ServiceError::NotReady("uploads need a signed-in account")),
        }
    }

    /// Directory UPLOAD and ⋮ Upload (`0x725ba4`) of items Services cannot build alone. A
    /// preset is built on the device and sent with [`Self::upload_preset`], one user capture
    /// with [`Self::upload_capture`]; more of one kind are a batch (`super::Device::upload`).
    /// So after the same checks this names what is still missing. An IR is refused first
    /// ([`NO_IR_UPLOAD`]).
    pub(super) fn upload(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        items: &[tetrad_ui::UploadItem],
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        use tetrad_ui::UploadItem;
        if items.is_empty() {
            return Ok(Vec::new());
        }
        if (items.iter()).any(|item| matches!(item, UploadItem::Ir(_))) {
            return Err(ServiceError::Unsupported(NO_IR_UPLOAD));
        }
        self.upload_ready(backends, settings, now)?;
        let any = |kind: fn(&UploadItem) -> bool| items.iter().any(kind);
        let mut missing = Vec::new();
        if any(|item| matches!(item, UploadItem::Preset(_) | UploadItem::Slot(_))) {
            missing.push(
                "the device for a preset (the record, the grid image and the dependency walk: \
                 one preset at a time, Device::prepare_upload)",
            );
        }
        if any(|item| matches!(item, UploadItem::Capture(_))) {
            missing.push("the device for a capture (one user capture at a time, Device::upload)");
        }
        Ok(vec![notice(format!(
            "Upload not sent ({} item(s)): the upload builder still needs {}",
            items.len(),
            missing.join(", ")
        ))])
    }
}

/// A Directory preset upload built on the device thread (`super::Device::prepare_upload`): the
/// request without its cloud id, and the `.pb` whose sidecar takes the cloud id after a good reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetDraft {
    pub path: String,
    /// The `.pb` the draft was read from (review r37 J4).
    pub file: PresetFile,
    pub upload: ProductUpload,
    /// The user and Downloads captures of the preset in walk order (`0x7414c0`: from the
    /// last capture block); the walk checks each on the cloud.
    pub captures: Vec<WalkCapture>,
    /// The upload came from Cortex Control (`0x463d2c` flag): its questions go to Control
    /// only. Set when the device accepts the upload (review r39 K3).
    pub from_control: bool,
}

/// One user or Downloads capture of a preset walk (`0x7414c0`): the fields of its status check
/// (`0x6f94ec` mode 0, from the record) and its own upload (`0x1e1a40`). Review r39 K9: an
/// upload that did not build refuses the preset only when the walk uploads the capture (status
/// 3, or OVERWRITE); a capture the walk only names needs none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkCapture {
    pub check: CaptureCheck,
    pub origin: CaptureOrigin,
    /// The upload without its cloud id, or why it did not build.
    pub upload: Result<Box<ProductUpload>, String>,
}

/// Review r37 J4 (O): the preset file of a draft, as its host path and stamp (device, inode,
/// modification time, length). Stock `0x73d818` first checks that the record still lives
/// (`0x7650ec`); a library write stages and renames, so a save, a delete, a paste or a
/// restore changes the stamp. A `cloudId` write changes only the sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetFile {
    pub host: PathBuf,
    stamp: Option<(u64, u64, i64, i64, u64)>,
}

impl PresetFile {
    /// The stamp of `host` now; a file that does not read never matches.
    pub fn read(host: PathBuf) -> Self {
        let stamp = Self::stamp(&host);
        Self { host, stamp }
    }

    fn stamp(host: &Path) -> Option<(u64, u64, i64, i64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(host).ok()?;
        Some((
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.len(),
        ))
    }

    /// The file is still the one the draft was read from.
    pub fn unchanged(&self) -> bool {
        self.stamp.is_some() && Self::stamp(&self.host) == self.stamp
    }
}

/// Review r37 J4 (O texts; stock ends with `done(1)` and shows nothing).
pub(super) const PRESET_CHANGED: &str = "The Preset was removed or replaced on this device \
                                         during the upload, so it was not uploaded.";
pub(super) const PRESET_CHANGED_AFTER: &str = "The Preset was uploaded, but it was removed or \
                                               replaced on this device during the upload, so \
                                               its cloud id was not saved.";

/// The preset upload in flight (`0x1db6ac`): one at a time (O).
#[derive(Debug)]
pub(super) enum PresetUpload {
    /// The name check is out (`0x719794`).
    NameCheck(Box<PresetDraft>),
    /// The name is on the account: the overwrite dialog asks (`0x46485c`) as `question`
    /// ([`UploadQuestion::serial`]).
    Overwrite {
        draft: Box<PresetDraft>,
        cloud_id: String,
        question: u64,
    },
    /// The user captures go first (`0x7414c0` → `0x73d818`).
    Walk(Box<CaptureWalk>),
    /// The upload is out (`0x1e27f4`).
    Sending(Box<PresetDraft>),
}

impl PresetUpload {
    pub(super) fn draft(&self) -> &PresetDraft {
        match self {
            Self::NameCheck(draft) | Self::Overwrite { draft, .. } | Self::Sending(draft) => draft,
            Self::Walk(walk) => &walk.draft,
        }
    }

    /// The upload waits for the user: the preset overwrite dialog or the walk's capture
    /// question. No request is out.
    pub(super) fn asks(&self) -> bool {
        match self {
            Self::Overwrite { .. } => true,
            Self::Walk(walk) => matches!(walk.step, WalkStep::Asking(_)),
            _ => false,
        }
    }
}

/// The capture walk of a preset upload (`0x7414c0`, statuses `0x7419b8`, then `0x73d818`).
/// `draft.captures` holds the captures not checked yet; `draft.upload.dependencies` is the
/// record list `+0x50` the walk fills.
#[derive(Debug)]
pub(super) struct CaptureWalk {
    pub(super) draft: PresetDraft,
    pub(super) step: WalkStep,
    /// Status 1 and 7 (`+0xcc`): on the cloud as the found id.
    pub(super) found: Vec<UploadDependency>,
    /// Status 3 (`+0xc0`): uploaded as new products before the preset.
    pub(super) create: Vec<WalkCapture>,
    /// Status 5 and 8 (`+0xd8`): the question, with the found id.
    pub(super) ask: Vec<(WalkCapture, String)>,
    /// Status 0 and 2 (`+0xa8`) and 4 and 6 (`+0xb4`): the capture names.
    pub(super) unavailable: Vec<String>,
    pub(super) taken: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum WalkStep {
    /// The status check of the first of `draft.captures` is out (`0x74102c`).
    Checking,
    /// The upload of the capture with this hash is out (`0x1e1a40`).
    Uploading(String, CaptureOrigin),
    /// The question asks about the last of `ask` (`0x465178` mode 0) as this
    /// [`UploadQuestion::serial`].
    Asking(u64),
}

/// Walk end `0x73e4cc` (title "Failed to upload Preset"): statuses 0 and 2.
pub(super) const UNAVAILABLE: &str =
    "This Preset could not be uploaded as one or more Neural Captures are no longer available.";
/// Walk end `0x73e4cc`: statuses 4 and 6.
pub(super) const TAKEN: &str = "This Preset could not be uploaded as one or more Neural Captures with the \
                     same name already exist on your Cortex Cloud account. Please rename the \
                     existing Capture(s) or the Capture(s) you are uploading.";

/// A user capture upload built on the device thread (`super::Device::prepare_capture_upload`,
/// re-r34 §1): the request without its cloud id, and the capture key whose `cloudUUID` takes
/// the cloud id after a good reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDraft {
    pub hash: String,
    pub origin: CaptureOrigin,
    pub upload: ProductUpload,
    /// The upload came from Cortex Control: as [`PresetDraft::from_control`].
    pub from_control: bool,
}

/// The store of a capture record (`0x6f6484`: the user store, then Downloads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOrigin {
    /// A good upload writes the record's `cloudUUID`.
    User,
    /// Only a preset walk uploads it (re-r38 §1); a good upload writes nothing local.
    Downloads,
}

/// The capture upload in flight (`0x1db0e0`): one at a time (O).
#[derive(Debug)]
pub(super) enum CaptureUpload {
    /// The status check is out (`0x6f94ec`).
    Checking(Box<CaptureDraft>),
    /// Status 5 or 8: the question waits (`0x465178` mode 0) as `question`
    /// ([`UploadQuestion::serial`]) with the found cloud id.
    Asking {
        draft: Box<CaptureDraft>,
        cloud_id: String,
        question: u64,
    },
    /// The POST or PUT is out (`0x1e1a40`).
    Sending(Box<CaptureDraft>),
}

impl CaptureUpload {
    pub(super) fn draft(&self) -> &CaptureDraft {
        match self {
            Self::Checking(draft) | Self::Asking { draft, .. } | Self::Sending(draft) => draft,
        }
    }
}

/// The end of a Directory preset or capture upload, for the result dialog
/// ([`Services::take_upload_ends`], [`Self::dialog`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadEnd {
    pub kind: ProductKind,
    /// The preset or capture name.
    pub name: String,
    /// The product's cloud id, or why the upload failed or was refused.
    pub result: Result<String, String>,
    /// The stock title of a refusal when it is not the kind's failure title (`0x464e30`).
    pub title: Option<&'static str>,
    /// An item of a batch: a good one shows no dialog (C: stock only moves the bar).
    pub batch: bool,
}

/// A Multi Select or Cortex Control batch upload (re-r38 §2: `0x785dcc`, loops `0x847934`
/// captures and `0x848308` presets): one item at a time in selection order; the end of each
/// item (good, refused, failed, skipped) starts the next.
#[derive(Debug)]
pub(super) struct UploadBatch {
    /// [`BatchView::serial`].
    serial: u64,
    kind: ProductKind,
    /// [`BatchView::source`].
    source: Option<String>,
    /// The keys not started yet (preset paths or capture keys).
    keys: VecDeque<String>,
    total: usize,
    /// The started items, the one in flight included.
    handled: usize,
    from_control: bool,
    /// The stop flag `+0x7c`.
    cancel: bool,
    /// An item is in flight.
    running: bool,
    /// [`Effect::UploadNext`] is out for the ended item.
    next_asked: bool,
    answers: BatchAnswers,
}

/// The batch answers `0x464830` resets.
#[derive(Debug, Default)]
struct BatchAnswers {
    /// OVERWRITE or SKIP for every later overwrite question of the batch (`DAT_0154af3c`).
    overwrite: Option<bool>,
    /// The not-author notice was shown (`af40`).
    not_author_shown: bool,
    /// The name-taken notice was shown (`af41`).
    name_taken_shown: bool,
}

/// A local key's upload state in the transfer state map ([`Services::upload_states`]):
/// `CloudTransferItemState` 3, 4 and 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadState {
    NotUploaded,
    Uploading,
    Uploaded,
}

/// An upload question that waits for its answer ([`Services::upload_prompt`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadQuestion<'a> {
    /// `Preset`: answer with `UiCommand::UploadOverwrite`; `NeuralCapture`:
    /// `UiCommand::CaptureUploadOverwrite`.
    pub kind: ProductKind,
    /// The preset or capture name.
    pub name: &'a str,
    /// A batch question: the batch texts and SKIP; the answer serves the rest of the batch.
    pub batch: bool,
    /// The question's own upload came from Cortex Control: the question goes to Control
    /// only (review r39 K3).
    pub from_control: bool,
    /// Counts the questions asked, from 1: an answer names the question it answers
    /// ([`Services::answer_upload`]).
    pub serial: u64,
}

/// The next batch item for the device to build ([`Services::batch_next`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchItem {
    pub kind: ProductKind,
    /// A preset path or a capture key.
    pub key: String,
    /// The batch came from Cortex Control (`0x785dcc(.., fromControl)`).
    pub from_control: bool,
}

impl UploadQuestion<'_> {
    /// The stock title (`0x46485c`, `0x465178`).
    pub fn title(&self) -> String {
        match (self.kind, self.batch) {
            (ProductKind::Preset, true) => "Overwrite Existing Presets?",
            (ProductKind::Preset, false) => "Preset already exists",
            _ => "Neural Capture already exists",
        }
        .to_owned()
    }

    /// The stock text (`0x46485c`, `0x465178` mode 0, the account forms).
    pub fn text(&self) -> String {
        match (self.kind, self.batch) {
            (ProductKind::Preset, true) => {
                "Some of the Presets you are trying to upload already exist in your account."
                    .to_owned()
            }
            (ProductKind::Preset, false) => {
                format!("Preset {} already exists. Overwrite?", self.name)
            }
            (_, true) => "Some of the neural captures you selected already exist in your \
                          account. Would you like to overwrite them?"
                .to_owned(),
            (_, false) => format!(
                "Neural Capture {} already exists in your account. Would you like to \
                 overwrite it?",
                self.name
            ),
        }
    }

    /// The device buttons (`0x463d2c`): OVERWRITE, then SKIP in a batch, else CANCEL.
    pub fn buttons(&self) -> (&'static str, &'static str) {
        ("OVERWRITE", if self.batch { "SKIP" } else { "CANCEL" })
    }
}

impl UploadEnd {
    /// The result dialog: (title, body). A failure under the stock title of its kind ("Failed
    /// to upload Preset", `0x73e4cc`; "Failed to upload Neural Capture", `0x464ce4`; I for a
    /// request failure) or [`Self::title`]; a good upload with an owned text (stock only
    /// closes its progress dialog, `0x7839a4`).
    pub fn dialog(&self) -> (&'static str, String) {
        let capture = self.kind == ProductKind::NeuralCapture;
        let (noun, failed) = match capture {
            true => ("Neural Capture", "Failed to upload Neural Capture"),
            false => ("Preset", "Failed to upload Preset"),
        };
        match &self.result {
            Ok(_) => (
                if capture {
                    "Neural Capture uploaded"
                } else {
                    "Preset uploaded"
                },
                format!(
                    "{noun} {} was uploaded to your Cortex Cloud account.",
                    self.name
                ),
            ),
            Err(reason) => (self.title.unwrap_or(failed), reason.clone()),
        }
    }
}

/// The `X-Cloud-Api-Meta-Signature` bytes of a capture upload (`0x6fffe4`): the seed-only
/// capture cipher (secret slot 0: `0x6aab4c` → `0x9f4e28(0, …)`) over the lowercase hex SHA-256
/// of the capture's metadata (`0x7ee62c(product, 0)` → `0x6a9204`). `seed` is a cipher handle;
/// no key bytes leave it.
pub fn capture_meta_signature(seed: &DeviceCipher, metadata: &[u8]) -> Vec<u8> {
    let mut signature = product_digest(metadata).into_bytes();
    seed.apply(&mut signature);
    signature
}
