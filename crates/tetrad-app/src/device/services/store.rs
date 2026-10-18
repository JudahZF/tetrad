//! Device-side service steps: backup staging, capture keys, the Downloads stores.

use super::*;

/// Library steps around backup jobs, on the device thread (r17-lib MR 1 and 2).
impl<P: super::platform::Platform> super::Device<P> {
    /// Before a backup job runs: a create gets its [`Stage`] plan (the worker copies and
    /// re-keys); a restore takes the library off the device and closes `zenui.db`
    /// (`0x69daac`) so nothing reads or writes the user data during the extract.
    pub(in crate::device) fn prepare_service_job(&mut self, job: ServiceJob) -> ServiceJob {
        let ServiceJob::Backup(job) = job else {
            return job;
        };
        ServiceJob::Backup(match job {
            BackupJob::Create { dest, .. } => BackupJob::Create {
                dest,
                stage: self.stage_plan(),
            },
            BackupJob::CreateLocal { header, stamp, .. } => BackupJob::CreateLocal {
                stage: self.stage_plan(),
                header,
                stamp,
            },
            job @ (BackupJob::Restore { .. } | BackupJob::RestoreLocal { .. }) => {
                let dropped = (self.bridges.services).drop_library_asks("a backup restore");
                self.apply(Ok::<_, super::DeviceError>(dropped));
                if let Some(mut library) = self.state.library.take() {
                    if let Some(Err(error)) = library.database.take().map(StockDb::close) {
                        self.notice(Subsystem::Library, format!("zenui.db close: {error}"));
                    }
                    self.bridges.services.parked_library = Some(library);
                    self.bridges.services.downloads_changed();
                    self.notice(
                        Subsystem::Library,
                        "restoring a backup: library edits, preset saves and settings flushes \
                         wait until it ends"
                            .into(),
                    );
                }
                job
            }
            job => job,
        })
    }

    /// The stage of a create, or `None` (pack the file as it is) without an open database.
    pub(super) fn stage_plan(&mut self) -> Option<Box<Stage>> {
        self.state.library.as_ref()?.database.as_ref()?;
        let database = self.platform.storage().database;
        let keys = (self.capture_ciphers())
            .map(|(global, local)| Handoff::new(CaptureKeys { global, local }));
        Some(Box::new(Stage {
            dir: database.with_file_name(BACKUP_STAGE),
            database,
            keys,
        }))
    }

    /// The seed-only capture cipher (secret mode 0) and this unit's device cipher, as shared
    /// `Arc` handles. The seed comes from the stock ZenUI image, read once per run and only
    /// here (review r18 F15).
    pub(in crate::device) fn capture_ciphers(
        &mut self,
    ) -> Result<(DeviceCipher, DeviceCipher), String> {
        let local = self
            .media
            .as_ref()
            .ok_or("no device media cipher")?
            .cipher()
            .clone();
        Ok((self.capture_seed()?, local))
    }

    /// The DOWNLOADS rows of `kind` (review r28 A1): first the products stored on this unit in
    /// table order (`downloads::stored_rows`), then the queue rows listed this run that are
    /// not stored, in list order. I: the stock merge order is not traced. A stored row loads
    /// on a tap (`0x78d204`: `vt+0x6c` finds it → `0x5b37f0`), a queue row downloads. IRs have
    /// no Downloads table: only queue rows. The views read the stored rows on a worker
    /// (`Self::poll_stored_downloads`); this reads them here.
    pub fn downloads_rows(&self, kind: ProductKind) -> Vec<DownloadsRow> {
        self.probe.enter();
        let table = match kind {
            ProductKind::Preset => Some(DownloadKind::Preset),
            ProductKind::NeuralCapture => Some(DownloadKind::Capture),
            ProductKind::ImpulseResponse => None,
        };
        let db = (self.state.library.as_ref()).and_then(|library| library.database.as_ref());
        let cipher = self.media.as_ref().map(|media| media.cipher());
        let stored = (table.zip(db))
            .map(|(table, db)| downloads::stored_rows(&db.view(), table, cipher))
            .unwrap_or_default();
        let rows = self.with_queue_rows(stored, kind);
        let label = crate::device::probe::Label::new(format_args!("downloads_rows {kind:?}"));
        self.probe.leave(label);
        rows
    }

    /// `stored`, then the queue rows of `kind` listed this run that are not stored.
    pub(in crate::device) fn with_queue_rows(
        &self,
        mut rows: Vec<DownloadsRow>,
        kind: ProductKind,
    ) -> Vec<DownloadsRow> {
        let queued = (self.bridges.services).cloud_folder(kind, BrowseScope::DownloadQueue);
        let mut listed: std::collections::HashSet<String> =
            rows.iter().map(|row| row.id.clone()).collect();
        for row in queued.into_iter().flat_map(|folder| &folder.rows) {
            if listed.insert(row.id.clone()) {
                rows.push(DownloadsRow {
                    id: row.id.clone(),
                    name: row.name.clone(),
                    author: row.author_username.clone(),
                    version: row.version.clone(),
                    hash: None,
                    stored: false,
                });
            }
        }
        rows
    }

