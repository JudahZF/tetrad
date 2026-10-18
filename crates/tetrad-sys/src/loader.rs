//! Hardware for `tetrad_loader::exec::execute`: [`BootHardware`] implements `PhysicalMemory` over `/dev/mem`
//! windows and `CoreControl` through [`CoreControlNode`].
//!
//! Stock maps the whole MMIO range in one call (`mmap64(.., 0x10000000, .., 0x30000000)`, ZenUI `0x8a57b0`).
//! Owned: 256 MiB is a large share of a 32-bit address space, so MMIO is mapped in [`MMIO_CHUNK`] sub-windows on
//! first use. The stock restart quiesce touches three of them (`0x30ff0000`, `0x31000000`, `0x31080000`).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use tetrad_dsp_host::{MemoryError, SharedMemory};
use tetrad_loader::PhysAddr;
use tetrad_loader::exec::{AccessError, CoreControl, PhysicalMemory};
use tetrad_loader::intent::{CoreAction, LoaderCore};

use crate::corectrl::CoreControlNode;
use crate::mem::{MapError, PhysicalWindow};
use crate::paths::{CORECTRL, DEV_MEM};

/// L2 SRAM used by loadsharc (booter sections, mailboxes, stream buffers).
pub const L2: (u32, u32) = (0x2008_0000, 0x4_0000);
/// Core 1 and core 2 L1 windows.
pub const L1: [(u32, u32); 2] = [(0x2824_0000, 0x16_0000), (0x28a4_0000, 0x16_0000)];
/// SEC, RCU, timers and the other system MMIO blocks.
pub const MMIO: (u32, u32) = (0x3000_0000, 0x1000_0000);
pub const MMIO_CHUNK: u32 = 0x1_0000;
/// The kernel's physical memory map; its "System RAM" ranges are Linux RAM.
pub const IOMEM: &str = "/proc/iomem";

/// Linux "System RAM" ranges from [`IOMEM`], inclusive ends as the file prints them. The SOC2
/// image copy never lands there (review B4): a wrong type 5 address would overwrite kernel or
/// page-cache memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemRam(Vec<(u64, u64)>);

impl SystemRam {
    /// Every `start-end : System RAM` line, top level or nested. Fails on a malformed RAM line,
    /// on no RAM line, and on hidden addresses (a reader without `CAP_SYS_ADMIN` sees zeros).
    pub fn parse(iomem: &str) -> io::Result<Self> {
        let invalid = |detail: &str| io::Error::new(io::ErrorKind::InvalidData, detail.to_owned());
        let ranges = iomem
            .lines()
            .filter_map(|line| line.split_once(" : "))
            .filter(|(_, name)| name.trim() == "System RAM")
            .map(|(range, _)| {
                let (start, end) = range
                    .trim()
                    .split_once('-')
                    .ok_or_else(|| invalid("System RAM line without a range"))?;
                let hex = |text| {
                    u64::from_str_radix(text, 16)
                        .map_err(|_| invalid("System RAM address is not hex"))
                };
                Ok((hex(start)?, hex(end)?))
            })
            .collect::<io::Result<Vec<_>>>()?;
        if ranges.iter().all(|&range| range == (0, 0)) {
            return Err(invalid("no readable System RAM range"));
        }
        Ok(Self(ranges))
    }

    fn overlaps(&self, base: u64, len: u64) -> bool {
        let last = base + len.saturating_sub(1);
        self.0
            .iter()
            .any(|&(start, end)| base <= end && start <= last)
    }
}

/// Physical memory and core control for one boot. Every window is `MAP_SHARED` from a `/dev/mem` descriptor
/// opened `O_RDWR | O_SYNC` (stock `0x101002`), so L2 and MMIO map strongly ordered and DDR write-combined.
#[derive(Debug)]
pub struct BootHardware {
    mem: File,
    fixed: [PhysicalWindow; 3],
    mmio: BTreeMap<u32, PhysicalWindow>,
    soc2: Option<PhysicalWindow>,
    /// Read at open; an error refuses every SOC2 buffer (fail closed).
    system_ram: Result<SystemRam, String>,
    corectrl: CoreControlNode,
    origin: Instant,
}

