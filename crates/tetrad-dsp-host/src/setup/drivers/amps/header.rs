//! Four drivers that hold the DSP with int parameter 0 = 2, then rebuild
//! buffer 0 from two stock header rows and replay their cached controls:
//!
//! | Driver | Ctor | `+0x30` | `+0x48` = timer slot | Buffer 3 | Replay |
//! | --- | --- | --- | --- | --- | --- |
//! | DumbleOdsLerche 1176 | 0x5658fc | 0x5652b8 | 0x565ea8 / 0x565fe0 | 0x5657ac | 0x565d9c |
//! | FenderVibroverb 1181 | 0x57e7ac | 0x57e6b4 | 0x57ed5c / 0x57eb5c | 0x57ea94 | inline |
//! | JmTwoRockCustomReverb 1182 | 0x57fb14 | 0x57f9c4 | 0x5801cc / 0x580000 | 0x57fe38 | 0x57ff2c |
//! | JmDumbleSss 1183 | 0x57f148 | 0x57efdc | 0x57f7f0 / 0x57f61c | 0x57f488 | 0x57f564 |

use std::sync::Arc;

use tetrad_dsp_protocol::DeploymentTarget;

use super::{
    AmpTables, Driver, DriverKind, DspWrite, ModelIdentity, ParamSetMode, RELOAD_DELAY, Stock,
    StockTables, TableError, Timer, metadata, send_buffers, word,
};

/// Int parameter 0: 2 holds the DSP, 0 releases it before the upload.
const HOLD: u32 = 0;
const HEADER_BYTES: usize = 0x78;
/// ODS layout bit of switch 0 (drive on; clean off).
const ODS_DRIVE: usize = 1 << 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Circuit {
    Ods,
    Vibroverb,
    TwoRock,
    Sss,
}

impl Circuit {
    fn layouts(self) -> &'static [metadata::Channel] {
        match self {
            Self::Ods => &metadata::ODS,
            Self::Vibroverb => &metadata::VIBRO,
            Self::TwoRock => &metadata::TWOROCK,
            Self::Sss => &metadata::SSS,
        }
    }

    /// Switch parameter indices, most significant layout bit first
    /// (`value != 0`, NaN on).
    fn switches(self) -> &'static [u32] {
        match self {
            Self::Ods => &[0, 9, 10, 11, 12],
            Self::Vibroverb => &[1],
            Self::TwoRock => &[0, 1],
            Self::Sss => &[0, 1, 2],
        }
    }

    /// Layout bits that stock inverts: TwoRock's second selector (0x580218).
    fn inverted(self) -> usize {
        usize::from(self == Self::TwoRock)
    }

    /// Cached controls (`+0xa4`/`+0xa8 + 4·index`; stock stores before its
    /// bounds check, which ModelRepo's counts never exceed).
    fn controls(self) -> usize {
        match self {
            Self::Ods => 14,
            Self::Vibroverb => 5,
            Self::TwoRock | Self::Sss => 9,
        }
    }

    /// Controls replayed after the upload, in stock order (mode 0).
    fn replay(self) -> &'static [u32] {
        match self {
            Self::Ods => &[1, 2, 3, 4, 7, 8, 13, 5, 6],
            Self::Vibroverb => &[0, 3, 2, 4],
            Self::TwoRock => &[2, 4, 3, 5, 6, 7, 8],
            Self::Sss => &[3, 6, 5, 4, 7, 8],
        }
    }

    /// Buffer-0 header of one layout: 0x78 bytes from two 0x3c rows with
    /// the stock word patches.
    fn header(self, layout: usize, tables: &impl StockTables) -> Result<Arc<[u8]>, TableError> {
        let channel = &self.layouts()[layout];
        let mut bytes = tables.read(channel.buffers[0])?;
        if bytes.len() != HEADER_BYTES {
            return Err(TableError::Malformed);
        }
        let mut put = |offset: usize, value: u32| {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(0x30, word(tables, channel.words[0])?);
        let second = word(tables, channel.words[1])?;
        match self {
            // 0x565590: one f32 `vadd` of 7.0.
            Self::Ods => {
                put(0x70, (f32::from_bits(second) + 7.0).to_bits());
                put(0x5c, 11);
                put(0x60, if layout & ODS_DRIVE != 0 { 2 } else { 0 });
                put(0x68, 1);
            }
            Self::TwoRock => {
                put(0x70, second);
                put(0x5c, 11);
                put(0x60, 0);
            }
            Self::Vibroverb | Self::Sss => put(0x70, second),
        }
        Ok(bytes.into())
    }
}

