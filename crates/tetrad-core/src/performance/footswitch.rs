//! Footswitch identities and press/release actions per mode.
//!
//! Tables `0xa19238` (physical → logical) and `0xa19268` (logical → physical)
//! are exact inverses. The gesture slot `0x20eec4` maps each HAL stomp event
//! through the first table. Logical 0–7 are A–H, 8 is DOWN (HAL 4), 9 is TEMPO
//! (HAL 9) and 10 is UP (HAL 10). Stomp assignments store logical indices.
//! Dispatch: `0x20eec4` → `0x58b098` (hybrid halves) → `0x20d380` (per kind),
//! preset select `0x4a2960`, steps `0x4a2c80`/`0x5b57b8`. Bank index ↔ preset:
//! `0x49d534` (`size·bank + slot`) and `0x49d1bc` (`i / size`, `i % size`).

use super::mode::{FootswitchKind, FootswitchMode};
use super::stomp::StompIndex;
use crate::workflow::SceneIndex;

pub const PHYSICAL_TO_LOGICAL: [u8; 11] = [0, 1, 2, 3, 8, 4, 5, 6, 7, 9, 10];
pub const LOGICAL_TO_PHYSICAL: [u8; 11] = [0, 1, 2, 3, 5, 6, 7, 8, 4, 9, 10];

/// HAL mask of the mode chord: DOWN (HAL 4) and TEMPO (HAL 9) held together.
pub const MODE_CHORD_MASK: u16 = (1 << 4) | (1 << 9);

/// HAL (physical) stomp index; bit `1 << index` of the pressed mask
/// (`0xa4df60`). The QC TEMPO switch is HAL 9 (`0x20db38`; Atma uses 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HalStomp(u8);

impl HalStomp {
    pub const QC_TEMPO: Self = Self(9);

    pub fn new(index: u32) -> Option<Self> {
        (index < 11).then_some(Self(index as u8))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    pub fn mask(self) -> u16 {
        1 << self.0
    }
}

/// A logical footswitch index (0..=10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Footswitch(u8);

impl Footswitch {
    pub const DOWN: Self = Self(8);
    pub const TEMPO: Self = Self(9);
    pub const UP: Self = Self(10);

    pub fn from_physical(stomp: HalStomp) -> Self {
        Self(PHYSICAL_TO_LOGICAL[usize::from(stomp.get())])
    }

    pub fn logical(self) -> u8 {
        self.0
    }

    pub fn physical(self) -> HalStomp {
        HalStomp(LOGICAL_TO_PHYSICAL[usize::from(self.0)])
    }

    /// The footswitch of a stomp: same logical index (`0xa4e1e8` equals `0xa19268`).
    pub fn of_stomp(stomp: StompIndex) -> Self {
        Self(stomp.get())
    }

    /// One of the eight lettered footswitches A–H.
    pub fn letter(self) -> Option<u8> {
        (self.0 < 8).then_some(self.0)
    }

    /// `0x58b098` (QC): hybrid modes give A–D to the first kind and E–H to
    /// the second. Scene and stomp indices are not remapped on QC.
    pub fn kind_in(self, mode: FootswitchMode) -> Option<FootswitchKind> {
        let letter = self.letter()?;
        Some(match mode.kinds() {
            (single, None) => single,
            (first, Some(_)) if letter < 4 => first,
            (_, Some(second)) => second,
        })
    }
}

/// Presets per bank. QC 8, mini 4 (`0x00499a60`); half of that when the mode cycle
/// splits banks ([`BankSize::of_cycle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BankSize(u8);

impl BankSize {
    pub const QC: Self = Self(8);

    pub const fn new(mini: bool, split: bool) -> Self {
        Self(match (mini, split) {
            (false, false) => 8,
            (false, true) | (true, false) => 4,
            (true, true) => 2,
        })
    }

