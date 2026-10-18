//! `zenui.db` around a backup: the staged copy and its capture re-key, and the close and reopen
//! around a restore (tetrad-app reopens after a failed extract and restarts after a good one,
//! review r18 F1).
//!
//! - Create (`0x004770f4`): the staged copy runs `0x006a6358(db, 0)` (downloads) and
//!   `0x006a68f4(db, 0)` (user type-2 rows with `readonly = 0`): the captures go to the
//!   global (seed-only) cipher, so another unit can read them.
//! - Restore (`0x00406844`): `0x0069daac` closes the database before extraction; the
//!   reload `0x003fcf64` opens it again (`0x006a5fd0` → `0x006a576c`) and runs both
//!   translations with argument 1: the captures go back to this unit's cipher.

use std::path::{Path, PathBuf};

use tetrad_media::factory::DeviceCipher;
use tetrad_storage::stock::{
    CaptureEncryption, DbLimits, SchemaVersion, StockDb, StoreTranslation,
};

use super::translate_capture_stores;
use crate::Result;

/// The two device ciphers of a capture re-key, borrowed from their one owner.
#[derive(Clone, Copy)]
pub struct CaptureCiphers<'a> {
    /// Secret mode 0 (seed only): the global store and backups.
    pub global: &'a DeviceCipher,
    /// Secret mode 1 (seed and serial): the local store of this unit.
    pub local: &'a DeviceCipher,
}

/// Argument 2 of `0x006a6358` / `0x006a68f4`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rekey {
    /// 0: to global, on the staged copy of a backup (`0x004770f4`).
    ForBackup,
    /// 1: to local, on a restored database (`0x003fcf64`; boot does the same).
    AfterRestore,
}

/// Re-keys both capture stores of an open database: downloads, then user, each in one
/// transaction with its `nc_encryption` flag. A store already flagged with the target is
/// not read (`0x006a46ec`).
pub fn rekey(
    db: &mut StockDb,
    direction: Rekey,
    ciphers: CaptureCiphers<'_>,
) -> Result<[StoreTranslation; 2]> {
    let (target, from, to) = match direction {
        Rekey::ForBackup => (CaptureEncryption::Global, ciphers.local, ciphers.global),
        Rekey::AfterRestore => (CaptureEncryption::Local, ciphers.global, ciphers.local),
    };
    translate_capture_stores(db, target, from, to)
}

/// [`rekey`] over a database file, such as the staged `zenui.db` of a backup. Never run it on
/// the live file for [`Rekey::ForBackup`]: make the copy first ([`StockDb::copy_to`]).
pub fn rekey_for_backup(
    db_path: &Path,
    direction: Rekey,
    ciphers: CaptureCiphers<'_>,
) -> Result<[StoreTranslation; 2]> {
    let mut db = StockDb::open(db_path, DbLimits::default(), false)?;
    let rekeyed = rekey(&mut db, direction, ciphers)?;
    db.close()?;
    Ok(rekeyed)
}

/// The staged `zenui.db` of a backup create (`0x004763b4`, re-key `0x004770f4`): a
/// stepped copy of `database` into `dir` ([`StockDb::copy_to`]), made through its own
/// read-only connection so that it runs on a worker while the device keeps its live handle
/// and writes (review r18 F8, r19 F4, F5). `dir` is emptied first, and removed again when
/// the copy fails. The re-key runs on the copy only; its result comes back beside the
/// staged path, and a failed re-key keeps the copy (unit-bound captures). `None` ciphers
/// skip the re-key.
pub fn stage_for_backup(
    database: &Path,
    dir: &Path,
    ciphers: Option<CaptureCiphers<'_>>,
) -> Result<(PathBuf, Option<Result<[StoreTranslation; 2]>>)> {
    let staged = dir.join("zenui.db");
    let copied = (|| {
        match std::fs::remove_dir_all(dir) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => std::fs::create_dir_all(dir)?,
        }
        let db = StockDb::open(database, DbLimits::default(), true)?;
        db.copy_to(&staged)?;
        Ok(db.close()?)
    })();
    if let Err(error) = copied {
        _ = std::fs::remove_dir_all(dir);
        return Err(error);
    }
    let rekeyed = ciphers.map(|ciphers| rekey_for_backup(&staged, Rekey::ForBackup, ciphers));
    Ok((staged, rekeyed))
}

/// The database after [`reopen_after_restore`].
pub struct Reopened {
    pub db: StockDb,
    /// No file was there, so a new database was made (`0x006a576c`).
    pub created: bool,
    /// Stock patches applied to a restored database below 410.1.
    pub upgraded: Vec<SchemaVersion>,
    /// Downloads, then user store. `None`: no ciphers, the stores keep their keys (a global
    /// store stays refused for capture reads until a later boot re-keys it).
    pub rekeyed: Option<Result<[StoreTranslation; 2]>>,
    /// The boot maintenance (`0x006a4140`). A failure keeps the open database: captures
    /// and IRs still play (review r18 F7).
    pub maintenance: Result<()>,
}

/// The database part of the reload after a restore (`0x003fcf64`), in stock order: open
/// or create (`0x006a5fd0`, with the patch chain), re-key both stores to local
/// ([`Rekey::AfterRestore`]), then the boot maintenance (`0x0076f674` → `0x006a4140`).
/// The caller first closes the old handle ([`StockDb::close`], `0x0069daac`), runs the
/// extraction, then calls this on success and on failure, as stock does (`0x006a5fd0`).
///
/// Not here (the caller owns them, as at boot): the legacy migrations (`0x006e72d4`,
/// `0x006e71cc`), the legacy model defaults, the search index and the view refresh.
/// A failed re-key or boot maintenance keeps the open database (the store stays flagged as
/// before).
pub fn reopen_after_restore(
    path: &Path,
    limits: DbLimits,
    ciphers: Option<CaptureCiphers<'_>>,
) -> Result<Reopened> {
    let (mut db, created, upgraded) = if std::fs::symlink_metadata(path).is_ok() {
        let upgraded = StockDb::upgrade(path, limits)?;
        (StockDb::open(path, limits, false)?, false, upgraded)
    } else {
        (StockDb::create(path, limits)?, true, Vec::new())
    };
    let rekeyed = ciphers.map(|ciphers| rekey(&mut db, Rekey::AfterRestore, ciphers));
    let maintenance =
        (db.transaction(|write| write.boot_maintenance().map(drop))).map_err(Into::into);
    Ok(Reopened {
        db,
        created,
        upgraded,
        rekeyed,
        maintenance,
    })
}
