//! MIDI DIN UART (`/dev/ttySC1`) with the stock libzenhal termios setup (`CMIDI::Open`, VA 0x1d644).
//!
//! Stock requests B38400. `S80drvinit` (`setserial /dev/ttySC1 divisor 225 spd_cust`) makes the driver use
//! divisor 225 for that rate: SCLK0 112.5 MHz / (16 × 225) = 31250 baud. [`set_custom_divisor`] performs the
//! same `TIOCGSERIAL`/`TIOCSSERIAL` step for a boot without that script, and leaves the termios alone.
//!
//! Reads follow the stock `VMIN 0`, `VTIME 1`: [`MidiUart::read_available`] returns the bytes available, or
//! `None` after 100 ms of silence. Bytes stay raw; parse them with `tetrad_hal::midi::MidiParser`.
//!
//! Stock gate: ZenUI calls `CMIDI::Open(0x0049c14c())`. With `false`, libzenhal never opens `/dev/ttySC1` and
//! starts no RX thread (`_useLinuxUart = 0`; the mini routes MIDI through the DSP). The gate is
//! `tetrad_hal::platform::Platform::uses_midi_uart`; `crate::hal::LinuxHal::open` applies it. Open the UART
//! directly only where that gate passes.
//!
//! The termios edit is `tetrad_hal::uart::MIDI_UART`; the const asserts below tie its values to the libc names.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use tetrad_hal::uart::MIDI_UART;

/// `S80drvinit:127`: 112.5 MHz / (16 × 31250).
pub const STOCK_CUSTOM_DIVISOR: NonZeroU32 = NonZeroU32::new(225).unwrap();

/// Stock `c_cflag & 0x7ffffcbf | 0x8b0` (VA 0x1d644).
const CFLAG_CLEAR: libc::tcflag_t = MIDI_UART.cflag_clear;
const CFLAG_SET: libc::tcflag_t = MIDI_UART.cflag_set;
/// Stock `c_iflag & 0xffffe2be`.
const IFLAG_CLEAR: libc::tcflag_t = MIDI_UART.iflag_clear;
/// Owned addition: input bits that would alter or drop MIDI bytes. Stock leaves them at the port default,
/// which is clear after boot, so the result matches stock on a fresh port.
const IFLAG_CLEAR_OWNED: libc::tcflag_t =
    libc::ISTRIP | libc::IGNCR | libc::INPCK | libc::PARMRK | libc::IUCLC;
const STOCK_VMIN: libc::cc_t = MIDI_UART.vmin;
/// Tenths of a second.
const STOCK_VTIME: libc::cc_t = MIDI_UART.vtime;

// tetrad-hal states the stock edit as numbers; these are the same bits by name (asm-generic termbits, identical on
// ARM and the x86 host).
const _: () = {
    assert!(CFLAG_CLEAR == libc::CRTSCTS | libc::PARODD | libc::PARENB | libc::CSTOPB);
    assert!(CFLAG_SET == libc::CLOCAL | libc::CREAD | libc::CS8);
    assert!(
        IFLAG_CLEAR
            == libc::IGNBRK | libc::INLCR | libc::ICRNL | libc::IXON | libc::IXANY | libc::IXOFF
    );
    assert!(MIDI_UART.speed == libc::B38400 && MIDI_UART.oflag == 0 && MIDI_UART.lflag == 0);
};

/// `linux/tty_flags.h`: `ASYNC_SPD_HI | ASYNC_SPD_VHI | ASYNC_SPD_SHI`, and `ASYNC_SPD_CUST`.
const ASYNC_SPD_MASK: libc::c_int = 0x1030;
const ASYNC_SPD_CUST: libc::c_int = 0x0030;

// Device ABI (ADI sysroot asm-generic/termbits.h, glibc 2.18 bits/termios.h, linux/serial.h).
#[cfg(target_arch = "arm")]
const _: () = {
    assert!(CFLAG_CLEAR == !0x7fff_fcbf && CFLAG_SET == 0x8b0);
    assert!(IFLAG_CLEAR == !0xffff_e2be);
    assert!(libc::B38400 == 0o17 && libc::TCSANOW == 0);
    assert!(libc::VMIN == 6 && libc::VTIME == 5);
    assert!(libc::O_RDWR | libc::O_NOCTTY | libc::O_SYNC == 0x10_1102);
    assert!(size_of::<libc::termios>() == 60);
    assert!(std::mem::offset_of!(libc::termios, c_cc) == 17);
    assert!(size_of::<SerialStruct>() == 60);
    assert!(std::mem::offset_of!(SerialStruct, flags) == 16);
    assert!(std::mem::offset_of!(SerialStruct, custom_divisor) == 24);
    assert!(libc::TIOCGSERIAL == 0x541e && libc::TIOCSSERIAL == 0x541f);
};

