//! Rotating line log for bring-up runs: `<prefix>.000.log`, `<prefix>.001.log`, … in the log
//! folder. A file rotates at [`ROTATE_BYTES`]; at most [`MAX_FILES`] files are kept. The first
//! file is pinned (it holds the boot, key and deploy lines, review B13): rotation deletes the
//! oldest file after it. Budget: 32 × 256 KB = 8 MB per journal, 16 MB for run and trace on
//! `/media/p4`. Lines are written unbuffered: a panic or a guard abort keeps every line written
//! before it. Write errors are ignored, as `Platform::log` does.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One file holds up to 256 KB (the stage plan pulls one file per `cat`).
pub const ROTATE_BYTES: u64 = 256 * 1024;
pub const MAX_FILES: u32 = 32;

#[derive(Debug)]
pub struct Journal {
    dir: PathBuf,
    prefix: &'static str,
    index: u32,
    file: Option<File>,
    bytes: u64,
}

impl Journal {
    /// Starts `<dir>/<prefix>.000.log`. Fails only when the first file cannot be created.
    pub fn create(dir: &Path, prefix: &'static str) -> std::io::Result<Self> {
        let file = File::create(Self::path_in(dir, prefix, 0))?;
        Ok(Self {
            dir: dir.to_owned(),
            prefix,
            index: 0,
            file: Some(file),
            bytes: 0,
        })
    }

    pub fn line(&mut self, line: &str) {
        if self.bytes >= ROTATE_BYTES {
            self.rotate();
        }
        if let Some(file) = self.file.as_mut()
            && file.write_all(format!("{line}\n").as_bytes()).is_ok()
        {
            self.bytes += line.len() as u64 + 1;
        }
    }

    fn rotate(&mut self) {
        self.index += 1;
        self.bytes = 0;
        self.file = File::create(self.path(self.index)).ok();
        // File 0 plus the newest `MAX_FILES - 1`.
        if let Some(oldest) = (self.index + 1)
            .checked_sub(MAX_FILES)
            .filter(|&oldest| oldest > 0)
        {
            let _ = fs::remove_file(self.path(oldest));
        }
    }

    fn path(&self, index: u32) -> PathBuf {
        Self::path_in(&self.dir, self.prefix, index)
    }

    fn path_in(dir: &Path, prefix: &str, index: u32) -> PathBuf {
        dir.join(format!("{prefix}.{index:03}.log"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B13: a long run keeps file 0 (boot, keys, deploy) and the newest files, never more
    /// than `MAX_FILES`.
    #[test]
    fn rotation_keeps_the_first_file() {
        let dir = std::env::temp_dir().join(format!("tetrad-app-journal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal::create(&dir, "run").unwrap();
        journal.line("BOOT first line");
        // Four lines fill one file; 40 files are written.
        let line = "x".repeat(ROTATE_BYTES as usize / 4);
        for _ in 0..40 * 4 {
            journal.line(&line);
        }
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names.len(), MAX_FILES as usize);
        assert_eq!(names[0], "run.000.log");
        assert_eq!(
            names[1],
            format!("run.{:03}.log", journal.index + 2 - MAX_FILES)
        );
        assert!(
            fs::read_to_string(dir.join("run.000.log"))
                .unwrap()
                .starts_with("BOOT first line")
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
