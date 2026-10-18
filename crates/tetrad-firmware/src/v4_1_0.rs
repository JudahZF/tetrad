//! CorOS 4.1.0: the reference build. The site VAs in the code are its VAs.

use crate::{BooterCore, Identity, LoadsharcLayout, MediaOffsets, Profile, ZenUi, ZenUiVas};

pub(crate) const IDENTITY: Identity = Identity {
    zenui_sha256: crate::sha256("c3c2d0a645443948bf4ffd7a9a2222911a548a773c8e09c218af1aeed65c09d7"),
    loadsharc_sha256: crate::sha256(
        "d145d5e934439592c52b4e76570acc0e9216e03b857a8ad8325eee9d56f254bb",
    ),
};

pub(crate) const PROFILE: Profile = Profile {
    firmware: crate::Firmware::V4_1_0,
    version: "4.1.0",
    zenui: ZenUi {
        vas: ZenUiVas::Canonical,
        media: MediaOffsets {
            // VA 0x0141_1478 (`0x009f4bbc` immediates; .rodata copy).
            cipher_seed: 21_009_528,
            // VA 0x00a5_39b8.
            reverb_table: 0xa4_b9b8,
            // VA 0x00f5_5780.
            default_ir: 0xf4_d780,
            // VA 0x00a4_fc00.
            petrucci_rooms: 0xa4_7c00,
            // VA 0x00a5_2c00.
            mono_reverb: 0xa4_ac00,
            // VA 0x00a5_2f10.
            stereo_reverb: 0xa4_af10,
        },
    },
    loadsharc: LoadsharcLayout {
        core1: BooterCore {
            config_va: 0x123c0,
            app_mailbox: 0x2008_3000,
        },
        core2: BooterCore {
            config_va: 0x15040,
            app_mailbox: 0x2008_2000,
        },
    },
};
