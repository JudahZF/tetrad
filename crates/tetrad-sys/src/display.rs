//! Custom display control and explicit RGB8-to-fbdev write plans.
//!
//! `/dev/zendisplay/fd` is NOT the pixel sink. Stock `zendisplay_test` uses `/dev/fb0`. No open,
//! ioctl or write happens implicitly.

use std::fs::{File, OpenOptions};
use std::io;
use std::ops::Range;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::ptr::NonNull;

const ENABLE: libc::c_ulong = 0xe203;
const DISABLE: libc::c_ulong = 0xe204;
const RESET: libc::c_ulong = 0xe205;
const SET_BRIGHTNESS: libc::c_ulong = 0x4004_e206;
const GET_VARIABLE_INFO: libc::c_ulong = 0x4600;
const GET_FIXED_INFO: libc::c_ulong = 0x4602;

/// Stock accepts an unsigned comparison of the integer against 100 (VA 0x193c8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brightness(u8);

impl Brightness {
    pub fn new(percent: u8) -> Option<Self> {
        (percent <= 100).then_some(Self(percent))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCommand {
    Enable,
    Disable,
    Reset,
    SetBrightness(Brightness),
}

/// Owns only the custom control descriptor, not a pixel framebuffer or touch node.
#[derive(Debug)]
pub struct DisplayControl {
    file: File,
}

impl DisplayControl {
    /// Explicit opt-in. Supply the custom control node, not `/dev/fb0`.
    /// Stock VA 0x18a38 opens read-only with O_NONBLOCK; std also adds O_CLOEXEC.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self { file })
    }

    /// Adopts a descriptor. No ioctl is sent until `apply`; the caller selects its ABI.
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self {
            file: File::from(fd),
        }
    }

    /// Explicit hardware operation. Unlike stock, reports ioctl failures.
    pub fn apply(&self, command: ControlCommand) -> io::Result<()> {
        let result = match command {
            ControlCommand::SetBrightness(Brightness(percent)) => {
                let value = i32::from(percent);
                // SAFETY: The recovered request consumes one 32-bit integer by pointer (VA 0x193c8).
                // `value` is initialized and lives through the call; the owned descriptor stays open.
                unsafe { libc::ioctl(self.file.as_raw_fd(), SET_BRIGHTNESS, &value) }
            }
            command => {
                let request = match command {
                    ControlCommand::Enable => ENABLE,   // VA 0x19350.
                    ControlCommand::Disable => DISABLE, // VA 0x19378.
                    ControlCommand::Reset => RESET,     // VA 0x193a0.
                    ControlCommand::SetBrightness(_) => unreachable!(),
                };
                // SAFETY: These recovered requests take a zero scalar, not a pointer. The descriptor
                // is owned and stays open for the call. No framebuffer request is sent to this node.
                unsafe { libc::ioctl(self.file.as_raw_fd(), request, 0 as libc::c_ulong) }
            }
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Linux fb_bitfield; offsets count from the least significant bit of the pixel word.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Channel {
    pub offset: u32,
    pub length: u32,
    pub msb_right: u32,
}

/// Owned conversion: little-endian 16/24/32-bit words, RGB fields of 1..=8 bits.
/// Alpha, if present, is opaque; otherwise unused bits are zero. No byte-order guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelFormat {
    pub bits_per_pixel: u32,
    pub red: Channel,
    pub green: Channel,
    pub blue: Channel,
    pub alpha: Channel,
}

impl PixelFormat {
    fn validate(self) -> io::Result<usize> {
        if !matches!(self.bits_per_pixel, 16 | 24 | 32) {
            return Err(invalid("only 16, 24 and 32 bits per pixel are supported"));
        }
        let mut used = 0;
        for (index, channel) in [self.red, self.green, self.blue, self.alpha]
            .into_iter()
            .enumerate()
        {
            if channel.msb_right != 0
                || channel.length > 8
                || (index != 3 && channel.length == 0)
                || channel.offset > self.bits_per_pixel
                || channel.length > self.bits_per_pixel - channel.offset
            {
                return Err(invalid("unsupported channel field"));
            }
            if channel.length != 0 {
                let mask = ((1_u32 << channel.length) - 1) << channel.offset;
                if used & mask != 0 {
                    return Err(invalid("pixel channel fields overlap"));
                }
                used |= mask;
            }
        }
        Ok((self.bits_per_pixel / 8) as usize)
    }

    /// 16-bit R5 G6 B5 with no alpha (the QC panel).
    fn is_rgb565(self) -> bool {
        let field = |offset, length| Channel {
            offset,
            length,
            msb_right: 0,
        };
        self.bits_per_pixel == 16
            && self.red == field(11, 5)
            && self.green == field(5, 6)
            && self.blue == field(0, 5)
            && self.alpha.length == 0
    }

    fn encode(self, rgb: &[u8]) -> [u8; 4] {
        let mut word = 0;
        for (channel, value) in [
            (self.red, rgb[0]),
            (self.green, rgb[1]),
            (self.blue, rgb[2]),
            (self.alpha, 255),
        ] {
            if channel.length != 0 {
                word |= u32::from(value >> (8 - channel.length)) << channel.offset;
            }
        }
        word.to_le_bytes()
    }
}

/// Relevant fbdev metadata. Values must be reported or explicitly supplied, never guessed from QC
/// screen size. `from_arm32_le` decodes offline header-shaped synthetic/evidence bytes without I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramebufferInfo {
    pub width: u32,
    pub height: u32,
    pub virtual_width: u32,
    pub virtual_height: u32,
    pub xoffset: u32,
    pub yoffset: u32,
    pub line_length: u32,
    pub memory_length: u32,
    pub framebuffer_type: u32,
    pub visual: u32,
    pub grayscale: u32,
    pub nonstandard: u32,
    pub vmode: u32,
    pub rotate: u32,
    pub format: PixelFormat,
}

impl FramebufferInfo {
    /// ADI ARM sysroot linux/fb.h: 160-byte fb_var_screeninfo and 68-byte fb_fix_screeninfo.
    /// This is a decoder, not an ioctl interface for a host with a different unsigned-long ABI.
    pub fn from_arm32_le(variable: &[u8; 160], fixed: &[u8; 68]) -> Self {
        let words = std::array::from_fn(|i| {
            u32::from_le_bytes(variable[i * 4..i * 4 + 4].try_into().unwrap())
        });
        let word = |offset| u32::from_le_bytes(fixed[offset..offset + 4].try_into().unwrap());
        Self::from_words(words, word(20), word(24), word(32), word(44))
    }

