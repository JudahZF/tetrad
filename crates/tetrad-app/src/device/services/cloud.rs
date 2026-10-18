//! The cloud queue: jobs to the worker, replies, dropped and lost calls.

use super::*;

impl Services {
    pub(super) fn unix_millis(&self, now: Duration) -> Result<i64, ServiceError> {
        let (set, at) = self
            .wall
            .ok_or(ServiceError::NotReady("time is not set yet (no NTP sync)"))?;
        let elapsed = i64::try_from(now.saturating_sub(at).as_millis()).unwrap_or(0);
        Ok(set.saturating_mul(1000).saturating_add(elapsed))
    }

    pub(super) fn unix_seconds(&self, now: Duration) -> Result<i64, ServiceError> {
        self.unix_millis(now).map(|ms| ms.div_euclid(1000))
    }

    /// The route of the cloud calls, set by the device before services work from
    /// `Control::cloud_route`: Cortex Control while it is connected (`0x6e0444`) and its
    /// KeepAlive says online (`0x6e045c`), else `None` (Wi-Fi). Stock tests both per call in
    /// `0x80da7c` (re-r44 §3); here each cloud job and backup body takes the route when it is
    /// handed to the worker (O). The update check keeps its own transport.
    pub fn set_control_route(&mut self, route: Option<ControlTransport>) {
        self.control_route = route;
    }

    /// The session, loaded from the saved tokens on first use, in the `CLOUD_*` stage.
    pub(super) fn session(
        &mut self,
        settings: &mut DeviceSettings,
    ) -> Result<&mut CloudApi, ServiceError> {
        if self.cloud_out {
            return Err(ServiceError::NotReady(
                "the cloud session is on the service worker",
            ));
        }
        if self.cloud.is_none() {
            self.cloud = Some(Self::load_session(settings, self.cloud_stage.clone())?);
        }
        self.cloud
            .as_mut()
            .ok_or(ServiceError::NotReady("no cloud session"))
    }

    /// The session's sign-in state, also while a job holds it.
    pub(super) fn signed_in(
        &mut self,
        settings: &mut DeviceSettings,
    ) -> Result<bool, ServiceError> {
        if self.cloud_out {
            return Ok(self.cloud_signed_in);
        }
        Ok(self.session(settings)?.is_signed_in())
    }

    /// Boot: the cloud session takes the saved tokens into memory. A factory reset blanks
    /// `AccountConfig.ini` before its steps run; the unpair (`0x004de79c` → `0x00308250`) still
    /// signs with this in-memory bearer, and the logout (`0x002ff6e0`) comes after it.
    pub fn load_cloud(&mut self, settings: &mut DeviceSettings) -> Result<(), ServiceError> {
        if self.cloud_out {
            return Ok(());
        }
        self.session(settings).map(drop)
    }

    pub(super) fn load_session(
        settings: &mut DeviceSettings,
        stage: CloudStage,
    ) -> Result<CloudApi, TokenStoreError> {
        let domain = match settings.settings().cloud_domain {
            SavedDomain::Default => CloudDomain::Default,
            SavedDomain::Alternative => CloudDomain::Alternative,
        };
        let endpoint = CloudEndpoint { stage, domain };
        // Filled in before each signed request from `SERIAL_PATH`.
        let device = DeviceIdentity {
            client_type: ClientType::QuadCortex,
            firmware: tetrad_firmware::running().version.to_owned(),
            serial: String::new(),
        };
        CloudApi::load(&mut SettingsTokens(settings), endpoint, device, false)
    }

