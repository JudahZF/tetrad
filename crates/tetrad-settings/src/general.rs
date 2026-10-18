//! Root `[General]` keys of ZenjackConfig.ini, plus the two keys stock
//! keeps in an explicit "General" group (`[%General]` on disk).

use crate::Variant;
use crate::typed::{KeyReader, KeyWriter, SettingError, bounded, stock_enum};

bounded!(
    /// `brightness`, LCD backlight percent. Stock clamps below 4 and above 99
    /// (`0x00409da4`).
    ScreenBrightness: i32, 4, 100, default 50
);

bounded!(
    /// `ledBrightness` and `dimmedLedBrightness`, footswitch LED percent.
    LedBrightness: i32, 0, 100, default 27
);

/// Display and LED keys. `zenUI::BrightnessSettings`: load `0x00409228`,
/// save `0x00408dbc`, defaults `0x0040b0cc`.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub brightness: ScreenBrightness,
    pub led_brightness: LedBrightness,
    /// The UI keeps its slider step below the `led_brightness` step.
    pub dimmed_led_brightness: LedBrightness,
    pub dimmed_preset: bool,
    pub dimmed_stomp: bool,
    pub dimmed_scene: bool,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            brightness: ScreenBrightness::DEFAULT,
            led_brightness: LedBrightness::DEFAULT,
            dimmed_led_brightness: LedBrightness(6),
            dimmed_preset: false,
            dimmed_stomp: true,
            dimmed_scene: false,
        }
    }
}

impl Display {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let d = Self::default();
        // Stock clamps on apply instead of rejecting.
        let raw = r.int("brightness", d.brightness.get());
        let brightness = r.check(
            "brightness",
            ScreenBrightness::new(raw),
            ScreenBrightness::clamped(raw),
        );
        let led = r.int("ledBrightness", d.led_brightness.get());
        let dimmed = r.int("dimmedLedBrightness", d.dimmed_led_brightness.get());
        Self {
            brightness,
            led_brightness: r.check(
                "ledBrightness",
                LedBrightness::new(led),
                LedBrightness::clamped(led),
            ),
            dimmed_led_brightness: r.check(
                "dimmedLedBrightness",
                LedBrightness::new(dimmed),
                LedBrightness::clamped(dimmed),
            ),
            dimmed_preset: r.bool("dimmedPreset", d.dimmed_preset),
            dimmed_stomp: r.bool("dimmedStomp", d.dimmed_stomp),
            dimmed_scene: r.bool("dimmedScene", d.dimmed_scene),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.int("brightness", self.brightness.get(), old.brightness.get());
        w.int(
            "ledBrightness",
            self.led_brightness.get(),
            old.led_brightness.get(),
        );
        w.int(
            "dimmedLedBrightness",
            self.dimmed_led_brightness.get(),
            old.dimmed_led_brightness.get(),
        );
        w.bool("dimmedPreset", self.dimmed_preset, old.dimmed_preset);
        w.bool("dimmedStomp", self.dimmed_stomp, old.dimmed_stomp);
        w.bool("dimmedScene", self.dimmed_scene, old.dimmed_scene);
    }
}

/// Outputs the master volume knob drives. `0x001fc044` load (all
/// default true), `0x001fbd38` save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasterVolumeAssignment {
    pub out12: bool,
    pub out34: bool,
    pub send12: bool,
    pub headphones: bool,
}

impl Default for MasterVolumeAssignment {
    fn default() -> Self {
        Self {
            out12: true,
            out34: true,
            send12: true,
            headphones: true,
        }
    }
}

impl MasterVolumeAssignment {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        Self {
            out12: r.bool("masterVolumeAssignedOut12", true),
            out34: r.bool("masterVolumeAssignedOut34", true),
            send12: r.bool("masterVolumeAssignedSend12", true),
            headphones: r.bool("masterVolumeAssignedHeadphones", true),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.bool("masterVolumeAssignedOut12", self.out12, old.out12);
        w.bool("masterVolumeAssignedOut34", self.out34, old.out34);
        w.bool("masterVolumeAssignedSend12", self.send12, old.send12);
        w.bool(
            "masterVolumeAssignedHeadphones",
            self.headphones,
            old.headphones,
        );
    }
}

/// Per grid row global bypass. `globalCabBypassRow1..4` bypass cab, IR-type
/// and cab-capture blocks; `irCabBypassRow1..4` the second set
/// (`isGlobalBypassed` `0x0041111c`). Load `0x00410e50`, save
/// `0x00410968`; all default false.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlobalBypassRows {
    pub cab: [bool; 4],
    pub ir: [bool; 4],
}

