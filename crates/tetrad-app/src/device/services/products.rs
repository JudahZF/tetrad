//! Cloud products: folders, downloads, the download queue and the capture download question.

use super::*;

impl Services {
    /// One cloud product on the cloud worker (`0x720ebc`); needs time and a signed-in account.
    /// The row is built and stored on the device thread ([`Effect::StoreDownload`]). The listed
    /// folders give the origin: a product listed in a DOWNLOADS folder goes into its Downloads
    /// table (`0x71d538` → `0x7eb270`) and leaves the queue after its store; any other does not
    /// leave it (review r28 A2; I: the request has no scope, so a product listed in both CLOUD
    /// and DOWNLOADS leaves the queue), and a CLOUD capture goes into the NC saving destination
    /// (re-r34 §2). An IR has no Downloads table: the device stores it into the IR saving
    /// destination (`0x79992c` → `0x799134`, `Device::store_cloud_download`).
    pub fn download_product(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        product_id: String,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let queued = (self.cloud_folders.iter())
            .filter(|(_, folder)| folder.rows.iter().any(|row| row.id == product_id))
            .any(|((_, scope), _)| *scope == BrowseScope::DownloadQueue);
        let origin = match queued {
            true => DownloadOrigin::Queue,
            false => DownloadOrigin::Cloud,
        };
        self.request_product(backends, settings, product_id, origin, now)
    }

    /// Cortex Control CloudProduct (46) DOWNLOAD of one key (`0xf3f24`): a capture with no
    /// `to_folder` goes from the cloud captures folder into the NC saving destination
    /// (`0x1e8a70` → `0x1d9954`, the Directory capture path, re-r34 §2); an IR goes into the IR
    /// saving destination (`0x799134`, as a Directory IR). A preset needs its `to_folder` setlist
    /// slot (`0x1dd61c`, [`super::Device::download_presets`]): refused here.
    pub fn control_download(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        kind: ProductKind,
        product_id: String,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        match kind {
            ProductKind::NeuralCapture | ProductKind::ImpulseResponse => {
                self.request_product(backends, settings, product_id, DownloadOrigin::Cloud, now)
            }
            ProductKind::Preset => Err(ServiceError::Unsupported(
                "a cloud preset download needs its setlist slot (Device::download_presets)",
            )),
        }
    }

    /// A Multi Select DOWNLOAD of cloud captures or IRs (`0x7867c4` → `0x847620`, per row
    /// `0x79992c`, the batch flag set): each id as [`Self::download_product`], as one batch
    /// for [`Self::capture_name_taken`] and one run ([`Self::bulk_run`]; its kind is the
    /// listed folder of the first id, else captures). An empty list starts nothing; a running
    /// run refuses it.
    pub fn download_products(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        ids: Vec<String>,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.bulk_running() {
            return Err(ServiceError::NotReady(RUN_IN_PROGRESS));
        }
        self.upload_ready(backends, settings, now)?;
        let kind = (ids.first())
            .and_then(|first| {
                (self.cloud_folders.iter())
                    .find(|(_, folder)| folder.rows.iter().any(|row| row.id == *first))
            })
            .map_or(ProductKind::NeuralCapture, |((kind, _), _)| *kind);
        let mut effects = Vec::new();
        for id in &ids {
            effects.extend(self.download_product(backends, settings, id.clone(), now)?);
        }
        self.start_download_run(kind, ids, false);
        Ok(effects)
    }

    /// The batch of a multi-key download and its run (`0x847620`: `+0x55`, the stop flag
    /// cleared, the batch answers reset `0x464830`). A run whose downloads all ended already
    /// (dropped at once) ends here.
    fn start_download_run(&mut self, kind: ProductKind, ids: Vec<String>, from_control: bool) {
        let run = (!ids.is_empty()).then(|| self.next_run(ids.len(), from_control));
        self.download_batch = Some(DownloadBatch {
            kind,
            ids,
            told: false,
            run,
            dropped: 0,
        });
        self.check_download_run();
    }

    /// Cortex Control CloudProduct (46) DOWNLOAD of more capture or IR keys (`0x786264` →
    /// `0x847620`): each key as [`Self::control_download`], as one batch for
    /// [`Self::capture_name_taken`] and one run ([`Self::bulk_run`]). A preset kind (setlist
    /// slots, `super::Device::download_presets`), an empty list and a running run are refused
    /// before any request.
    pub fn control_downloads(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        kind: ProductKind,
        ids: Vec<String>,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if ids.is_empty() {
            return Err(ServiceError::Unsupported("a cloud download without a key"));
        }
        if kind == ProductKind::Preset {
            return Err(ServiceError::Unsupported(
                "a cloud preset download needs its setlist slot (Device::download_presets)",
            ));
        }
        if self.bulk_running() {
            return Err(ServiceError::NotReady(RUN_IN_PROGRESS));
        }
        self.upload_ready(backends, settings, now)?;
        let mut effects = Vec::new();
        for id in &ids {
            effects.extend(self.control_download(backends, settings, kind, id.clone(), now)?);
        }
        self.start_download_run(kind, ids, true);
        Ok(effects)
    }

