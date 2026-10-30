//! HCI over a UART (H4, Core Vol 4 Part A): the packet framing, the commands Tetrad sends and
//! the events it reads (Vol 4 Part E §7). Only the LE subset that a BT 4.0 LE-only controller
//! (the WILC3000) has.

/// An address as HCI carries it: least significant octet first.
pub type Address = [u8; 6];

/// H4 packet indicators.
const COMMAND: u8 = 0x01;
const ACL: u8 = 0x02;
const EVENT: u8 = 0x04;

/// One packet from the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// An event: code, then parameters.
    Event(Vec<u8>),
    /// ACL data: the 4-octet header, then the data.
    Acl(Vec<u8>),
}

/// The controller sent a byte that starts no packet: the stream is out of step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("HCI stream out of step: 0x{0:02x} starts no packet")]
pub struct Desync(pub u8);

/// Cuts the byte stream from the UART into packets.
#[derive(Debug, Default)]
pub struct H4Reader {
    buffer: Vec<u8>,
}

impl H4Reader {
    /// Appends `bytes` and returns the packets that are now complete. A byte that starts no
    /// packet drops the rest of the buffer: the caller resets the controller.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Packet>, Desync> {
        self.buffer.extend_from_slice(bytes);
        let mut packets = Vec::new();
        while let Some(&kind) = self.buffer.first() {
            let (header, length) = match kind {
                EVENT if self.buffer.len() >= 3 => (3, usize::from(self.buffer[2])),
                ACL if self.buffer.len() >= 5 => (
                    5,
                    usize::from(u16::from_le_bytes([self.buffer[3], self.buffer[4]])),
                ),
                EVENT | ACL => break,
                other => {
                    self.buffer.clear();
                    return Err(Desync(other));
                }
            };
            if self.buffer.len() < header + length {
                break;
            }
            let packet: Vec<u8> = self.buffer.drain(..header + length).skip(1).collect();
            packets.push(if kind == EVENT {
                Packet::Event(packet)
            } else {
                Packet::Acl(packet)
            });
        }
        Ok(packets)
    }
}

/// HCI command opcodes (OGF << 10 | OCF).
pub mod op {
    pub const DISCONNECT: u16 = 0x0406;
    pub const SET_EVENT_MASK: u16 = 0x0c01;
    pub const RESET: u16 = 0x0c03;
    pub const READ_LOCAL_VERSION: u16 = 0x1001;
    pub const READ_BD_ADDR: u16 = 0x1009;
    pub const LE_SET_EVENT_MASK: u16 = 0x2001;
    pub const LE_READ_BUFFER_SIZE: u16 = 0x2002;
    pub const LE_SET_ADVERTISING_PARAMETERS: u16 = 0x2006;
    pub const LE_SET_ADVERTISING_DATA: u16 = 0x2008;
    pub const LE_SET_SCAN_RESPONSE_DATA: u16 = 0x2009;
    pub const LE_SET_ADVERTISE_ENABLE: u16 = 0x200a;
    pub const LE_SET_SCAN_PARAMETERS: u16 = 0x200b;
    pub const LE_SET_SCAN_ENABLE: u16 = 0x200c;
    pub const LE_CREATE_CONNECTION: u16 = 0x200d;
    pub const LE_CREATE_CONNECTION_CANCEL: u16 = 0x200e;
    pub const LE_CONNECTION_UPDATE: u16 = 0x2013;
    pub const LE_START_ENCRYPTION: u16 = 0x2019;
    pub const LE_LTK_REPLY: u16 = 0x201a;
    pub const LE_LTK_NEGATIVE_REPLY: u16 = 0x201b;
}

/// One command as H4 bytes.
pub fn command(opcode: u16, parameters: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + parameters.len());
    out.push(COMMAND);
    out.extend_from_slice(&opcode.to_le_bytes());
    out.push(parameters.len() as u8);
    out.extend_from_slice(parameters);
    out
}

/// One ACL fragment as H4 bytes. `first` marks the start of an L2CAP PDU.
pub fn acl(handle: u16, first: bool, data: &[u8]) -> Vec<u8> {
    let flags: u16 = if first { 0x0000 } else { 0x1000 };
    let mut out = Vec::with_capacity(5 + data.len());
    out.push(ACL);
    out.extend_from_slice(&((handle & 0x0fff) | flags).to_le_bytes());
    out.extend_from_slice(&(data.len() as u16).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Status codes Tetrad reads or sends (Vol 1 Part F).
pub mod status {
    pub const SUCCESS: u8 = 0x00;
    pub const UNKNOWN_CONNECTION: u8 = 0x02;
    pub const PIN_OR_KEY_MISSING: u8 = 0x06;
    pub const CONNECTION_TIMEOUT: u8 = 0x08;
    pub const CONNECTION_LIMIT: u8 = 0x09;
    pub const COMMAND_DISALLOWED: u8 = 0x0c;
    pub const REMOTE_USER_TERMINATED: u8 = 0x13;
    pub const LOCAL_HOST_TERMINATED: u8 = 0x16;
    pub const UNKNOWN_CONNECTION_ID: u8 = 0x02;
    pub const AUTHENTICATION_FAILURE: u8 = 0x05;
    pub const UNKNOWN_COMMAND: u8 = 0x01;
}

/// The LE link role of the local controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    Central,
    Peripheral,
}

/// One advertising report from a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 0 ADV_IND, 1 ADV_DIRECT_IND, 2 ADV_SCAN_IND, 3 ADV_NONCONN_IND, 4 SCAN_RSP.
    pub kind: u8,
    pub address_type: u8,
    pub address: Address,
    pub data: Vec<u8>,
    pub rssi: i8,
}

