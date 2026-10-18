//! Owned resident placement in SC589 SHARC+ L1 and its system byte aliases.
//!
//! Apertures (loadsharc pools): the core-1 host L1 aperture starts at system `0x2824_0000` and core
//! 2 at `0x28a4_0000`; local byte alias `0x24_0000` is normal word `0x9_0000`.
//!
//! Code:
//! - C: ISA48 word `w` of block 0 (`0x9_0000..=0x9_7fff`) is window byte
//!   `0x24_0000 + 6 * (w - 0x9_0000)`: the ZenUI L1 ISA table (`0x11e4650`,
//!   transfer address VA `0x5d8e7c`; `tetrad_dsp_host::blob`). Stock exercises the
//!   same table for model code in block 3.
//! - D: each stock kernel uses block-0 bytes `0x24_0000..0x24_a7f4` and no
//!   more: vectors (core-1 LDR `0x97d0`, `0x300` B), kernel data (LDR literals
//!   and fills to `0x24_2800`) and its descending C stack, B7 `0x24_2800` to
//!   I7 `0x24_a7f0` (init core 1 `0x2318e..0x2319a`, core 2 `0x2229a..0x222a6`;
//!   L7 `0x7ff4` = the span in bytes). So that range is RAM the core writes at
//!   run time; path (b) does not load the stock kernel, so nothing else owns it.
//! - D: the stock SHARC booter runs while the LDR streams. Its loadsharc table
//!   (core 1 VA `0x123c0`) lists only `0x9_0000` (`0x300` B) and `0x9_00a8`
//!   (6 B) in L1, and loadsharc copies neither; its code, data and stack are in
//!   L2 (B7 `0x200b_d920`, core-1 parcel `0xbb_df91`).
//! - I: instruction fetch past the vector table (no stock site found); no
//!   cache claims block 0 (the SC589 example LDF gives block 0 none).
//!
//! The owned code range is that stock envelope: 7166 words, ending exactly at
//! the stock stack top. Data stays in the stock-written block-1 range.

use tetrad_sharc::address::{DmWordAddress32, PmWordAddress24};
use tetrad_sharc::ldr::LdrCore;

/// First PM word of block 0 and of the vector table.
pub const CODE_BASE: u32 = 0x9_0000;
/// Block-0 bytes a stock kernel uses (vectors, data, stack): 7166 ISA48 words.
pub const CODE_WORDS: u32 = 0xa7f4 / 6;
const CODE_LOCAL_BYTE: u32 = 0x24_0000;

/// First normal word of block 1 (local byte alias `0x2c_0000`).
pub const DATA_BASE: u32 = 0xb_0000;
/// Stock-written block-1 bytes `0x2_1800` as 32-bit words.
pub const DATA_WORDS: u32 = 0x2_1800 / 4;
const DATA_LOCAL_BYTE: u32 = 0x2c_0000;

/// The shared message window (ZenUI `0x54df98`, core-1 file `0x11cda`).
pub const MESSAGE_WINDOW: u32 = 0x2008_8000;
pub const MESSAGE_WINDOW_BYTES: u32 = 0x1_791c;

/// A SOC1 SHARC+ core as named by the loader (`tetrad_loader::intent::LoaderCore`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharcCore {
    Core1,
    Core2,
}

impl SharcCore {
    const fn system_offset(self) -> u32 {
        match self {
            Self::Core1 => 0x2800_0000,
            Self::Core2 => 0x2880_0000,
        }
    }
    pub const fn ldr(self) -> LdrCore {
        match self {
            Self::Core1 => LdrCore::Core1,
            Self::Core2 => LdrCore::Core2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlacementError {
    #[error("PM word {0:#x} is outside the owned block-0 code range")]
    Code(u32),
    #[error("DM word {0:#x} is outside the owned block-1 data range")]
    Data(u32),
}

/// System byte address of a packed ISA48 word.
pub fn code_system_address(core: SharcCore, pm: PmWordAddress24) -> Result<u32, PlacementError> {
    let offset = pm
        .get()
        .checked_sub(CODE_BASE)
        .filter(|offset| *offset < CODE_WORDS)
        .ok_or(PlacementError::Code(pm.get()))?;
    Ok(core.system_offset() + CODE_LOCAL_BYTE + 6 * offset)
}

/// System byte address of a block-1 normal word.
pub fn data_system_address(core: SharcCore, dm: DmWordAddress32) -> Result<u32, PlacementError> {
    let offset = dm
        .get()
        .checked_sub(DATA_BASE)
        .filter(|offset| *offset < DATA_WORDS)
        .ok_or(PlacementError::Data(dm.get()))?;
    Ok(core.system_offset() + DATA_LOCAL_BYTE + 4 * offset)
}

/// Inverse of [`data_system_address`], for DMA models that follow descriptors.
pub fn system_to_data(core: SharcCore, system: u32) -> Option<DmWordAddress32> {
    let offset = system
        .checked_sub(core.system_offset() + DATA_LOCAL_BYTE)
        .filter(|offset| offset.is_multiple_of(4) && offset / 4 < DATA_WORDS)?;
    Some(DmWordAddress32::new(DATA_BASE + offset / 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_matches_stock_vector_and_data_apertures() {
        let pm = |a| PmWordAddress24::new(a).unwrap();
        assert_eq!(
            code_system_address(SharcCore::Core1, pm(CODE_BASE)),
            Ok(0x2824_0000)
        );
        // 128 vector words end where the stock vector block ends (count 0x300).
        assert_eq!(
            code_system_address(SharcCore::Core2, pm(CODE_BASE + 128)),
            Ok(0x28a4_0300)
        );
        // The last owned word ends at the stock stack top, byte 0x24_a7f4.
        assert_eq!(
            code_system_address(SharcCore::Core1, pm(CODE_BASE + CODE_WORDS - 1)),
            Ok(0x2824_a7f4 - 6)
        );
        assert_eq!(
            code_system_address(SharcCore::Core1, pm(CODE_BASE + CODE_WORDS)),
            Err(PlacementError::Code(CODE_BASE + CODE_WORDS))
        );
        let dm = DmWordAddress32::new;
        assert_eq!(
            data_system_address(SharcCore::Core1, dm(DATA_BASE)),
            Ok(0x282c_0000)
        );
        assert_eq!(
            data_system_address(SharcCore::Core1, dm(DATA_BASE + DATA_WORDS - 1)),
            Ok(0x282e_17fc)
        );
        assert_eq!(
            data_system_address(SharcCore::Core1, dm(DATA_BASE - 1)),
            Err(PlacementError::Data(DATA_BASE - 1))
        );
    }
}
