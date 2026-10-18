//! Data of the support pages: Device Information (`0x40d27c`) and the DSP Diagnostics rows
//! (`0x3672a8`, update slot `0x364ba0`).

use std::path::Path;

use md5::{Digest, Md5};

use tetrad_dsp_protocol::DspCore;
use tetrad_dsp_protocol::responses::{DiagnosticsBlock, Soc2ArmDiagnostics};
use tetrad_hal::hal::{Board, FirmwareKind, Hal, Query, Reading};
use tetrad_hal::zencoder::FirmwareVersion;
use tetrad_media::factory::DeviceSerial;
use tetrad_services::exec::Files;

/// `0x66c4b8`.
pub const MAC_PATH: &str = "/sys/class/net/wlan0/address";
/// `CVersions` Zenwireless (libzenhal `0x189f8`) runs `md5sum <this> | cut -b-32`.
pub const WIRELESS_FIRMWARE_PATH: &str = "/lib/firmware/mchp/wilc3000_wifi_firmware.bin";
/// `CVersions::ubootVersion` (libzenhal `0x18a18`) runs `cat /dev/mtd0 | grep "^U-Boot "`.
pub const UBOOT_PATH: &str = "/dev/mtd0";
/// Owned bound (review 2 R6, review r26 S6): stock streams the whole partition through
/// `grep`; Tetrad reads only this prefix (the banner is near the start).
pub const UBOOT_READ_MAX: u64 = 4 << 20;
/// `/proc/sys/kernel` names of the `uname` fields `CVersions::linuxKernelVersion` (libzenhal
/// `0x18928`) prints, without `machine`.
const KERNEL_FIELDS: [&str; 4] = ["ostype", "hostname", "osrelease", "version"];
/// I: the `uname` machine of the ARMv7 Cortex-A5 kernel (`setup_processor` prints "armv7" and
/// the endianness letter).
const KERNEL_MACHINE: &str = "armv7l";

/// Device Information values, read once at boot. `None` when the source is missing. The
/// serial keeps its redacted `Debug`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeviceInfo {
    /// The serial file (`0x66b55c` shows its first 9 characters).
    pub serial: Option<DeviceSerial>,
    pub mac: Option<String>,
    /// `"%s %s %s %s %s %s"` of the `uname` fields (`0x18928`).
    pub kernel: Option<String>,
    /// The first `/dev/mtd0` line that starts with "U-Boot " (`0x18a18`).
    pub uboot: Option<String>,
    /// Zenwireless: the lowercase MD5 hex of the WILC3000 firmware (`0x189f8`).
    pub wireless: Option<String>,
    /// `CZenjack` and `CZencoder` firmware, application then bootloader (`Query::Firmware`).
    /// Only the boards that have them (not the mini, not ESS) read them.
    pub zenjack: [Option<FirmwareVersion>; 2],
    pub zencoder: [Option<FirmwareVersion>; 2],
}

impl DeviceInfo {
    /// Reads the file values through the platform files. A missing or unreadable source
    /// leaves its value `None`; nothing is written.
    pub fn read(files: &mut impl Files, serial: Option<DeviceSerial>) -> Self {
        let mut text = |path: &str| {
            let raw = files.read(Path::new(path)).ok().flatten()?;
            Some(String::from_utf8_lossy(&raw).trim().to_owned())
        };
        let mac = text(MAC_PATH).filter(|mac| !mac.is_empty());
        let fields: Option<Vec<String>> = KERNEL_FIELDS
            .iter()
            .map(|field| text(&format!("/proc/sys/kernel/{field}")))
            .collect();
        let kernel = fields.map(|fields| {
            let domain = text("/proc/sys/kernel/domainname").unwrap_or_default();
            format!("{} {KERNEL_MACHINE} {domain}", fields.join(" "))
        });
        // R6, S6: only the first UBOOT_READ_MAX bytes, whatever the partition size.
        let image = (files.read_bounded(Path::new(UBOOT_PATH), UBOOT_READ_MAX)).ok();
        // Busybox `grep` ends a line at NUL too; on the QC the banner follows a NUL.
        let uboot = image.flatten().and_then(|image| {
            image
                .split(|&byte| byte == b'\n' || byte == 0)
                .find(|line| line.starts_with(b"U-Boot "))
                .map(|line| String::from_utf8_lossy(line).trim().to_owned())
        });
        let wireless = (files.read(Path::new(WIRELESS_FIRMWARE_PATH)).ok().flatten())
            .map(|image| format!("{:x}", Md5::digest(&image)));
        Self {
            serial,
            mac,
            kernel,
            uboot,
            wireless,
            ..Self::default()
        }
    }

