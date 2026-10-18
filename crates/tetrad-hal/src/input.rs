use crate::RecordLengthError;

/// The two signed 32-bit timeval fields in the pinned ARM ABI.
/// Raw fields are retained even when the timestamp is not normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmTimestamp {
    pub seconds: i32,
    pub microseconds: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputRecord {
    pub timestamp: ArmTimestamp,
    pub event_type: u16,
    pub code: u16,
    pub value: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnobStompEvent {
    Knob {
        delta: i32,
    },
    /// Keep every raw transition in input order; do not coalesce stomp events.
    Stomp {
        value: i32,
    },
    Unknown {
        event_type: u16,
        code: u16,
        value: i32,
    },
}

impl InputRecord {
    pub const BYTE_LEN: usize = 16;

    pub fn decode(bytes: &[u8]) -> Result<Self, RecordLengthError> {
        let b: &[u8; 16] = bytes.try_into().map_err(|_| RecordLengthError {
            expected: Self::BYTE_LEN,
            actual: bytes.len(),
        })?;
        Ok(Self {
            timestamp: ArmTimestamp {
                seconds: i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                microseconds: i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            },
            event_type: u16::from_le_bytes([b[8], b[9]]),
            code: u16::from_le_bytes([b[10], b[11]]),
            value: i32::from_le_bytes([b[12], b[13], b[14], b[15]]),
        })
    }

    pub fn event(self) -> KnobStompEvent {
        match (self.event_type, self.code) {
            (2, 8) => KnobStompEvent::Knob {
                // VA 0x7300: signed full-word comparison, not a byte cast.
                delta: if self.value > 128 {
                    self.value - 256
                } else {
                    self.value
                },
            },
            (1, 273) => KnobStompEvent::Stomp { value: self.value },
            (event_type, code) => KnobStompEvent::Unknown {
                event_type,
                code,
                value: self.value,
            },
        }
    }
}

/// This custom jack record has no evdev timestamp, type or code fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JackRecord {
    pub words: [u32; 4],
}

impl JackRecord {
    pub const BYTE_LEN: usize = 16;

    pub fn decode(bytes: &[u8]) -> Result<Self, RecordLengthError> {
        let b: &[u8; 16] = bytes.try_into().map_err(|_| RecordLengthError {
            expected: Self::BYTE_LEN,
            actual: bytes.len(),
        })?;
        Ok(Self {
            words: std::array::from_fn(|i| {
                let j = i * 4;
                u32::from_le_bytes([b[j], b[j + 1], b[j + 2], b[j + 3]])
            }),
        })
    }

    pub fn status_words(self) -> [u16; 2] {
        // Stock STRH deliberately truncates both combined 32-bit words.
        [
            (self.words[0] | (self.words[2] << 8)) as u16,
            (self.words[1] | (self.words[3] << 8)) as u16,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(event_type: u16, code: u16, value: i32) -> InputRecord {
        let mut b = [0; 16];
        b[..4].copy_from_slice(&(-2_i32).to_le_bytes());
        b[4..8].copy_from_slice(&999_999_i32.to_le_bytes());
        b[8..10].copy_from_slice(&event_type.to_le_bytes());
        b[10..12].copy_from_slice(&code.to_le_bytes());
        b[12..].copy_from_slice(&value.to_le_bytes());
        InputRecord::decode(&b).unwrap()
    }

    #[test]
    fn arm_layout_unknowns_and_exact_lengths() {
        let r = record(0xffff, 0xabcd, i32::MIN);
        assert_eq!(
            r.timestamp,
            ArmTimestamp {
                seconds: -2,
                microseconds: 999_999
            }
        );
        assert_eq!(
            r.event(),
            KnobStompEvent::Unknown {
                event_type: 0xffff,
                code: 0xabcd,
                value: i32::MIN
            }
        );
        for length in [0, 15, 17, 24] {
            assert_eq!(
                InputRecord::decode(&vec![0; length]).unwrap_err().actual,
                length
            );
            assert!(JackRecord::decode(&vec![0; length]).is_err());
        }
    }

    #[test]
    fn knob_signed_word_boundary_and_ordered_stomps() {
        for (raw, delta) in [
            (127, 127),
            (128, 128),
            (129, -127),
            (255, -1),
            (256, 0),
            (257, 1),
            (-1, -1),
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX - 256),
        ] {
            assert_eq!(record(2, 8, raw).event(), KnobStompEvent::Knob { delta });
        }
        let events: Vec<_> = [1, 0, 1, 2, 0, 99]
            .into_iter()
            .map(|v| record(1, 273, v).event())
            .collect();
        assert_eq!(
            events,
            [1, 0, 1, 2, 0, 99].map(|value| KnobStompEvent::Stomp { value })
        );
        assert!(matches!(
            record(2, 273, 1).event(),
            KnobStompEvent::Unknown { .. }
        ));
    }

    #[test]
    fn jack_preserves_words_and_truncates_after_or() {
        let words = [0x1234_ab12_u32, 0xdead_00ff, 0xffff_1234, 0x1234_5678];
        let b: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let r = JackRecord::decode(&b).unwrap();
        assert_eq!(r.words, words);
        assert_eq!(r.status_words(), [0xbf12, 0x78ff]);
    }
}
