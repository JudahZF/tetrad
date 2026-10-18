//! Physical memory windows mapped through `/dev/mem`.
//!
//! Stock ZenUI opens `/dev/mem` with flags `0x101002` (`O_RDWR | O_SYNC`, VA 0x54df98) and maps each
//! window `MAP_SHARED` from a page-aligned offset. Inferred from Linux 4.0 `phys_mem_access_prot` (ARM): with
//! `O_SYNC`, RAM pages map write-combined; non-RAM pages (L2 SRAM, MMIO) map strongly ordered either way.
//! Strongly ordered memory faults on unaligned access, so the window uses only aligned 32-bit, 16-bit and
//! single-byte volatile accesses, never `memcpy`.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::ptr::NonNull;

use tetrad_dsp_host::{MemoryError, SharedMemory};

const WORD: usize = size_of::<u32>();

// glibc 2.18 ARM: O_SYNC = __O_SYNC | O_DSYNC (asm-generic/fcntl.h), so O_RDWR | O_SYNC is the stock 0x101002.
#[cfg(target_arch = "arm")]
const _: () = assert!(libc::O_RDWR | libc::O_SYNC == 0x10_1002);

#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("a physical window must not be empty")]
    Empty,
    #[error("physical range {base:#x}+{len:#x} does not fit the address space")]
    Overflow { base: u64, len: usize },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One `MAP_SHARED` mapping of the physical range `base..base + len`. Offsets are relative to `base`.
/// Dropping the window unmaps it; no access happens on map or unmap.
#[derive(Debug)]
pub struct PhysicalWindow {
    /// Page-aligned start of the mapping; the window begins `residue` bytes later.
    mapping: NonNull<u8>,
    mapping_len: usize,
    residue: usize,
    len: usize,
    base: u64,
}

// SAFETY: the window exclusively owns its mapping. The raw pointer is not tied to a thread, and every access
// goes through `&self`/`&mut self`, so moving the owner to another thread cannot create unsynchronized aliasing.
unsafe impl Send for PhysicalWindow {}

