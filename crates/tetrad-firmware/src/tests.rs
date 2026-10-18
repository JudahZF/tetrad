use super::*;

/// The 4.1.0 profile holds the exact values of the constants it replaced
/// (`ZENUI_SHA256`, `LOADSHARC_SHA256`, tetrad-media `STOCK_ZENUI_*_OFFSET`, the loadsharc
/// `CoreMap::of` table, `FIRMWARE_VERSION`).
#[test]
fn v4_1_0_profile_keeps_the_old_pins() {
    let identity = Firmware::V4_1_0.identity();
    assert_eq!(
        identity.zenui_sha256,
        [
            0xc3, 0xc2, 0xd0, 0xa6, 0x45, 0x44, 0x39, 0x48, 0xbf, 0x4f, 0xfd, 0x7a, 0x9a, 0x22,
            0x22, 0x91, 0x1a, 0x54, 0x8a, 0x77, 0x3c, 0x8e, 0x09, 0xc2, 0x18, 0xaf, 0x1a, 0xee,
            0xd6, 0x5c, 0x09, 0xd7,
        ]
    );
    let hex: String = (identity.loadsharc_sha256.iter())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        hex,
        "d145d5e934439592c52b4e76570acc0e9216e03b857a8ad8325eee9d56f254bb"
    );
    let profile = Firmware::V4_1_0.profile().unwrap();
    assert_eq!(profile.firmware, Firmware::V4_1_0);
    assert_eq!(profile.version, "4.1.0");
    assert_eq!(profile.zenui.vas, ZenUiVas::Canonical);
    assert_eq!(
        profile.zenui.media,
        MediaOffsets {
            cipher_seed: 21_009_528,
            reverb_table: 0xa4_b9b8,
            default_ir: 0xf4_d780,
            petrucci_rooms: 0xa4_7c00,
            mono_reverb: 0xa4_ac00,
            stereo_reverb: 0xa4_af10,
        }
    );
    assert_eq!(
        profile.loadsharc,
        LoadsharcLayout {
            core1: BooterCore {
                config_va: 0x123c0,
                app_mailbox: 0x2008_3000,
            },
            core2: BooterCore {
                config_va: 0x15040,
                app_mailbox: 0x2008_2000,
            },
        }
    );
}

#[test]
fn identify_needs_the_zenui_and_loadsharc_of_one_build() {
    let v410 = Firmware::V4_1_0.identity();
    let v411 = Firmware::V4_1_1.identity();
    assert_eq!(
        Firmware::identify(&v410.zenui_sha256, &v410.loadsharc_sha256),
        Ok(Firmware::V4_1_0)
    );
    assert_eq!(
        Firmware::identify(&v411.zenui_sha256, &v411.loadsharc_sha256),
        Ok(Firmware::V4_1_1)
    );
    assert_eq!(
        Firmware::identify(&v410.zenui_sha256, &v411.loadsharc_sha256),
        Err(FirmwareError::LoadsharcMismatch(Firmware::V4_1_0))
    );
    assert_eq!(
        Firmware::identify(&[0; 32], &v410.loadsharc_sha256),
        Err(FirmwareError::UnknownZenUi)
    );
}

/// A draft build has a profile exactly when no slot is open; the error names the open slots.
#[test]
fn a_draft_build_is_refused_while_a_slot_is_open() {
    let missing = v4_1_1::DRAFT.missing();
    match Firmware::V4_1_1.profile() {
        Ok(profile) => {
            assert!(missing.is_empty());
            assert!(matches!(profile.zenui.vas, ZenUiVas::Relocated(_)));
        }
        Err(error) => assert_eq!(
            error,
            FirmwareError::Incomplete {
                firmware: Firmware::V4_1_1,
                missing,
            }
        ),
    }
}

/// The INSTALL version guard: an offered version is supported only as the version string of
/// a complete build; anything else (a newer CorOS, the opaque `versionId`, nothing) is not.
#[test]
fn by_version_names_only_complete_builds() {
    assert_eq!(Firmware::by_version("4.1.0"), Some(Firmware::V4_1_0));
    assert_eq!(
        Firmware::by_version(" 4.1.1\n"),
        Firmware::V4_1_1.profile().ok().map(|_| Firmware::V4_1_1)
    );
    for other in [
        "4.1.2",
        "4.1",
        "CorOS 4.1.1",
        "G_mwOjzYsxyL2iV8N5l6r1z7yQdN5gvV",
        "",
    ] {
        assert_eq!(Firmware::by_version(other), None, "{other}");
    }
}