    /// Queues `calls` behind the job on the worker, or sends them now.
    pub(super) fn cloud_job(
        &mut self,
        settings: &mut DeviceSettings,
        calls: Vec<CloudCall>,
        signer: Option<(String, SharcSigner)>,
        now: Duration,
    ) -> Vec<Effect> {
        // Review r47 N4: a download batch with no download left in flight is over (its stores
        // ran on the device thread before this request), so a later store of one of its ids
        // (an OVERWRITE answer, `CaptureOverwrite`) gets the single name-taken text (stock
        // clears the flag at each batch start, `0x464830`).
        let out = &self.downloads_out;
        if (self.download_batch.as_ref())
            .is_some_and(|batch| !batch.ids.iter().any(|id| out.contains(id)))
        {
            self.download_batch = None;
        }
        self.cloud_queue.push_back(PendingCloud { calls, signer });
        self.next_cloud(settings, now)
    }

    /// Hands the next queued calls and the session to the worker.
    pub(super) fn next_cloud(
        &mut self,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        if self.cloud_out || self.cloud_queue.is_empty() {
            return Vec::new();
        }
        if let Err(error) = self.session(settings) {
            return self.drop_pending(&error);
        }
        let (Some(session), Some(pending)) = (self.cloud.take(), self.cloud_queue.pop_front())
        else {
            return Vec::new();
        };
        self.cloud_out = true;
        self.cloud_signed_in = session.is_signed_in();
        let job = CloudJob {
            session,
            tokens: TokenLog::default(),
            clock: self.unix_millis(now).ok(),
            signer: pending.signer,
            calls: pending.calls,
            via_control: self.control_route.clone(),
        };
        vec![Effect::Service(ServiceJob::Cloud(Handoff::new(job)))]
    }

    /// A [`CloudReply`]: the session comes home, token changes reach `AccountConfig.ini`,
    /// each result continues its flow, then the next queued calls go out.
    pub(super) fn cloud_done<B: ServiceBackends>(
        &mut self,
        io: &mut ServiceIo<'_, B>,
        settings: &mut DeviceSettings,
        reply: Option<CloudReply>,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(reply) = reply else {
            return Vec::new();
        };
        self.cloud_out = false;
        self.cloud = reply.session;
        let mut effects = save_tokens(settings, reply.tokens);
        if reply.lost {
            effects.extend(self.cloud_lost(io));
        }
        for done in reply.done {
            effects.extend(self.finish_call(io, settings, done, now));
        }
        effects.extend(self.next_cloud(settings, now));
        effects
    }

    /// Queued calls that will not run (no session, no backends): each flow ends as a failure,
    /// as its failed reply would end it (owned, review r14 S-4). A dropped unpair forgets no
    /// networks: that needs the supplicant.
    pub(super) fn drop_pending(&mut self, reason: &dyn Display) -> Vec<Effect> {
        let mut effects = vec![notice(format!("cloud requests dropped: {reason}"))];
        let calls: Vec<CloudCall> = (self.cloud_queue.drain(..))
            .flat_map(|pending| pending.calls)
            .collect();
        for call in calls {
            if let Some(flow) = call.upload_flow() {
                self.upload_lost(flow, &reason.to_string());
                effects.extend(self.batch_due());
                continue;
            }
            match call {
                call if call.is_licence_call() => self.licence = None,
                CloudCall::Unpair(_) | CloudCall::Logout(_) => {
                    self.unpairing = self.unpairing.saturating_sub(1);
                }
                CloudCall::UpdateCheck { .. }
                    if self.updater.state == UpdaterState::RequestingData =>
                {
                    effects.extend(self.report(UpdaterState::Failed));
                }
                CloudCall::ControlLogin { request_id, .. }
                | CloudCall::Pair(PairFor::Control(request_id)) => {
                    effects.push(control_login_failed(request_id));
                }
                CloudCall::Nc2(CloudRequest::Nc2Job(_)) => {
                    effects.push(Effect::Nc2(Nc2Input::JobFailed {
                        online: self.online == Some(true),
                    }));
                }
                CloudCall::Nc2(CloudRequest::Nc2Download(_)) => {
                    effects.push(Effect::Nc2(Nc2Input::Downloaded(None)));
                }
                CloudCall::BackupTarget { .. }
                | CloudCall::BackupUpdate { .. }
                | CloudCall::BackupStatus { .. }
                | CloudCall::BackupSource { .. } => {
                    effects.extend(self.backup_failed(&reason.to_string()));
                }
                CloudCall::Backups { .. } => effects.extend(self.backups_list_failed(&reason)),
                CloudCall::BackupRename { .. } => {
                    effects.extend(self.backup_rename_failed(&reason));
                }
                CloudCall::Browse(page) => {
                    let folder = self.cloud_folder_mut(page.kind, page.scope);
                    folder.fetching = false;
                    folder.error = Some(reason.to_string());
                }
                CloudCall::DeleteProduct { cloud_id, .. }
                    if (self.delete_run.as_ref())
                        .is_some_and(|run| run.current.as_ref() == Some(&cloud_id)) =>
                {
                    self.stop_delete_run(false);
                }
                CloudCall::DownloadCapture { hash } => {
                    self.end_capture_download(hash, DownloadOutcome::Failed);
                }
                CloudCall::Download { product_id, origin } => {
                    self.end_download(&product_id);
                    if matches!(
                        origin,
                        DownloadOrigin::Slot { .. } | DownloadOrigin::Dependency { .. }
                    ) {
                        effects.extend(self.stop_preset_downloads("its request was dropped"));
                    }
                    if matches!(origin, DownloadOrigin::QueueDependency { .. }) {
                        effects.extend(self.drop_queue_presets("its request was dropped"));
                    }
                }
                _ => {}
            }
        }
        effects
    }

