//! LED bridge contracts: the bank overlay blink timing and the Settings all-off view.

use std::time::Duration;

use tetrad_app::device::leds::{BLINK_PHASE, Leds};
use tetrad_app::device::{Effect, State};
use tetrad_core::performance::footswitch::BankOverlay;
use tetrad_core::performance::leds::PALETTE;
use tetrad_hal::hal::Command;
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_hal::requests::ElementLed;
use tetrad_hal::zencoder::led_output;
use tetrad_settings::device::{DeviceSettings, Hardware};
use tetrad_settings::{SettingsFileId, SettingsStorage};
use tetrad_ui::{Screen, SettingsPage};

struct NoFiles;

impl SettingsStorage for NoFiles {
    fn read(&self, _: SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn write(&mut self, _: SettingsFileId, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn remove(&mut self, _: SettingsFileId) -> std::io::Result<()> {
        Ok(())
    }
}

fn qc() -> State {
    let (settings, _) = DeviceSettings::load(&NoFiles, Hardware::QuadCortex).unwrap();
    State::new(Product::QuadCortex, HalPlatform::QcOriginal, settings)
}

fn footswitch_writes(effects: Vec<Effect>) -> Vec<ElementLed> {
    effects
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Hal(Command::ElementLed(led)) => Some(led),
            _ => None,
        })
        .collect()
}

/// 500 ms per phase, starting dim; a bank step keeps the phase; closing stops the timer.
#[test]
fn overlay_blink_starts_dim_alternates_every_500ms_and_survives_a_bank_step() {
    let mut state = qc();
    let mut leds = Leds::default();
    leds.update(&state);
    let open_at = Duration::from_millis(1_234);
    state.now = open_at;
    state.performance.footswitch.overlay = Some(BankOverlay {
        bank: 0,
        highlighted: Some(2),
    });
    // Preset mode: slot 2 is hardware 2.
    let target = |effects| {
        footswitch_writes(effects)
            .into_iter()
            .find(|led| led.index == 2)
            .map(|led| led.opaque)
    };
    let dim = led_output(PALETTE[2], 0.3).1;
    let bright = led_output(PALETTE[2], 1.0).1;
    assert_ne!(dim, bright);
    assert_eq!(target(leds.update(&state)), Some(dim));
    assert_eq!(leds.blink_deadline(state.now), Some(open_at + BLINK_PHASE));
    state.now = open_at + BLINK_PHASE;
    assert_eq!(target(leds.update(&state)), Some(bright));
    assert_eq!(
        leds.blink_deadline(state.now),
        Some(open_at + BLINK_PHASE * 2)
    );
    // A bank step 200 ms into the bright phase does not restart the animation.
    state.now += Duration::from_millis(200);
    state.performance.footswitch.overlay = Some(BankOverlay {
        bank: 1,
        highlighted: Some(2),
    });
    assert_eq!(target(leds.update(&state)), None);
    state.now = open_at + BLINK_PHASE * 2;
    assert_eq!(target(leds.update(&state)), Some(dim));
    state.performance.footswitch.overlay = None;
    leds.update(&state);
    assert_eq!(leds.blink_deadline(state.now), None);
}

/// Settings turns every LED off, except the Brightness page, which holds the last frame.
#[test]
fn settings_turns_all_leds_off_but_brightness_page_holds_the_frame() {
    let mut state = qc();
    let mut leds = Leds::default();
    leds.update(&state);
    state.screen = Screen::Settings(SettingsPage::Brightness);
    assert_eq!(footswitch_writes(leds.update(&state)), []);
    state.screen = Screen::Settings(SettingsPage::GlobalBypass);
    let writes = footswitch_writes(leds.update(&state));
    assert!(!writes.is_empty());
    assert!(writes.iter().all(|led| led.opaque == 0));
}
