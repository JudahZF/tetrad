use super::*;
use qc_format::blob::{LinkRange, Section, SectionBytes};
use qc_format::diff::{Run, SectionRuns};

/// Synthetic two-section blob; no stock bytes.
pub(super) fn synthetic(code: Vec<u8>, dm: Vec<u8>) -> ModuleBlob {
    let ranges = vec![
        LinkRange::for_contents(Section::Dm, dm.len()).unwrap(),
        LinkRange::for_contents(Section::Code, code.len()).unwrap(),
    ];
    ModuleBlob {
        contents: vec![
            SectionBytes {
                section: Section::Dm,
                bytes: dm,
            },
            SectionBytes {
                section: Section::Code,
                bytes: code,
            },
        ],
        relocations: Vec::new(),
        ranges,
        entry: Section::Code.base(),
    }
}

#[test]
fn injected_key_decrypts_and_diff_records_select_the_core_variant() {
    let blob = synthetic(vec![1, 2, 3, 4], vec![9; 8]);
    let key = BlobKey::new(b"test secret, not a device key");
    let encrypted = key.0.crypt(&blob.to_bytes().unwrap());
    assert_eq!(decrypt(&key, &encrypted).unwrap(), blob);
    assert!(decrypt(&BlobKey::new(b"wrong"), &encrypted).is_err());
    assert_eq!(format!("{key:?}"), "BlobKey(..)");

    let diff = DiffRecord {
        name: "gain".into(),
        contents: vec![SectionRuns {
            section: Section::Code,
            runs: vec![Run {
                offset: 2,
                values: vec![7, 8],
            }],
        }],
        relocations: Vec::new(),
        ranges: Vec::new(),
    };
    assert_eq!(for_core("gain", &blob, DspCore::Core0, None).unwrap(), blob);
    let core2 = for_core("gain", &blob, DspCore::Core2, Some(&diff)).unwrap();
    assert_eq!(core2.content(Section::Code).unwrap(), [1, 2, 7, 8]);
    assert!(matches!(
        for_core("gain", &blob, DspCore::Core1, None),
        Err(BlobError::MissingDiff(DspCore::Core1))
    ));
    assert!(matches!(
        for_core("other", &blob, DspCore::Core1, Some(&diff)),
        Err(BlobError::DiffName { .. })
    ));
    assert_eq!(
        blob_path("gain").unwrap(),
        "/opt/neuraldsp/blobs/core1/gain_blob.bin"
    );
    assert!(blob_path("../x").is_err());
    assert_eq!(diffs_path(DspCore::Core0), None);
    assert_eq!(
        diffs_path(DspCore::Core3).unwrap(),
        "/opt/neuraldsp/blobs/core4/diffs.blob"
    );
}