    /// The worker stopped with a job (owned): the flows it held end as failures.
    pub(super) fn cloud_lost<B: ServiceBackends>(
        &mut self,
        io: &mut ServiceIo<'_, B>,
    ) -> Vec<Effect> {
        let mut effects = vec![notice(format!(
            "cloud requests lost: {}",
            JobError::WorkerStopped
        ))];
        if matches!(self.licence, Some(LicenceStep::Signing)) {
            self.licence = None;
        }
        if self.updater.state == UpdaterState::RequestingData {
            effects.extend(self.report(UpdaterState::Failed));
        }
        if matches!(
            self.backup,
            Some(
                BackupFlow::Targeting { .. }
                    | BackupFlow::Locating
                    | BackupFlow::Validating { next: None, .. }
            )
        ) {
            effects.extend(self.backup_failed(&JobError::WorkerStopped.to_string()));
        }
        // A folder page or NC editor DOWNLOAD still queued keeps its state; one on the lost
        // job does not come back.
        let queued: Vec<(ProductKind, BrowseScope)> = (self.cloud_queue.iter())
            .flat_map(|pending| &pending.calls)
            .filter_map(|call| match call {
                CloudCall::Browse(page) => Some((page.kind, page.scope)),
                _ => None,
            })
            .collect();
        let queued_hashes: Vec<String> = (self.cloud_queue.iter())
            .flat_map(|pending| &pending.calls)
            .filter_map(|call| match call {
                CloudCall::DownloadCapture { hash } => Some(hash.clone()),
                _ => None,
            })
            .collect();
        let lost: Vec<String> = (self.capture_jobs.iter())
            .filter(|hash| !queued_hashes.contains(hash))
            .cloned()
            .collect();
        for hash in lost {
            self.end_capture_download(hash, DownloadOutcome::Failed);
        }
        let queued_ids: Vec<&String> = (self.cloud_queue.iter())
            .flat_map(|pending| &pending.calls)
            .filter_map(|call| match call {
                CloudCall::Download { product_id, .. } => Some(product_id),
                _ => None,
            })
            .collect();
        (self.downloads_out).retain(|id| queued_ids.contains(&id));
        let held = (self.queue_held.front()).and_then(|held| held.current.as_ref());
        let held_lost = held.is_some_and(|id| !queued_ids.contains(&id));
        let current = (self.preset_downloads.as_ref()).and_then(PresetBatch::waits_on);
        if current.is_some_and(|id| !queued_ids.contains(&id)) {
            effects.extend(self.stop_preset_downloads("the worker stopped"));
        }
        if held_lost {
            effects.extend(self.drop_queue_presets("the worker stopped"));
        }
        self.check_download_run();
        let delete_lost = (self.delete_run.as_ref())
            .and_then(|run| run.current.as_ref())
            .is_some_and(|id| {
                !(self.cloud_queue.iter())
                    .flat_map(|pending| &pending.calls)
                    .any(|call| matches!(call, CloudCall::DeleteProduct { cloud_id, .. } if cloud_id == id))
            });
        if delete_lost {
            self.stop_delete_run(false);
        }
        let uploads_queued: Vec<UploadFlow> = (self.cloud_queue.iter())
            .flat_map(|pending| &pending.calls)
            .filter_map(CloudCall::upload_flow)
            .collect();
        for flow in [UploadFlow::Preset, UploadFlow::Capture] {
            if !uploads_queued.contains(&flow) {
                self.upload_lost(flow, &JobError::WorkerStopped.to_string());
            }
        }
        self.cloud_changes += 1;
        for (key, folder) in &mut self.cloud_folders {
            if folder.fetching && !queued.contains(key) {
                folder.fetching = false;
                folder.error = Some(JobError::WorkerStopped.to_string());
            }
        }
        // Only the unpairs of the lost job end; queued and batched ones still run.
        let waiting = (self.cloud_queue.iter())
            .flat_map(|pending| &pending.calls)
            .chain(self.batch.iter().flat_map(|batch| &batch.jobs))
            .filter(|call| matches!(call, CloudCall::Unpair(_) | CloudCall::Logout(_)))
            .count();
        while usize::try_from(self.unpairing).is_ok_and(|unpairing| unpairing > waiting) {
            effects.extend(self.unpair_finished(io.supplicant));
        }
        effects
    }