    /// The text for a cloud capture `name` that another user capture holds (`0x464e30(name,
    /// 1)`, C): a single download gets its own text; in a batch ([`Self::download_products`],
    /// [`Self::control_downloads`]; stock `0x843c90`) the batch text shows once
    /// (`DAT_0154af41`, cleared at each batch start `0x464830`), then `None`. Tetrad shows the
    /// text as a notice, not a dialog (O).
    pub fn capture_name_taken(&mut self, product_id: &str, name: &str) -> Option<String> {
        match &mut self.download_batch {
            Some(batch) if batch.ids.iter().any(|id| id == product_id) => {
                (!std::mem::replace(&mut batch.told, true)).then(|| {
                    "Some of the Neural Captures you selected have names that already exist on \
                     your Quad Cortex and will not be downloaded."
                        .to_owned()
                })
            }
            _ => Some(format!(
                "A Neural Capture named {name} already exists on your Quad Cortex. The capture \
                 must be renamed either on cloud or locally on your Quad Cortex."
            )),
        }
    }

    /// Cloud presets into setlist slots: the Directory Multi Select DOWNLOAD
    /// (`multiselectDownloadSlot` `0x794910` → `0x795b18` → per item `0x84d8f0` → `0x1dd61c`)
    /// and Cortex Control 46 DOWNLOAD with `to_folder` (`0x248838` → `0x1dd61c`). One cloud job
    /// per item, in order; the next starts when the device stored the last one
    /// ([`Self::preset_stored`]; I: `Multiselection::consecutivePastePresetsDone` starts the
    /// next item from the done callback). An item whose download is out is skipped (`0x1dd61c`:
    /// transfer state 1 → the done callback). The first failed download or store stops the
    /// rest, as PASTE does (O). One run at a time (O). The device checks every slot first
    /// (`super::Device::download_presets`). More than one item is a run ([`Self::bulk_run`],
    /// `0x786ac4` → `0x84e68c`; I: a single item is the Control one-key path `0x1e8a70`, no
    /// run); a running run refuses it.
    pub fn download_presets(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        setlist: String,
        items: impl IntoIterator<Item = impl Into<SlotDownload>>,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.preset_downloads.is_some() {
            return Err(ServiceError::NotReady("a preset download is in progress"));
        }
        self.upload_ready(backends, settings, now)?;
        let waiting: VecDeque<SlotDownload> = items.into_iter().map(Into::into).collect();
        if waiting.len() > 1 && self.bulk_running() {
            return Err(ServiceError::NotReady(RUN_IN_PROGRESS));
        }
        let run = (waiting.len() > 1).then(|| self.next_run(waiting.len(), true));
        self.preset_downloads = Some(PresetBatch {
            setlist,
            run,
            total: waiting.len(),
            waiting,
            current: None,
            held: None,
            stored: 0,
        });
        Ok(self.next_preset_download(settings, now))
    }

    /// The device stored (or failed to store) the current item of [`Self::download_presets`]:
    /// the next one starts, or the run stops.
    pub fn preset_stored(
        &mut self,
        settings: &mut DeviceSettings,
        stored: bool,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(batch) = self.preset_downloads.as_mut() else {
            return Vec::new();
        };
        batch.current = None;
        if !stored {
            return self.stop_preset_downloads("its store failed");
        }
        batch.stored += 1;
        self.next_preset_download(settings, now)
    }

