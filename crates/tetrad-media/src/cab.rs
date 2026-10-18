//! Cab blocks: host-side IR generation and the cab driver sends.
//!
//! A cab mic does not pick one factory file. The driver decrypts every file
//! of the selected entity, `zenUI::NeuralFIRSelector` blends them into one
//! 1,024-word IR (stored time-reversed), and the driver filters, scales and
//! sends it as `ReceiveData` to argument 0 (mic A) or 1 (mic B). Float
//! operation order and fused multiply-adds follow the VFP instructions at the
//! cited VAs. Stock data (default IR, room IRs, tables) is always injected.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use crate::block::{DataTransfer, IrBlockError, REVERB_TABLE_ARGUMENT, ReverbTable};
use crate::ir::{IrChannel, IrSamples, MAX_IR_SAMPLES};

pub const CAB_IR_WORDS: usize = MAX_IR_SAMPLES;
pub type CabIr = [f32; CAB_IR_WORDS];

/// Taps with `|w| <= 1e-5` are dropped (literal 0x3727c5ac, VA 0x56c4e4).
pub const TAP_EPSILON: f32 = 1e-5;
/// ML scattered coordinates (VA 0xf56780 x, 0xf567a8 y): positions 1..8 on a
/// line and FAR at (0.5, 1). Exact stock bits.
const ML_X_BITS: [u32; 9] = [
    0,
    0x3e12_6e98,
    0x3e92_4745,
    0x3edb_7176,
    0x3f12_2d0e,
    0x3f36_dc5d,
    0x3f5b_645a,
    0x3f80_0000,
    0x3f00_0000,
];
const ML_Y_BITS: [u32; 9] = [0, 0, 0, 0, 0, 0, 0, 0, 0x3f80_0000];
/// Selector ctor position (VA 0x56e9e8) and default-node x.
pub const DEFAULT_POSITION_BITS: u32 = 0x3edb_7176;

pub const PETRUCCI_ROOM_IRS: usize = 3;

pub const SAMPLE_RATE_HZ: f64 = 48_000.0;
/// `0.707f` widened to double (ctor VA 0x5555e8).
pub const FILTER_Q: f32 = 0.707;
pub const LOW_CUT_OFF_HZ: f64 = 20.0;
pub const HIGH_CUT_OFF_HZ: f64 = 20_000.0;
/// Stereo per-mic factor 0x3eb5aa1a (≈ −9 dB, VA 0x555e74).
pub const STEREO_MIC_SCALE: f32 = 0.354_813_4;
/// Stereo ctor sends `{2, 2}` with this argument (VA 0x556a54).
pub const CAB_CONFIG_ARGUMENT: u8 = 3;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CabError {
    #[error("a cab IR set needs at least one IR")]
    EmptySet,
    #[error("ZenUI image has {0} bytes; too short for the cab data")]
    ZenUiTooShort(usize),
    #[error(transparent)]
    Table(#[from] IrBlockError),
}

/// Stock data the cab drivers embed in ZenUI.
#[derive(Clone)]
pub struct StockCabData {
    pub default_ir: Arc<CabIr>,
    pub petrucci_rooms: [Arc<CabIr>; PETRUCCI_ROOM_IRS],
    pub mono_reverb: ReverbTable,
    pub stereo_reverb: ReverbTable,
}

impl StockCabData {
    /// Slices every table from the stock ZenUI image, at the file offsets of the running
    /// build (`tetrad_firmware::MediaOffsets`; bytes are never embedded). This does not verify
    /// the build.
    pub fn from_stock_zenui(image: &[u8]) -> Result<Self, CabError> {
        let offsets = tetrad_firmware::running().zenui.media;
        let words = |offset: usize| -> Result<Arc<CabIr>, CabError> {
            let bytes = image
                .get(offset..offset + CAB_IR_WORDS * 4)
                .ok_or(CabError::ZenUiTooShort(image.len()))?;
            Ok(Arc::new(std::array::from_fn(|i| {
                f32::from_le_bytes(bytes[4 * i..4 * i + 4].try_into().expect("4 bytes"))
            })))
        };
        let room = |i: usize| words(offsets.petrucci_rooms + i * CAB_IR_WORDS * 4);
        Ok(Self {
            default_ir: words(offsets.default_ir)?,
            petrucci_rooms: [room(0)?, room(1)?, room(2)?],
            mono_reverb: ReverbTable::from_stock_zenui_at(image, offsets.mono_reverb)?,
            stereo_reverb: ReverbTable::from_stock_zenui_at(image, offsets.stereo_reverb)?,
        })
    }
}

impl fmt::Debug for StockCabData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StockCabData(<stock data>)")
    }
}

