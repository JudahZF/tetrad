//! Cab drivers: `zenUI::CabinetSimulatorDriver` (mono; ctor VA `0x5555e8`,
//! hooks `0x554788`, `0x553e08`, `0x5551d8`), its subclass `ParallaxDriver`
//! (ctor `0x5602f4`, `+0x30` `0x56018c`) and `CabinetSimulatorStereoDriver`
//! (ctor `0x556a54`, hooks `0x556188`, `0x5559a0`, `0x555f10`). The IR blend,
//! filters and sends are tetrad-media's [`CabBlock`].
//!
//! "Plugin" below is the stock test `0x2ed194`: the block's model has a
//! ModelRepo `plugin_id` (PCOM, Petrucci, John Mayer and other artist cabs).

use std::sync::Arc;
use std::time::Duration;

use tetrad_dsp_protocol::DeploymentTarget;
use tetrad_media::block::{DataTransfer, REVERB_TABLE_ARGUMENT};
use tetrad_media::cab::{Biquad, CabBlock, CabDriver, StockCabData, is_petrucci};
use tetrad_media::ir::IrChannel;

use super::super::{BlockBypass, DspWrite, ModelIdentity, ParamSetMode, SceneValue, SetupError};
use super::catalog::{to_display, to_normalized};
use super::media::{CabIrs, Media, MediaLibrary, ZeroTimer, data};
use super::{Driver, DriverKind};

const PARALLAX: u32 = 3008;
const BASS_CAB_MONO: u32 = 21007;
const BASS_CAB_STEREO: u32 = 33007;
/// ParallaxDriver passes mic base 14 to the cab ctor (VA `0x560300`).
const PARALLAX_MIC_BASE: u32 = 14;
const PLUGIN_MODE: u32 = 28;
/// Mono OUTPUT VOLUME always goes to DSP index 18, also for Parallax whose
/// OUTPUT VOLUME is 32 (literal at VA `0x554ba8`).
const OUTPUT_VOLUME: u32 = 18;

/// Driver `+0x1c` bits.
const MIC_A: u8 = 1;
const MIC_B: u8 = 2;
const ROOM_L: u8 = 4;
const ALL_PARTS: u8 = 0xf;

/// Power-law skews and ranges (literals at VAs `0x554d60..0x554d78`,
/// `0x5565fc..0x55662c`, `0x555010..0x555018`).
const LEVEL_SKEW: f32 = f32::from_bits(0x409e_b419);
const OUTPUT_SKEW: f32 = f32::from_bits(0x409d_c28f);
const FILTER_SKEW: f32 = f32::from_bits(0x3e99_999a);
const ROOM_LEVEL_SKEW: f32 = f32::from_bits(0x3f9a_e148);
const IR_LOADER_FILTER_SKEW: f32 = f32::from_bits(0x3f19_999a);
const IR_LOADER_LEVEL_SKEW: f32 = 3.5;
/// Parallax OUTPUT (38) values (VA `0x55ff74`).
const PARALLAX_OUTPUT: f32 = f32::from_bits(0x3f4c_cccd);
const PARALLAX_OUTPUT_CAB_ONLY: f32 = f32::from_bits(0x3f55_5555);

/// `powf(10, dB / 20)` in f32 (VA `0x60749c`).
fn db_to_gain(db: f32) -> f32 {
    10_f32.powf(db / 20.0)
}

/// John Mayer cabs re-skew ROOM L/R LEVEL from 1.21 to the cab LEVEL skew.
fn reskew_room_level(value: f32) -> f32 {
    let db = to_display(value, -40.0, 6.0, ROOM_LEVEL_SKEW);
    to_normalized(db, -40.0, 6.0, LEVEL_SKEW)
}

