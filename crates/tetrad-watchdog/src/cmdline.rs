//! Stock readers of `/proc/cmdline`. Four stock parsers read the product ID in
//! four different ways; each method reproduces one so that Rust decisions match
//! stock decisions byte for byte.

use tetrad_hal::platform::Platform;

/// Hardware product. The kernel command line names the mini `atma`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    QuadCortex,
    QuadCortexMini,
}

impl Product {
    /// Name in the crash text (ZenWatchdog main `0xbc50`, strings `0xf88c`, `0xf8a0`).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::QuadCortex => "Quad Cortex",
            Self::QuadCortexMini => "Quad Cortex mini",
        }
    }
}

/// ZenUI mini test `0x499a60` (`Platform::is_atma`): real ATMA and "ATMA on QC"
/// emulation are both the mini. This is the `Product` for [`crate::power`].
impl From<Platform> for Product {
    fn from(platform: Platform) -> Self {
        if platform.is_atma() {
            Self::QuadCortexMini
        } else {
            Self::QuadCortex
        }
    }
}

/// How `zenreboot` restarts the device (main `0x9580`, branch `0x96f0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartMethod {
    /// `CZenjack::Restart` through the jack HAL; every ID except exact `atma`.
    HalRestart,
    /// `system("/bin/reboot")`; exact `atma` only.
    InitReboot,
}

/// Product classification by `S80drvinit` `get_product_id` (lines 6-16, 62-99).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrvinitProduct {
    QuadCortex,
    Atma,
    /// The script prints `unknown product_id` and continues.
    Unknown(Vec<u8>),
}

/// Root partition digit from `grep -o 'mmcblk0p[0-9]'` (`S80drvinit:147`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootPartition(u8);

impl RootPartition {
    pub fn new(digit: u8) -> Option<Self> {
        (digit <= 9).then_some(Self(digit))
    }

    pub fn digit(self) -> u8 {
        self.0
    }

    /// Device name such as `mmcblk0p1`.
    pub fn device_name(self) -> String {
        format!("mmcblk0p{}", self.0)
    }
}

/// Raw `/proc/cmdline` bytes, including the trailing newline the kernel adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelCmdline<'a>(&'a [u8]);

const PRODUCT_KEY: &[u8] = b"productid=";
const MINI: &[u8] = b"atma";