/// Pads a decoded IR to 1,024 words. Stock reads 1,024 words from shorter
/// custom buffers (an over-read); zero padding is owned policy.
pub fn cab_ir(samples: &IrSamples) -> Arc<CabIr> {
    let mut words = [0.0; CAB_IR_WORDS];
    for (word, sample) in words.iter_mut().zip(samples.samples()) {
        *word = sample;
    }
    Arc::new(words)
}

#[derive(Clone)]
struct Node {
    samples: Arc<CabIr>,
    x: f32,
    y: f32,
}

impl Node {
    fn new(samples: Arc<CabIr>) -> Self {
        Self {
            samples,
            x: 0.0,
            y: 0.0,
        }
    }

    fn distance(&self, x: f32, y: f32) -> f32 {
        let dx = x - self.x;
        let dy = y - self.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// One mic's `NeuralFIRSelector` (ctor VA 0x56e9e8). State persists between
/// renders as in stock: the scattered sort reorders nodes in place, and a
/// scattered render with no usable point keeps the previous output.
#[derive(Clone)]
pub struct MicIrSet {
    nodes: Vec<Node>,
    rows: usize,
    cols: usize,
    name: String,
    position: f32,
    distance: f32,
    blend: bool,
    output: Box<CabIr>,
}

impl fmt::Debug for MicIrSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MicIrSet")
            .field("nodes", &self.nodes.len())
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("name", &self.name)
            .field("position", &self.position)
            .field("distance", &self.distance)
            .field("blend", &self.blend)
            .finish()
    }
}

impl MicIrSet {
    pub fn new(default_ir: Arc<CabIr>) -> Self {
        let position = f32::from_bits(DEFAULT_POSITION_BITS);
        Self {
            nodes: vec![Node {
                samples: default_ir,
                x: position,
                y: 0.0,
            }],
            rows: 1,
            cols: 1,
            name: "Single".into(),
            position,
            distance: 0.0,
            blend: false,
            output: Box::new([0.0; CAB_IR_WORDS]),
        }
    }

    /// `0x0056f130`: `tag` is the entity tag, or `Single` for custom IRs.
    /// Files are in catalog order. 9 "ML" files get scattered coordinates;
    /// 15 "ML" files become 2 rows (01..14, FAR ×14) renamed "NG"; NG/ME/JP/CW
    /// sets get 3 rows. `cols = count / rows`. Blend turns on when `n != 1`.
    pub fn load(&mut self, tag: &str, irs: &[Arc<CabIr>]) -> Result<(), CabError> {
        let n = irs.len();
        if n == 0 {
            return Err(CabError::EmptySet);
        }
        let mut name = tag.to_owned();
        let has = |part: &str| tag.contains(part);
        let (nodes, rows) = if n == 9 && has("ML") {
            let nodes = irs
                .iter()
                .zip(ML_X_BITS.iter().zip(ML_Y_BITS))
                .map(|(ir, (&x, y))| Node {
                    samples: Arc::clone(ir),
                    x: f32::from_bits(x),
                    y: f32::from_bits(y),
                })
                .collect();
            (nodes, 1)
        } else if n == 15 && has("ML") {
            name = "NG".into();
            let near = irs[..14].iter().cloned().map(Node::new);
            let far = std::iter::repeat_n(Node::new(Arc::clone(&irs[14])), 14);
            (near.chain(far).collect(), 2)
        } else if ["NG", "ME", "JP", "CW"].iter().any(|part| has(part)) {
            (irs.iter().cloned().map(Node::new).collect(), 3)
        } else if n != 1 && !has("Single") {
            // Stock logs "loadIR Should not be here" and keeps every IR.
            (irs.iter().cloned().map(Node::new).collect(), 1)
        } else {
            (vec![Node::new(Arc::clone(&irs[0]))], 1)
        };
        self.cols = nodes.len() / rows;
        self.nodes = nodes;
        self.rows = rows;
        self.blend = n != 1;
        self.name = name;
        Ok(())
    }