impl BootHardware {
    /// Explicit hardware opt-in: opens `/dev/mem` and `/dev/corectrl` and maps L2 and both L1 windows.
    pub fn open() -> Result<Self, MapError> {
        let mem = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_SYNC)
            .open(DEV_MEM)?;
        Self::new(mem, CoreControlNode::open(Path::new(CORECTRL))?)
    }

    /// `mem` is any mappable descriptor whose file offsets are physical addresses (tests use a sparse file).
    /// Reads Linux System RAM from [`IOMEM`] for the SOC2 buffer check.
    pub fn new(mem: File, corectrl: CoreControlNode) -> Result<Self, MapError> {
        let map =
            |(base, len): (u32, u32)| PhysicalWindow::map(mem.as_fd(), base.into(), len as usize);
        let fixed = [map(L2)?, map(L1[0])?, map(L1[1])?];
        let system_ram = std::fs::read_to_string(IOMEM)
            .and_then(|iomem| SystemRam::parse(&iomem))
            .map_err(|error| format!("{IOMEM}: {error}"));
        Ok(Self {
            mem,
            fixed,
            mmio: BTreeMap::new(),
            soc2: None,
            system_ram,
            corectrl,
            origin: Instant::now(),
        })
    }

    /// The SOC2 DDR buffer SOC1 reported (type 5, stock copy `0x89bae0`), `len` = both images. Map it before
    /// running the copy plan; it replaces an earlier buffer. Owned guard (review B4): a range that overlaps
    /// Linux System RAM, or any range when System RAM is unknown, is refused and nothing is mapped.
    pub fn map_soc2_buffer(&mut self, base: PhysAddr, len: u32) -> Result<(), MapError> {
        self.soc2 = None;
        let refused = |reason: String| {
            MapError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("SOC2 buffer {base}+{len:#x} refused: {reason}"),
            ))
        };
        match &self.system_ram {
            Ok(ram) if ram.overlaps(base.0.into(), len.into()) => {
                return Err(refused("it overlaps Linux System RAM".into()));
            }
            Ok(_) => {}
            Err(reason) => return Err(refused(format!("System RAM unknown ({reason})"))),
        }
        self.soc2 = Some(PhysicalWindow::map(
            self.mem.as_fd(),
            base.0.into(),
            len as usize,
        )?);
        Ok(())
    }

    pub fn unmap_soc2_buffer(&mut self) {
        self.soc2 = None;
    }

    /// The window holding `address` and the offset into it. An MMIO chunk is mapped on first use.
    fn locate(&mut self, address: PhysAddr) -> Result<(&mut PhysicalWindow, usize), AccessError> {
        let at = u64::from(address.0);
        let contains = |window: &PhysicalWindow| {
            (window.base()..window.base() + window.len() as u64).contains(&at)
        };
        let (mmio_base, mmio_len) = MMIO;
        let window = if let Some(index) = self.fixed.iter().position(contains) {
            &mut self.fixed[index]
        } else if self.soc2.as_ref().is_some_and(contains) {
            self.soc2.as_mut().expect("checked above")
        } else if (mmio_base..mmio_base + mmio_len).contains(&address.0) {
            let chunk = address.0 - address.0 % MMIO_CHUNK;
            match self.mmio.entry(chunk) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let window =
                        PhysicalWindow::map(self.mem.as_fd(), chunk.into(), MMIO_CHUNK as usize)
                            .map_err(|error| AccessError::Device(map_error_kind(&error)))?;
                    entry.insert(window)
                }
            }
        } else {
            return Err(AccessError::Unmapped(address));
        };
        let offset = (at - window.base()) as usize;
        Ok((window, offset))
    }

    /// Runs one access inside the window holding `address`; a range past the window end is `Unmapped`.
    fn access<T>(
        &mut self,
        address: PhysAddr,
        access: impl FnOnce(&mut PhysicalWindow, usize) -> Result<T, MemoryError>,
    ) -> Result<T, AccessError> {
        let (window, offset) = self.locate(address)?;
        access(window, offset).map_err(|error| match error {
            MemoryError::Misaligned(_) => AccessError::Misaligned(address),
            _ => AccessError::Unmapped(address),
        })
    }
}

