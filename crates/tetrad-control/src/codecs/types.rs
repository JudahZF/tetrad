//! Descriptor-derived enums. No dispatcher policy is implied by these names.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown protobuf enum value {0}")]
pub struct UnknownEnumValue(pub i32);

pub trait ProtoEnum: Copy + Sized {
    fn from_number(number: i32) -> EnumValue<Self>;
    fn number(self) -> i32;
}

/// Proto3 enums are open. Unknown values remain numeric, never transmuted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnumValue<E> {
    Known(E),
    Unknown(i32),
}

impl<E: ProtoEnum> EnumValue<E> {
    pub fn number(self) -> i32 {
        match self {
            Self::Known(value) => value.number(),
            Self::Unknown(number) => number,
        }
    }
}

impl<E: ProtoEnum> From<E> for EnumValue<E> {
    fn from(value: E) -> Self {
        Self::Known(value)
    }
}

macro_rules! proto_enum {
    ($name:ident { $($variant:ident = $number:literal),* $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(i32)]
        pub enum $name { $($variant = $number,)* }

        impl TryFrom<i32> for $name {
            type Error = UnknownEnumValue;

            fn try_from(number: i32) -> Result<Self, Self::Error> {
                match number {
                    $($number => Ok(Self::$variant),)*
                    _ => Err(UnknownEnumValue(number)),
                }
            }
        }

        impl ProtoEnum for $name {
            fn from_number(number: i32) -> EnumValue<Self> {
                match Self::try_from(number) {
                    Ok(value) => EnumValue::Known(value),
                    Err(_) => EnumValue::Unknown(number),
                }
            }

            fn number(self) -> i32 {
                self as i32
            }
        }
    };
}

// ProductionAutomation.proto, ZenUI file offset 0x120dac8, nested Enum.
proto_enum!(CortexMessageType {
    Undefined = 0,
    Grid = 1,
    SetlistPosition = 2,
    IOSettings = 3,
    File = 4,
    IOMeter = 5,
    Tuner = 6,
    Diagnostics = 7,
    MIDISettings = 8,
    GeneralSettings = 9,
    Version = 10,
    ProductionAutomationMode = 11,
    GridMove = 12,
    Scene = 13,
    Mode = 14,
    RecallPreset = 15,
    EnableCaptureOut = 16,
    MasterVolume = 17,
    CloudLogin = 18,
    DefaultParameters = 19,
    RecentsFavorites = 20,
    UndoRedo = 21,
    SceneCopy = 22,
    SceneLabel = 23,
    ShowGigView = 24,
    Screenshot = 25,
    CPULoad = 26,
    ShowTuner = 27,
    Looper = 28,
    ProductForward = 29,
    BackupsForward = 30,
    LogsForward = 31,
    KeepAlive = 32,
    GlobalTempo = 33,
    PresetDirty = 34,
    ModuleStats = 35,
    NeuralCapture = 36,
    GridModelMeter = 37,
    GlobalEQ = 38,
    RecentSearches = 39,
    LocalBackup = 40,
    CloudBackup = 41,
    CompilerInhibitedModules = 42,
    SystemTimeSync = 43,
    Logs = 44,
    ProcessDownloadsQueue = 45,
    CloudProduct = 46,
    Confirmation = 47,
    SceneColor = 48,
    Connection = 49,
    NewModels = 50,
    ModelRepo = 51,
    ResetCommsBuffers = 52,
    SuspendConnection = 53,
    PinnedModels = 54,
    GigViewButton = 55,
    GenericError = 56,
    BulkOperation = 57,
    License = 58,
    PresetSpeedTest = 59,
    Updater = 60,
    UpdaterForward = 61,
    GainCalibration = 62,
    NeuralCapture2 = 63,
    Serialization = 64,
    TestFarm = 65,
    ProductionTest = 66,
    LoadAutomatedTestPreset = 67,
    SetTestPresetInputOutputPorts = 68,
    SetTestPresetSplitMixPoints = 69,
    GenerateTestPreset = 70,
    ModelPreset = 71,
    RemoteControl = 72,
    NumberOfMessageTypes = 73,
});