impl PhysicalWindow {
    /// Explicit hardware opt-in: opens `path` (normally [`crate::paths::DEV_MEM`]) with the stock
    /// `O_RDWR | O_SYNC` plus `O_CLOEXEC`, maps the range and closes the descriptor; the mapping stays valid.
    pub fn open(path: &Path, base: u64, len: usize) -> Result<Self, MapError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_SYNC)
            .open(path)?;
        Self::map(file.as_fd(), base, len)
    }

    /// [`Self::open`] for reads only: `O_RDONLY | O_SYNC` (the same memory type) and `PROT_READ`. The
    /// window derefs to a shared [`PhysicalWindow`] only, so no write method is reachable.
    pub fn open_read_only(path: &Path, base: u64, len: usize) -> Result<ReadOnlyWindow, MapError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_SYNC)
            .open(path)?;
        Self::map_with(file.as_fd(), base, len, libc::PROT_READ).map(ReadOnlyWindow)
    }

    /// Maps `base..base + len` of any mappable descriptor read/write and shared. Tests use a regular file;
    /// on the device this is a `/dev/mem` descriptor whose open flags select the memory type.
    pub fn map(fd: BorrowedFd<'_>, base: u64, len: usize) -> Result<Self, MapError> {
        Self::map_with(fd, base, len, libc::PROT_READ | libc::PROT_WRITE)
    }

    fn map_with(
        fd: BorrowedFd<'_>,
        base: u64,
        len: usize,
        protection: libc::c_int,
    ) -> Result<Self, MapError> {
        if len == 0 {
            return Err(MapError::Empty);
        }
        let page = page_size()?;
        let residue = (base % page as u64) as usize;
        let overflow = || MapError::Overflow { base, len };
        base.checked_add(len as u64).ok_or_else(overflow)?;
        let mapping_len = residue.checked_add(len).ok_or_else(overflow)?;
        let offset = libc::off64_t::try_from(base - residue as u64).map_err(|_| overflow())?;
        // SAFETY: a fresh mapping (NULL hint, no MAP_FIXED) cannot alias existing Rust memory. `fd` is borrowed
        // and open for the call; the kernel keeps its own file reference for the mapping's lifetime.
        let address = unsafe {
            libc::mmap64(
                std::ptr::null_mut(),
                mapping_len,
                protection,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                offset,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error().into());
        }
        let mapping = NonNull::new(address.cast::<u8>())
            .ok_or_else(|| io::Error::other("mmap returned NULL"))?;
        Ok(Self {
            mapping,
            mapping_len,
            residue,
            len,
            base,
        })
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    pub fn read_u8(&self, offset: usize) -> Result<u8, MemoryError> {
        let address = self.span(offset, 1)?;
        // SAFETY: `span` proved the byte lies inside the live mapping.
        Ok(unsafe { address.read_volatile() })
    }

    pub fn write_u8(&mut self, offset: usize, value: u8) -> Result<(), MemoryError> {
        let address = self.span(offset, 1)?;
        // SAFETY: `span` proved the byte lies inside the live, writable mapping; `&mut self` excludes other users.
        unsafe { address.write_volatile(value) };
        Ok(())
    }

    /// One aligned 16-bit load (`ldrh`). `Misaligned` here means not 2-byte aligned.
    pub fn read_u16(&self, offset: usize) -> Result<u16, MemoryError> {
        let address = self.aligned::<u16>(offset)?;
        // SAFETY: `aligned` proved an aligned, in-bounds halfword of the live mapping.
        Ok(unsafe { address.read_volatile() })
    }

    /// One aligned 16-bit store (`strh`); stock quiesce uses it for TIMER0 `TMR6/7_CFG` (VA 0x8a57b0).
    pub fn write_u16(&mut self, offset: usize, value: u16) -> Result<(), MemoryError> {
        let address = self.aligned::<u16>(offset)?;
        // SAFETY: `aligned` proved an aligned, in-bounds halfword of the live, writable mapping; `&mut self` is
        // exclusive.
        unsafe { address.write_volatile(value) };
        Ok(())
    }

    /// Full-system data memory barrier (`dmb sy`, ARMv7), the only barrier variant in stock ZenUI. It orders
    /// accesses; it does not clean or invalidate caches. Host builds use a sequentially consistent fence.
    pub fn barrier() {
        #[cfg(target_arch = "arm")]
        // SAFETY: `dmb sy` has no operands and changes no register or Rust-visible state. Without `nomem` the
        // asm block is also a compiler barrier for all memory accesses.
        unsafe {
            std::arch::asm!("dmb sy", options(nostack, preserves_flags));
        }
        #[cfg(not(target_arch = "arm"))]
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }

    /// Pointer to `offset` after a checked bounds test for `len` bytes.
    fn span(&self, offset: usize, len: usize) -> Result<*mut u8, MemoryError> {
        match offset.checked_add(len) {
            Some(end) if end <= self.len => {
                // SAFETY: `residue + offset + len <= mapping_len`, so the result stays inside the mapping.
                Ok(unsafe { self.mapping.as_ptr().add(self.residue + offset) })
            }
            _ => Err(MemoryError::OutOfBounds {
                offset,
                len,
                window: self.len,
            }),
        }
    }

    /// Pointer to an aligned `T`. Alignment is judged on the physical address, not only on `offset`.
    fn aligned<T>(&self, offset: usize) -> Result<*mut T, MemoryError> {
        let address = self.span(offset, size_of::<T>())?;
        if !(self.residue + offset).is_multiple_of(size_of::<T>()) {
            return Err(MemoryError::Misaligned(offset));
        }
        Ok(address.cast())
    }

    /// Splits `offset..offset + len` into unaligned head bytes, whole aligned words and tail bytes.
    fn pieces(&self, offset: usize, len: usize) -> Result<Pieces, MemoryError> {
        self.span(offset, len)?;
        let head = ((WORD - (self.residue + offset) % WORD) % WORD).min(len);
        let words = (len - head) / WORD;
        Ok(Pieces { head, words })
    }
}

struct Pieces {
    head: usize,
    words: usize,
}

/// A `PROT_READ` window ([`PhysicalWindow::open_read_only`]). Every write method of [`PhysicalWindow`]
/// takes `&mut self`, and this type gives out only `&PhysicalWindow`, so it can never write.
#[derive(Debug)]
pub struct ReadOnlyWindow(PhysicalWindow);

impl std::ops::Deref for ReadOnlyWindow {
    type Target = PhysicalWindow;

    fn deref(&self) -> &PhysicalWindow {
        &self.0
    }
}

impl SharedMemory for PhysicalWindow {
    fn len(&self) -> usize {
        self.len
    }

    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<(), MemoryError> {
        let Pieces { head, words } = self.pieces(offset, buf.len())?;
        let (head_bytes, rest) = buf.split_at_mut(head);
        let (word_bytes, tail_bytes) = rest.split_at_mut(words * WORD);
        for (i, byte) in head_bytes.iter_mut().enumerate() {
            *byte = self.read_u8(offset + i)?;
        }
        for (i, chunk) in word_bytes.chunks_exact_mut(WORD).enumerate() {
            chunk.copy_from_slice(&self.read_u32(offset + head + i * WORD)?.to_ne_bytes());
        }
        let tail = offset + head + words * WORD;
        for (i, byte) in tail_bytes.iter_mut().enumerate() {
            *byte = self.read_u8(tail + i)?;
        }
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let Pieces { head, words } = self.pieces(offset, data.len())?;
        let (head_bytes, rest) = data.split_at(head);
        let (word_bytes, tail_bytes) = rest.split_at(words * WORD);
        for (i, &byte) in head_bytes.iter().enumerate() {
            self.write_u8(offset + i, byte)?;
        }
        for (i, chunk) in word_bytes.chunks_exact(WORD).enumerate() {
            let value = u32::from_ne_bytes(chunk.try_into().expect("exact word chunk"));
            self.write_u32(offset + head + i * WORD, value)?;
        }
        let tail = offset + head + words * WORD;
        for (i, &byte) in tail_bytes.iter().enumerate() {
            self.write_u8(tail + i, byte)?;
        }
        Ok(())
    }