fn map_error_kind(error: &MapError) -> io::ErrorKind {
    match error {
        MapError::Io(error) => error.kind(),
        _ => io::ErrorKind::InvalidInput,
    }
}

impl PhysicalMemory for BootHardware {
    fn read_u32(&mut self, address: PhysAddr) -> Result<u32, AccessError> {
        let value = self.access(address, |window, offset| window.read_u32(offset))?;
        PhysicalWindow::barrier();
        Ok(value)
    }

    fn write_u32(&mut self, address: PhysAddr, value: u32) -> Result<(), AccessError> {
        self.access(address, |window, offset| window.write_u32(offset, value))?;
        PhysicalWindow::barrier();
        Ok(())
    }

    fn write_u16(&mut self, address: PhysAddr, value: u16) -> Result<(), AccessError> {
        self.access(address, |window, offset| window.write_u16(offset, value))?;
        PhysicalWindow::barrier();
        Ok(())
    }

    /// Splits at window ends (MMIO chunks), then writes with aligned words between head and tail bytes. A
    /// `dmb sy` follows, so the bytes are ordered before the next mailbox access (sufficiency for DDR is **I**).
    fn write_bytes(&mut self, address: PhysAddr, bytes: &[u8]) -> Result<(), AccessError> {
        let mut done = 0;
        while done < bytes.len() {
            let at = u32::try_from(done)
                .ok()
                .and_then(|done| address.checked_add(done))
                .ok_or(AccessError::Unmapped(address))?;
            let written = self.access(at, |window, offset| {
                let count = (window.len() - offset).min(bytes.len() - done);
                window.write(offset, &bytes[done..done + count])?;
                Ok(count)
            })?;
            done += written;
        }
        PhysicalWindow::barrier();
        Ok(())
    }

    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn delay(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

impl CoreControl for BootHardware {
    fn control(&mut self, core: LoaderCore, action: CoreAction) -> Result<(), AccessError> {
        self.corectrl.control(core, action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sparse file stands in for `/dev/mem`: offsets are physical addresses.
    fn hardware(tag: &str) -> (BootHardware, File) {
        let path =
            std::env::temp_dir().join(format!("tetrad-sys-mem-{tag}-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(u64::from(MMIO.0 + MMIO.1)).unwrap();
        std::fs::remove_file(&path).unwrap();
        let (reader, _writer) = io::pipe().unwrap();
        let corectrl = CoreControlNode::from_fd(reader.into());
        let mut hardware = BootHardware::new(file.try_clone().unwrap(), corectrl).unwrap();
        // The host's own map is not the QC's: Linux RAM of the shipped device tree.
        hardware.system_ram = SystemRam::parse(IOMEM_SAMPLE).map_err(|error| error.to_string());
        (hardware, file)
    }

    /// A QC-shaped `/proc/iomem` (invented, shaped as the `zpu.dtb` memory node).
    const IOMEM_SAMPLE: &str = "\
20080000-20080fff : icc
31000000-310000ff : serial
83000000-91ffffff : System RAM
  83008000-834fffff : Kernel code
  83500000-835fffff : Kernel data
";

    fn read_at(file: &File, address: u32, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        std::os::unix::fs::FileExt::read_exact_at(file, &mut bytes, address.into()).unwrap();
        bytes
    }

    /// Accesses land at their physical address in L2, L1, on-demand MMIO chunks and the SOC2 buffer; anything
    /// else is refused, and alignment is checked on the physical address.
    #[test]
    fn accesses_resolve_to_physical_windows_and_mmio_maps_on_demand() {
        let (mut hw, file) = hardware("windows");
        hw.write_u32(PhysAddr(0x200b_cd98), 0xdead).unwrap();
        assert_eq!(hw.read_u32(PhysAddr(0x200b_cd98)), Ok(0xdead));
        hw.write_bytes(PhysAddr(0x2839_fffe), b"QC").unwrap();
        hw.write_u16(PhysAddr(0x3100_1120), 0xbeef).unwrap();
        // A byte run across an MMIO chunk boundary maps both chunks.
        hw.write_bytes(PhysAddr(0x3108_fffe), &[1, 2, 3, 4])
            .unwrap();
        assert_eq!(hw.mmio.len(), 3);
        assert_eq!(read_at(&file, 0x200b_cd98, 4), 0xdead_u32.to_ne_bytes());
        assert_eq!(read_at(&file, 0x2839_fffe, 2), b"QC");
        assert_eq!(read_at(&file, 0x3100_1120, 2), 0xbeef_u16.to_ne_bytes());
        assert_eq!(read_at(&file, 0x3108_fffe, 4), [1, 2, 3, 4]);

        assert_eq!(
            hw.write_u32(PhysAddr(0x8000_0000), 1),
            Err(AccessError::Unmapped(PhysAddr(0x8000_0000)))
        );
        assert_eq!(
            hw.read_u32(PhysAddr(0x2008_0002)),
            Err(AccessError::Misaligned(PhysAddr(0x2008_0002)))
        );
        // The last word of L2 fits; a word straddling its end does not.
        assert!(hw.write_u32(PhysAddr(0x200b_fffc), 0).is_ok());
        assert_eq!(
            hw.write_bytes(PhysAddr(0x200b_fffe), &[0; 4]),
            Err(AccessError::Unmapped(PhysAddr(0x200c_0000)))
        );

        hw.map_soc2_buffer(PhysAddr(0x2000_1000), 0x100).unwrap();
        hw.write_bytes(PhysAddr(0x2000_1000), b"BLw").unwrap();
        assert_eq!(read_at(&file, 0x2000_1000, 3), b"BLw");
        hw.unmap_soc2_buffer();
        assert_eq!(
            hw.write_u32(PhysAddr(0x2000_1000), 0),
            Err(AccessError::Unmapped(PhysAddr(0x2000_1000)))
        );
    }

    /// Review B4: a SOC2 buffer that touches Linux System RAM is refused before any map or write, and so is
    /// every buffer when the map is unreadable (hidden zero addresses); a buffer outside RAM maps. (The DDR
    /// window is tetrad-loader's check; the sparse test file ends at `0x4000_0000`.)
    #[test]
    fn soc2_buffer_in_system_ram_is_refused() {
        let (mut hw, file) = hardware("soc2-ram");
        for (base, len) in [
            (0x8300_0000, 0x100),
            (0x82ff_fff0, 0x20),
            (0x91ff_ff00, 0x100),
        ] {
            assert!(
                hw.map_soc2_buffer(PhysAddr(base), len).is_err(),
                "{base:#x}"
            );
            assert!(hw.soc2.is_none());
        }
        hw.map_soc2_buffer(PhysAddr(0x2000_2000), 0x100).unwrap();
        hw.write_bytes(PhysAddr(0x2000_2000), b"BLw").unwrap();
        assert_eq!(read_at(&file, 0x2000_2000, 3), b"BLw");

        let hidden = "00000000-00000000 : System RAM\n  00000000-00000000 : Kernel code\n";
        hw.system_ram = SystemRam::parse(hidden).map_err(|error| error.to_string());
        assert!(hw.map_soc2_buffer(PhysAddr(0x8100_0000), 0x100).is_err());
        assert!(SystemRam::parse("83000000-xyz : System RAM\n").is_err());
    }

    /// Core control reaches the descriptor as one ioctl (a pipe refuses it); entry 0 never reaches it.
    #[test]
    fn core_control_sends_the_scalar_intent() {
        let (mut hw, _file) = hardware("corectrl");
        assert_eq!(
            hw.control(LoaderCore::Core2, CoreAction::Stop),
            Err(AccessError::Device(
                io::Error::from_raw_os_error(libc::ENOTTY).kind()
            ))
        );
        assert_eq!(
            hw.control(LoaderCore::Core1, CoreAction::SetEntry(0)),
            Err(AccessError::Device(io::ErrorKind::InvalidInput))
        );
    }
}