    /// A downloaded product on its way to the device store. A preset of the slot run first
    /// fetches its Neural Capture dependencies (`0x742990` → `0x740b84` → `0x74012c`): from
    /// the back of `dependencies.product`, each entry that is not `missing`, is `visible` and
    /// has an id, one at a time from the cloud captures folder (root `+0x2c`, `vt+0x48` =
    /// `0x720ebc`); the preset is stored after the last one ([`Self::next_dependency`]). An
    /// entry it skips is a notice (O; stock shows the "Neural Capture(s) missing" /
    /// "unavailable" dialogs of `0x742378` only for a single download). O: an id listed twice
    /// downloads once.
    ///
    /// A queue preset (a DOWNLOADS row or the Control 45 walk, origin `Queue`) fetches its
    /// captures the same way into the captures Downloads table first (re-r34 §4,
    /// [`Self::next_queue_dependency`]).
    pub(super) fn product_downloaded(
        &mut self,
        id: &str,
        download: CloudDownload,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let current = (self.preset_downloads.as_ref())
            .is_some_and(|batch| batch.held.is_none() && batch.current.as_deref() == Some(id));
        let slot = current && matches!(download.origin, DownloadOrigin::Slot { .. });
        let queue = download.origin == DownloadOrigin::Queue
            && download.product.kind == ProductKind::Preset;
        if !(slot || queue) {
            return vec![Effect::StoreDownload(Box::new(download))];
        }
        let (mut effects, mut waiting) = (Vec::new(), VecDeque::new());
        for dependency in download.product.dependencies.iter().rev() {
            if !dependency.fetched() {
                effects.push(notice(format!(
                    "cloud preset {}: Neural Capture {} ({}) is {} on Cortex Cloud; not \
                     downloaded",
                    download.product.id,
                    dependency.name,
                    dependency.hash,
                    if dependency.missing {
                        "missing"
                    } else {
                        "not available"
                    }
                )));
            } else if waiting
                .iter()
                .all(|other: &ProductDependency| other.id != dependency.id)
            {
                waiting.push_back(dependency.clone());
            }
        }
        if waiting.is_empty() {
            effects.push(Effect::StoreDownload(Box::new(download)));
            return effects;
        }
        let held = HeldPreset {
            download: Box::new(download),
            waiting,
            current: None,
        };
        if !slot {
            self.queue_held.push_back(held);
            if self.queue_held.len() == 1 {
                effects.extend(self.next_queue_dependency(settings, now));
            }
            return effects;
        }
        if let Some(batch) = self.preset_downloads.as_mut() {
            batch.held = Some(held);
        }
        effects.extend(self.next_dependency(settings, now));
        effects
    }

    /// The next capture of the first held queue preset goes out (re-r34 §4: from the back,
    /// state 1, `0x7061b0(Downloads NC, 0, id)`); after its last capture the preset goes to the
    /// device store (its own Downloads row and queue `DELETE`) and the next held preset starts.
    /// The device calls this after each good capture store.
    pub fn next_queue_dependency(
        &mut self,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        while let Some(held) = self.queue_held.front_mut() {
            let Some(dependency) = held.waiting.pop_front() else {
                let done = self.queue_held.pop_front().map(|held| held.download);
                effects.extend(done.map(Effect::StoreDownload));
                continue;
            };
            held.current = Some(dependency.id.clone());
            let origin = DownloadOrigin::QueueDependency {
                preset: held.download.product.id.clone(),
                hash: dependency.hash,
            };
            self.downloads_out.push(dependency.id.clone());
            let call = CloudCall::Download {
                product_id: dependency.id,
                origin,
            };
            effects.extend(self.cloud_job(settings, vec![call], None, now));
            break;
        }
        effects
    }

    /// `0x73bf8c`: a capture of the first held queue preset failed, so that preset is not
    /// stored (done 0); the next held preset starts.
    pub(super) fn stop_queue_preset(
        &mut self,
        reason: &str,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(held) = self.queue_held.pop_front() else {
            return Vec::new();
        };
        let id = &held.download.product.id;
        let mut effects = vec![notice(format!("cloud preset {id} not stored: {reason}"))];
        effects.extend(self.next_queue_dependency(settings, now));
        effects
    }

    /// The held queue presets all end unstored (their requests were dropped or lost).
    pub(super) fn drop_queue_presets(&mut self, reason: &str) -> Vec<Effect> {
        (self.queue_held.drain(..))
            .map(|held| {
                let id = &held.download.product.id;
                notice(format!("cloud preset {id} not stored: {reason}"))
            })
            .collect()
    }

    /// The next capture dependency of the held preset goes out; after the last one the
    /// preset goes to the device store. The device calls this after each dependency store,
    /// stored or not (C: `0x1d8e9c` goes on either way).
    pub fn next_dependency(&mut self, settings: &mut DeviceSettings, now: Duration) -> Vec<Effect> {
        let Some(held) = (self.preset_downloads.as_mut()).and_then(|batch| batch.held.as_mut())
        else {
            return Vec::new();
        };
        let Some(dependency) = held.waiting.pop_front() else {
            let held = (self.preset_downloads.as_mut()).and_then(|batch| batch.held.take());
            return (held.into_iter())
                .map(|held| Effect::StoreDownload(held.download))
                .collect();
        };
        held.current = Some(dependency.id.clone());
        let origin = DownloadOrigin::Dependency {
            preset: held.download.product.id.clone(),
            hash: dependency.hash.clone(),
        };
        // `0x74012c` sets the transfer state 1 (`0x7db73c`) before the request.
        self.downloads_out.push(dependency.id.clone());
        let call = CloudCall::Download {
            product_id: dependency.id,
            origin,
        };
        self.cloud_job(settings, vec![call], None, now)
    }