impl GlobalBypassRows {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut rows = Self::default();
        for row in 0..4 {
            rows.cab[row] = r.bool(&format!("globalCabBypassRow{}", row + 1), false);
            rows.ir[row] = r.bool(&format!("irCabBypassRow{}", row + 1), false);
        }
        rows
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for row in 0..4 {
            w.bool(
                &format!("globalCabBypassRow{}", row + 1),
                self.cab[row],
                old.cab[row],
            );
            w.bool(
                &format!("irCabBypassRow{}", row + 1),
                self.ir[row],
                old.ir[row],
            );
        }
    }
}

/// `bypassSaveMode`: whether a bypass change in Scene mode is saved to the
/// scene. Stored as the `BypassOverwriteMode` key without its `en` prefix
/// (`0x003b72a4` load, `0x003b7c40` save). Numbering follows moc order
/// and the Cortex Control `SceneBlockBypass` enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BypassSaveMode {
    #[default]
    Always,
    NonStomp,
    Never,
}

impl BypassSaveMode {
    pub const fn stock_name(self) -> &'static str {
        match self {
            Self::Always => "SaveBypassAlways",
            Self::NonStomp => "SaveBypassNonStomp",
            Self::Never => "SaveBypassNever",
        }
    }

    pub fn from_stock_name(name: &str) -> Result<Self, SettingError> {
        [Self::Always, Self::NonStomp, Self::Never]
            .into_iter()
            .find(|m| m.stock_name() == name)
            .ok_or_else(|| SettingError::UnknownValue {
                name: "BypassSaveMode",
                value: name.to_owned(),
            })
    }
}

stock_enum!(
    /// `General/powerButtonSensitivity` (`[%General]`). Off disables the touch
    /// power button; others call `SetPowerButtonSensitivity(value - 1)`.
    PowerButtonSensitivity {
        Off = 0,
        Low = 1,
        Medium = 2,
        High = 3,
    }
);

bounded!(
    /// `holdTiming` index; footswitch hold delay is `(index + 5) * 100` ms
    /// (`0x0064f5bc`, assert `0..=5`). Delay table `0x412d64`, default index 3
    /// (`0x653d18`), stored at settings `+0x70` (`0x64f47c`).
    HoldTiming: i32, 0, 5, default 3
);

impl HoldTiming {
    pub const fn millis(self) -> u32 {
        (self.0 as u32 + 5) * 100
    }
}

/// Behaviour flags without a group of their own. Each key has its own stock
/// reader and writer (VAs in the p-settings doc).
#[derive(Clone, Debug, PartialEq)]
pub struct GeneralSettings {
    /// `gigViewStompAccess`.
    pub gig_view_stomp_access: bool,
    /// `swapTempoAndTunerAccess`.
    pub swap_tempo_and_tuner_access: bool,
    /// `dynamicDelayCompensationSetting` (Dynamic Latency Compensation).
    pub dynamic_delay_compensation: bool,
    /// `stompModeBypassAssignment`: auto-assign stomp footswitches.
    pub stomp_mode_auto_assign: bool,
    /// `presetReloadSettings` / `sceneReloadSettings`: re-pressing the active
    /// footswitch reloads it.
    pub preset_reload: bool,
    pub scene_reload: bool,
    pub bypass_save_mode: BypassSaveMode,
    /// `atmaPowerOnMode`: true = power on when power is connected.
    pub atma_power_on_automatic: bool,
    pub power_button_sensitivity: PowerButtonSensitivity,
    pub hold_timing: HoldTiming,
    /// `DisableConnectionCheck` (internet connection check).
    pub disable_connection_check: bool,
    pub dont_show_excess_preset_downloads: bool,
    pub dont_show_excess_capture_downloads: bool,
    /// `General/customName` (`[%General]`). `None` means the stock default
    /// "Neural DSP %1" with the product name; stock never stores `""`.
    pub custom_name: Option<String>,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            gig_view_stomp_access: false,
            swap_tempo_and_tuner_access: false,
            dynamic_delay_compensation: true,
            stomp_mode_auto_assign: false,
            preset_reload: false,
            scene_reload: false,
            bypass_save_mode: BypassSaveMode::Always,
            atma_power_on_automatic: true,
            power_button_sensitivity: PowerButtonSensitivity::Medium,
            hold_timing: HoldTiming::DEFAULT,
            disable_connection_check: false,
            dont_show_excess_preset_downloads: false,
            dont_show_excess_capture_downloads: false,
            custom_name: None,
        }
    }
}