    /// Keeps the stored DOWNLOADS rows of the views current (A3): takes a finished worker
    /// read, and starts one after a change of the Downloads tables
    /// ([`Services::downloads_changes`]). Returns whether new rows landed.
    pub(in crate::device) fn poll_stored_downloads(&mut self) -> bool {
        let changes = self.bridges.services.downloads_changes();
        let db = (self.state.library.as_ref()).and_then(|library| library.database.as_ref());
        let source = match db {
            None => downloads::Source::None,
            Some(db) => match db.path() {
                Some(path) => downloads::Source::File(path.to_owned()),
                None => downloads::Source::Open(db),
            },
        };
        let cipher = self.media.as_ref().map(|media| media.cipher());
        (self.bridges.services.stored_downloads).poll(changes, source, cipher)
    }

    /// [`capture_meta_signature`] with this device's seed-only capture cipher.
    pub fn meta_signature(&mut self, metadata: &[u8]) -> Result<Vec<u8>, String> {
        Ok(capture_meta_signature(&self.capture_seed()?, metadata))
    }

    /// The Downloads folder whose "Excessive %1 in Downloads folder" dialog (`0x22bd50`)
    /// shows now (`0x22cab0`), with this device's "Don't show again" flags. `None` without
    /// an open library. Stock checks at boot after the What's New popup and after a restore
    /// reload; the boot check opens the dialog and sends the Control push (`app/mod.rs`,
    /// `app/ui.rs`, `app/control.rs`).
    pub fn excessive_downloads(&self) -> Result<Option<DownloadKind>, tetrad_library::Error> {
        let Some(db) = (self.state.library.as_ref()).and_then(|library| library.database.as_ref())
        else {
            return Ok(None);
        };
        let general = &self.state.settings.settings().general;
        tetrad_library::stock::excessive_downloads(db, |kind| match kind {
            DownloadKind::Capture => general.dont_show_excess_capture_downloads,
            DownloadKind::Preset => general.dont_show_excess_preset_downloads,
        })
    }

    /// The seed-only capture cipher (secret mode 0).
    pub(super) fn capture_seed(&mut self) -> Result<DeviceCipher, String> {
        if self.bridges.services.capture_seed.is_none() {
            let image = (self.platform)
                .dsp_file(super::platform::DspFile::ZenUi)
                .map_err(|error| error.to_string())?;
            let seed = tetrad_media::factory::FactoryIrSeed::from_stock_zenui(&image)
                .map_err(|error| error.to_string())?;
            self.bridges.services.capture_seed = Some(DeviceCipher::seed(&seed));
        }
        (self.bridges.services.capture_seed.clone()).ok_or_else(|| "no capture seed".into())
    }

    /// The seed-only capture cipher (secret mode 0) from the platform, in place of the read of
    /// the stock ZenUI image (`keep_capture_seed`): a handle, never key bytes.
    pub fn set_capture_seed(&mut self, seed: DeviceCipher) {
        self.bridges.services.capture_seed = Some(seed);
    }

    /// Takes the capture seed from the ZenUI image the DSP boot reads for its tables, so no
    /// backup or restore reads the 22 MB image again (review r18 F8, F15).
    pub(in crate::device) fn keep_capture_seed(&mut self, image: &[u8]) {
        if let Ok(seed) = tetrad_media::factory::FactoryIrSeed::from_stock_zenui(image) {
            self.bridges.services.capture_seed = Some(DeviceCipher::seed(&seed));
        }
    }

    /// A power request ([`Effect::Power`]) while a restore extracts: held until the job ends,
    /// then run (`Services::backup_done`), so a power-off or restart cannot stop the process
    /// with the user data half extracted (review r19 F3; I: stock runs the extract on the main
    /// thread, so nothing else runs until it ends). The last request wins. Returns the action
    /// that may run now.
    pub(in crate::device) fn power_request(&mut self, action: PowerAction) -> Option<PowerAction> {
        let services = &mut self.bridges.services;
        if !services.restoring() {
            return Some(action);
        }
        if services.held_power.replace(action).is_none() {
            let detail = format!("{action:?} waits until the backup restore ends");
            self.notice(Subsystem::Services, detail);
        }
        None
    }

