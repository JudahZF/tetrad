//! Stock model blob path: decrypt with the DSP-delivered blob key, parse,
//! select the per-core variant, place every slot with the stock bump
//! allocator, relocate, and split into section transfers (VAs 0x880880,
//! 0x5e0c4c, 0x87b228, 0x5e62a0, 0x5d7c44, 0x5d71f4, 0x5d683c, 0x5d8e7c).
//! No file access: the caller reads the files named here.

use std::fmt;

use qc_format::blob::{LinkRange, ModuleBlob, Section, validate_blob_name};
use qc_format::crypto::NdspCipher;
use qc_format::diff::DiffRecord;
use tetrad_dsp_protocol::DspCore;

use crate::responses::KeyMaterial;

/// Stock blob root. Core 1 holds whole blobs; `core2`..`core4` hold
/// `diffs.blob` (not encrypted) for transport cores 1..=3.
pub const BLOB_ROOT: &str = "/opt/neuraldsp/blobs";

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error(transparent)]
    Format(#[from] qc_format::FormatError),
    #[error("no diff record for this blob on core {0:?}")]
    MissingDiff(DspCore),
    #[error("diff record for {found:?} does not match blob {expected:?}")]
    DiffName { expected: String, found: String },
    /// Stock FitChecker returns 0: "requested models won't fit in core %d".
    #[error("models do not fit the {0:?} pool")]
    WontFit(Section),
    /// Stock sizes a one-unit section as zero and would overlap the next one.
    #[error("{0:?} section of one address unit is refused")]
    OneUnitSection(Section),
    /// Stock skips kinds above 12 and leaves the field unrelocated.
    #[error("code relocation kind {0} is not recovered")]
    UnknownRelocation(u32),
    #[error("{section:?} relocation at unit {offset} is outside the section")]
    RelocationBounds { section: Section, offset: u32 },
    /// A parsed but unvalidated blob: relocations or a transfer name a
    /// section without a link range or content.
    #[error("blob has no {0:?} section for its relocations")]
    MissingSection(Section),
    /// `ModuleBlob::validate` refuses these; stock has no data0 handler.
    #[error("data0 relocations are not supported")]
    Data0Relocation,
}

/// Blob decryption key. The DSP delivers it at boot as response `0x25`
/// (stored at controller `+0xf024`). Never embedded; `Debug` hides it.
pub struct BlobKey(NdspCipher);

impl BlobKey {
    pub fn new(secret: &[u8]) -> Self {
        Self(NdspCipher::derive(secret))
    }
}

impl From<&KeyMaterial> for BlobKey {
    fn from(key: &KeyMaterial) -> Self {
        Self::new(key.as_bytes())
    }
}

impl fmt::Debug for BlobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BlobKey(..)")
    }
}

/// `/opt/neuraldsp/blobs/core1/<name>_blob.bin` (strings at VA 0xa2ab4c).
pub fn blob_path(name: &str) -> Result<String, BlobError> {
    validate_blob_name(name)?;
    Ok(format!("{BLOB_ROOT}/core1/{name}_blob.bin"))
}

/// `diffs.blob` for transport cores 1..=3 (directories `core2`..`core4`).
pub fn diffs_path(core: DspCore) -> Option<String> {
    (core != DspCore::Core0).then(|| format!("{BLOB_ROOT}/core{}/diffs.blob", core as u8 + 1))
}

/// One encrypted `<name>_blob.bin` to a parsed blob (VA 0x5e0c4c). Stock
/// checks structure only; this also runs `ModuleBlob::validate`.
pub fn decrypt(key: &BlobKey, encrypted: &[u8]) -> Result<ModuleBlob, BlobError> {
    let blob = ModuleBlob::parse(&key.0.crypt(encrypted))?;
    blob.validate()?;
    Ok(blob)
}

/// Per-core variant (VA 0x87b228): transport core 0 runs the core 1 blob;
/// other cores apply that blob's record from their `diffs.blob`.
pub fn for_core(
    name: &str,
    blob: &ModuleBlob,
    core: DspCore,
    diff: Option<&DiffRecord>,
) -> Result<ModuleBlob, BlobError> {
    if core == DspCore::Core0 {
        return Ok(blob.clone());
    }
    let diff = diff.ok_or(BlobError::MissingDiff(core))?;
    if diff.name != name {
        return Err(BlobError::DiffName {
            expected: name.into(),
            found: diff.name.clone(),
        });
    }
    let patched = diff.apply(blob)?;
    patched.validate()?;
    Ok(patched)
}

/// Per-core runtime pools `[start, end)` in section units, the same on every
/// core (VA 0x882148, static init 0xbb56c). Code shares L1 block 3 with ISA.
pub const POOLS: [(Section, u32, u32); 4] = [
    (Section::Dm, 0x2e_1800, 0x2f_0000),
    (Section::Data0, 0x30_2000, 0x30_4000),
    (Section::Isa, 0x0e_0000, 0x0e_5555),
    (Section::Code, 0x1c_0000, 0x1d_0000),
];