    pub(super) fn next_preset_download(
        &mut self,
        settings: &mut DeviceSettings,
        now: Duration,
    ) -> Vec<Effect> {
        let Some(batch) = self.preset_downloads.as_mut() else {
            return Vec::new();
        };
        let stopped = batch.run.is_some_and(|run| run.cancel);
        let next = loop {
            match batch.waiting.pop_front().filter(|_| !stopped) {
                Some(item) if self.downloads_out.contains(&item.id) => {}
                Some(item) => break item,
                None => {
                    let (stored, total) = (batch.stored, batch.total);
                    let setlist = std::mem::take(&mut batch.setlist);
                    if let Some(run) = self.preset_downloads.take().and_then(|batch| batch.run) {
                        self.end_run(run, ProductKind::Preset, true);
                    }
                    return vec![notice(format!(
                        "preset download into {setlist}: {stored} of {total} stored"
                    ))];
                }
            }
        };
        let SlotDownload {
            id: product_id,
            slot,
            replace,
        } = next;
        batch.current = Some(product_id.clone());
        let origin = DownloadOrigin::Slot {
            setlist: batch.setlist.clone(),
            slot,
            replace,
        };
        self.downloads_out.push(product_id.clone());
        let call = CloudCall::Download { product_id, origin };
        self.cloud_job(settings, vec![call], None, now)
    }

    /// Ends the run of [`Self::download_presets`]: the items not started stay in the cloud.
    pub(super) fn stop_preset_downloads(&mut self, reason: &str) -> Vec<Effect> {
        let Some(batch) = self.preset_downloads.take() else {
            return Vec::new();
        };
        if let Some(run) = batch.run {
            self.end_run(run, ProductKind::Preset, false);
        }
        vec![notice(format!(
            "preset download into {} stopped after {} of {}: {reason}",
            batch.setlist, batch.stored, batch.total
        ))]
    }

    /// Cortex Control CloudProduct (46) DELETE (`0xf330c` → `0x1d916c` → the cloud folder
    /// remove `vt+0x4c` = `0x7180bc`): `DELETE user/products/<id>` on the cloud worker; needs
    /// time and a signed-in account. One id is one request. More ids are one run
    /// (`0x785b74` → `0x84c614` → `0x84be40`, C; [`Self::bulk_run`]): one request at a time in
    /// order, the reply (good or not) starts the next, the stop flag ends it before the next
    /// ([`Self::cancel_bulk_run`]); a running run refuses it. O: a dropped or lost request
    /// ends the run (the rest would fail the same way). This removes products from the user's
    /// cloud account (stock asks nothing on the device): only an explicit Control DELETE may
    /// call it, never an automatic path. Bring-up refuses it without `--persist` and
    /// `--services`.
    pub fn control_delete(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        kind: ProductKind,
        ids: Vec<String>,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if ids.is_empty() || ids.iter().any(String::is_empty) {
            return Err(ServiceError::Unsupported(
                "a cloud delete without a product id",
            ));
        }
        if ids.len() > 1 && self.bulk_running() {
            return Err(ServiceError::NotReady(RUN_IN_PROGRESS));
        }
        self.upload_ready(backends, settings, now)?;
        if let [cloud_id] = ids.as_slice() {
            let call = CloudCall::DeleteProduct {
                kind,
                cloud_id: cloud_id.clone(),
            };
            return Ok(self.cloud_job(settings, vec![call], None, now));
        }
        self.delete_run = Some(DeleteRun {
            run: self.next_run(ids.len(), true),
            kind,
            waiting: ids.into(),
            current: None,
            handled: 0,
        });
        Ok(self.next_delete(settings, now))
    }

