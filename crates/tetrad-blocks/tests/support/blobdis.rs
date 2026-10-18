//! V4: the research disassembler `blobdis` (subprocess only, roadmap §3)
//! decodes an owned blob's section 2. It is not in this repository: callers are
//! `#[ignore]` tests that run the binary that `QC_BLOBDIS` names, and skip without it.

use std::process::Command;

use tetrad_blocks::{BuiltModule, Callback};

/// The decode covers every section-2 parcel in order and shows the shim of
/// the ABI record §4: one kind-8 pointer store per installed `ctx` word, one
/// kind-11 CJUMP with its two pushes and kind-7 return literal per stub with an
/// ISA adapter (destroy has none), and the C return after the entry and in
/// every stub.
pub fn verify_shim_decode(built: &BuiltModule) {
    let Some(blobdis) = std::env::var_os("QC_BLOBDIS") else {
        eprintln!("skipped: set QC_BLOBDIS to the blobdis binary");
        return;
    };
    let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("owned-{}.raw", built.model_id));
    std::fs::write(&path, built.blob.to_bytes().unwrap()).unwrap();
    let output = Command::new(blobdis).arg(&path).output().unwrap();
    assert!(output.status.success(), "blobdis failed on {path:?}");
    let text = String::from_utf8(output.stdout).unwrap();

    // Columns: parcel offset, instruction hex (4 digits per parcel), text.
    let mut next = 0;
    for line in text.lines() {
        let mut columns = line.split_whitespace();
        let offset = u32::from_str_radix(columns.next().unwrap(), 16).unwrap();
        let parcels = columns.next().unwrap().len() as u32 / 4;
        assert_eq!(offset, next, "decode lost alignment at {line:?}");
        next += parcels;
    }
    assert_eq!(next, built.stats.visa_bytes / 2);

    let count = |needle: &str| text.lines().filter(|l| l.contains(needle)).count();
    for callback in std::iter::once(0).chain(Callback::ALL.map(|c| c as u32)) {
        assert_eq!(
            count(&format!("dm (0x{callback:x},i4)=r2")),
            1,
            "ctx[{callback}]"
        );
    }
    assert_eq!(count("reloc type 8"), 1 + Callback::ALL.len());
    let adapters = Callback::ALL.len() - 1;
    assert_eq!(
        (
            count("cjump"),
            count("reloc type 11"),
            count("dm (i7,m7)=r2"),
            count("reloc type 7")
        ),
        (adapters, adapters, adapters, adapters)
    );
    let returns = 1 + Callback::ALL.len();
    assert_eq!(
        (
            count("i12=dm (m7,i6)"),
            count("jump (m14,i12) (db)"),
            count("rframe")
        ),
        (returns, returns, returns)
    );
}