    /// The Zenjack and Zencoder firmware rows (classic QC only).
    pub fn read_boards(&mut self, hal: &mut impl Hal) {
        let mut firmware = |board| {
            [FirmwareKind::Application, FirmwareKind::Bootloader].map(|kind| {
                match hal.read(Query::Firmware(board, kind)) {
                    Ok(Reading::Firmware(version)) => version,
                    _ => None,
                }
            })
        };
        self.zenjack = firmware(Board::Zenjack);
        self.zencoder = firmware(Board::Zencoder);
    }
}

/// One core row of DSP Diagnostics, as `0x364ba0` reads the block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoreDiagnostics {
    /// "Drp Msg:": word 0.
    pub dropped_messages: u32,
    /// "FP exc:": word 3.
    pub fp_exceptions: u32,
    /// "CPU Av/Pk(%):": `f32` words 5, 9 and 11 in print order (the separators are not
    /// recovered).
    pub cpu: [f32; 3],
    /// "EMDMA (%):": `f32` word 10.
    pub emdma: f32,
    /// "Heap(%):" DM, PM, SD: words 6, 8, 7 as `u32` over 153,592, 114,680 and 2^24, × 100.
    pub heap: [f32; 3],
}

impl From<&DiagnosticsBlock> for CoreDiagnostics {
    fn from(block: &DiagnosticsBlock) -> Self {
        let words = &block.words;
        let float = |index: usize| f32::from_bits(words[index]);
        Self {
            dropped_messages: words[0],
            fp_exceptions: words[3],
            cpu: [float(5), float(9), float(11)],
            emdma: float(10),
            heap: [
                words[6] as f32 * 100.0 / 153_592.0,
                words[8] as f32 * 100.0 / 114_680.0,
                words[7] as f32 * 100.0 * 5.960_464_5e-8,
            ],
        }
    }
}

/// The last diagnostics the DSP sent: one row per core and the last SOC2 ARM block (USB
/// Statistics). The DSP page's "SOC2 ARM" column has no writer in stock: `0x364ba0` takes a
/// column per `0x00` block and those come from cores 0..3 only, so it keeps its "-1".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DspDiagnostics {
    pub cores: [Option<CoreDiagnostics>; 4],
    pub soc2: Option<Soc2ArmDiagnostics>,
}

/// One USB Statistics row: the label of `0x689de8`, the value text of `0x68a90c` (stock
/// appends a `\n`; left out here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbStatistic {
    pub label: &'static str,
    pub value: String,
    pub tone: UsbTone,
}

/// The colour `0x68a90c` gives a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbTone {
    /// The item keeps its colour.
    Plain,
    /// `Qt::white`: a read/write distance in its range.
    InRange,
    /// `Qt::red`: a read/write distance out of its range.
    OutOfRange,
    /// `Qt::green` (the +1 sample count).
    Plus,
    /// `Qt::blue` (the −1 sample count).
    Minus,
}

impl DspDiagnostics {
    pub fn record(&mut self, core: DspCore, block: &DiagnosticsBlock) {
        self.cores[core as usize] = Some(block.into());
    }

