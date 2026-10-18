//! Sysfs GPIO input, as ZenUI reads GPIO 97 before it opens `/dev/uio2` (VA 0x8a7bbc).

use std::fs;
use std::io;
use std::path::PathBuf;

/// Read by `0x8a7bbc` together with the product checks that gate `/dev/uio2`.
pub const UIO2_GATE_GPIO: u32 = 97;

/// One sysfs GPIO under a caller-chosen root (normally [`crate::paths::SYSFS_GPIO`]). Construction touches nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysfsGpio {
    root: PathBuf,
    number: u32,
}

impl SysfsGpio {
    pub fn new(root: impl Into<PathBuf>, number: u32) -> Self {
        Self {
            root: root.into(),
            number,
        }
    }

    fn value_path(&self) -> PathBuf {
        self.root.join(format!("gpio{}/value", self.number))
    }

    /// Stock runs `echo N > export` and `echo in > gpioN/direction` only when `gpioN/value` is missing.
    /// Returns whether it exported. Errors propagate; stock ignores them.
    pub fn export_input(&self) -> io::Result<bool> {
        if self.value_path().is_file() {
            return Ok(false);
        }
        fs::write(self.root.join("export"), format!("{}\n", self.number))?;
        let direction = self.root.join(format!("gpio{}/direction", self.number));
        fs::write(direction, "in\n")?;
        Ok(true)
    }

    /// Stock reads up to 100 bytes and tests only byte 0 against `'1'`. An empty value reads as low.
    /// Unlike stock, a failed open or read is an error, not a silent low.
    pub fn read(&self) -> io::Result<bool> {
        Ok(fs::read(self.value_path())?.first() == Some(&b'1'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_runs_only_when_value_is_missing_and_only_a_leading_one_is_high() {
        let root = std::env::temp_dir().join(format!("tetrad-sys-gpio-{}", std::process::id()));
        fs::create_dir_all(root.join("gpio97")).unwrap();
        let gpio = SysfsGpio::new(&root, UIO2_GATE_GPIO);

        assert!(gpio.read().is_err());
        assert!(gpio.export_input().unwrap());
        assert_eq!(fs::read_to_string(root.join("export")).unwrap(), "97\n");
        assert_eq!(
            fs::read_to_string(root.join("gpio97/direction")).unwrap(),
            "in\n"
        );

        for (value, high) in [("1\n", true), ("0\n", false), ("", false), (" 1", false)] {
            fs::write(root.join("gpio97/value"), value).unwrap();
            assert_eq!(gpio.read().unwrap(), high, "{value:?}");
        }
        fs::remove_file(root.join("export")).unwrap();
        assert!(!gpio.export_input().unwrap());
        assert!(!root.join("export").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