    /// `0x58b968`: banks split when **any** mode of the cycle (ModeController `+0xc`) is a
    /// hybrid with PRESET (modes 3, 4, 5, 7), not only the active mode.
    pub fn of_cycle(mini: bool, cycle: impl IntoIterator<Item = FootswitchMode>) -> Self {
        let split = cycle
            .into_iter()
            .any(|mode| mode.is_hybrid() && mode.includes(FootswitchKind::Preset));
        Self::new(mini, split)
    }

    pub const fn get(self) -> u32 {
        self.0 as u32
    }

    /// Bank of an absolute preset index.
    pub const fn bank(self, preset: u32) -> u32 {
        preset / self.get()
    }

    /// Absolute index of the bank's first preset.
    pub const fn first(self, bank: u32) -> u32 {
        bank.wrapping_mul(self.get())
    }
}

impl Default for BankSize {
    fn default() -> Self {
        Self::QC
    }
}

/// Bank overlay (NavigationBar preview) as seen by the footswitches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BankOverlay {
    /// Caller-validated overlay bank.
    pub bank: u32,
    /// Bank slot (offset from the bank start) of the previewed preset, if any.
    pub highlighted: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FootswitchContext {
    pub mode: FootswitchMode,
    /// Absolute index of the loaded preset.
    pub current_preset: u32,
    pub overlay: Option<BankOverlay>,
    /// ModeController `+0x14` scene copy/paste mode.
    pub scene_copy: bool,
    /// Gig View footswitch access setting; when on, the mode chord cycles on
    /// release (`0x20e4c0`) instead of on press.
    pub gig_view_access: bool,
    /// From the product and the mode cycle; the caller updates it when the cycle changes.
    pub bank_size: BankSize,
}

/// Releases still owed to a mode chord, as a bit mask of logical indices. The
/// caller keeps one per footswitch session and passes it to every press and
/// release.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChordReleases(u16);

impl ChordReleases {
    fn owe(&mut self, switch: Footswitch) {
        self.0 |= 1u16 << switch.logical();
    }

    fn settle(&mut self, switch: Footswitch) -> bool {
        let bit = 1u16 << switch.logical();
        let owed = self.0 & bit != 0;
        self.0 &= !bit;
        owed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FootswitchAction {
    /// Mode cycle on the DOWN+TEMPO chord (`0x4a2620` → `0x58cd0c`): on press
    /// without Gig View access, on release with it.
    CycleMode,
    /// TEMPO press while the bank overlay is active (`0x4a2d00`).
    CancelBankOverlay,
    /// Load an absolute preset index; a footswitch always reloads.
    LoadPreset(u32),
    /// First press in the overlay previews this absolute preset.
    PreviewPreset(u32),
    /// Enter the overlay if needed and step its bank.
    StepBank(i8),
    /// Pure scene/stomp modes: load `(current ± 1) & 0xff` directly.
    StepPreset(i8),
    /// Non-forced scene selection; the active scene is a no-op.
    SelectScene(SceneIndex),
    /// Scene copy mode: copy/swap into this scene, then force-select it.
    CopyScene(SceneIndex),
    StompPress(StompIndex),
    StompRelease(StompIndex),
}

/// A Cortex Control Gig View button (GigViewButton 55, `0x109ab4`) on the page of its tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GigButton {
    /// Release on a preset tile: a load that is not forced (`0x65ffdc`).
    LoadPreset(u32),
    /// Release on a scene tile: `setScene` (`0x6648f0`).
    SelectScene(SceneIndex),
    /// Press and release on a stomp tile (`0x661358`, `0x6613c4`).
    StompPress(StompIndex),
    StompRelease(StompIndex),
}

/// The other half of the DOWN/TEMPO mode chord.
fn chord_partner(switch: Footswitch) -> Option<Footswitch> {
    match switch {
        Footswitch::DOWN => Some(Footswitch::TEMPO),
        Footswitch::TEMPO => Some(Footswitch::DOWN),
        _ => None,
    }
}

impl FootswitchContext {
    /// A press (value ≥ 0.5). `held` is the HAL pressed mask after the event.
    /// TEMPO presses also feed [`super::tempo::TapTempo`].
    pub fn press(
        &self,
        switch: Footswitch,
        held: u16,
        chord: &mut ChordReleases,
    ) -> Option<FootswitchAction> {
        if held & MODE_CHORD_MASK == MODE_CHORD_MASK
            && matches!(switch, Footswitch::DOWN | Footswitch::TEMPO)
        {
            if self.gig_view_access {
                return None;
            }
            chord.owe(Footswitch::DOWN);
            chord.owe(Footswitch::TEMPO);
            return Some(FootswitchAction::CycleMode);
        }
        if switch == Footswitch::TEMPO {
            return self.overlay.map(|_| FootswitchAction::CancelBankOverlay);
        }
        let letter = switch.letter()?;
        if let Some(overlay) = self.overlay {
            return Some(self.overlay_select(overlay, letter));
        }
        Some(match switch.kind_in(self.mode)? {
            FootswitchKind::Preset => FootswitchAction::LoadPreset(self.slot_target(letter)),
            FootswitchKind::Scene => {
                let scene = SceneIndex::new(i32::from(letter)).expect("letters are 0..=7");
                if self.scene_copy {
                    FootswitchAction::CopyScene(scene)
                } else {
                    FootswitchAction::SelectScene(scene)
                }
            }
            FootswitchKind::Stomp => FootswitchAction::StompPress(stomp_of(letter)),
        })
    }

