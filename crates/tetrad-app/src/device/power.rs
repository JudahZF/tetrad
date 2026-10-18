//! The power paths (shutdown, restart) and the reset steps.

use tetrad_dsp_host::session::Clock;
use tetrad_hal::hal::{Command as HalCommand, Hal};
use tetrad_hal::zenjack::{Control, HeadphoneVolume, Setting, Value};
use tetrad_settings::device::{ResetEffect, ResetKind};
use tetrad_watchdog::power::{
    self, DspPowerCommand, Policy, PowerBackend, PowerReport, PowerStep, StepStatus,
};

use super::platform::{Platform, ServiceBackends};
use super::{
    Device, DeviceError, DspCommand, FACTORY_SETLIST, PowerAction, PresetRef, Subsystem, Timer,
    jacks, leds, licence_repush, services,
};

impl<P: Platform> Device<P> {
    /// Reset work outside the settings files (`ResetKind::effects`), then the Cortex Control
    /// resync (`0x29a4f8`): a factory reset sends the file listing and CloudLogin DELETE
    /// (`0x4de79c`), a settings reset sends neither (w2b-control M5).
    pub(super) fn reset_steps(&mut self, kind: ResetKind) {
        for &step in kind.effects() {
            match step {
                ResetEffect::ResetDatabase => {
                    let database = self.recreate_database();
                    self.report(database);
                    let wiped = (self.state.library.as_mut())
                        .ok_or(DeviceError::Unavailable("the preset library"))
                        .and_then(|library| Ok(library.presets.factory_reset()?));
                    if let Some(stale) = self.report(wiped).and_then(|saved| saved.search_index) {
                        self.notice(Subsystem::Library, stale.to_string());
                    }
                    self.refresh_directory_media();
                }
                // `0x005b37f0`: preset 0 of the Factory Library, after the clear of every undo
                // frame (`0x4dea3c`, C). It calls `0x5b2f40` directly: no load frame (C).
                ResetEffect::LoadFactoryPreset => {
                    self.state.history.clear_all();
                    let factory = PresetRef {
                        setlist: Some(FACTORY_SETLIST.into()),
                        index: 0,
                    };
                    let loaded = self.load_preset(&factory, false);
                    self.report(loaded);
                }
                // `0x001f0c50`: the plugin blocks lose their licences at once.
                ResetEffect::ClearLicences => {
                    let before = std::mem::take(&mut self.state.licences);
                    if let Some(dsp) = self.dsp.as_mut() {
                        dsp.set_licences(self.state.licences);
                    }
                    let mut effects = vec![self.bridges.services.clear_licences()];
                    effects.extend(licence_repush(&self.state, &before, &[]));
                    self.apply(Ok::<_, DeviceError>(effects));
                }
                ResetEffect::RemoveRecentSearches => {
                    let cleared = (self.state.library.as_mut())
                        .ok_or(DeviceError::Unavailable("the preset library"))
                        .and_then(|library| Ok(library.presets.clear_recent_searches()?));
                    self.report(cleared);
                }
                // `0x308250`: unpair, then `user/signout`. The networks wait for it (F1).
                ResetEffect::CloudUnpair => {
                    let now = self.clock.now();
                    let unpaired = self.bridges.services.unpair(
                        self.platform.services(),
                        &mut self.state.settings,
                        services::AfterUnpair::SignOut,
                        now,
                    );
                    self.apply(unpaired);
                }
                ResetEffect::RemoveWifiNetworks => {
                    let forgotten = (self.bridges.services)
                        .forget_all(self.platform.services())
                        .map_err(DeviceError::from);
                    self.report(forgotten);
                }
                ResetEffect::ResetSystemFlags
                | ResetEffect::RemoveDebugFiles
                | ResetEffect::ResetBootCount
                | ResetEffect::ClearNativeConf => {
                    let files = self.platform.services().files();
                    let done = services::reset_files(files, step).map_err(DeviceError::from);
                    self.report(done);
                }
            }
        }
        let now = self.clock.now();
        let resync = match kind {
            ResetKind::Factory => self.bridges.control.factory_resync(&mut self.state, now),
            ResetKind::Settings => self.bridges.control.resync(false, &mut self.state, now),
        };
        self.apply(resync);
    }

