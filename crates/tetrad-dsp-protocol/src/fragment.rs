//! Generic 16-byte headers. Special stock paths are not deployment support.
use crate::{CodecLimits, CoreId, DspCore, Error, MessageType, read_u32};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Marker {
    Single = 0,
    First = 1,
    Last = 2,
    Middle = 3,
}

impl TryFrom<u8> for Marker {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Single),
            1 => Ok(Self::First),
            2 => Ok(Self::Last),
            3 => Ok(Self::Middle),
            _ => Err(Error::Invalid("fragment marker")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageMetadata {
    /// Raw byte retained for unknown/future types; use `MessageType::try_from`
    /// to classify known types without changing opaque wire round trips.
    pub message_type: u8,
    pub core: CoreId,
    /// Generic headers use a byte; the thirty-slot bound applies to deployment.
    pub slot: u8,
    pub argument: u32,
}

/// `Debug` shows only the payload length.
#[derive(Clone, Copy)]
pub struct Fragment<'a> {
    metadata: MessageMetadata,
    marker: Marker,
    total_bytes: u32,
    payload: &'a [u8],
}

impl<'a> Fragment<'a> {
    pub fn decode(bytes: &'a [u8], limits: CodecLimits) -> Result<Self, Error> {
        if bytes.len() < 16 {
            return Err(Error::Invalid("fragment header"));
        }
        let metadata = MessageMetadata {
            message_type: bytes[0],
            core: CoreId::try_from(bytes[1])?,
            slot: bytes[2],
            argument: read_u32(bytes, 4),
        };
        let marker = Marker::try_from(bytes[3])?;
        let total_bytes = read_u32(bytes, 12);
        let payload = &bytes[16..];
        if usize::try_from(read_u32(bytes, 8)).ok() != Some(bytes.len()) {
            return Err(Error::Invalid("fragment length"));
        }
        let total = usize::try_from(total_bytes).map_err(|_| Error::Overflow("message length"))?;
        if total > limits.max_message_bytes {
            return Err(Error::Limit("fragmented message"));
        }
        if payload.len() > metadata.core.fragment_payload_max() {
            return Err(Error::Limit("fragment payload"));
        }
        if (marker == Marker::Single && total != payload.len())
            || (marker != Marker::Single && (payload.is_empty() || total <= payload.len()))
        {
            return Err(Error::Invalid("fragment total/marker"));
        }
        if matches!(marker, Marker::First | Marker::Middle)
            && payload.len() != metadata.core.fragment_payload_max()
        {
            return Err(Error::Invalid("non-final fragment size"));
        }
        Ok(Self {
            metadata,
            marker,
            total_bytes,
            payload,
        })
    }
    pub fn metadata(self) -> MessageMetadata {
        self.metadata
    }
    pub fn marker(self) -> Marker {
        self.marker
    }
    pub fn total_bytes(self) -> u32 {
        self.total_bytes
    }
    pub fn payload(self) -> &'a [u8] {
        self.payload
    }
    pub fn to_bytes(self) -> Vec<u8> {
        let size = self.payload.len() + 16;
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&[
            self.metadata.message_type,
            self.metadata.core.wire(),
            self.metadata.slot,
            self.marker as u8,
        ]);
        bytes.extend_from_slice(&self.metadata.argument.to_le_bytes());
        bytes.extend_from_slice(&(size as u32).to_le_bytes());
        bytes.extend_from_slice(&self.total_bytes.to_le_bytes());
        bytes.extend_from_slice(self.payload);
        bytes
    }
}

/// Borrowed fragments, including one empty Single fragment for an empty message.
#[derive(Debug)]
pub struct Fragmenter<'a> {
    metadata: MessageMetadata,
    payload: &'a [u8],
    total: u32,
    offset: usize,
    finished: bool,
}

impl<'a> Fragmenter<'a> {
    pub fn new(
        metadata: MessageMetadata,
        payload: &'a [u8],
        limits: CodecLimits,
    ) -> Result<Self, Error> {
        // VA 0x51965c copies LoadModel and ReceiveData straight into core 0/1
        // memory; cores 2..=4 use generic framing (`+2` slot, `+4` address or
        // receive-buffer index). The stock ReceiveData fallback to generic
        // framing on cores 0/1 is refused as owned policy.
        if matches!(
            MessageType::try_from(metadata.message_type),
            Ok(MessageType::LoadModel | MessageType::ReceiveData)
        ) && matches!(metadata.core, CoreId::Dsp(DspCore::Core0 | DspCore::Core1))
        {
            return Err(Error::DirectMemoryRequired);
        }
        if payload.len() > limits.max_message_bytes {
            return Err(Error::Limit("message"));
        }
        let total = u32::try_from(payload.len()).map_err(|_| Error::Overflow("message length"))?;
        Ok(Self {
            metadata,
            payload,
            total,
            offset: 0,
            finished: false,
        })
    }
}

