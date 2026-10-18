use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[derive(Debug, thiserror::Error)]
#[error("{path}: {source}")]
pub struct FileError {
    path: PathBuf,
    #[source]
    source: io::Error,
}

/// Reads at most `limit` bytes from a regular file (symlinks are followed).
///
/// The pathname check refuses ordinary FIFOs and devices without opening them. The path
/// can still be replaced before the open; `read_regular` makes that case refuse instead
/// of block on Unix.
pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, FileError> {
    let read = || -> io::Result<Vec<u8>> {
        if !path.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "input must be a regular file",
            ));
        }
        read_regular(path, limit)
    };
    read().map_err(|source| FileError {
        path: path.to_owned(),
        source,
    })
}

/// Opens `path` and refuses unless the descriptor is a regular file.
///
/// On Unix the open is `O_NONBLOCK`, so a FIFO without a writer cannot block before the
/// descriptor check. Regular-file reads ignore the flag. A replaced device node is still
/// opened before refusal. Other targets use a blocking open and keep that FIFO window.
fn read_regular(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "input must be a regular file",
        ));
    }
    if metadata.len() > limit as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input exceeds the file-size limit",
        ));
    }
    let bound = limit
        .checked_add(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file-size limit"))?;
    let mut bytes = Vec::new();
    file.take(bound as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input grew beyond the file-size limit",
        ));
    }
    Ok(bytes)
}

/// Never replaces an existing file, including a hard link or symlink to the input.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), FileError> {
    let write = || -> io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    write().map_err(|source| FileError {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "tetrad-io-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn exports_refuse_existing_files_and_aliases() {
        let directory = Directory::new();
        let input = directory.0.join("input");
        let alias = directory.0.join("alias");
        std::fs::write(&input, b"original").unwrap();
        std::fs::hard_link(&input, &alias).unwrap();
        assert!(write_new(&input, b"replacement").is_err());
        assert!(write_new(&alias, b"replacement").is_err());
        #[cfg(unix)]
        {
            let symlink = directory.0.join("symlink");
            std::os::unix::fs::symlink(&input, &symlink).unwrap();
            assert!(write_new(&symlink, b"replacement").is_err());
        }
        assert_eq!(std::fs::read(input).unwrap(), b"original");
    }

    #[test]
    fn bounded_reads_reject_oversized_and_non_file_inputs() {
        let directory = Directory::new();
        let input = directory.0.join("input");
        std::fs::write(&input, b"12345").unwrap();
        assert!(read_bounded(&input, 4).is_err());
        assert!(read_bounded(&directory.0, 4).is_err());
        assert_eq!(read_bounded(&input, 5).unwrap(), b"12345");
    }

    /// The state after the pathname check passed and a FIFO without a writer replaced the
    /// file. A blocking open would wait here forever; the timeout turns that into a failure.
    #[cfg(unix)]
    #[test]
    fn substituted_fifo_refuses_without_blocking() {
        let directory = Directory::new();
        let fifo = directory.0.join("fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("host mkfifo utility is required for this test");
        assert!(status.success());
        let (send, receive) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || send.send(read_regular(&path, 4)).unwrap());
        let error = receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("open blocked on the FIFO")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    /// Receipts print paths lossily, but the I/O itself must take native byte paths.
    #[cfg(unix)]
    #[test]
    fn native_byte_paths_and_symlinks_read_and_write() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let directory = Directory::new();
        let output = directory.0.join(OsStr::from_bytes(b"out-\xff"));
        let link = directory.0.join(OsStr::from_bytes(b"link-\xfe"));
        write_new(&output, b"native").unwrap();
        std::os::unix::fs::symlink(&output, &link).unwrap();
        assert_eq!(read_bounded(&link, 6).unwrap(), b"native");
        assert!(write_new(&link, b"replacement").is_err());
    }

    #[test]
    fn new_exports_are_private_and_exact() {
        let directory = Directory::new();
        let output = directory.0.join("output");
        write_new(&output, b"export").unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"export");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                output.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
