//! Remote view model (owned, Tetrad; docs/remote.md). The application fills [`RemoteView`];
//! the Tetrad › Remote page shows it and answers with [`crate::UiCommand::SetRemote`], and for a
//! received backup with [`crate::UiCommand::RemoteRestore`] or [`crate::UiCommand::RemoteDiscard`].

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteView {
    pub enabled: bool,
    /// The second switch: while Remote runs, the first row of the ⋮ menu shows the PIN.
    pub pin_in_menu: bool,
    /// The page address (`http://<ip>`) while the unit has a Wi-Fi lease.
    pub address: Option<String>,
    /// The PIN the page asks for, while Remote runs.
    pub pin: Option<String>,
    /// The last upload or the last problem, such as "Port 80 is in use".
    pub status: Option<String>,
    /// A backup that the page sent. The page offers RESTORE and DISCARD for it; nothing is
    /// restored before that touch.
    pub restore: Option<RemoteRestore>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteRestore {
    /// Which received backup this is: a new upload gets a new id, so a RESTORE confirmed for one
    /// backup never restores another.
    pub id: u64,
    /// The file name the page sent.
    pub name: String,
    /// As "34.2 MB".
    pub size: String,
    /// The restore runs.
    pub restoring: bool,
}

/// A short message from the application. The UI shows it as a toast each time `id` changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToastView {
    pub id: u64,
    pub text: String,
}
