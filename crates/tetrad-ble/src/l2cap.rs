//! L2CAP on an LE link (Core Vol 3 Part A): the basic frame, its reassembly from ACL
//! fragments, and the LE signalling channel's connection parameter update.

/// Fixed channels of an LE link.
pub const ATT: u16 = 0x0004;
pub const SIGNAL: u16 = 0x0005;
pub const SMP: u16 = 0x0006;

/// The largest L2CAP payload Tetrad takes: an ATT PDU at the largest MTU it offers, with room.
pub const MAX_PAYLOAD: usize = 517;

/// Reassembles one link's L2CAP frames from ACL fragments.
#[derive(Debug, Default)]
pub struct Reassembler {
    pending: Vec<u8>,
}

/// A frame that does not fit the rules: it is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("an L2CAP continuation with no start")]
    Orphan,
    #[error("an L2CAP frame of {0} bytes is too long")]
    TooLong(usize),
}

impl Reassembler {
    /// One ACL fragment (`first`: the packet boundary flag says start). Returns the channel and
    /// payload of a complete frame.
    pub fn push(&mut self, first: bool, data: &[u8]) -> Result<Option<(u16, Vec<u8>)>, FrameError> {
        if first {
            self.pending.clear();
        } else if self.pending.is_empty() {
            return Err(FrameError::Orphan);
        }
        self.pending.extend_from_slice(data);
        if self.pending.len() < 4 {
            return Ok(None);
        }
        let length = usize::from(u16::from_le_bytes([self.pending[0], self.pending[1]]));
        if length > MAX_PAYLOAD {
            self.pending.clear();
            return Err(FrameError::TooLong(length));
        }
        if self.pending.len() < 4 + length {
            return Ok(None);
        }
        let channel = u16::from_le_bytes([self.pending[2], self.pending[3]]);
        let payload = self.pending[4..4 + length].to_vec();
        self.pending.clear();
        Ok(Some((channel, payload)))
    }
}

/// A basic frame: length, channel, payload.
pub fn frame(channel: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(&channel.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Signalling codes.
const COMMAND_REJECT: u8 = 0x01;
const PARAMETER_UPDATE_REQUEST: u8 = 0x12;
const PARAMETER_UPDATE_RESPONSE: u8 = 0x13;

/// Connection parameters, in the units of HCI (1.25 ms intervals, 10 ms timeout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameters {
    pub min_interval: u16,
    pub max_interval: u16,
    pub latency: u16,
    pub timeout: u16,
}

/// A request of the peripheral for new connection parameters.
pub fn parameter_update_request(identifier: u8, parameters: Parameters) -> Vec<u8> {
    let mut out = vec![PARAMETER_UPDATE_REQUEST, identifier, 8, 0];
    for value in [
        parameters.min_interval,
        parameters.max_interval,
        parameters.latency,
        parameters.timeout,
    ] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// What a signalling packet asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// A peripheral asks the central (Tetrad) for new parameters.
    UpdateRequest {
        identifier: u8,
        parameters: Parameters,
    },
    /// The central's answer to Tetrad's request.
    UpdateResponse { accepted: bool },
    /// Anything else: answer with a Command Reject.
    Unknown { identifier: u8 },
    /// A reject or an empty packet: no answer.
    Ignore,
}

pub fn parse_signal(packet: &[u8]) -> Signal {
    let (Some(&code), Some(&identifier)) = (packet.first(), packet.get(1)) else {
        return Signal::Ignore;
    };
    let word = |at: usize| {
        packet
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    match code {
        PARAMETER_UPDATE_REQUEST => match (word(4), word(6), word(8), word(10)) {
            (Some(min_interval), Some(max_interval), Some(latency), Some(timeout)) => {
                Signal::UpdateRequest {
                    identifier,
                    parameters: Parameters {
                        min_interval,
                        max_interval,
                        latency,
                        timeout,
                    },
                }
            }
            _ => Signal::Unknown { identifier },
        },
        PARAMETER_UPDATE_RESPONSE => Signal::UpdateResponse {
            accepted: word(4) == Some(0),
        },
        COMMAND_REJECT => Signal::Ignore,
        _ => Signal::Unknown { identifier },
    }
}

/// Command Reject, "command not understood".
pub fn reject(identifier: u8) -> Vec<u8> {
    vec![COMMAND_REJECT, identifier, 2, 0, 0, 0]
}

/// The central's answer to an update request.
pub fn update_response(identifier: u8, accepted: bool) -> Vec<u8> {
    vec![
        PARAMETER_UPDATE_RESPONSE,
        identifier,
        2,
        0,
        u8::from(!accepted),
        0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_reassemble_from_fragments() {
        let pdu = frame(ATT, &[1, 2, 3, 4, 5, 6]);
        let mut r = Reassembler::default();
        assert_eq!(r.push(true, &pdu[..3]), Ok(None));
        assert_eq!(r.push(false, &pdu[3..7]), Ok(None));
        assert_eq!(
            r.push(false, &pdu[7..]),
            Ok(Some((ATT, vec![1, 2, 3, 4, 5, 6])))
        );
        assert_eq!(r.push(false, &[1]), Err(FrameError::Orphan));
        assert_eq!(
            r.push(true, &[0xff, 0xff, 4, 0]),
            Err(FrameError::TooLong(0xffff))
        );
    }

    #[test]
    fn signalling_round_trips() {
        let parameters = Parameters {
            min_interval: 9,
            max_interval: 12,
            latency: 0,
            timeout: 300,
        };
        let request = parameter_update_request(7, parameters);
        assert_eq!(
            parse_signal(&request),
            Signal::UpdateRequest {
                identifier: 7,
                parameters
            }
        );
        assert_eq!(
            parse_signal(&update_response(7, true)),
            Signal::UpdateResponse { accepted: true }
        );
        assert_eq!(
            parse_signal(&[0x0a, 3, 0, 0]),
            Signal::Unknown { identifier: 3 }
        );
    }
}