    /// `0x0056e8cc`: the built-in default IR as the only node. Rows, cols
    /// and name are left unchanged, as in stock.
    pub fn load_default(&mut self, default_ir: Arc<CabIr>) {
        self.nodes = vec![Node {
            samples: default_ir,
            x: f32::from_bits(DEFAULT_POSITION_BITS),
            y: 0.0,
        }];
        self.blend = false;
    }

    /// POSITION (normalized 0..1, column axis). Returns whether it changed.
    pub fn set_position(&mut self, value: f32) -> bool {
        std::mem::replace(&mut self.position, value) != value
    }

    /// DISTANCE (normalized 0..1, row axis). Returns whether it changed.
    pub fn set_distance(&mut self, value: f32) -> bool {
        std::mem::replace(&mut self.distance, value) != value
    }

    /// GRID MODE: `value >= 0.5` blends; otherwise the first node is used.
    pub fn set_grid_mode(&mut self, value: f32) -> bool {
        let blend = value >= 0.5;
        std::mem::replace(&mut self.blend, blend) != blend
    }

    /// `0x0056e72c`: recomputes the time-reversed output.
    pub fn render(&mut self) -> &CabIr {
        if self.name == "ML" {
            self.render_scattered();
        } else {
            self.render_grid();
        }
        &self.output
    }

    /// `0x0056c6fc` with `0x0056c5a0` taps; `out = fma(s, w, out)`.
    fn render_grid(&mut self) {
        let taps = if self.blend {
            self.bilinear_taps()
        } else {
            vec![(0, 1.0)]
        };
        self.output.fill(0.0);
        for (index, weight) in taps {
            let samples = &self.nodes[index].samples;
            for (i, sample) in samples.iter().enumerate() {
                let out = &mut self.output[CAB_IR_WORDS - 1 - i];
                *out = sample.mul_add(weight, *out);
            }
        }
    }

    fn bilinear_taps(&self) -> Vec<(usize, f32)> {
        let (rows, cols) = (self.rows as f32, self.cols as f32);
        let ry = (self.rows as i32 - 1) as f32 * self.distance;
        let fy = ry.floor();
        let (fx, frac_x) = if self.cols < 2 {
            (0.0, 0.0)
        } else {
            let cx = (self.cols as i32 - 1) as f32 * self.position;
            let fx = cx.floor();
            (fx, cx - fx)
        };
        let wx = 1.0 - frac_x;
        let base = cols.mul_add(fy, fx) as i32;
        let mut taps = Vec::with_capacity(4);
        if rows <= fy + 1.0 {
            self.tap(&mut taps, wx, base);
            self.tap(&mut taps, frac_x, base + 1);
        } else {
            let frac_y = ry - fy;
            let wy = 1.0 - frac_y;
            let next = cols.mul_add(fy + 1.0, fx);
            self.tap(&mut taps, wy * wx, base);
            self.tap(&mut taps, frac_x * wy, base + 1);
            self.tap(&mut taps, frac_y * wx, next as i32);
            self.tap(&mut taps, frac_y * frac_x, (next + 1.0) as i32);
        }
        taps
    }