    /// A release. `held` is the HAL pressed mask after the event. UP/DOWN act
    /// here unless a long press already handled them.
    pub fn release(
        &self,
        switch: Footswitch,
        held: u16,
        long_pressed: bool,
        chord: &mut ChordReleases,
    ) -> Option<FootswitchAction> {
        if let Some(partner) = chord_partner(switch) {
            if chord.settle(switch) {
                return None;
            }
            if self.gig_view_access && held & partner.physical().mask() != 0 {
                chord.owe(partner);
                return Some(FootswitchAction::CycleMode);
            }
        }
        let step = match switch {
            Footswitch::UP => 1,
            Footswitch::DOWN => -1,
            _ => {
                let letter = switch.letter()?;
                return (self.overlay.is_none()
                    && switch.kind_in(self.mode) == Some(FootswitchKind::Stomp))
                .then(|| FootswitchAction::StompRelease(stomp_of(letter)));
            }
        };
        if long_pressed {
            return None;
        }
        Some(
            if self.overlay.is_some() || self.mode.includes(FootswitchKind::Preset) {
                FootswitchAction::StepBank(step)
            } else {
                FootswitchAction::StepPreset(step)
            },
        )
    }

    /// Long press of UP/DOWN (`0x20e054`): overlay bank step; release is then
    /// suppressed.
    pub fn long_press(&self, switch: Footswitch) -> Option<FootswitchAction> {
        match switch {
            Footswitch::UP => Some(FootswitchAction::StepBank(1)),
            Footswitch::DOWN => Some(FootswitchAction::StepBank(-1)),
            _ => None,
        }
    }

    /// Bank slot of a lettered switch. A hybrid preset half has four switches, so its
    /// slot is the letter within the half (`0x58b098`). Otherwise (also Scene/Stomp
    /// hybrids 6 and 8 in the overlay) the slot is the letter;
    /// with split banks in pure PRESET mode, E–H reach the next bank of four, as the Gig
    /// View fills its tiles with `bank·4 + pos` (`0x660c78`; footswitch path inferred).
    fn preset_slot(&self, letter: u8) -> u32 {
        if self.mode.is_hybrid() && self.mode.includes(FootswitchKind::Preset) {
            u32::from(letter % 4)
        } else {
            u32::from(letter)
        }
    }

