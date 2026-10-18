//! IR Loader driver (`zenUI::IRLoaderDriver`, models 29001..=29008): ctor
//! VA `0x55a138`, `+0x30` `0x5596a8`, `+0x34` `0x559db4`, `+0x48`
//! `0x5595cc`. Layouts, messages and IR words come from tetrad-media.

use std::time::Duration;

use tetrad_dsp_protocol::DeploymentTarget;
use tetrad_media::block::{IrLoaderLoadPlan, IrLoaderModel, IrSlotContent};
use tetrad_media::ir::{IrChannel, IrSamples};

use super::super::{DspWrite, ModelIdentity, ParamSetMode, SetupError};
use super::media::{Media, MediaLibrary, ZeroTimer, data};
use super::{Driver, DriverKind};

const SIZE: u32 = 20;

/// One `impulseResponseVector` record. The ctor record (one tap) is never
/// sent; a missing product is a count-zero record.
#[derive(Debug)]
enum Record {
    Default,
    Loaded(IrSamples),
    Missing,
}

#[derive(Debug)]
pub(super) struct IrLoader {
    target: DeploymentTarget,
    model: IrLoaderModel,
    records: Vec<Record>,
    /// `+0x24`: one bit per record.
    dirty: u8,
    sizes: [f32; 3],
    timer: ZeroTimer,
}

impl IrLoader {
    /// Ctor: `{ir_count, output_channels}` to buffer 3, the table to buffer
    /// 2 on non-Lite models, and a timer start.
    pub(super) fn new(
        identity: &ModelIdentity,
        media: &Media<'_>,
        out: &mut Vec<DspWrite>,
    ) -> Result<Self, SetupError> {
        let model = IrLoaderModel::from_model_id(identity.model_id).ok_or(
            SetupError::UnsupportedDriver {
                model_id: identity.model_id,
                driver: DriverKind::IrLoader,
            },
        )?;
        let count = model.ir_count() as usize;
        let stock = media.stock;
        let plan = IrLoaderLoadPlan::new(
            model,
            Some(&stock.ir_loader_table),
            &vec![IrSlotContent::Default; count],
        )
        .expect("one default slot per record and the stock table");
        let target = identity.target;
        out.extend(
            plan.messages()
                .into_iter()
                .map(|message| data(target, message.argument, message.payload)),
        );
        let mut timer = ZeroTimer::default();
        timer.start();
        Ok(Self {
            target,
            model,
            records: (0..count).map(|_| Record::Default).collect(),
            dirty: 0,
            sizes: stock.ir_loader_sizes,
            timer,
        })
    }

    fn dual(&self) -> bool {
        self.model.ir_count() == 2
    }

    /// `loadIR(id, channel)` (VA `0x5599a0`): the record changes, its bit
    /// goes dirty and the timer starts; the send waits for `+0x48`.
    fn load(&mut self, channel: IrChannel, product_id: &str, library: &dyn MediaLibrary) {
        self.records[channel.index() as usize] = match library.ir_product(product_id) {
            Some(samples) => Record::Loaded(samples),
            None => Record::Missing,
        };
        self.dirty |= 1 << channel.index();
        self.timer.start();
    }
}

impl Driver for IrLoader {
    fn kind(&self) -> DriverKind {
        DriverKind::IrLoader
    }

