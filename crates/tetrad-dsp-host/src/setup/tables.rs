//! Stock driver tables by ZenUI virtual address. The repository holds no
//! table bytes: the platform reads them from the device's own ZenUI.
//!
//! Every [`Segment`] names its data by the CorOS 4.1.0 VA (the site VA, a stable
//! id). [`ElfTables`] accepts only the ZenUI of the running build
//! (`tetrad_firmware::running()`, by SHA-256) and reads each site at that build's VA
//! (`tetrad_firmware::ZenUiVas`). [`check_zenui_vas`] proves before start-up that
//! the build has a VA for every site.

use sha2::{Digest, Sha256};
use tetrad_firmware::{Profile, ZenUiVas};

/// One `memcpy` source in a driver constructor. `va` is the site VA (CorOS 4.1.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub va: u32,
    pub len: u32,
}

/// The 57-byte seed of the kernel, `moduleReport.xml` and media ciphers (`0x009f4bbc`
/// immediates; .rodata copy at this VA).
pub const CIPHER_SEED: StockPayload = StockPayload::new(&[Segment {
    va: 0x0141_1478,
    len: tetrad_media::factory::SEED_LEN as u32,
}]);

/// Every site the platform reads from ZenUI by VA: the driver payloads
/// ([`super::drivers::stock_payloads`]), the media tables and [`CIPHER_SEED`].
pub fn zenui_reads() -> impl Iterator<Item = StockPayload> {
    super::drivers::stock_payloads()
        .chain(super::drivers::media::payloads())
        .chain([CIPHER_SEED])
}

/// Whether `profile` has a ZenUI VA for every site of [`zenui_reads`]. Call it before
/// `tetrad_firmware::select`; a build that fails must not run.
pub fn check_zenui_vas(profile: &Profile) -> Result<(), TableError> {
    let vas = profile.zenui.vas;
    zenui_reads()
        .flat_map(|payload| payload.segments().iter().copied())
        .try_for_each(|segment| relocate(vas, segment).map(drop))
}

/// The segment at its VA in the build of `vas`.
fn relocate(vas: ZenUiVas, segment: Segment) -> Result<Segment, TableError> {
    let va = vas
        .relocate(segment.va, segment.len)
        .ok_or(TableError::Unrelocated {
            va: segment.va,
            len: segment.len,
        })?;
    Ok(Segment { va, ..segment })
}

/// A constructor payload: its segments concatenated in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StockPayload(&'static [Segment]);

impl StockPayload {
    pub const fn new(segments: &'static [Segment]) -> Self {
        Self(segments)
    }
    pub const fn segments(self) -> &'static [Segment] {
        self.0
    }
    pub fn len(self) -> usize {
        self.0.iter().map(|segment| segment.len as usize).sum()
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TableError {
    #[error("not the ZenUI ELF of the running CorOS build")]
    WrongImage,
    #[error("malformed ELF program headers")]
    Malformed,
    /// Not file-backed, or not kept by [`ElfTables::keeping`].
    #[error("{len:#x} bytes at VA {va:#x} are not available")]
    Unmapped { va: u32, len: u32 },
    /// No stock tables were loaded (no `/opt/neuraldsp/ZenUI`).
    #[error("no stock tables are loaded")]
    Absent,
    /// The running build has no VA for this site ([`check_zenui_vas`]).
    #[error("{len:#x} bytes at site VA {va:#x} have no VA in the running build")]
    Unrelocated { va: u32, len: u32 },
}

/// Source of stock table bytes.
pub trait StockTables {
    fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError>;
}

/// Tables that may be absent: every read fails with [`TableError::Absent`].
impl<T: StockTables + ?Sized> StockTables for Option<&T> {
    fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
        self.ok_or(TableError::Absent)?.read(payload)
    }
}

#[derive(Debug, Clone, Copy)]
struct Load {
    va: u32,
    offset: u32,
    file_bytes: u32,
}

/// The ZenUI ELF of the running build in memory, read through its `PT_LOAD`
/// headers, or only the ranges it keeps ([`Self::keeping`]). Reads take site VAs.
pub struct ElfTables {
    image: Vec<u8>,
    loads: Vec<Load>,
    vas: ZenUiVas,
}

impl std::fmt::Debug for ElfTables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElfTables")
            .field("bytes", &self.image.len())
            .field("loads", &self.loads)
            .finish()
    }
}

const PT_LOAD: u32 = 1;

impl ElfTables {
    /// The driver tables only ([`super::drivers::stock_payloads`], about
    /// 7.3 MB instead of the 22 MB image). Use this for the long-lived
    /// table that [`crate::session::DspSession::run_setup`] reads.
    pub fn drivers(image: Vec<u8>) -> Result<Self, TableError> {
        Self::keeping(image, super::drivers::stock_payloads())
    }