    /// The 21 USB Statistics rows from the last SOC2 ARM block (`0x68a90c`, offsets as the
    /// Cortex Control serializer `0x8e4440` reads them). `hid_sent` is the Cortex Control HID
    /// report count (HID object `+0x3f8`, counted by `0x6dfbf8` for each sent report that is
    /// not a reset marker). `None` before the first block: stock shows "---".
    pub fn usb_statistics(&self, hid_sent: u32) -> Option<[UsbStatistic; 21]> {
        let record = &self.soc2.as_ref()?.0;
        // `%d` prints the word as signed.
        let word = |at: usize| i32::from_le_bytes(record[at..at + 4].try_into().expect("4 bytes"));
        let long = |at: usize| u64::from_le_bytes(record[at..at + 8].try_into().expect("8 bytes"));
        // `(uint)(value - low) < 0x101`: white inside `low..=low + 0x100`, else red.
        let distance = |at: usize, low: u32| {
            let value = word(at);
            let tone = if (value as u32).wrapping_sub(low) < 0x101 {
                UsbTone::InRange
            } else {
                UsbTone::OutOfRange
            };
            (value.to_string(), tone)
        };
        let row = |label, (value, tone): (String, UsbTone)| UsbStatistic { label, value, tone };
        let plain = |label, value: String| row(label, (value, UsbTone::Plain));
        Some([
            plain(
                "USBConnected",
                if record[0] != 0 { "Yes" } else { "No" }.into(),
            ),
            plain("IN rd/wr idx", format!("{}/{}", word(0x04), word(0x08))),
            plain("OUT rd/wr idx", format!("{}/{}", word(0x0c), word(0x10))),
            plain(
                "UI to DSP HID count (tx/rx)",
                format!("{}/{}", hid_sent as i32, word(0x78)),
            ),
            row("USBAudioInRdWrDistanceAverage", distance(0x34, 0x100)),
            row("USBAudioOutRdWrDistanceAverage", distance(0x30, 0x228)),
            plain("USBAudioRxFrame", long(0x18).to_string()),
            plain("USBAudioTxFrame", long(0x20).to_string()),
            plain("USBAudioTxFrameSkipped", word(0x28).to_string()),
            plain("USBAudioTxFrameAborted", word(0x2c).to_string()),
            plain("USBAudioOUTGapsDetected", word(0x38).to_string()),
            plain("USBAudioOUTGapResets", word(0x3c).to_string()),
            plain("USBAudioOUTBufferResets", record[0x48].to_string()),
            row(
                "USBAudioPlusOneSampleCount",
                (word(0x40).to_string(), UsbTone::Plus),
            ),
            row(
                "USBAudioMinusOneSampleCount",
                (word(0x44).to_string(), UsbTone::Minus),
            ),
            plain("USBAudioStreamingEPEnabled", word(0x4c).to_string()),
            plain("USBAudioStreamingEPDisabled", word(0x50).to_string()),
            plain("USBMidiInCount", long(0x60).to_string()),
            plain("USBMidiOutCount", long(0x58).to_string()),
            plain("HID driver sent report count", long(0x68).to_string()),
            plain("HID driver received report count", long(0x70).to_string()),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_services::fake::MemFiles;

    /// Review 2 R6, review r26 S6: start-up reads only a [`UBOOT_READ_MAX`] prefix of
    /// `/dev/mtd0`, with or without `/proc/mtd`: a large partition still gives the banner, and
    /// a banner past the prefix is not seen (a whole read would find it), and a banner after a
    /// NUL (the QC's layout) is found. Invented images.
    #[test]
    fn uboot_line_comes_from_a_bounded_mtd0_prefix() {
        let uboot = |head: &[u8], tail: &[u8]| {
            let mut image = head.to_vec();
            image.resize(usize::try_from(UBOOT_READ_MAX).unwrap() + 2, 0xff);
            image.extend_from_slice(tail);
            let mut files = MemFiles::default();
            files.put(UBOOT_PATH, image);
            DeviceInfo::read(&mut files, None).uboot
        };
        let banner = b"\x00junk\nU-Boot 2099.01 (test)\n";
        assert_eq!(uboot(banner, b"").as_deref(), Some("U-Boot 2099.01 (test)"));
        assert_eq!(uboot(b"", b"\nU-Boot 2099.01 (late)\n"), None);
        assert_eq!(
            uboot(b"\0\0U-Boot 2015.01 (QC)\n", b"").as_deref(),
            Some("U-Boot 2015.01 (QC)")
        );
    }
}
