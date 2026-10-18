//! One cloud job on the service worker: its calls, replies and the session it borrows.

use super::*;

/// A cloud request the capture bridge asks for (`Effect::Cloud`). The reply comes back as
/// `Effect::Nc2` input or an [`Nc2Download`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudRequest {
    /// `Nc2Effect::GetJob`: the job URL (VA 0x0020068c).
    Nc2Job(String),
    /// `Nc2Effect::DeleteProduct`: the product URL (VA 0x00717c94).
    Nc2Delete(String),
    /// `Nc2Effect::DownloadProduct`: the product id (VA 0x007061b0).
    Nc2Download(String),
}

/// A downloaded NC2 product: the decoded payload, still encrypted, and the product record
/// (`GET products/<id>`). `Debug` shows the payload length only.
#[derive(Clone, PartialEq)]
pub struct Nc2Download {
    pub product_id: String,
    pub payload: Vec<u8>,
    pub product: serde_json::Value,
}

impl fmt::Debug for Nc2Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Nc2Download")
            .field("product_id", &self.product_id)
            .field("payload", &self.payload.len())
            .finish_non_exhaustive()
    }
}

/// One cloud request of a [`CloudJob`], run on the service worker. Signed calls also wait in
/// a [`SigningBatch`] first.
#[derive(Debug)]
pub(super) enum CloudCall {
    /// `user/signin`, no bearer (0x0030c768).
    SignIn {
        username: String,
        password: Secret,
    },
    /// The tokens of a Cortex Control login (0x00450158); no request.
    ControlLogin {
        username: Option<String>,
        tokens: Tokens,
        request_id: Option<u64>,
    },
    /// `POST /hardware/v1/pair` (0x00307b04 flag 1); after a sign-in also `user/profile`.
    Pair(PairFor),
    /// `DELETE /hardware/v1/pair` (0x00307b04 flag 0), then the logout `AfterUnpair` names.
    Unpair(AfterUnpair),
    /// The logout alone, after an unpair that never went out.
    Logout(AfterUnpair),
    /// `GET /firmware/v1/…` (0x00469d90), then the offer and release notes.
    UpdateCheck {
        suffix: Option<String>,
        then_download: bool,
        /// Cortex Control carries the check and the download (`CloudUpdaterUSB`).
        via_control: Option<ControlTransport>,
    },
    /// `POST /purchase/v1/licenses/activate` and the reply check (0x001f0194, 0x001f2ccc).
    Licences,
    /// `GET /backups/v1`, after `DELETE <delete>` of one listed backup (0x003046a8).
    Backups {
        delete: Option<String>,
    },
    /// `POST <path>/rename` with `{"name"}` (0x0030d054); the list goes after it as its own
    /// call (C: `0x150ab8` → `0x152218`).
    BackupRename {
        path: String,
        name: String,
    },
    /// `GET /backups/v1/generateBackupUrl/<name>` (0x00303300).
    BackupTarget {
        name: String,
    },
    /// One `GET <backupUrl>/status` after the backup PUT (`0x3084fc`).
    BackupStatus {
        path: String,
    },
    /// Backup ⋮ Update: `GET <backupUrl>/update` (`0x303564`), the same reply as
    /// [`Self::BackupTarget`].
    BackupUpdate {
        path: String,
    },
    /// `GET <backup path>` (0x0030d4b0): the reply's `url` is where the body is (I), and its
    /// `creator` is the backup's hardware (`0x3ff910` → `0x30d814`, callback `0x407908`).
    BackupSource {
        path: String,
    },
    Nc2(CloudRequest),
    /// A Directory upload (`0x725ba4`) of one flow: its reply completes only that flow.
    Upload {
        flow: UploadFlow,
        upload: Box<ProductUpload>,
    },
    /// The preset name check of a Directory upload (`0x719794`).
    NameCheck(String),
    /// The upload check of a user capture (`0x6f94ec`, [`CloudApi::capture_upload_status`]).
    CaptureStatus(CaptureCheck),
    /// The upload check of a capture in a preset's walk (`0x74102c`: `0x6f94ec` mode 0).
    WalkStatus(CaptureCheck),
    /// One product into its Downloads folder (`0x720ebc` chain).
    Download {
        product_id: String,
        origin: DownloadOrigin,
    },
    /// The account's download queue of one kind: page by page, then each product.
    DownloadQueue(ProductKind),
    /// One page of a Directory cloud folder ([`Services::browse_cloud`]).
    Browse(BrowsePage),
    /// NC editor DOWNLOAD: the capture with this NC hash (`0x7bcac8`), then its download.
    DownloadCapture {
        hash: String,
    },
    /// `DELETE user/downloaded_products_queue/<cloud id>` (`0x718204`) after a stored download.
    LeaveQueue {
        cloud_id: String,
    },
    /// `DELETE user/products/<cloud id>` (`0x7180bc`): Cortex Control 46 DELETE only.
    DeleteProduct {
        kind: ProductKind,
        cloud_id: String,
    },
}

