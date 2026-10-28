//! Tetrad's Bluetooth Low Energy host (docs/bluetooth.md), for the BLE controller in the Quad
//! Cortex's WILC3000 (Bluetooth 4.0, LE only, HCI over a UART).
//!
//! The crate does no I/O. [`Host`] is a state machine: the platform gives it the bytes from the
//! UART and the time, and writes the bytes it returns. Its [`Event`]s go to the device; the
//! device answers with the methods of [`Host`]. Layers: [`hci`] (H4 framing, commands, events),
//! [`l2cap`], [`att`] (the GATT server and a small client), [`smp`] (Just Works pairing, LE
//! Secure Connections and legacy, with [`crypto`]) and [`midi`] (the BLE MIDI packet format).
#![forbid(unsafe_code)]

pub mod att;
pub mod bond;
pub mod crypto;
pub mod hci;
mod host;
pub mod l2cap;
pub mod link;
pub mod midi;
pub mod smp;

pub use bond::Bond;
pub use hci::Address;
pub use host::{Command, Config, Event, Host, Rng};

/// A Bluetooth UUID. A 16-bit UUID is the base UUID with those bits; both forms compare equal.
#[derive(Debug, Clone, Copy, Eq)]
pub enum Uuid {
    Short(u16),
    /// Least significant octet first, as ATT carries it.
    Long([u8; 16]),
}

/// `00000000-0000-1000-8000-00805F9B34FB`.
const BASE: u128 = 0x0000_0000_0000_1000_8000_0080_5f9b_34fb;

impl Uuid {
    pub const fn from_u128(value: u128) -> Self {
        Self::Long(value.to_le_bytes())
    }

    pub fn as_u128(self) -> u128 {
        match self {
            Self::Short(short) => BASE | (u128::from(short) << 96),
            Self::Long(bytes) => u128::from_le_bytes(bytes),
        }
    }

    /// The 16-bit form, when the UUID has one.
    pub fn short(self) -> Option<u16> {
        let value = self.as_u128();
        (value & !(0xffff_u128 << 96) == BASE).then_some((value >> 96) as u16)
    }

    /// The bytes ATT carries: 2 for a 16-bit UUID, else 16.
    pub fn to_bytes(self) -> Vec<u8> {
        match self.short() {
            Some(short) => short.to_le_bytes().to_vec(),
            None => self.as_u128().to_le_bytes().to_vec(),
        }
    }

    /// From 2 or 16 bytes as ATT carries them.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        match bytes.len() {
            2 => Some(Self::Short(u16::from_le_bytes([bytes[0], bytes[1]]))),
            16 => {
                let mut long = [0; 16];
                long.copy_from_slice(bytes);
                Some(Self::Long(long))
            }
            _ => None,
        }
    }
}

impl PartialEq for Uuid {
    fn eq(&self, other: &Self) -> bool {
        self.as_u128() == other.as_u128()
    }
}

impl std::hash::Hash for Uuid {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_u128().hash(state);
    }
}

impl std::fmt::Display for Uuid {
    /// The usual form: `0000ffe0-0000-1000-8000-00805f9b34fb`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = self.as_u128();
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            v >> 96,
            (v >> 80) & 0xffff,
            (v >> 64) & 0xffff,
            (v >> 48) & 0xffff,
            v & 0xffff_ffff_ffff
        )
    }
}

impl std::str::FromStr for Uuid {
    type Err = String;

    /// The usual form with dashes, or 4 hexadecimal digits for a 16-bit UUID.
    fn from_str(text: &str) -> Result<Self, String> {
        let hex: String = text.chars().filter(|c| *c != '-').collect();
        let bad = || format!("{text:?} is not a UUID");
        match hex.len() {
            4 => u16::from_str_radix(&hex, 16)
                .map(Self::Short)
                .map_err(|_| bad()),
            32 if text.len() == 36 || text.len() == 32 => u128::from_str_radix(&hex, 16)
                .map(Self::from_u128)
                .map_err(|_| bad()),
            _ => Err(bad()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Uuid;

    #[test]
    fn uuids_compare_across_forms_and_print_as_usual() {
        let long: Uuid = "0000180f-0000-1000-8000-00805f9b34fb".parse().unwrap();
        assert_eq!(long, Uuid::Short(0x180f));
        assert_eq!(long.to_bytes(), vec![0x0f, 0x18]);
        assert_eq!(crate::midi::SERVICE.to_bytes().len(), 16);
        assert_eq!(
            crate::midi::SERVICE.to_string(),
            "03b80e5a-ede8-4b33-a751-6ce34ec4c700"
        );
        assert_eq!("2a00".parse::<Uuid>(), Ok(Uuid::Short(0x2a00)));
        assert!("not-a-uuid".parse::<Uuid>().is_err());
    }
}
