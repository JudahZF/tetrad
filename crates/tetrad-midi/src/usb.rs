//! USB MIDI events. The SHARC owns the USB device; ZenUI sends events with
//! DSP message 0x18 (`enUSBMIDIOut`) and receives them as DSP response
//! records. This module maps them to and from [`MidiMessage`].

use crate::message::{Channel, MidiMessage, Realtime, U7};

/// The bytes the stock 0x18 writers store: `code` (a USB-MIDI code index
/// number) at +16, zero at +17, status at +18 and two data bytes at +19.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UsbMidiEvent {
    pub code: u8,
    pub status: u8,
    pub data: [u8; 2],
}

impl UsbMidiEvent {
    /// Outbound mapping of `0x008762f4`: CC uses code 0x0b, PC 0x0c and
    /// timing clock 0x0f. Stock never sends other kinds over USB, so they have
    /// no USB form (`None`) and cannot become malformed packets.
    pub fn from_message(message: &MidiMessage) -> Option<Self> {
        if let MidiMessage::Realtime(Realtime::TimingClock) = message {
            return Some(Self {
                code: 0x0f,
                status: 0xf8,
                data: [0; 2],
            });
        }
        let code = match message {
            MidiMessage::ControlChange { .. } => 0x0b,
            MidiMessage::ProgramChange { .. } => 0x0c,
            _ => return None,
        };
        let bytes = message.to_bytes();
        Some(Self {
            code,
            status: bytes[0],
            data: [
                bytes.get(1).copied().unwrap_or(0),
                bytes.get(2).copied().unwrap_or(0),
            ],
        })
    }

    /// Inbound decode of a USB record (DSP response 0x07) as the drain
    /// `0x0089ece0` does it. Codes 0x08, 0x09, 0x0b and 0x0c give note
    /// off/on, CC and PC. Codes 0x05 and 0x0f carry one real-time byte. Stock
    /// turns every other code into a CC on channel 0, which no channel filter
    /// accepts; this returns `None` for them.
    pub fn to_message(&self) -> Option<MidiMessage> {
        let channel = Channel::from_index(self.status & 0x0f)?;
        let [a, b] = self.data.map(|byte| U7::masked(byte.into()));
        Some(match self.code {
            0x08 => MidiMessage::NoteOff {
                channel,
                key: a,
                velocity: b,
            },
            0x09 => MidiMessage::NoteOn {
                channel,
                key: a,
                velocity: b,
            },
            0x0b => MidiMessage::ControlChange {
                channel,
                controller: a,
                value: b,
            },
            0x0c => MidiMessage::ProgramChange {
                channel,
                program: a,
            },
            0x05 | 0x0f => MidiMessage::Realtime(match self.status {
                0xf8 => Realtime::TimingClock,
                0xfa => Realtime::Start,
                0xfb => Realtime::Continue,
                0xfc => Realtime::Stop,
                _ => return None,
            }),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outbound codes follow the stock writer, and USB records decode back for
    /// the handled kinds.
    #[test]
    fn stock_code_mapping() {
        let channel = Channel::from_number(10).unwrap();
        let value = U7::new(100).unwrap();
        let cc = MidiMessage::ControlChange {
            channel,
            controller: value,
            value,
        };
        let pc = MidiMessage::ProgramChange {
            channel,
            program: value,
        };
        let event = |m: &MidiMessage| UsbMidiEvent::from_message(m).unwrap();
        assert_eq!(
            event(&cc),
            UsbMidiEvent {
                code: 0x0b,
                status: 0xb9,
                data: [100, 100]
            }
        );
        assert_eq!(
            event(&pc),
            UsbMidiEvent {
                code: 0x0c,
                status: 0xc9,
                data: [100, 0]
            }
        );
        assert_eq!(event(&cc).to_message(), Some(cc));
        assert_eq!(event(&pc).to_message(), Some(pc));
        let host_bend = UsbMidiEvent {
            code: 0x0e,
            status: 0xe9,
            data: [0, 64],
        };
        assert_eq!(host_bend.to_message(), None);
        let clock = MidiMessage::Realtime(Realtime::TimingClock);
        assert_eq!(event(&clock).to_message(), Some(clock));
    }

    /// Kinds stock never sends over USB have no packet. A code-9 fallback here
    /// would put status 0x8n/0x9n/0xEn under CIN 9, which hosts read as malformed.
    #[test]
    fn kinds_without_usb_form() {
        let channel = Channel::from_number(10).unwrap();
        let value = U7::new(100).unwrap();
        let unsent = [
            MidiMessage::PitchBend { channel, value: 0 },
            MidiMessage::NoteOn {
                channel,
                key: value,
                velocity: value,
            },
            MidiMessage::NoteOff {
                channel,
                key: value,
                velocity: value,
            },
        ];
        for message in &unsent {
            assert_eq!(UsbMidiEvent::from_message(message), None, "{message:?}");
        }
    }
}
