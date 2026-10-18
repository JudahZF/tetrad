use crate::{CodecLimits, DeploymentTarget, DspCore, Error, MessageType, ModuleSlot, read_u32};

pub const PARAMETER_CHANGE_TYPE: u8 = MessageType::ParameterChange as u8;
pub const BULK_PARAMETER_CHANGE_TYPE: u8 = MessageType::BulkParameterChange as u8;

/// Bytes 3 and 12..16 remain caller-owned opaque bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterChange([u8; 20]);

impl ParameterChange {
    pub fn from_template(
        mut template: [u8; 20],
        target: DeploymentTarget,
        index: u32,
        value: f32,
    ) -> Self {
        template[0] = PARAMETER_CHANGE_TYPE;
        template[1] = target.core as u8;
        template[2] = target.slot.index();
        template[4..8].copy_from_slice(&index.to_le_bytes());
        template[8..12].copy_from_slice(&20_u32.to_le_bytes());
        template[16..20].copy_from_slice(&value.to_bits().to_le_bytes());
        Self(template)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let raw: [u8; 20] = bytes
            .try_into()
            .map_err(|_| Error::Invalid("ParameterChange size"))?;
        if raw[0] != PARAMETER_CHANGE_TYPE || read_u32(&raw, 8) != 20 {
            return Err(Error::Invalid("ParameterChange type/length"));
        }
        DspCore::try_from(raw[1])?;
        ModuleSlot::try_from(u32::from(raw[2]))?;
        Ok(Self(raw))
    }
    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
    pub fn target(&self) -> DeploymentTarget {
        // Validated by both construction paths.
        DeploymentTarget {
            core: DspCore::try_from(self.0[1]).expect("validated core"),
            slot: ModuleSlot::try_from(u32::from(self.0[2])).expect("validated slot"),
        }
    }
    pub fn parameter_index(&self) -> u32 {
        read_u32(&self.0, 4)
    }
    pub fn value(&self) -> f32 {
        f32::from_bits(read_u32(&self.0, 16))
    }
}

/// Recovered builder values; their DSP semantics remain unresolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BulkMode {
    Value8 = 8,
    Value16 = 16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkRecord {
    pub slot: ModuleSlot,
    pub parameter_index: u32,
    /// Preserve all IEEE-754 bit patterns, including NaN payloads.
    pub value_bits: u32,
}

impl BulkRecord {
    pub fn from_bytes(bytes: [u8; 12]) -> Result<Self, Error> {
        Ok(Self {
            slot: ModuleSlot::try_from(read_u32(&bytes, 0))?,
            parameter_index: read_u32(&bytes, 4),
            value_bits: read_u32(&bytes, 8),
        })
    }
    pub fn to_bytes(self) -> [u8; 12] {
        let mut result = [0; 12];
        result[..4].copy_from_slice(&u32::from(self.slot.index()).to_le_bytes());
        result[4..8].copy_from_slice(&self.parameter_index.to_le_bytes());
        result[8..].copy_from_slice(&self.value_bits.to_le_bytes());
        result
    }
}

/// Borrowed, validated packet. Opaque header bytes and mode values survive decoding.
#[derive(Debug, Clone, Copy)]
pub struct BulkParameterChange<'a> {
    bytes: &'a [u8],
    core: DspCore,
}

impl<'a> BulkParameterChange<'a> {
    pub fn decode(bytes: &'a [u8], limits: CodecLimits) -> Result<Self, Error> {
        if bytes.len() > limits.max_message_bytes {
            return Err(Error::Limit("bulk message"));
        }
        if bytes.len() < 16 || bytes[0] != BULK_PARAMETER_CHANGE_TYPE {
            return Err(Error::Invalid("bulk header"));
        }
        let core = DspCore::try_from(bytes[1])?;
        let count =
            usize::try_from(read_u32(bytes, 12)).map_err(|_| Error::Overflow("bulk count"))?;
        if count > limits.max_records {
            return Err(Error::Limit("bulk records"));
        }
        let size = count
            .checked_mul(12)
            .and_then(|n| n.checked_add(16))
            .ok_or(Error::Overflow("bulk size"))?;
        if size != bytes.len() || usize::try_from(read_u32(bytes, 8)).ok() != Some(size) {
            return Err(Error::Invalid("bulk count/length"));
        }
        for record in bytes[16..].chunks_exact(12) {
            ModuleSlot::try_from(read_u32(record, 0))?;
        }
        Ok(Self { bytes, core })
    }
    pub fn core(self) -> DspCore {
        self.core
    }
    pub fn mode_word(self) -> u32 {
        read_u32(self.bytes, 4)
    }
    pub fn as_bytes(self) -> &'a [u8] {
        self.bytes
    }
    pub fn records(self) -> impl ExactSizeIterator<Item = BulkRecord> + 'a {
        self.bytes[16..].chunks_exact(12).map(|record| {
            BulkRecord::from_bytes(record.try_into().expect("12-byte chunk"))
                .expect("validated record")
        })
    }
}

pub fn encode_bulk(
    mut template: [u8; 16],
    core: DspCore,
    mode: BulkMode,
    records: &[BulkRecord],
    limits: CodecLimits,
) -> Result<Vec<u8>, Error> {
    if records.len() > limits.max_records {
        return Err(Error::Limit("bulk records"));
    }
    let size = records
        .len()
        .checked_mul(12)
        .and_then(|n| n.checked_add(16))
        .ok_or(Error::Overflow("bulk size"))?;
    if size > limits.max_message_bytes {
        return Err(Error::Limit("bulk message"));
    }
    let count = u32::try_from(records.len()).map_err(|_| Error::Overflow("bulk count"))?;
    let wire_size = u32::try_from(size).map_err(|_| Error::Overflow("bulk length"))?;
    template[0] = BULK_PARAMETER_CHANGE_TYPE;
    template[1] = core as u8;
    template[2] = 0;
    template[4..8].copy_from_slice(&(mode as u32).to_le_bytes());
    template[8..12].copy_from_slice(&wire_size.to_le_bytes());
    template[12..16].copy_from_slice(&count.to_le_bytes());
    let mut bytes = Vec::with_capacity(size);
    bytes.extend_from_slice(&template);
    for record in records {
        bytes.extend_from_slice(&record.to_bytes());
    }
    Ok(bytes)
}