impl CloudCall {
    /// The activation and its pre-pair (health r34 #5): a factory reset drops both, and a
    /// dropped one ends the licence step.
    pub(super) fn is_licence_call(&self) -> bool {
        matches!(self, Self::Licences | Self::Pair(PairFor::Licences))
    }

    /// The Directory upload flow this call belongs to.
    pub(super) fn upload_flow(&self) -> Option<UploadFlow> {
        match self {
            Self::NameCheck(_) | Self::WalkStatus(_) => Some(UploadFlow::Preset),
            Self::CaptureStatus(_) => Some(UploadFlow::Capture),
            Self::Upload { flow, .. } => Some(*flow),
            _ => None,
        }
    }
}

/// The Directory upload a cloud call belongs to (review r36 H5): its reply completes only
/// that upload, whatever the product kind (a preset walk also uploads captures).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UploadFlow {
    /// [`Services::upload_preset`] and its capture walk.
    Preset,
    /// [`Services::upload_capture`].
    Capture,
}

impl UploadFlow {
    pub(super) fn kind(self) -> ProductKind {
        match self {
            Self::Preset => ProductKind::Preset,
            Self::Capture => ProductKind::NeuralCapture,
        }
    }
}

/// One cloud folder page: the raw reply row count and the rows that parse.
#[derive(Debug)]
pub(super) struct BrowsedRows {
    pub(super) raw: usize,
    pub(super) rows: Vec<CloudProduct>,
}

/// The result of one [`CloudCall`], applied on the event loop.
#[derive(Debug)]
pub(super) enum CloudDone {
    SignIn(Result<(), ApiError>),
    ControlLogin {
        request_id: Option<u64>,
        result: Result<(), ApiError>,
    },
    /// `paired` holds the firmware prefix stored as `last_paired_version` (0x00307784).
    Pair {
        reason: PairFor,
        paired: Result<String, ServiceError>,
        profile: Option<Result<(), ServiceError>>,
    },
    /// `unpaired` is `None` for [`CloudCall::Logout`].
    Logout {
        unpaired: Option<Result<String, ServiceError>>,
        logout: Result<(), ApiError>,
    },
    UpdateCheck {
        then_download: bool,
        checked: Result<(CheckOutcome, Option<(String, String)>), ServiceError>,
        via_control: Option<ControlTransport>,
    },
    Licences(Result<OpaqueBody, ServiceError>),
    Backups(Result<Vec<serde_json::Value>, ServiceError>),
    BackupRenamed(Result<(), ServiceError>),
    BackupTarget(Result<BackupUrl, ServiceError>),
    BackupStatus(Result<BackupState, ServiceError>),
    /// The download `url` and the `creator` text ("" when absent).
    BackupSource(Result<(String, String), ServiceError>),
    Nc2Job(Result<serde_json::Value, ServiceError>),
    Nc2Delete(Result<(), ServiceError>),
    Nc2Download {
        product_id: String,
        result: Result<(Vec<u8>, serde_json::Value), ServiceError>,
    },
    /// The product's cloud id (`0x71edc0`).
    Upload {
        flow: UploadFlow,
        name: String,
        result: Result<String, ServiceError>,
    },
    CaptureStatus(Result<CaptureUploadStatus, ServiceError>),
    WalkStatus(Result<CaptureUploadStatus, ServiceError>),
    /// The cloud id of the account's preset with the upload's name, `None` when it is free.
    NameCheck(Result<Option<String>, ServiceError>),
    /// The rows of one cloud folder page, each through `0x722464`, in list order.
    /// `rows` is the raw reply row count (`0x433b3c` compares it with the page size), before
    /// the parse filter.
    Browse {
        page: BrowsePage,
        result: Result<BrowsedRows, ServiceError>,
    },
    /// Downloaded products, each parsed and unpacked (or why not), in queue order.
    Downloads(Vec<(String, Result<CloudDownload, ServiceError>)>),
    /// The NC editor DOWNLOAD of `hash` ([`CloudCall::DownloadCapture`]).
    CaptureDownload {
        hash: String,
        result: Result<Box<CloudDownload>, ServiceError>,
    },
    LeftQueue {
        cloud_id: String,
        result: Result<(), ServiceError>,
    },
    Deleted {
        kind: ProductKind,
        cloud_id: String,
        result: Result<(), ServiceError>,
    },
}

