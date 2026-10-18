//! Tuner (`[Tuner]`, `[TunerInput]`, `[TunerMute]`) and tempo
//! (`[TempoSettings]`) settings.

use std::collections::BTreeMap;

use crate::typed::{KeyReader, KeyWriter, SettingError, stock_enum};
use crate::{SettingsFile, Variant};

stock_enum!(
    /// Stock `InputPortId` (table `DAT_01544e08`). The tuner offers Input 1,
    /// Input 2, Input 1/2, Return 1, Return 2, USB 5 and USB 6.
    InputPortId {
        Empty = 0,
        Input1 = 1,
        Input2 = 2,
        Input12 = 3,
        Return1 = 4,
        Return2 = 5,
        Return12 = 6,
        PrevRow = 7,
        Usb5 = 8,
        Usb6 = 9,
        Usb7 = 10,
        Usb8 = 11,
        Usb56 = 12,
        Usb78 = 13,
    }
);

impl InputPortId {
    pub const TUNER_CHOICES: [Self; 7] = [
        Self::Input1,
        Self::Input2,
        Self::Input12,
        Self::Return1,
        Self::Return2,
        Self::Usb5,
        Self::Usb6,
    ];
}

/// Tuner state. Load `0x00653d18`; save `0x0064cb38` (offset),
/// `0x0064f5e0` (live), `0x0064cc60` (input), `0x0064cd68` (mute).
#[derive(Clone, Debug, PartialEq)]
pub struct TunerSettings {
    /// `Tuner/BaseFreqOffsetHz` (float): reference = 440 Hz + offset.
    reference_offset_hz: f32,
    /// `Tuner/liveTunerEnabled`.
    pub live_tuner: bool,
    /// `TunerInput/inputPortId`.
    pub input: InputPortId,
    /// `TunerMute/tunerMutesOutputs`.
    pub mutes_outputs: bool,
}

impl Default for TunerSettings {
    fn default() -> Self {
        Self {
            reference_offset_hz: 0.0,
            live_tuner: false,
            input: InputPortId::Input1,
            mutes_outputs: true,
        }
    }
}

impl TunerSettings {
    /// The UI range is 400..=480 Hz (offset = norm * 80 - 40).
    pub const OFFSET_RANGE_HZ: (f32, f32) = (-40.0, 40.0);

    pub fn reference_offset_hz(&self) -> f32 {
        self.reference_offset_hz
    }

    pub fn reference_hz(&self) -> f32 {
        440.0 + self.reference_offset_hz
    }

    pub fn set_reference_offset_hz(&mut self, offset: f32) -> Result<(), SettingError> {
        let (min, max) = Self::OFFSET_RANGE_HZ;
        if !(min..=max).contains(&offset) {
            return Err(SettingError::OutOfRange {
                name: "BaseFreqOffsetHz",
                value: f64::from(offset),
                min: f64::from(min),
                max: f64::from(max),
            });
        }
        self.reference_offset_hz = offset;
        Ok(())
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let d = Self::default();
        let mut tuner = Self {
            live_tuner: r.bool("Tuner/liveTunerEnabled", d.live_tuner),
            mutes_outputs: r.bool("TunerMute/tunerMutesOutputs", d.mutes_outputs),
            ..d.clone()
        };
        let offset = r.f32("Tuner/BaseFreqOffsetHz", 0.0);
        if let Err(e) = tuner.set_reference_offset_hz(offset) {
            r.issue("Tuner/BaseFreqOffsetHz", crate::LoadIssueKind::Invalid(e));
        }
        let input = r.int("TunerInput/inputPortId", d.input.to_stock());
        tuner.input = r.check(
            "TunerInput/inputPortId",
            InputPortId::from_stock(input),
            d.input,
        );
        tuner
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.f32(
            "Tuner/BaseFreqOffsetHz",
            self.reference_offset_hz,
            old.reference_offset_hz,
        );
        w.bool("Tuner/liveTunerEnabled", self.live_tuner, old.live_tuner);
        w.put("TunerInput/inputPortId", &self.input, &old.input, |v| {
            Variant::Int(v.to_stock())
        });
        w.bool(
            "TunerMute/tunerMutesOutputs",
            self.mutes_outputs,
            old.mutes_outputs,
        );
    }
}

/// Device 25000 default for each tempo param, normalized 0..=1. The catalog
/// supplies the values; this crate does not know them. Must define every
/// param, including 2 (the legacy-layout trigger, see
/// [`TempoSettings::migrate_legacy`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TempoDefaults {
    pub params: BTreeMap<u16, f32>,
}

