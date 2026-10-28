//! Bluetooth (owned, Tetrad; docs/bluetooth.md): the device side of the BLE host that the
//! platform runs on a thread ([`BluetoothBackend`]). This bridge keeps the switches
//! (`bluetooth.json`) and the bonds (`bluetooth-bonds.json`), routes BLE MIDI through the MIDI
//! bridge, and lets new devices pair only while the Bluetooth page is open.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tetrad_ble::bond::{self, Kind};
use tetrad_ble::{Address, Bond, Command, Config, Event};
use tetrad_ui::{BluetoothDevice, BluetoothView, Screen, SettingsPage};

use super::platform::{Platform, ServiceBackends};
use super::{Device, Effect, Subsystem, Timer};

/// The switches.
pub const SETTINGS_FILE: &str = "/media/p4/tetrad/bluetooth.json";
/// The bonds: the keys of the paired devices. The support file never takes it.
pub const BONDS_FILE: &str = "/media/p4/tetrad/bluetooth-bonds.json";
const MAX_SETTINGS_BYTES: u64 = 4 * 1024;
const MAX_BONDS_BYTES: u64 = 256 * 1024;
/// After a fault, Bluetooth starts again after this, at most [`MAX_RESTARTS`] times in a row.
const RESTART_DELAY: Duration = Duration::from_secs(5);
const MAX_RESTARTS: u8 = 3;

/// The BLE host of the platform. Its events arrive as [`super::platform::Input::Bluetooth`].
pub trait BluetoothBackend {
    /// Starts the controller and the host. `Err` is the reason for the page.
    fn start(&mut self, config: Config, bonds: Vec<Bond>) -> Result<(), String>;
    /// Ends the links and stops advertising.
    fn stop(&mut self);
    fn send(&mut self, command: Command);
}

fn on() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
struct Stored {
    enabled: bool,
    #[serde(default = "on")]
    host_mode: bool,
    #[serde(default = "on")]
    midi: bool,
}

#[derive(Debug, Default)]
pub struct Bluetooth {
    /// The files were read (at the first preset load).
    loaded: bool,
    enabled: bool,
    host_mode: bool,
    midi: bool,
    /// The backend runs.
    running: bool,
    /// The unit's name, once the controller is set up.
    name: Option<String>,
    status: Option<String>,
    bonds: Vec<Bond>,
    /// The phone or computer connected now.
    host: Option<Address>,
    /// A host listens for MIDI notifications.
    listening: bool,
    /// New devices may pair: the Bluetooth page is open.
    pairable: bool,
    restarts: u8,
}

impl Bluetooth {
    pub fn view(&self) -> BluetoothView {
        let label = |address: &Address| {
            (self.bonds.iter())
                .find(|bond| bond.address == *address)
                .map_or_else(|| bond::format_address(address), Bond::label)
        };
        BluetoothView {
            enabled: self.enabled,
            host_mode: self.host_mode,
            midi: self.midi,
            name: self.name.clone(),
            status: self.status.clone(),
            host: self.host.as_ref().map(label),
            devices: (self.bonds.iter())
                .map(|bond| BluetoothDevice {
                    address: bond::format_address(&bond.address),
                    label: bond.label(),
                    kind: match bond.kind {
                        Kind::Host => "Phone or computer",
                        Kind::Device => "Device",
                    }
                    .to_owned(),
                    connected: self.host == Some(bond.address),
                })
                .collect(),
        }
    }

    /// MIDI goes out over BLE: a host listens and MIDI over Bluetooth is on.
    pub fn midi_out(&self) -> bool {
        self.listening && self.midi
    }

    /// The support file's view: the switches and the state, no keys.
    pub fn support_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "enabled={} host_mode={} midi={} running={}",
                self.enabled, self.host_mode, self.midi, self.running
            ),
            format!("name={}", self.name.as_deref().unwrap_or("-")),
            format!("status={}", self.status.as_deref().unwrap_or("-")),
            format!("host_connected={}", self.host.is_some()),
        ];
        for bond in &self.bonds {
            let kind = match bond.kind {
                Kind::Host => "host",
                Kind::Device => "device",
            };
            let secure = if bond.secure { "secure" } else { "legacy" };
            lines.push(format!("bond {kind} {secure} {}", bond.label()));
        }
        lines
    }
}