    /// `0x0056c484`: clamp to the last node, drop near-zero weights. Stock
    /// throws on a negative index; clamping it to 0 is owned policy.
    fn tap(&self, taps: &mut Vec<(usize, f32)>, weight: f32, index: i32) {
        let last = self.nodes.len().saturating_sub(1);
        let index = usize::try_from(index).unwrap_or(0).min(last);
        if weight.abs() > TAP_EPSILON {
            taps.push((index, weight));
        }
    }

    /// `0x0056dbd8`: nearest points by angle, then copy, 2-point inverse
    /// distance or 3-point barycentric weights (unfused float ops).
    fn render_scattered(&mut self) {
        let (sx, sy) = (self.position, self.distance);
        self.nodes.sort_by(|a, b| {
            a.distance(sx, sy)
                .partial_cmp(&b.distance(sx, sy))
                .unwrap_or(Ordering::Equal)
        });
        let angle = |node: &Node| (node.y - sy).atan2(node.x - sx);
        // Pass 1: up to two nodes on an axis through the selector point.
        let mut picked: Vec<usize> = Vec::with_capacity(3);
        for (i, node) in self.nodes.iter().enumerate() {
            if picked.len() >= 2 {
                break;
            }
            let turn = (f64::from(angle(node)) % std::f64::consts::TAU) as f32;
            let quarter = f64::from(turn) % f64::from(std::f32::consts::FRAC_PI_2);
            if quarter.abs() <= 1e-8 {
                picked.push(i);
            }
        }
        if picked.len() < 2 {
            // Pass 2: two nearest, then one inside the opposite angular gap.
            picked.clear();
            let tau = f64::from(std::f32::consts::TAU);
            let (mut hi, mut lo) = (0.0_f64, 0.0_f64);
            for (i, node) in self.nodes.iter().enumerate() {
                if picked.len() >= 3 {
                    break;
                }
                let mut a = angle(node);
                if picked.len() < 2 {
                    a += std::f32::consts::PI;
                    if a < 0.0 {
                        a += std::f32::consts::TAU;
                    }
                    let wrapped = f64::from((f64::from(a) % tau) as f32);
                    if picked.is_empty() {
                        hi = wrapped;
                    } else {
                        lo = wrapped;
                    }
                    picked.push(i);
                    continue;
                }
                (hi, lo) = if hi < lo { (lo, hi) } else { (hi, lo) };
                if a < 0.0 {
                    a += std::f32::consts::TAU;
                }
                let a = (f64::from(a) % tau) as f32;
                let wide = f64::from(a);
                let inside = if hi - lo < std::f64::consts::PI {
                    lo < wide && wide < hi
                } else {
                    (hi < wide && a < std::f32::consts::TAU) || (wide < lo && a > 0.0)
                };
                if inside {
                    picked.push(i);
                }
            }
        }
        let out = &mut self.output;
        let reversed = |i: usize| CAB_IR_WORDS - 1 - i;
        match picked[..] {
            [only] => {
                for (i, sample) in self.nodes[only].samples.iter().enumerate() {
                    out[reversed(i)] = *sample;
                }
            }
            [first, second] => {
                let (a, b) = (&self.nodes[first], &self.nodes[second]);
                let (da, db) = (a.distance(sx, sy), b.distance(sx, sy));
                let sum = da + db;
                let (wa, wb) = if sum == 0.0 {
                    (1.0, 1.0)
                } else {
                    (1.0 - da / sum, 1.0 - db / sum)
                };
                for i in 0..CAB_IR_WORDS {
                    out[reversed(i)] = wa * a.samples[i] + wb * b.samples[i];
                }
            }
            [p0, p1, p2] => {
                out.fill(0.0);
                let [n0, n1, n2] = [p0, p1, p2].map(|i| &self.nodes[i]);
                for vertex in [p0, p1, p2] {
                    let e0 = if vertex == p0 { 1.0 } else { 0.0 };
                    let e1 = if vertex == p1 { 1.0 } else { 0.0 };
                    let e2 = if vertex == p2 { 1.0 } else { 0.0 };
                    let (x0, y0) = (n0.x, n0.y);
                    let a = (n1.y - y0) * (e2 - e0) - (n2.y - y0) * (e1 - e0);
                    let b = (e1 - e0) * (n2.x - x0) - (e2 - e0) * (n1.x - x0);
                    let d = (n2.y - y0) * (n1.x - x0) - (n1.y - y0) * (n2.x - x0);
                    let weight = if d == 0.0 {
                        0.0
                    } else {
                        -((sx * a + sy * b) - (a * x0 + y0 * b + e0 * d)) / d
                    };
                    for (i, sample) in self.nodes[vertex].samples.iter().enumerate() {
                        let slot = &mut out[reversed(i)];
                        *slot += sample * weight;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Biquad coefficients as the driver stores them (doubles of f32 results).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Biquad {
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// HPF design inlined in the mono handler (case 16, VA ≈0x554dfc);
    /// 20 Hz means off (identity).
    pub fn low_cut(cutoff_hz: f64) -> Self {
        if cutoff_hz == LOW_CUT_OFF_HZ {
            return Self::IDENTITY;
        }
        let k = Self::warp(cutoff_hz) as f32;
        Self::design(k, -2.0, k.mul_add(k, -1.0))
    }

    /// LPF design (case 17, VA ≈0x554e98); 20,000 Hz means off (identity).
    pub fn high_cut(cutoff_hz: f64) -> Self {
        if cutoff_hz == HIGH_CUT_OFF_HZ {
            return Self::IDENTITY;
        }
        let k = (1.0 / Self::warp(cutoff_hz)) as f32;
        Self::design(k, 2.0, (-k).mul_add(k, 1.0))
    }

    fn warp(cutoff_hz: f64) -> f64 {
        (cutoff_hz * f64::from(std::f32::consts::PI) / SAMPLE_RATE_HZ).tan()
    }

    fn design(k: f32, b1_scale: f32, a1_term: f32) -> Self {
        let inv_q = (1.0 / f64::from(FILTER_Q)) as f32;
        let n = 1.0 / k.mul_add(k, k.mul_add(inv_q, 1.0));
        Self {
            b0: f64::from(n),
            b1: f64::from(n * b1_scale),
            b2: f64::from(n),
            a1: f64::from((n + n) * a1_term),
            a2: f64::from(n * k.mul_add(k, (-k).mul_add(inv_q, 1.0))),
        }
    }

    /// Transposed direct form II over the stored (reversed) buffer from the
    /// last word down, state zeroed per call (VA 0x55501c).
    fn apply(&self, words: &mut CabIr) {
        let [b0, b1, b2, a1, a2] = [self.b0, self.b1, self.b2, self.a1, self.a2].map(|c| c as f32);
        let (mut z1, mut z2) = (0.0_f32, 0.0_f32);
        for word in words.iter_mut().rev() {
            let x = *word;
            let y = b0.mul_add(x, z1);
            *word = y;
            z1 = b1.mul_add(x, -(y * a1)) + z2;
            z2 = b2.mul_add(x, -(y * a2));
        }
    }
}

/// Cab drivers. Mono: `CabinetSimulatorDriver` (biquads on the host).
/// Stereo: `CabinetSimulatorStereoDriver` (−9 dB scale, filters on the DSP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CabDriver {
    Mono,
    Stereo,
}

/// Models whose mono path scales by the driver gain and sends room IRs
/// instead of the reverb table (Petrucci family).
pub fn is_petrucci(model_id: u32) -> bool {
    matches!(model_id, 12078 | 12115 | 12116 | 32089)
}

/// Host-side state of one cab block: two mics, the two filters and gain.
#[derive(Debug, Clone)]
pub struct CabBlock {
    pub driver: CabDriver,
    pub model_id: u32,
    pub mics: [MicIrSet; 2],
    pub high_cut: Biquad,
    pub low_cut: Biquad,
    /// Mono `+0x2164` / stereo `+0x20cc`; 1.0 at construction.
    pub gain: f32,
    /// Petrucci ROOM L/R DIST. values (normalized).
    pub room_distance: [f32; 2],
}

impl CabBlock {
    pub fn new(driver: CabDriver, model_id: u32, stock: &StockCabData) -> Self {
        let mic = MicIrSet::new(Arc::clone(&stock.default_ir));
        Self {
            driver,
            model_id,
            mics: [mic.clone(), mic],
            high_cut: Biquad::IDENTITY,
            low_cut: Biquad::IDENTITY,
            gain: 1.0,
            room_distance: [0.0; 2],
        }
    }

    /// Messages the driver ctor sends. `room_predicate` is the stock grid
    /// query `0x002ed194` (meaning unverified; caller supplies it). Mono
    /// Petrucci models instead mark mics and rooms dirty: call
    /// [`Self::mic_transfer`] and [`Self::room_transfer`].
    pub fn ctor_messages(&self, stock: &StockCabData, room_predicate: bool) -> Vec<DataTransfer> {
        // Bass cab 007 (M 21007, ST 33007) sets PLUGIN MODE instead of a table.
        match self.driver {
            CabDriver::Stereo => {
                let config = DataTransfer {
                    argument: CAB_CONFIG_ARGUMENT,
                    payload: [2_u32, 2].iter().flat_map(|v| v.to_le_bytes()).collect(),
                };
                let mut messages = vec![config];
                if room_predicate && self.model_id != 33007 {
                    messages.push(stock.stereo_reverb.transfer(REVERB_TABLE_ARGUMENT));
                }
                messages
            }
            CabDriver::Mono => {
                const PARALLAX: u32 = 3008;
                if room_predicate
                    && !is_petrucci(self.model_id)
                    && !matches!(self.model_id, PARALLAX | 21007)
                {
                    vec![stock.mono_reverb.transfer(REVERB_TABLE_ARGUMENT)]
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Renders `mic` and builds its send (VA 0x55501c mono, 0x555e74 stereo).
    pub fn mic_transfer(&mut self, mic: IrChannel) -> DataTransfer {
        let mut words = *self.mics[mic.index() as usize].render();
        match self.driver {
            CabDriver::Mono => {
                self.high_cut.apply(&mut words);
                self.low_cut.apply(&mut words);
                if is_petrucci(self.model_id) {
                    for word in &mut words {
                        *word *= self.gain;
                    }
                }
            }
            CabDriver::Stereo => {
                for word in &mut words {
                    *word = *word * STEREO_MIC_SCALE * self.gain;
                }
            }
        }
        transfer(mic.index(), &words)
    }

    /// Petrucci room IR for side 0/1 → argument 2/3 (VA 0x553ed8): index
    /// `max(lroundf(v * 3) - 1, 0)`, clamped to the 3 stock IRs (owned clamp).
    pub fn room_transfer(&self, stock: &StockCabData, side: IrChannel) -> DataTransfer {
        let value = self.room_distance[side.index() as usize];
        let index = ((value * 3.0).round() as i64 - 1).clamp(0, PETRUCCI_ROOM_IRS as i64 - 1);
        let mut words = *stock.petrucci_rooms[index as usize];
        self.high_cut.apply(&mut words);
        self.low_cut.apply(&mut words);
        for word in &mut words {
            *word *= self.gain;
        }
        transfer(side.index() + 2, &words)
    }
}

fn transfer(argument: u8, words: &CabIr) -> DataTransfer {
    DataTransfer {
        argument,
        payload: words.iter().flat_map(|word| word.to_le_bytes()).collect(),
    }
}
