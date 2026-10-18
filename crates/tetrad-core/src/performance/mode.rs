//! Footswitch mode meanings for the numeric `ModeId`.
//!
//! `ModeButton::updateMode` (`0x674ed0`) switches on the mode: 0–2 show one
//! label/icon, 3–8 show two icons, and other values log
//! `"ModeButton::updateMode():Unallowed mode."`.

use crate::workflow::ModeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FootswitchKind {
    Preset,
    Scene,
    Stomp,
}

/// Stock mode numbers. Hybrid names keep the stock icon order; which footswitch
/// row each half controls is a renderer/HAL fact, not established here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FootswitchMode {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("mode {0} passes the 0..=9 validator but ModeButton treats it as unallowed")]
pub struct UnallowedMode(pub u8);

impl TryFrom<ModeId> for FootswitchMode {
    type Error = UnallowedMode;

    fn try_from(mode: ModeId) -> Result<Self, UnallowedMode> {
        Ok(match mode.get() {
            0 => Self::Preset,
            1 => Self::Scene,
            2 => Self::Stomp,
            3 => Self::PresetScene,
            4 => Self::PresetStomp,
            5 => Self::ScenePreset,
            6 => Self::SceneStomp,
            7 => Self::StompPreset,
            8 => Self::StompScene,
            other => return Err(UnallowedMode(other)),
        })
    }
}

impl FootswitchMode {
    pub fn id(self) -> ModeId {
        ModeId::new(self as u8).expect("stock mode numbers are below 10")
    }

    /// First kind, then the second kind of a hybrid mode.
    pub fn kinds(self) -> (FootswitchKind, Option<FootswitchKind>) {
        use FootswitchKind::{Preset, Scene, Stomp};
        match self {
            Self::Preset => (Preset, None),
            Self::Scene => (Scene, None),
            Self::Stomp => (Stomp, None),
            Self::PresetScene => (Preset, Some(Scene)),
            Self::PresetStomp => (Preset, Some(Stomp)),
            Self::ScenePreset => (Scene, Some(Preset)),
            Self::SceneStomp => (Scene, Some(Stomp)),
            Self::StompPreset => (Stomp, Some(Preset)),
            Self::StompScene => (Stomp, Some(Scene)),
        }
    }

    pub fn is_hybrid(self) -> bool {
        self.kinds().1.is_some()
    }

    pub fn includes(self, kind: FootswitchKind) -> bool {
        let (first, second) = self.kinds();
        first == kind || second == Some(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_mode_numbers_round_trip_and_nine_is_unallowed() {
        for value in 0..=8 {
            let mode = FootswitchMode::try_from(ModeId::new(value).unwrap()).unwrap();
            assert_eq!(mode.id().get(), value);
            assert_eq!(mode.is_hybrid(), value >= 3);
        }
        let mode = FootswitchMode::try_from(ModeId::new(6).unwrap()).unwrap();
        assert_eq!(
            mode.kinds(),
            (FootswitchKind::Scene, Some(FootswitchKind::Stomp))
        );
        assert_eq!(
            FootswitchMode::try_from(ModeId::new(9).unwrap()),
            Err(UnallowedMode(9))
        );
    }
}