/// `struct serial_struct` from `linux/serial.h`; native pointer and `unsigned long` widths give the ARM layout
/// (60 bytes) on the device.
#[repr(C)]
struct SerialStruct {
    kind: libc::c_int,
    line: libc::c_int,
    port: libc::c_uint,
    irq: libc::c_int,
    flags: libc::c_int,
    xmit_fifo_size: libc::c_int,
    custom_divisor: libc::c_int,
    baud_base: libc::c_int,
    close_delay: libc::c_ushort,
    io_type: libc::c_char,
    reserved_char: [libc::c_char; 1],
    hub6: libc::c_int,
    closing_wait: libc::c_ushort,
    closing_wait2: libc::c_ushort,
    iomem_base: *mut libc::c_uchar,
    iomem_reg_shift: libc::c_ushort,
    port_high: libc::c_uint,
    iomap_base: libc::c_ulong,
}

/// One configured MIDI UART descriptor. Blocking, like stock; `VTIME` bounds each read.
#[derive(Debug)]
pub struct MidiUart {
    file: File,
}

impl MidiUart {
    /// Opens with the stock `O_RDWR | O_NOCTTY | O_SYNC` (`0x101102`) plus `O_CLOEXEC`, then applies the stock
    /// termios. Nothing is flushed, like stock.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_SYNC)
            .open(path)?;
        Self::configure(file)
    }

    /// Adopts a caller tty descriptor and applies the stock termios.
    pub fn from_fd(fd: OwnedFd) -> io::Result<Self> {
        Self::configure(File::from(fd))
    }

    fn configure(file: File) -> io::Result<Self> {
        let fd = file.as_raw_fd();
        let mut termios = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `termios` is a live, writable termios; tcgetattr fully initializes it on success.
        if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: tcgetattr returned 0, so every field is initialized.
        let mut termios = unsafe { termios.assume_init() };
        // SAFETY: both calls only write the speed fields of the exclusively borrowed struct.
        unsafe {
            libc::cfsetospeed(&mut termios, libc::B38400);
            libc::cfsetispeed(&mut termios, libc::B38400);
        }
        termios.c_cflag = (termios.c_cflag & !CFLAG_CLEAR) | CFLAG_SET;
        termios.c_iflag &= !(IFLAG_CLEAR | IFLAG_CLEAR_OWNED);
        termios.c_oflag = MIDI_UART.oflag;
        termios.c_lflag = MIDI_UART.lflag;
        termios.c_cc[libc::VMIN] = STOCK_VMIN;
        termios.c_cc[libc::VTIME] = STOCK_VTIME;
        // SAFETY: `termios` is initialized and only read by tcsetattr; `file` keeps the descriptor open.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { file })
    }

    /// One `read`: the bytes already queued, or `None` after the 100 ms `VTIME` timeout with none (and for an
    /// empty `buf`). Not `std::io::Read`: there a 0 means end of file, and std adapters would stop on an idle
    /// line. Stock reads one byte per call (`_Rx`, VA 0x1e330) and is called again on 0.
    pub fn read_available(&mut self, buf: &mut [u8]) -> io::Result<Option<NonZeroUsize>> {
        Ok(NonZeroUsize::new(self.file.read(buf)?))
    }
}