fn parcels(values: &[u16]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// Hand-computed vectors for the stock allocator (VA 0x5e62a0), relocation
/// (VAs 0x5d71f4, 0x5d683c) and transfer addresses (VA 0x5d8e7c).
#[test]
fn placement_relocation_and_transfer_addresses_follow_the_stock_model() {
    use qc_format::blob::SectionWords;
    // Slot 0: DM 32 B, ISA 2 words, code 8 parcels. Slot 1 empty.
    let mut first = synthetic(vec![0; 16], vec![0; 32]);
    first.contents.push(SectionBytes {
        section: Section::Isa,
        bytes: vec![0; 12],
    });
    first
        .ranges
        .push(LinkRange::for_contents(Section::Isa, 12).unwrap());
    // Slot 2: CALL to its own parcel 6, a PC-relative kernel call, an
    // imm32 DM pointer, and a DM word that points at parcel 6.
    let mut second = synthetic(
        parcels(&[
            0x1804, 0x00f8, 0x0006, // kind 11 -> 0xf80006
            0x07be, 0x001e, 0xeeb7, // kind 0 rel -> 0x116eeba (kernel)
            0x0f10, 0x6030, 0x0004, // kind 8 -> 0x60300004
            0x0000,
        ]),
        vec![0; 20],
    );
    second.relocations = vec![
        SectionWords {
            section: Section::Code,
            words: vec![
                11,
                0,
                0xf8_0000,
                6,
                0,
                3,
                0x7a_aaab,
                0x9c_440f,
                8,
                6,
                0x6030_0000,
                4,
            ],
        },
        SectionWords {
            section: Section::Dm,
            words: vec![0, 0xf8_0000, 6],
        },
    ];
    second.entry = 0xf8_0006;
    first.validate().unwrap();
    second.validate().unwrap();

    let placements = place(&[Some(&first), None, Some(&second)]).unwrap();
    assert!(placements[1].is_none());
    let model = relocate(&second, placements[2].as_ref().unwrap()).unwrap();
    // DM: 0x2e1800 + align16(32); code: 0x1c0000 + 3 * 2 ISA words + 8 parcels.
    let code = &model.transfers[1];
    assert_eq!(
        model
            .transfers
            .iter()
            .map(|t| (t.section, t.address))
            .collect::<Vec<_>>(),
        [(Section::Dm, 0x2e_1820), (Section::Code, 0x38_001c)]
    );
    assert_eq!(model.entry, 0x1c_0014);
    assert_eq!(
        code.bytes,
        parcels(&[
            0x1804, 0x001c, 0x0014, // 0x1c000e + 6
            0x07be, 0x00fa, 0xeea9, // 0x116eeba - (0x1c000e + 3), 24 bits
            0x0f10, 0x002e, 0x1824, // 0x2e1820 + 4
            0x0000,
        ])
    );
    assert_eq!(model.transfers[0].bytes[..4], 0x1c_0014_u32.to_le_bytes());

    let first = relocate(&first, placements[0].as_ref().unwrap()).unwrap();
    assert_eq!(
        first
            .transfers
            .iter()
            .map(|t| (t.section, t.address))
            .collect::<Vec<_>>(),
        [
            (Section::Dm, 0x2e_1800),
            (Section::Code, 0x38_000c),
            (Section::Isa, 0x38_0000)
        ]
    );
    let huge = synthetic(vec![0; 2], vec![0; 0xe801]);
    assert!(matches!(
        place(&[Some(&huge)]),
        Err(BlobError::WontFit(Section::Dm))
    ));
    let mut unknown = second.clone();
    unknown.relocations[0].words[0] = 13;
    assert!(matches!(
        relocate(&unknown, placements[2].as_ref().unwrap()),
        Err(BlobError::UnknownRelocation(13))
    ));
}

/// Review R3, R12: parsed but unvalidated blobs give typed errors, never a
/// panic (missing section, data0 group, DM word past the end, huge offset).
#[test]
fn relocate_refuses_unvalidated_blobs_without_panicking() {
    use qc_format::blob::SectionWords;
    let base = synthetic(vec![0; 6], vec![0; 8]);
    let placement = place(&[Some(&base)]).unwrap().remove(0).unwrap();
    let with = |section, words: Vec<u32>| {
        let mut blob = base.clone();
        blob.relocations = vec![SectionWords { section, words }];
        relocate(&blob, &placement)
    };
    assert!(matches!(
        with(Section::Isa, vec![0, 0, 0, 0]),
        Err(BlobError::MissingSection(Section::Isa))
    ));
    let mut data0 = base.clone();
    data0.contents.push(SectionBytes {
        section: Section::Data0,
        bytes: vec![0; 4],
    });
    data0
        .ranges
        .push(LinkRange::for_contents(Section::Data0, 4).unwrap());
    data0.relocations = vec![SectionWords {
        section: Section::Data0,
        words: vec![0, 0, 0],
    }];
    let placement0 = place(&[Some(&data0)]).unwrap().remove(0).unwrap();
    assert!(matches!(
        relocate(&data0, &placement0),
        Err(BlobError::Data0Relocation)
    ));
    assert!(matches!(
        with(Section::Dm, vec![6, 0, 0]),
        Err(BlobError::RelocationBounds {
            section: Section::Dm,
            ..
        })
    ));
    assert!(matches!(
        with(Section::Code, vec![4, u32::MAX, 0, 0]),
        Err(BlobError::RelocationBounds {
            section: Section::Code,
            ..
        })
    ));
}

/// Corpus check over the local decrypted stock blobs: each blob placed alone
/// relocates so that every code field re-decodes to `remap(target)` (mod the
/// field width) and every data word equals it. Run with `--ignored`.
#[test]
#[ignore = "reads $QC_CORPUS_DIR/plain/blobs/core1"]
fn every_stock_blob_relocates_consistently() {
    let Some(root) = crate::corpus("plain/blobs/core1") else {
        return;
    };
    let mut checked = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let blob = ModuleBlob::parse(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
        let placement = place(&[Some(&blob)]).unwrap().remove(0).unwrap();
        let model = relocate(&blob, &placement).unwrap();
        let mut placed = blob.clone();
        for transfer in &model.transfers {
            *placed.content_mut(transfer.section).unwrap() = transfer.bytes.clone();
        }
        for group in &blob.relocations {
            let bytes = placed.content(group.section).unwrap();
            if group.section == Section::Dm {
                for record in group.data_relocations().unwrap() {
                    let at = record.byte_offset as usize;
                    let expected = placement.remap(record.target_base + record.target_offset);
                    assert_eq!(bytes[at..at + 4], expected.to_le_bytes());
                    checked += 1;
                }
                continue;
            }
            let new_start = placement.remap(blob.range(group.section).unwrap().start);
            for record in group.code_relocations().unwrap() {
                let at = record.offset as usize * group.section.unit_bytes() as usize;
                let word =
                    |i: usize| u32::from(u16::from_le_bytes([bytes[at + i], bytes[at + i + 1]]));
                let (low, high) = match group.section {
                    Section::Code => (word(2) << 16 | word(4), word(0)),
                    _ => (word(0) | word(2) << 16, word(4)),
                };
                let (mask, relative) = field(record.kind).unwrap();
                let relative = relative.unwrap_or(high & 0x100 != 0);
                let decoded = if relative {
                    (low & mask).wrapping_add(new_start + record.offset)
                } else {
                    low & mask
                };
                let target = placement.remap(record.target_base.wrapping_add(record.target_offset));
                assert_eq!(decoded & mask, target & mask, "kind {}", record.kind);
                checked += 1;
            }
        }
    }
    assert!(checked > 150_000, "{checked} relocations checked");
}