    pub(super) fn finish_call<B: ServiceBackends>(
        &mut self,
        io: &mut ServiceIo<'_, B>,
        settings: &mut DeviceSettings,
        done: CloudDone,
        now: Duration,
    ) -> Vec<Effect> {
        match done {
            CloudDone::SignIn(Ok(())) => self.start_pair(io.files, PairFor::SignIn, now),
            CloudDone::SignIn(Err(error)) => vec![notice(format!("sign-in failed: {error}"))],
            CloudDone::ControlLogin {
                request_id,
                result: Ok(()),
            } => self.start_pair(io.files, PairFor::Control(request_id), now),
            CloudDone::ControlLogin {
                request_id,
                result: Err(error),
            } => vec![
                notice(format!("Got invalid access token from Control: {error}")),
                control_login_failed(request_id),
            ],
            CloudDone::Pair {
                reason,
                paired,
                profile,
            } => {
                let paired = paired.map(|version| {
                    settings.update(|s| s.paired_version = PairedVersion(version));
                    flush(settings).into_iter().collect()
                });
                let mut effects =
                    self.after_pair(io.files, reason, paired.map_err(Into::into), now);
                // 0x0030afb8: a failed profile drops the in-memory tokens (0x0044d590).
                if let Some(Err(error)) = profile {
                    if let Some(api) = self.cloud.as_mut() {
                        api.forget_tokens();
                    }
                    effects.push(notice(format!(
                        "Fetching user profile failed. Network error? ({error})"
                    )));
                }
                effects
            }
            CloudDone::Logout { unpaired, logout } => {
                let mut effects = Vec::new();
                match unpaired {
                    Some(Ok(version)) => {
                        settings.update(|s| s.paired_version = PairedVersion(version));
                        effects.extend(flush(settings));
                    }
                    Some(Err(error)) => effects.push(notice(format!(
                        "(Un)pairHardware() failed when logging out: {error}"
                    ))),
                    None => {}
                }
                if let Err(error) = logout {
                    effects.push(notice(format!("sign-out request failed: {error}")));
                }
                effects.extend(self.unpair_finished(io.supplicant));
                effects
            }
            CloudDone::UpdateCheck {
                then_download,
                checked,
                via_control,
            } => {
                let mut effects = self.checked(checked);
                if then_download
                    && !self.installing()
                    && self.updater.status == UpdaterStatus::NewVersionAvailableToDownload
                {
                    effects.extend(
                        self.start_download(via_control).unwrap_or_else(|error| {
                            vec![notice(format!("update download: {error}"))]
                        }),
                    );
                }
                effects
            }
            CloudDone::Licences(Ok(code)) => {
                self.licence = Some(LicenceStep::Dsp(now));
                vec![Effect::Dsp(DspCommand::Request(Request::Blob50(code)))]
            }
            CloudDone::Licences(Err(error)) => self.licence_failed(error),
            CloudDone::Backups(Ok(items)) => {
                self.backups = items.iter().filter_map(CloudBackup::parse).collect();
                if let Some(renaming) = self.control_rename_listed(settings, now) {
                    return renaming;
                }
                let lists = [
                    ControlCloud::List,
                    ControlCloud::Delete,
                    ControlCloud::Rename,
                ];
                let done = super::control::CloudBackupDone::Done;
                self.control_cloud_end(&lists, done).into_iter().collect()
            }
            CloudDone::Backups(Err(error)) => self.backups_list_failed(&error),
            CloudDone::BackupRenamed(result) => self.backup_renamed(settings, result, now),
            CloudDone::BackupTarget(Ok(target)) => {
                let update = matches!(self.backup, Some(BackupFlow::Targeting { update: true }));
                let (path, header) = match (target.data(), self.unix_seconds(now)) {
                    (Ok(data), Ok(unix)) => (data.url, self.backup_header(settings, unix)),
                    (Err(error), _) => return self.backup_failed(&error.to_string()),
                    (_, Err(error)) => return self.backup_failed(&error.to_string()),
                };
                self.backup = Some(BackupFlow::Uploading { path, update });
                vec![Effect::Service(ServiceJob::Backup(BackupJob::Upload {
                    archive: UPLOAD_ARCHIVE.into(),
                    target,
                    header: Box::new(header),
                    via_control: self.control_route.clone(),
                }))]
            }
            // `0x30e5c0`: "completed" ends well, and so does an error ("Backup upload status
            // error!"); any other state polls again after 1 s.
            CloudDone::BackupStatus(result) => match (self.backup.take(), result) {
                (Some(BackupFlow::Validating { update, .. }), Ok(BackupState::Completed)) => {
                    self.backup_created(settings, Vec::new(), update, now)
                }
                (Some(BackupFlow::Validating { update, .. }), Err(error)) => {
                    let note = format!("Backup upload status error! {error}");
                    self.backup_created(settings, vec![note], update, now)
                }
                (
                    Some(BackupFlow::Validating {
                        path,
                        since,
                        update,
                        ..
                    }),
                    Ok(_),
                ) => {
                    let next = Some(now + BACKUP_POLL_INTERVAL);
                    self.backup = Some(BackupFlow::Validating {
                        path,
                        since,
                        next,
                        update,
                    });
                    Vec::new()
                }
                (flow, _) => {
                    self.backup = flow;
                    Vec::new()
                }
            },
            CloudDone::BackupSource(Ok((url, creator))) => {
                // Review r23 K8: `0x407908` reads `creator` from this reply, not the list item.
                self.restore_source = Some(source_hardware(&creator));
                self.backup = Some(BackupFlow::Fetching);
                vec![Effect::Service(ServiceJob::Backup(BackupJob::Fetch {
                    url,
                    via_control: self.control_route.clone(),
                }))]
            }
            CloudDone::BackupTarget(Err(error)) | CloudDone::BackupSource(Err(error)) => {
                self.backup_failed(&error.to_string())
            }
            // 0x00201118: an unreadable body fails the poll as a reply; transport errors follow
            // the internet status (0x00200ddc).
            CloudDone::Nc2Job(result) => vec![Effect::Nc2(match result {
                Ok(reply) => Nc2Input::JobReply(serde_json::to_vec(&reply).unwrap_or_default()),
                Err(ServiceError::Cloud(ApiError::Reply(_))) => Nc2Input::JobReply(Vec::new()),
                Err(_) => Nc2Input::JobFailed {
                    online: self.online == Some(true),
                },
            })],
            // A preset walk's capture goes on with the walk.
            CloudDone::Upload {
                flow: UploadFlow::Preset,
                name,
                result,
            } if matches!(self.preset_upload, Some(PresetUpload::Walk(_))) => {
                self.walk_uploaded(settings, name, result, now)
            }
            // A preset or capture upload ends on the device: the sidecar `cloudId` or the
            // capture `cloudUUID` write ([`Effect::PresetUploaded`], [`Effect::CaptureUploaded`],
            // `0x1d5944`), only for the upload of the reply's own flow.
            CloudDone::Upload { flow, name, result } => {
                let log = notice(match &result {
                    Ok(id) => format!("Uploaded {name} (cloud id {id})"),
                    Err(error) => format!("upload of {name} failed: {error}"),
                });
                let written = match (self.take_sending(flow), result, flow) {
                    (Some(Ok(hash)), Ok(cloud_id), UploadFlow::Capture) => {
                        (self.upload_states).insert(hash.clone(), UploadState::Uploaded);
                        Some(Effect::CaptureUploaded {
                            hash,
                            name,
                            cloud_id,
                        })
                    }
                    (Some(Ok(path)), Ok(cloud_id), UploadFlow::Preset) => {
                        (self.upload_states).insert(path.clone(), UploadState::Uploaded);
                        Some(Effect::PresetUploaded {
                            path,
                            name,
                            cloud_id,
                        })
                    }
                    // Review r37 J4: the product is on the account, but the preset file is
                    // gone or replaced, so no `cloudId` is written (O).
                    (Some(Err(_)), Ok(_), _) => {
                        self.end_upload(flow.kind(), name, Err(PRESET_CHANGED_AFTER.into()));
                        None
                    }
                    (Some(_), Err(error), _) => {
                        self.end_upload(flow.kind(), name, Err(error.to_string()));
                        None
                    }
                    (None, _, _) => None,
                };
                std::iter::once(log).chain(written).collect()
            }
            CloudDone::NameCheck(result) => self.name_checked(settings, result, now),
            CloudDone::CaptureStatus(result) => self.capture_checked(settings, result, now),
            CloudDone::WalkStatus(result) => self.walk_checked(settings, result, now),
            CloudDone::Browse { page, result } => {
                let folder = self.cloud_folder_mut(page.kind, page.scope);
                folder.fetching = false;
                match result {
                    // Review r27 R3: a page is full by its raw rows, so a refused row does
                    // not end the paging.
                    Ok(BrowsedRows { raw, rows }) => {
                        folder.more = raw == CLOUD_PAGE_ROWS as usize;
                        folder.next_page = page.page + 1;
                        for row in rows {
                            if !folder.rows.iter().any(|known| known.id == row.id) {
                                folder.rows.push(row);
                            }
                        }
                        folder.more &= folder.rows.len() < CLOUD_FOLDER_ROWS;
                        folder.error = None;
                        Vec::new()
                    }
                    Err(error) => {
                        folder.error = Some(error.to_string());
                        // The UI shows the stock text; the log also names the HTTP status.
                        let status = match &error {
                            ServiceError::Cloud(tetrad_services::cloud::api::ApiError::Cloud(
                                reply,
                            )) => format!(" (HTTP {})", reply.http_status),
                            _ => String::new(),
                        };
                        vec![notice(format!(
                            "cloud folder request failed: {error}{status}"
                        ))]
                    }
                }
            }
            // 0x0071d538: a product that does not download or unpack is not stored. The store
            // effect runs on the device thread before the next request is read, so the
            // download ends here.
            CloudDone::Downloads(downloads) => {
                let mut effects = Vec::new();
                for (id, download) in downloads {
                    self.end_download(&id);
                    match download {
                        Ok(download) => {
                            effects.extend(self.product_downloaded(&id, download, settings, now))
                        }
                        Err(error) => {
                            let text = format!("cloud product {id} not downloaded: {error}");
                            effects.push(notice(text));
                            // `0x73bf8c`: a failed dependency ends its preset with `false`.
                            if (self.preset_downloads.as_ref())
                                .is_some_and(|batch| batch.waits_on() == Some(&id))
                            {
                                effects.extend(self.stop_preset_downloads("its download failed"));
                            }
                            if (self.queue_held.front())
                                .is_some_and(|held| held.current.as_ref() == Some(&id))
                            {
                                let reason = "a capture download failed";
                                effects.extend(self.stop_queue_preset(reason, settings, now));
                            }
                        }
                    }
                }
                effects
            }
            // Review r27 R1: the job ends on the store of this download (or here on an error).
            CloudDone::CaptureDownload { hash, result } => match result {
                Ok(download) => vec![Effect::StoreDownload(download)],
                Err(error) => {
                    self.end_capture_download(hash.clone(), DownloadOutcome::Failed);
                    vec![notice(format!(
                        "cloud product {hash} not downloaded: {error}"
                    ))]
                }
            },
            // O: the listed CLOUD row goes with a good reply (stock refresh not traced). The
            // ids are logged either way. In a run the reply starts the next DELETE.
            CloudDone::Deleted {
                kind,
                cloud_id,
                result,
            } => {
                let text = match result {
                    Ok(()) => {
                        let listed = (self.cloud_folder(kind, BrowseScope::OwnedByMe))
                            .is_some_and(|folder| folder.rows.iter().any(|row| row.id == cloud_id));
                        if listed {
                            let folder = self.cloud_folder_mut(kind, BrowseScope::OwnedByMe);
                            folder.rows.retain(|row| row.id != cloud_id);
                        }
                        format!("cloud product {cloud_id} deleted from the account")
                    }
                    Err(error) => format!("cloud product {cloud_id} not deleted: {error}"),
                };
                let mut effects = vec![notice(text)];
                effects.extend(self.delete_ended(&cloud_id, settings, now));
                effects
            }
            CloudDone::LeftQueue { cloud_id, result } => result
                .err()
                .map(|error| notice(format!("{cloud_id} stays in the download queue: {error}")))
                .into_iter()
                .collect(),
            CloudDone::Nc2Delete(result) => result
                .err()
                .map(|error| notice(format!("NC2 product delete failed: {error}")))
                .into_iter()
                .collect(),
            CloudDone::Nc2Download { product_id, result } => match result {
                // 0x0071d538 → 0x0045fa44: a product that fails its check is not stored.
                // I: an empty payload is the check Tetrad knows; the stock rule (vtable `+0x98`)
                // is not recovered.
                Ok((payload, product)) if payload.is_empty() => {
                    let name = product.get("name").and_then(serde_json::Value::as_str);
                    let error = CloudError::corrupt_product(name.unwrap_or_default(), &product_id);
                    let text = format!(
                        "{} ({})",
                        error.message.as_deref().unwrap_or(""),
                        error.text()
                    );
                    vec![notice(text), Effect::Nc2(Nc2Input::Downloaded(None))]
                }
                Ok((payload, product)) => vec![Effect::Nc2Download(Box::new(Nc2Download {
                    product_id,
                    payload,
                    product,
                }))],
                Err(error) => vec![
                    notice(format!("NC2 product download failed: {error}")),
                    Effect::Nc2(Nc2Input::Downloaded(None)),
                ],
            },
        }
    }

    /// A cloud request from the capture bridge.
    pub fn cloud_request(
        &mut self,
        settings: &mut DeviceSettings,
        request: CloudRequest,
        now: Duration,
    ) -> Vec<Effect> {
        self.cloud_job(settings, vec![CloudCall::Nc2(request)], None, now)
    }
}