    /// A Gig View button on `tile` (`0x109ab4`): the page is the tile's kind in the current
    /// mode (`0x5898ac`, hybrid split `0x2bfc2c` as [`Footswitch::kind_in`]). A preset or
    /// scene press and a release elsewhere do nothing. I: the bank overlay and scene copy
    /// mode are not consulted.
    pub fn gig_button(&self, tile: u8, pressed: bool) -> Option<GigButton> {
        let letter = Footswitch::of_stomp(StompIndex::new(u32::from(tile))?).letter()?;
        Some(match (Footswitch(letter).kind_in(self.mode)?, pressed) {
            (FootswitchKind::Preset, false) => GigButton::LoadPreset(self.slot_target(letter)),
            (FootswitchKind::Scene, false) => {
                GigButton::SelectScene(SceneIndex::new(i32::from(letter)).ok()?)
            }
            (FootswitchKind::Stomp, true) => GigButton::StompPress(stomp_of(letter)),
            (FootswitchKind::Stomp, false) => GigButton::StompRelease(stomp_of(letter)),
            _ => return None,
        })
    }

    /// `0x4a2960`: `slot + bank start of the current preset` (`current & !7` on QC).
    fn slot_target(&self, letter: u8) -> u32 {
        let size = self.bank_size;
        size.first(size.bank(self.current_preset)) + self.preset_slot(letter)
    }