/// Buffer-0 headers of every layout, per circuit.
pub(super) struct Headers {
    ods: Box<[Arc<[u8]>]>,
    vibroverb: Box<[Arc<[u8]>]>,
    two_rock: Box<[Arc<[u8]>]>,
    sss: Box<[Arc<[u8]>]>,
}

impl Headers {
    pub(super) fn read(tables: &impl StockTables) -> Result<Self, TableError> {
        let read = |circuit: Circuit| {
            (0..circuit.layouts().len())
                .map(|layout| circuit.header(layout, tables))
                .collect::<Result<Box<[_]>, _>>()
        };
        Ok(Self {
            ods: read(Circuit::Ods)?,
            vibroverb: read(Circuit::Vibroverb)?,
            two_rock: read(Circuit::TwoRock)?,
            sss: read(Circuit::Sss)?,
        })
    }

    fn get(&self, circuit: Circuit) -> &[Arc<[u8]>] {
        match circuit {
            Circuit::Ods => &self.ods,
            Circuit::Vibroverb => &self.vibroverb,
            Circuit::TwoRock => &self.two_rock,
            Circuit::Sss => &self.sss,
        }
    }
}

#[derive(Debug)]
pub(crate) struct HeaderAmp {
    target: DeploymentTarget,
    circuit: Circuit,
    headers: Stock<Box<[Arc<[u8]>]>>,
    /// Raw switch bits, most significant first.
    bits: usize,
    /// Owned zeros where stock leaves the heap uninitialized.
    controls: Box<[f32]>,
    dirty: bool,
    timer: Timer,
}

impl HeaderAmp {
    /// ODS starts with every switch on, holds, and uploads at once (no
    /// timer); the other three send nothing.
    pub(crate) fn new(
        identity: &ModelIdentity,
        circuit: Circuit,
        tables: &AmpTables,
        out: &mut Vec<DspWrite>,
    ) -> Self {
        let mut driver = Self {
            target: identity.target,
            circuit,
            headers: Stock(tables.headers.0.get(circuit).into()),
            bits: 0,
            controls: vec![0.0; circuit.controls()].into(),
            dirty: true,
            timer: Timer::default(),
        };
        if circuit == Circuit::Ods {
            driver.bits = (1 << circuit.switches().len()) - 1;
            driver.hold(2, out);
            driver.flush(out);
        }
        driver
    }

    fn hold(&self, value: u32, out: &mut Vec<DspWrite>) {
        out.push(DspWrite::IntParameter {
            target: self.target,
            index: HOLD,
            value,
        });
    }

    fn ods_drive(&self) -> bool {
        self.circuit == Circuit::Ods && self.bits & ODS_DRIVE != 0
    }

