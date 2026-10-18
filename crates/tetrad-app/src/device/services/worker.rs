//! The service worker jobs: [`run_job`], the backup and installer runs.

use super::*;

/// Runs one job to the end on the calling thread. Platform workers and the inline default of
/// [`ServiceBackends::run_job`] both use it.
pub fn run_job(backends: &mut impl ServiceBackends, job: ServiceJob) -> ServiceNotice {
    match job {
        ServiceJob::Dhcp(run) => ServiceNotice::Dhcp {
            run,
            result: dhcp::acquire(backends.runner()),
        },
        ServiceJob::Internet(work) => ServiceNotice::Internet(match backends.lend() {
            Some(io) => work.run(io.net, io.runner),
            None => work.failed(NetError::Io(JobError::NoBackends.to_string())),
        }),
        ServiceJob::Download(job) => ServiceNotice::Download(match backends.lend() {
            Some(io) => {
                let (offer, cancel) = (&job.offer, &job.cancel);
                match &job.via_control {
                    Some(control) => {
                        let files = std::cell::RefCell::new(io.files);
                        let mut http = control.client(&files);
                        download_update(&mut http, &mut &files, io.runner, offer, cancel)
                    }
                    None => download_update(io.http, io.files, io.runner, offer, cancel),
                }
                .map_err(JobError::from)
            }
            None => Err(JobError::NoBackends),
        }),
        ServiceJob::Backup(job) => ServiceNotice::Backup(match backends.lend() {
            Some(io) => run_backup(io, job),
            None => Err(JobError::NoBackends),
        }),
        ServiceJob::Install => ServiceNotice::Install(match backends.lend() {
            Some(io) => run_installer(io),
            None => Err(JobError::NoBackends),
        }),
        ServiceJob::Cloud(job) => {
            let reply = job.take().map(|job| match backends.lend() {
                Some(io) => job.run(io.http, io.files),
                None => job.lost(),
            });
            ServiceNotice::Cloud(Handoff::new(reply.unwrap_or_else(CloudReply::gone)))
        }
    }
}

/// A cloud backup body can be large; the transfer gets this long (O).
pub(super) const BACKUP_TRANSFER_TIMEOUT: Duration = Duration::from_secs(1_800);
/// A backup body through Cortex Control (`0x1417b4`, `0x1458ac`: 600 000 ms).
const CONTROL_BACKUP_TIMEOUT: Duration = Duration::from_millis(600_000);

/// The `Hardware-Source` identity of a backup body through Cortex Control (as the cloud
/// session's, [`Services::load_session`]).
fn control_identity() -> DeviceIdentity {
    DeviceIdentity {
        client_type: ClientType::QuadCortex,
        firmware: tetrad_firmware::running().version.to_owned(),
        serial: String::new(),
    }
}
/// The backup `/status` poll: one GET each second (`0x30e5c0` → `0x45940c(.., 1000, ..)`) for
/// at most 60 s from the PUT reply (`0x3084fc`).
pub(super) const BACKUP_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub(super) const BACKUP_POLL_LIMIT: Duration = Duration::from_secs(60);