    /// A second request for a product whose download is out is ignored (review r28 A7):
    /// stock keeps a transfer state per product id and starts a capture or IR download only
    /// when it is not 1, Downloading (`0x7db480`, `0x1d9954`, `0x79992c`), sets it to 1
    /// (`0x7db73c`), and puts the old state back when the download ends.
    pub(super) fn request_product(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        product_id: String,
        origin: DownloadOrigin,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if self.downloads_out.contains(&product_id) {
            return Ok(Vec::new());
        }
        self.upload_ready(backends, settings, now)?;
        // A single download of a batch id is single again (a batch entry sets its batch after).
        if let Some(batch) = &mut self.download_batch {
            batch.ids.retain(|id| *id != product_id);
        }
        self.downloads_out.push(product_id.clone());
        let call = CloudCall::Download { product_id, origin };
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// The product ids whose download ([`Self::download_product`],
    /// [`Self::control_download`]) is queued or on the worker, oldest first: the products in
    /// transfer state 1, Downloading (CloudProduct 46 READ `0xf5470` sends every product of
    /// the state map `0x7db468`). The upload states (3, 4, 5) by local key:
    /// [`Self::upload_states`].
    pub fn downloads_in_flight(&self) -> &[String] {
        &self.downloads_out
    }

    /// Ends the download of `product_id`: its transfer state goes back (`0x79a384`). The
    /// last download of a batch ends its run.
    pub(super) fn end_download(&mut self, product_id: &str) {
        self.downloads_out.retain(|id| id != product_id);
        self.check_download_run();
    }

    /// Counts the changes of the Downloads tables this run (each store, review r28 A1; the
    /// other changes of `downloads_changed`, review r29 B10): a view of
    /// [`super::Device::downloads_rows`] rebuilds when it changes.
    pub fn downloads_changes(&self) -> u64 {
        self.downloads_changes
    }

    /// The cloud change counter (review r28 A8, review r29 B3, O): it changes when a
    /// Directory cloud folder may have changed (any change of its rows, `more`, `fetching` or
    /// `error`) and with every change of [`Self::downloads_changes`]. While it holds its
    /// value the cloud folders and the DOWNLOADS rows ([`super::Device::downloads_rows`]) are
    /// as they were, so a view needs no rebuild.
    pub fn cloud_changes(&self) -> u64 {
        self.cloud_changes
    }

    /// A Downloads table may have changed (review r29 B10): a store, the boot capture store
    /// re-key or quarantine, the library parked for a restore or back after it. Both
    /// counters move.
    pub(in crate::device) fn downloads_changed(&mut self) {
        self.downloads_changes += 1;
        self.cloud_changes += 1;
    }

    /// The Directory cloud folder of `kind` and `scope` as listed so far; `None` before its
    /// first [`Self::browse_cloud`].
    pub fn cloud_folder(&self, kind: ProductKind, scope: BrowseScope) -> Option<&CloudFolder> {
        (self.cloud_folders.iter())
            .find(|(key, _)| *key == (kind, scope))
            .map(|(_, folder)| folder)
    }

    /// Every change of a cloud folder goes through here: each borrow counts as a change
    /// ([`Self::cloud_changes`]; one that changes nothing costs one view rebuild).
    pub(super) fn cloud_folder_mut(
        &mut self,
        kind: ProductKind,
        scope: BrowseScope,
    ) -> &mut CloudFolder {
        self.cloud_changes += 1;
        let at = match (self.cloud_folders.iter()).position(|(key, _)| *key == (kind, scope)) {
            Some(at) => at,
            None => {
                self.cloud_folders
                    .push(((kind, scope), CloudFolder::default()));
                self.cloud_folders.len() - 1
            }
        };
        &mut self.cloud_folders[at].1
    }

    /// One page of a Directory cloud folder (`CloudFolderModel::fetchMore` `0x433358` →
    /// `listCloudProducts` `0x71aa28`, B10) on the cloud worker; the rows land in
    /// [`Self::cloud_folder`]. `refresh` drops the rows and asks page 0. Else, when the last
    /// page was full, the next page of [`CLOUD_PAGE_ROWS`] rows; a row already listed is not
    /// added twice (O). Stock numbers the page by the rows it holds (`n / 50`); Tetrad counts the
    /// pages that landed (O), because a refused or repeated row is not listed. `scope`
    /// [`BrowseScope::OwnedByMe`] is a CLOUD folder, [`BrowseScope::DownloadQueue`] a Downloads
    /// one (folder `+0x40`). Name order, ascending: the model defaults `+0x58` = `0x21`
    /// (`name`, `0x74df0c`) and `+0x5c` = 1 (ctor `0x42fa5c`, C); Tetrad has no sort control. No
    /// request (and no refresh) while one is out, and none when no more rows follow. A refused
    /// request still leaves the folder with its `error` (review r28 A5, O), so the view can say
    /// why it is empty.
    pub fn browse_cloud(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        kind: ProductKind,
        scope: BrowseScope,
        refresh: bool,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        if let Err(error) = self.upload_ready(backends, settings, now) {
            let folder = self.cloud_folder_mut(kind, scope);
            if !folder.fetching {
                folder.error = Some(error.to_string());
            }
            return Err(error);
        }
        let folder = self.cloud_folder_mut(kind, scope);
        // Review r27 R4: a refresh while a page is out is ignored, so the old page cannot
        // land in the fresh list.
        if folder.fetching || !(refresh || folder.more) {
            return Ok(Vec::new());
        }
        if refresh {
            *folder = CloudFolder::default();
        }
        folder.fetching = true;
        let page = BrowsePage {
            kind,
            scope,
            page: folder.next_page,
            size: CLOUD_PAGE_ROWS,
            sort: CLOUD_SORT.into(),
            ascending: true,
        };
        Ok(self.cloud_job(settings, vec![CloudCall::Browse(page)], None, now))
    }

    /// NC editor DOWNLOAD (`onCaptureErrorButtonPressed` `0x3c892c`): `searchForCapture`
    /// (`0x7bcac8`, `GET products/exists?hash=<hash>`), then, when the first result has a cloud
    /// id and is visible or the account's own (`0x3c5ba4`), that product into the captures
    /// Downloads folder (`0x1d9954` to `0x76be70(root, 2)`; I: the folder of type 2 at root
    /// `+0xd0` is that Downloads folder). Any other result or error is a Services notice (stock
    /// shows the missing-file text again, `0x3c5aa0`). Each request ends once as a
    /// [`CaptureDownloadEnd`]; a refused request ends at once.
    pub fn download_capture(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        hash: String,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        let ready = match hash.is_empty() {
            true => Err(ServiceError::NotReady("NC hash is empty")),
            false => self.upload_ready(backends, settings, now),
        };
        if let Err(error) = ready {
            self.capture_ends.push(CaptureDownloadEnd {
                hash,
                outcome: DownloadOutcome::Failed,
            });
            return Err(error);
        }
        self.capture_jobs.push(hash.clone());
        let call = CloudCall::DownloadCapture { hash };
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// The NC editor DOWNLOAD jobs that ended since the last call, oldest first (review r27
    /// R1: the UI ends its request on these, not on a notice).
    pub fn take_capture_download_ends(&mut self) -> Vec<CaptureDownloadEnd> {
        std::mem::take(&mut self.capture_ends)
    }

    /// The ended NC editor DOWNLOAD jobs not taken yet.
    pub fn capture_download_ends(&self) -> &[CaptureDownloadEnd] {
        &self.capture_ends
    }

    /// Ends one NC editor DOWNLOAD job of `hash` (the oldest, when the same hash is asked
    /// twice).
    pub(super) fn end_capture_download(&mut self, hash: String, outcome: DownloadOutcome) {
        if let Some(at) = self.capture_jobs.iter().position(|job| *job == hash) {
            self.capture_jobs.remove(at);
        }
        self.capture_ends.push(CaptureDownloadEnd { hash, outcome });
    }

    /// The download queue client (Cortex Control ProcessDownloadsQueue 45, `0x1326a8`): the
    /// account's queue of `kind` (`inDownloadedQueue=true`, `0x71aa28`) page by page as the
    /// stock cloud list model's fetchMore (`0x433358`, B10), each item through
    /// `0x722464`, then each product into its Downloads folder; a stored product leaves the
    /// queue (`0x718204`). I: that Control 45 reaches this fetchMore is not traced (stock
    /// refreshes its cloud downloads model, `0x1326a8`).
    pub fn process_downloads_queue(
        &mut self,
        backends: &mut impl ServiceBackends,
        settings: &mut DeviceSettings,
        kind: ProductKind,
        now: Duration,
    ) -> Result<Vec<Effect>, ServiceError> {
        self.upload_ready(backends, settings, now)?;
        let call = CloudCall::DownloadQueue(kind);
        Ok(self.cloud_job(settings, vec![call], None, now))
    }

    /// The cloud capture download that waits for "Neural Capture %3 already exists on your %1
    /// (%2). Would you like to overwrite it?" (`0x465178` mode 1): `name` the cloud name,
    /// `existing` the stored name.
    pub fn capture_question(&self) -> Option<&CaptureQuestion> {
        self.capture_ask.as_ref().map(|ask| &ask.question)
    }

    /// A cloud capture download whose hash the user store holds under another name or tags
    /// (status 5/8) waits for [`Self::capture_question`]. O: while one waits, another is not
    /// stored (`false`); the first stays.
    pub fn ask_capture(&mut self, question: CaptureQuestion, download: CloudDownload) -> bool {
        if self.capture_ask.is_some() {
            return false;
        }
        self.capture_ask = Some(CaptureAsk { question, download });
        true
    }

    /// Review r35 G4 (I: the factory reset `0x4de79c` and the restore end other flows too):
    /// the questions that would write into the library later (the cloud capture question, the
    /// capture upload question) and the held queue presets end, so no later answer writes into
    /// a reset or restored library. A batch upload ends too, with `success: false` (O; its item
    /// in flight completes); any other run stops before its next item (O). Returns the
    /// notices of the dropped queue presets.
    pub(super) fn drop_library_asks(&mut self, reason: &str) -> Vec<Effect> {
        if self.cancel_upload_batch() {
            self.end_batch();
        } else {
            self.cancel_bulk_run();
        }
        self.capture_ask = None;
        (self.capture_upload).take_if(|upload| matches!(upload, CaptureUpload::Asking { .. }));
        (self.preset_upload).take_if(|upload| {
            matches!(upload, PresetUpload::Walk(walk) if matches!(walk.step, WalkStep::Asking(_)))
        });
        self.drop_queue_presets(reason)
    }

    /// The answer to [`Self::capture_question`]: OVERWRITE (`0x1d8f48`) sends the waiting
    /// download back to the device store as [`DownloadOrigin::CaptureOverwrite`]; CANCEL
    /// (`0x1d5ac8`) drops it and writes nothing.
    pub(super) fn answer_capture(&mut self, overwrite: bool) -> Vec<Effect> {
        match (self.capture_ask.take(), overwrite) {
            (Some(ask), true) => vec![Effect::StoreDownload(Box::new(CloudDownload {
                origin: DownloadOrigin::CaptureOverwrite,
                ..ask.download
            }))],
            _ => Vec::new(),
        }
    }
}

/// A downloaded cloud product: its record (`0x7240b0`) and its payload in the folder form
/// (`0x71e820`). The device checks it and builds the Downloads row with its cipher handles
/// (`Device::store_cloud_download`).
#[derive(Debug, Clone, PartialEq)]
pub struct CloudDownload {
    pub product: CloudProduct,
    pub payload: ProductPayload,
    pub origin: DownloadOrigin,
}

/// Who asked for a cloud download. It decides the queue `DELETE` after the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOrigin {
    /// The download queue walk (Cortex Control 45) or a DOWNLOADS row: the stored product
    /// leaves the queue (`0x1e4db4` → `0x718204`).
    Queue,
    /// A Directory CLOUD row (`0x79992c`: preset `0x719f90`, capture `0x1d9954`) or a Cortex
    /// Control 46 DOWNLOAD (`0xf3f24`): no `DELETE` for a product that was never queued
    /// (review r28 A2). A capture goes into the NC saving destination of the user store, not
    /// Downloads (`0x1d9954(.., 0x76be70(root, 2), ..)`; re-r34 §2).
    Cloud,
    /// A Neural Capture that the queue preset `preset` needs (re-r34 §4: `0x7406c0` →
    /// `0x74012c` with the captures Downloads folder): into the Downloads table, then the
    /// capture leaves the queue (`0x7409d0` → `0x1e4db4`). `hash` as for [`Self::Dependency`].
    QueueDependency { preset: String, hash: String },
    /// OVERWRITE of [`Services::capture_question`]: the same download, sorted again; while
    /// the store still holds the hash under another name, only its metadata is replaced.
    CaptureOverwrite,
    /// The NC editor DOWNLOAD of this NC hash: no `DELETE`; its job ends on the store (review
    /// r27 R1).
    Editor(String),
    /// A preset into a setlist slot ([`Services::download_presets`]): no `DELETE`; the store
    /// goes into the slot (`StockLibrary::store_cloud_preset`), not into Downloads.
    /// `replace`: see [`SlotDownload::replace`].
    Slot {
        setlist: String,
        slot: usize,
        replace: Option<String>,
    },
    /// A Neural Capture that the slot preset `preset` (its cloud id) needs (`0x740b84` →
    /// `0x74012c`): no `DELETE`; the store goes into the Captures Library user root. `hash` is
    /// the capture key the preset names (its `dependencies.product` entry).
    Dependency { preset: String, hash: String },
}

/// A cloud capture download waiting for its overwrite answer.
#[derive(Debug)]
pub(super) struct CaptureAsk {
    pub(super) question: CaptureQuestion,
    pub(super) download: CloudDownload,
}

/// One cloud preset of [`Services::download_presets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotDownload {
    /// The cloud product id.
    pub id: String,
    pub slot: usize,
    /// The path the slot held when the device checked it: the preset the user agreed to
    /// overwrite (`super::Device::overwrite_presets`). `None`: the slot was empty.
    pub replace: Option<String>,
}