    /// Sends one cached control to its DSP index (ODS clean/drive moves 5..8
    /// and 13).
    fn remap(&self, index: u32, value: f32, out: &mut Vec<DspWrite>) {
        let drive = self.ods_drive();
        let (mapped, value) = match (self.circuit, index) {
            // `fminf(v, 0.99f)`: NaN sends 0.99.
            (Circuit::Ods, 1) => (0, value.min(0.99)),
            (Circuit::Ods, 2..=4) => (index - 1, value),
            (Circuit::Ods, 5 | 6) if drive => (index - 1, value),
            (Circuit::Ods, 7 | 8) => (if drive { index - 1 } else { index - 3 }, value),
            (Circuit::Ods, 13) => (if drive { 8 } else { 6 }, value),
            (Circuit::Vibroverb, 0) => {
                out.push(DspWrite::parameter(self.target, 3, value));
                (0, value)
            }
            (Circuit::Vibroverb, 2 | 4) => (index, value),
            (Circuit::Vibroverb, 3) => (1, value),
            // `fmaxf(0.05f, v)`: NaN sends 0.05.
            (Circuit::TwoRock, 6) => (4, value.max(0.05)),
            (Circuit::TwoRock, 2..=5) => ([3, 2, 1, 0][index as usize - 2], value),
            (Circuit::TwoRock, 7 | 8) => (index - 2, value),
            (Circuit::Sss, 3..=8) => ([0, 3, 2, 1, 4, 5][index as usize - 3], value),
            _ => return,
        };
        out.push(DspWrite::parameter(self.target, mapped, value));
    }

    fn flush(&mut self, out: &mut Vec<DspWrite>) {
        if self.timer.active || !self.dirty {
            return;
        }
        let layout = self.bits ^ self.circuit.inverted();
        self.hold(0, out);
        out.push(DspWrite::Data {
            target: self.target,
            buffer: 0,
            payload: self.headers.0[layout].clone(),
        });
        send_buffers(self.target, &self.circuit.layouts()[layout], 1..4, out);
        for &index in self.circuit.replay() {
            self.remap(index, self.controls[index as usize], out);
        }
        self.dirty = false;
    }
}

impl Driver for HeaderAmp {
    fn kind(&self) -> DriverKind {
        match self.circuit {
            Circuit::Ods => DriverKind::DumbleOdsLerche,
            Circuit::Vibroverb => DriverKind::FenderVibroverb,
            Circuit::TwoRock => DriverKind::JmTwoRockCustomReverb,
            Circuit::Sss => DriverKind::JmDumbleSss,
        }
    }