impl<P: Platform> Device<P> {
    /// Reads the files once and starts Bluetooth when it is on. Called at the first preset load,
    /// as Remote is: start-up never waits for it.
    pub(super) fn load_bluetooth(&mut self) {
        if std::mem::replace(&mut self.bridges.bluetooth.loaded, true) {
            return;
        }
        use tetrad_services::exec::Files;
        let files = self.platform.services().files();
        let settings = files.read_bounded(Path::new(SETTINGS_FILE), MAX_SETTINGS_BYTES);
        let bonds = files.read_bounded(Path::new(BONDS_FILE), MAX_BONDS_BYTES);
        let stored = match settings {
            Ok(Some(bytes)) => serde_json::from_slice::<Stored>(&bytes).map_err(|e| e.to_string()),
            Ok(None) => Ok(Stored {
                enabled: false,
                host_mode: true,
                midi: true,
            }),
            Err(error) => Err(error.to_string()),
        };
        let stored = stored.unwrap_or_else(|error| {
            self.notice(Subsystem::Bluetooth, format!("{SETTINGS_FILE}: {error}"));
            Stored {
                enabled: false,
                host_mode: true,
                midi: true,
            }
        });
        let bonds = match bonds {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                self.notice(Subsystem::Bluetooth, format!("{BONDS_FILE}: {error}"));
                Vec::new()
            }),
            Ok(None) => Vec::new(),
            Err(error) => {
                self.notice(Subsystem::Bluetooth, format!("{BONDS_FILE}: {error}"));
                Vec::new()
            }
        };
        let bluetooth = &mut self.bridges.bluetooth;
        bluetooth.enabled = stored.enabled;
        bluetooth.host_mode = stored.host_mode;
        bluetooth.midi = stored.midi;
        bluetooth.bonds = bonds;
        if stored.enabled {
            self.start_bluetooth();
        }
    }

    /// The Bluetooth page switches, saved in `bluetooth.json`.
    pub(super) fn set_bluetooth(&mut self, enabled: bool, host_mode: bool, midi: bool) {
        let bluetooth = &mut self.bridges.bluetooth;
        let before = (bluetooth.enabled, bluetooth.host_mode, bluetooth.midi);
        if before == (enabled, host_mode, midi) {
            return;
        }
        bluetooth.enabled = enabled;
        bluetooth.host_mode = host_mode;
        bluetooth.midi = midi;
        match (before.0, enabled) {
            (false, true) => {
                self.bridges.bluetooth.restarts = 0;
                self.start_bluetooth();
            }
            (true, false) => self.stop_bluetooth(None),
            _ if host_mode != before.1 => self.bluetooth_command(Command::SetAdvertise(host_mode)),
            _ => {}
        }
        self.sync_bluetooth_midi();
        use tetrad_services::exec::Files;
        let bytes = serde_json::to_vec(&Stored {
            enabled,
            host_mode,
            midi,
        })
        .unwrap_or_default();
        let files = self.platform.services().files();
        let path = Path::new(SETTINGS_FILE);
        let saved = (path.parent())
            .map_or(Ok(()), |dir| files.create_dirs(dir))
            .and_then(|()| files.write_atomic(path, &bytes));
        if let Err(error) = saved {
            self.notice(
                Subsystem::Bluetooth,
                format!("Bluetooth not saved: {error}"),
            );
        }
    }

    /// FORGET on the Bluetooth page.
    pub(super) fn forget_bluetooth(&mut self, address: &str) {
        let Some(address) = bond::parse_address(address) else {
            return;
        };
        self.bridges
            .bluetooth
            .bonds
            .retain(|bond| bond.address != address);
        self.bluetooth_command(Command::Forget(address));
        self.save_bonds();
    }

    fn bluetooth_command(&mut self, command: Command) {
        if !self.bridges.bluetooth.running {
            return;
        }
        if let Some(backend) = self.platform.bluetooth() {
            backend.send(command);
        }
    }

    fn start_bluetooth(&mut self) {
        let bluetooth = &self.bridges.bluetooth;
        let config = Config {
            name: None,
            advertise: bluetooth.host_mode,
            pairable: bluetooth.pairable,
            link: false,
        };
        let bonds = bluetooth.bonds.clone();
        let started = match self.platform.bluetooth() {
            Some(backend) => backend.start(config, bonds),
            None => Err("Bluetooth does not run in this mode".to_owned()),
        };
        let bluetooth = &mut self.bridges.bluetooth;
        match started {
            Ok(()) => {
                bluetooth.running = true;
                bluetooth.status = Some("Starting".to_owned());
            }
            Err(reason) => {
                bluetooth.status = Some(reason.clone());
                self.notice(Subsystem::Bluetooth, format!("not started: {reason}"));
            }
        }
    }

    fn stop_bluetooth(&mut self, status: Option<String>) {
        if let Some(backend) = self.platform.bluetooth() {
            backend.stop();
        }
        let bluetooth = &mut self.bridges.bluetooth;
        bluetooth.running = false;
        bluetooth.name = None;
        bluetooth.host = None;
        bluetooth.listening = false;
        bluetooth.status = status;
        self.sync_bluetooth_midi();
    }

    /// MIDI out follows the listening host and the MIDI switch.
    fn sync_bluetooth_midi(&mut self) {
        let on = self.bridges.bluetooth.midi_out();
        self.bridges.midi.set_ble_out(on);
    }

    /// New devices may pair only while the Bluetooth page is open. Called once per loop pass.
    pub(super) fn sync_bluetooth_pairable(&mut self) {
        let open = self.bridges.ui.view().screen() == Screen::Settings(SettingsPage::Bluetooth);
        let bluetooth = &mut self.bridges.bluetooth;
        if open != bluetooth.pairable {
            bluetooth.pairable = open;
            self.bluetooth_command(Command::SetPairable(open));
        }
    }

    /// [`Timer::Bluetooth`]: the restart after a fault.
    pub(super) fn restart_bluetooth(&mut self) {
        let bluetooth = &self.bridges.bluetooth;
        if bluetooth.enabled && !bluetooth.running {
            self.start_bluetooth();
        }
    }

    fn save_bonds(&mut self) {
        use tetrad_services::exec::Files;
        let bytes = serde_json::to_vec_pretty(&self.bridges.bluetooth.bonds).unwrap_or_default();
        let files = self.platform.services().files();
        let path = Path::new(BONDS_FILE);
        let saved = (path.parent())
            .map_or(Ok(()), |dir| files.create_dirs(dir))
            .and_then(|()| files.write_atomic(path, &bytes));
        if let Err(error) = saved {
            self.notice(Subsystem::Bluetooth, format!("bonds not saved: {error}"));
        }
    }

    /// One event of the BLE host.
    pub(super) fn on_bluetooth(&mut self, event: Event, now: Duration) -> Vec<Effect> {
        if !self.bridges.bluetooth.running {
            return Vec::new();
        }
        match event {
            Event::Ready { name, .. } => {
                let bluetooth = &mut self.bridges.bluetooth;
                bluetooth.restarts = 0;
                bluetooth.status = None;
                bluetooth.name = Some(name.clone());
                self.notice(Subsystem::Bluetooth, format!("on as {name}"));
            }
            Event::Fault(reason) => {
                self.notice(Subsystem::Bluetooth, format!("stopped: {reason}"));
                let restarts = self.bridges.bluetooth.restarts;
                if restarts < MAX_RESTARTS {
                    self.bridges.bluetooth.restarts += 1;
                    self.stop_bluetooth(Some("Restarting".to_owned()));
                    self.start_timer(Timer::Bluetooth, RESTART_DELAY);
                } else {
                    let status = format!("Stopped: {reason}. Turn Bluetooth off and on.");
                    self.stop_bluetooth(Some(status));
                }
            }
            Event::HostConnected { address, bonded } => {
                self.bridges.bluetooth.host = Some(address);
                let known = if bonded { "paired" } else { "new" };
                let text = format!("a {known} host connected");
                self.notice(Subsystem::Bluetooth, text);
            }
            Event::HostDisconnected { reason, .. } => {
                self.bridges.bluetooth.host = None;
                let text = format!("the host left (0x{reason:02x})");
                self.notice(Subsystem::Bluetooth, text);
            }
            Event::Bonded(bond) => {
                let bonds = &mut self.bridges.bluetooth.bonds;
                match bonds.iter().position(|known| known.address == bond.address) {
                    Some(index) => bonds[index] = bond,
                    None => {
                        let text = format!("paired with {}", bond.label());
                        bonds.push(bond);
                        self.notice(Subsystem::Bluetooth, text);
                    }
                }
                self.save_bonds();
            }
            Event::PairingFailed { reason, refused } => {
                if refused {
                    let text = "To pair a new device, open Settings › Tetrad › Bluetooth";
                    self.bridges.remote.show_toast(text.to_owned());
                }
                let text = format!("a pairing failed (0x{reason:02x})");
                self.notice(Subsystem::Bluetooth, text);
            }
            Event::Midi(message) => {
                // An update install or a backup restore drops MIDI input, as for USB and DIN.
                let held = self.bridges.services.inputs_masked() || self.restore_hold();
                if self.bridges.bluetooth.midi && !held {
                    let effects = self.bridges.midi.on_ble(&message, &mut self.state, now);
                    return effects.unwrap_or_else(|error| {
                        self.notice(Subsystem::Midi, error.to_string());
                        Vec::new()
                    });
                }
            }
            Event::MidiListening(on) => {
                self.bridges.bluetooth.listening = on;
                self.sync_bluetooth_midi();
            }
            Event::Notice(text) => self.notice(Subsystem::Bluetooth, text),
        }
        Vec::new()
    }

    /// One MIDI message from the MIDI bridge for the BLE hosts.
    pub(super) fn bluetooth_midi(&mut self, message: Vec<u8>) {
        self.bluetooth_command(Command::SendMidi(message));
    }
}
