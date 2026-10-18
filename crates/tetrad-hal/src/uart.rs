//! `ZenHAL::CMIDI` UART settings for `/dev/ttySC1` (`CMIDI::Open` VA 0xd644).
//! Values are the ARM glibc 2.18 `termios` bits. MIDI byte parsing is in [`crate::midi`].

/// The `termios` edit stock applies after `tcgetattr`, then `tcsetattr(TCSANOW)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartConfig {
    /// `cfsetispeed`/`cfsetospeed` code. `B38400` (0o17). The 31250 baud MIDI rate
    /// comes from the UART divisor: confirmed by `S80drvinit:127` (`setserial ... divisor 225 spd_cust`).
    pub speed: u32,
    /// Bits cleared then set in `c_cflag`.
    pub cflag_clear: u32,
    pub cflag_set: u32,
    /// Bits cleared in `c_iflag`.
    pub iflag_clear: u32,
    /// `c_oflag` and `c_lflag` are both set to zero.
    pub oflag: u32,
    pub lflag: u32,
    /// `c_cc[VMIN]` (index 6) and `c_cc[VTIME]` (index 5, tenths of a second).
    pub vmin: u8,
    pub vtime: u8,
}

pub const MIDI_UART: UartConfig = UartConfig {
    speed: 0o17,
    // CSTOPB 0o100 | PARENB 0o400 | PARODD 0o1000 | CRTSCTS 0x80000000.
    cflag_clear: 0x8000_0340,
    // CS8 0o60 | CREAD 0o200 | CLOCAL 0o4000.
    cflag_set: 0x8b0,
    // IGNBRK | INLCR | ICRNL | IXON | IXANY | IXOFF.
    iflag_clear: 0x1d41,
    oflag: 0,
    lflag: 0,
    vmin: 0,
    vtime: 1,
};

/// `ClearStaleMessages` (VA 0xdc20) drops queued RX messages older than one second.
pub const STALE_MESSAGE_AGE: std::time::Duration = std::time::Duration::from_secs(1);

impl UartConfig {
    pub fn apply_cflag(self, cflag: u32) -> u32 {
        (cflag & !self.cflag_clear) | self.cflag_set
    }

    pub fn apply_iflag(self, iflag: u32) -> u32 {
        iflag & !self.iflag_clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_equal_stock_immediates() {
        // VA 0xd6d8..0xd6e8: `bic 0x80000000; bic 0x340; orr 0x8b0` and `bic 0x1d40; bic 1`.
        assert_eq!(!MIDI_UART.cflag_clear, 0x7fff_fcbf);
        assert_eq!(!MIDI_UART.iflag_clear, 0xffff_e2be);
        assert_eq!(MIDI_UART.apply_cflag(0xffff_ffff), 0x7fff_fcbf);
        assert_eq!(MIDI_UART.apply_cflag(0), 0x8b0);
    }
}
