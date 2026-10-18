//! Bounded MIDI 1.0 stream parsing, independent of UART and stock C++ ABI.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    NoteOff,
    NoteOn,
    PolyPressure,
    ControlChange,
    ProgramChange,
    ChannelPressure,
    PitchBend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Channel(u8);

impl Channel {
    /// Zero-based MIDI channel, in 0..=15.
    pub fn index(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortStatus {
    Channel { kind: ChannelKind, channel: Channel },
    TimeCodeQuarterFrame,
    SongPosition,
    SongSelect,
    TuneRequest,
}

impl ShortStatus {
    pub fn byte(self) -> u8 {
        match self {
            Self::Channel { kind, channel } => {
                let base = match kind {
                    ChannelKind::NoteOff => 0x80,
                    ChannelKind::NoteOn => 0x90,
                    ChannelKind::PolyPressure => 0xa0,
                    ChannelKind::ControlChange => 0xb0,
                    ChannelKind::ProgramChange => 0xc0,
                    ChannelKind::ChannelPressure => 0xd0,
                    ChannelKind::PitchBend => 0xe0,
                };
                base | channel.0
            }
            Self::TimeCodeQuarterFrame => 0xf1,
            Self::SongPosition => 0xf2,
            Self::SongSelect => 0xf3,
            Self::TuneRequest => 0xf6,
        }
    }

    pub fn data_len(self) -> usize {
        match self {
            Self::Channel {
                kind: ChannelKind::ProgramChange | ChannelKind::ChannelPressure,
                ..
            }
            | Self::TimeCodeQuarterFrame
            | Self::SongSelect => 1,
            Self::TuneRequest => 0,
            _ => 2,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        let channel = Channel(byte & 0x0f);
        let kind = match byte >> 4 {
            8 => ChannelKind::NoteOff,
            9 => ChannelKind::NoteOn,
            10 => ChannelKind::PolyPressure,
            11 => ChannelKind::ControlChange,
            12 => ChannelKind::ProgramChange,
            13 => ChannelKind::ChannelPressure,
            14 => ChannelKind::PitchBend,
            _ => {
                return match byte {
                    0xf1 => Some(Self::TimeCodeQuarterFrame),
                    0xf2 => Some(Self::SongPosition),
                    0xf3 => Some(Self::SongSelect),
                    0xf6 => Some(Self::TuneRequest),
                    _ => None,
                };
            }
        };
        Some(Self::Channel { kind, channel })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortMessage {
    status: ShortStatus,
    data: [u8; 2],
}

impl ShortMessage {
    pub fn status(self) -> ShortStatus {
        self.status
    }
    pub fn data(&self) -> &[u8] {
        &self.data[..self.status.data_len()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiEvent {
    Message(ShortMessage),
    /// Data excludes F0/F7 delimiters and interleaved realtime bytes.
    SysEx(Vec<u8>),
    /// Preserve every F8..FF byte, including reserved F9 and FD.
    Realtime(u8),
    Malformed(MidiError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiError {
    UnexpectedData(u8),
    UndefinedStatus(u8),
    UnexpectedEndOfSysEx,
    InterruptedMessage {
        status: ShortStatus,
        received: usize,
        expected: usize,
    },
    IncompleteMessage {
        status: ShortStatus,
        received: usize,
        expected: usize,
    },
    UnterminatedSysEx {
        received: usize,
    },
    SysExLimit {
        limit: usize,
    },
}

#[derive(Debug)]
struct Pending {
    status: ShortStatus,
    data: [u8; 2],
    received: usize,
}

#[derive(Debug)]
enum SysExState {
    None,
    Collecting(Vec<u8>),
    /// Report overflow once, then discard data through F7 or a new status.
    Discarding,
}

#[derive(Debug)]
pub struct MidiParser {
    sysex_limit: usize,
    sysex: SysExState,
    running: Option<ShortStatus>,
    pending: Option<Pending>,
}

impl MidiParser {
    /// The caller chooses the maximum number of SysEx data bytes.
    /// Each `push` returns at most two events; there is no internal event queue.
    pub fn new(sysex_limit: usize) -> Self {
        Self {
            sysex_limit,
            sysex: SysExState::None,
            running: None,
            pending: None,
        }
    }

    pub fn push(&mut self, byte: u8) -> Vec<MidiEvent> {
        let mut events = Vec::new();
        if byte >= 0xf8 {
            events.push(MidiEvent::Realtime(byte));
            return events;
        }
        if byte == 0xf7 {
            match std::mem::replace(&mut self.sysex, SysExState::None) {
                SysExState::Collecting(data) => events.push(MidiEvent::SysEx(data)),
                SysExState::Discarding => (),
                SysExState::None => {
                    self.interrupt_pending(&mut events);
                    events.push(MidiEvent::Malformed(MidiError::UnexpectedEndOfSysEx));
                }
            }
            self.running = None;
            return events;
        }
        if byte >= 0x80 {
            if let SysExState::Collecting(data) =
                std::mem::replace(&mut self.sysex, SysExState::None)
            {
                events.push(MidiEvent::Malformed(MidiError::UnterminatedSysEx {
                    received: data.len(),
                }));
            }
            self.interrupt_pending(&mut events);
            if byte == 0xf0 {
                self.running = None;
                self.sysex = SysExState::Collecting(Vec::new());
            } else if let Some(status) = ShortStatus::from_byte(byte) {
                self.running = if matches!(status, ShortStatus::Channel { .. }) {
                    Some(status)
                } else {
                    None
                };
                if status.data_len() == 0 {
                    events.push(MidiEvent::Message(ShortMessage {
                        status,
                        data: [0; 2],
                    }));
                } else {
                    self.pending = Some(Pending {
                        status,
                        data: [0; 2],
                        received: 0,
                    });
                }
            } else {
                self.running = None;
                events.push(MidiEvent::Malformed(MidiError::UndefinedStatus(byte)));
            }
            return events;
        }
        match &mut self.sysex {
            SysExState::Collecting(data) => {
                if data.len() == self.sysex_limit {
                    self.sysex = SysExState::Discarding;
                    events.push(MidiEvent::Malformed(MidiError::SysExLimit {
                        limit: self.sysex_limit,
                    }));
                } else {
                    data.push(byte);
                }
                return events;
            }
            SysExState::Discarding => return events,
            SysExState::None => (),
        }
        if self.pending.is_none() {
            if let Some(status) = self.running {
                self.pending = Some(Pending {
                    status,
                    data: [0; 2],
                    received: 0,
                });
            } else {
                events.push(MidiEvent::Malformed(MidiError::UnexpectedData(byte)));
                return events;
            }
        }
        if let Some(pending) = &mut self.pending {
            pending.data[pending.received] = byte;
            pending.received += 1;
            if pending.received == pending.status.data_len() {
                events.push(MidiEvent::Message(ShortMessage {
                    status: pending.status,
                    data: pending.data,
                }));
                self.pending = None;
            }
        }
        events
    }

    fn interrupt_pending(&mut self, events: &mut Vec<MidiEvent>) {
        if let Some(pending) = self.pending.take() {
            events.push(MidiEvent::Malformed(MidiError::InterruptedMessage {
                status: pending.status,
                received: pending.received,
                expected: pending.status.data_len(),
            }));
        }
    }

    /// End an input stream, report partial data, and clear running status.
    /// Do not call this at ordinary input chunk boundaries.
    pub fn finish(&mut self) -> Vec<MidiEvent> {
        let mut events = Vec::new();
        if let Some(pending) = self.pending.take() {
            events.push(MidiEvent::Malformed(MidiError::IncompleteMessage {
                status: pending.status,
                received: pending.received,
                expected: pending.status.data_len(),
            }));
        }
        if let SysExState::Collecting(data) = std::mem::replace(&mut self.sysex, SysExState::None) {
            events.push(MidiEvent::Malformed(MidiError::UnterminatedSysEx {
                received: data.len(),
            }));
        }
        self.running = None;
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(parser: &mut MidiParser, bytes: &[u8]) -> Vec<MidiEvent> {
        bytes.iter().flat_map(|&b| parser.push(b)).collect()
    }

    fn message(event: &MidiEvent, status: u8, data: &[u8]) {
        let MidiEvent::Message(m) = event else {
            panic!("expected a complete MIDI message")
        };
        assert_eq!(m.status().byte(), status);
        assert_eq!(m.data(), data);
    }

    #[test]
    fn running_status_and_realtime_keep_message_order_across_chunks() {
        let mut p = MidiParser::new(16);
        assert!(feed(&mut p, &[0x92, 60]).is_empty());
        let e = feed(&mut p, &[0xf8, 100, 61, 0xfe, 0, 0xc2, 7, 8]);
        assert_eq!(e.len(), 6);
        assert_eq!(e[0], MidiEvent::Realtime(0xf8));
        message(&e[1], 0x92, &[60, 100]);
        assert_eq!(e[2], MidiEvent::Realtime(0xfe));
        message(&e[3], 0x92, &[61, 0]);
        message(&e[4], 0xc2, &[7]);
        message(&e[5], 0xc2, &[8]);
        assert!(p.finish().is_empty());
    }

    #[test]
    fn all_channel_and_system_common_lengths_and_reserved_bytes() {
        for high in 8..=14 {
            for channel in 0..16 {
                let status = (high << 4) | channel;
                let mut p = MidiParser::new(0);
                let len = if high == 12 || high == 13 { 1 } else { 2 };
                let mut bytes = vec![status, 1];
                if len == 2 {
                    bytes.push(2);
                }
                let e = feed(&mut p, &bytes);
                message(&e[0], status, &bytes[1..]);
                let ShortStatus::Channel { channel: c, .. } = p.running.unwrap() else {
                    unreachable!()
                };
                assert_eq!(c.index(), channel);
            }
        }
        let mut p = MidiParser::new(0);
        let e = feed(
            &mut p,
            &[
                0xc0, 1, 0xf1, 2, 0xf2, 3, 0xfa, 4, 0xf3, 5, 0xf6, 6, 0xf4, 0xf5, 0xf9, 0xfd,
            ],
        );
        message(&e[1], 0xf1, &[2]);
        assert_eq!(e[2], MidiEvent::Realtime(0xfa));
        message(&e[3], 0xf2, &[3, 4]);
        message(&e[4], 0xf3, &[5]);
        message(&e[5], 0xf6, &[]);
        assert_eq!(e[6], MidiEvent::Malformed(MidiError::UnexpectedData(6)));
        assert_eq!(e[7], MidiEvent::Malformed(MidiError::UndefinedStatus(0xf4)));
        assert_eq!(e[8], MidiEvent::Malformed(MidiError::UndefinedStatus(0xf5)));
        assert_eq!(
            &e[9..],
            &[MidiEvent::Realtime(0xf9), MidiEvent::Realtime(0xfd)]
        );
    }

    #[test]
    fn interrupted_incomplete_and_resynchronized_messages() {
        let mut p = MidiParser::new(4);
        let e = feed(&mut p, &[0x90, 1, 0xc0, 2]);
        assert!(matches!(
            e[0],
            MidiEvent::Malformed(MidiError::InterruptedMessage {
                received: 1,
                expected: 2,
                ..
            })
        ));
        message(&e[1], 0xc0, &[2]);
        assert!(feed(&mut p, &[0x80]).is_empty());
        assert!(matches!(
            p.finish()[0],
            MidiEvent::Malformed(MidiError::IncompleteMessage {
                received: 0,
                expected: 2,
                ..
            })
        ));
        assert_eq!(
            p.push(3),
            [MidiEvent::Malformed(MidiError::UnexpectedData(3))]
        );
        let e = feed(&mut p, &[0x90, 1, 0xf7]);
        assert_eq!(e.len(), 2);
        assert_eq!(e[1], MidiEvent::Malformed(MidiError::UnexpectedEndOfSysEx));
        assert_eq!(
            p.push(2),
            [MidiEvent::Malformed(MidiError::UnexpectedData(2))]
        );
    }

    #[test]
    fn sysex_cap_realtime_and_resynchronization() {
        let mut p = MidiParser::new(2);
        assert_eq!(
            feed(&mut p, &[0xf0, 1, 0xf8, 2, 0xf7]),
            [MidiEvent::Realtime(0xf8), MidiEvent::SysEx(vec![1, 2])]
        );
        assert_eq!(
            feed(&mut p, &[0xf0, 1, 2, 3, 4, 0xff, 0xf7]),
            [
                MidiEvent::Malformed(MidiError::SysExLimit { limit: 2 }),
                MidiEvent::Realtime(0xff)
            ]
        );
        let e = feed(&mut p, &[0xf0, 1, 0xc0, 9]);
        assert_eq!(
            e[0],
            MidiEvent::Malformed(MidiError::UnterminatedSysEx { received: 1 })
        );
        message(&e[1], 0xc0, &[9]);
        assert!(feed(&mut p, &[0xf0, 1]).is_empty());
        assert_eq!(
            p.finish(),
            [MidiEvent::Malformed(MidiError::UnterminatedSysEx {
                received: 1
            })]
        );
        let mut zero = MidiParser::new(0);
        assert_eq!(feed(&mut zero, &[0xf0, 0xf7]), [MidiEvent::SysEx(vec![])]);
        assert_eq!(
            feed(&mut zero, &[0xf0, 1, 0xf7]),
            [MidiEvent::Malformed(MidiError::SysExLimit { limit: 0 })]
        );
    }
}
