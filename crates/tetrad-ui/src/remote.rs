//! Remote view model (owned, Tetrad; docs/remote.md). The application fills [`RemoteView`];
//! the System › Remote page shows it and answers with [`crate::UiCommand::SetRemote`].

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteView {
    pub enabled: bool,
    /// The page address (`http://<ip>`) while the unit has a Wi-Fi lease.
    pub address: Option<String>,
    /// The PIN the page asks for, while Remote runs.
    pub pin: Option<String>,
    /// The last upload or the last problem, such as "Port 80 is in use".
    pub status: Option<String>,
}

/// A short message from the application. The UI shows it as a toast each time `id` changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToastView {
    pub id: u64,
    pub text: String,
}