/// Token changes made on the worker, written to `AccountConfig.ini` when the reply is
/// applied ([`SettingsTokens`]). The session already holds the tokens it needs.
#[derive(Default)]
pub(super) struct TokenLog(pub(super) Vec<Option<(Tokens, Option<String>)>>);

impl TokenStore for TokenLog {
    fn load(&mut self) -> Result<Option<Tokens>, TokenStoreError> {
        Ok(None)
    }

    fn store(&mut self, tokens: &Tokens, username: Option<&str>) -> Result<(), TokenStoreError> {
        self.0
            .push(Some((tokens.clone(), username.map(str::to_owned))));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), TokenStoreError> {
        self.0.push(None);
        Ok(())
    }

    fn username(&mut self) -> Result<Option<String>, TokenStoreError> {
        Ok(None)
    }
}

/// Cloud requests for the service worker with the session they run in.
pub struct CloudJob {
    pub(super) session: CloudApi,
    pub(super) tokens: TokenLog,
    /// Unix milliseconds at dispatch; `None` before the first NTP sync.
    pub(super) clock: Option<i64>,
    /// The signing batch's serial and signer, for signed calls.
    pub(super) signer: Option<(String, SharcSigner)>,
    pub(super) calls: Vec<CloudCall>,
    /// [`Services::set_control_route`] at dispatch: the calls go through Cortex Control.
    pub(super) via_control: Option<ControlTransport>,
}

impl fmt::Debug for CloudJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudJob")
            .field("calls", &self.calls)
            .finish_non_exhaustive()
    }
}

/// The session back from the worker, its token changes and one result per call. `lost`:
/// the worker stopped first, so `done` is empty and `session` may be gone too.
pub struct CloudReply {
    pub(super) session: Option<CloudApi>,
    pub(super) tokens: TokenLog,
    pub(super) done: Vec<CloudDone>,
    pub(super) lost: bool,
}

impl CloudReply {
    /// A job whose payload is gone: the worker took it and stopped.
    pub(super) fn gone() -> Self {
        Self {
            session: None,
            tokens: TokenLog::default(),
            done: Vec::new(),
            lost: true,
        }
    }
}