    /// Checks the image, then keeps only the bytes of `payloads` (merged
    /// ranges); other reads fail with [`TableError::Unmapped`].
    pub fn keeping(
        image: Vec<u8>,
        payloads: impl IntoIterator<Item = StockPayload>,
    ) -> Result<Self, TableError> {
        Self::new(image)?.keep(payloads)
    }

    /// Only the bytes of `payloads` of this image (merged ranges).
    pub(crate) fn keep(
        self,
        payloads: impl IntoIterator<Item = StockPayload>,
    ) -> Result<Self, TableError> {
        let full = self;
        let mut segments = payloads
            .into_iter()
            .flat_map(|payload| payload.segments().iter().copied())
            .map(|segment| relocate(full.vas, segment))
            .collect::<Result<Vec<_>, _>>()?;
        segments.sort_by_key(|segment| segment.va);
        let end = |segment: &Segment| u64::from(segment.va) + u64::from(segment.len);
        let mut merged: Vec<Segment> = Vec::new();
        for segment in segments {
            match merged.last_mut() {
                Some(last) if u64::from(segment.va) <= end(last) => {
                    last.len = (end(last).max(end(&segment)) - u64::from(last.va)) as u32;
                }
                _ => merged.push(segment),
            }
        }
        let mut image = Vec::new();
        let mut loads = Vec::with_capacity(merged.len());
        for segment in merged {
            loads.push(Load {
                va: segment.va,
                offset: image.len() as u32,
                file_bytes: segment.len,
            });
            image.extend_from_slice(full.mapped(segment)?);
        }
        Ok(Self {
            image,
            loads,
            vas: full.vas,
        })
    }

    /// The whole checked image; fine for one-off reads.
    pub fn new(image: Vec<u8>) -> Result<Self, TableError> {
        let running = tetrad_firmware::running();
        if Sha256::digest(&image).as_slice() != running.firmware.identity().zenui_sha256 {
            return Err(TableError::WrongImage);
        }
        let u16_at = |at: usize| {
            image
                .get(at..at + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
        };
        let u32_at = |at: usize| {
            image
                .get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let header = (|| {
            let phoff = usize::try_from(u32_at(0x1c)?).ok()?;
            let entry = usize::from(u16_at(0x2a)?);
            let count = usize::from(u16_at(0x2c)?);
            let mut loads = Vec::new();
            for index in 0..count {
                let at = phoff.checked_add(index.checked_mul(entry)?)?;
                if u32_at(at)? == PT_LOAD {
                    loads.push(Load {
                        offset: u32_at(at + 4)?,
                        va: u32_at(at + 8)?,
                        file_bytes: u32_at(at + 16)?,
                    });
                }
            }
            Some(loads)
        })();
        let loads = header.ok_or(TableError::Malformed)?;
        Ok(Self {
            image,
            loads,
            vas: running.zenui.vas,
        })
    }

    /// No bytes: every read fails with [`TableError::Unmapped`].
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            image: Vec::new(),
            loads: Vec::new(),
            vas: ZenUiVas::Canonical,
        }
    }

    /// `bytes` mapped at build VA `base`, with the site map `vas`; no hash check.
    #[cfg(test)]
    pub(crate) fn flat(base: u32, bytes: Vec<u8>, vas: ZenUiVas) -> Self {
        let loads = vec![Load {
            va: base,
            offset: 0,
            file_bytes: bytes.len() as u32,
        }];
        Self {
            image: bytes,
            loads,
            vas,
        }
    }

    /// The bytes of a segment at its VA in this build (already relocated).
    fn mapped(&self, segment: Segment) -> Result<&[u8], TableError> {
        let unmapped = TableError::Unmapped {
            va: segment.va,
            len: segment.len,
        };
        let load = self
            .loads
            .iter()
            .find(|load| {
                segment.va >= load.va
                    && u64::from(segment.va) + u64::from(segment.len)
                        <= u64::from(load.va) + u64::from(load.file_bytes)
            })
            .ok_or(unmapped.clone())?;
        let start = (load.offset + (segment.va - load.va)) as usize;
        self.image
            .get(start..start + segment.len as usize)
            .ok_or(unmapped)
    }
}

impl StockTables for ElfTables {
    fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
        let mut bytes = Vec::with_capacity(payload.len());
        for &segment in payload.segments() {
            bytes.extend_from_slice(self.mapped(relocate(self.vas, segment)?)?);
        }
        Ok(bytes)
    }
}
