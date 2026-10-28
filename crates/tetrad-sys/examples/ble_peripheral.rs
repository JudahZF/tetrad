//! Dev check on a unit (not shipped): runs the Bluetooth host as a BLE MIDI peripheral, prints
//! its events, answers each MIDI message with the same message, and sends a CC every 2 s.
//! Bonds go to `/tmp/ble-bonds.json`. Stop Tetrad's own Bluetooth first.

use std::time::{Duration, Instant};

use tetrad_ble::{Bond, Command, Config, Event};
use tetrad_sys::ble::Bluetooth;

const BONDS: &str = "/tmp/ble-bonds.json";

fn main() {
    let mut bonds: Vec<Bond> = std::fs::read(BONDS)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let config = Config {
        name: None,
        advertise: true,
        pairable: true,
        link: false,
    };
    let mut bluetooth = Bluetooth::start(config, bonds.clone()).expect("start");
    let mut next_cc = Instant::now() + Duration::from_secs(2);
    let mut value = 0u8;
    loop {
        for event in bluetooth.events() {
            println!("{event:?}");
            match event {
                Event::Midi(message) => bluetooth.send(Command::SendMidi(message)),
                Event::Bonded(bond) => {
                    bonds.retain(|known| known.address != bond.address);
                    bonds.push(bond);
                    let _ = std::fs::write(BONDS, serde_json::to_vec_pretty(&bonds).unwrap());
                }
                Event::Fault(_) => return,
                _ => {}
            }
        }
        if Instant::now() >= next_cc {
            next_cc += Duration::from_secs(2);
            value = (value + 16) % 128;
            bluetooth.send(Command::SendMidi(vec![0xb0, 20, value]));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