impl From<(String, usize)> for SlotDownload {
    fn from((id, slot): (String, usize)) -> Self {
        Self {
            id,
            slot,
            replace: None,
        }
    }
}

/// A Multi Select download of captures or IRs ([`Services::download_products`],
/// [`Services::control_downloads`]).
#[derive(Debug)]
pub(super) struct DownloadBatch {
    pub(super) kind: ProductKind,
    /// The batch's product ids; a later single download of one takes it out.
    pub(super) ids: Vec<String>,
    /// The name-taken text was shown (`DAT_0154af41`).
    pub(super) told: bool,
    /// The run while a download of the batch is out ([`Services::bulk_run`]).
    pub(super) run: Option<RunState>,
    /// The queued downloads the stop flag dropped ([`Services::cancel_bulk_run`]).
    pub(super) dropped: usize,
}

/// A run of [`Services::download_presets`].
#[derive(Debug)]
pub(super) struct PresetBatch {
    /// The setlist name.
    pub(super) setlist: String,
    /// More than one item: the run ([`Services::bulk_run`]).
    pub(super) run: Option<RunState>,
    /// The items not started yet, in order.
    pub(super) waiting: VecDeque<SlotDownload>,
    /// The product id downloading or storing now.
    pub(super) current: Option<String>,
    /// The current preset while its capture dependencies download.
    pub(super) held: Option<HeldPreset>,
    pub(super) stored: usize,
    pub(super) total: usize,
}

