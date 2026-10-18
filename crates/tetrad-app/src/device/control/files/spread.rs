//! A File (4) READ listing or a CloudProduct (46) READ transfer state, built over several device
//! passes (A3). Stock builds and sends each in its receiver. At QC scale the connect-burst File
//! READ reads about 500 sidecars, product rows and capture metadata, and held one Tetrad pass for
//! hundreds of ms. Each [`Spread::step`] reads at most [`READS_PER_PASS`] files and finishes at
//! most one folder, so touch, footswitches and frames run between the steps. The messages, their
//! order and their request ID are the ones the receiver sent in one pass.

use std::collections::{BTreeSet, VecDeque};

use tetrad_control::codecs::{
    CloudProductMessage, CloudTransferItemState, CloudTransferState, CloudTransferStateItem,
    ControlMessage, FileMessage, MessageAction, ProductData,
};
use tetrad_control::device::DeviceResult;
use tetrad_media::factory::DeviceCipher;
use tetrad_storage::stock::DownloadKind;

use super::super::super::State;
use super::super::super::services::UploadState;
use super::super::products;
use super::{FolderRef, Library, PendingFile, Source};

/// The library reads ([`PendingFile::reads`]) of one step. Host, release, QC-scale sim: a capture
/// row with its metadata costs about 6 µs, a Downloads metadata about 1 µs, so a step costs at
/// most about 0.2 ms on the host and about 30 ms on the QC (general code there runs about
/// 150 times slower).
pub const READS_PER_PASS: usize = 32;

/// One reply part, in send order.
pub(super) enum Part {
    /// `user_content_estimate` ([`Library::estimate`]).
    Estimate,
    Folder(FolderRef),
    /// A message built with the request (the factory IR folder).
    Ready(FileMessage),
}

enum Output {
    /// File READ: each part goes out as a File UPDATE; a Downloads folder goes after its
    /// transfer state push ([`transfer_push`]).
    Listing,
    /// CloudProduct READ: the folders fill the transfer state keys ([`add_synced`]); DELETE
    /// and UPDATE go out at the end ([`products::transfer_state`]).
    Transfer {
        keys: BTreeSet<String>,
        in_flight: Vec<String>,
        uploads: Vec<(String, UploadState)>,
    },
}

/// The folder in progress: its built files and the files still to read.
struct Building {
    folder: FolderRef,
    files: Vec<ProductData>,
    pending: std::vec::IntoIter<PendingFile>,
}

/// A READ reply in progress. The device runs one [`Self::step`] per pass, in place of the next
/// Control message, until it is done.
pub(in crate::device) struct Spread {
    parts: VecDeque<Part>,
    building: Option<Building>,
    output: Output,
}

impl Spread {
    pub(super) fn listing(parts: Vec<Part>) -> Self {
        Self {
            parts: parts.into(),
            building: None,
            output: Output::Listing,
        }
    }

    pub(super) fn transfer(
        folders: Vec<FolderRef>,
        in_flight: Vec<String>,
        uploads: Vec<(String, UploadState)>,
    ) -> Self {
        Self {
            parts: folders.into_iter().map(Part::Folder).collect(),
            building: None,
            output: Output::Transfer {
                keys: BTreeSet::new(),
                in_flight,
                uploads,
            },
        }
    }

    /// One step: the estimate, a ready message, or up to [`READS_PER_PASS`] reads of the
    /// current folder (with its message when it is complete). Returns the messages of the step
    /// and whether the reply is complete.
    pub(in crate::device) fn step(
        &mut self,
        state: &State,
        cipher: Option<&DeviceCipher>,
    ) -> DeviceResult<(Vec<ControlMessage>, bool)> {
        let library = Library::new(state, cipher)?;
        let mut out = Vec::new();
        let building = match self.building.take() {
            Some(building) => Some(building),
            None => match self.parts.pop_front() {
                None => None,
                Some(Part::Estimate) => {
                    out.push(estimate(library.estimate()?).into());
                    None
                }
                Some(Part::Ready(message)) => {
                    out.push(message.into());
                    None
                }
                Some(Part::Folder(folder)) => Some(Building {
                    pending: library.pending(&folder)?.into_iter(),
                    folder,
                    files: Vec::new(),
                }),
            },
        };
        if let Some(mut building) = building {
            let (mut reads, mut part) = (0, Vec::new());
            while reads < READS_PER_PASS
                && let Some(pending) = building.pending.next()
            {
                reads += usize::from(pending.reads());
                part.push(pending);
            }
            building.files.extend(library.files(part)?);
            match building.pending.len() {
                0 => self.finish(building, &mut out),
                _ => self.building = Some(building),
            }
        }
        let done = self.building.is_none() && self.parts.is_empty();
        if done
            && let Output::Transfer {
                keys,
                in_flight,
                uploads,
            } = &mut self.output
        {
            let keys = std::mem::take(keys);
            let messages = products::transfer_state(keys, uploads, in_flight);
            out.extend(messages.map(ControlMessage::from));
        }
        Ok((out, done))
    }

