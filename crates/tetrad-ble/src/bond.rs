//! Bonds: the keys of a paired peer, so that it encrypts again without a new pairing. The
//! device keeps them in a file; the host only reads and makes them.

use serde::{Deserialize, Serialize};

use crate::crypto::Key;
use crate::hci::Address;

/// What the peer is to Tetrad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A phone or computer that connects to Tetrad (Host mode).
    Host,
    /// A device that Tetrad connects to (My devices).
    Device,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bond {
    /// The identity address, written as usual (`AC:36:1B:FA:A0:2B`).
    #[serde(with = "address")]
    pub address: Address,
    /// 0 public, 1 random static.
    pub address_type: u8,
    pub kind: Kind,
    /// The peer's identity resolving key, when it gave one: it resolves its private addresses.
    #[serde(default, with = "hex_option")]
    pub irk: Option<Key>,
    /// The long-term key, as HCI carries it.
    #[serde(with = "hex")]
    pub ltk: Key,
    /// Legacy pairing: the EDIV and Rand that name `ltk`. Both 0 for LE Secure Connections.
    #[serde(default)]
    pub ediv: u16,
    #[serde(default, with = "hex")]
    pub rand: [u8; 8],
    /// LE Secure Connections made the key.
    #[serde(default)]
    pub secure: bool,
    /// The peer's GAP device name, when Tetrad could read it.
    #[serde(default)]
    pub name: Option<String>,
    /// A host's notification switches (CCCD bits, [`crate::att::Notify`]), kept across links.
    #[serde(default)]
    pub notify: u8,
    /// The attribute table digest that the host saw ([`crate::att::Database::digest`]).
    #[serde(default)]
    pub digest: u32,
    /// A host whose Tetrad app gave the Bluetooth PIN. The device sets it; the host keeps it.
    #[serde(default)]
    pub app: bool,
}

impl Bond {
    /// The name to show: the device name, else the address.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format_address(&self.address))
    }
}

/// `AC:36:1B:FA:A0:2B` for an address as HCI carries it.
pub fn format_address(address: &Address) -> String {
    let parts: Vec<String> = address.iter().rev().map(|b| format!("{b:02X}")).collect();
    parts.join(":")
}

pub fn parse_address(text: &str) -> Option<Address> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut address = [0; 6];
    for (i, part) in parts.iter().rev().enumerate() {
        if part.len() != 2 {
            return None;
        }
        address[i] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(address)
}

mod address {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(address: &[u8; 6], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::format_address(address))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 6], D::Error> {
        let text = String::deserialize(d)?;
        super::parse_address(&text).ok_or_else(|| D::Error::custom("not an address"))
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

mod hex {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer, const N: usize>(
        bytes: &[u8; N],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::to_hex(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<[u8; N], D::Error> {
        let text = String::deserialize(d)?;
        super::from_hex(&text).ok_or_else(|| D::Error::custom("not a key"))
    }
}

mod hex_option {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &Option<[u8; 16]>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => s.serialize_some(&super::to_hex(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[u8; 16]>, D::Error> {
        let text = Option::<String>::deserialize(d)?;
        text.map(|text| super::from_hex(&text).ok_or_else(|| D::Error::custom("not a key")))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bond_round_trips_through_json_with_readable_addresses() {
        let bond = Bond {
            address: [0x2b, 0xa0, 0xfa, 0x1b, 0x36, 0xac],
            address_type: 0,
            kind: Kind::Host,
            irk: Some([7; 16]),
            ltk: [1; 16],
            ediv: 0,
            rand: [0; 8],
            secure: true,
            name: Some("zevlor".into()),
            notify: 2,
            digest: 9,
            app: true,
        };
        let json = serde_json::to_string(&bond).unwrap();
        assert!(json.contains("\"AC:36:1B:FA:A0:2B\""), "{json}");
        assert_eq!(serde_json::from_str::<Bond>(&json).unwrap(), bond);
        assert_eq!(parse_address("ac:36:1b:fa:a0:2b"), Some(bond.address));
        assert_eq!(parse_address("ac:36"), None);
    }
}