impl CloudJob {
    /// Runs the calls on `http` (Wi-Fi), or through Cortex Control when the job took its
    /// route: ProductForward 29 with the `Hardware-Source` headers (`0x80c254`, re-r44 §3). The
    /// update check keeps its own transport (`CloudUpdaterUSB` only after
    /// RestartDownloadViaControl, `0x4e7808`).
    pub(super) fn run(mut self, http: &mut impl HttpClient, files: &mut impl Files) -> CloudReply {
        let calls = std::mem::take(&mut self.calls);
        let done = match self.via_control.clone() {
            Some(control) => {
                let files = std::cell::RefCell::new(files);
                let mut forward = control.client(&files);
                let mut done = Vec::with_capacity(calls.len());
                for call in calls {
                    let routed = !matches!(call, CloudCall::UpdateCheck { .. });
                    self.session.via_control = routed;
                    done.push(match routed {
                        true => self.call(&mut forward, &mut &files, call),
                        false => self.call(http, &mut &files, call),
                    });
                }
                self.session.via_control = false;
                done
            }
            None => (calls.into_iter())
                .map(|call| self.call(http, files, call))
                .collect(),
        };
        CloudReply {
            session: Some(self.session),
            tokens: self.tokens,
            done,
            lost: false,
        }
    }

    pub(super) fn lost(self) -> CloudReply {
        CloudReply {
            session: Some(self.session),
            tokens: self.tokens,
            done: Vec::new(),
            lost: true,
        }
    }

    pub(super) fn unix(&self) -> Result<i64, ServiceError> {
        self.clock
            .map(|ms| ms.div_euclid(1000))
            .ok_or(ServiceError::NotReady(NO_TIME))
    }

    pub(super) fn signed(
        &mut self,
    ) -> Result<(&mut CloudApi, &mut TokenLog, &mut SharcSigner), ServiceError> {
        let (serial, signer) = self
            .signer
            .as_mut()
            .ok_or(ServiceError::NotReady("no signing batch"))?;
        self.session.device.serial = serial.clone();
        Ok((&mut self.session, &mut self.tokens, signer))
    }