impl TryFrom<u32> for CortexMessageType {
    type Error = u32;

    fn try_from(number: u32) -> Result<Self, Self::Error> {
        i32::try_from(number)
            .ok()
            .and_then(|number| Self::try_from(number).ok())
            .ok_or(number)
    }
}

proto_enum!(MessageAction {
    Create = 0,
    Update = 1,
    Delete = 2,
    Read = 3,
    Move = 4,
    Copy = 5,
    Upload = 6,
    Download = 7,
    Swap = 8,
});

proto_enum!(LooperUpdateType {
    Meter = 0,
    Buttons = 1,
});

proto_enum!(SuspendReason {
    Backup = 0,
    FactoryReset = 1,
    SettingsReset = 2,
});

// HTTPOperation.Enum, used by ProductForwardRequest.operation.
proto_enum!(HttpOperation {
    Get = 0,
    Put = 1,
    Post = 2,
    Delete = 3,
});

// VersionMessage.DeviceType, nested in the descriptor's VersionMessage.
proto_enum!(VersionDeviceType {
    Qc = 0,
    Atma = 1,
});

// GenericErrorMessage.ErrorCode, nested in the descriptor's GenericErrorMessage.
proto_enum!(GenericErrorCode {
    VersionIsNewer = 0,
    VersionIsOlder = 1,
    CloudLoginFailed = 2,
    ExcessDownloadsPresetsWarningTriggered = 3,
    ExcessDownloadsCapturesWarningTriggered = 4,
});

proto_enum!(UpdaterCommand {
    Check = 0,
    CheckAndDownload = 1,
    Install = 2,
    RestartDownloadViaControl = 3,
    CancelDownload = 4,
});

proto_enum!(UpdaterStatus {
    None = 0,
    NewVersionAvailableToDownload = 1,
    NewVersionDownloaded = 2,
    NoNewVersionAvailable = 3,
});

proto_enum!(UpdaterState {
    Idle = 0,
    RequestingData = 1,
    DownloadingData = 2,
    Updating = 3,
    Reboot = 4,
    Failed = 5,
});

proto_enum!(BackupsForwardRequestType {
    CreateOrUpdate = 0,
    Download = 1,
    Rename = 2,
    Delete = 3,
    List = 4,
    FetchUrl = 5,
    UploadStatus = 6,
});

proto_enum!(LogsForwardRequestType {
    FetchUrls = 0,
    UploadLogs = 1,
    UploadCoredump = 2,
});

proto_enum!(UpdaterForwardRequestType {
    Check = 0,
    DownloadChangelog = 1,
    DownloadUpdateHeader = 2,
    DownloadUpdate = 3,
});

// RecallPresetReason.Enum, used by RecallPresetMessage.reason.
proto_enum!(RecallPresetReason {
    Other = 0,
    Undo = 1,
    Save = 2,
});

// GridMessage.UpdateType, nested in the descriptor's GridMessage.
proto_enum!(GridUpdateType {
    MeterUpdate = 0,
    ModelPreset = 1,
});

// RecentsFavoritesMessage.RecentFavoriteType.
proto_enum!(RecentFavoriteType {
    Preset = 0,
    ImpulseResponse = 1,
    NeuralCapture = 2,
});

proto_enum!(SceneBlockBypass {
    AlwaysOverwrite = 0,
    NonstompOverwrite = 1,
    NeverOverwrite = 2,
});

proto_enum!(CloudDomain {
    DefaultDomain = 0,
    AlternativeDomain = 1,
});

proto_enum!(PowerOptions {
    Shutdown = 0,
    Reboot = 1,
    Standby = 2,
    WakeUp = 3,
});

proto_enum!(AtmaPowerOnMode {
    PowerButton = 0,
    PowerOnWhenPluggedIn = 1,
});

