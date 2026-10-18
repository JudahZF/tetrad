//! U-Boot environment through `fw_printenv` / `fw_setenv` (`/etc/fw_env.config`: `/dev/mtd0`,
//! offset 0x10000, size 0x2000).
//!
//! Stock ZenUI loads all variables with `fw_printenv` (VA 0x0047d5b4) and saves by writing
//! `name value\n` lines to `/tmp/newVars`, then running `fw_setenv -s /tmp/newVars`
//! (VA 0x0047c310). Only gain calibration (writer VA 0x0036f3c4) and master volume
//! (VA 0x001fc850) are written. Names come from static init VA 0x00091f90.

use std::collections::BTreeMap;
use std::path::Path;

use thiserror::Error;

use crate::exec::{Command, CommandError, CommandRunner, ExitStatus, FileError, Files};

pub const SCRIPT_PATH: &str = "/tmp/newVars";

const MASTER_VOLUME_MIN: &str = "MASTER_VOLUME_MIN";
const MASTER_VOLUME_MAX: &str = "MASTER_VOLUME_MAX";

/// Output gain calibration, in `GainCalibrationOutput` order (Out1..4, Send1, Send2, HpL, HpR).
const CALIBRATION_NAMES: [&str; 8] = [
    "CAL_OUT_1",
    "CAL_OUT_2",
    "CAL_OUT_3",
    "CAL_OUT_4",
    "CAL_SEND_1",
    "CAL_SEND_2",
    "CAL_HP_L",
    "CAL_HP_R",
];

/// One gain calibration variable. Its name is the only text this module writes for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibrationVar {
    Out1,
    Out2,
    Out3,
    Out4,
    Send1,
    Send2,
    HpL,
    HpR,
}

impl CalibrationVar {
    pub fn name(self) -> &'static str {
        CALIBRATION_NAMES[self as usize]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BootEnvError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error("`{0}` ended with {1:?}")]
    Failed(&'static str, ExitStatus),
    /// A key outside the calibration set, or a value that is not a finite float.
    #[error("invalid variable `{0}`")]
    Invalid(String),
}

/// All variables from `fw_printenv` (`name=value` lines).
pub fn load(runner: &mut impl CommandRunner) -> Result<BTreeMap<String, String>, BootEnvError> {
    let output = runner.run(&Command::new("fw_printenv", None::<&str>))?;
    if !output.status.success() {
        return Err(BootEnvError::Failed("fw_printenv", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect())
}

/// Write gain calibration in one `fw_setenv -s` call. Values use the shortest decimal that
/// parses back to the same `f32`.
pub fn save_calibration(
    runner: &mut impl CommandRunner,
    files: &mut impl Files,
    values: &[(CalibrationVar, f32)],
) -> Result<(), BootEnvError> {
    let lines: Vec<(&str, f32)> = values
        .iter()
        .map(|&(var, value)| (var.name(), value))
        .collect();
    write_lines(runner, files, &lines)
}

/// Write the master volume range in one `fw_setenv -s` call.
pub fn save_master_volume(
    runner: &mut impl CommandRunner,
    files: &mut impl Files,
    min: f32,
    max: f32,
) -> Result<(), BootEnvError> {
    write_lines(
        runner,
        files,
        &[(MASTER_VOLUME_MIN, min), (MASTER_VOLUME_MAX, max)],
    )
}

fn is_allowed(name: &str) -> bool {
    name == MASTER_VOLUME_MIN || name == MASTER_VOLUME_MAX || CALIBRATION_NAMES.contains(&name)
}

/// The only place a variable name becomes script text. Nothing reaches `fw_setenv` unless its
/// name is allowed and its value is finite, so an empty value cannot delete a variable.
fn write_lines(
    runner: &mut impl CommandRunner,
    files: &mut impl Files,
    lines: &[(&str, f32)],
) -> Result<(), BootEnvError> {
    let mut script = String::new();
    for &(name, value) in lines {
        if !is_allowed(name) || !value.is_finite() {
            return Err(BootEnvError::Invalid(name.into()));
        }
        script += &format!("{name} {value}\n");
    }
    let path = Path::new(SCRIPT_PATH);
    files.write(path, script.as_bytes())?;
    let output = runner.run(&Command::new("fw_setenv", ["-s", SCRIPT_PATH]))?;
    if !output.status.success() {
        return Err(BootEnvError::Failed("fw_setenv", output.status));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::Output;
    use crate::fake::{MemFiles, ScriptedRunner};

    #[test]
    fn save_writes_stock_script_and_load_parses() {
        let mut runner = ScriptedRunner::default();
        let mut files = MemFiles::default();
        runner.expect_ok("fw_setenv -s /tmp/newVars").expect(
            "fw_printenv",
            Ok(Output::code(0).with_stdout("CAL_OUT_1=1.0125\nbootdelay=0\n")),
        );
        save_calibration(
            &mut runner,
            &mut files,
            &[(CalibrationVar::Out1, 1.0125), (CalibrationVar::HpL, 0.5)],
        )
        .unwrap();
        assert_eq!(
            files.get(SCRIPT_PATH).unwrap(),
            b"CAL_OUT_1 1.0125\nCAL_HP_L 0.5\n"
        );
        assert_eq!(load(&mut runner).unwrap()["CAL_OUT_1"], "1.0125");
    }

    /// Keys outside the calibration set (for example `bootcmd`) and non-finite values never
    /// reach the script, so `fw_setenv` is not run.
    #[test]
    fn rejects_other_keys_and_non_finite_values() {
        let mut runner = ScriptedRunner::default();
        let mut files = MemFiles::default();
        assert_eq!(
            write_lines(&mut runner, &mut files, &[("bootcmd", 0.0)]),
            Err(BootEnvError::Invalid("bootcmd".into()))
        );
        assert_eq!(
            save_master_volume(&mut runner, &mut files, f32::NAN, 6.0),
            Err(BootEnvError::Invalid(MASTER_VOLUME_MIN.into()))
        );
        assert!(files.get(SCRIPT_PATH).is_none());
    }
}
