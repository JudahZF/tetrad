//! The kernel fault count of DSP core 0 for the stage verdict (H1: "no fault count change").
//!
//! The k1 NaN guard adds 1 to DM `0x301820` and stores the slot at DM `0x301824` (k1
//! `0x166035..0x16603e`, ABI record §3 "NaN guard"). The only other direct-address store is
//! L2 `0xbb2705` (`= m5` after a memset: an init clear, inferred). Stock ZenUI reads no SHARC
//! word for it. The diagnostics reply word 3 ("FPEx count", `0x89ece0`) is most likely the same
//! counter, but the kernel reply builder is not traced, so the bring-up reads the word itself.
//!
//! The watch holds the platform SHARC memory ([`Platform::sharc_memory`]) and calls only
//! [`SharcMemory::read`], 4 bytes at [`FAULT_COUNT_ADDRESS`]. It never writes DSP memory.
//!
//! [`Platform::sharc_memory`]: crate::device::Platform::sharc_memory

use tetrad_capture::session::SharcMemory;

/// Physical address of DM `0x301820` on DSP core 0 (SHARC core 1): the core-1 L1 window
/// (tetrad-sys `loader::L1[0]`) maps DM `0x240000` at `0x2824_0000`.
pub const FAULT_COUNT_ADDRESS: u32 = 0x2830_1820;

/// Every sample of one run, compared with the first one.
pub struct FaultWatch {
    memory: Box<dyn SharcMemory>,
    first: Option<u32>,
    last: Option<u32>,
    changed: bool,
    unreadable: u64,
}

impl FaultWatch {
    pub fn new(memory: Box<dyn SharcMemory>) -> Self {
        Self {
            memory,
            first: None,
            last: None,
            changed: false,
            unreadable: 0,
        }
    }

    /// One read of the count. Returns the log field `faults core0=<n>`.
    pub fn sample(&mut self) -> String {
        let mut word = [0; 4];
        match self.memory.read(FAULT_COUNT_ADDRESS, &mut word) {
            Ok(()) => {
                let count = u32::from_le_bytes(word);
                self.changed |= *self.first.get_or_insert(count) != count;
                self.last = Some(count);
                format!("faults core0={count}")
            }
            Err(error) => {
                self.unreadable += 1;
                format!("faults core0=? ({error})")
            }
        }
    }

    /// The verdict reason, or `None` when every read gave the first count.
    pub fn failure(&self) -> Option<String> {
        match (self.first, self.last) {
            _ if self.unreadable > 0 => {
                Some(format!("fault count unreadable {} times", self.unreadable))
            }
            (Some(first), Some(last)) if self.changed => {
                Some(format!("fault count core0 changed {first} -> {last}"))
            }
            (None, _) => Some("fault count not read".into()),
            _ => None,
        }
    }
}
