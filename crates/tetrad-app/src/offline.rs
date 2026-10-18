//! Explicit bounded local-file inputs and exclusive outputs for the offline app.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{0}: input must be a regular file")]
    NotRegular(PathBuf),
    #[error("{path}: input exceeds {limit} bytes")]
    Limit { path: PathBuf, limit: usize },
    #[error("raw float input has a partial sample")]
    PartialSample,
    #[error("raw float sample {0} is not finite")]
    NonFinite(usize),
}

/// Reads a caller-controlled regular-file path. Unix opens are nonblocking so
/// a FIFO swapped in after the path check cannot block before descriptor admission.
/// This does not restrict arbitrary device-open side effects or non-Unix blocking.
pub fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>, FileError> {
    let error = |source| FileError::Io {
        path: path.to_owned(),
        source,
    };
    if !std::fs::metadata(path).map_err(error)?.is_file() {
        return Err(FileError::NotRegular(path.to_owned()));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(error)?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() {
        return Err(FileError::NotRegular(path.to_owned()));
    }
    if metadata.len() > limit as u64 {
        return Err(FileError::Limit {
            path: path.to_owned(),
            limit,
        });
    }
    let mut bytes = Vec::new();
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > limit {
        return Err(FileError::Limit {
            path: path.to_owned(),
            limit,
        });
    }
    Ok(bytes)
}

/// Raw mono/interleaved samples are explicitly classified by the command, not
/// guessed from the filename. Channel framing is checked by each consumer.
pub fn decode_f32(bytes: &[u8]) -> Result<Vec<f32>, FileError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(FileError::PartialSample);
    }
    bytes
        .chunks_exact(4)
        .enumerate()
        .map(|(index, word)| {
            let value = f32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            if value.is_finite() {
                Ok(value)
            } else {
                Err(FileError::NonFinite(index))
            }
        })
        .collect()
}

/// The parent directory must be caller-controlled. Existing files, including
/// symlinks and hard links, are refused; a failed write may leave a partial file.
pub fn write_new(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<(), FileError> {
    let error = |source| FileError::Io {
        path: path.to_owned(),
        source,
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(error)?;
    write(&mut file).map_err(error)?;
    file.flush().map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_float_input_preserves_signed_zero_and_refuses_bad_words() {
        let bytes: Vec<_> = [-0.0_f32, 0.25, -0.5]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let values = decode_f32(&bytes).unwrap();
        assert_eq!(values[0].to_bits(), (-0.0_f32).to_bits());
        assert_eq!(values[1..], [0.25, -0.5]);
        assert!(matches!(
            decode_f32(&bytes[..bytes.len() - 1]),
            Err(FileError::PartialSample)
        ));
        assert!(matches!(
            decode_f32(&f32::NAN.to_le_bytes()),
            Err(FileError::NonFinite(0))
        ));
    }
}
