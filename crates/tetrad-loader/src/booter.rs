//! The SHARC booter embedded in stock `/usr/bin/loadsharc_SC589` and its L2 mailboxes.
//!
//! loadsharc does not parse LDR on ARM. Per core it uploads an 8-section booter
//! program into shared L2 (`0x94d0`), points the core at it, and streams the raw
//! kernel file through an L2 mailbox (`0x87e4`). The booter bytes are proprietary,
//! so they are read at run time from the device's own loadsharc binary; nothing is
//! embedded here. The table VA and the app mailbox belong to one build: they come from the
//! running firmware profile (`tetrad_firmware::running().loadsharc`). Callers check the
//! loadsharc SHA-256 against that build before [`StockBooter::parse`]. The SC589 windows
//! below are hardware facts.
use crate::intent::LoaderCore;
use crate::{Error, PhysAddr};

/// Shared L2 window loadsharc maps from `/dev/mem`.
pub const L2_BASE: u32 = 0x2008_0000;
pub const L2_LEN: u32 = 0x4_0000;
/// Per-core L1 window length loadsharc maps.
pub const L1_LEN: u32 = 0x16_0000;
/// Core-local L1 range accepted for the argument buffer (`0x9100..`).
const L1_LOCAL: u32 = 0x24_0000;
const NW_BASE: u32 = 0x0802_0000;
const NW_LEN: u32 = 0x1_0000;
const VISA_BASE: u32 = 0x00ba_0000;
const ISA_BASE: u32 = 0x005a_0000;
const SHORT_LEN: u32 = 0x2_0000;
/// `VISA * 2 + VISA_TO_BYTE` is the L2 byte address (`0x96c4`).
const VISA_TO_BYTE: u32 = L2_BASE - 0x0174_0000;
const MAX_SECTIONS: u32 = 16;

/// Byte order loadsharc applies while copying one section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swap {
    /// Plain `memcpy` (L2 byte target).
    None,
    /// Each 4-byte group reversed (normal-word target).
    Words,
    /// Each byte pair swapped (VISA short-word target).
    Halfwords,
}

impl Swap {
    /// Stock copy loops run in whole groups, so a section of `size` bytes writes this many.
    pub const fn written_len(self, size: u32) -> u32 {
        match self {
            Self::None => size,
            Self::Words => size.next_multiple_of(4),
            Self::Halfwords => size.next_multiple_of(2),
        }
    }
}

/// Section source bytes. `Zeros` covers loadsharc's own `.bss`, which is zero at process start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source<'a> {
    Bytes(&'a [u8]),
    Zeros(u32),
}

impl Source<'_> {
    pub fn len(&self) -> u32 {
        match self {
            // Bounded by the u32 span that produced the slice.
            Self::Bytes(bytes) => bytes.len() as u32,
            Self::Zeros(len) => *len,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement<'a> {
    pub address: PhysAddr,
    pub swap: Swap,
    /// Already widened to [`Swap::written_len`]; the stock loop over-reads the next source bytes.
    pub source: Source<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BooterSection<'a> {
    /// SHARC address from the table (byte, normal-word or VISA space).
    pub target: u32,
    pub size: u32,
    /// `None` when stock skips the section (core-local L1 targets such as `0x90000`).
    pub placement: Option<Placement<'a>>,
}

/// Classifies a table target exactly as loadsharc `0x94d0` does, in the same order.
fn place(target: u32, size: u32) -> Result<Option<(PhysAddr, Swap)>, Error> {
    let (address, swap) = if target.wrapping_sub(L2_BASE) < L2_LEN {
        (target, Swap::None)
    } else if target.wrapping_sub(NW_BASE) < NW_LEN {
        (target * 4, Swap::Words)
    } else if target.wrapping_sub(VISA_BASE) < SHORT_LEN {
        (target * 2 + VISA_TO_BYTE, Swap::Halfwords)
    } else if target.wrapping_sub(ISA_BASE) < SHORT_LEN {
        // Stock computes `target * 4 - 0x1740000` from the L2 mapping, which lands below it.
        return Err(Error::Booter("ISA section would be written outside L2"));
    } else {
        return Ok(None);
    };
    let end = address
        .checked_add(swap.written_len(size))
        .ok_or(Error::Overflow("booter section"))?;
    if end > L2_BASE + L2_LEN {
        return Err(Error::Booter("section runs past the L2 window"));
    }
    Ok(Some((PhysAddr(address), swap)))
}

/// L2 mailbox of the stream phase (`0x8d00..0x9060`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamMailbox {
    /// Where each requested chunk of the LDR file is written.
    pub buffer: PhysAddr,
    /// Bytes from `buffer` to the next mailbox word. Stock never checks it; tetrad-loader does.
    pub capacity: u32,
    /// ARM writes the delivered byte count here (`ptrLdSync2ShByteCount`).
    pub to_sharc_count: PhysAddr,
    /// SHARC writes the requested byte count here (`ptrLdSync2ArmByteCount`).
    pub to_arm_count: PhysAddr,
    pub arm_to_sharc: PhysAddr,
    pub sharc_to_arm: PhysAddr,
}