    /// Every step at once, for a caller that sends the reply in one pass (the resync).
    pub(in crate::device::control) fn run(
        mut self,
        state: &State,
        cipher: Option<&DeviceCipher>,
    ) -> DeviceResult<Vec<ControlMessage>> {
        let mut out = Vec::new();
        loop {
            let (messages, done) = self.step(state, cipher)?;
            out.extend(messages);
            if done {
                return Ok(out);
            }
        }
    }

    fn finish(&mut self, building: Building, out: &mut Vec<ControlMessage>) {
        let Building { folder, files, .. } = building;
        match &mut self.output {
            Output::Listing => {
                if let Source::Downloads(kind) = folder.source {
                    out.extend(transfer_push(kind, &files).map(ControlMessage::from));
                }
                out.push(folder.finish(files).into());
            }
            Output::Transfer { keys, .. } => add_synced(keys, &folder, &files),
        }
    }
}

/// `File Read part <folder key>` or `CloudProduct Read part <folder key>`: the folder in
/// progress, else the next part (diagnostics).
impl std::fmt::Display for Spread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reply = match self.output {
            Output::Listing => "File",
            Output::Transfer { .. } => "CloudProduct",
        };
        let part = match (&self.building, self.parts.front()) {
            (Some(building), _) => building.folder.key.as_str(),
            (None, Some(Part::Folder(folder))) => folder.key.as_str(),
            (None, Some(Part::Estimate)) => "estimate",
            (None, Some(Part::Ready(_))) => "ready",
            (None, None) => "end",
        };
        write!(f, "{reply} Read part {part}")
    }
}

fn estimate(count: i32) -> FileMessage {
    FileMessage {
        action: Some(MessageAction::Update.into()),
        user_content_estimate: Some(count),
        ..Default::default()
    }
}

/// The cloud ids of a Downloads folder: a preset's row id (its key), a capture's metadata
/// `cloudUUID`.
fn downloaded(kind: DownloadKind, files: &[ProductData]) -> BTreeSet<String> {
    (files.iter())
        .filter_map(|file| match kind {
            DownloadKind::Preset => file.key.clone(),
            DownloadKind::Capture => file.cloud_id.clone(),
        })
        .collect()
}

/// The transfer state push (`0x7db73c` → `0x9eb92c`) that goes with a listed Downloads
/// folder: listing it loads its rows, and each enters the state map as Downloaded (S7 capture:
/// an UPDATE with the 9 preset ids before `cloud-0-1`, one with the 380 capture ids before
/// `cloud-2-1`, C). I: stock loads the folders once per boot and then pushes only changes;
/// Tetrad pushes with every listing (the states do not change). None for no ids.
fn transfer_push(kind: DownloadKind, files: &[ProductData]) -> Option<CloudProductMessage> {
    let ids = downloaded(kind, files);
    let items = (ids.into_iter())
        .map(|id| CloudTransferStateItem {
            id: Some(id),
            state: Some(CloudTransferItemState::Downloaded.into()),
        })
        .collect::<Vec<_>>();
    (!items.is_empty()).then(|| CloudProductMessage {
        action: Some(MessageAction::Update.into()),
        cloud_transfer_state: Some(CloudTransferState { items }),
        ..Default::default()
    })
}

/// The keys the transfer state map (`0x7db468`) holds as Downloaded (2) once the library is
/// loaded (S7 capture, CloudProduct READ: 479 keys, C): every Downloads row by cloud id
/// ([`downloaded`]), and each library capture and setlist preset that has a cloud id by its
/// key and by that id (a capture's metadata `cloudUUID`, a preset's sidecar `cloudId`). Not
/// built: IRs (their metadata keys are not recovered; the capture QC has none).
fn add_synced(keys: &mut BTreeSet<String>, folder: &FolderRef, files: &[ProductData]) {
    if let Source::Downloads(kind) = folder.source {
        keys.extend(downloaded(kind, files));
        return;
    }
    for file in files {
        if let (Some(key), Some(cloud)) = (&file.key, &file.cloud_id) {
            keys.insert(key.clone());
            keys.insert(cloud.clone());
        }
    }
}