    fn from_words(
        v: [u32; 40],
        memory_length: u32,
        framebuffer_type: u32,
        visual: u32,
        line_length: u32,
    ) -> Self {
        let channel = |index| Channel {
            offset: v[index],
            length: v[index + 1],
            msb_right: v[index + 2],
        };
        Self {
            width: v[0],
            height: v[1],
            virtual_width: v[2],
            virtual_height: v[3],
            xoffset: v[4],
            yoffset: v[5],
            line_length,
            memory_length,
            framebuffer_type,
            visual,
            grayscale: v[7],
            nonstandard: v[20],
            vmode: v[33],
            rotate: v[34],
            format: PixelFormat {
                bits_per_pixel: v[6],
                red: channel(8),
                green: channel(11),
                blue: channel(14),
                alpha: channel(17),
            },
        }
    }

    /// Rejects planar/palette/directcolor/FourCC, grayscale, nonstandard modes, wrapping and rotation.
    /// No mode switch or alpha/channel fallback is attempted.
    pub fn layout(self) -> io::Result<FramebufferLayout> {
        if self.framebuffer_type != 0
            || self.visual != 2
            || self.grayscale != 0
            || self.nonstandard != 0
            || self.vmode != 0
            || self.rotate != 0
        {
            return Err(invalid(
                "requires unrotated, noninterlaced packed truecolor fbdev",
            ));
        }
        let bytes_per_pixel = self.format.validate()?;
        let width = usize::try_from(self.width).map_err(|_| invalid("width exceeds usize"))?;
        let height = usize::try_from(self.height).map_err(|_| invalid("height exceeds usize"))?;
        let stride = self.line_length as usize;
        let memory_length = self.memory_length as usize;
        if width == 0
            || height == 0
            || self
                .xoffset
                .checked_add(self.width)
                .is_none_or(|x| x > self.virtual_width)
            || self
                .yoffset
                .checked_add(self.height)
                .is_none_or(|y| y > self.virtual_height)
        {
            return Err(invalid("visible dimensions exceed virtual dimensions"));
        }
        let virtual_row = (self.virtual_width as usize)
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| invalid("virtual row size overflow"))?;
        let virtual_size = stride
            .checked_mul(self.virtual_height as usize)
            .ok_or_else(|| invalid("virtual framebuffer size overflow"))?;
        if stride < virtual_row || virtual_size > memory_length {
            return Err(invalid(
                "virtual framebuffer exceeds stride or memory length",
            ));
        }
        let row_bytes = width
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| invalid("visible row size overflow"))?;
        let start = (self.yoffset as usize)
            .checked_mul(stride)
            .and_then(|y| {
                (self.xoffset as usize)
                    .checked_mul(bytes_per_pixel)
                    .and_then(|x| y.checked_add(x))
            })
            .ok_or_else(|| invalid("visible origin overflow"))?;
        let end = (height - 1)
            .checked_mul(stride)
            .and_then(|rows| start.checked_add(rows))
            .and_then(|last| last.checked_add(row_bytes))
            .ok_or_else(|| invalid("visible output size overflow"))?;
        let rgb_bytes = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| invalid("RGB8 source size overflow"))?;
        if end > memory_length {
            return Err(invalid("visible output exceeds framebuffer memory"));
        }
        Ok(FramebufferLayout {
            info: self,
            bytes_per_pixel,
            row_bytes,
            start,
            end,
            rgb_bytes,
        })
    }
}