fn mic_of(index: usize) -> IrChannel {
    if index == 0 {
        IrChannel::Channel0
    } else {
        IrChannel::Channel1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Mono,
    Parallax,
    Stereo,
}

/// ParallaxDriver flags `+0x2170..=+0x2177`; the ctor sets all of them.
#[derive(Debug)]
struct ParallaxFlags {
    low: bool,
    mid: bool,
    high: bool,
    mic_a: bool,
    mic_b: bool,
    pedal: bool,
    cab: bool,
    eq: bool,
}

#[derive(Debug)]
pub(super) struct Cab {
    target: DeploymentTarget,
    model_id: u32,
    layout: Layout,
    plugin: bool,
    petrucci: bool,
    mic_base: u32,
    block: CabBlock,
    stock: Arc<StockCabData>,
    room_sizes: [[f32; 3]; 3],
    /// Last `ir selector` of each mic and what it resolved to (stock keeps
    /// the strings in the preset and the sets in its IR cache).
    selectors: [Option<(String, CabIrs)>; 2],
    /// Stored cutoffs (mono `+0x2118`, `+0x20d0`).
    low_cut_hz: f64,
    high_cut_hz: f64,
    dirty: u8,
    mic_bypassed: [bool; 2],
    room_on: [bool; 2],
    bypass: BlockBypass,
    /// The block push reloads both mics before `+0x48` (VA `0x5174a8`).
    reload: bool,
    timer: ZeroTimer,
    parallax: ParallaxFlags,
}

impl Cab {
    /// Factory step and ctor writes.
    pub(super) fn new(
        identity: &ModelIdentity,
        kind: DriverKind,
        media: &Media<'_>,
        out: &mut Vec<DspWrite>,
    ) -> Self {
        let model_id = identity.model_id;
        let stock = Arc::clone(&media.stock.cab);
        let (layout, driver, mic_base) = match kind {
            DriverKind::CabinetSimulatorStereo => (Layout::Stereo, CabDriver::Stereo, 0),
            DriverKind::Parallax => (Layout::Parallax, CabDriver::Mono, PARALLAX_MIC_BASE),
            _ => (Layout::Mono, CabDriver::Mono, 0),
        };
        let mut cab = Self {
            target: identity.target,
            model_id,
            layout,
            plugin: media.library.plugin_id(model_id).is_some(),
            petrucci: is_petrucci(model_id),
            mic_base,
            block: CabBlock::new(driver, model_id, &stock),
            stock,
            room_sizes: media.stock.room_sizes,
            selectors: [None, None],
            low_cut_hz: 20.0,
            high_cut_hz: 20_000.0,
            dirty: MIC_A,
            mic_bypassed: [false; 2],
            room_on: [false; 2],
            bypass: BlockBypass::default(),
            reload: false,
            timer: ZeroTimer::default(),
            parallax: ParallaxFlags {
                low: true,
                mid: true,
                high: true,
                mic_a: true,
                mic_b: true,
                pedal: true,
                cab: true,
                eq: true,
            },
        };
        cab.construct(out);
        cab
    }

    fn construct(&mut self, out: &mut Vec<DspWrite>) {
        let messages = self.block.ctor_messages(&self.stock, self.plugin);
        let mut messages = messages.into_iter().map(|m| self.data(m));
        match self.layout {
            Layout::Stereo => {
                // {2, 2} to buffer 3, GRID MODE 7/15 = 0, then the table.
                out.extend(messages.next());
                self.send(7, 0.0, out);
                self.send(15, 0.0, out);
                if self.model_id == BASS_CAB_STEREO {
                    self.send(PLUGIN_MODE, 1.0, out);
                } else if self.plugin {
                    out.extend(messages);
                    self.send(PLUGIN_MODE, 0.0, out);
                }
            }
            Layout::Mono | Layout::Parallax => {
                if self.plugin && self.model_id != PARALLAX {
                    let mode = f32::from(u8::from(self.model_id == BASS_CAB_MONO));
                    self.send(PLUGIN_MODE, mode, out);
                }
                out.extend(messages);
                if self.petrucci {
                    self.dirty = ALL_PARTS;
                }
            }
        }
    }

    fn data(&self, message: DataTransfer) -> DspWrite {
        data(self.target, message.argument, message.payload)
    }

    fn send(&self, index: u32, value: f32, out: &mut Vec<DspWrite>) {
        out.push(DspWrite::parameter(self.target, index, value));
    }

    fn mic_slot(&mut self, mic: IrChannel) -> &mut tetrad_media::cab::MicIrSet {
        &mut self.block.mics[mic.index() as usize]
    }

    fn selector_mic(&self, index: u32) -> Option<IrChannel> {
        match index.checked_sub(self.mic_base) {
            Some(1) => Some(IrChannel::Channel0),
            Some(9) => Some(IrChannel::Channel1),
            _ => None,
        }
    }

    /// `0x55411c` / `0x555af8` (factory or custom set) and `0x51284c`
    /// (default IR): the mic's set changes and its bit goes dirty.
    fn load(&mut self, mic: IrChannel, irs: &CabIrs) {
        let default_ir = Arc::clone(&self.stock.default_ir);
        let set = self.mic_slot(mic);
        let loaded = match irs {
            CabIrs::Factory { tag, irs } => set.load(tag, irs).is_ok(),
            CabIrs::Custom(ir) => set.load("Single", std::slice::from_ref(ir)).is_ok(),
            CabIrs::Missing => false,
        };
        if !loaded {
            set.load_default(default_ir);
        }
        self.dirty |= MIC_A << mic.index();
    }

    /// `0x328ec0` for both mics: an empty selector loads nothing.
    fn reload_mics(&mut self) {
        for i in 0..2 {
            if let Some((text, irs)) = self.selectors[i].clone()
                && !text.is_empty()
            {
                self.load(mic_of(i), &irs);
            }
        }
    }

    /// Stock `+0x48` body: each dirty part in order mic A, mic B, room L,
    /// room R (stereo: mics only). Returns whether anything went out.
    fn send_dirty(&mut self, out: &mut Vec<DspWrite>) -> bool {
        let parts = match self.layout {
            Layout::Stereo => MIC_A | MIC_B,
            Layout::Mono | Layout::Parallax => ALL_PARTS,
        };
        let dirty = self.dirty & parts;
        self.dirty &= !parts;
        for i in 0..2 {
            if dirty & (MIC_A << i) != 0 {
                let message = self.block.mic_transfer(mic_of(i));
                out.push(self.data(message));
            }
        }
        for i in 0..2 {
            if dirty & (ROOM_L << i) != 0 {
                // 0x554070: Petrucci rooms; other models resend the table.
                let message = if self.petrucci {
                    self.block.room_transfer(&self.stock, mic_of(i))
                } else {
                    self.stock.mono_reverb.transfer(REVERB_TABLE_ARGUMENT)
                };
                out.push(self.data(message));
            }
        }
        dirty != 0
    }

    fn after(&mut self, mode: ParamSetMode, out: &mut Vec<DspWrite>) -> bool {
        if mode == ParamSetMode::Live {
            self.parameters_pushed(out);
        }
        false
    }

    /// The effective block bypass (`0x51711c` args): both mics off bypass
    /// the block; MUTE then only flags a bypassed block.
    fn send_bypass(&self, out: &mut Vec<DspWrite>) {
        let both = self.mic_bypassed[0] && self.mic_bypassed[1];
        let state = BlockBypass {
            bypassed: self.bypass.bypassed || both,
            muted: self.bypass.muted && (!both || self.bypass.bypassed),
        }
        .state();
        out.push(DspWrite::Bypass {
            target: self.target,
            state,
        });
    }

    fn room_value(&self, side: usize) -> f32 {
        f32::from(u8::from(self.room_on[side] && !self.mic_bypassed[side]))
    }

    /// Mono mic bypass (VA `0x5543c8`); the generic `0x02` still goes out.
    fn mic_bypass(&mut self, side: usize, bypassed: bool, out: &mut Vec<DspWrite>) {
        if !self.plugin || self.model_id == PARALLAX {
            return;
        }
        self.mic_bypassed[side] = bypassed;
        self.send_bypass(out);
        if self.model_id != BASS_CAB_MONO {
            let index = if self.petrucci { [19, 21] } else { [24, 26] };
            self.send(index[side], self.room_value(side), out);
        }
    }

    fn john_mayer(&self) -> bool {
        match self.layout {
            Layout::Stereo => (32078..=32080).contains(&self.model_id),
            _ => (12079..=12081).contains(&self.model_id),
        }
    }

    /// Room SIZE (VA `0x554644`).
    fn room_size(&self, value: f32, out: &mut Vec<DspWrite>) {
        if self.john_mayer() {
            let size = ((2.0 * value).round() as u32).min(2) as usize;
            let [sizes, pre_delays, low_passes] = self.room_sizes;
            self.send(23, sizes[size], out);
            let pre_delay = to_normalized(pre_delays[size], 1.0, 100.0, IR_LOADER_FILTER_SKEW);
            self.send(20, pre_delay, out);
            let low_pass = to_normalized(low_passes[size], 800.0, 20_000.0, IR_LOADER_FILTER_SKEW);
            self.send(22, low_pass, out);
        } else if self.plugin && self.model_id != PARALLAX {
            self.send(23, 1.0, out);
        }
    }

    fn filter_dirty(&self) -> u8 {
        if self.petrucci {
            ALL_PARTS
        } else {
            MIC_A | MIC_B
        }
    }

    /// Mono `+0x30` (VA `0x554788`). Indices are relative to the mic base
    /// (0, or 14 for Parallax); the room and plugin tail uses absolute ones.
    fn mono_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let mic = if index >= self.mic_base + 8 {
            IrChannel::Channel1
        } else {
            IrChannel::Channel0
        };
        let bit = |changed: bool| u8::from(changed) << mic.index();
        match index.wrapping_sub(self.mic_base) {
            rel @ (0 | 8) => {
                self.mic_bypass(rel as usize / 8, value > 0.5, out);
                return true;
            }
            2 | 10 => {
                if !self.plugin || self.model_id == PARALLAX {
                    return true;
                }
                let db = to_display(value, -40.0, 6.0, LEVEL_SKEW);
                self.send(index, db_to_gain(db - 6.0), out);
                return false;
            }
            3 | 6 | 11 | 14 => return true,
            4 | 12 => self.dirty |= bit(self.mic_slot(mic).set_distance(value)),
            5 | 13 => self.dirty |= bit(self.mic_slot(mic).set_position(value)),
            7 | 15 => self.dirty |= bit(self.mic_slot(mic).set_grid_mode(value)),
            16 => {
                let hz = f64::from(to_display(value, 20.0, 500.0, FILTER_SKEW));
                if hz != self.low_cut_hz {
                    self.low_cut_hz = hz;
                    self.block.low_cut = Biquad::low_cut(hz);
                    self.dirty |= self.filter_dirty();
                }
            }
            17 => {
                let hz = f64::from(to_display(value, 500.0, 20_000.0, FILTER_SKEW));
                if hz != self.high_cut_hz {
                    self.high_cut_hz = hz;
                    self.block.high_cut = Biquad::high_cut(hz);
                    self.dirty |= self.filter_dirty();
                }
            }
            18 => {
                let gain = db_to_gain(to_display(value, -96.0, 12.0, OUTPUT_SKEW));
                let old = std::mem::replace(&mut self.block.gain, gain);
                if !self.petrucci {
                    self.send(OUTPUT_VOLUME, gain, out);
                    return false;
                }
                if old != gain {
                    self.dirty = ALL_PARTS;
                }
            }
            _ => {}
        }
        if self.petrucci {
            match index {
                24 | 25 => {
                    let side = (index - 24) as usize;
                    if value != self.block.room_distance[side] {
                        self.block.room_distance[side] = value;
                        self.dirty |= ROOM_L << side;
                        return self.after(mode, out);
                    }
                }
                19 | 21 => {
                    let side = (index - 19) as usize / 2;
                    self.room_on[side] = value > 0.0;
                    self.send(index, self.room_value(side), out);
                    return self.after(mode, out);
                }
                _ => {}
            }
            self.send(index, value, out);
            return self.after(mode, out);
        }
        if self.plugin
            && !matches!(self.model_id, PARALLAX | BASS_CAB_MONO)
            && (19..=27).contains(&index)
        {
            match index {
                24 | 26 => {
                    let side = (index - 24) as usize / 2;
                    self.room_on[side] = value > 0.0;
                    self.send(index, self.room_value(side), out);
                }
                25 | 27 if self.john_mayer() => self.send(index, reskew_room_level(value), out),
                20 | 22 if self.john_mayer() => return false,
                23 => self.room_size(value, out),
                _ => self.send(index, value, out),
            }
        }
        self.after(mode, out)
    }

    /// ParallaxDriver `+0x30` (VA `0x56018c`): the cab hook for 14..=32,
    /// then the pedal and cab switches through `0x55ff74`.
    fn parallax_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let pass = !(14..33).contains(&index) || self.mono_parameter(index, value, mode, out);
        let flags = &mut self.parallax;
        let flag = match index {
            14 => &mut flags.mic_a,
            22 => &mut flags.mic_b,
            34 => &mut flags.low,
            35 => &mut flags.mid,
            36 => &mut flags.high,
            37 => &mut flags.eq,
            39 => &mut flags.pedal,
            40 => &mut flags.cab,
            41 => return false,
            _ => return pass,
        };
        *flag = if matches!(index, 14 | 22) {
            value < 0.5
        } else {
            value > 0.5
        };
        self.parallax_update(out);
        false
    }

    /// VA `0x55ff74`: band and mic enables gated by PEDAL/CAB ACTIVE, and
    /// OUTPUT 38 (+2 dB when only the cab plays).
    fn parallax_update(&self, out: &mut Vec<DspWrite>) {
        let f = &self.parallax;
        let pedal_silent = !f.pedal || !(f.low || f.mid || f.high);
        let cab_silent = !f.cab || !(f.mic_a || f.mic_b);
        let on = |b: bool| f32::from(u8::from(b));
        self.send(34, on(f.pedal && f.low), out);
        self.send(35, on(f.pedal && f.mid), out);
        self.send(36, on(f.pedal && f.high), out);
        self.send(37, on(f.eq), out);
        self.send(14, on(!(f.mic_a && f.cab)), out);
        self.send(22, on(!(f.mic_b && f.cab)), out);
        let output = if pedal_silent && !cab_silent {
            PARALLAX_OUTPUT_CAB_ONLY
        } else {
            PARALLAX_OUTPUT
        };
        self.send(38, output, out);
    }

    /// Stereo `+0x30` (VA `0x556188`): every value is remapped onto the
    /// IR Loader Lite DSP layout and sent by the driver.
    fn stereo_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        let mic = if index > 7 {
            IrChannel::Channel1
        } else {
            IrChannel::Channel0
        };
        let bit = |changed: bool| u8::from(changed) << mic.index();
        match index {
            0 | 8 => {
                if self.plugin {
                    self.mic_bypassed[mic.index() as usize] = value > 0.5;
                    self.send_bypass(out);
                }
                self.send(index, value, out);
            }
            2 | 10 => {
                let db = to_display(value, -40.0, 6.0, LEVEL_SKEW);
                let level = if !self.plugin && db == -40.0 {
                    0.0
                } else {
                    to_normalized(db, -96.0, 12.0, IR_LOADER_LEVEL_SKEW)
                };
                self.send(index + 1, level, out);
            }
            3 | 11 => self.send(index + 3, value, out),
            6 | 14 => self.send(index - 5, value, out),
            4 | 12 => self.dirty |= bit(self.mic_slot(mic).set_distance(value)),
            5 | 13 => self.dirty |= bit(self.mic_slot(mic).set_position(value)),
            7 | 15 => self.dirty |= bit(self.mic_slot(mic).set_grid_mode(value)),
            16 => {
                let hz = to_display(value, 20.0, 500.0, FILTER_SKEW);
                let low_cut = to_normalized(hz, 20.0, 800.0, IR_LOADER_FILTER_SKEW);
                self.send(4, low_cut, out);
                self.send(12, low_cut, out);
            }
            17 => {
                let hz = to_display(value, 500.0, 20_000.0, FILTER_SKEW);
                let high_cut = to_normalized(hz, 1000.0, 20_000.0, IR_LOADER_FILTER_SKEW);
                self.send(5, high_cut, out);
                self.send(13, high_cut, out);
            }
            18 => {
                let db = to_display(value, -96.0, 12.0, OUTPUT_SKEW);
                self.send(21, to_normalized(db, -96.0, 12.0, 1.0), out);
            }
            _ => {}
        }
        if mode == ParamSetMode::Live {
            self.parameters_pushed(out);
        }
        let skipped = self.john_mayer() && index & !2 == 20;
        if self.plugin && self.model_id != BASS_CAB_STEREO && !skipped {
            match index {
                19..=23 => self.send(index - 3, value, out),
                25 | 27 if self.john_mayer() => self.send(index, reskew_room_level(value), out),
                24..=27 => self.send(index, value, out),
                _ => {}
            }
        }
        false
    }
}

