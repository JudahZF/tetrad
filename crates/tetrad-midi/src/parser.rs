//! Byte-stream receive parser: running status, SysEx and interleaved real-time.

use crate::message::{Channel, MidiMessage, Realtime, U7};

/// Default SysEx data limit. Stock ZenUI does not act on SysEx; the limit
/// only bounds memory for a hostile stream.
pub const DEFAULT_SYSEX_LIMIT: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseEvent {
    Message(MidiMessage),
    Error(ParseError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("data byte {0:#04x} without a status")]
    UnexpectedData(u8),
    #[error("undefined status byte {0:#04x}")]
    UndefinedStatus(u8),
    #[error("F7 without a SysEx start")]
    UnexpectedEndOfSysEx,
    #[error("status {status:#04x} interrupted after {received} data byte(s)")]
    Interrupted { status: u8, received: usize },
    #[error("SysEx ended by a status byte after {received} data byte(s)")]
    UnterminatedSysEx { received: usize },
    #[error("SysEx longer than {limit} bytes; dropped")]
    SysExTooLong { limit: usize },
}

#[derive(Debug, Clone)]
enum SysEx {
    Idle,
    Collecting(Vec<u8>),
    /// Overflow was reported; drop bytes up to F7 or the next status.
    Discarding,
}

#[derive(Debug, Clone)]
pub struct MidiParser {
    sysex_limit: usize,
    sysex: SysEx,
    /// Channel-voice status kept for running status.
    running: Option<u8>,
    /// Status and data received for the message being assembled.
    pending: Option<(u8, [u8; 2], usize)>,
}

impl Default for MidiParser {
    fn default() -> Self {
        Self::new(DEFAULT_SYSEX_LIMIT)
    }
}

/// Data bytes that follow a status byte, or `None` for undefined statuses.
const fn data_len(status: u8) -> Option<usize> {
    match status {
        0x80..=0xbf | 0xe0..=0xef => Some(2),
        0xc0..=0xdf => Some(1),
        0xf1 | 0xf3 => Some(1),
        0xf2 => Some(2),
        0xf6 => Some(0),
        _ => None,
    }
}

fn assemble(status: u8, data: [u8; 2]) -> MidiMessage {
    let channel = Channel::from_index(status & 0x0f).expect("low nibble is a channel");
    let [a, b] = data.map(|byte| U7::masked(byte.into()));
    let wide = u16::from(data[0]) | (u16::from(data[1]) << 7);
    match status {
        0x80..=0x8f => MidiMessage::NoteOff {
            channel,
            key: a,
            velocity: b,
        },
        0x90..=0x9f => MidiMessage::NoteOn {
            channel,
            key: a,
            velocity: b,
        },
        0xa0..=0xaf => MidiMessage::PolyPressure {
            channel,
            key: a,
            pressure: b,
        },
        0xb0..=0xbf => MidiMessage::ControlChange {
            channel,
            controller: a,
            value: b,
        },
        0xc0..=0xcf => MidiMessage::ProgramChange {
            channel,
            program: a,
        },
        0xd0..=0xdf => MidiMessage::ChannelPressure {
            channel,
            pressure: a,
        },
        0xe0..=0xef => MidiMessage::PitchBend {
            channel,
            value: wide,
        },
        0xf1 => MidiMessage::TimeCodeQuarterFrame(a),
        0xf2 => MidiMessage::SongPosition(wide),
        0xf3 => MidiMessage::SongSelect(a),
        _ => MidiMessage::TuneRequest,
    }
}

