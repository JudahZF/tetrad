//! Explicit read-only acceptance command. Missing files are errors, never skips.
#![forbid(unsafe_code)]
use std::{
    error::Error,
    fs::File,
    io::{self, Read},
    path::Path,
};
use tetrad_loader::{BlockData, Limits, RawLdr, Signature};

fn audit(
    path: &Path,
    expected_blocks: usize,
    expected_bytes: usize,
    signature: Signature,
) -> Result<(), Box<dyn Error>> {
    let limits = Limits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_stream_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let parsed = RawLdr::parse(&bytes, limits)?;
    parsed.validate_signature(signature)?;
    if bytes.len() != expected_bytes || parsed.blocks().len() != expected_blocks {
        return Err(io::Error::other(format!("stock totals differ: expected {expected_blocks} blocks/{expected_bytes} bytes, got {}/{}", parsed.blocks().len(), bytes.len())).into());
    }
    let mut reconstructed = Vec::with_capacity(bytes.len());
    for block in parsed.blocks() {
        reconstructed.extend_from_slice(&block.header.to_bytes());
        if let BlockData::Literal(payload) = block.data {
            reconstructed.extend_from_slice(payload);
        }
    }
    if reconstructed != bytes {
        return Err(io::Error::other("LDR no-op roundtrip differs").into());
    }
    println!(
        "{signature:?}: {} blocks, {} bytes, exact EOF, XOR-valid headers, byte-exact roundtrip",
        parsed.blocks().len(),
        bytes.len()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err(io::Error::other("usage: stock_ldr_audit CORE1_RAW_LDR CORE2_RAW_LDR").into());
    }
    audit(Path::new(&args[0]), 143, 171392, Signature::Core1)?;
    audit(Path::new(&args[1]), 136, 161388, Signature::Core2)?;
    Ok(())
}