/// `[TempoSettings]` `0..N-1`: normalized params of virtual device 25000
/// (metronome/tempo; 25 params in 4.1.0). Values are clamped to 0..=1 on
/// load. Load `0x006514bc`, save one `0x0064c870` (skipped below 1e-5
/// relative change), save defaults `0x00650b4c`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TempoSettings {
    pub params: BTreeMap<u16, f32>,
}

impl TempoSettings {
    pub const BPM_PARAM: u16 = 0;
    /// 0 = tempo per preset, 1 = global.
    pub const SCOPE_PARAM: u16 = 1;
    /// 1.0 = tempo LED on.
    pub const LED_PARAM: u16 = 2;
    pub const BPM_RANGE: (f32, f32) = (40.0, 240.0);

    /// BPM from param 0 (linear 40..=240, `0x0064b8f8`); `None` when the
    /// device default applies.
    pub fn bpm(&self) -> Option<f32> {
        let (lo, hi) = Self::BPM_RANGE;
        self.params
            .get(&Self::BPM_PARAM)
            .map(|v| lo + v * (hi - lo))
    }

    pub fn set_bpm(&mut self, bpm: f32) -> Result<(), SettingError> {
        let (lo, hi) = Self::BPM_RANGE;
        if !(lo..=hi).contains(&bpm) {
            return Err(SettingError::OutOfRange {
                name: "bpm",
                value: f64::from(bpm),
                min: f64::from(lo),
                max: f64::from(hi),
            });
        }
        self.params.insert(Self::BPM_PARAM, (bpm - lo) / (hi - lo));
        Ok(())
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut params = BTreeMap::new();
        for key in r.file.child_keys("TempoSettings") {
            if let Ok(index) = key.parse::<u16>() {
                params.insert(
                    index,
                    r.f32(&format!("TempoSettings/{key}"), 0.0).clamp(0.0, 1.0),
                );
            }
        }
        Self { params }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for (index, value) in &self.params {
            let before = old.params.get(index).copied();
            // Stock skips changes below 1e-5 relative to the old value.
            if before.is_none_or(|b| (value - b).abs() > 1e-5 * b.abs().max(f32::MIN_POSITIVE)) {
                w.file
                    .set(&format!("TempoSettings/{index}"), &Variant::Float(*value));
            }
        }
        for index in old.params.keys().filter(|i| !self.params.contains_key(i)) {
            w.file.remove(&format!("TempoSettings/{index}"));
        }
    }

    /// Adds the device default for each param that is missing. Stock writes
    /// these when the group is missing, after reset, and on every boot.
    pub fn fill_missing(&mut self, defaults: &TempoDefaults) {
        for (index, value) in &defaults.params {
            self.params.entry(*index).or_insert(*value);
        }
    }

    /// Converts the pre-4.x layout in place (`0x0064be94`, "Migrating
    /// metronome to new format"): int BPM in `0` (default 120, clamped
    /// 40..=240), int scope in `1`, bool `tempoLedIsOff` (removed) into
    /// param 2. Returns true when the file was migrated. Must run before
    /// [`TempoSettings::fill_missing`], which would add param 2 and hide the
    /// old layout. The trigger is a missing `TempoSettings/2`: stock calls
    /// `contains` with `QString("%1").arg(2)` (confirmed).
    pub fn migrate_legacy(file: &mut SettingsFile) -> bool {
        if file.child_keys("TempoSettings").is_empty() || file.contains("TempoSettings/2") {
            return false;
        }
        let read_int = |file: &SettingsFile, key: &str, default: i32| {
            file.value(key)
                .and_then(Result::ok)
                .and_then(|v| v.to_int())
                .unwrap_or(default)
        };
        let (lo, hi) = Self::BPM_RANGE;
        let bpm = read_int(file, "TempoSettings/0", 120).clamp(lo as i32, hi as i32) as f32;
        let scope = read_int(file, "TempoSettings/1", 0);
        let led_off = file
            .value("TempoSettings/tempoLedIsOff")
            .and_then(Result::ok)
            .is_some_and(|v| v.to_bool());
        file.set("TempoSettings/0", &Variant::Float((bpm - lo) / (hi - lo)));
        file.set(
            "TempoSettings/1",
            &Variant::Float(if scope != 0 { 1.0 } else { 0.0 }),
        );
        file.set(
            "TempoSettings/2",
            &Variant::Float(if led_off { 0.0 } else { 1.0 }),
        );
        file.remove("TempoSettings/tempoLedIsOff");
        true
    }
}