impl<'a> KernelCmdline<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    /// ZenWatchdog main `0xbc50`: the text up to the first NUL contains
    /// `productid=atma` anywhere (case-sensitive `QString::indexOf`).
    pub fn watchdog_product(self) -> Product {
        let text = self.0.split(|&byte| byte == 0).next().unwrap_or_default();
        if find(text, b"productid=atma").is_some() {
            Product::QuadCortexMini
        } else {
            Product::QuadCortex
        }
    }

    /// libzenhal `CZenCommon::isAtma` (`0x184d8`) through `getProcCmdlineValue`
    /// (`0x17a10`): first line only, tokens split on single spaces, the first
    /// token that starts with `productid=`; exact `atma` is the mini. ZenUI uses
    /// it for restart, power gates and the power button; with the "ATMA on QC"
    /// emulation file present, use `From<Platform>` instead.
    pub fn hal_product(self) -> Product {
        // Lossy decoding never touches ASCII bytes, so the decision is unchanged.
        let text = String::from_utf8_lossy(self.0);
        if tetrad_hal::platform::Product::from_cmdline(&text) == tetrad_hal::platform::Product::Atma
        {
            Product::QuadCortexMini
        } else {
            Product::QuadCortex
        }
    }

    /// zenreboot parser `0x9214`: the first `productid=` value runs to the next
    /// space or to the end of the file. A last token therefore keeps the
    /// trailing newline. Default `QC` (main `0x9580`).
    pub fn zenreboot_product_id(self) -> &'a [u8] {
        let Some(start) = find(self.0, PRODUCT_KEY).map(|at| at + PRODUCT_KEY.len()) else {
            return b"QC";
        };
        let rest = &self.0[start..];
        &rest[..rest.iter().position(|&b| b == b' ').unwrap_or(rest.len())]
    }

    /// zenreboot main `0x9580`: exact `atma` (string `0xa138`) selects
    /// `/bin/reboot`; any other value, including the default, the HAL.
    pub fn zenreboot_restart(self) -> RestartMethod {
        if self.zenreboot_product_id() == MINI {
            RestartMethod::InitReboot
        } else {
            RestartMethod::HalRestart
        }
    }

    /// `grep -oE "productid=([^ ]*)" | cut -d '=' -f 2` per line, then shell
    /// command substitution (trailing newlines removed). Several matches give a
    /// multi-line value that never equals a known product.
    pub fn drvinit_product(self) -> DrvinitProduct {
        let mut value = Vec::new();
        for line in self.0.split(|&b| b == b'\n') {
            let mut rest = line;
            while let Some(at) = find(rest, PRODUCT_KEY) {
                let token = &rest[at + PRODUCT_KEY.len()..];
                let end = token.iter().position(|&b| b == b' ').unwrap_or(token.len());
                // `cut -f 2` keeps only the text up to a further `=`.
                let field = &token[..end];
                value.extend_from_slice(field.split(|&b| b == b'=').next().unwrap_or_default());
                value.push(b'\n');
                rest = &token[end..];
            }
        }
        while value.last() == Some(&b'\n') {
            value.pop();
        }
        match value.as_slice() {
            b"" | b"quadcortex" => DrvinitProduct::QuadCortex,
            MINI => DrvinitProduct::Atma,
            _ => DrvinitProduct::Unknown(value),
        }
    }

    /// First `mmcblk0p<digit>`. Stock joins every match with newlines; the
    /// device command line has one root entry, so the first match is used.
    pub fn root_partition(self) -> Option<RootPartition> {
        const PREFIX: &[u8] = b"mmcblk0p";
        let mut rest = self.0;
        while let Some(at) = find(rest, PREFIX) {
            let after = &rest[at + PREFIX.len()..];
            match after.first() {
                Some(digit @ b'0'..=b'9') => return RootPartition::new(digit - b'0'),
                _ => rest = after,
            }
        }
        None
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_stock_product_parsers_disagree_on_edge_cases() {
        // A last token keeps the kernel's trailing newline in zenreboot only,
        // so the stock mini takes the HAL path instead of `/bin/reboot` there.
        let last = KernelCmdline::new(b"root=/dev/mmcblk0p2 productid=atma\n");
        assert_eq!(last.watchdog_product(), Product::QuadCortexMini);
        assert_eq!(last.hal_product(), Product::QuadCortexMini);
        assert_eq!(last.drvinit_product(), DrvinitProduct::Atma);
        assert_eq!(last.zenreboot_product_id(), b"atma\n");
        assert_eq!(last.zenreboot_restart(), RestartMethod::HalRestart);
        assert_eq!(last.root_partition(), RootPartition::new(2));

        let middle = KernelCmdline::new(b"productid=atma hwid=3 root=/dev/mmcblk0p1\n");
        assert_eq!(middle.zenreboot_restart(), RestartMethod::InitReboot);

        // A longer value is still a mini to the watchdog's substring check.
        let longer = KernelCmdline::new(b"productid=atmax \n");
        assert_eq!(longer.watchdog_product(), Product::QuadCortexMini);
        assert_eq!(
            longer.drvinit_product(),
            DrvinitProduct::Unknown(b"atmax".to_vec())
        );
        assert_eq!(longer.zenreboot_restart(), RestartMethod::HalRestart);

        // libzenhal needs a token prefix and reads the first line only.
        let prefixed = KernelCmdline::new(b"xproductid=atma\n");
        assert_eq!(prefixed.watchdog_product(), Product::QuadCortexMini);
        assert_eq!(prefixed.hal_product(), Product::QuadCortex);
        let second_line = KernelCmdline::new(b"root=x\nproductid=atma\n");
        assert_eq!(second_line.hal_product(), Product::QuadCortex);
        assert_eq!(second_line.drvinit_product(), DrvinitProduct::Atma);

        let absent = KernelCmdline::new(b"console=ttySC0\n");
        assert_eq!(absent.watchdog_product(), Product::QuadCortex);
        assert_eq!(absent.drvinit_product(), DrvinitProduct::QuadCortex);
        assert_eq!(absent.zenreboot_product_id(), b"QC");
        assert_eq!(absent.root_partition(), None);
    }
}
