//! Footswitch mode (`[Mode]`), looper footswitch map (`[LooperSettings]`)
//! and the Atma master volume (`[Atma]`).

use crate::typed::{KeyReader, KeyWriter, SettingError, stock_enum};
use crate::{Hardware, Variant};

stock_enum!(
    /// `Mode/mode`. Hybrids put two modes on the two footswitch rows (row
    /// order inferred); 9 is invalid.
    FootswitchMode {
        Preset = 0,
        Scene = 1,
        Stomp = 2,
        PresetScene = 3,
        PresetStomp = 4,
        ScenePreset = 5,
        SceneStomp = 6,
        StompPreset = 7,
        StompScene = 8,
    }
);

impl FootswitchMode {
    /// Slots a mode takes in the available list (hybrids take two).
    pub const fn slots(self) -> usize {
        if self.to_stock() >= 3 { 2 } else { 1 }
    }
}

/// ModeController state. Load `0x0058d2ec` / `0x0058d0a4`, save
/// `0x0058c738` / `0x0058c1f4`, list validator `0x0058bcbc`.
#[derive(Clone, Debug, PartialEq)]
pub struct ModeSettings {
    mode: FootswitchMode,
    available: Vec<FootswitchMode>,
    /// `Mode/atmaPage`: Atma UI page (group inferred).
    pub atma_page: i32,
}

impl Default for ModeSettings {
    fn default() -> Self {
        Self {
            mode: FootswitchMode::Preset,
            available: Self::DEFAULT_AVAILABLE.to_vec(),
            atma_page: 0,
        }
    }
}

impl ModeSettings {
    /// `DAT_0155d408`. The list maps to MIDI CC#47 values 0, 1, 2.
    pub const DEFAULT_AVAILABLE: [FootswitchMode; 3] = [
        FootswitchMode::Preset,
        FootswitchMode::Scene,
        FootswitchMode::Stomp,
    ];
    pub const MAX_SLOTS: usize = 3;

    pub fn mode(&self) -> FootswitchMode {
        self.mode
    }

    pub fn available(&self) -> &[FootswitchMode] {
        &self.available
    }

    pub fn set_mode(&mut self, mode: FootswitchMode) -> Result<(), SettingError> {
        if !self.available.contains(&mode) {
            return Err(SettingError::UnknownValue {
                name: "mode (not available)",
                value: format!("{mode:?}"),
            });
        }
        self.mode = mode;
        Ok(())
    }

    /// Sets the list; the current mode falls back to the first entry when it
    /// is no longer available.
    pub fn set_available(&mut self, modes: Vec<FootswitchMode>) -> Result<(), SettingError> {
        let slots: usize = modes.iter().map(|m| m.slots()).sum();
        if modes.is_empty() || slots > Self::MAX_SLOTS {
            return Err(SettingError::OutOfRange {
                name: "availableModes slots",
                value: slots as f64,
                min: 1.0,
                max: Self::MAX_SLOTS as f64,
            });
        }
        if !modes.contains(&self.mode) {
            self.mode = modes[0];
        }
        self.available = modes;
        Ok(())
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut settings = Self {
            atma_page: r.int("Mode/atmaPage", 0),
            ..Self::default()
        };
        if r.file.contains("Mode/availableModes") {
            let raw = r.int_list("Mode/availableModes");
            let modes: Result<Vec<_>, _> =
                raw.into_iter().map(FootswitchMode::from_stock).collect();
            let result = modes.and_then(|m| settings.set_available(m));
            r.check("Mode/availableModes", result, ());
        }
        let mode = r.int("Mode/mode", 0);
        let result = FootswitchMode::from_stock(mode).and_then(|m| settings.set_mode(m));
        // Stock falls back to the first available mode.
        if let Err(e) = result {
            r.issue("Mode/mode", crate::LoadIssueKind::Invalid(e));
            settings.mode = settings.available[0];
        }
        settings
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.put("Mode/mode", &self.mode, &old.mode, |m| {
            Variant::Int(m.to_stock())
        });
        w.put(
            "Mode/availableModes",
            &self.available,
            &old.available,
            |list| Variant::List(list.iter().map(|m| Variant::Int(m.to_stock())).collect()),
        );
        w.int("Mode/atmaPage", self.atma_page, old.atma_page);
    }
}

/// `[LooperSettings]`: `stompAssignments0..7` map looper footswitch i to a
/// looper action param. Must be a permutation of the hardware default set
/// (`0x006524b8`), otherwise stock resets it. `lockMode` is read only.
/// Load `0x00653d18`, save `0x00651fa4`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LooperSettings {
    stomp_assignments: [i32; 8],
    pub lock_mode: bool,
    hardware: Hardware,
}

impl LooperSettings {
    /// Rodata `0x011e9448` (Quad Cortex) and `0x011e9428` (Atma).
    pub const fn default_assignments(hardware: Hardware) -> [i32; 8] {
        match hardware {
            Hardware::QuadCortex => [3, 4, 5, 6, 0, 1, 7, 2],
            Hardware::Atma => [6, 2, 0, 1, 3, 4, 5, 7],
        }
    }

    pub fn new(hardware: Hardware) -> Self {
        Self {
            stomp_assignments: Self::default_assignments(hardware),
            lock_mode: false,
            hardware,
        }
    }

    pub fn stomp_assignments(&self) -> [i32; 8] {
        self.stomp_assignments
    }

    pub fn set_stomp_assignments(&mut self, assignments: [i32; 8]) -> Result<(), SettingError> {
        let mut sorted = assignments;
        sorted.sort_unstable();
        let mut expected = Self::default_assignments(self.hardware);
        expected.sort_unstable();
        if sorted != expected {
            return Err(SettingError::UnknownValue {
                name: "stompAssignments",
                value: format!("{assignments:?}"),
            });
        }
        self.stomp_assignments = assignments;
        Ok(())
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let mut looper = Self::new(r.hardware);
        looper.lock_mode = r.bool("LooperSettings/lockMode", false);
        let mut assignments = looper.stomp_assignments;
        for (i, slot) in assignments.iter_mut().enumerate() {
            *slot = r.int(&format!("LooperSettings/stompAssignments{i}"), *slot);
        }
        let result = looper.set_stomp_assignments(assignments);
        r.check("LooperSettings/stompAssignments", result, ());
        looper
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        for (i, (new, old)) in self
            .stomp_assignments
            .iter()
            .zip(&old.stomp_assignments)
            .enumerate()
        {
            w.int(&format!("LooperSettings/stompAssignments{i}"), *new, *old);
        }
    }
}

impl Default for LooperSettings {
    fn default() -> Self {
        Self::new(Hardware::QuadCortex)
    }
}

/// `Atma/AtmaVolume`: master volume on Atma only, 0..=1 in 0.01 steps
/// (`0x001fbb7c`), default 0.25. Save `0x0064e754`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmaVolume(f32);

impl Default for AtmaVolume {
    fn default() -> Self {
        Self(0.25)
    }
}

impl AtmaVolume {
    pub fn new(value: f32) -> Result<Self, SettingError> {
        if !(0.0..=1.0).contains(&value) {
            return Err(SettingError::OutOfRange {
                name: "AtmaVolume",
                value: f64::from(value),
                min: 0.0,
                max: 1.0,
            });
        }
        Ok(Self((value * 100.0).round() / 100.0))
    }

    pub fn get(self) -> f32 {
        self.0
    }

    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let raw = r.f32("Atma/AtmaVolume", 0.25);
        r.check("Atma/AtmaVolume", Self::new(raw), Self::default())
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.f32("Atma/AtmaVolume", self.0, old.0);
    }
}
