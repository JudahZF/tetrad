//! Boot steps of [`Device::start_with`]: library, DSP, globals and identity.

use tetrad_dsp_host::session::SendPolicy;
use tetrad_dsp_host::transport::ControlPacket;
use tetrad_hal::hal::{Board, Hal, Query, Reading};
use tetrad_hal::platform::{Platform as HalPlatform, Product};
use tetrad_library::stock::{StockLibrary, StockLimits};
use tetrad_loader::plan::Variant;
use tetrad_storage::stock::{DbLimits, StockDb};

use super::platform::{DspFile, Marker, Platform};
use super::{
    Device, DeviceError, DspCommand, Library, Subsystem, control, dsp, features, global_eq, media,
    owned, preset,
};

impl<P: Platform> Device<P> {
    /// `StockLibrary` on `/media/p4` and its search index (`0x0073545c`), then `zenui.db`,
    /// its boot maintenance (`0x006a0598`, `0x006a077c`) and the legacy model-default
    /// import (`0x006a4140`). Index and import failures are notices; the library still opens.
    /// A bring-up run without `--persist` skips the index, the maintenance and the import and
    /// opens `zenui.db` read-only.
    pub(super) fn open_library(&mut self) -> Result<(), DeviceError> {
        let persist = self.options.persist;
        let roots = self.platform.storage();
        let factory_db = roots.factory.host().join("factory.db");
        let mut presets =
            StockLibrary::open(roots.user, StockLimits::default())?.with_factory(roots.factory);
        if persist && let Err(error) = presets.prepare_search_index() {
            self.notice(Subsystem::Library, format!("search index: {error}"));
        }
        // r16-media: the stock patch chain on a database below 410.1 (refused without --persist).
        if let Some(detail) = media::upgrade_database(&roots.database, &self.options) {
            self.notice(Subsystem::Library, detail);
        }
        let database =
            StockDb::open(&roots.database, DbLimits::default(), !persist).and_then(|mut db| {
                if persist {
                    db.transaction(|tx| tx.boot_maintenance())?;
                }
                Ok(db)
            });
        let database = match database {
            Ok(mut db) => {
                if persist {
                    self.import_legacy_model_defaults(&mut db);
                }
                Some(db)
            }
            Err(error) => {
                self.notice(Subsystem::Library, format!("zenui.db: {error}"));
                None
            }
        };
        self.state.library = Some(Library { presets, database });
        // `0x008328a4`: `factory.db` read-only (factory captures, Virtual Device Presets).
        match StockDb::open_factory(&factory_db, DbLimits::default()) {
            Ok(factory) => self.bridges.ui.set_factory_presets(factory),
            Err(error) => self.notice(Subsystem::Library, format!("factory.db: {error}")),
        }
        // Cortex Control Grid block sets read factory presets too (`0x1f78f4`).
        if let Ok(factory) = StockDb::open_factory(&factory_db, DbLimits::default()) {
            self.bridges.control.set_factory_presets(factory);
        }
        Ok(())
    }