    pub(super) fn call(
        &mut self,
        http: &mut impl HttpClient,
        files: &mut impl Files,
        call: CloudCall,
    ) -> CloudDone {
        match call {
            CloudCall::SignIn { username, password } => {
                let password = String::from_utf8_lossy(password.expose());
                CloudDone::SignIn(self.session.sign_in(
                    http,
                    &mut self.tokens,
                    &username,
                    &password,
                ))
            }
            CloudCall::ControlLogin {
                username,
                tokens,
                request_id,
            } => CloudDone::ControlLogin {
                request_id,
                result: (self.session).adopt_tokens(&mut self.tokens, tokens, username.as_deref()),
            },
            CloudCall::Pair(reason) => {
                let paired = self.pair(http, true);
                // 0x0030afb8 → 0x00301b94 (0x00307540 for a Control login): the profile
                // follows a successful sign-in pair.
                let signed_in = matches!(reason, PairFor::SignIn | PairFor::Control(_));
                let profile = (signed_in && paired.is_ok()).then(|| {
                    let unix: i64 = self.unix()?;
                    self.session.profile(http, &mut self.tokens, unix)?;
                    Ok::<_, ServiceError>(())
                });
                CloudDone::Pair {
                    reason,
                    paired,
                    profile,
                }
            }
            CloudCall::Unpair(after) => CloudDone::Logout {
                unpaired: Some(self.pair(http, false)),
                logout: self.logout(http, after),
            },
            CloudCall::Logout(after) => CloudDone::Logout {
                unpaired: None,
                logout: self.logout(http, after),
            },
            CloudCall::UpdateCheck {
                suffix,
                then_download,
                via_control,
            } => CloudDone::UpdateCheck {
                then_download,
                checked: match &via_control {
                    Some(control) => {
                        let files = std::cell::RefCell::new(files);
                        let mut http = control.client(&files);
                        self.update_check(&mut http, &mut &files, suffix.as_deref())
                    }
                    None => self.update_check(http, files, suffix.as_deref()),
                },
                via_control,
            },
            CloudCall::Licences => CloudDone::Licences(self.licences(http)),
            CloudCall::Backups { delete } => CloudDone::Backups(self.unix().and_then(|unix| {
                if let Some(path) = &delete {
                    (self.session).delete_backup(http, &mut self.tokens, path, unix)?;
                }
                Ok(self.session.list_backups(http, &mut self.tokens, unix)?)
            })),
            CloudCall::BackupRename { path, name } => {
                CloudDone::BackupRenamed(self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    Ok(self
                        .session
                        .rename_backup(http, tokens, &path, &name, unix)?)
                }))
            }
            CloudCall::BackupTarget { name } => {
                CloudDone::BackupTarget(self.unix().and_then(|unix| {
                    Ok(self
                        .session
                        .backup_upload_url(http, &mut self.tokens, &name, unix)?)
                }))
            }
            CloudCall::BackupUpdate { path } => {
                CloudDone::BackupTarget(self.unix().and_then(|unix| {
                    Ok(self
                        .session
                        .backup_update_url(http, &mut self.tokens, &path, unix)?)
                }))
            }
            CloudCall::BackupStatus { path } => {
                CloudDone::BackupStatus(self.unix().and_then(|unix| {
                    Ok(self
                        .session
                        .backup_state(http, &mut self.tokens, &path, unix)?)
                }))
            }
            CloudCall::BackupSource { path } => {
                CloudDone::BackupSource(self.unix().and_then(|unix| {
                    let reply = self
                        .session
                        .backup_details(http, &mut self.tokens, &path, unix)?;
                    let text = |key| reply.get(key).and_then(serde_json::Value::as_str);
                    let url =
                        text("url")
                            .filter(|url| !url.is_empty())
                            .ok_or(ServiceError::NotReady(
                                "the backup reply has no download url",
                            ))?;
                    Ok((
                        url.to_owned(),
                        text("creator").unwrap_or_default().to_owned(),
                    ))
                }))
            }
            CloudCall::Upload { flow, upload } => CloudDone::Upload {
                result: self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    Ok(self.session.upload_product(http, tokens, &upload, unix)?)
                }),
                flow,
                name: upload.name,
            },
            CloudCall::CaptureStatus(check) => {
                CloudDone::CaptureStatus(self.capture_status(http, &check, CheckMode::Capture))
            }
            CloudCall::WalkStatus(check) => {
                CloudDone::WalkStatus(self.capture_status(http, &check, CheckMode::Preset))
            }
            CloudCall::NameCheck(name) => CloudDone::NameCheck(self.unix().and_then(|unix| {
                let tokens = &mut self.tokens;
                let kind = ProductKind::Preset;
                Ok((self.session).product_name_taken(http, tokens, &name, kind, unix)?)
            })),
            CloudCall::Download { product_id, origin } => {
                let download = (self.download(http, &product_id))
                    .map(|download| CloudDownload { origin, ..download });
                CloudDone::Downloads(vec![(product_id, download)])
            }
            CloudCall::DownloadQueue(kind) => CloudDone::Downloads(self.download_queue(http, kind)),
            // O: an item the parser refuses (an unknown `type`) is left out of the list; stock
            // logs "bad product type" and lists it as a preset.
            CloudCall::Browse(page) => CloudDone::Browse {
                result: self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    let items = self.session.browse(http, tokens, &page, unix)?;
                    Ok(BrowsedRows {
                        raw: items.len(),
                        rows: (items.iter())
                            .filter_map(|item| CloudProduct::from_search_item(item).ok())
                            .collect(),
                    })
                }),
                page,
            },
            CloudCall::DownloadCapture { hash } => {
                let found = self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    Ok(self.session.search_capture(http, tokens, &hash, unix)?)
                });
                let result = match found {
                    Ok(found) if found.downloadable() => (self.download(http, &found.cloud_id))
                        .map(|download| {
                            Box::new(CloudDownload {
                                origin: DownloadOrigin::Editor(hash.clone()),
                                ..download
                            })
                        }),
                    Ok(_) => Err(ServiceError::NotReady(CAPTURE_NOT_IN_CLOUD)),
                    Err(error) => Err(error),
                };
                CloudDone::CaptureDownload { hash, result }
            }
            CloudCall::DeleteProduct { kind, cloud_id } => CloudDone::Deleted {
                result: self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    Ok((self.session).delete_product(http, tokens, &cloud_id, unix)?)
                }),
                kind,
                cloud_id,
            },
            CloudCall::LeaveQueue { cloud_id } => CloudDone::LeftQueue {
                result: self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    (self.session).remove_from_download_queue(http, tokens, &cloud_id, unix)?;
                    Ok(())
                }),
                cloud_id,
            },
            CloudCall::Nc2(CloudRequest::Nc2Job(url)) => {
                CloudDone::Nc2Job(self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    Ok(self
                        .session
                        .call(http, tokens, Method::Get, url, Route::Product, unix)?)
                }))
            }
            CloudCall::Nc2(CloudRequest::Nc2Delete(url)) => {
                CloudDone::Nc2Delete(self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    self.session
                        .call(http, tokens, Method::Delete, url, Route::Product, unix)?;
                    Ok(())
                }))
            }
            CloudCall::Nc2(CloudRequest::Nc2Download(product_id)) => {
                let result = self.unix().and_then(|unix| {
                    let tokens = &mut self.tokens;
                    let payload = self
                        .session
                        .product_payload(http, tokens, &product_id, unix)?;
                    let product = self.session.product(http, tokens, &product_id, unix)?;
                    Ok((payload, product))
                });
                CloudDone::Nc2Download { product_id, result }
            }
        }
    }

    pub(super) fn capture_status(
        &mut self,
        http: &mut impl HttpClient,
        check: &CaptureCheck,
        mode: CheckMode,
    ) -> Result<CaptureUploadStatus, ServiceError> {
        let unix = self.unix()?;
        let tokens = &mut self.tokens;
        Ok((self.session).capture_upload_status(http, tokens, check, mode, unix)?)
    }

    /// One product download (`0x720ebc` → `0x724ce4`), unpacked by its folder form
    /// (`0x71e820`, vtable `+0x9c`).
    pub(super) fn download(
        &mut self,
        http: &mut impl HttpClient,
        product_id: &str,
    ) -> Result<CloudDownload, ServiceError> {
        let unix = self.unix()?;
        let (product, payload) =
            (self.session).download_cloud_product(http, &mut self.tokens, product_id, unix)?;
        let payload = ProductPayload::unpack(product.kind, payload).map_err(product_error)?;
        Ok(CloudDownload {
            product,
            payload,
            origin: DownloadOrigin::Queue,
        })
    }

    /// The queue walk of [`Services::process_downloads_queue`]: pages 0, 1, … of
    /// [`CLOUD_PAGE_ROWS`] rows (`0x71aa28`, B10), while a page is full (`0x433b3c`). O: a
    /// product that leaves the queue during the walk shifts the later pages, so a row it
    /// skips stays queued for the next walk. O: at most [`DOWNLOAD_QUEUE_PAGES`] pages; a
    /// product id seen on an earlier page is not downloaded again; no download starts when one
    /// more payload of [`PAYLOAD_LIMIT`] could take the held payloads over
    /// [`DOWNLOAD_QUEUE_BYTES`] (the rest stays queued for the next walk). A
    /// product that is gone from the cloud (HTTP 404) leaves the queue at once.
    pub(super) fn download_queue(
        &mut self,
        http: &mut impl HttpClient,
        kind: ProductKind,
    ) -> Vec<(String, Result<CloudDownload, ServiceError>)> {
        // Review r27 R8: the held payloads stay at most DOWNLOAD_QUEUE_BYTES.
        let full = |held: usize| held + PAYLOAD_LIMIT as usize > DOWNLOAD_QUEUE_BYTES;
        let mut done = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut held = 0;
        for number in 0..DOWNLOAD_QUEUE_PAGES as u32 {
            let page = BrowsePage {
                kind,
                scope: BrowseScope::DownloadQueue,
                page: number,
                size: CLOUD_PAGE_ROWS,
                sort: super::products::CLOUD_SORT.into(),
                ascending: true,
            };
            let listed = self.unix().and_then(|unix| {
                let tokens = &mut self.tokens;
                Ok(self.session.browse(http, tokens, &page, unix)?)
            });
            let items = match listed {
                Ok(items) => items,
                Err(error) => {
                    done.push((String::new(), Err(error)));
                    break;
                }
            };
            for item in &items {
                let product = match CloudProduct::from_search_item(item) {
                    Ok(product) => product,
                    Err(error) => {
                        done.push((String::new(), Err(product_error(error))));
                        continue;
                    }
                };
                if full(held) || !seen.insert(product.id.clone()) {
                    continue;
                }
                let download = self.download(http, &product.id);
                held += download.as_ref().map_or(0, |d| d.payload.size());
                // Review r26 S8: a product the cloud no longer has (HTTP 404) leaves the queue
                // (`0x1e48d4`, `0x1e4a58` → `0x45ef6c` → `0x1e4db4`, C). Open: stock also
                // leaves on local error -20 for a capture (`0x45ef48`; not traced).
                let gone = matches!(&download,
                    Err(ServiceError::Cloud(ApiError::Cloud(error))) if error.http_status == 404);
                let id = product.id;
                done.push((id.clone(), download));
                if gone {
                    let left = self.unix().and_then(|unix| {
                        let tokens = &mut self.tokens;
                        (self.session).remove_from_download_queue(http, tokens, &id, unix)?;
                        Ok(())
                    });
                    if let Err(error) = left {
                        done.push((id, Err(error)));
                    }
                }
            }
            if items.len() != CLOUD_PAGE_ROWS as usize || full(held) {
                break;
            }
        }
        done
    }

    /// `POST` or `DELETE /hardware/v1/pair` (0x00301ec0).
    pub(super) fn pair(
        &mut self,
        http: &mut impl HttpClient,
        pair: bool,
    ) -> Result<String, ServiceError> {
        let unix = self.unix()?;
        let (api, store, signer) = self.signed()?;
        api.pair_hardware(http, store, signer, pair, unix)?;
        Ok(api.device.firmware_prefix().to_owned())
    }

    pub(super) fn logout(
        &mut self,
        http: &mut impl HttpClient,
        after: AfterUnpair,
    ) -> Result<(), ApiError> {
        match after {
            AfterUnpair::SignOut => self.session.sign_out(http, &mut self.tokens),
            AfterUnpair::LocalLogout => Ok(self.session.clear_tokens(&mut self.tokens)?),
        }
    }

    pub(super) fn update_check(
        &mut self,
        http: &mut impl HttpClient,
        files: &mut impl Files,
        suffix: Option<&str>,
    ) -> Result<(CheckOutcome, Option<(String, String)>), ServiceError> {
        let unix = self.unix()?;
        let (api, store, signer) = self.signed()?;
        let outcome = api.check_for_update(http, store, signer, suffix, unix)?;
        let mut notes = None;
        if let CheckOutcome::Available(offer) = &outcome {
            tetrad_services::updater::cloud::publish_offer(http, files, offer)?;
            notes = tetrad_services::updater::cloud::release_notes(files)?;
        }
        Ok((outcome, notes))
    }

    /// The signed activation, the reply check and the decoded code for the DSP (`0x50`).
    pub(super) fn licences(
        &mut self,
        http: &mut impl HttpClient,
    ) -> Result<OpaqueBody, ServiceError> {
        let now_ms = self.clock.ok_or(ServiceError::NotReady(NO_TIME))?;
        let (api, store, signer) = self.signed()?;
        let reply = api.activate_licenses(http, store, signer, now_ms.div_euclid(1000))?;
        // Stock checks with a fresh `0x35` batch (0x004a10f8); the material is bound to the
        // serial, so this batch's copy gives the same digest (I).
        let code = api.licence_activation_code(&signer.material, &reply, now_ms)?;
        OpaqueBody::try_from(code.as_bytes()).map_err(|_| {
            ServiceError::Unsupported("an activation code longer than one DSP request (0x50)")
        })
    }
}