/// One placed section: runtime `[start, end)` and the inclusive link range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub section: Section,
    pub start: u32,
    pub end: u32,
    pub link: LinkRange,
}

/// The regions of one placed blob, in stock order (DM, data0, ISA, code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub regions: Vec<Region>,
}

impl Placement {
    /// VA 0x5d8698: the first region whose link range holds `address`
    /// moves it; addresses outside every region (kernel calls) stay.
    pub fn remap(&self, address: u32) -> u32 {
        self.regions
            .iter()
            .find(|region| region.link.contains(address))
            .map_or(address, |region| {
                address
                    .wrapping_sub(region.link.start)
                    .wrapping_add(region.start)
            })
    }
}

fn units(blob: &ModuleBlob, section: Section) -> Result<u32, BlobError> {
    match blob.range(section) {
        None => Ok(0),
        Some(range) if range.start == range.end => Err(BlobError::OneUnitSection(section)),
        Some(range) => Ok(range.end - range.start + 1),
    }
}

/// Stock FitChecker (VA 0x5e62a0) for one core's slots in order. `None` is
/// an empty slot. Pass 1 places DM then data0 for every slot, each 16-byte
/// aligned; pass 2 places ISA words, moving the code cursor 3 parcels per
/// word; pass 3 places code. Absent sections get no region (owned: stock
/// pushes an empty one with link range `{0, 0}`).
pub fn place(slots: &[Option<&ModuleBlob>]) -> Result<Vec<Option<Placement>>, BlobError> {
    let mut cursors = POOLS.map(|(_, start, _)| start);
    let mut placements: Vec<Option<Placement>> = slots
        .iter()
        .map(|slot| {
            slot.map(|_| Placement {
                regions: Vec::new(),
            })
        })
        .collect();
    let push = |pool: usize,
                blob: &ModuleBlob,
                placement: &mut Placement,
                cursors: &mut [u32; 4]|
     -> Result<u32, BlobError> {
        let (section, _, limit) = POOLS[pool];
        let size = units(blob, section)?;
        let start = cursors[pool];
        let end = start
            .checked_add(size)
            .filter(|end| *end <= limit)
            .ok_or(BlobError::WontFit(section))?;
        if let Some(link) = blob.range(section) {
            placement.regions.push(Region {
                section,
                start,
                end,
                link: *link,
            });
        }
        cursors[pool] = end;
        Ok(size)
    };
    for (blob, placement) in slots.iter().zip(&mut placements) {
        let (Some(blob), Some(placement)) = (blob, placement) else {
            continue;
        };
        for pool in [0, 1] {
            push(pool, blob, placement, &mut cursors)?;
            cursors[pool] = cursors[pool].next_multiple_of(16);
        }
    }
    for (blob, placement) in slots.iter().zip(&mut placements) {
        let (Some(blob), Some(placement)) = (blob, placement) else {
            continue;
        };
        let words = push(2, blob, placement, &mut cursors)?;
        cursors[3] = cursors[3].saturating_add(3 * words);
    }
    for (blob, placement) in slots.iter().zip(&mut placements) {
        if let (Some(blob), Some(placement)) = (blob, placement) {
            push(3, blob, placement, &mut cursors)?;
        }
    }
    Ok(placements)
}

/// One section copy: stock type 7 with `+4` = window-relative byte address.
#[derive(Clone, PartialEq, Eq)]
pub struct SectionTransfer {
    pub section: Section,
    pub address: u32,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for SectionTransfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SectionTransfer")
            .field("section", &self.section)
            .field("address", &format_args!("{:#x}", self.address))
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// A relocated model ready to copy and start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedModel {
    /// Runtime code address of the entry; the `0x1c` start record word.
    pub entry: u32,
    /// Sections 0, 1, 2, 3 in stock order, empty ones omitted.
    pub transfers: Vec<SectionTransfer>,
}

/// L1 block tables `(first, last, byte base)` for code parcels and ISA words
/// (VA 0x5d8e7c, tables 0x11e4620 / 0x11e4650).
const CODE_BLOCKS: [(u32, u32, u32); 4] = [
    (0x12_0000, 0x13_7fff, 0x24_0000),
    (0x16_0000, 0x17_7fff, 0x2c_0000),
    (0x18_0000, 0x18_ffff, 0x30_0000),
    (0x1c_0000, 0x1c_ffff, 0x38_0000),
];
const ISA_BLOCKS: [(u32, u32, u32); 4] = [
    (0x9_0000, 0x9_7fff, 0x24_0000),
    (0xb_0000, 0xb_7fff, 0x2c_0000),
    (0xc_0000, 0xc_5554, 0x30_0000),
    (0xe_0000, 0xe_5554, 0x38_0000),
];

