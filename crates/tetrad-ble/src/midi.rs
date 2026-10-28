//! BLE MIDI (the MIDI Association's "MIDI over Bluetooth Low Energy" 1.0): the service, and the
//! packet format of its one characteristic.
//!
//! A packet is a header byte (`10tttttt`, the high 6 bits of a 13-bit millisecond timestamp),
//! then messages. Each message starts with a timestamp byte (`1ttttttt`, the low 7 bits) and its
//! status. Running status may leave out the status, and the timestamp byte too when the time is
//! the same. A SysEx that does not fit goes on in the next packets, which then start with data
//! bytes, and ends with a timestamp byte before `F7`. Real-time bytes may sit inside a SysEx.

use crate::Uuid;

/// The BLE MIDI service, `03B80E5A-EDE8-4B33-A751-6CE34EC4C700`.
pub const SERVICE: Uuid = Uuid::from_u128(0x03b80e5a_ede8_4b33_a751_6ce34ec4c700);
/// Its MIDI I/O characteristic, `7772E5DB-3868-4112-A1A9-F2669D106BF3`.
pub const IO: Uuid = Uuid::from_u128(0x7772e5db_3868_4112_a1a9_f2669d106bf3);

/// The data bytes after a status byte; SysEx (`F0`) is open-ended.
fn data_length(status: u8) -> usize {
    match status {
        0x80..=0xbf | 0xe0..=0xef => 2,
        0xc0..=0xdf => 1,
        0xf1 | 0xf3 => 1,
        0xf2 => 2,
        _ => 0,
    }
}

/// Turns BLE MIDI packets into MIDI byte messages, each with its own status. Keeps the running
/// status and an open SysEx from one packet to the next.
#[derive(Debug, Default)]
pub struct Decoder {
    running: Option<u8>,
    sysex: Option<Vec<u8>>,
}

/// The longest SysEx the decoder keeps; a longer one is dropped.
pub const MAX_SYSEX: usize = 4096;

impl Decoder {
    /// The messages in one packet. A malformed packet gives the messages before the fault and
    /// clears the running status.
    pub fn packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let Some((&header, body)) = packet.split_first() else {
            return out;
        };
        if header & 0xc0 != 0x80 {
            self.running = None;
            return out;
        }
        let mut at = 0;
        while at < body.len() {
            // An open SysEx takes data bytes with no timestamp: at the start of a packet, and
            // after a real-time byte inside it.
            if let Some(sysex) = &mut self.sysex
                && body[at] < 0x80
            {
                while let Some(&byte) = body.get(at).filter(|b| **b < 0x80) {
                    sysex.push(byte);
                    at += 1;
                }
                if sysex.len() > MAX_SYSEX {
                    self.sysex = None;
                }
                continue;
            }
            // A timestamp, or data bytes of the running status at the same time.
            if body[at] >= 0x80 {
                at += 1;
            }
            let Some(&byte) = body.get(at) else {
                break;
            };
            if byte < 0x80 {
                let Some(status) = self.running.filter(|_| self.sysex.is_none()) else {
                    self.running = None;
                    return out;
                };
                let length = data_length(status);
                let Some(data) = body
                    .get(at..at + length)
                    .filter(|d| d.iter().all(|b| *b < 0x80))
                else {
                    self.running = None;
                    return out;
                };
                let mut message = vec![status];
                message.extend_from_slice(data);
                out.push(message);
                at += length;
                continue;
            }
            at += 1;
            match byte {
                0xf8..=0xff => out.push(vec![byte]),
                0xf7 => {
                    if let Some(mut sysex) = self.sysex.take() {
                        sysex.push(0xf7);
                        if sysex.len() <= MAX_SYSEX {
                            out.push(sysex);
                        }
                    }
                }
                0xf0 => {
                    self.running = None;
                    self.sysex = Some(vec![0xf0]);
                }
                status => {
                    // A status ends an unfinished SysEx.
                    self.sysex = None;
                    self.running = (status < 0xf0).then_some(status);
                    let length = data_length(status);
                    let Some(data) = body
                        .get(at..at + length)
                        .filter(|d| d.iter().all(|b| *b < 0x80))
                    else {
                        self.running = None;
                        return out;
                    };
                    let mut message = vec![status];
                    message.extend_from_slice(data);
                    out.push(message);
                    at += length;
                }
            }
        }
        out
    }
}