/// L2 mailbox of the application phase, immediates in loadsharc `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppMailbox {
    pub sharc_to_arm: PhysAddr,
    pub arm_to_sharc: PhysAddr,
    /// SHARC writes the argument buffer address here.
    pub data_to_arm: PhysAddr,
    /// ARM writes the argument length here.
    pub data_to_sharc: PhysAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    L1,
    L2,
}

/// Per-core constants of loadsharc `main` (`0x87e4`): the core-local bases (hardware) and
/// the app mailbox and table VA of the running build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreMap {
    /// OR-ed into core-local addresses.
    pub local_base: u32,
    pub l1: PhysAddr,
    pub app: AppMailbox,
    config_va: u32,
}

impl CoreMap {
    pub fn of(core: LoaderCore) -> Self {
        let layout = tetrad_firmware::running().loadsharc;
        let (local_base, l1, booter) = match core {
            LoaderCore::Core1 => (0x2800_0000, 0x2824_0000, layout.core1),
            LoaderCore::Core2 => (0x2880_0000, 0x28a4_0000, layout.core2),
        };
        let (app, config_va) = (booter.app_mailbox, booter.config_va);
        Self {
            local_base,
            l1: PhysAddr(l1),
            app: AppMailbox {
                sharc_to_arm: PhysAddr(app),
                arm_to_sharc: PhysAddr(app + 4),
                data_to_arm: PhysAddr(app + 8),
                data_to_sharc: PhysAddr(app + 12),
            },
            config_va,
        }
    }
    /// Mailbox address translation of loadsharc `0x949c`: L2 is identity, anything
    /// else keeps its low 22 bits under the core base. `None` when the result is
    /// outside both mapped windows.
    pub fn translate(&self, address: u32) -> Option<PhysAddr> {
        if address.wrapping_sub(L2_BASE) < L2_LEN {
            return Some(PhysAddr(address));
        }
        let local = (address & 0x003f_ffff) | self.local_base;
        (local.wrapping_sub(self.l1.0) < L1_LEN).then_some(PhysAddr(local))
    }
    /// Argument buffer placement of loadsharc `main` (`0x9130..0x91b8`).
    pub fn argument_address(&self, address: u32) -> Option<(PhysAddr, Region)> {
        if address.wrapping_sub(L1_LOCAL) < L1_LEN {
            Some((PhysAddr(address | self.local_base), Region::L1))
        } else if address.wrapping_sub(L2_BASE) < L2_LEN {
            Some((PhysAddr(address), Region::L2))
        } else {
            None
        }
    }
    /// End of the mapped window that holds `address`.
    pub fn window_end(&self, address: PhysAddr) -> Option<u32> {
        if address.0.wrapping_sub(L2_BASE) < L2_LEN {
            Some(L2_BASE + L2_LEN)
        } else if address.0.wrapping_sub(self.l1.0) < L1_LEN {
            Some(self.l1.0 + L1_LEN)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BooterImage<'a> {
    pub core: LoaderCore,
    pub map: CoreMap,
    /// VISA address of the booter entry, written to the core's start vector.
    pub app_start: u32,
    pub stream: StreamMailbox,
    pub sections: Vec<BooterSection<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockBooter<'a> {
    pub core1: BooterImage<'a>,
    pub core2: BooterImage<'a>,
}

impl<'a> StockBooter<'a> {
    /// Reads both booter tables from the loadsharc ELF of the running build. Fails unless each
    /// table carries its `SharcBooter_Core<n>.dxe` name section.
    pub fn parse(elf: &'a [u8]) -> Result<Self, Error> {
        let elf = Elf::parse(elf)?;
        Ok(Self {
            core1: image(&elf, LoaderCore::Core1)?,
            core2: image(&elf, LoaderCore::Core2)?,
        })
    }
    pub fn image(&self, core: LoaderCore) -> &BooterImage<'a> {
        match core {
            LoaderCore::Core1 => &self.core1,
            LoaderCore::Core2 => &self.core2,
        }
    }
}

fn image<'a>(elf: &Elf<'a>, core: LoaderCore) -> Result<BooterImage<'a>, Error> {
    let map = CoreMap::of(core);
    // Config words: stream buffer, to-SHARC count, to-ARM count, ARM→SHARC, SHARC→ARM,
    // app start, section count; then 12-byte (target, source VA, size) entries.
    let word = |index: u32| elf.word(map.config_va + 4 * index);
    let mailbox = |index: u32| {
        let address = map
            .translate(word(index)?)
            .filter(|address| address.0.is_multiple_of(4))
            .ok_or(Error::Booter("mailbox outside the mapped windows"))?;
        Ok::<_, Error>(address)
    };
    let buffer = mailbox(0)?;
    let to_sharc_count = mailbox(1)?;
    let to_arm_count = mailbox(2)?;
    let arm_to_sharc = mailbox(3)?;
    let sharc_to_arm = mailbox(4)?;
    let app_start = word(5)?;
    let count = word(6)?;
    if count == 0 || count > MAX_SECTIONS {
        return Err(Error::Limit("booter sections"));
    }
    if app_start.wrapping_sub(VISA_BASE) >= SHORT_LEN {
        return Err(Error::Booter("app start is not an L2 VISA address"));
    }
    let limit = map
        .window_end(buffer)
        .ok_or(Error::Booter("stream buffer"))?;
    let capacity = [to_sharc_count, to_arm_count, arm_to_sharc, sharc_to_arm]
        .into_iter()
        .map(|address| address.0)
        .filter(|&address| address > buffer.0)
        .fold(limit, u32::min)
        - buffer.0;
    let sections = (0..count)
        .map(|index| {
            let entry = map.config_va + 0x1c + 12 * index;
            let target = elf.word(entry)?;
            let source = elf.word(entry + 4)?;
            let size = elf.word(entry + 8)?;
            let placement = match place(target, size)? {
                Some((address, swap)) => Some(Placement {
                    address,
                    swap,
                    source: elf.span(source, swap.written_len(size))?,
                }),
                None => None,
            };
            Ok(BooterSection {
                target,
                size,
                placement,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let name = match core {
        LoaderCore::Core1 => b"SharcBooter_Core1.dxe\0",
        LoaderCore::Core2 => b"SharcBooter_Core2.dxe\0",
    };
    let named = sections.iter().any(|section| {
        matches!(section.placement, Some(Placement { source: Source::Bytes(bytes), .. }) if bytes == name)
    });
    if !named {
        return Err(Error::Booter(
            "booter name section not found; not the loadsharc of the running build",
        ));
    }
    Ok(BooterImage {
        core,
        map,
        app_start,
        stream: StreamMailbox {
            buffer,
            capacity,
            to_sharc_count,
            to_arm_count,
            arm_to_sharc,
            sharc_to_arm,
        },
        sections,
    })
}

/// Minimal ELF32 little-endian ARM executable view: virtual address → file bytes.
struct Elf<'a> {
    bytes: &'a [u8],
    loads: Vec<Load>,
}

#[derive(Clone, Copy)]
struct Load {
    vaddr: u32,
    offset: u32,
    file_size: u32,
    mem_size: u32,
}

const PT_LOAD: u32 = 1;
const EM_ARM: u16 = 40;
const ET_EXEC: u16 = 2;
const MAX_PROGRAM_HEADERS: u16 = 32;

impl<'a> Elf<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let header = bytes.get(..0x34).ok_or(Error::Truncated {
            part: "ELF header",
            offset: bytes.len(),
        })?;
        let half = |offset: usize| u16::from_le_bytes([header[offset], header[offset + 1]]);
        if header[..6] != *b"\x7fELF\x01\x01" || half(0x10) != ET_EXEC || half(0x12) != EM_ARM {
            return Err(Error::Booter("not a little-endian ARM ELF32 executable"));
        }
        let table = le_word(bytes, 0x1c)? as usize;
        let (entry_size, count) = (half(0x2a), half(0x2c));
        if entry_size != 32 || count > MAX_PROGRAM_HEADERS {
            return Err(Error::Booter("unexpected program header table"));
        }
        let mut loads = Vec::new();
        for index in 0..usize::from(count) {
            let at = index
                .checked_mul(32)
                .and_then(|offset| table.checked_add(offset))
                .ok_or(Error::Overflow("program header"))?;
            if le_word(bytes, at)? != PT_LOAD {
                continue;
            }
            let load = Load {
                offset: le_word(bytes, at + 4)?,
                vaddr: le_word(bytes, at + 8)?,
                file_size: le_word(bytes, at + 16)?,
                mem_size: le_word(bytes, at + 20)?,
            };
            let file_end = load.offset.checked_add(load.file_size);
            if load.mem_size < load.file_size
                || file_end.is_none_or(|end| end as usize > bytes.len())
                || load.vaddr.checked_add(load.mem_size).is_none()
            {
                return Err(Error::Booter("PT_LOAD segment exceeds the file"));
            }
            loads.push(load);
        }
        Ok(Self { bytes, loads })
    }
    /// The span `vaddr..vaddr + len` in one segment, wholly file-backed or wholly zero-fill.
    fn span(&self, vaddr: u32, len: u32) -> Result<Source<'a>, Error> {
        let end = vaddr.checked_add(len).ok_or(Error::Overflow("ELF span"))?;
        let load = self
            .loads
            .iter()
            .find(|load| vaddr >= load.vaddr && end <= load.vaddr + load.mem_size)
            .ok_or(Error::Booter("source address is not in a loaded segment"))?;
        let relative = vaddr - load.vaddr;
        if relative >= load.file_size {
            return Ok(Source::Zeros(len));
        }
        if relative + len > load.file_size {
            return Err(Error::Booter("source straddles file data and zero fill"));
        }
        let start = (load.offset + relative) as usize;
        Ok(Source::Bytes(&self.bytes[start..start + len as usize]))
    }
    fn word(&self, vaddr: u32) -> Result<u32, Error> {
        match self.span(vaddr, 4)? {
            Source::Bytes(bytes) => le_word(bytes, 0),
            Source::Zeros(_) => Ok(0),
        }
    }
}

fn le_word(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    offset
        .checked_add(4)
        .and_then(|end| bytes.get(offset..end))
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .ok_or(Error::Truncated {
            part: "ELF word",
            offset,
        })
}