/// Window byte address for a section's runtime start (VA 0x5d8e7c).
/// Byte sections are unchanged; unknown blocks are returned unchanged.
pub fn transfer_address(section: Section, start: u32) -> u32 {
    let table = match section {
        Section::Data0 | Section::Dm => return start,
        Section::Code => &CODE_BLOCKS,
        Section::Isa => &ISA_BLOCKS,
    };
    table
        .iter()
        .find(|(first, last, _)| (*first..=*last).contains(&start))
        .map_or(start, |(first, _, base)| {
            base + section.unit_bytes() * (start - first)
        })
}

/// Code relocation field per kind (VA 0x5d9cdc): `(mask, relative)` where
/// `None` means "relative when instruction bit 40 is set".
fn field(kind: u32) -> Result<(u32, Option<bool>), BlobError> {
    Ok(match kind {
        0 => (0xff_ffff, None),
        1..=3 => (0x7f_ffff, Some(true)),
        4..=10 => (u32::MAX, Some(false)),
        11 => (0xff_ffff, Some(false)),
        12 => (0xff_ffff, Some(true)),
        _ => return Err(BlobError::UnknownRelocation(kind)),
    })
}

/// Relocates one per-core blob to its placement (VAs 0x5d7c44, 0x5d683c,
/// 0x5d71f4) and splits it into transfers. Code relocations ignore the
/// record's target words: the target is decoded from the instruction field
/// (relative fields from `link start + offset`), remapped, and re-encoded.
/// Data relocations store `remap(target_base + target_offset)` as LE u32.
pub fn relocate(blob: &ModuleBlob, placement: &Placement) -> Result<PlacedModel, BlobError> {
    let mut placed = blob.clone();
    for group in &blob.relocations {
        let section = group.section;
        let missing = || BlobError::MissingSection(section);
        let old = *blob.range(section).ok_or_else(missing)?;
        let new_start = placement.remap(old.start);
        let original = blob.content(section).ok_or_else(missing)?;
        let bytes = placed.content_mut(section).ok_or_else(missing)?;
        let bounds = |offset: u32| BlobError::RelocationBounds { section, offset };
        match section {
            Section::Dm => {
                for record in group.data_relocations()? {
                    let at = record.byte_offset as usize;
                    let value =
                        placement.remap(record.target_base.wrapping_add(record.target_offset));
                    bytes
                        .get_mut(at..at.saturating_add(4))
                        .filter(|word| word.len() == 4)
                        .ok_or_else(|| bounds(record.byte_offset))?
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
            Section::Code | Section::Isa => {
                for record in group.code_relocations()? {
                    let at = (record.offset as usize)
                        .checked_mul(section.unit_bytes() as usize)
                        .ok_or_else(|| bounds(record.offset))?;
                    let window: [u8; 6] = at
                        .checked_add(6)
                        .and_then(|end| original.get(at..end))
                        .and_then(|window| window.try_into().ok())
                        .ok_or_else(|| bounds(record.offset))?;
                    let (low, high) = match section {
                        Section::Code => (
                            u32::from(u16::from_le_bytes([window[2], window[3]])) << 16
                                | u32::from(u16::from_le_bytes([window[4], window[5]])),
                            u16::from_le_bytes([window[0], window[1]]),
                        ),
                        _ => (
                            u32::from_le_bytes([window[0], window[1], window[2], window[3]]),
                            u16::from_le_bytes([window[4], window[5]]),
                        ),
                    };
                    let (mask, relative) = field(record.kind)?;
                    let relative = relative.unwrap_or(high & 0x100 != 0);
                    let raw = low & mask;
                    let target = if relative {
                        let half = (mask >> 1) + 1;
                        (raw ^ half)
                            .wrapping_sub(half)
                            .wrapping_add(old.start.wrapping_add(record.offset))
                    } else {
                        raw
                    };
                    let mut value = placement.remap(target);
                    if relative {
                        value = value.wrapping_sub(new_start.wrapping_add(record.offset));
                    }
                    let low = (low & !mask) | (value & mask);
                    let out = bytes
                        .get_mut(at..at + 6)
                        .ok_or_else(|| bounds(record.offset))?;
                    match section {
                        Section::Code => {
                            out[2..4].copy_from_slice(&((low >> 16) as u16).to_le_bytes());
                            out[4..6].copy_from_slice(&(low as u16).to_le_bytes());
                        }
                        _ => out[..4].copy_from_slice(&low.to_le_bytes()),
                    }
                }
            }
            Section::Data0 => return Err(BlobError::Data0Relocation),
        }
    }
    let mut transfers = Vec::new();
    for section in Section::ALL {
        let Some(bytes) = placed.content(section) else {
            continue;
        };
        let range = blob
            .range(section)
            .ok_or(BlobError::MissingSection(section))?;
        let start = placement.remap(range.start);
        transfers.push(SectionTransfer {
            section,
            address: transfer_address(section, start),
            bytes: bytes.to_vec(),
        });
    }
    Ok(PlacedModel {
        entry: placement.remap(blob.entry),
        transfers,
    })
}

#[cfg(test)]
mod tests;