    /// [`Effect::StoreDownload`]: the folder check and the row (`download_row`: a V1 capture
    /// from the seed-only cipher, NC2 under the DSP key, the row in the device cipher), then
    /// its Downloads table (`store_download`: one transaction, stock `REPLACE`; a capture only
    /// into a local store). Without a library (also while a restore holds it) nothing is
    /// stored. A stored product goes to Cortex Control (`0x250634`, r27-control MR 2); a
    /// [`DownloadOrigin::Queue`] product then leaves the download queue (`0x1e4db4` →
    /// `0x718204`). Review r27 R1: an NC editor DOWNLOAD ends its job here. Review r28 A2: an
    /// editor or CLOUD row download sends no queue `DELETE` (I: no call to `0x718204` found in
    /// `0x1d9954` or `0x719f90`; not fully traced).
    pub fn store_cloud_download(&mut self, download: CloudDownload) {
        use tetrad_library::stock::{DownloadKeys, StoredDownload, download_row, store_download};
        let CloudDownload {
            product,
            payload,
            origin,
        } = download;
        if let DownloadOrigin::Slot {
            setlist,
            slot,
            replace,
        } = &origin
        {
            return self.store_slot_preset(&product, &payload, setlist, *slot, replace.as_deref());
        }
        if let DownloadOrigin::Dependency { preset, hash } = &origin {
            return self.store_dependency(&product, &payload, preset, hash);
        }
        if product.kind == ProductKind::ImpulseResponse {
            return self.store_cloud_ir(&product, &payload, origin);
        }
        if product.kind == ProductKind::NeuralCapture
            && matches!(
                origin,
                DownloadOrigin::Cloud | DownloadOrigin::CaptureOverwrite
            )
        {
            let overwrite = origin == DownloadOrigin::CaptureOverwrite;
            return self.store_user_capture(product, payload, overwrite);
        }
        let id = product.id.clone();
        let ciphers = (product.kind == ProductKind::NeuralCapture)
            .then(|| self.capture_ciphers())
            .transpose();
        let nc2 = self
            .media
            .as_ref()
            .and_then(|media| media.nc2_cipher().cloned());
        let row = ciphers
            .map_err(|error| error.to_string())
            .and_then(|ciphers| {
                let keys = ciphers.as_ref().map(|(global, local)| DownloadKeys {
                    global,
                    local,
                    nc2: nc2.as_ref(),
                });
                download_row(&product, &payload, keys.as_ref()).map_err(|error| error.to_string())
            });
        // Review r33 F1 (O), as for a slot preset's capture.
        let row = match (&origin, row) {
            (DownloadOrigin::QueueDependency { hash, .. }, Ok((_, row)))
                if row.hash.as_deref() != Some(hash) =>
            {
                Err(format!(
                    "its capture hash {} is not the hash {hash} the preset names",
                    row.hash.unwrap_or_default()
                ))
            }
            (_, row) => row,
        };
        let db = (self.state.library.as_mut()).and_then(|library| library.database.as_mut());
        let stored = match (row, db) {
            (Err(error), _) => Err(error),
            (Ok(_), None) => Err("the library is closed".to_owned()),
            (Ok((kind, row)), Some(db)) => store_download(db, kind, &row)
                .map(|stored| (kind, stored, row.id))
                .map_err(|error| error.to_string()),
        };
        let detail = match &stored {
            Ok((_, StoredDownload::Added, _)) => format!("cloud product {id} added to Downloads"),
            Ok((_, StoredDownload::Replaced, _)) => {
                format!("cloud product {id} updated in Downloads")
            }
            Err(error) => format!("cloud product {id} not stored: {error}"),
        };
        self.notice(Subsystem::Library, detail);
        if let DownloadOrigin::Editor(hash) = &origin {
            let outcome = match stored {
                Ok(_) => DownloadOutcome::Stored,
                Err(_) => DownloadOutcome::Failed,
            };
            (self.bridges.services).end_capture_download(hash.clone(), outcome);
        }
        let queue_dependency = matches!(origin, DownloadOrigin::QueueDependency { .. });
        let Ok((kind, _, row_id)) = stored else {
            if queue_dependency {
                // `0x73bf8c`: the preset is not stored.
                let now = tetrad_dsp_host::session::Clock::now(&self.clock);
                let services = &mut self.bridges.services;
                let reason = "one of its captures was not stored";
                let effects = services.stop_queue_preset(reason, &mut self.state.settings, now);
                self.apply(Ok::<_, ServiceError>(effects));
            }
            return;
        };
        self.bridges.services.downloads_changed();
        if kind == DownloadKind::Capture {
            self.refresh_directory_media();
        }
        // `0x71d538` → `0x250634(control, type, id)`: Cortex Control hears the stored row (a
        // capture by its cloud id `+0x30`, a preset by `+4`) while it is connected.
        let pushes = (self.bridges.control).download_stored(&self.state, kind, &row_id);
        self.apply(Ok::<_, ServiceError>(pushes));
        if !(origin == DownloadOrigin::Queue || queue_dependency) {
            return;
        }
        // A queue preset's capture leaves the queue too (`0x7409d0` → `0x1e4db4`), then the
        // walk goes on.
        let call = CloudCall::LeaveQueue { cloud_id: id };
        let now = tetrad_dsp_host::session::Clock::now(&self.clock);
        let services = &mut self.bridges.services;
        let mut effects = services.cloud_job(&mut self.state.settings, vec![call], None, now);
        if queue_dependency {
            effects.extend(services.next_queue_dependency(&mut self.state.settings, now));
        }
        self.apply(Ok::<_, ServiceError>(effects));
    }