impl<'a> Iterator for Fragmenter<'a> {
    type Item = Fragment<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let maximum = self.metadata.core.fragment_payload_max();
        let remaining = self.payload.len() - self.offset;
        let size = remaining.min(maximum);
        let marker = if self.payload.len() <= maximum {
            Marker::Single
        } else if self.offset == 0 {
            Marker::First
        } else if remaining <= maximum {
            Marker::Last
        } else {
            Marker::Middle
        };
        let end = self.offset + size;
        let result = Fragment {
            metadata: self.metadata,
            marker,
            total_bytes: self.total,
            payload: &self.payload[self.offset..end],
        };
        self.offset = end;
        self.finished = self.offset == self.payload.len();
        Some(result)
    }
}

/// `Debug` shows only the payload length: a message can hold key bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct ReassembledMessage {
    pub metadata: MessageMetadata,
    pub payload: Vec<u8>,
}

struct Active {
    metadata: MessageMetadata,
    total: u32,
    payload: Vec<u8>,
}

impl std::fmt::Debug for Fragment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fragment")
            .field("metadata", &self.metadata)
            .field("marker", &self.marker)
            .field("total_bytes", &self.total_bytes)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

impl std::fmt::Debug for ReassembledMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReassembledMessage")
            .field("metadata", &self.metadata)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

impl std::fmt::Debug for Active {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Active")
            .field("metadata", &self.metadata)
            .field("total", &self.total)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

#[derive(Debug)]
pub struct Reassembler {
    limits: CodecLimits,
    active: Option<Active>,
}

impl Reassembler {
    pub fn new(limits: CodecLimits) -> Self {
        Self {
            limits,
            active: None,
        }
    }
    /// Any invalid fragment aborts the current logical message.
    pub fn push(&mut self, fragment: Fragment<'_>) -> Result<Option<ReassembledMessage>, Error> {
        let result = self.push_validated(fragment);
        if result.is_err() {
            self.active = None;
        }
        result
    }
    fn push_validated(
        &mut self,
        fragment: Fragment<'_>,
    ) -> Result<Option<ReassembledMessage>, Error> {
        let total =
            usize::try_from(fragment.total_bytes).map_err(|_| Error::Overflow("message length"))?;
        if total > self.limits.max_message_bytes {
            return Err(Error::Limit("reassembled message"));
        }
        match fragment.marker {
            Marker::Single | Marker::First => {
                if self.active.is_some() {
                    return Err(Error::Invalid("fragment sequence"));
                }
                if fragment.marker == Marker::Single {
                    return Ok(Some(ReassembledMessage {
                        metadata: fragment.metadata,
                        payload: fragment.payload.to_vec(),
                    }));
                }
                self.active = Some(Active {
                    metadata: fragment.metadata,
                    total: fragment.total_bytes,
                    payload: fragment.payload.to_vec(),
                });
                Ok(None)
            }
            Marker::Middle | Marker::Last => {
                let active = self
                    .active
                    .as_mut()
                    .ok_or(Error::Invalid("fragment sequence"))?;
                if active.metadata != fragment.metadata || active.total != fragment.total_bytes {
                    return Err(Error::Invalid("fragment identity"));
                }
                let new_size = active
                    .payload
                    .len()
                    .checked_add(fragment.payload.len())
                    .ok_or(Error::Overflow("reassembly"))?;
                if new_size > total
                    || (fragment.marker == Marker::Middle && new_size >= total)
                    || (fragment.marker == Marker::Last && new_size != total)
                {
                    return Err(Error::Invalid("reassembly length"));
                }
                active.payload.extend_from_slice(fragment.payload);
                if fragment.marker == Marker::Last {
                    let done = self.active.take().expect("active message");
                    Ok(Some(ReassembledMessage {
                        metadata: done.metadata,
                        payload: done.payload,
                    }))
                } else {
                    Ok(None)
                }
            }
        }
    }
    pub fn finish(&self) -> Result<(), Error> {
        if self.active.is_some() {
            Err(Error::Invalid("truncated fragment sequence"))
        } else {
            Ok(())
        }
    }
}