/// Catches data-entry errors in a relocation table: unsorted or overlapping entries, or a
/// range past the 32-bit address space.
#[test]
fn relocation_tables_are_sorted_and_disjoint() {
    for table in [v4_1_1::DRAFT.zenui_vas.unwrap_or_default()] {
        for entry in table {
            assert!(entry.len > 0, "{entry:x?}");
            assert!(entry.from.checked_add(entry.len).is_some(), "{entry:x?}");
            assert!(entry.to.checked_add(entry.len).is_some(), "{entry:x?}");
        }
        for pair in table.windows(2) {
            assert!(pair[0].from + pair[0].len <= pair[1].from, "{pair:x?}");
        }
    }
}

#[test]
fn relocate_maps_whole_ranges_only() {
    assert_eq!(
        ZenUiVas::Canonical.relocate(0x0141_1478, 57),
        Some(0x0141_1478)
    );
    let table = &[
        Relocation {
            from: 0x1000,
            len: 0x100,
            to: 0x2000,
        },
        Relocation {
            from: 0x1100,
            len: 0x10,
            to: 0x0800,
        },
    ];
    let vas = ZenUiVas::Relocated(table);
    assert_eq!(vas.relocate(0x1000, 0x100), Some(0x2000));
    assert_eq!(vas.relocate(0x1080, 0x10), Some(0x2080));
    assert_eq!(vas.relocate(0x1104, 4), Some(0x0804));
    // Across the end of an entry, below the first one, past the last one.
    assert_eq!(vas.relocate(0x10f0, 0x20), None);
    assert_eq!(vas.relocate(0x0fff, 1), None);
    assert_eq!(vas.relocate(0x1110, 1), None);
}

#[test]
fn the_default_build_runs_without_a_selection() {
    assert_eq!(running().firmware, Firmware::DEFAULT);
    assert_eq!(select(Firmware::DEFAULT.profile().unwrap()), Ok(()));
}

/// The RE results of each 4.1.1 read site (`data/v4_1_1_sites.tsv`): `(4.1.0 VA, len,
/// 4.1.1 VA)`.
fn v4_1_1_sites() -> impl Iterator<Item = (u32, u32, u32)> {
    let hex = |field: &str| u32::from_str_radix(field.trim_start_matches("0x"), 16).unwrap();
    include_str!("../data/v4_1_1_sites.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(move |line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert!(matches!(fields[3], "C" | "I"), "{line}");
            (hex(fields[0]), hex(fields[1]), hex(fields[2]))
        })
}

/// Every site the RE found maps to its 4.1.1 VA, and that VA also fits the section deltas
/// of the drivers note (`.rodata` +0xf2f8 from 0x00a3_edc0, `.data` +0xf8d0 from
/// 0x0153_5b00): two independent forms of the same data.
#[test]
fn v4_1_1_sites_match_the_re_map() {
    let vas = Firmware::V4_1_1.profile().unwrap().zenui.vas;
    let mut count = 0;
    for (va, len, expected) in v4_1_1_sites() {
        assert_eq!(vas.relocate(va, len), Some(expected), "{va:#x}");
        let delta = if va >= 0x0153_5b00 { 0xf8d0 } else { 0xf2f8 };
        assert!(va >= 0x00a3_edc0 && expected == va + delta, "{va:#x}");
        count += 1;
    }
    assert_eq!(count, 703);
}

/// The media file offsets and the site VAs name the same tables: in each complete build an
/// offset is its relocated 4.1.0 table VA − 0x8000 (the first `PT_LOAD` segment).
#[test]
fn media_offsets_agree_with_the_zenui_vas() {
    let site = |media: MediaOffsets| {
        [
            (media.cipher_seed, 57),
            (media.reverb_table, 0x1c0),
            (media.default_ir, 0x1000),
            (media.petrucci_rooms, 0x3000),
            (media.mono_reverb, 0x1c0),
            (media.stereo_reverb, 0x1c0),
        ]
    };
    let sites = site(Firmware::V4_1_0.profile().unwrap().zenui.media);
    for firmware in Firmware::ALL {
        let Ok(profile) = firmware.profile() else {
            continue;
        };
        for ((offset, len), (own, _)) in sites.into_iter().zip(site(profile.zenui.media)) {
            let va = profile.zenui.vas.relocate(offset as u32 + 0x8000, len);
            assert_eq!(va, Some(own as u32 + 0x8000), "{firmware} {offset:#x}");
        }
    }
}