/// `setserial <tty> divisor <n> spd_cust`: the driver then runs at `baud_base / divisor` (31250 on the QC with
/// [`STOCK_CUSTOM_DIVISOR`]). Opens non-blocking and changes only the divisor, so the port's termios is kept.
/// Needs `CAP_SYS_ADMIN` to change the divisor.
pub fn set_custom_divisor(path: &Path, divisor: NonZeroU32) -> io::Result<()> {
    let divisor = libc::c_int::try_from(divisor.get())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "divisor exceeds c_int"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)?;
    let fd = file.as_raw_fd();
    let mut serial = MaybeUninit::<SerialStruct>::zeroed();
    // SAFETY: TIOCGSERIAL writes one `struct serial_struct`; `serial` is a live buffer of exactly that
    // layout. An all-zero SerialStruct (null pointer included) is a valid value.
    if unsafe { libc::ioctl(fd, libc::TIOCGSERIAL, serial.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: zero-initialized, then filled by the kernel; every bit pattern is valid for these fields.
    let mut serial = unsafe { serial.assume_init() };
    serial.flags = (serial.flags & !ASYNC_SPD_MASK) | ASYNC_SPD_CUST;
    serial.custom_divisor = divisor;
    // SAFETY: TIOCSSERIAL only reads one `struct serial_struct` from the live, initialized `serial`.
    if unsafe { libc::ioctl(fd, libc::TIOCSSERIAL, &serial) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Stock `MessageSyncPost` (VA 0x1db60) writes one whole 1-, 2- or 3-byte message per `write`; use
/// `write_all` with one message to keep that shape.
impl Write for MidiUart {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsFd for MidiUart {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn stock_termios_is_applied_and_bytes_pass_raw_with_the_vtime_timeout() {
        let (master, slave) = crate::testing::pty_pair();
        let mut uart = MidiUart::from_fd(slave.fd).unwrap();

        let mut termios = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fully initializes the live termios on success, asserted before use.
        let termios = unsafe {
            assert_eq!(
                libc::tcgetattr(uart.as_fd().as_raw_fd(), termios.as_mut_ptr()),
                0
            );
            termios.assume_init()
        };
        assert_eq!(termios.c_cflag & (CFLAG_SET | CFLAG_CLEAR), CFLAG_SET);
        assert_eq!(termios.c_iflag & (IFLAG_CLEAR | IFLAG_CLEAR_OWNED), 0);
        assert_eq!((termios.c_oflag, termios.c_lflag), (0, 0));
        assert_eq!(
            (termios.c_cc[libc::VMIN], termios.c_cc[libc::VTIME]),
            (STOCK_VMIN, STOCK_VTIME)
        );
        // SAFETY: both only read the speed fields of an initialized termios.
        let speeds = unsafe { (libc::cfgetispeed(&termios), libc::cfgetospeed(&termios)) };
        assert_eq!(speeds, (libc::B38400, libc::B38400));

        // Note-on and a CR byte arrive unchanged: no ICRNL, ISTRIP or canonical processing.
        let mut master = File::from(master);
        master.write_all(&[0x90, 0x0d, 0x7f]).unwrap();
        let mut buf = [0; 8];
        let mut got = Vec::new();
        while got.len() < 3 {
            let n = uart
                .read_available(&mut buf)
                .unwrap()
                .map_or(0, NonZeroUsize::get);
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, [0x90, 0x0d, 0x7f]);

        let started = Instant::now();
        assert_eq!(uart.read_available(&mut buf).unwrap(), None);
        assert!(started.elapsed() >= Duration::from_millis(50));

        uart.write_all(&[0xf8]).unwrap();
        let mut echoed = [0; 1];
        master.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, [0xf8]);
    }

    #[test]
    fn custom_divisor_leaves_termios_alone() {
        let (_master, slave) = crate::testing::pty_pair();
        let uart = MidiUart::from_fd(slave.fd).unwrap();
        let path =
            std::fs::read_link(format!("/proc/self/fd/{}", uart.as_fd().as_raw_fd())).unwrap();
        let before = line_settings(uart.as_fd());
        // A pty has no serial_struct, so the driver rejects the ioctl. The port must be unchanged either way.
        assert!(set_custom_divisor(&path, STOCK_CUSTOM_DIVISOR).is_err());
        assert_eq!(line_settings(uart.as_fd()), before);
    }

    /// The termios fields a divisor change must not touch, including the speeds termios reports.
    fn line_settings(
        fd: BorrowedFd<'_>,
    ) -> (
        libc::tcflag_t,
        libc::tcflag_t,
        libc::tcflag_t,
        libc::tcflag_t,
        [libc::cc_t; libc::NCCS],
        libc::speed_t,
        libc::speed_t,
    ) {
        let mut termios = MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr fully initializes the live termios on success, asserted before use.
        let termios = unsafe {
            assert_eq!(libc::tcgetattr(fd.as_raw_fd(), termios.as_mut_ptr()), 0);
            termios.assume_init()
        };
        // SAFETY: both only read the speed fields of an initialized termios.
        let (ispeed, ospeed) =
            unsafe { (libc::cfgetispeed(&termios), libc::cfgetospeed(&termios)) };
        (
            termios.c_iflag,
            termios.c_oflag,
            termios.c_cflag,
            termios.c_lflag,
            termios.c_cc,
            ispeed,
            ospeed,
        )
    }
}