proto_enum!(MidiClockOut {
    Off = 0,
    MidiDinOnly = 1,
    UsbMidiOnly = 2,
    BothUsbAndDinMidi = 3,
});

proto_enum!(StorageInfoType {
    Presets = 0,
    NeuralCaptures = 1,
    ImpulseResponses = 2,
});

// Descriptor spelling "SUCCEDED" is kept as Succeeded in Rust.
proto_enum!(CloudBackupErrorCode {
    BackupOperationInProgress = 0,
    BackupCreationFailed = 1,
    BackupCreationSucceeded = 2,
    BackupUpdateFailed = 3,
    BackupUpdateNotFound = 4,
    BackupUpdateSucceeded = 5,
    BackupRenameFailed = 6,
    BackupRenameNotFound = 7,
    BackupRenameSucceeded = 8,
    BackupDownloadFailed = 9,
    BackupDownloadNotFound = 10,
    BackupDownloadSucceeded = 11,
    BackupDeleteFailed = 12,
    BackupDeleteNotFound = 13,
    BackupDeleteSucceeded = 14,
    BackupListingFailed = 15,
    BackupListingSucceeded = 16,
    NotConnectedOrLoggedIn = 17,
    GridNotVisible = 18,
});

// CloudTransferStateItem.State.
proto_enum!(CloudTransferItemState {
    NotDownloaded = 0,
    Downloading = 1,
    Downloaded = 2,
    NotUploaded = 3,
    Uploading = 4,
    Uploaded = 5,
});

proto_enum!(ConfirmationAction {
    Ok = 0,
    Cancel = 1,
});

// LicenseMessage.LicenseUpdateStatus.
proto_enum!(LicenseUpdateStatus {
    Idle = 0,
    Updating = 1,
    Failed = 2,
});

// GainCalParameter.GainCalibrationOutput.
proto_enum!(GainCalibrationOutput {
    Out1Gain = 0,
    Out2Gain = 1,
    Out3Gain = 2,
    Out4Gain = 3,
    Send1 = 4,
    Send2 = 5,
    HpLGain = 6,
    HpRGain = 7,
});

// GainCalInputPortParameter.InputPortId, also used by the test-preset ports.
proto_enum!(GainCalInputPortId {
    Empty = 0,
    Input1 = 1,
    Input2 = 2,
    Input12 = 3,
    Return1 = 4,
    Return2 = 5,
    Return12 = 6,
    PrevRow = 7,
    UsbIn5 = 8,
    UsbIn6 = 9,
    UsbIn7 = 10,
    UsbIn8 = 11,
    UsbIn56 = 12,
    UsbIn78 = 13,
    SidechainBuffer = 14,
    MaxPorts = 15,
});

// GainCalOutputPortParameter.OutputPortId.
proto_enum!(GainCalOutputPortId {
    Empty = 0,
    Xlr12 = 1,
    Output34 = 2,
    Send12 = 3,
    Xlr1 = 4,
    Xlr2 = 5,
    Output3 = 6,
    Output4 = 7,
    Send1 = 8,
    Send2 = 9,
    UsbOut5 = 10,
    UsbOut6 = 11,
    UsbOut7 = 12,
    UsbOut8 = 13,
    UsbOut56 = 14,
    UsbOut78 = 15,
    NextRow3 = 16,
    NextRow4 = 17,
    NextRow34 = 18,
    MultipleOuts = 19,
    UsbOut3 = 20,
    UsbOut4 = 21,
    UsbOut34 = 22,
    MaxPorts = 23,
});

// SerializationMessage.Result.
proto_enum!(SerializationResult {
    Success = 0,
    IsSerialized = 1,
});

proto_enum!(TestFarmTestState {
    None = 0,
    Queued = 1,
    Running = 2,
    Passed = 3,
    Failed = 4,
    Aborting = 5,
    Aborted = 6,
});

// RemoteControlMouse.Type.
proto_enum!(RemoteControlMouseType {
    Press = 0,
    Release = 1,
    Move = 2,
    Tap = 3,
    Drag = 4,
});