/// The packets for one MIDI message (status first, complete) at `millis`, each at most `max`
/// bytes (the ATT MTU less 3). Most messages give one packet; a long SysEx gives several.
pub fn encode(message: &[u8], millis: u64, max: usize) -> Vec<Vec<u8>> {
    let max = max.max(5);
    let header = 0x80 | ((millis >> 7) & 0x3f) as u8;
    let stamp = 0x80 | (millis & 0x7f) as u8;
    let Some((&status, rest)) = message.split_first() else {
        return Vec::new();
    };
    if status != 0xf0 {
        let mut packet = vec![header, stamp];
        packet.extend_from_slice(message);
        return vec![packet];
    }
    // SysEx: F0 and data, then the end with its own timestamp byte.
    let data = rest.strip_suffix(&[0xf7]).unwrap_or(rest);
    let mut packets = Vec::new();
    let mut packet = vec![header, stamp, 0xf0];
    for &byte in data {
        if packet.len() == max {
            packets.push(std::mem::replace(&mut packet, vec![header]));
        }
        packet.push(byte);
    }
    if packet.len() + 2 > max {
        packets.push(std::mem::replace(&mut packet, vec![header]));
    }
    packet.extend_from_slice(&[stamp, 0xf7]);
    packets.push(packet);
    packets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_and_running_status_messages_decode() {
        let mut d = Decoder::default();
        // Note on, then two more under running status: one with its own timestamp, one without.
        let packet = [0x80, 0x81, 0x90, 60, 100, 0x82, 62, 100, 64, 100];
        assert_eq!(
            d.packet(&packet),
            vec![
                vec![0x90, 60, 100],
                vec![0x90, 62, 100],
                vec![0x90, 64, 100]
            ]
        );
        // Program change and channel pressure have one data byte.
        assert_eq!(
            d.packet(&[0x80, 0x80, 0xc1, 5, 0x80, 0xd1, 9]),
            vec![vec![0xc1, 5], vec![0xd1, 9]]
        );
        // Running status carries across packets; channel pressure has one data byte each.
        assert_eq!(
            d.packet(&[0x80, 0x80, 7, 8]),
            vec![vec![0xd1, 7], vec![0xd1, 8]]
        );
    }

    #[test]
    fn a_sysex_runs_across_packets_with_real_time_inside() {
        let mut d = Decoder::default();
        assert_eq!(
            d.packet(&[0x80, 0x80, 0xf0, 0x7d, 0x01]),
            Vec::<Vec<u8>>::new()
        );
        // A clock inside the SysEx, then the end.
        assert_eq!(
            d.packet(&[0x80, 0x02, 0x81, 0xf8, 0x03, 0x82, 0xf7]),
            vec![vec![0xf8], vec![0xf0, 0x7d, 0x01, 0x02, 0x03, 0xf7]]
        );
    }

    #[test]
    fn malformed_packets_stop_at_the_fault() {
        let mut d = Decoder::default();
        assert!(d.packet(&[0x40, 0x80, 0x90, 1, 2]).is_empty());
        // Data with no running status.
        assert!(d.packet(&[0x80, 0x80, 5, 6]).is_empty());
        // A cut message.
        assert_eq!(d.packet(&[0x80, 0x80, 0xb0, 7]), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn encoded_packets_decode_to_the_same_messages() {
        let mut d = Decoder::default();
        let packets = encode(&[0xb0, 7, 100], 0x1234, 20);
        assert_eq!(packets, vec![vec![0x80 | 0x24, 0x80 | 0x34, 0xb0, 7, 100]]);
        assert_eq!(d.packet(&packets[0]), vec![vec![0xb0, 7, 100]]);

        let mut sysex = vec![0xf0];
        sysex.extend(0..40u8);
        sysex.push(0xf7);
        let packets = encode(&sysex, 5, 20);
        assert!(packets.len() > 1 && packets.iter().all(|p| p.len() <= 20));
        let decoded: Vec<_> = packets.iter().flat_map(|p| d.packet(p)).collect();
        assert_eq!(decoded, vec![sysex]);
    }
}
