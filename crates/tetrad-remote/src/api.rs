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
}

impl Request {
    pub fn pin(&self) -> &str {
        match self {
            Self::Status { pin } | Self::Upload { pin, .. } => pin,
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
    WrongPin,
    Off,
    Refused(String),
    /// The device did not answer in time. Only the server makes this reply.
    Busy,
}

impl Reply {
    /// The status code and the JSON body.
    pub(crate) fn response(&self) -> (u16, serde_json::Value) {
        use serde_json::json;
        match self {
            Self::Status { folder } => (200, json!({ "folder": folder })),
            Self::Added { name, folder } => (200, json!({ "name": name, "folder": folder })),
            Self::WrongPin => (401, json!({ "error": "Wrong PIN" })),
            Self::Off => (503, json!({ "error": "Remote is off" })),
            Self::Refused(reason) => (422, json!({ "error": reason })),
            Self::Busy => (503, json!({ "error": "The unit did not answer" })),
        }
    }
}