pub(super) fn run_backup<B: ServiceBackends + ?Sized>(
    io: ServiceIo<'_, B>,
    job: BackupJob,
) -> Result<BackupDone, JobError> {
    let root = Path::new(backup::DATA_ROOT);
    /// Packs with the staged database, then removes the stage. A stage that does not build
    /// fails the pack.
    fn pack(
        files: &mut impl Files,
        root: &Path,
        stage: Option<Box<Stage>>,
        dest: &Path,
        notes: &mut Vec<String>,
    ) -> Result<Summary, BackupError> {
        let staged = (stage.as_deref())
            .map(|stage| build_stage(stage, notes))
            .transpose()?;
        let database = staged.unwrap_or(root).join("zenui.db");
        let packed = backup::create(files, root, &database, dest);
        // Made with `std::fs` beside `zenui.db` by `build_stage`, so removed the same way.
        match staged {
            Some(dir) => std::fs::remove_dir_all(dir)
                .map_err(|error| {
                    BackupError::from(FileError {
                        op: "remove the backup stage",
                        path: dir.to_owned(),
                        reason: error.to_string(),
                    })
                })
                .and(packed),
            None => packed,
        }
    }
    let mut notes = Vec::new();
    Ok(match job {
        BackupJob::Create { dest, stage } => {
            let summary = pack(io.files, root, stage, &dest, &mut notes)?;
            BackupDone::Created { summary, notes }
        }
        BackupJob::CreateLocal {
            stage,
            header,
            stamp,
        } => {
            let dest = Path::new(LOCAL_ARCHIVE);
            pack(io.files, root, stage, dest, &mut notes)?;
            let archive = io.files.read(dest).map_err(BackupError::from)?;
            io.files.remove(dest).map_err(BackupError::from)?;
            let archive = archive.ok_or(BackupError::Empty)?;
            let json = backup::local::encode(&archive, &header, Some(&stamp));
            BackupDone::LocalCreated { json, notes }
        }
        BackupJob::RestoreLocal { json } => {
            let local = backup::local::decode(&json, tetrad_firmware::running().version)?;
            let dest = Path::new(backup::DOWNLOADED_BACKUP);
            io.files
                .write(dest, &local.archive)
                .map_err(BackupError::from)?;
            BackupDone::Restored(backup::restore(io.files, root, dest)?)
        }
        BackupJob::Upload {
            archive,
            target,
            header,
            via_control,
        } => {
            let blob = io.files.read(&archive).map_err(BackupError::from)?;
            let sent = match blob {
                Some(blob) => {
                    let file = backup::local::encode(&blob, &header, None).into_bytes();
                    match &via_control {
                        Some(control) => {
                            let files = std::cell::RefCell::new(&mut *io.files);
                            let device = control_identity();
                            upload_backup(&mut control.client(&files), &target, Some(&device), file)
                        }
                        None => upload_backup(io.http, &target, None, file),
                    }
                    .map_err(JobError::from)
                }
                None => Err(BackupError::Empty.into()),
            };
            // Health r34 #4: a failed clean-up after a good PUT is a note, not a failed
            // upload (a retry would make a second cloud backup).
            let removed = io.files.remove(&archive);
            sent?;
            let notes = removed
                .err()
                .map(|error| format!("backup upload clean-up: {error}"))
                .into_iter()
                .collect();
            BackupDone::Uploaded { notes }
        }
        BackupJob::Fetch { url, via_control } => {
            let dest = Path::new(backup::DOWNLOADED_BACKUP);
            let route = Route::Backups(BackupsOp::Download);
            let status = match &via_control {
                // `0x1417b4`: BackupsForward 30 request type 1, 600 000 ms, the `0x1458ac` headers.
                Some(control) => {
                    let device = control_identity();
                    let request = HttpRequest::new(Method::Get, url, route, CONTROL_BACKUP_TIMEOUT)
                        .header(
                            "X-Cloud-Api-Hardware-Source-Type",
                            device.client_type.as_str(),
                        )
                        .header("X-Cloud-Api-Hardware-Source-Version", device.firmware)
                        .header("Content-Transfer-Encoding", "binary");
                    let files = std::cell::RefCell::new(&mut *io.files);
                    control.client(&files).download(&request, dest)
                }
                None => {
                    let request =
                        HttpRequest::new(Method::Get, url, route, BACKUP_TRANSFER_TIMEOUT);
                    io.http.download(&request, dest)
                }
            }
            .map_err(|e: HttpError| JobError::Cloud(e.into()))?;
            if !(200..300).contains(&status) {
                io.files.remove(dest).map_err(BackupError::from)?;
                return Err(ApiError::Cloud(CloudError::from_reply(status, b"")).into());
            }
            // The body is the cloud backup file (`0x407908` → `0x4072fc(.., 0, ..)`); a raw or
            // base64 archive (a Tetrad upload before the file wrapper) still reads.
            let body = io.files.read(dest).map_err(BackupError::from)?;
            let archive = match body.as_deref() {
                Some(body) if body.trim_ascii_start().starts_with(b"{") => {
                    let text = std::str::from_utf8(body)
                        .map_err(|_| BackupError::Corrupt("the backup file is not UTF-8".into()))?;
                    Some(backup::local::decode_cloud(
                        text,
                        tetrad_firmware::running().version,
                    )?)
                }
                Some(body) => backup::decode_download(body),
                None => None,
            };
            if let Some(archive) = archive {
                io.files.write(dest, &archive).map_err(BackupError::from)?;
            }
            BackupDone::Fetched
        }
        BackupJob::Restore { archive } => {
            BackupDone::Restored(backup::restore(io.files, root, &archive)?)
        }
    })
}

/// The [`Stage`] on the worker ([`tetrad_library::stock::stage_for_backup`]).
pub(super) fn build_stage<'s>(
    stage: &'s Stage,
    notes: &mut Vec<String>,
) -> Result<&'s Path, BackupError> {
    use tetrad_library::stock::{CaptureCiphers, stage_for_backup};
    let keys = stage.keys.clone().and_then(|keys| {
        keys.take()
            .ok_or_else(|| "the capture ciphers were taken".into())
    });
    let ciphers = keys.as_ref().ok().map(|keys| CaptureCiphers {
        global: &keys.global,
        local: &keys.local,
    });
    match stage_for_backup(&stage.database, &stage.dir, ciphers) {
        Ok((_, rekeyed)) => {
            let rekeyed = match (rekeyed, keys) {
                (Some(rekeyed), _) => rekeyed.map(drop).map_err(|error| error.to_string()),
                (None, keys) => keys.map(drop),
            };
            if let Err(error) = rekeyed {
                notes.push(format!("backup keeps captures bound to this unit: {error}"));
            }
            Ok(&stage.dir)
        }
        Err(error) => Err(BackupError::Stage(error.to_string())),
    }
}

pub(super) fn run_installer<B: ServiceBackends + ?Sized>(
    io: ServiceIo<'_, B>,
) -> Result<InstallOutcome, JobError> {
    let mut run = install::start(io.runner, io.files)?;
    loop {
        match run.poll(io.files)? {
            InstallPoll::Running { .. } => {
                std::thread::sleep(Duration::from_millis(STATUS_POLL_MS));
            }
            InstallPoll::Finished(outcome) => return Ok(outcome),
        }
    }
}