/// A downloaded slot preset that waits for its Neural Capture dependencies (`0x742990` →
/// `0x740b84` → `0x74012c`); it is stored after the last one.
#[derive(Debug)]
pub(super) struct HeldPreset {
    pub(super) download: Box<CloudDownload>,
    /// The dependencies not asked yet, in download order (stock pops the list's back).
    pub(super) waiting: VecDeque<ProductDependency>,
    /// The cloud id of the dependency downloading or storing now.
    pub(super) current: Option<String>,
}

impl PresetBatch {
    /// The product id the run waits for: a dependency of the held preset, else the preset.
    pub(super) fn waits_on(&self) -> Option<&String> {
        match &self.held {
            Some(held) => held.current.as_ref(),
            None => self.current.as_ref(),
        }
    }
}

/// The end of one NC editor DOWNLOAD request (review r27 R1), for the UI
/// ([`Services::take_capture_download_ends`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureDownloadEnd {
    /// The NC hash the editor asked for.
    pub hash: String,
    pub outcome: DownloadOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// The capture is in the captures Downloads table.
    Stored,
    /// Refused, not found or hidden in the cloud, not downloaded, dropped, lost or not stored.
    Failed,
}

/// A Directory cloud folder as listed so far (`CloudFolderModel`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CloudFolder {
    /// Rows in list order, each through `0x722464`.
    pub rows: Vec<CloudProduct>,
    /// The last page was full ([`CLOUD_PAGE_ROWS`] rows; `0x433b3c` sets model `+0x40`, which
    /// canFetchMore `0x42ea50` reads), so more can follow.
    pub more: bool,
    /// The page the next request asks for: the pages that landed so far.
    pub next_page: u32,
    /// A page request is on the worker.
    pub fetching: bool,
    /// Why the last page request failed.
    pub error: Option<String>,
}