    /// A Directory CLOUD or Control 46 capture download into the NC saving destination (the
    /// stored `downloadDestinationFolderNC`, else the user root; `0x1d9954(.., 0x76be70(root,
    /// 2), ..)` → `0x1e5f24` → `0x6f8584` → `0x1e2bf4`), sorted and stored by
    /// [`tetrad_library::stock::store_cloud_capture`] (re-r34 §2). A factory hash is left alone
    /// (status 9; a `factory.db` that does not open counts as no factory hash: the worst case
    /// is a user copy). Statuses 5 and 8 wait in [`Services::capture_question`]; after
    /// OVERWRITE (`overwrite`) the download is sorted again, and only while it still asks is
    /// the metadata of the user capture with that hash replaced
    /// ([`tetrad_library::stock::overwrite_capture_metadata`]: never its rawdata, never another
    /// capture). O: a second question while one waits is not stored (a notice).
    pub(super) fn store_user_capture(
        &mut self,
        product: CloudProduct,
        payload: ProductPayload,
        overwrite: bool,
    ) {
        use tetrad_library::stock::{
            CloudCapture, DownloadKeys, download_row, overwrite_capture_metadata,
            saving_destination, store_cloud_capture,
        };
        let nc2 = (self.media.as_ref()).and_then(|media| media.nc2_cipher().cloned());
        let row = self.capture_ciphers().and_then(|(global, local)| {
            let keys = DownloadKeys {
                global: &global,
                local: &local,
                nc2: nc2.as_ref(),
            };
            download_row(&product, &payload, Some(&keys)).map_err(|error| error.to_string())
        });
        let destination = (self.state.settings.settings().library)
            .download_destination_nc
            .clone();
        let factory = self.platform.storage().factory.host().join("factory.db");
        let factory =
            StockDb::open_factory(&factory, tetrad_storage::stock::DbLimits::default()).ok();
        let db = (self.state.library.as_mut()).and_then(|library| library.database.as_mut());
        let sorted = match (row, db, self.media.as_ref()) {
            (Err(error), ..) => Err(error),
            (Ok(_), None, _) | (Ok(_), _, None) => Err("the library is closed".to_owned()),
            (Ok((_, row)), Some(db), Some(media)) => {
                let hash = row.hash.clone().unwrap_or_default();
                let in_factory = (factory.as_ref())
                    .and_then(|factory| factory.view().product(&hash).ok().flatten())
                    .is_some_and(|found| {
                        found.product_type == tetrad_storage::stock::ProductType::Capture
                    });
                let kind = tetrad_storage::stock::ProductType::Capture;
                saving_destination(&db.view(), kind, destination.as_deref())
                    .and_then(|folder| {
                        let (name, tags) = (&product.name, &product.tags);
                        store_cloud_capture(
                            db,
                            &row,
                            name,
                            tags,
                            folder,
                            in_factory,
                            media.cipher(),
                        )
                    })
                    .and_then(|sorted| match (sorted, overwrite) {
                        (CloudCapture::Ask { .. }, true) => {
                            let metadata = row.metadata.as_deref().unwrap_or_default();
                            overwrite_capture_metadata(db, &hash, metadata).map(|()| None)
                        }
                        (sorted, _) => Ok(Some(sorted)),
                    })
                    .map_err(|error| error.to_string())
            }
        };
        let (id, name) = (&product.id, &product.name);
        let detail = match &sorted {
            Ok(None) => format!("cloud capture {name} ({id}) replaced the details of its capture"),
            Ok(Some(CloudCapture::Added)) => {
                format!("cloud capture {name} ({id}) added to the Captures Library")
            }
            Ok(Some(CloudCapture::Linked)) => {
                format!("cloud capture {name} ({id}) added to the saving destination")
            }
            Ok(Some(CloudCapture::Present)) => {
                format!("cloud capture {name} ({id}) is in the saving destination")
            }
            Ok(Some(CloudCapture::Factory)) => {
                format!("cloud capture {name} ({id}) is a factory capture; not stored")
            }
            // `0x464e30(name, 1)`: no list dialog (r39-ui); in a batch, one text per batch.
            Ok(Some(CloudCapture::NameTaken | CloudCapture::NameOfOther)) => {
                match self.bridges.services.capture_name_taken(id, name) {
                    Some(text) => text,
                    None => return,
                }
            }
            Ok(Some(CloudCapture::Ask { existing })) => {
                format!("cloud capture {name} ({id}) waits: {existing} has its hash")
            }
            Err(error) => format!("cloud capture {name} ({id}) not stored: {error}"),
        };
        if let Ok(Some(CloudCapture::Ask { existing })) = sorted.clone() {
            let question = CaptureQuestion {
                name: name.clone(),
                existing,
            };
            let download = CloudDownload {
                product: product.clone(),
                payload,
                origin: DownloadOrigin::Cloud,
            };
            if !self.bridges.services.ask_capture(question, download) {
                let detail = format!("cloud capture {name} ({id}) not stored: a question is open");
                return self.notice(Subsystem::Library, detail);
            }
        }
        self.notice(Subsystem::Library, detail);
        if matches!(
            sorted,
            Ok(None | Some(CloudCapture::Added | CloudCapture::Linked))
        ) {
            self.refresh_directory_media();
        }
    }

