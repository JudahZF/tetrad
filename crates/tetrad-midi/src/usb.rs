//! USB MIDI events. The SHARC owns the USB device; ZenUI sends events with
//! DSP message 0x18 (`enUSBMIDIOut`) and receives them as DSP response
//! records. This module maps them to and from [`MidiMessage`] with the
//! USB MIDI 1.0 code index number (CIN) table.
//!
//! (C) USB probe 2026-10-22: the SOC2 firmware forwards every CIN 0x2..=0xF
//! byte for byte in both directions, so every [`MidiMessage`] kind, SysEx
//! included, has a USB form.

use crate::message::{MidiMessage, Realtime};
use crate::parser::{DEFAULT_SYSEX_LIMIT, assemble};

/// The bytes the stock 0x18 writers store: `code` (a USB-MIDI code index
/// number) at +16, zero at +17, status at +18 and two data bytes at +19.
/// Inbound records (DSP response 0x07) carry the same fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UsbMidiEvent {
    pub code: u8,
    pub status: u8,
    pub data: [u8; 2],
}

impl UsbMidiEvent {
    /// The USB packets for one message, in send order. (C) USB probe 2026-10-22: the host
    /// receives exactly the MIDI bytes for each CIN used here.
    ///
    /// - Channel voice: the CIN is the status high nibble (0x8..=0xE). Stock used 0x0b for CC
    ///   and 0x0c for PC (`0x008762f4`), which this keeps.
    /// - System common: 0x2 (F1, F3), 0x3 (F2), 0x5 (F6).
    /// - Real-time: 0xF, as stock sends timing clock.
    /// - SysEx: F0, the data and F7 in groups of three; 0x4 for each full group before the
    ///   end, then 0x5, 0x6 or 0x7 for an end group of one, two or three bytes.
    pub fn packets(message: &MidiMessage) -> Vec<Self> {
        let bytes = message.to_bytes();
        let packet = |code: u8, chunk: &[u8]| Self {
            code,
            status: chunk[0],
            data: [
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ],
        };
        if let MidiMessage::SysEx(_) = message {
            let last = bytes.len().div_ceil(3) - 1;
            return bytes
                .chunks(3)
                .enumerate()
                .map(|(index, chunk)| {
                    let code = if index < last {
                        0x04
                    } else {
                        0x04 + chunk.len() as u8
                    };
                    packet(code, chunk)
                })
                .collect();
        }
        let code = match bytes[0] {
            status @ 0x80..=0xef => status >> 4,
            0xf1 | 0xf3 => 0x02,
            0xf2 => 0x03,
            0xf6 => 0x05,
            _ => 0x0f,
        };
        vec![packet(code, &bytes)]
    }

    /// Decode a packet that holds a whole message: every kind except SysEx (CIN 0x4, 0x6,
    /// 0x7 and 0x5 with F7; see [`UsbMidiReceiver`]). The status must fit the CIN, so a
    /// malformed packet gives `None`. Data bytes keep their low seven bits.
    ///
    /// (C) USB probe 2026-10-22: host pitch bend `e1 10 50` arrives as `(0e e1 10 50)`;
    /// real-time FA/FC as CIN 0x0f. Stock also takes CIN 0x05 for one real-time byte.
    pub fn to_message(&self) -> Option<MidiMessage> {
        let status = self.status;
        let fits = match self.code {
            0x02 => matches!(status, 0xf1 | 0xf3),
            0x03 => status == 0xf2,
            0x05 | 0x0f if status == 0xf6 => return Some(MidiMessage::TuneRequest),
            0x05 | 0x0f => return Realtime::from_byte(status).map(MidiMessage::Realtime),
            code @ 0x08..=0x0e => status >> 4 == code,
            _ => false,
        };
        fits.then(|| assemble(status, self.data.map(|byte| byte & 0x7f)))
    }
}