    /// Caches every value; a switch change marks dirty in every mode and,
    /// outside the push with an idle timer, holds and starts the timer.
    /// Controls go out remapped. Never passes the generic `0x02`.
    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        if let Some(slot) = self.controls.get_mut(index as usize) {
            *slot = value;
        }
        let switches = self.circuit.switches();
        let Some(position) = switches.iter().position(|&switch| switch == index) else {
            self.remap(index, value, out);
            return false;
        };
        let bit = 1 << (switches.len() - 1 - position);
        let bits = if value != 0.0 {
            self.bits | bit
        } else {
            self.bits & !bit
        };
        if bits != self.bits {
            self.bits = bits;
            self.dirty = true;
            if mode != ParamSetMode::Push && !self.timer.active {
                self.hold(2, out);
                self.timer.start(RELOAD_DELAY);
            }
        }
        false
    }

    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        self.flush(out);
    }

    fn take_timer(&mut self) -> Option<std::time::Duration> {
        self.timer.pending.take()
    }

    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.elapsed();
        self.flush(out);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{identity, tables};
    use super::*;

    fn header(write: &DspWrite) -> &[u8] {
        match write {
            DspWrite::Data {
                buffer: 0, payload, ..
            } => payload,
            other => panic!("expected the buffer-0 header, got {other:?}"),
        }
    }

    fn at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn new(model_id: u32, circuit: Circuit, out: &mut Vec<DspWrite>) -> HeaderAmp {
        HeaderAmp::new(&identity(model_id), circuit, &tables(), out)
    }

    /// ODS constructor: hold, release, the drive header (+7.0 at 0x70,
    /// 11/2/1 at 0x5c/0x60/0x68), three buffers, then the nine-control
    /// replay. Going clean moves index 7 to DSP 4 and patches 0x60 = 0.
    #[test]
    fn ods_constructor_uploads_drive_header_and_clean_switch_remaps() {
        let target = identity(1176).target;
        let mut out = Vec::new();
        let mut driver = new(1176, Circuit::Ods, &mut out);
        let hold = |value| DspWrite::IntParameter {
            target,
            index: HOLD,
            value,
        };
        assert_eq!(out[..2], [hold(2), hold(0)]);
        let bytes = header(&out[2]);
        assert_eq!(f32::from_bits(at(bytes, 0x70)), 9.0);
        assert_eq!(
            [at(bytes, 0x5c), at(bytes, 0x60), at(bytes, 0x68)],
            [11, 2, 1]
        );
        assert_eq!(out.len(), 3 + 3 + 9);
        out.clear();
        driver.update_parameter(0, 0.0, ParamSetMode::Recall, &mut out);
        assert_eq!(out, [hold(2)]);
        assert_eq!(driver.take_timer(), Some(RELOAD_DELAY));
        out.clear();
        driver.update_parameter(1, f32::NAN, ParamSetMode::Recall, &mut out);
        driver.update_parameter(7, 0.4, ParamSetMode::Recall, &mut out);
        assert_eq!(
            out,
            [
                DspWrite::parameter(target, 0, 0.99),
                DspWrite::parameter(target, 4, 0.4)
            ]
        );
        out.clear();
        driver.parameters_pushed(&mut out);
        assert!(out.is_empty());
        driver.timer_elapsed(&mut out);
        assert_eq!(at(header(&out[1]), 0x60), 0);
        assert_eq!(
            out[2],
            DspWrite::StockData {
                target,
                buffer: 1,
                payload: metadata::ODS[0b0_1111].buffers[1]
            }
        );
    }

    /// Vibroverb volume writes DSP 3 then 0, live and in the replay.
    #[test]
    fn vibroverb_volume_writes_two_indices() {
        let target = identity(1181).target;
        let mut out = Vec::new();
        let mut driver = new(1181, Circuit::Vibroverb, &mut out);
        assert!(out.is_empty());
        assert!(!driver.update_parameter(0, 0.6, ParamSetMode::Push, &mut out));
        let volume = [
            DspWrite::parameter(target, 3, 0.6),
            DspWrite::parameter(target, 0, 0.6),
        ];
        assert_eq!(out, volume);
        out.clear();
        driver.parameters_pushed(&mut out);
        assert_eq!(out[5..7], volume);
    }

    /// TwoRock inverts its second selector and clamps master to ≥ 0.05.
    #[test]
    fn tworock_inverts_second_selector_and_clamps_master() {
        let target = identity(1182).target;
        let mut out = Vec::new();
        let mut driver = new(1182, Circuit::TwoRock, &mut out);
        driver.update_parameter(6, f32::NAN, ParamSetMode::Push, &mut out);
        assert_eq!(out, [DspWrite::parameter(target, 4, 0.05)]);
        out.clear();
        driver.parameters_pushed(&mut out);
        assert_eq!(at(header(&out[1]), 0x5c), 11);
        assert_eq!(
            out[2],
            DspWrite::StockData {
                target,
                buffer: 1,
                payload: metadata::TWOROCK[1].buffers[1]
            }
        );
        out.clear();
        driver.update_parameter(1, 1.0, ParamSetMode::Live, &mut out);
        driver.timer_elapsed(&mut out);
        assert_eq!(
            out[3],
            DspWrite::StockData {
                target,
                buffer: 1,
                payload: metadata::TWOROCK[0].buffers[1]
            }
        );
    }

    /// SSS layout = idx0·4 + idx1·2 + idx2; tone (6) goes to DSP 1.
    #[test]
    fn sss_switch_order_selects_layout() {
        let target = identity(1183).target;
        let mut out = Vec::new();
        let mut driver = new(1183, Circuit::Sss, &mut out);
        for (index, value) in [(0, 1.0), (1, 0.0), (2, -0.5), (6, 0.3)] {
            driver.update_parameter(index, value, ParamSetMode::Push, &mut out);
        }
        assert_eq!(out, [DspWrite::parameter(target, 1, 0.3)]);
        out.clear();
        driver.parameters_pushed(&mut out);
        assert_eq!(
            out[2],
            DspWrite::StockData {
                target,
                buffer: 1,
                payload: metadata::SSS[0b101].buffers[1]
            }
        );
    }
}