/// The events Tetrad uses. Others are [`Event::Other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    CommandComplete {
        credits: u8,
        opcode: u16,
        /// The return parameters; the first octet is the status for most commands.
        parameters: Vec<u8>,
    },
    CommandStatus {
        status: u8,
        credits: u8,
        opcode: u16,
    },
    Disconnected {
        handle: u16,
        reason: u8,
    },
    EncryptionChange {
        status: u8,
        handle: u16,
        enabled: bool,
    },
    /// The link has a new key and stays encrypted (a bonded central encrypts again).
    KeyRefreshed {
        status: u8,
        handle: u16,
    },
    /// Data packets the controller has sent, per handle: their buffers are free again.
    CompletedPackets(Vec<(u16, u16)>),
    HardwareError(u8),
    Connected {
        status: u8,
        handle: u16,
        role: Role,
        peer_type: u8,
        peer: Address,
        interval: u16,
    },
    Advertising(Vec<Report>),
    ConnectionUpdated {
        handle: u16,
        interval: u16,
    },
    LtkRequest {
        handle: u16,
        rand: [u8; 8],
        ediv: u16,
    },
    Other(u8),
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

impl Event {
    /// Decodes an event packet (code, length, parameters). `None` for a truncated packet.
    pub fn parse(packet: &[u8]) -> Option<Self> {
        let (&code, rest) = packet.split_first()?;
        let p = rest.get(1..)?;
        Some(match code {
            0x0e => Self::CommandComplete {
                credits: *p.first()?,
                opcode: u16_at(p, 1)?,
                parameters: p.get(3..)?.to_vec(),
            },
            0x0f => Self::CommandStatus {
                status: *p.first()?,
                credits: *p.get(1)?,
                opcode: u16_at(p, 2)?,
            },
            0x05 => Self::Disconnected {
                handle: u16_at(p, 1)? & 0x0fff,
                reason: *p.get(3)?,
            },
            0x08 => Self::EncryptionChange {
                status: *p.first()?,
                handle: u16_at(p, 1)? & 0x0fff,
                enabled: *p.get(3)? != 0,
            },
            0x30 => Self::KeyRefreshed {
                status: *p.first()?,
                handle: u16_at(p, 1)? & 0x0fff,
            },
            0x13 => {
                let count = usize::from(*p.first()?);
                let pairs = (0..count)
                    .map(|i| Some((u16_at(p, 1 + i * 4)? & 0x0fff, u16_at(p, 3 + i * 4)?)))
                    .collect::<Option<Vec<_>>>()?;
                Self::CompletedPackets(pairs)
            }
            0x10 => Self::HardwareError(*p.first()?),
            0x3e => Self::parse_le(p)?,
            other => Self::Other(other),
        })
    }

    fn parse_le(p: &[u8]) -> Option<Self> {
        let (&sub, p) = p.split_first()?;
        Some(match sub {
            0x01 => {
                let mut peer = [0; 6];
                peer.copy_from_slice(p.get(5..11)?);
                Self::Connected {
                    status: *p.first()?,
                    handle: u16_at(p, 1)? & 0x0fff,
                    role: if *p.get(3)? == 0 {
                        Role::Central
                    } else {
                        Role::Peripheral
                    },
                    peer_type: *p.get(4)?,
                    peer,
                    interval: u16_at(p, 11)?,
                }
            }
            0x02 => {
                let count = usize::from(*p.first()?);
                let mut reports = Vec::with_capacity(count);
                let mut at = 1;
                for _ in 0..count {
                    let kind = *p.get(at)?;
                    let address_type = *p.get(at + 1)?;
                    let mut address = [0; 6];
                    address.copy_from_slice(p.get(at + 2..at + 8)?);
                    let length = usize::from(*p.get(at + 8)?);
                    let data = p.get(at + 9..at + 9 + length)?.to_vec();
                    let rssi = *p.get(at + 9 + length)? as i8;
                    at += 10 + length;
                    reports.push(Report {
                        kind,
                        address_type,
                        address,
                        data,
                        rssi,
                    });
                }
                Self::Advertising(reports)
            }
            0x03 => Self::ConnectionUpdated {
                handle: u16_at(p, 1)? & 0x0fff,
                interval: u16_at(p, 3)?,
            },
            0x05 => {
                let mut rand = [0; 8];
                rand.copy_from_slice(p.get(2..10)?);
                Self::LtkRequest {
                    handle: u16_at(p, 0)? & 0x0fff,
                    rand,
                    ediv: u16_at(p, 10)?,
                }
            }
            _ => Self::Other(0x3e),
        })
    }
}