impl GeneralSettings {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let d = Self::default();
        let bypass = r.string("bypassSaveMode", d.bypass_save_mode.stock_name());
        let power = r.int(
            "General/powerButtonSensitivity",
            d.power_button_sensitivity.to_stock(),
        );
        let hold = r.int("holdTiming", d.hold_timing.get());
        let custom_name = r.string("General/customName", "");
        Self {
            gig_view_stomp_access: r.bool("gigViewStompAccess", d.gig_view_stomp_access),
            swap_tempo_and_tuner_access: r
                .bool("swapTempoAndTunerAccess", d.swap_tempo_and_tuner_access),
            dynamic_delay_compensation: r.bool(
                "dynamicDelayCompensationSetting",
                d.dynamic_delay_compensation,
            ),
            stomp_mode_auto_assign: r.bool("stompModeBypassAssignment", d.stomp_mode_auto_assign),
            preset_reload: r.bool("presetReloadSettings", d.preset_reload),
            scene_reload: r.bool("sceneReloadSettings", d.scene_reload),
            // Stock ignores an unknown name and keeps its current mode.
            bypass_save_mode: r.check(
                "bypassSaveMode",
                BypassSaveMode::from_stock_name(&bypass),
                d.bypass_save_mode,
            ),
            atma_power_on_automatic: r.bool("atmaPowerOnMode", d.atma_power_on_automatic),
            power_button_sensitivity: r.check(
                "General/powerButtonSensitivity",
                PowerButtonSensitivity::from_stock(power),
                d.power_button_sensitivity,
            ),
            hold_timing: r.check("holdTiming", HoldTiming::new(hold), d.hold_timing),
            disable_connection_check: r.bool("DisableConnectionCheck", d.disable_connection_check),
            dont_show_excess_preset_downloads: r.bool(
                "dontShowExcessPresetDownloads",
                d.dont_show_excess_preset_downloads,
            ),
            dont_show_excess_capture_downloads: r.bool(
                "dontShowExcessCaptureDownloads",
                d.dont_show_excess_capture_downloads,
            ),
            custom_name: (!custom_name.is_empty()).then_some(custom_name),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.bool(
            "gigViewStompAccess",
            self.gig_view_stomp_access,
            old.gig_view_stomp_access,
        );
        w.bool(
            "swapTempoAndTunerAccess",
            self.swap_tempo_and_tuner_access,
            old.swap_tempo_and_tuner_access,
        );
        w.bool(
            "dynamicDelayCompensationSetting",
            self.dynamic_delay_compensation,
            old.dynamic_delay_compensation,
        );
        w.bool(
            "stompModeBypassAssignment",
            self.stomp_mode_auto_assign,
            old.stomp_mode_auto_assign,
        );
        w.bool(
            "presetReloadSettings",
            self.preset_reload,
            old.preset_reload,
        );
        w.bool("sceneReloadSettings", self.scene_reload, old.scene_reload);
        w.put(
            "bypassSaveMode",
            &self.bypass_save_mode,
            &old.bypass_save_mode,
            |m| Variant::String(m.stock_name().to_owned()),
        );
        w.bool(
            "atmaPowerOnMode",
            self.atma_power_on_automatic,
            old.atma_power_on_automatic,
        );
        w.put(
            "General/powerButtonSensitivity",
            &self.power_button_sensitivity,
            &old.power_button_sensitivity,
            |v| Variant::Int(v.to_stock()),
        );
        w.int("holdTiming", self.hold_timing.get(), old.hold_timing.get());
        w.bool(
            "DisableConnectionCheck",
            self.disable_connection_check,
            old.disable_connection_check,
        );
        w.bool(
            "dontShowExcessPresetDownloads",
            self.dont_show_excess_preset_downloads,
            old.dont_show_excess_preset_downloads,
        );
        w.bool(
            "dontShowExcessCaptureDownloads",
            self.dont_show_excess_capture_downloads,
            old.dont_show_excess_capture_downloads,
        );
        // Stock writes the default name instead of ""; removing the key
        // gives the same reading.
        if self.custom_name != old.custom_name {
            match &self.custom_name {
                Some(name) => w
                    .file
                    .set("General/customName", &Variant::String(name.clone())),
                None => w.file.remove("General/customName"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HoldTiming;

    #[test]
    fn hold_delay_steps_match_stock_table() {
        let delays: Vec<u32> = (0..=5)
            .map(|index| HoldTiming::new(index).map(HoldTiming::millis))
            .collect::<Result<_, _>>()
            .expect("hold timing indices 0..=5 are valid");
        assert_eq!(delays, [500, 600, 700, 800, 900, 1000]);
        assert_eq!(HoldTiming::DEFAULT.millis(), 800);
    }
}
