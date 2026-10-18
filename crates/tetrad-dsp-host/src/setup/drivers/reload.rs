//! Pieces shared by the r14 drivers that reload stock tables or replay values after a short mute
//! (Glitch, Horizon, ODR-1, Katana, Rabea, JM Overdrive).

use std::time::Duration;

/// A stock payload of single-segment literals.
macro_rules! payload {
    ($($va:expr => $len:expr),+ $(,)?) => {
        $crate::setup::StockPayload::new(&[$($crate::setup::Segment { va: $va, len: $len }),+])
    };
}
pub(super) use payload;

/// One single-shot `QTimer` (`singleShot` set in each constructor). `start`
/// (re)arms it; the runtime takes `pending` through
/// [`super::Driver::take_timer`] and runs the slot, which calls `elapsed`.
/// `active` is stock's `timerId ≥ 0` test.
#[derive(Debug, Default)]
pub(super) struct Timer {
    pub(super) pending: Option<Duration>,
    pub(super) active: bool,
}

impl Timer {
    pub(super) fn start(&mut self, millis: u64) {
        self.pending = Some(Duration::from_millis(millis));
        self.active = true;
    }

    pub(super) fn elapsed(&mut self) {
        self.pending = None;
        self.active = false;
    }
}

/// Little-endian `u32` words of stock bytes.
pub(super) fn words(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("4-byte chunk")))
}