    /// Factory reset of `zenui.db`: close (`0x0069daac`), remove the file (`0x006a551c`), then
    /// open a new one (`0x006a576c`: the patch chain on an empty database) with the boot
    /// maintenance (user roots, `0x0076f674` → `0x00834e10`). A failure leaves no database
    /// open, as a failed boot open does.
    pub(super) fn recreate_database(&mut self) -> Result<(), DeviceError> {
        let path = self.platform.storage().database;
        let library =
            (self.state.library.as_mut()).ok_or(DeviceError::Unavailable("the preset library"))?;
        library.database = None;
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(tetrad_storage::Error::from(error).into());
            }
            _ => {}
        }
        let mut database = StockDb::create(&path, DbLimits::default())?;
        database.transaction(|tx| tx.boot_maintenance())?;
        library.database = Some(database);
        Ok(())
    }

    /// `0x006a4140` runs when the user database major is not 300; a writable `StockDb` is
    /// 410.1, so it always does. The groups leave `ZenjackConfig.ini` only after the import
    /// succeeded; on failure they stay for the next boot.
    pub(super) fn import_legacy_model_defaults(&mut self, db: &mut StockDb) {
        let legacy = self.state.settings.legacy_model_defaults();
        if legacy.is_empty() {
            return;
        }
        match tetrad_library::stock::import_legacy_model_defaults(db, &legacy) {
            Ok(()) => {
                self.state.settings.remove_legacy_model_defaults(&legacy);
                self.schedule_settings_flush();
            }
            Err(error) => self.notice(
                Subsystem::Library,
                format!("legacy model defaults kept for the next boot: {error}"),
            ),
        }
    }

    /// SHARC boot, SOC2, keys, the catalog and the Cortex Control ModelRepo payload (without
    /// it the Control handshake never completes). A missing key leaves the catalog empty.
    pub(super) fn boot_dsp(&mut self) -> Result<(), DeviceError> {
        // `-a QC` when `0x49c14c` is nonzero, else `Atma` (a-boot §2).
        let variant = if self.state.platform.uses_midi_uart() {
            Variant::QuadCortex
        } else {
            Variant::Atma
        };
        let booted = dsp::boot(&mut self.platform, variant)?;
        if booted.soc1_retried {
            self.notice(
                Subsystem::Dsp,
                "core 0 was silent after the SOC1 restart; SOC1 was restarted once more".into(),
            );
        }
        for failure in &booted.soc1_failures {
            self.notice(
                Subsystem::Dsp,
                format!("SOC1 {:?} load failed: {}", failure.core, failure.error),
            );
        }
        if !matches!(
            booted.soc2,
            dsp::Soc2Status::Open | dsp::Soc2Status::Skipped
        ) {
            self.notice(Subsystem::Dsp, format!("SOC2 boot: {:?}", booted.soc2));
        }
        self.state.soc2 = Some(booted.soc2);
        self.dsp = Some(booted.dsp);
        if booted.soc2.faulted() {
            let cause = dsp::FaultCause::Boot(booted.soc2);
            let muted = dsp::raise_fault(self.dsp.as_mut(), &mut self.state.dsp_fault, cause);
            self.report(muted.map_err(DeviceError::from));
        }
        if let Some(dsp) = self.dsp.as_mut() {
            dsp.set_licences(self.state.licences);
        }
        let key = booted
            .repo_key
            .ok_or(dsp::DspError::MissingKey("ModelRepo"))?;
        let platform = &mut self.platform;
        let repo = platform
            .dsp_file(DspFile::ModelRepo)
            .map_err(DeviceError::platform)?;
        let report = platform
            .dsp_file(DspFile::ModuleReport)
            .map_err(DeviceError::platform)?;
        let preset::LoadedCatalog { mut specs, plain } =
            preset::load_catalog(&repo, &report, &key)?;
        // The opt-in owned stock-ID map; a refusal keeps every model stock.
        let owned = owned::install(&self.options, &mut specs).map_err(dsp::DspError::from);
        if let Some(owned) = self.report(owned.map_err(DeviceError::from))
            && let Some(dsp) = self.dsp.as_mut()
        {
            dsp.owned = owned;
        }
        self.state.specs = Some(specs);
        // Not fatal: the tables and the IR library below still load (w2b-control M1).
        let control = self.bridges.control.set_model_repo(&plain);
        self.report(control.map_err(DeviceError::from));
        // Driver, NOTE and media tables from the stock image; without them table drivers
        // are muted at deploy.
        let tables = self
            .platform
            .zenui_image()
            .map_err(DeviceError::platform)
            .and_then(|zenui| {
                self.keep_capture_seed(zenui.bytes());
                let dsp = self
                    .dsp
                    .as_mut()
                    .ok_or(DeviceError::Unavailable("the DSP"))?;
                Ok(dsp.load_tables(zenui)?)
            });
        self.report(tables);
        let library = self
            .platform
            .media_cipher()
            .ok_or(DeviceError::Unavailable("the device media cipher"))
            .and_then(|cipher| {
                let catalog = self
                    .platform
                    .dsp_file(DspFile::FactoryIrCatalog)
                    .map_err(DeviceError::platform)?;
                media::MediaLibrary::new(&catalog, cipher).map_err(DeviceError::platform)
            });
        self.media = self.report(library);
        // Factory captures (`factory.db`) for the capture blocks of a preset.
        let factory_db = self.platform.storage().factory.host().join("factory.db");
        if let (Some(media), Ok(factory)) = (
            self.media.as_mut(),
            StockDb::open_factory(&factory_db, DbLimits::default()),
        ) {
            media.set_factory(factory);
        }
        // r29-control: the factory IR folder of the Cortex Control File READ (`0x248ad4`).
        if let Ok(catalog) = self.platform.dsp_file(DspFile::FactoryIrCatalog) {
            self.bridges.control.set_factory_irs(&catalog);
        }
        // r16-media: legacy IR/capture migrations and the capture store translation, before the
        // media entries below read the database.
        self.migrate_boot_media();
        // NC2 capture records need the `0x27` boot vector (w4 capture).
        if let (Some(media), Some(key)) = (self.media.as_mut(), booted.capture_key.as_ref()) {
            media.set_nc2_key(key.as_bytes());
        }
        // r23-media: the capture corruption check (`0x702f70`) needs the NC2 key.
        self.check_capture_corruption();
        if let Some(media) = &self.media {
            let database = self
                .state
                .library
                .as_ref()
                .and_then(|library| library.database.as_ref());
            let (entries, failures) = media.entries(database);
            self.state.capture_names = control::capture_names(&entries);
            self.bridges
                .control
                .set_media_cipher(media.cipher().clone());
            self.bridges.ui.set_media_cipher(media.cipher().clone());
            self.bridges.ui.set_media(entries);
            let details = media.directory_media(database);
            match details {
                Ok(details) => self.bridges.ui.set_directory_media(details),
                Err(error) => self.notice(Subsystem::Library, error.to_string()),
            }
            for failure in failures {
                self.notice(Subsystem::Library, failure);
            }
        }
        Ok(())
    }

    /// The metronome driver's constructor upload (`0x558ab8` → `sendAllSounds` `0x558554`): the
    /// `0x47` bundle in generic fragments to core 2 (I: per fragment, no wait, 100 ms). The DSP
    /// answers `0x1a`, which sends the `0x47` trigger ([`DspEvent::Soc2SoundsReceived`]).
    pub(super) fn upload_metronome_sounds(&mut self) -> Result<(), DeviceError> {
        let factory = self.platform.storage().factory;
        let (bundle, missing) =
            features::metronome_sounds(factory.host()).map_err(DeviceError::platform)?;
        for file in missing {
            self.notice(
                Subsystem::Dsp,
                format!("metronome sound {file} is missing; sent empty"),
            );
        }
        let fragments = bundle
            .fragments(tetrad_dsp_protocol::CodecLimits::default())
            .map_err(dsp::DspError::from)?;
        let dsp = self
            .dsp
            .as_mut()
            .ok_or(DeviceError::Unavailable("the DSP"))?;
        for fragment in fragments.iter() {
            dsp.send(
                ControlPacket::Fragment(fragment),
                SendPolicy::new(false, 100),
            )?;
        }
        Ok(())
    }

    /// `migrate_on_boot` needs the device-25000 tempo defaults, so it runs after the catalog
    /// load and before any tempo write.
    pub(super) fn migrate_settings(&mut self, boot_count: u32) {
        let defaults = self.state.specs.as_ref().and_then(preset::tempo_defaults);
        if defaults.is_none() {
            self.notice(
                Subsystem::Settings,
                "tempo defaults unavailable without the catalog; missing tempo keys stay absent"
                    .into(),
            );
        }
        self.state
            .settings
            .migrate_on_boot(boot_count, &defaults.unwrap_or_default());
        self.schedule_settings_flush();
    }

    /// The Global EQ loader `0x2fbf08` (a read failure keeps the factory default).
    pub(super) fn load_global_eq_default(&mut self) {
        let factory_db = self.platform.storage().factory.host().join("factory.db");
        let factory = || StockDb::open_factory(&factory_db, DbLimits::default());
        match global_eq::user_default(&self.state, factory) {
            Ok(default) => self.state.global_eq_default = default,
            Err(error) => self.notice(
                Subsystem::Library,
                format!("Global EQ default model preset: {error}"),
            ),
        }
    }

    pub(super) fn apply_globals(&mut self) -> Result<(), DeviceError> {
        self.dsp_command(DspCommand::Globals)
    }
}

/// libzenhal product (`getProcCmdlineValue` `0x17a10`) and the ZenUI board platform
/// (`0x004962ac`). Revision reads fall back to the stock substitutes (zenjack 4, zencoder 3).
pub(super) fn identify<P: Platform>(platform: &mut P) -> Result<(Product, HalPlatform), P::Error> {
    let product = Product::from_cmdline(&platform.kernel_cmdline()?);
    let mut revision = |board, substitute| match platform.hal().read(Query::Revision(board)) {
        Ok(Reading::Word(word)) => word,
        _ => substitute,
    };
    let ess = product.is_qc_with_ess(revision(Board::Zenjack, 4), revision(Board::Zencoder, 3));
    let atma_emulation = platform.marker(Marker::AtmaEmulation)?;
    let ess_emulation = platform.marker(Marker::EssEmulation)?;
    let hal_platform = HalPlatform::detect(&product, ess, atma_emulation, ess_emulation);
    Ok((product, hal_platform))
}