    /// Cloud presets into empty setlist slots (UI Multi Select DOWNLOAD, Cortex Control 46
    /// DOWNLOAD with `to_folder`): [`Self::overwrite_presets`] that refuses an occupied slot.
    pub fn download_presets(
        &mut self,
        setlist: &str,
        items: Vec<(String, usize)>,
        now: Duration,
    ) -> Result<Vec<Effect>, super::DeviceError> {
        self.slot_downloads(setlist, items, false, now)
    }

    /// Cloud presets into setlist slots after OVERWRITE of "Overwrite Existing Presets?"
    /// (`0x1dcf2c` → `0x46485c(name, 1)`; OVERWRITE `0x1d8a04` runs the same store `0x742990`
    /// as an empty slot). Every target is checked before the first request (O, user data
    /// first, the PASTE rules): the setlist exists, each slot is in range and used once, and
    /// no target slot holds the loaded preset. Each item keeps the path its slot holds now
    /// ([`SlotDownload::replace`]); its store goes ahead only while the slot still holds it.
    /// Then [`Services::download_presets`]. `setlist` is the setlist name; `items` are (cloud
    /// product id, slot) in order.
    pub fn overwrite_presets(
        &mut self,
        setlist: &str,
        items: Vec<(String, usize)>,
        now: Duration,
    ) -> Result<Vec<Effect>, super::DeviceError> {
        self.slot_downloads(setlist, items, true, now)
    }

