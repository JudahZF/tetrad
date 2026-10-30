//! Bluetooth view model (owned, Tetrad; docs/bluetooth.md). The application fills
//! [`BluetoothView`]; the Tetrad › Bluetooth page shows it and answers with
//! [`crate::UiCommand::SetBluetooth`] and [`crate::UiCommand::ForgetBluetooth`].

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BluetoothView {
    pub enabled: bool,
    /// A phone or computer may connect (BLE MIDI, the Tetrad app).
    pub host_mode: bool,
    /// MIDI over Bluetooth, in and out.
    pub midi: bool,
    /// The unit's Bluetooth name while the radio runs, such as "Quad Cortex 1FEB".
    pub name: Option<String>,
    /// Why Bluetooth does not run, or a step of its start, such as "Starting".
    pub status: Option<String>,
    /// The PIN that the Tetrad app asks for, while Bluetooth runs.
    pub pin: Option<String>,
    /// The phone or computer that is connected now: its name or address.
    pub host: Option<String>,
    /// The paired devices.
    pub devices: Vec<BluetoothDevice>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BluetoothDevice {
    /// The address, as the forget command names it (`AC:36:1B:FA:A0:2B`).
    pub address: String,
    /// Its name, or the address.
    pub label: String,
    /// What it is, such as "Phone or computer".
    pub kind: String,
    pub connected: bool,
}