    /// `+0x30` jump table: IR PATH never passes; slot B values only on Dual
    /// models; BALANCE A not on Single (ST); SIZE is sent from the stock
    /// table on non-Lite models.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        _mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match index {
            0 | 1 | 3 | 4 | 5 | 7 | 16..=19 | 21 => true,
            6 => self.dual() || self.model.output_channels() == 1,
            8 | 9 | 11..=15 => self.dual(),
            SIZE => {
                if !self.model.is_lite() {
                    let size = ((2.0 * value).round() as u32).min(2) as usize;
                    out.push(DspWrite::parameter(self.target, SIZE, self.sizes[size]));
                }
                false
            }
            _ => false,
        }
    }

    /// `+0x48`: each dirty record to its buffer, `count × 4` bytes.
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        for (i, channel) in [IrChannel::Channel0, IrChannel::Channel1]
            .into_iter()
            .enumerate()
            .take(self.records.len())
        {
            if self.dirty & (1 << i) == 0 {
                continue;
            }
            self.dirty &= !(1 << i);
            let payload = match &self.records[i] {
                Record::Loaded(samples) => samples.to_le_bytes(),
                Record::Missing | Record::Default => Vec::new(),
            };
            out.push(data(self.target, channel.index(), payload));
        }
    }

    fn take_timer(&mut self) -> Option<Duration> {
        self.timer.take()
    }

    /// `parameterChangeTimerSlot` (VA `0x559eb8`) calls `+0x48`.
    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.fired();
        self.parameters_pushed(out);
    }

    /// `+0x34` (VA `0x559db4`): IR PATH 2 loads record 0; IR PATH 10 loads
    /// record 1 on Dual models. Every mode loads the same way.
    fn update_text(
        &mut self,
        index: u32,
        text: &str,
        _mode: ParamSetMode,
        library: &dyn MediaLibrary,
        _out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        if let Some(channel) = self.model.channel_for_param(index) {
            self.load(channel, text, library);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DspCore, ModuleSlot};

    use super::*;
    use crate::setup::drivers::media::test_media::{TestMedia, samples};
    use crate::setup::test_factory::build_with;
    use crate::setup::{Block, BlockBypass, SetupValue};

    fn identity(model_id: u32) -> ModelIdentity {
        ModelIdentity {
            target: DeploymentTarget {
                core: DspCore::Core0,
                slot: ModuleSlot::try_from(3).unwrap(),
            },
            model_id,
            category: 29,
            fx_loop_send: None,
        }
    }

    fn writes(out: &[DspWrite]) -> Vec<(u8, usize)> {
        out.iter()
            .filter_map(|w| match w {
                DspWrite::Data {
                    buffer, payload, ..
                } => Some((*buffer, payload.len())),
                _ => None,
            })
            .collect()
    }

    fn push(block: &mut Block, out: &mut Vec<DspWrite>) {
        let mut values = vec![SetupValue::Float(0.5_f32.to_bits()); 26];
        for at in [2, 10, 22, 23] {
            values[at] = SetupValue::Text;
        }
        block
            .push(&values, BlockBypass::default(), None, out)
            .unwrap();
    }

    /// Ctor: config to buffer 3, the table to buffer 2 unless Lite; the
    /// `+0x30` table: IR PATH, slot B on Single models, BALANCE A on Single
    /// (ST) never pass; SIZE goes out from the stock table.
    #[test]
    fn ctor_messages_and_parameter_filter_follow_the_model() {
        let media = TestMedia::default();
        let mut out = Vec::new();
        let mut dual = build_with(identity(29004), &[], &media, &mut out).unwrap();
        assert_eq!(writes(&out), [(3, 8), (2, 448)]);
        let DspWrite::Data { payload, .. } = &out[0] else {
            panic!("config first");
        };
        assert_eq!(payload[..], [2, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(dual.take_timer(), Some(Duration::ZERO));
        out.clear();
        build_with(identity(29006), &[], &media, &mut out).unwrap();
        assert_eq!(writes(&out), [(3, 8)], "Lite: no table");

        out.clear();
        let mut single = build_with(identity(29002), &[], &media, &mut out).unwrap();
        out.clear();
        push(&mut single, &mut out);
        let sent: Vec<u32> = out
            .iter()
            .filter_map(|w| match w {
                DspWrite::Parameter { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [0, 1, 3, 4, 5, 7, 16, 17, 18, 19, 20, 21]);
        assert!(out.contains(&DspWrite::parameter(identity(29002).target, SIZE, 0.5)));
        out.clear();
        push(&mut dual, &mut out);
        assert!(
            out.iter()
                .any(|w| matches!(w, DspWrite::Parameter { index: 13, .. }))
        );
    }

    /// `loadIR`: a found product sends its words (at most 1,024), a missing
    /// one a count-zero record; record 1 only on Dual models. Live loads wait
    /// for the 0 ms timer slot.
    #[test]
    fn ir_paths_load_records_sent_by_the_after_push_hook() {
        let mut media = TestMedia::default();
        media
            .products
            .insert("CIR_a".into(), samples(&[0.25; 2000]));
        let mut out = Vec::new();
        let mut block = build_with(identity(29003), &[], &media, &mut out).unwrap();
        block.take_timer();
        block
            .set_text(2, "CIR_a", ParamSetMode::Push, &media, &mut out)
            .unwrap();
        block
            .set_text(10, "gone", ParamSetMode::Push, &media, &mut out)
            .unwrap();
        out.clear();
        push(&mut block, &mut out);
        assert_eq!(writes(&out), [(0, 4096), (1, 0)]);

        out.clear();
        block
            .set_text(2, "gone", ParamSetMode::Live, &media, &mut out)
            .unwrap();
        assert!(out.is_empty());
        assert_eq!(block.take_timer(), Some(Duration::ZERO));
        block.timer_elapsed(&mut out);
        assert_eq!(writes(&out), [(0, 0)]);

        out.clear();
        let mut single = build_with(identity(29001), &[], &media, &mut out).unwrap();
        single
            .set_text(10, "CIR_a", ParamSetMode::Push, &media, &mut out)
            .unwrap();
        out.clear();
        push(&mut single, &mut out);
        assert!(writes(&out).is_empty());
    }
}