impl Driver for Cab {
    fn kind(&self) -> DriverKind {
        match self.layout {
            Layout::Mono => DriverKind::CabinetSimulator,
            Layout::Parallax => DriverKind::Parallax,
            Layout::Stereo => DriverKind::CabinetSimulatorStereo,
        }
    }

    /// The push stores 1 in `+0x1c`, which is the cab dirty mask.
    fn begin_push(&mut self) {
        self.dirty = MIC_A;
        self.reload = true;
    }

    fn update_parameter(
        &mut self,
        index: u32,
        value: f32,
        mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        match self.layout {
            Layout::Mono => self.mono_parameter(index, value, mode, out),
            Layout::Parallax => self.parallax_parameter(index, value, mode, out),
            Layout::Stereo => self.stereo_parameter(index, value, mode, out),
        }
    }

    /// `+0x3c` (VAs `0x553e08`, `0x5559a0`): plugin cabs send the effective
    /// bypass themselves.
    fn bypass(&mut self, bypass: BlockBypass, out: &mut Vec<DspWrite>) -> bool {
        if !self.plugin || (self.layout != Layout::Stereo && self.model_id == PARALLAX) {
            return true;
        }
        self.bypass = bypass;
        self.send_bypass(out);
        false
    }

    /// `+0x48` (VAs `0x5551d8`, `0x555f10`): sends the dirty parts only when
    /// the timer is idle, then starts it. The push first reloads both mics.
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        if std::mem::take(&mut self.reload) {
            self.reload_mics();
        }
        if self.dirty != 0 && !self.timer.is_active() {
            self.send_dirty(out);
            self.timer.start();
        }
    }

    /// Scene recall reloads both mics of mono cabs only (VA `0x526cc0`
    /// tests driver types 12 and 21; stereo drivers are type 32).
    fn scene_recalled(&mut self, _: SceneValue<'_>, _out: &mut Vec<DspWrite>) {
        if self.layout != Layout::Stereo {
            self.reload_mics();
        }
    }

    fn take_timer(&mut self) -> Option<Duration> {
        self.timer.take()
    }

    /// `parameterChangeTimerSlot` (VAs `0x5552d0`, `0x555fb8`): sends what
    /// went dirty while the timer ran and restarts it.
    fn timer_elapsed(&mut self, out: &mut Vec<DspWrite>) {
        self.timer.fired();
        if self.send_dirty(out) {
            self.timer.start();
        }
    }

    /// `ir selector` (mic base + 1 and + 9). Live edits resolve and load at
    /// once, then `+0x48` (VAs `0x1727a0` → `0x3289c4` → `0x5126cc`); an
    /// unchanged live value is ignored. Push values are stored for the
    /// push-end reload. Recall values are stored for the mono reload only:
    /// stock never reloads a stereo cab in a recall.
    fn update_text(
        &mut self,
        index: u32,
        text: &str,
        mode: ParamSetMode,
        library: &dyn MediaLibrary,
        out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        let Some(mic) = self.selector_mic(index) else {
            return Ok(());
        };
        if mode == ParamSetMode::Recall && self.layout == Layout::Stereo {
            return Ok(());
        }
        let slot = &mut self.selectors[mic.index() as usize];
        if mode == ParamSetMode::Live && slot.as_ref().is_some_and(|(stored, _)| stored == text) {
            return Ok(());
        }
        let irs = library.cab_irs(text);
        *slot = Some((text.to_owned(), irs.clone()));
        if mode == ParamSetMode::Live {
            self.load(mic, &irs);
            self.parameters_pushed(out);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
