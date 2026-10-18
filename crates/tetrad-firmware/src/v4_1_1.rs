//! CorOS 4.1.1: every slot is filled from the 2026-10-17 RE notes, so the build is complete.
//!
//! The per-site RE results (driver payload VAs, seed, media offsets, loadsharc and version
//! string) are in `data/v4_1_1_sites.tsv`; a test checks each row against [`ZENUI_VAS`].

use crate::{Draft, Identity, MediaOffsets, Relocation};

/// Hashes of `/opt/neuraldsp/ZenUI` and `/usr/bin/loadsharc_SC589` in the root file system of
/// the CorOS 4.1.1 update package.
pub(crate) const IDENTITY: Identity = Identity {
    zenui_sha256: crate::sha256("a595eca83aa06b9dc7f687ed915d911b7c9ee55e4df3f0839273deae5a2c4684"),
    loadsharc_sha256: crate::sha256(
        "f623fe239144852911b50e134b4f2670696e1181ba26842a9bc565dc8c09cd85",
    ),
};

/// The 4.1.1 VAs of the site VAs. B1 found all 731 driver segments with unchanged bytes,
/// each moved by its section delta (C 451, I 280, O 0). A whole-image diff (`rodata_diff.py`)
/// finds no changed word in the two payload spans below, so each span moves as one block.
/// The spans start at the lowest site and end at the end of the highest site; a VA outside
/// them has no 4.1.1 VA.
pub(crate) const ZENUI_VAS: &[Relocation] = &[
    // `.rodata` payloads: +0xf2f8 (the delta from 4.1.0 VA 0x00a3_edc0 up).
    Relocation {
        from: 0x00a4_fc00,
        len: 0x011d_eb28 - 0x00a4_fc00,
        to: 0x00a4_fc00 + 0xf2f8,
    },
    // Cipher seed (`CIPHER_SEED`): its own entry, because `.rodata` near it has changed
    // words. C: the 57-byte slice has the same SHA-256 and occurs once in the 4.1.1 image.
    Relocation {
        from: 0x0141_1478,
        len: 57,
        to: 0x0142_0770,
    },
    // `.data` payloads (BognaUber and other amp tables): +0xf8d0.
    Relocation {
        from: 0x0153_99e8,
        len: 0x0153_f570 - 0x0153_99e8,
        to: 0x0153_99e8 + 0xf8d0,
    },
];

pub(crate) const DRAFT: Draft = Draft {
    // C: the ZenUI literal `4.1.1` (file offset 0xa46010) replaces `4.1.0`; the preset,
    // Control and cloud format strings are unchanged. The git hash `85914f2` (was `83eef6f`)
    // is only in libzenhal and `/etc/os-release`; Tetrad sends no git hash (Control field 4
    // carries the CorOS version).
    version: Some("4.1.1"),
    zenui_vas: Some(ZENUI_VAS),
    // C: byte search and the code that reads each table (pins-4.1.1.tsv).
    media: Some(MediaOffsets {
        cipher_seed: 21_071_728,
        reverb_table: 0xa5_acb0,
        default_ir: 0xf5_ca78,
        petrucci_rooms: 0xa5_6ef8,
        mono_reverb: 0xa5_9ef8,
        stereo_reverb: 0xa5_a208,
    }),
    // C: the 4.1.1 loadsharc is the 4.1.0 build, not stripped: the mapped bytes are equal,
    // so the booter tables and the `main` immediates are the 4.1.0 ones.
    loadsharc: Some(crate::v4_1_0::PROFILE.loadsharc),
};