    /// Stock power paths (o-lifecycle §5): DSP `0x20` value 0 (stock "mute first"; an unmute
    /// if bit 0 = mute, see `dsp::DEPLOY_MUTE`, inferred until bring-up S4a), headphones 0,
    /// then the teardown. Settings are flushed and `zenui.db` closed when the teardown starts.
    /// [`Policy::Safe`] remounts read-only when the unmount fails. There is no Wi-Fi step:
    /// SHUTDOWN `0x6a87d4` and REBOOT `0x6a8874` go from the mute straight to `0x66a208` /
    /// `0x66b0cc`; the Wi-Fi teardown `0x889f0c` runs only from the Wi-Fi object's own
    /// bring-up failure slots (`0x88df3c`, `0x88e1b8`, `0x892dcc`, `0x8931b8`).
    pub fn power(mut self, action: PowerAction) -> Result<PowerReport, DeviceError> {
        // The queued Cortex Control reports go out first, as when each report was sent at
        // once (owned: stock leaves its sender deques to the event loop).
        self.send_control_reports(usize::MAX);
        // An I/O edit inside its 250 ms save window is stored before the flush (owned).
        if self.timers.remove(&Timer::ConnectorsSave).is_some() {
            for effect in jacks::save(&mut self.state) {
                self.execute(effect);
            }
        }
        let product = tetrad_watchdog::cmdline::Product::from(self.state.platform);
        let steps = match action {
            PowerAction::PowerOff => power::power_off(),
            PowerAction::Restart(reason) => power::restart(reason, product),
        };
        power::execute(&steps, Policy::Safe, &mut self).map_err(|abort| DeviceError::PowerAborted {
            step: abort.step,
            source: Box::new(abort.error),
            report: abort.report,
        })
    }

    /// Steps that need application state: mute, delays and the mini LEDs.
    pub(super) fn power_step(&mut self, step: PowerStep) -> Result<StepStatus, DeviceError> {
        let done = |result: Result<(), DeviceError>, device: &mut Self| match result {
            Ok(()) => StepStatus::Success,
            Err(error) => {
                device.notice(Subsystem::Power, format!("{step:?}: {error}"));
                StepStatus::Failure
            }
        };
        Ok(match step {
            PowerStep::Dsp(DspPowerCommand::PowerFlags(flags)) => {
                let result = self.dsp_command(DspCommand::MasterMute(flags.into()));
                done(result, self)
            }
            PowerStep::Dsp(DspPowerCommand::MiniRestart) => done(
                Err(DeviceError::Unavailable("mini restart command 0x5a")),
                self,
            ),
            PowerStep::SurfaceLedsOff => {
                let hal = self.platform.hal();
                let result = leds::surface_off().try_for_each(|command| hal.apply(command));
                done(result.map_err(DeviceError::from), self)
            }
            PowerStep::MuteHeadphones => {
                // `SetHeadPhoneVolume(0.0, true, false, 3, false, 25000)`: mute writes word 100
                // and `false` skips the read-back (libzenhal `0x1b158`), as standby does.
                let setting = Setting::new(
                    Control::HeadphoneVolume,
                    Value::HeadphoneVolume(HeadphoneVolume::Muted),
                )
                .map_err(tetrad_hal::hal::HalError::from)?;
                let result = self
                    .platform
                    .hal()
                    .apply(HalCommand::Set(
                        setting,
                        tetrad_hal::analog::Verification::Unverified,
                    ))
                    .map_err(DeviceError::from);
                done(result, self)
            }
            PowerStep::Delay(duration) => {
                self.clock.clone().sleep(duration);
                StepStatus::Success
            }
            PowerStep::StopLogPipelines => {
                let flushed = self.flush_settings();
                let status = done(flushed.map_err(DeviceError::from), self);
                if let Some(library) = self.state.library.as_mut() {
                    library.database = None;
                }
                let system = self.system_power_step(step)?;
                if status == StepStatus::Failure {
                    status
                } else {
                    system
                }
            }
            step => self.system_power_step(step)?,
        })
    }

    fn system_power_step(&mut self, step: PowerStep) -> Result<StepStatus, DeviceError> {
        self.platform
            .power()
            .perform(step)
            .map_err(|error| DeviceError::Platform(error.to_string().into()))
    }
}

impl<P: Platform> PowerBackend for Device<P> {
    type Error = DeviceError;

    fn perform(&mut self, step: PowerStep) -> Result<StepStatus, DeviceError> {
        self.power_step(step)
    }
}
