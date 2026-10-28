//! What the page asks of the device, and what the device answers.

/// The request header that carries the PIN. A custom header also makes a browser send a
/// CORS preflight, which the server does not answer, so other sites cannot post uploads.
pub const PIN_HEADER: &str = "x-tetrad-pin";

/// A request that the device answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The PIN check of the page, and the folder that uploads go to.
    Status { pin: String },
    /// One IR as a stock WAV (mono 48 kHz float32, data chunk at byte 38).
    Upload {
        pin: String,
        name: String,
        wav: Vec<u8>,
    },
    /// The installed extensions that take asset files (NAM models, for example), with their
    /// files.
    Files { pin: String },
    /// The next bytes of file `name` for extension `id`. `offset` is the number of bytes already
    /// stored, and `last` ends the file: the unit then keeps it and lets the extension read it.
    FileChunk {
        pin: String,
        id: String,
        name: String,
        offset: u64,
        last: bool,
        bytes: Vec<u8>,
    },
    /// Removes file `name` of extension `id`.
    FileDelete {
        pin: String,
        id: String,
        name: String,
    },
    /// The support file (docs/support.md).
    Support { pin: String },
}

impl Request {
    pub fn pin(&self) -> &str {
        match self {
            Self::Status { pin }
            | Self::Upload { pin, .. }
            | Self::Files { pin }
            | Self::FileChunk { pin, .. }
            | Self::FileDelete { pin, .. }
            | Self::Support { pin } => pin,
        }
    }
}

/// One request for the device. `id` matches the [`Reply`] to the open connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub id: u64,
    pub request: Request,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Status {
        folder: String,
    },
    Added {
        name: String,
        folder: String,
    },
    Files {
        extensions: Vec<ExtensionFiles>,
    },
    /// The chunk is stored; `received` bytes of the file are here.
    FileReceived {
        received: u64,
    },
    /// The last chunk arrived and the file is in place.
    FileStored {
        name: String,
        size: u64,
    },
    FileDeleted {
        name: String,
    },
    /// A download: the file name and a zip archive.
    File {
        name: String,
        zip: Vec<u8>,
    },
    WrongPin,
    Off,
    Refused(String),
    /// The device did not answer in time. Only the server makes this reply.
    Busy,
}

/// One extension that takes asset files, and the files it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionFiles {
    pub id: String,
    pub name: String,
    /// What the files are, such as "NAM model".
    pub label: String,
    /// The extensions a file may have, with the dot (`.nam`).
    pub extensions: Vec<String>,
    /// The largest file the extension reads.
    pub max_bytes: u64,
    pub files: Vec<FileEntry>,
}

/// One file of an extension and what the extension made of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    /// `ready` (its block is built), `waiting` (the extension has not read it yet) or `failed`.
    pub state: String,
    /// Why it failed.
    pub detail: String,
}

impl Reply {
    /// The status code and the JSON body. [`Reply::File`] is written as the file itself
    /// (`http::serve_one`), never as JSON.
    pub(crate) fn response(&self) -> (u16, serde_json::Value) {
        use serde_json::json;
        match self {
            Self::Status { folder } => (200, json!({ "folder": folder })),
            Self::Added { name, folder } => (200, json!({ "name": name, "folder": folder })),
            Self::Files { extensions } => (
                200,
                json!({ "extensions": extensions.iter().map(|extension| json!({
                    "id": extension.id,
                    "name": extension.name,
                    "label": extension.label,
                    "extensions": extension.extensions,
                    "maxBytes": extension.max_bytes,
                    "files": extension.files.iter().map(|file| json!({
                        "name": file.name,
                        "size": file.size,
                        "state": file.state,
                        "detail": file.detail,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>() }),
            ),
            Self::FileReceived { received } => (200, json!({ "received": received })),
            Self::FileStored { name, size } => (200, json!({ "name": name, "size": size })),
            Self::FileDeleted { name } => (200, json!({ "name": name })),
            Self::File { name, .. } => (200, json!({ "name": name })),
            Self::WrongPin => (401, json!({ "error": "Wrong PIN" })),
            Self::Off => (503, json!({ "error": "Remote is off" })),
            Self::Refused(reason) => (422, json!({ "error": reason })),
            Self::Busy => (503, json!({ "error": "The unit did not answer" })),
        }
    }
}
