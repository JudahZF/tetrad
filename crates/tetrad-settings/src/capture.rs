//! Neural Capture dialog I/O state: `[NeuralCaptureDialogSettings]` (NC1,
//! load `0x003ce280`, save `0x003cdeb8`) and
//! `[NeuralCapture2ControllerSettings]` (NC2, load `0x001ffcf8`, save
//! `0x001fe998`). Values are normalized params of the capture I/O device,
//! stored as floats.

use crate::Hardware;
use crate::typed::{KeyReader, KeyWriter, stock_enum};

stock_enum!(
    /// `In1Type` / `In2Type` (stored 0.0 / 1.0).
    CaptureInputType {
        Instrument = 0,
        Mic = 1,
    }
);

stock_enum!(
    /// `In1Imp` / `In2Imp`: 9 steps, index = round(v * 8).
    InputImpedance {
        K10 = 0,
        K33 = 1,
        K68 = 2,
        K91 = 3,
        K130 = 4,
        K232 = 5,
        K470 = 6,
        M1 = 7,
        M10 = 8,
    }
);

/// Levels and switches shared by both capture dialogs. Index 0 is input 1.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureIo {
    /// `ReturnLevel` (labelled IN 1 LEVEL): -12..+60 dB (Atma -12..+30 dB).
    pub return_level: f32,
    /// `InLevel` (labelled IN 2 LEVEL).
    pub in_level: f32,
    /// `HPLevel`: -40..+12 dB, 0.76923 = 0 dB.
    pub hp_level: f32,
    pub input_type: [CaptureInputType; 2],
    pub input_ground_lift: [bool; 2],
    /// `In1Phantom` / `In2Phantom`: 48 V.
    pub input_phantom: [bool; 2],
    pub output_ground_lift: [bool; 2],
    pub input_impedance: [InputImpedance; 2],
}

impl CaptureIo {
    pub fn new(hardware: Hardware) -> Self {
        // 0 dB on both scales.
        let level = match hardware {
            Hardware::QuadCortex => 1.0 / 6.0,
            Hardware::Atma => 2.0 / 7.0,
        };
        Self {
            return_level: level,
            in_level: level,
            hp_level: 0.76923,
            input_type: [CaptureInputType::Instrument; 2],
            input_ground_lift: [false; 2],
            input_phantom: [false; 2],
            output_ground_lift: [false; 2],
            input_impedance: [InputImpedance::M1; 2],
        }
    }

    fn load(group: &str, r: &mut KeyReader) -> Self {
        let d = Self::new(r.hardware);
        let mut io = Self {
            return_level: r.f32(&format!("{group}/ReturnLevel"), d.return_level),
            in_level: r.f32(&format!("{group}/InLevel"), d.in_level),
            hp_level: r.f32(&format!("{group}/HPLevel"), d.hp_level),
            ..d
        };
        for i in 0..2 {
            let n = i + 1;
            let ty = r.f32(&format!("{group}/In{n}Type"), 0.0).round() as i32;
            io.input_type[i] = r.check(
                &format!("{group}/In{n}Type"),
                CaptureInputType::from_stock(ty),
                io.input_type[i],
            );
            io.input_ground_lift[i] = r.f32(&format!("{group}/In{n}GR"), 0.0) >= 0.5;
            io.input_phantom[i] = r.f32(&format!("{group}/In{n}Phantom"), 0.0) >= 0.5;
            io.output_ground_lift[i] = r.f32(&format!("{group}/Out{n}GR"), 0.0) >= 0.5;
            let imp = (r.f32(&format!("{group}/In{n}Imp"), 0.875) * 8.0).round() as i32;
            io.input_impedance[i] = r.check(
                &format!("{group}/In{n}Imp"),
                InputImpedance::from_stock(imp),
                io.input_impedance[i],
            );
        }
        io
    }

    fn store(&self, old: &Self, group: &str, w: &mut KeyWriter) {
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        w.f32(
            &format!("{group}/ReturnLevel"),
            self.return_level,
            old.return_level,
        );
        w.f32(&format!("{group}/InLevel"), self.in_level, old.in_level);
        w.f32(&format!("{group}/HPLevel"), self.hp_level, old.hp_level);
        for i in 0..2 {
            let n = i + 1;
            w.f32(
                &format!("{group}/In{n}Type"),
                self.input_type[i].to_stock() as f32,
                old.input_type[i].to_stock() as f32,
            );
            w.f32(
                &format!("{group}/In{n}GR"),
                flag(self.input_ground_lift[i]),
                flag(old.input_ground_lift[i]),
            );
            w.f32(
                &format!("{group}/In{n}Phantom"),
                flag(self.input_phantom[i]),
                flag(old.input_phantom[i]),
            );
            w.f32(
                &format!("{group}/Out{n}GR"),
                flag(self.output_ground_lift[i]),
                flag(old.output_ground_lift[i]),
            );
            w.f32(
                &format!("{group}/In{n}Imp"),
                self.input_impedance[i].to_stock() as f32 / 8.0,
                old.input_impedance[i].to_stock() as f32 / 8.0,
            );
        }
    }
}

/// One capture dialog: I/O plus the A/B monitoring cab sim (device id,
/// default 12001, and its bypass, default true).
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureDialog {
    pub io: CaptureIo,
    pub cabsim_id: u32,
    pub cabsim_bypass: bool,
}

/// Both capture dialogs.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureSettings {
    /// NC1: cab sim keys `ABingHash` (save `0x0064e880`) and `ABingBypass`
    /// (`0x0064e98c`).
    pub nc1: CaptureDialog,
    /// NC2: `capture2CabsimId` (`0x0064ea98`), `capture2CabsimBypass`
    /// (`0x0064eba0`).
    pub nc2: CaptureDialog,
}

const NC1: (&str, &str, &str) = ("NeuralCaptureDialogSettings", "ABingHash", "ABingBypass");
const NC2: (&str, &str, &str) = (
    "NeuralCapture2ControllerSettings",
    "capture2CabsimId",
    "capture2CabsimBypass",
);
pub const DEFAULT_CABSIM_ID: u32 = 12001;

impl CaptureSettings {
    pub fn new(hardware: Hardware) -> Self {
        let dialog = CaptureDialog {
            io: CaptureIo::new(hardware),
            cabsim_id: DEFAULT_CABSIM_ID,
            cabsim_bypass: true,
        };
        Self {
            nc1: dialog.clone(),
            nc2: dialog,
        }
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let dialog = |r: &mut KeyReader, (group, id, bypass): (&str, &str, &str)| CaptureDialog {
            io: CaptureIo::load(group, r),
            cabsim_id: r.int(&format!("{group}/{id}"), DEFAULT_CABSIM_ID as i32) as u32,
            cabsim_bypass: r.bool(&format!("{group}/{bypass}"), true),
        };
        Self {
            nc1: dialog(r, NC1),
            nc2: dialog(r, NC2),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for ((new, old), (group, id, bypass)) in [(&self.nc1, &old.nc1), (&self.nc2, &old.nc2)]
            .into_iter()
            .zip([NC1, NC2])
        {
            new.io.store(&old.io, group, w);
            w.put(
                &format!("{group}/{id}"),
                &new.cabsim_id,
                &old.cabsim_id,
                |v| crate::Variant::UInt(*v),
            );
            w.bool(
                &format!("{group}/{bypass}"),
                new.cabsim_bypass,
                old.cabsim_bypass,
            );
        }
    }
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self::new(Hardware::QuadCortex)
    }
}