impl MidiParser {
    pub fn new(sysex_limit: usize) -> Self {
        Self {
            sysex_limit,
            sysex: SysEx::Idle,
            running: None,
            pending: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ParseEvent> {
        let mut events = Vec::new();
        for &byte in bytes {
            self.push(byte, &mut events);
        }
        events
    }

    pub fn push(&mut self, byte: u8, events: &mut Vec<ParseEvent>) {
        if byte >= 0xf8 {
            // Real-time bytes never disturb running status, pending data or SysEx.
            events.push(match Realtime::from_byte(byte) {
                Some(realtime) => ParseEvent::Message(MidiMessage::Realtime(realtime)),
                None => ParseEvent::Error(ParseError::UndefinedStatus(byte)),
            });
            return;
        }
        if byte < 0x80 {
            self.data(byte, events);
            return;
        }
        // Any other status byte ends SysEx and an incomplete message.
        match std::mem::replace(&mut self.sysex, SysEx::Idle) {
            SysEx::Collecting(data) if byte == 0xf7 => {
                events.push(ParseEvent::Message(MidiMessage::SysEx(data)));
                return;
            }
            SysEx::Collecting(data) => {
                events.push(ParseEvent::Error(ParseError::UnterminatedSysEx {
                    received: data.len(),
                }))
            }
            SysEx::Discarding if byte == 0xf7 => return,
            SysEx::Discarding | SysEx::Idle => {}
        }
        if let Some((status, _, received)) = self.pending.take() {
            events.push(ParseEvent::Error(ParseError::Interrupted {
                status,
                received,
            }));
        }
        self.running = None;
        match byte {
            0xf0 => self.sysex = SysEx::Collecting(Vec::new()),
            0xf7 => events.push(ParseEvent::Error(ParseError::UnexpectedEndOfSysEx)),
            _ => match data_len(byte) {
                None => events.push(ParseEvent::Error(ParseError::UndefinedStatus(byte))),
                Some(0) => events.push(ParseEvent::Message(assemble(byte, [0; 2]))),
                Some(_) => {
                    if byte < 0xf0 {
                        self.running = Some(byte);
                    }
                    self.pending = Some((byte, [0; 2], 0));
                }
            },
        }
    }

    fn data(&mut self, byte: u8, events: &mut Vec<ParseEvent>) {
        match &mut self.sysex {
            SysEx::Collecting(data) if data.len() == self.sysex_limit => {
                self.sysex = SysEx::Discarding;
                events.push(ParseEvent::Error(ParseError::SysExTooLong {
                    limit: self.sysex_limit,
                }));
                return;
            }
            SysEx::Collecting(data) => {
                data.push(byte);
                return;
            }
            SysEx::Discarding => return,
            SysEx::Idle => {}
        }
        let (status, mut data, mut received) = match (self.pending.take(), self.running) {
            (Some(pending), _) => pending,
            (None, Some(status)) => (status, [0; 2], 0),
            (None, None) => {
                events.push(ParseEvent::Error(ParseError::UnexpectedData(byte)));
                return;
            }
        };
        data[received] = byte;
        received += 1;
        if Some(received) == data_len(status) {
            events.push(ParseEvent::Message(assemble(status, data)));
        } else {
            self.pending = Some((status, data, received));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(events: Vec<ParseEvent>) -> Vec<MidiMessage> {
        events
            .into_iter()
            .map(|event| match event {
                ParseEvent::Message(message) => message,
                ParseEvent::Error(error) => panic!("unexpected {error}"),
            })
            .collect()
    }

    fn cc(channel: u8, controller: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange {
            channel: Channel::from_index(channel).unwrap(),
            controller: U7::new(controller).unwrap(),
            value: U7::new(value).unwrap(),
        }
    }

    /// Running status survives interleaved clock bytes and spans many messages;
    /// a system-common message clears it.
    #[test]
    fn running_status_with_interleaved_realtime() {
        let mut parser = MidiParser::default();
        let events = parser.feed(&[0xb2, 47, 0xf8, 1, 48, 127, 0xf6, 5]);
        assert_eq!(
            events[..4],
            [
                ParseEvent::Message(MidiMessage::Realtime(Realtime::TimingClock)),
                ParseEvent::Message(cc(2, 47, 1)),
                ParseEvent::Message(cc(2, 48, 127)),
                ParseEvent::Message(MidiMessage::TuneRequest),
            ]
        );
        assert_eq!(events[4], ParseEvent::Error(ParseError::UnexpectedData(5)));
    }

    /// SysEx keeps real-time bytes out of its data, ends on F7, is bounded,
    /// and an interrupting status byte is reported but still parsed.
    #[test]
    fn sysex_bounds_and_interruption() {
        let mut parser = MidiParser::new(2);
        let events = messages(parser.feed(&[0xf0, 1, 0xfa, 2, 0xf7]));
        assert_eq!(
            events,
            [
                MidiMessage::Realtime(Realtime::Start),
                MidiMessage::SysEx(vec![1, 2])
            ]
        );
        let events = parser.feed(&[0xf0, 1, 2, 3, 4, 0xf7, 0xf0, 9, 0xc0, 3]);
        assert_eq!(
            events,
            [
                ParseEvent::Error(ParseError::SysExTooLong { limit: 2 }),
                ParseEvent::Error(ParseError::UnterminatedSysEx { received: 1 }),
                ParseEvent::Message(MidiMessage::ProgramChange {
                    channel: Channel::from_index(0).unwrap(),
                    program: U7::new(3).unwrap(),
                }),
            ]
        );
    }

    /// Encoder output parses back to the same messages, byte-split anywhere.
    #[test]
    fn encoder_round_trip() {
        let channel = Channel::from_number(16).unwrap();
        let sent = [
            MidiMessage::PitchBend {
                channel,
                value: 0x2001,
            },
            MidiMessage::SongPosition(0x3fff),
            MidiMessage::SysEx(vec![0x00, 0x7f]),
            cc(15, 0, 127),
        ];
        let bytes: Vec<u8> = sent.iter().flat_map(MidiMessage::to_bytes).collect();
        let mut parser = MidiParser::default();
        let mut events = Vec::new();
        for chunk in bytes.chunks(1) {
            events.extend(parser.feed(chunk));
        }
        assert_eq!(messages(events), sent);
    }
}