/// Inbound USB MIDI with SysEx reassembly. A packet that holds a whole message decodes on
/// its own ([`UsbMidiEvent::to_message`]); CIN 0x4..=0x7 sequences collect into
/// [`MidiMessage::SysEx`].
///
/// (C) USB probe 2026-10-22: host SysEx `F0 7D 11 22 33 F7` arrives as `(04 f0 7d 11)
/// (07 22 33 f7)`, and `F0 7D 44 F7` as `(04 f0 7d 44)(05 f7 00 00)`.
///
/// (O) As the DIN [`crate::MidiParser`] does: real-time packets may interleave with a SysEx;
/// any other message, a new F0 or a stray status byte drops the SysEx being collected; a
/// SysEx longer than the limit is dropped up to its F7, so a SysEx that never ends holds at
/// most `limit` bytes.
#[derive(Debug, Clone)]
pub struct UsbMidiReceiver {
    limit: usize,
    /// The SysEx data so far, without F0.
    sysex: Option<Vec<u8>>,
}

impl Default for UsbMidiReceiver {
    fn default() -> Self {
        Self::new(DEFAULT_SYSEX_LIMIT)
    }
}

impl UsbMidiReceiver {
    pub fn new(limit: usize) -> Self {
        Self { limit, sysex: None }
    }