    /// In the overlay every lettered switch selects a preset: the first press
    /// previews `bank·size + slot` (the caller keeps the overlay bank and highlights
    /// the slot), pressing the highlighted slot loads it.
    fn overlay_select(&self, overlay: BankOverlay, letter: u8) -> FootswitchAction {
        let slot = self.preset_slot(letter);
        let target = self.bank_size.first(overlay.bank) + slot;
        if overlay.highlighted.map(u32::from) == Some(slot) {
            FootswitchAction::LoadPreset(target)
        } else {
            FootswitchAction::PreviewPreset(target)
        }
    }
}

/// `0x5b57b8`: the direct preset step wraps within one byte.
pub fn stepped_preset(current: u32, step: i8) -> u32 {
    current.wrapping_add_signed(i32::from(step)) & 0xff
}

fn stomp_of(letter: u8) -> StompIndex {
    StompIndex::new(u32::from(letter)).expect("letters are valid stomp indices")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hal(index: u32) -> Footswitch {
        Footswitch::from_physical(HalStomp::new(index).unwrap())
    }

    fn ctx(mode: FootswitchMode) -> FootswitchContext {
        FootswitchContext {
            mode,
            current_preset: 0,
            overlay: None,
            scene_copy: false,
            gig_view_access: false,
            bank_size: BankSize::QC,
        }
    }

    #[test]
    fn identity_tables_are_inverse_and_name_up_down_tempo() {
        for physical in 0..11 {
            assert_eq!(u32::from(hal(physical).physical().get()), physical);
        }
        assert_eq!(hal(4), Footswitch::DOWN);
        assert_eq!(hal(9), Footswitch::TEMPO);
        assert_eq!(Footswitch::TEMPO.physical(), HalStomp::QC_TEMPO);
        assert_eq!(hal(10), Footswitch::UP);
        assert_eq!(hal(8).letter(), Some(7));
    }

    #[test]
    fn preset_mode_replays_the_in_binary_test_sequence() {
        // StompTest `0x438970` vectors: h2 → slot 2, h8 → slot 7, UP steps the bank.
        let mut chord = ChordReleases::default();
        let mut c = ctx(FootswitchMode::Preset);
        assert_eq!(
            c.press(hal(2), 1 << 2, &mut chord),
            Some(FootswitchAction::LoadPreset(2))
        );
        assert_eq!(
            c.press(hal(8), 1 << 8, &mut chord),
            Some(FootswitchAction::LoadPreset(7))
        );
        c.current_preset = 7;
        assert_eq!(
            c.release(hal(10), 0, false, &mut chord),
            Some(FootswitchAction::StepBank(1))
        );
        c.overlay = Some(BankOverlay {
            bank: 1,
            highlighted: None,
        });
        assert_eq!(
            c.press(hal(0), 1, &mut chord),
            Some(FootswitchAction::PreviewPreset(8))
        );
        c.overlay = Some(BankOverlay {
            bank: 1,
            highlighted: Some(0),
        });
        assert_eq!(
            c.press(hal(0), 1, &mut chord),
            Some(FootswitchAction::LoadPreset(8))
        );
        assert_eq!(
            c.press(hal(9), 1 << 9, &mut chord),
            Some(FootswitchAction::CancelBankOverlay)
        );
        assert_eq!(c.release(hal(4), 0, true, &mut chord), None);
    }

    #[test]
    fn bank_size_splits_only_when_the_cycle_holds_a_hybrid_with_preset() {
        use FootswitchMode::*;
        // `0x58b968`: modes 3, 4, 5, 7 anywhere in the cycle; 6 and 8 keep full banks.
        for mode in [PresetScene, PresetStomp, ScenePreset, StompPreset] {
            assert_eq!(BankSize::of_cycle(false, [Preset, mode]).get(), 4);
        }
        assert_eq!(
            BankSize::of_cycle(false, [Preset, Scene, SceneStomp, StompScene]).get(),
            8
        );
        assert_eq!(BankSize::of_cycle(true, [Preset, Scene]).get(), 4);
        assert_eq!(BankSize::of_cycle(true, [Scene, PresetScene]).get(), 2);
    }

    #[test]
    fn split_banks_target_bank_of_four_slots_in_each_mode_family() {
        let mut chord = ChordReleases::default();
        let split = |mode| FootswitchContext {
            current_preset: 9,
            bank_size: BankSize::new(false, true),
            ..ctx(mode)
        };
        let load = |index| Some(FootswitchAction::LoadPreset(index));
        // Pure PRESET with a split cycle: bank 2 starts at 8; E–H reach the next four (I).
        let preset = split(FootswitchMode::Preset);
        assert_eq!(preset.press(hal(0), 1, &mut chord), load(8));
        assert_eq!(preset.press(hal(5), 1 << 5, &mut chord), load(12));
        // Preset first (A–D) or second (E–H): the slot is the letter within the half.
        let first = split(FootswitchMode::PresetScene);
        assert_eq!(first.press(hal(3), 1 << 3, &mut chord), load(11));
        let scene = SceneIndex::new(4).unwrap();
        assert_eq!(
            first.press(hal(5), 1 << 5, &mut chord),
            Some(FootswitchAction::SelectScene(scene))
        );
        let second = split(FootswitchMode::StompPreset);
        assert_eq!(second.press(hal(5), 1 << 5, &mut chord), load(8));
        assert_eq!(second.press(hal(8), 1 << 8, &mut chord), load(11));
        let overlay = FootswitchContext {
            overlay: Some(BankOverlay {
                bank: 3,
                highlighted: None,
            }),
            ..first
        };
        assert_eq!(
            overlay.press(hal(6), 1 << 6, &mut chord),
            Some(FootswitchAction::PreviewPreset(13))
        );
        let armed = FootswitchContext {
            overlay: Some(BankOverlay {
                bank: 3,
                highlighted: Some(1),
            }),
            ..first
        };
        assert_eq!(armed.press(hal(6), 1 << 6, &mut chord), load(13));
        // Scene/Stomp hybrids (6, 8) keep banks of eight and full overlay slots.
        let stomp_scene = FootswitchContext {
            overlay: Some(BankOverlay {
                bank: 1,
                highlighted: None,
            }),
            ..ctx(FootswitchMode::SceneStomp)
        };
        assert_eq!(
            stomp_scene.press(hal(6), 1 << 6, &mut chord),
            Some(FootswitchAction::PreviewPreset(13))
        );
    }

    #[test]
    fn overlay_steps_the_bank_on_short_release_in_any_mode() {
        let mut chord = ChordReleases::default();
        let mut scene = ctx(FootswitchMode::Scene);
        scene.overlay = Some(BankOverlay {
            bank: 2,
            highlighted: None,
        });
        assert_eq!(
            scene.release(hal(10), 0, false, &mut chord),
            Some(FootswitchAction::StepBank(1))
        );
        assert_eq!(
            scene.release(hal(4), 0, false, &mut chord),
            Some(FootswitchAction::StepBank(-1))
        );
    }

    #[test]
    fn mode_chord_cycles_on_press_and_then_ignores_both_releases() {
        let mut chord = ChordReleases::default();
        let scene = ctx(FootswitchMode::Scene);
        assert_eq!(
            scene.press(hal(9), MODE_CHORD_MASK, &mut chord),
            Some(FootswitchAction::CycleMode)
        );
        assert_eq!(scene.release(hal(4), 1 << 9, false, &mut chord), None);
        assert_eq!(scene.release(hal(9), 0, false, &mut chord), None);
        assert_eq!(
            scene.release(hal(4), 0, false, &mut chord),
            Some(FootswitchAction::StepPreset(-1))
        );
    }

    #[test]
    fn gig_view_access_cycles_on_release_while_the_partner_is_held() {
        let mut chord = ChordReleases::default();
        let gig = FootswitchContext {
            gig_view_access: true,
            ..ctx(FootswitchMode::Scene)
        };
        assert_eq!(gig.press(hal(9), MODE_CHORD_MASK, &mut chord), None);
        assert_eq!(
            gig.release(hal(4), 1 << 9, false, &mut chord),
            Some(FootswitchAction::CycleMode)
        );
        assert_eq!(gig.release(hal(9), 0, false, &mut chord), None);
        assert_eq!(
            gig.release(hal(9), 0, false, &mut chord),
            None,
            "a later TEMPO release is a plain release, not a chord"
        );

        let mut chord = ChordReleases::default();
        assert_eq!(gig.press(hal(4), MODE_CHORD_MASK, &mut chord), None);
        assert_eq!(
            gig.release(hal(9), 1 << 4, false, &mut chord),
            Some(FootswitchAction::CycleMode)
        );
        assert_eq!(gig.release(hal(4), 0, false, &mut chord), None);
    }

    #[test]
    fn scene_stomp_and_hybrid_halves_dispatch_by_letter() {
        let mut chord = ChordReleases::default();
        let scene = ctx(FootswitchMode::Scene);
        let pick = |i| Some(FootswitchAction::SelectScene(SceneIndex::new(i).unwrap()));
        assert_eq!(scene.press(hal(8), 1 << 8, &mut chord), pick(7));
        assert_eq!(
            scene.release(hal(10), 0, false, &mut chord),
            Some(FootswitchAction::StepPreset(1))
        );
        assert_eq!(stepped_preset(0, -1), 255);
        let stomp = ctx(FootswitchMode::Stomp);
        let s0 = StompIndex::new(0).unwrap();
        assert_eq!(
            stomp.press(hal(0), 1, &mut chord),
            Some(FootswitchAction::StompPress(s0))
        );
        assert_eq!(
            stomp.release(hal(0), 0, false, &mut chord),
            Some(FootswitchAction::StompRelease(s0))
        );
        // testQCHybridMode: mode 8 Stomp/Scene → h5 is scene 4, h0 a stomp.
        let hybrid = ctx(FootswitchMode::StompScene);
        assert_eq!(hybrid.press(hal(5), 1 << 5, &mut chord), pick(4));
        assert_eq!(
            hybrid.press(hal(0), 1, &mut chord),
            Some(FootswitchAction::StompPress(s0))
        );
    }
}