/// One DOWNLOADS row ([`super::Device::downloads_rows`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadsRow {
    /// The cloud product id: the Downloads table key and the download key.
    pub id: String,
    pub name: String,
    pub author: String,
    /// A capture's record version (`"1"`, `"2"`); a queue row's product `version`.
    pub version: String,
    /// A stored capture's NC hash (the `hash` column).
    pub hash: Option<String>,
    /// The product is in its Downloads table: a tap loads it, it does not download again.
    pub stored: bool,
}

/// The cloud product type of a Directory item kind.
pub(in crate::device) fn product_kind(kind: tetrad_ui::ItemKind) -> ProductKind {
    match kind {
        tetrad_ui::ItemKind::Preset => ProductKind::Preset,
        tetrad_ui::ItemKind::Capture => ProductKind::NeuralCapture,
        tetrad_ui::ItemKind::Ir => ProductKind::ImpulseResponse,
    }
}

/// CLOUD lists the account's products, DOWNLOADS its download queue (folder `+0x40`).
pub(in crate::device) fn browse_scope(scope: tetrad_ui::CloudScope) -> BrowseScope {
    match scope {
        tetrad_ui::CloudScope::Cloud => BrowseScope::OwnedByMe,
        tetrad_ui::CloudScope::Downloads => BrowseScope::DownloadQueue,
    }
}

/// O: a cloud folder stops asking for more rows at this many.
pub const CLOUD_FOLDER_ROWS: usize = 2_000;

/// Rows asked per cloud folder page: the stock list model's fetchMore asks folder vtable
/// `+0x38` for the rows `[n, n + 50)` (`0x433358`); a page of 50 rows has more after it
/// (`0x433b3c`). The stock device logs show `size=50&page=0`.
pub const CLOUD_PAGE_ROWS: u32 = 50;

/// The cloud folder sort key: the stock model default `0x21` through `0x74df0c`.
pub(super) const CLOUD_SORT: &str = "name";

/// O: the most pages one queue walk reads.
pub const DOWNLOAD_QUEUE_PAGES: usize = 20;

/// O: the most payload bytes one queue walk holds (the worker keeps every payload of the walk
/// in memory until the device stores them); each payload is at most [`PAYLOAD_LIMIT`].
pub const DOWNLOAD_QUEUE_BYTES: usize = 32 << 20;

pub(super) fn product_error(error: tetrad_services::cloud::products::ProductError) -> ServiceError {
    ServiceError::Cloud(ApiError::Reply(error.to_string()))
}