/// The advertising data structures (AD) in `data`: `(type, value)`.
pub fn ad_structures(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(&length) = data.get(at) {
        let length = usize::from(length);
        if length == 0 || at + 1 + length > data.len() {
            break;
        }
        out.push((data[at + 1], &data[at + 2..at + 1 + length]));
        at += 1 + length;
    }
    out
}

/// The local name in advertising data, complete (0x09) or shortened (0x08).
pub fn ad_name(data: &[u8]) -> Option<String> {
    ad_structures(data)
        .into_iter()
        .find(|(kind, _)| matches!(kind, 0x08 | 0x09))
        .map(|(_, name)| String::from_utf8_lossy(name).into_owned())
}

/// The 128-bit service UUIDs in advertising data (0x06, 0x07), as UUIDs.
pub fn ad_services(data: &[u8]) -> Vec<crate::Uuid> {
    let mut out = Vec::new();
    for (kind, value) in ad_structures(data) {
        match kind {
            0x02 | 0x03 => out.extend(
                value
                    .chunks_exact(2)
                    .map(|c| crate::Uuid::Short(u16::from_le_bytes([c[0], c[1]]))),
            ),
            0x06 | 0x07 => out.extend(value.chunks_exact(16).map(|c| {
                let mut bytes = [0; 16];
                bytes.copy_from_slice(c);
                crate::Uuid::Long(bytes)
            })),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reader_cuts_packets_across_reads() {
        let mut reader = H4Reader::default();
        // HCI Reset complete, cut in the middle; then an ACL packet.
        assert_eq!(reader.push(&[0x04, 0x0e, 0x04]).unwrap(), vec![]);
        let packets = reader
            .push(&[
                0x0f, 0x03, 0x0c, 0x00, 0x02, 0x40, 0x20, 0x02, 0x00, 0xaa, 0xbb,
            ])
            .unwrap();
        assert_eq!(
            packets,
            vec![
                Packet::Event(vec![0x0e, 0x04, 0x0f, 0x03, 0x0c, 0x00]),
                Packet::Acl(vec![0x40, 0x20, 0x02, 0x00, 0xaa, 0xbb]),
            ]
        );
        assert_eq!(reader.push(&[0x00, 0x04]), Err(Desync(0x00)));
    }

    #[test]
    fn events_decode() {
        assert_eq!(
            Event::parse(&[0x0e, 0x04, 0x0f, 0x03, 0x0c, 0x00]),
            Some(Event::CommandComplete {
                credits: 0x0f,
                opcode: op::RESET,
                parameters: vec![0x00]
            })
        );
        // The LE Connection Complete the WILC3000 sent as a peripheral (2026-10-28).
        let connected = [
            0x3e, 0x13, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x2b, 0xa0, 0xfa, 0x1b, 0x36, 0xac,
            0x24, 0x00, 0x00, 0x00, 0x2a, 0x00, 0x00,
        ];
        assert_eq!(
            Event::parse(&connected),
            Some(Event::Connected {
                status: 0,
                handle: 0,
                role: Role::Peripheral,
                peer_type: 0,
                peer: [0x2b, 0xa0, 0xfa, 0x1b, 0x36, 0xac],
                interval: 0x24,
            })
        );
        assert_eq!(
            Event::parse(&[0x05, 0x04, 0x00, 0x00, 0x00, 0x13]),
            Some(Event::Disconnected {
                handle: 0,
                reason: 0x13
            })
        );
        assert_eq!(
            Event::parse(&[0x13, 0x05, 0x01, 0x40, 0x00, 0x02, 0x00]),
            Some(Event::CompletedPackets(vec![(0x40, 2)]))
        );
        assert_eq!(Event::parse(&[0x0e, 0x04, 0x0f]), None);
    }

    #[test]
    fn commands_and_acl_frame() {
        assert_eq!(command(op::RESET, &[]), [0x01, 0x03, 0x0c, 0x00]);
        assert_eq!(
            acl(0x40, true, &[1, 2]),
            [0x02, 0x40, 0x00, 0x02, 0x00, 1, 2]
        );
        assert_eq!(acl(0x40, false, &[3]), [0x02, 0x40, 0x10, 0x01, 0x00, 3]);
    }

    #[test]
    fn advertising_data_names_and_services() {
        let data = [
            0x02, 0x01, 0x06, 0x03, 0x03, 0x12, 0x18, 0x05, 0x09, b'P', b'e', b'd', b'l',
        ];
        assert_eq!(ad_name(&data).as_deref(), Some("Pedl"));
        assert_eq!(ad_services(&data), vec![crate::Uuid::Short(0x1812)]);
    }
}