    fn read_u32(&self, offset: usize) -> Result<u32, MemoryError> {
        let address = self.aligned::<u32>(offset)?;
        // SAFETY: `aligned` proved an aligned, in-bounds word of the live mapping. One volatile load is one `ldr`.
        Ok(unsafe { address.read_volatile() })
    }

    fn write_u32(&mut self, offset: usize, value: u32) -> Result<(), MemoryError> {
        let address = self.aligned::<u32>(offset)?;
        // SAFETY: `aligned` proved an aligned, in-bounds word of the live, writable mapping; `&mut self` is exclusive.
        unsafe { address.write_volatile(value) };
        Ok(())
    }

    fn acquire_barrier(&mut self) {
        Self::barrier();
    }

    fn publish_barrier(&mut self) {
        Self::barrier();
    }
}

impl Drop for PhysicalWindow {
    fn drop(&mut self) {
        // SAFETY: `mapping`/`mapping_len` are exactly what mmap returned and requested; nothing else unmaps it.
        // No reference into the mapping can outlive `self`, because every accessor copies values out.
        unsafe { libc::munmap(self.mapping.as_ptr().cast(), self.mapping_len) };
    }
}

fn page_size() -> io::Result<usize> {
    // SAFETY: sysconf takes no pointer and has no memory-safety preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(size)
        .ok()
        .filter(|size| size.is_power_of_two())
        .ok_or_else(io::Error::last_os_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;

    #[test]
    fn unaligned_base_maps_residue_and_splits_bytes_and_words() {
        let page = page_size().unwrap();
        let path = std::env::temp_dir().join(format!("tetrad-sys-mem-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        let pattern: Vec<u8> = (0..3 * page).map(|i| (i % 251) as u8).collect();
        file.write_all_at(&pattern, 0).unwrap();

        // Physical base page + 5: the mapping starts at `page`, the window 5 bytes later.
        let base = page as u64 + 5;
        let mut window = PhysicalWindow::map(file.as_fd(), base, 64).unwrap();
        assert_eq!((window.base(), window.len()), (base, 64));
        let mut seen = [0; 64];
        window.read(0, &mut seen).unwrap();
        assert_eq!(seen, pattern[page + 5..page + 69]);

        // Word alignment follows the physical address: offset 3 is base + 8, offset 0 is not aligned.
        assert_eq!(window.read_u32(0), Err(MemoryError::Misaligned(0)));
        let word = u32::from_ne_bytes(pattern[page + 8..page + 12].try_into().unwrap());
        assert_eq!(window.read_u32(3), Ok(word));
        assert_eq!(
            window.read_u32(63),
            Err(MemoryError::OutOfBounds {
                offset: 63,
                len: 4,
                window: 64
            })
        );
        assert_eq!(
            window.write(60, &[0; 5]),
            Err(MemoryError::OutOfBounds {
                offset: 60,
                len: 5,
                window: 64
            })
        );

        // Head byte, two words and a tail byte reach the shared file at the physical offsets.
        window
            .write(
                2,
                &[0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9],
            )
            .unwrap();
        window.write_u32(7, 0x1122_3344).unwrap();
        window.publish_barrier();
        drop(window);
        let mut back = [0; 16];
        file.read_exact_at(&mut back, page as u64 + 7).unwrap();
        let mut expected = [
            0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0, 0, 0, 0, 0xa9, 0, 0, 0, 0, 0, 0,
        ];
        expected[5..9].copy_from_slice(&0x1122_3344_u32.to_ne_bytes());
        expected[10..].copy_from_slice(&pattern[page + 17..page + 23]);
        assert_eq!(back, expected);

        assert!(matches!(
            PhysicalWindow::map(file.as_fd(), base, 0),
            Err(MapError::Empty)
        ));
        assert!(matches!(
            PhysicalWindow::map(file.as_fd(), u64::MAX - 2, 8),
            Err(MapError::Overflow { .. })
        ));
    }

    /// The fault watch window (review r11, F13 leftover): `O_RDONLY` and `PROT_READ`. A shared
    /// `PROT_WRITE` map of a read-only descriptor fails with `EACCES`, so mapping a read-only
    /// file proves the protection.
    #[test]
    fn read_only_window_maps_without_write_access() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("tetrad-sys-mem-ro-{}", std::process::id()));
        std::fs::write(&path, [7, 0, 0, 0, 9, 0, 0, 0]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let window = PhysicalWindow::open_read_only(&path, 4, 4);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(window.unwrap().read_u32(0), Ok(9));
    }
}