/// Validated addressing. The write range of row y is `start + y * line_length .. + row_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramebufferLayout {
    info: FramebufferInfo,
    bytes_per_pixel: usize,
    row_bytes: usize,
    start: usize,
    end: usize,
    rgb_bytes: usize,
}

impl FramebufferLayout {
    pub fn info(self) -> FramebufferInfo {
        self.info
    }

    /// Converts a complete packed, top-row-first RGB8 frame; use Frame::width/height/rgb from tetrad-ui.
    /// Every size and address is checked before allocating or writing. No scaling or rotation.
    pub fn plan_rgb8(self, width: u32, height: u32, rgb: &[u8]) -> io::Result<WritePlan> {
        if width != self.info.width || height != self.info.height || rgb.len() != self.rgb_bytes {
            return Err(invalid(
                "RGB8 dimensions or byte count do not match visible framebuffer",
            ));
        }
        let output_len = self
            .row_bytes
            .checked_mul(height as usize)
            .ok_or_else(|| invalid("packed output size overflow"))?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(output_len)
            .map_err(|_| io::Error::other("cannot allocate converted framebuffer"))?;
        for rgb in rgb.chunks_exact(3) {
            pixels.extend_from_slice(&self.info.format.encode(rgb)[..self.bytes_per_pixel]);
        }
        Ok(WritePlan {
            layout: self,
            pixels,
        })
    }
}

/// An immutable, fully converted frame with bounded row writes. Padding and off-screen pixels stay
/// untouched. Construction performs no I/O; applying a plan is always a separate, explicit step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritePlan {
    layout: FramebufferLayout,
    pixels: Vec<u8>,
}

impl WritePlan {
    /// Returns (byte offset, native pixel bytes) for each visible row. Offsets are relative to fbdev
    /// file offset zero, not the physical smem_start address or an mmap page boundary.
    pub fn rows(&self) -> impl ExactSizeIterator<Item = (usize, &[u8])> {
        self.pixels
            .chunks_exact(self.layout.row_bytes)
            .enumerate()
            .map(|(y, row)| {
                (
                    self.layout.start + y * self.layout.info.line_length as usize,
                    row,
                )
            })
    }

    pub fn apply_to_slice(&self, backing: &mut [u8]) -> io::Result<()> {
        if backing.len() < self.layout.end {
            return Err(invalid(
                "byte backing is shorter than the complete write plan",
            ));
        }
        for (offset, row) in self.rows() {
            backing[offset..offset + row.len()].copy_from_slice(row);
        }
        Ok(())
    }
}

/// Explicit fbdev adapter: the device node is mapped shared and rows are copied into the map; a
/// caller-supplied regular synthetic file gets positioned writes. [`Self::present_rgb8`] encodes and
/// writes only the rows that changed since the last frame it presented. No format mutation,
/// panning, blanking, vsync, enable, reset or brightness operation is performed here.
///
/// Why the map (S1, 2026-10-15): on the QC, 480 row `pwrite`s cost ~215 ms per frame and the
/// panel showed each row land (fbdev `fb_write` copies through I/O accessors). Stock QWS maps
/// the node.
#[derive(Debug)]
pub struct Framebuffer {
    file: File,
    layout: FramebufferLayout,
    map: Option<Mapping>,
    /// RGB8 of the last frame [`Self::present_rgb8`] wrote; empty until then and after a failure.
    shown: Vec<u8>,
    /// One encoded row.
    row: Vec<u8>,
}

/// The whole fbdev memory (`smem_len` bytes from offset 0), mapped read/write and shared.
#[derive(Debug)]
struct Mapping {
    address: NonNull<u8>,
    len: usize,
}

