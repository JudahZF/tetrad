//! Amp drivers with channel tables (c-dsp4b). JP2C and SoloRevD only send
//! stock tables. Fortin, Deluxe, Cory Wong, ODS, Vibroverb, TwoRock and SSS
//! also send stock words and patched or scaled buffers; [`AmpTables::read`]
//! reads those once at start-up (`DriverTables::load`) and
//! [`crate::setup::Block::build`] builds them. No table bytes live in the repository.

use std::sync::Arc;
use std::time::Duration;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, SetupError};
use super::super::{StockPayload, StockTables, TableError};
use super::{Driver, DriverKind};

pub(super) mod cory_wong;
pub(super) mod deluxe;
pub(super) mod fortin;
pub(super) mod header;
mod jp2c;
mod metadata;
mod solo_rev_d;

pub(super) use jp2c::Jp2c;
pub(super) use solo_rev_d::SoloRevD;

macro_rules! payload {
    ($($va:expr => $len:expr),+ $(,)?) => {
        $crate::setup::StockPayload::new(&[$($crate::setup::Segment { va: $va, len: $len }),+])
    };
}
use payload;

/// The reload delay of every amp here except Deluxe (`QTimer::start(20)`).
const RELOAD_DELAY: Duration = Duration::from_millis(20);

/// Stock-derived values. `Debug` never prints them.
#[derive(Clone)]
struct Stock<T>(T);

impl<T> std::fmt::Debug for Stock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<stock data>")
    }
}

/// Stock words and generated buffers of the seven table amps, per channel
/// layout. Read them once from a SHA-256 checked [`super::super::ElfTables`]
/// (for example [`super::super::ElfTables::drivers`]).
#[derive(Debug)]
pub struct AmpTables {
    /// Fortin int parameter 11 per layout.
    fortin: Stock<Box<[u32]>>,
    /// Deluxe int parameter 7: 1166 layouts, then 1167 layouts.
    deluxe: Stock<[Box<[u32]>; 2]>,
    /// Cory Wong buffer 3 per layout, coefficient suffix scaled.
    cory_wong: Stock<Box<[Arc<[u8]>]>>,
    headers: Stock<header::Headers>,
}

impl AmpTables {
    pub fn read(tables: &impl StockTables) -> Result<Self, TableError> {
        Ok(Self {
            fortin: Stock(fortin::words(tables)?),
            deluxe: Stock(deluxe::words(tables)?),
            cory_wong: Stock(cory_wong::scaled_buffers(tables)?),
            headers: Stock(header::Headers::read(tables)?),
        })
    }
}

/// Every stock range these drivers send or [`AmpTables::read`] reads.
pub fn payloads() -> impl Iterator<Item = StockPayload> {
    jp2c::payloads()
        .chain(solo_rev_d::payloads())
        .chain(metadata::payloads())
}

/// One little-endian stock word.
fn word(tables: &impl StockTables, payload: StockPayload) -> Result<u32, TableError> {
    let bytes: [u8; 4] = tables
        .read(payload)?
        .try_into()
        .map_err(|_| TableError::Malformed)?;
    Ok(u32::from_le_bytes(bytes))
}

/// `ReceiveData` of every stock buffer in `buffers`, in index order.
fn send_buffers(
    target: tetrad_dsp_protocol::DeploymentTarget,
    layout: &metadata::Channel,
    buffers: std::ops::Range<u8>,
    out: &mut Vec<DspWrite>,
) {
    out.extend(buffers.map(|buffer| DspWrite::StockData {
        target,
        buffer,
        payload: layout.buffers[usize::from(buffer)],
    }));
}

/// One single-shot `QTimer`. `start` (re)arms it; the runtime takes the
/// delay through [`Driver::take_timer`] and then runs the slot, which
/// calls `elapsed`. `active` is stock's `QTimer::isActive` test.
#[derive(Debug, Default)]
struct Timer {
    pending: Option<Duration>,
    active: bool,
}

impl Timer {
    fn start(&mut self, delay: Duration) {
        self.pending = Some(delay);
        self.active = true;
    }
    fn elapsed(&mut self) {
        self.pending = None;
        self.active = false;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot};

    use super::super::driver_kind;
    use super::*;
    use crate::setup::test_factory::build;

    pub fn identity(model_id: u32) -> ModelIdentity {
        ModelIdentity {
            target: DeploymentTarget {
                core: DspCore::Core0,
                slot: ModuleSlot::try_from(8).unwrap(),
            },
            model_id,
            category: 1,
            fx_loop_send: None,
        }
    }

    /// Synthetic tables: every word is 2.0f32, every longer range repeats
    /// 1.5f32. No stock bytes.
    pub struct Synthetic;

    impl StockTables for Synthetic {
        fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
            let word = if payload.len() == 4 { 2.0_f32 } else { 1.5 };
            Ok(word
                .to_le_bytes()
                .into_iter()
                .cycle()
                .take(payload.len())
                .collect())
        }
    }

    /// Synthetic rows whose word `i` is `i / 128` as f32 (exact), so row and
    /// word order show in the results (review r14 D3). No stock bytes.
    pub struct Indexed;

    impl StockTables for Indexed {
        fn read(&self, payload: StockPayload) -> Result<Vec<u8>, TableError> {
            Ok((0..payload.len() / 4)
                .flat_map(|i| (i as f32 / 128.0).to_le_bytes())
                .collect())
        }
    }

    pub fn tables() -> AmpTables {
        AmpTables::read(&Synthetic).unwrap()
    }

    /// The seven table amps build from the injected amp words (Proposal A
    /// factory); JP2C needs only its constructor tables.
    #[test]
    fn table_amps_build_from_the_amp_tables() {
        for model_id in [1165, 1166, 1167, 1175, 1176, 1181, 1182, 1183, 1140] {
            let mut out = Vec::new();
            let driver = driver_kind(model_id, 1);
            let block = build(identity(model_id), &mut out).unwrap();
            assert_eq!(block.driver_kind(), driver);
        }
    }

    /// The pinned ZenUI through the long-lived driver table set: every amp
    /// range is registered in `stock_payloads`, and every layout of the
    /// seven table amps resolves. Nothing is printed or saved.
    #[test]
    #[ignore = "reads $QC_CORPUS_DIR/rootfs/opt/neuraldsp/ZenUI"]
    fn amp_tables_resolve_from_the_driver_table_set() {
        use crate::setup::ElfTables;

        let Some(path) = crate::corpus("rootfs/opt/neuraldsp/ZenUI") else {
            return;
        };
        let tables = ElfTables::drivers(std::fs::read(path).unwrap()).unwrap();
        for payload in payloads() {
            assert!(
                tables
                    .read(payload)
                    .is_ok_and(|bytes| bytes.len() == payload.len())
            );
        }
        assert!(AmpTables::read(&tables).is_ok());
    }
}