    /// One packet, in arrival order. Returns the message it completes, if any.
    pub fn receive(&mut self, event: UsbMidiEvent) -> Option<MidiMessage> {
        let len = match (event.code, event.status) {
            (0x04 | 0x07, _) => 3,
            (0x05, 0xf7) => 1,
            (0x06, _) => 2,
            _ => {
                let message = event.to_message()?;
                if !matches!(message, MidiMessage::Realtime(_)) {
                    self.sysex = None;
                }
                return Some(message);
            }
        };
        let [first, second] = event.data;
        for byte in [event.status, first, second].into_iter().take(len) {
            match byte {
                0xf0 => self.sysex = Some(Vec::new()),
                0xf7 => return self.sysex.take().map(MidiMessage::SysEx),
                0x00..=0x7f => match &mut self.sysex {
                    Some(data) if data.len() == self.limit => self.sysex = None,
                    Some(data) => data.push(byte),
                    None => {}
                },
                _ => self.sysex = None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Channel, U7};

    fn packet(code: u8, status: u8, data: [u8; 2]) -> UsbMidiEvent {
        UsbMidiEvent { code, status, data }
    }

    /// Every single-packet kind gets its CIN and decodes back. CC, PC and timing clock keep
    /// the stock codes.
    #[test]
    fn single_packet_kinds_round_trip() {
        let channel = Channel::from_number(2).unwrap();
        let value = U7::new(100).unwrap();
        let cases = [
            (
                MidiMessage::NoteOff {
                    channel,
                    key: value,
                    velocity: value,
                },
                packet(0x08, 0x81, [100, 100]),
            ),
            (
                MidiMessage::NoteOn {
                    channel,
                    key: value,
                    velocity: value,
                },
                packet(0x09, 0x91, [100, 100]),
            ),
            (
                MidiMessage::PolyPressure {
                    channel,
                    key: value,
                    pressure: value,
                },
                packet(0x0a, 0xa1, [100, 100]),
            ),
            (
                MidiMessage::ControlChange {
                    channel,
                    controller: value,
                    value,
                },
                packet(0x0b, 0xb1, [100, 100]),
            ),
            (
                MidiMessage::ProgramChange {
                    channel,
                    program: value,
                },
                packet(0x0c, 0xc1, [100, 0]),
            ),
            (
                MidiMessage::ChannelPressure {
                    channel,
                    pressure: value,
                },
                packet(0x0d, 0xd1, [100, 0]),
            ),
            (
                MidiMessage::PitchBend {
                    channel,
                    value: 0x2810,
                },
                packet(0x0e, 0xe1, [0x10, 0x50]),
            ),
            (
                MidiMessage::TimeCodeQuarterFrame(value),
                packet(0x02, 0xf1, [100, 0]),
            ),
            (MidiMessage::SongSelect(value), packet(0x02, 0xf3, [100, 0])),
            (
                MidiMessage::SongPosition(0x3fff),
                packet(0x03, 0xf2, [0x7f, 0x7f]),
            ),
            (MidiMessage::TuneRequest, packet(0x05, 0xf6, [0, 0])),
            (
                MidiMessage::Realtime(Realtime::TimingClock),
                packet(0x0f, 0xf8, [0, 0]),
            ),
            (
                MidiMessage::Realtime(Realtime::Stop),
                packet(0x0f, 0xfc, [0, 0]),
            ),
        ];
        for (message, event) in cases {
            assert_eq!(UsbMidiEvent::packets(&message), [event], "{message:?}");
            assert_eq!(event.to_message(), Some(message));
        }
        // A status that does not fit its CIN is malformed, not a message.
        assert_eq!(packet(0x09, 0xb1, [1, 2]).to_message(), None);
    }

    /// SysEx of 0..=4 data bytes uses every end CIN, and reassembles to the same message.
    #[test]
    fn sysex_packets_for_each_end_length() {
        let expected: [&[UsbMidiEvent]; 5] = [
            &[packet(0x06, 0xf0, [0xf7, 0])],
            &[packet(0x07, 0xf0, [0x7d, 0xf7])],
            &[packet(0x04, 0xf0, [0x7d, 0x11]), packet(0x05, 0xf7, [0, 0])],
            &[
                packet(0x04, 0xf0, [0x7d, 0x11]),
                packet(0x06, 0x22, [0xf7, 0]),
            ],
            &[
                packet(0x04, 0xf0, [0x7d, 0x11]),
                packet(0x07, 0x22, [0x33, 0xf7]),
            ],
        ];
        let mut receiver = UsbMidiReceiver::default();
        for (len, packets) in expected.into_iter().enumerate() {
            let message = MidiMessage::SysEx([0x7d, 0x11, 0x22, 0x33][..len].to_vec());
            assert_eq!(UsbMidiEvent::packets(&message), packets, "{len} bytes");
            let received: Vec<_> = packets
                .iter()
                .filter_map(|&event| receiver.receive(event))
                .collect();
            assert_eq!(received, [message]);
        }
    }

    /// A SysEx split over many packets survives an interleaved clock; another message drops
    /// it, and its stray end packet gives nothing.
    #[test]
    fn sysex_reassembly_and_interruption() {
        let mut receiver = UsbMidiReceiver::default();
        let data: Vec<u8> = (0..20).collect();
        let mut packets = UsbMidiEvent::packets(&MidiMessage::SysEx(data.clone()));
        let clock = packet(0x0f, 0xf8, [0, 0]);
        packets.insert(3, clock);
        let received: Vec<_> = packets
            .into_iter()
            .filter_map(|event| receiver.receive(event))
            .collect();
        assert_eq!(
            received,
            [
                MidiMessage::Realtime(Realtime::TimingClock),
                MidiMessage::SysEx(data)
            ]
        );

        let cc = packet(0x0b, 0xb0, [7, 1]);
        assert_eq!(receiver.receive(packet(0x04, 0xf0, [1, 2])), None);
        assert!(receiver.receive(cc).is_some());
        assert_eq!(receiver.receive(packet(0x06, 3, [0xf7, 0])), None);
    }

    /// A SysEx over the limit is dropped up to its F7, and the next one is whole again.
    #[test]
    fn sysex_limit() {
        let mut receiver = UsbMidiReceiver::new(4);
        let long = UsbMidiEvent::packets(&MidiMessage::SysEx(vec![1; 5]));
        assert!(
            long.into_iter()
                .all(|event| receiver.receive(event).is_none())
        );
        // A SysEx that never ends keeps at most the limit.
        receiver.receive(packet(0x04, 0xf0, [1, 1]));
        for _ in 0..100 {
            assert_eq!(receiver.receive(packet(0x04, 1, [1, 1])), None);
            assert!(receiver.sysex.as_ref().is_none_or(|data| data.len() <= 4));
        }
        let fits = MidiMessage::SysEx(vec![1; 4]);
        let received: Vec<_> = UsbMidiEvent::packets(&fits)
            .into_iter()
            .filter_map(|event| receiver.receive(event))
            .collect();
        assert_eq!(received, [fits]);
    }
}