impl Mapping {
    fn new(file: &File, len: usize) -> io::Result<Self> {
        // SAFETY: a fresh shared mapping (NULL hint, no MAP_FIXED) aliases no Rust memory; `file`
        // owns the descriptor for the call and the kernel keeps its own reference afterwards.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let address = NonNull::new(address.cast::<u8>())
            .ok_or_else(|| io::Error::other("mmap returned NULL"))?;
        Ok(Self { address, len })
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: `address..address + len` is the live mapping this value owns; `&mut self`
        // excludes other Rust borrows. Other processes may write the same pages (plain bytes).
        unsafe { std::slice::from_raw_parts_mut(self.address.as_ptr(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: exactly the address and length mmap returned; nothing else unmaps it.
        unsafe { libc::munmap(self.address.as_ptr().cast(), self.len) };
    }
}

// SAFETY: the mapping is owned memory of this process; moving its owner to another thread is fine.
unsafe impl Send for Mapping {}

impl Framebuffer {
    /// Explicit hardware opt-in: opens the caller's pixel node O_RDWR and queries 0x4600/0x4602,
    /// as recovered from zendisplay_test VA 0x8904. Never call this on the custom control node.
    pub fn open(path: &Path) -> io::Result<Self> {
        if !cfg!(target_endian = "little") {
            return Err(invalid(
                "native framebuffer query supports little-endian targets only",
            ));
        }
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut variable = [0_u32; 40];
        let mut fixed = RawFixedInfo::default();
        // SAFETY: linux/fb.h defines the output as 40 u32 words (160 bytes). The initialized array
        // is exclusively borrowed, correctly aligned and lives through ioctl; `file` owns the fd.
        let result =
            unsafe { libc::ioctl(file.as_raw_fd(), GET_VARIABLE_INFO, variable.as_mut_ptr()) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: RawFixedInfo is repr(C) with native c_ulong fields and linux/fb.h field order.
        // It is initialized and exclusively borrowed for the call; `file` owns the fd throughout.
        let result = unsafe { libc::ioctl(file.as_raw_fd(), GET_FIXED_INFO, &mut fixed) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        let layout = FramebufferInfo::from_words(
            variable,
            fixed.memory_length,
            fixed.framebuffer_type,
            fixed.visual,
            fixed.line_length,
        )
        .layout()?;
        let map = Some(Mapping::new(&file, layout.info.memory_length as usize)?);
        Ok(Self {
            file,
            layout,
            map,
            shown: Vec::new(),
            row: Vec::new(),
        })
    }

    /// No ioctl. Accepts only an existing regular synthetic file large enough for the declared
    /// framebuffer memory. Rejects read-only/O_APPEND descriptors; does not create, resize or clear it.
    pub fn from_file(file: File, layout: FramebufferLayout) -> io::Result<Self> {
        check_positioned_write_flags(&file)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() < u64::from(layout.info.memory_length) {
            return Err(invalid(
                "synthetic framebuffer must be a sufficiently large regular file",
            ));
        }
        Ok(Self {
            file,
            layout,
            map: None,
            shown: Vec::new(),
            row: Vec::new(),
        })
    }

    pub fn layout(&self) -> FramebufferLayout {
        self.layout
    }

    /// Explicitly writes every row of a prevalidated plan. Partial I/O failure may leave partial
    /// output, never reports success; std write_all_at handles short writes/EINTR. No atomic
    /// presentation promise. The caller must keep mode, visible offsets, descriptor flags and
    /// backing length stable. Forgets the last [`Self::present_rgb8`] frame.
    pub fn write_plan(&mut self, plan: &WritePlan) -> io::Result<()> {
        if plan.layout != self.layout {
            return Err(invalid(
                "write plan belongs to a different framebuffer layout",
            ));
        }
        check_positioned_write_flags(&self.file)?;
        let metadata = self.file.metadata()?;
        if metadata.is_file() && metadata.len() < u64::from(self.layout.info.memory_length) {
            return Err(invalid("synthetic framebuffer was truncated"));
        }
        self.shown.clear();
        for (offset, row) in plan.rows() {
            write_row(&mut self.map, &self.file, offset, row)?;
        }
        Ok(())
    }

    /// Another program wrote the framebuffer: the next [`Self::present_rgb8`] writes every row.
    pub fn forget_shown(&mut self) {
        self.shown.clear();
    }

    /// The visible rows as the node holds them now, in its own pixel format: what another
    /// program (the boot splash) left on the screen. [`Self::restore`] writes them back.
    pub fn save(&mut self) -> io::Result<Vec<u8>> {
        let layout = self.layout;
        let mut saved = vec![0; layout.row_bytes * layout.info.height as usize];
        for (y, row) in saved.chunks_exact_mut(layout.row_bytes).enumerate() {
            let offset = layout.start + y * layout.info.line_length as usize;
            match &mut self.map {
                Some(map) => row.copy_from_slice(&map.bytes()[offset..offset + row.len()]),
                None => self.file.read_exact_at(row, offset as u64)?,
            }
        }
        Ok(saved)
    }

    /// Writes rows from [`Self::save`] back and forgets the last presented frame.
    pub fn restore(&mut self, saved: &[u8]) -> io::Result<()> {
        let layout = self.layout;
        if saved.len() != layout.row_bytes * layout.info.height as usize {
            return Err(invalid("saved rows do not match the framebuffer layout"));
        }
        self.shown.clear();
        for (y, row) in saved.chunks_exact(layout.row_bytes).enumerate() {
            let offset = layout.start + y * layout.info.line_length as usize;
            write_row(&mut self.map, &self.file, offset, row)?;
        }
        Ok(())
    }

    /// A complete packed RGB8 frame (as [`FramebufferLayout::plan_rgb8`] takes it): only the rows
    /// that differ from the last frame presented here are encoded and written, each from its
    /// first to its last changed pixel, so an unchanged row costs one compare. The first frame,
    /// and the first after a failure or a [`Self::write_plan`], writes every row. Same
    /// validation and failure rules as `write_plan`.
    pub fn present_rgb8(&mut self, width: u32, height: u32, rgb: &[u8]) -> io::Result<()> {
        self.present_rgb8_rows(width, height, rgb, 0..height)
    }

    /// [`Self::present_rgb8`] when the caller knows that only `rows` (top first) can differ from
    /// the last frame presented here: the other rows are not compared, encoded or kept again.
    /// Without a last frame (the first, or after a failure), every row is written.
    pub fn present_rgb8_rows(
        &mut self,
        width: u32,
        height: u32,
        rgb: &[u8],
        rows: Range<u32>,
    ) -> io::Result<()> {
        let layout = self.layout;
        if width != layout.info.width
            || height != layout.info.height
            || rgb.len() != layout.rgb_bytes
        {
            return Err(invalid(
                "RGB8 dimensions or byte count do not match visible framebuffer",
            ));
        }
        check_positioned_write_flags(&self.file)?;
        let metadata = self.file.metadata()?;
        if metadata.is_file() && metadata.len() < u64::from(layout.info.memory_length) {
            return Err(invalid("synthetic framebuffer was truncated"));
        }
        // Taken, so a failed write leaves no last frame and the next present writes every row.
        let mut shown = std::mem::take(&mut self.shown);
        let fresh = shown.len() != rgb.len();
        let rows = if fresh {
            shown.clear();
            shown.resize(rgb.len(), 0);
            0..height as usize
        } else {
            rows.start.min(height) as usize..rows.end.min(height) as usize
        };
        let source_row = width as usize * 3;
        self.row.resize(layout.row_bytes, 0);
        for y in rows {
            let bytes = y * source_row..(y + 1) * source_row;
            let (source, kept) = (&rgb[bytes.clone()], &mut shown[bytes]);
            // Only the pixels from the first to the last that differ are encoded and written: a
            // knob turn changes ~78 of 800 (r53: 115 full rows took 6.8 ms on the QC).
            if !fresh && kept == source {
                continue;
            }
            let span = if fresh {
                0..width as usize
            } else {
                let bytes = || source.iter().zip(kept.iter());
                let first = bytes().position(|(a, b)| a != b).unwrap_or(0);
                let last = bytes().rposition(|(a, b)| a != b).unwrap_or(first);
                first / 3..last / 3 + 1
            };
            let (source, kept) = (
                &source[span.start * 3..span.end * 3],
                &mut kept[span.start * 3..span.end * 3],
            );
            if layout.info.format.is_rgb565() {
                // The QC panel format: one shift-and-mask per pixel, same result as `encode`.
                for (out, p) in self.row.chunks_exact_mut(2).zip(source.chunks_exact(3)) {
                    let word = (u16::from(p[0] >> 3) << 11)
                        | (u16::from(p[1] >> 2) << 5)
                        | u16::from(p[2] >> 3);
                    out.copy_from_slice(&word.to_le_bytes());
                }
            } else {
                let size = layout.bytes_per_pixel;
                for (out, pixel) in self.row.chunks_exact_mut(size).zip(source.chunks_exact(3)) {
                    out.copy_from_slice(&layout.info.format.encode(pixel)[..size]);
                }
            }
            let offset = layout.start
                + y * layout.info.line_length as usize
                + span.start * layout.bytes_per_pixel;
            let row = &self.row[..span.len() * layout.bytes_per_pixel];
            write_row(&mut self.map, &self.file, offset, row)?;
            kept.copy_from_slice(source);
        }
        self.shown = shown;
        Ok(())
    }
}

// ADI ARM sysroot linux/fb.h:154. Native unsigned long differs on the 64-bit host.
#[repr(C)]
#[derive(Default)]
struct RawFixedInfo {
    id: [u8; 16],
    memory_start: libc::c_ulong,
    memory_length: u32,
    framebuffer_type: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_length: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

#[cfg(target_arch = "arm")]
const _: () = {
    assert!(size_of::<libc::c_ulong>() == 4);
    assert!(size_of::<RawFixedInfo>() == 68);
    assert!(std::mem::offset_of!(RawFixedInfo, memory_length) == 20);
    assert!(std::mem::offset_of!(RawFixedInfo, line_length) == 44);
    assert!(libc::O_NONBLOCK == 0x800);
};

/// One encoded row at its byte offset: a copy into the map, or a positioned write. `layout()`
/// proved every visible row lies inside `memory_length`, the mapped length.
fn write_row(map: &mut Option<Mapping>, file: &File, offset: usize, row: &[u8]) -> io::Result<()> {
    match map {
        Some(map) => map.bytes()[offset..offset + row.len()].copy_from_slice(row),
        None => file.write_all_at(row, offset as u64)?,
    }
    Ok(())
}

fn check_positioned_write_flags(file: &File) -> io::Result<()> {
    // SAFETY: F_GETFL takes no pointer; `file` owns its descriptor for this entire call.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // Linux pwrite ignores the supplied offset with O_APPEND, breaking bounded row writes.
    if flags & libc::O_APPEND != 0 || flags & libc::O_ACCMODE == libc::O_RDONLY {
        return Err(invalid(
            "positioned output requires a writable descriptor without O_APPEND",
        ));
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn channel(offset: u32, length: u32) -> Channel {
        Channel {
            offset,
            length,
            msb_right: 0,
        }
    }

    fn info(bits: u32, red: Channel, green: Channel, blue: Channel) -> FramebufferInfo {
        FramebufferInfo {
            width: 2,
            height: 2,
            virtual_width: 4,
            virtual_height: 4,
            xoffset: 1,
            yoffset: 1,
            line_length: 4 * (bits / 8) + 4,
            memory_length: (4 * (bits / 8) + 4) * 4,
            framebuffer_type: 0,
            visual: 2,
            grayscale: 0,
            nonstandard: 0,
            vmode: 0,
            rotate: 0,
            format: PixelFormat {
                bits_per_pixel: bits,
                red,
                green,
                blue,
                alpha: Channel::default(),
            },
        }
    }

    #[test]
    fn recovered_pixel_layouts_encode_exact_little_endian_bytes() {
        let source = [255, 128, 7, 0, 255, 255, 255, 0, 0, 0, 0, 255];
        for (bits, red, green, blue, expected) in [
            (
                16,
                channel(11, 5),
                channel(5, 6),
                channel(0, 5),
                vec![0x00, 0xfc, 0xff, 0x07, 0x00, 0xf8, 0x1f, 0x00],
            ),
            (
                16,
                channel(0, 5),
                channel(5, 6),
                channel(11, 5),
                vec![0x1f, 0x04, 0xe0, 0xff, 0x1f, 0x00, 0x00, 0xf8],
            ),
            (
                24,
                channel(16, 8),
                channel(8, 8),
                channel(0, 8),
                vec![7, 128, 255, 255, 255, 0, 0, 0, 255, 255, 0, 0],
            ),
            (
                24,
                channel(0, 8),
                channel(8, 8),
                channel(16, 8),
                source.to_vec(),
            ),
            (
                32,
                channel(16, 8),
                channel(8, 8),
                channel(0, 8),
                vec![7, 128, 255, 0, 255, 255, 0, 0, 0, 0, 255, 0, 255, 0, 0, 0],
            ),
            (
                32,
                channel(0, 8),
                channel(8, 8),
                channel(16, 8),
                vec![255, 128, 7, 0, 0, 255, 255, 0, 255, 0, 0, 0, 0, 0, 255, 0],
            ),
        ] {
            let layout = info(bits, red, green, blue).layout().unwrap();
            let plan = layout.plan_rgb8(2, 2, &source).unwrap();
            assert_eq!(
                plan.rows()
                    .flat_map(|(_, row)| row.iter().copied())
                    .collect::<Vec<_>>(),
                expected
            );
            let mut backing = vec![0xa5; layout.info.memory_length as usize];
            plan.apply_to_slice(&mut backing).unwrap();
            for (offset, row) in plan.rows() {
                assert_eq!(&backing[offset..offset + row.len()], row);
            }
            for (offset, value) in backing.iter().enumerate() {
                if !plan
                    .rows()
                    .any(|(start, row)| (start..start + row.len()).contains(&offset))
                {
                    assert_eq!(*value, 0xa5, "offscreen/padding byte {offset}");
                }
            }
        }
    }

    #[test]
    fn invalid_metadata_or_source_cannot_produce_or_apply_a_write() {
        let valid = info(32, channel(16, 8), channel(8, 8), channel(0, 8));
        for field in 0..16 {
            let mut bad = valid;
            match field {
                0 => bad.width = 0,
                1 => bad.xoffset = u32::MAX,
                2 => bad.height = u32::MAX,
                3 => bad.line_length = 1,
                4 => bad.memory_length -= 1,
                5 => bad.format.red = channel(8, 8),
                6 => bad.format.red.msb_right = 1,
                7 => bad.format.blue.length = 0,
                8 => bad.format.alpha = channel(31, 2),
                9 => bad.format.bits_per_pixel = 15,
                10 => bad.visual = 4,
                11 => bad.framebuffer_type = 1,
                12 => bad.nonstandard = 1,
                13 => bad.rotate = 1,
                14 => bad.grayscale = 1,
                15 => bad.vmode = 256,
                _ => unreachable!(),
            }
            assert!(bad.layout().is_err(), "invalid metadata case {field}");
        }
        let layout = valid.layout().unwrap();
        assert!(layout.plan_rgb8(1, 2, &[0; 12]).is_err());
        assert!(layout.plan_rgb8(2, 2, &[0; 11]).is_err());
        let plan = layout.plan_rgb8(2, 2, &[0; 12]).unwrap();
        let mut short = vec![0xa5; layout.end - 1];
        assert!(plan.apply_to_slice(&mut short).is_err());
        assert!(short.iter().all(|&byte| byte == 0xa5));
    }

    #[test]
    fn arm_header_offsets_decode_without_host_abi_assumptions() {
        let mut variable = [0_u8; 160];
        let mut fixed = [0_u8; 68];
        for (index, value) in [
            (0, 800_u32),
            (1, 480),
            (2, 800),
            (3, 480),
            (6, 32),
            (8, 16),
            (9, 8),
            (11, 8),
            (12, 8),
            (15, 8),
        ] {
            variable[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [(16, 0x1234_5678_u32), (20, 1_536_000), (32, 2), (44, 3200)] {
            fixed[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        let decoded = FramebufferInfo::from_arm32_le(&variable, &fixed);
        let layout = decoded.layout().unwrap();
        assert_eq!(
            (decoded.width, decoded.height, decoded.line_length),
            (800, 480, 3200)
        );
        assert_eq!((layout.start, layout.end), (0, 1_536_000));
        // Sync polarity is not vmode/rotation; keep all three header offsets distinct.
        variable[128..132].copy_from_slice(&1_u32.to_le_bytes());
        assert!(
            FramebufferInfo::from_arm32_le(&variable, &fixed)
                .layout()
                .is_ok()
        );
        variable[132..136].copy_from_slice(&256_u32.to_le_bytes());
        let wrapped = FramebufferInfo::from_arm32_le(&variable, &fixed);
        assert_eq!(wrapped.vmode, 256);
        assert!(wrapped.layout().is_err());
        variable[132..136].fill(0);
        variable[136..140].copy_from_slice(&1_u32.to_le_bytes());
        let rotated = FramebufferInfo::from_arm32_le(&variable, &fixed);
        assert_eq!(rotated.rotate, 1);
        assert!(rotated.layout().is_err());
        let mut rgba = decoded;
        rgba.format.alpha = channel(24, 8);
        let source = vec![0; 800 * 480 * 3];
        let plan = rgba.layout().unwrap().plan_rgb8(800, 480, &source).unwrap();
        assert_eq!(&plan.rows().next().unwrap().1[..4], &[0, 0, 0, 255]);
        assert!(Brightness::new(0).is_some());
        assert!(Brightness::new(100).is_some());
        assert!(Brightness::new(101).is_none());
    }

    /// The boot selector draws over the splash and gives it back: save, present, restore
    /// leaves every byte (visible rows and gaps) as it was; a wrong-sized save is refused.
    #[test]
    fn restore_gives_back_the_saved_screen() {
        let path = std::env::temp_dir().join(format!("tetrad-display-save-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        let layout = info(16, channel(11, 5), channel(5, 6), channel(0, 5))
            .layout()
            .unwrap();
        let splash: Vec<u8> = (0..layout.info.memory_length).map(|i| i as u8).collect();
        file.write_all_at(&splash, 0).unwrap();
        let observer = file.try_clone().unwrap();
        let mut framebuffer = Framebuffer::from_file(file, layout).unwrap();
        let saved = framebuffer.save().unwrap();
        framebuffer.present_rgb8(2, 2, &[255; 12]).unwrap();
        let mut bytes = vec![0; splash.len()];
        observer.read_exact_at(&mut bytes, 0).unwrap();
        assert_ne!(bytes, splash);
        assert!(framebuffer.restore(&saved[1..]).is_err());
        framebuffer.restore(&saved).unwrap();
        observer.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes, splash);
    }

    #[test]
    fn synthetic_file_writes_preserve_gaps_and_reject_truncation() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "tetrad-display-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let append = OpenOptions::new().append(true).open(&path).unwrap();
        let read_only = File::open(&path).unwrap();
        // Only unlink the synthetic file this test created; the open descriptors retain its backing.
        std::fs::remove_file(path).unwrap();
        let layout = info(16, channel(11, 5), channel(5, 6), channel(0, 5))
            .layout()
            .unwrap();
        let mut expected = vec![0xa5; layout.info.memory_length as usize];
        file.write_all(&expected).unwrap();
        assert!(Framebuffer::from_file(append, layout).is_err());
        assert!(Framebuffer::from_file(read_only, layout).is_err());
        let mut observer = file.try_clone().unwrap();
        let mut framebuffer = Framebuffer::from_file(file, layout).unwrap();
        let plan = layout.plan_rgb8(2, 2, &[255; 12]).unwrap();
        plan.apply_to_slice(&mut expected).unwrap();
        framebuffer.write_plan(&plan).unwrap();
        observer.seek(SeekFrom::Start(0)).unwrap();
        let mut actual = Vec::new();
        observer.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
        // present_rgb8 writes what write_plan wrote; a repeat of the same frame writes nothing
        // (a byte changed behind the adapter's back stays), a changed frame rewrites the row.
        let row0 = plan.rows().next().unwrap().0 as u64;
        // The RGB565 fast path writes the same bytes as the generic encode of `write_plan`.
        let varied: Vec<u8> = (0..12_u8)
            .map(|i| i.wrapping_mul(23).wrapping_add(7))
            .collect();
        framebuffer
            .write_plan(&layout.plan_rgb8(2, 2, &varied).unwrap())
            .unwrap();
        let read_all = |file: &mut File| {
            let mut bytes = vec![0; layout.info.memory_length as usize];
            file.read_exact_at(&mut bytes, 0).unwrap();
            bytes
        };
        let generic = read_all(&mut observer);
        framebuffer.present_rgb8(2, 2, &[0; 12]).unwrap();
        framebuffer.present_rgb8(2, 2, &varied).unwrap();
        assert_eq!(read_all(&mut observer), generic);
        framebuffer.present_rgb8(2, 2, &[255; 12]).unwrap();
        observer.write_all_at(&[0x5a], row0).unwrap();
        framebuffer.present_rgb8(2, 2, &[255; 12]).unwrap();
        let mut byte = [0];
        observer.read_exact_at(&mut byte, row0).unwrap();
        assert_eq!(byte, [0x5a]);
        let mut changed = [255; 12];
        changed[0] = 0;
        framebuffer.present_rgb8(2, 2, &changed).unwrap();
        observer.read_exact_at(&mut byte, row0).unwrap();
        assert_ne!(byte, [0x5a]);
        // Only the changed pixels of a row are written: with pixel 1 changed, a byte changed
        // behind the adapter's back on pixel 0 stays, and pixel 1 reads (0, 255, 255).
        observer.write_all_at(&[0x5a], row0).unwrap();
        changed[3] = 0;
        framebuffer.present_rgb8(2, 2, &changed).unwrap();
        let mut row = [0; 4];
        observer.read_exact_at(&mut row, row0).unwrap();
        assert_eq!((row[0], [row[2], row[3]]), (0x5a, 0x07ff_u16.to_le_bytes()));
        // With a row hint only those rows are compared and written; without a last frame (a
        // failure, `forget_shown`) every row is written, black rows too.
        let before = read_all(&mut observer);
        framebuffer.present_rgb8_rows(2, 2, &[0; 12], 1..2).unwrap();
        let after = read_all(&mut observer);
        let (row0, row1) = (
            row0 as usize,
            row0 as usize + layout.info.line_length as usize,
        );
        assert_eq!(after[row0..row0 + 4], before[row0..row0 + 4]);
        assert_eq!(after[row1..row1 + 4], [0; 4]);
        assert_ne!(before[row1..row1 + 4], [0; 4]);
        framebuffer.forget_shown();
        framebuffer.present_rgb8_rows(2, 2, &[0; 12], 1..2).unwrap();
        assert_eq!(read_all(&mut observer), {
            let mut black = expected.clone();
            layout
                .plan_rgb8(2, 2, &[0; 12])
                .unwrap()
                .apply_to_slice(&mut black)
                .unwrap();
            black
        });
        let mut other = layout.info;
        other.xoffset = 0;
        let wrong = other.layout().unwrap().plan_rgb8(2, 2, &[0; 12]).unwrap();
        assert!(framebuffer.write_plan(&wrong).is_err());
        observer.set_len(1).unwrap();
        assert!(framebuffer.write_plan(&plan).is_err());
        assert_eq!(observer.metadata().unwrap().len(), 1);
        assert!(Framebuffer::from_file(observer, layout).is_err());
    }
}