    pub(super) fn slot_downloads(
        &mut self,
        setlist: &str,
        items: Vec<(String, usize)>,
        overwrite: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, super::DeviceError> {
        let library = (self.state.library.as_ref())
            .ok_or(super::DeviceError::Unavailable("the preset library"))?;
        let slots = library.presets.slots(setlist)?;
        let loaded = (self.state.current.as_ref()).and_then(|current| current.path.as_deref());
        let mut checked = Vec::with_capacity(items.len());
        for (index, (id, slot)) in items.iter().enumerate() {
            if id.is_empty() {
                return Err(
                    ServiceError::Unsupported("a cloud preset without a product id").into(),
                );
            }
            let held = match slots.get(*slot) {
                None => return Err(tetrad_library::Error::SlotRange(*slot).into()),
                Some(held) => held.clone(),
            };
            if held.is_some() && !overwrite {
                return Err(tetrad_library::Error::SlotOccupied(*slot).into());
            }
            if held.is_some() && held.as_deref() == loaded {
                return Err(ServiceError::Unsupported(
                    "a preset download onto the loaded preset's slot",
                )
                .into());
            }
            if items[..index].iter().any(|(_, other)| other == slot) {
                return Err(
                    ServiceError::Unsupported("a preset download with a slot used twice").into(),
                );
            }
            checked.push(SlotDownload {
                id: id.clone(),
                slot: *slot,
                replace: held,
            });
        }
        let backends = self.platform.services();
        let settings = &mut self.state.settings;
        let services = &mut self.bridges.services;
        Ok(services.download_presets(backends, settings, setlist.to_owned(), checked, now)?)
    }

    /// The store of one [`DownloadOrigin::Slot`] download (`0x1dcf2c` → `0x742990`):
    /// [`tetrad_library::stock::StockLibrary::store_cloud_preset`], then the next item of the run.
    /// An OVERWRITE store is refused when the slot now holds the loaded preset (O) or no
    /// longer holds `replace` (the library refuses it); a refused item stops the run.
    pub(super) fn store_slot_preset(
        &mut self,
        product: &CloudProduct,
        payload: &ProductPayload,
        setlist: &str,
        slot: usize,
        replace: Option<&str>,
    ) {
        let id = &product.id;
        let loaded = (self.state.current.as_ref()).and_then(|current| current.path.as_deref());
        let stored = match self.state.library.as_mut() {
            Some(_) if replace.is_some() && replace == loaded => {
                Err("the slot holds the loaded preset".to_owned())
            }
            Some(library) => (library.presets)
                .store_cloud_preset(setlist, slot, product, payload, replace)
                .map_err(|error| error.to_string()),
            None => Err("the library is closed".to_owned()),
        };
        let number = slot + 1;
        let detail = match &stored {
            Ok(saved) => format!(
                "cloud preset {id} stored in {setlist} slot {number}: {}",
                saved.value
            ),
            Err(error) => {
                format!("cloud preset {id} not stored in {setlist} slot {number}: {error}")
            }
        };
        self.notice(Subsystem::Library, detail);
        let mut effects = Vec::new();
        if let Ok(saved) = &stored {
            // Review r30 C2: the preset is in its slot; a failed clean-up is a notice.
            let tail = (saved.tail_error.as_ref()).map(|error| {
                format!(
                    "{}: the replaced preset was not fully removed: {error}",
                    saved.value
                )
            });
            let stale = (saved.search_index.as_ref()).map(ToString::to_string);
            let notices = tail.into_iter().chain(stale);
            effects.extend(notices.map(|detail| Effect::Notice(Subsystem::Library, detail)));
            effects.push(Effect::RefreshDirectory);
        }
        let now = tetrad_dsp_host::session::Clock::now(&self.clock);
        let services = &mut self.bridges.services;
        effects.extend(services.preset_stored(&mut self.state.settings, stored.is_ok(), now));
        self.apply(Ok::<_, ServiceError>(effects));
    }

    /// A cloud IR into the IR saving destination (`0x79992c` → `0x799134(.., 0x76be70(root,
    /// 1), -1)`; the stored `downloadDestinationFolderIR`, else the IR root) through
    /// [`tetrad_library::stock::import_cloud_ir`]. An IR with the same samples or a user IR with
    /// the same name is not written (O). A [`DownloadOrigin::Queue`] IR that the library now
    /// holds leaves the download queue (O, as the other queue products; stock not traced for
    /// IRs).
    pub(super) fn store_cloud_ir(
        &mut self,
        product: &CloudProduct,
        payload: &ProductPayload,
        origin: DownloadOrigin,
    ) {
        use tetrad_library::stock::{CloudIr, import_cloud_ir, saving_destination};
        const WAV: tetrad_media::ir::IrLimits = tetrad_media::ir::IrLimits {
            max_asset_bytes: PAYLOAD_LIMIT as usize,
            max_chunks: 4096,
        };
        let destination = self
            .state
            .settings
            .settings()
            .library
            .download_destination_ir
            .clone();
        let db = (self.state.library.as_mut()).and_then(|library| library.database.as_mut());
        let stored = match (payload, db, self.media.as_ref()) {
            (ProductPayload::Raw(wav), Some(db), Some(media)) => {
                let folder = saving_destination(
                    &db.view(),
                    tetrad_storage::stock::ProductType::Ir,
                    destination.as_deref(),
                );
                folder
                    .and_then(|folder| {
                        import_cloud_ir(db, folder, wav, product, media.cipher(), WAV)
                    })
                    .map_err(|error| error.to_string())
            }
            (ProductPayload::Raw(_), None, _) => Err("the library is closed".to_owned()),
            (ProductPayload::Raw(_), _, None) => Err("no device media cipher".to_owned()),
            (other, ..) => Err(format!("{other:?} is not an IR payload")),
        };
        let id = &product.id;
        let detail = match &stored {
            Ok(CloudIr::Added(key)) => format!("cloud IR {id} added to the IR library: {key}"),
            Ok(CloudIr::Present { name, .. }) => {
                format!("cloud IR {id} not stored: the library holds it as {name}")
            }
            Ok(CloudIr::NameTaken) => {
                format!(
                    "cloud IR {id} not stored: an IR named {} exists",
                    product.name
                )
            }
            Err(error) => format!("cloud IR {id} not stored: {error}"),
        };
        self.notice(Subsystem::Library, detail);
        if matches!(stored, Ok(CloudIr::Added(_))) {
            self.refresh_directory_media();
        }
        let held = matches!(stored, Ok(CloudIr::Added(_) | CloudIr::Present { .. }));
        if origin != DownloadOrigin::Queue || !held {
            return;
        }
        let call = CloudCall::LeaveQueue {
            cloud_id: id.clone(),
        };
        let now = tetrad_dsp_host::session::Clock::now(&self.clock);
        let services = &mut self.bridges.services;
        let effects = services.cloud_job(&mut self.state.settings, vec![call], None, now);
        self.apply(Ok::<_, ServiceError>(effects));
    }

    /// The store of one capture dependency of the held slot preset (`0x743654` "Downloaded
    /// capture" → `0x1d698c` → `0x6f8584` → `0x1d8e9c`), then the next dependency or the
    /// preset ([`Services::next_dependency`]; C: the walk goes on whether it stored or not).
    /// The row is the Downloads capture row (`download_row`, the same ciphers), added by
    /// [`tetrad_library::stock::store_library_capture`]. Not added, as `0x6f8584` sorts it: C, a
    /// hash the library holds (statuses 5–8, `0x1d8bac`); C, a name another user capture holds
    /// (status 4: stock opens `0x464e30(name, 1)`, "…names that already exist on your %1 and
    /// will not be downloaded"; Tetrad a notice, O; I: exact text, as `capture_named`). A factory
    /// hash (status 9) is added like any other (C: `0x1d8e9c` special-cases status 4 only, and
    /// `0x1d8bac` adds a key the user store lacks; review r33 F3).
    ///
    /// Review r33 F1 (O): the row's hash must be `expected`, the key the preset names (stock
    /// keys the add by record `+4`; how a download sets it is not traced, `0x7240b0` sets the
    /// cloud id first). On a mismatch nothing is written and the run stops, so the preset is
    /// not stored with a capture the library does not hold under its key.
    pub(super) fn store_dependency(
        &mut self,
        product: &CloudProduct,
        payload: &ProductPayload,
        preset: &str,
        expected: &str,
    ) {
        use tetrad_library::stock::{
            DownloadKeys, LibraryCapture, download_row, store_library_capture,
        };
        let nc2 = (self.media.as_ref()).and_then(|media| media.nc2_cipher().cloned());
        let row = self.capture_ciphers().and_then(|(global, local)| {
            let keys = DownloadKeys {
                global: &global,
                local: &local,
                nc2: nc2.as_ref(),
            };
            download_row(product, payload, Some(&keys)).map_err(|error| error.to_string())
        });
        let row = row.and_then(|(kind, row)| match row.hash.as_deref() {
            Some(hash) if hash == expected => Ok((kind, row)),
            other => Err(format!(
                "its capture hash {} is not the hash {expected} the preset names",
                other.unwrap_or_default()
            )),
        });
        let mismatch = row.is_err();
        let db = (self.state.library.as_mut()).and_then(|library| library.database.as_mut());
        let stored = match (row, db, self.media.as_ref()) {
            (Err(error), ..) => Err(error),
            (Ok(_), None, _) | (Ok(_), _, None) => Err("the library is closed".to_owned()),
            (Ok((_, row)), Some(db), Some(media)) => {
                let hash = row.hash.clone().unwrap_or_default();
                let taken = || -> Result<bool, super::DeviceError> {
                    let view = db.view();
                    Ok(view.product(&hash)?.is_none()
                        && media.capture_named(&view, &product.name, &hash)?)
                };
                match taken() {
                    Err(error) => Err(error.to_string()),
                    Ok(true) => Ok("has the name of another capture in the library; not stored"),
                    Ok(false) => match store_library_capture(db, &row) {
                        Ok(LibraryCapture::Added) => Ok("added to the Captures Library"),
                        Ok(LibraryCapture::Present) => Ok("is in the Captures Library"),
                        Err(error) => Err(error.to_string()),
                    },
                }
            }
        };
        let (id, name) = (&product.id, &product.name);
        let detail = match &stored {
            Ok(outcome) => format!("cloud preset {preset}: Neural Capture {name} ({id}) {outcome}"),
            Err(error) => {
                format!("cloud preset {preset}: Neural Capture {name} ({id}) not stored: {error}")
            }
        };
        self.notice(Subsystem::Library, detail);
        if stored == Ok("added to the Captures Library") {
            self.refresh_directory_media();
        }
        let now = tetrad_dsp_host::session::Clock::now(&self.clock);
        let services = &mut self.bridges.services;
        let effects = match mismatch {
            true => services.stop_preset_downloads("a capture is not the one its preset names"),
            false => services.next_dependency(&mut self.state.settings, now),
        };
        self.apply(Ok::<_, ServiceError>(effects));
    }

    /// The settings flush, held while a restore owns the user data: during the extract it
    /// waits one more delay; after a successful extract, until the reload reads the restored
    /// files ([`Self::reload_after_restore`]), it converts their levels and writes only what
    /// stock keeps from memory ([`keep_device_values_after_restore`]), so no in-memory value
    /// is written over the restored `ZenjackConfig.ini` (review r18 F1, F3).
    pub(in crate::device) fn flush_settings(
        &mut self,
    ) -> Result<(), tetrad_settings::StorageError> {
        let services = &mut self.bridges.services;
        if services.restored {
            let hardware = self.state.settings.hardware();
            if let Some(source) = services.restore_source.take()
                && let Err(error) = convert_backup_gains(self.platform.settings(), source, hardware)
            {
                self.bridges.services.restore_source = Some(source);
                return Err(error);
            }
            let memory = self.state.settings.settings();
            return keep_device_values_after_restore(self.platform.settings(), memory, hardware);
        }
        if services.restoring() {
            self.schedule_settings_flush();
            return Ok(());
        }
        self.state.settings.flush(self.platform.settings())
    }

    /// `0x3fcf64` after a good extract: the in-place reload, with no restart. Stock order,
    /// except that the settings come before the library (I: Tetrad's legacy model-default import
    /// reads them; stock re-reads them with `QSettings::sync` in step 9, after step 7):
    ///
    /// 1. The first flush converts the restored connector levels to this hardware (`0x480bcc`)
    ///    and keeps this device's volume and name (`0x64e73c`, `0x64fc30`).
    /// 2. The settings are read again from the restored files (`0x653d18`, `0x651f78`). The
    ///    files that are not backup members (`PermanentSettings.ini`, `AccountConfig.ini`,
    ///    `PluginLicenses.ini`) keep their values in memory (O: a value not yet flushed stays).
    ///    Missing tempo keys take the catalog defaults, as at boot. Every changed group
    ///    reaches its readers (step 10, [`crate::device::actions::Action::SettingsReloaded`]): DSP globals, Global EQ,
    ///    the live I/O, MIDI, expression calibration, the tuner path and the tempo.
    /// 3. The library comes back and `zenui.db` reopens with the boot media steps (steps 4, 6,
    ///    7, 11: [`Self::reopen_library`], which runs the capture store translation for global
    ///    flagged stores), then the capture corruption check (`0x702f70`).
    /// 4. Preset 0 of the Factory Library loads with no load frame (`0x5b37f0`, step 13), and
    ///    every undo frame goes (`0x647ac0`, step 14).
    /// 5. Cortex Control resyncs, as after a settings reset (I: the stock reload pushes are not
    ///    mapped).
    pub(in crate::device) fn reload_after_restore(&mut self) {
        let flushed = self.flush_settings();
        if let Err(error) = flushed {
            let detail = format!("settings after the restore: {error}");
            self.notice(Subsystem::Settings, detail);
        }
        self.bridges.services.restored = false;
        self.bridges.services.restore_source = None;
        let hardware = self.state.settings.hardware();
        match DeviceSettings::load(self.platform.settings(), hardware) {
            Ok((mut restored, issues)) => {
                if !issues.is_empty() {
                    let detail = format!(
                        "{} restored settings keys fell back to stock defaults",
                        issues.len()
                    );
                    self.notice(Subsystem::Settings, detail);
                }
                let memory = self.state.settings.settings();
                let tempo =
                    (self.state.specs.as_ref()).and_then(crate::device::preset::tempo_defaults);
                restored.update(|settings| {
                    settings.permanent = memory.permanent;
                    settings.cloud_domain = memory.cloud_domain;
                    settings.new_models = memory.new_models.clone();
                    settings.account = memory.account.clone();
                    settings.tokens = memory.tokens.clone();
                    settings.paired_version = memory.paired_version.clone();
                    settings.licenses = memory.licenses.clone();
                    if let Some(tempo) = &tempo {
                        settings.tempo.fill_missing(tempo);
                    }
                });
                let mut before = self.state.settings.clone();
                let events = before.update(|settings| *settings = restored.settings().clone());
                self.state.settings = restored;
                let reloaded = crate::device::actions::reduce(
                    crate::device::actions::Action::SettingsReloaded(events),
                    &mut self.state,
                );
                self.apply(reloaded);
            }
            Err(error) => {
                let detail = format!("restored settings not read: {error}; restarting");
                self.notice(Subsystem::Settings, detail);
                self.execute(Effect::Power(PowerAction::Restart(RestartReason::Direct)));
                return;
            }
        }
        self.reopen_library();
        self.check_capture_corruption();
        let factory = crate::device::PresetRef {
            setlist: Some(crate::device::FACTORY_SETLIST.into()),
            index: 0,
        };
        let loaded = self.load_preset(&factory, false);
        self.report(loaded);
        self.state.history.clear_all();
        // Without a Control session there is nothing to resync (B11 QC run: 17 NotSent
        // notices); a later connect sends the full state anyway.
        if self.bridges.control.connected() {
            let now = tetrad_dsp_host::session::Clock::now(&self.clock);
            let resync = self.bridges.control.resync(false, &mut self.state, now);
            self.apply(resync);
        }
    }

    /// [`Effect::LibraryReopen`] after a failed restore job: the library comes back, and
    /// `0x3fcf64` opens `zenui.db` again with the patch chain and the local re-key, then the
    /// boot steps the library does not own, the search index and the views. A failed open is
    /// tried once more; then the device restarts, so the library is never left closed for the
    /// run (review r18 F7).
    pub(in crate::device) fn reopen_library(&mut self) {
        let Some(library) = self.bridges.services.parked_library.take() else {
            return;
        };
        self.state.library = Some(library);
        let path = self.platform.storage().database;
        let limits = tetrad_storage::stock::DbLimits::default();
        let ciphers = self.capture_ciphers();
        let keys = ciphers
            .as_ref()
            .ok()
            .map(|(global, local)| tetrad_library::stock::CaptureCiphers { global, local });
        let reopened = tetrad_library::stock::reopen_after_restore(&path, limits, keys)
            .or_else(|_| tetrad_library::stock::reopen_after_restore(&path, limits, keys));
        let reopened = match reopened {
            Ok(reopened) => reopened,
            Err(error) => {
                let detail = format!(
                    "zenui.db could not be reopened after the restore ({error}); restarting"
                );
                self.notice(Subsystem::Library, detail);
                self.execute(Effect::Power(PowerAction::Restart(RestartReason::Direct)));
                return;
            }
        };
        let rekey = match (&reopened.rekeyed, &ciphers) {
            (Some(Ok(_)), _) => "captures re-keyed".to_owned(),
            (Some(Err(error)), _) => format!("capture re-key failed: {error}"),
            (None, Err(error)) => format!("captures not re-keyed: {error}"),
            (None, Ok(_)) => "captures not re-keyed".to_owned(),
        };
        let detail = format!(
            "zenui.db reopened after restore (created {}, {} patches, {rekey})",
            reopened.created,
            reopened.upgraded.len()
        );
        self.notice(Subsystem::Library, detail);
        if let Err(error) = &reopened.maintenance {
            let detail = format!("zenui.db boot maintenance after restore: {error}");
            self.notice(Subsystem::Library, detail);
        }
        let mut db = reopened.db;
        self.import_legacy_model_defaults(&mut db);
        // The model presets may differ (picker list, Control READ list).
        self.state.model_preset_revision += 1;
        if let Some(library) = self.state.library.as_mut() {
            library.database = Some(db);
        }
        self.bridges.services.downloads_changed();
        self.migrate_boot_media();
        let index =
            (self.state.library.as_mut()).map(|library| library.presets.rebuild_search_index());
        if let Some(Err(error)) = index {
            self.notice(Subsystem::Library, format!("search index: {error}"));
        }
        self.refresh_directory_media();
    }
}
