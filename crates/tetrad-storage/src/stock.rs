//! Stock CorOS `zenui.db` access (`/media/p4/zenui.db`).
//!
//! Table and column names, SQL shapes and value encodings come from ZenUI's DDL
//! and query strings (`0x00a453cc`, `0x011ee0e0..0x011eefbc`,
//! `0x01204208..0x01204b74`) and from a CorOS 4.1.0 database at schema 410.1.
//! Opening checks the schema and never migrates it; [`StockDb::upgrade`] runs
//! the stock patch chain on an older database, and [`StockDb::create`] makes a
//! new 410.1 database for the factory reset. Every write runs inside one immediate
//! transaction ([`StockDb::transaction`]).

use std::cell::RefCell;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, ToSql, TransactionBehavior, params};

use crate::{Error, READ_FLAGS, Result, WRITE_FLAGS, check_sidecars};

/// The schema version that CorOS 4.1.0 leaves in `schema_version`.
pub const COROS_4_1_SCHEMA: SchemaVersion = SchemaVersion {
    major: 410,
    minor: 1,
};

/// Columns each table must have. Order matches the stock DDL so that the
/// positional stock `VALUES (...)` inserts stay valid. The first six tables
/// also exist in `/opt/neuraldsp/factory.db` (factory DDL `0x00a4668c`,
/// `0x00a46b64`).
const TABLES: &[(&str, &[&str])] = &[
    ("schema_version", &["major", "minor"]),
    (
        "product",
        &["id", "type", "readonly", "rawdata", "metadata"],
    ),
    (
        "folder",
        &["id", "pdType", "folderType", "name", "parent", "readonly"],
    ),
    ("folder_content", &["folderId", "productId"]),
    (
        "default_param",
        &[
            "model_id",
            "param_id",
            "value",
            "exp",
            "exp_trim_max",
            "exp_trim_min",
        ],
    ),
    (
        "default_exp_switch",
        &[
            "model_id",
            "exp_switch_type",
            "exp_switch_inverted",
            "exp_switch_latch_emulation",
            "exp_switch_delay_ms",
        ],
    ),
    ("downloads_product_preset", &["id", "rawdata", "metadata"]),
    (
        "downloads_product_nc",
        &["id", "hash", "rawdata", "metadata"],
    ),
    ("nc_encryption", &["nc_type", "encryption"]),
    ("pinned_models", &["hash"]),
    (
        "model_presets",
        &[
            "id",
            "name",
            "hash",
            "expSwitchType",
            "expSwitchInverted",
            "expSwitchLatchEmulation",
            "expSwitchDelayMs",
        ],
    ),
    ("default_model_presets", &["hash", "presetId", "isFactory"]),
    (
        "model_preset_parameters",
        &[
            "presetId",
            "paramIndex",
            "value",
            "exp",
            "expTrimMax",
            "expTrimMin",
        ],
    ),
];

/// The user schema that ZenUI's patch chain (`0x00483b18`: 300.1, 300.2 at
/// `0x00a453cc`, 410.1 at `0x00a4693c`) leaves in an empty database. The
/// renamed download tables are written under their final names.
pub(crate) const USER_SCHEMA: &str = "
CREATE TABLE default_param (model_id TEXT,param_id INT,value TEXT,exp INT,exp_trim_max REAL,exp_trim_min REAL,PRIMARY KEY (model_id, param_id));
CREATE TABLE default_exp_switch (model_id TEXT,exp_switch_type INT,exp_switch_inverted INT,exp_switch_latch_emulation INT,exp_switch_delay_ms INT,PRIMARY KEY (model_id));
CREATE TABLE product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
CREATE TABLE downloads_product_preset ( id TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE TABLE downloads_product_nc ( id TEXT, hash TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE INDEX downloads_product_nc_hash ON downloads_product_nc (hash);
CREATE TABLE folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
CREATE TABLE folder_content ( folderId INTEGER, productId TEXT, FOREIGN KEY(folderId) REFERENCES folder(id), PRIMARY KEY(folderId,productId));
CREATE TABLE nc_encryption ( nc_type TEXT, encryption INT, PRIMARY KEY ( nc_type ) );
CREATE TABLE pinned_models (hash TEXT);
CREATE TABLE model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
CREATE TABLE default_model_presets ( hash INT PRIMARY KEY, presetId INTEGER, isFactory INTEGER );
CREATE TABLE model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, FOREIGN KEY (presetId) REFERENCES model_presets(id), PRIMARY KEY (presetId, paramIndex));
CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
";

/// The user patches of `0x00483b18` (`_INIT_367`): the version, the major
/// below which the patch is due (`0xc9` for the 300 group, `0x19a` for 410),
/// and its statements, one per line as stock runs them (each line trimmed,
/// then executed). Whitespace inside a line is compacted.
const USER_PATCHES: [(SchemaVersion, i64, &str); 3] = [
    (
        SchemaVersion {
            major: 300,
            minor: 1,
        },
        201,
        "DROP TABLE IF EXISTS schema_version;
CREATE TABLE schema_version ( major INTEGER, minor INTEGER);
INSERT INTO schema_version VALUES (300, 0);",
    ),
    (
        SchemaVersion {
            major: 300,
            minor: 2,
        },
        201,
        "PRAGMA foreign_keys = 0;
CREATE TABLE IF NOT EXISTS default_param ( model_id TEXT, param_id INT, value TEXT, exp INT, exp_trim_max REAL, exp_trim_min REAL, PRIMARY KEY (model_id, param_id));
CREATE TABLE IF NOT EXISTS default_exp_switch ( model_id TEXT, exp_switch_type INT, exp_switch_inverted INT, exp_switch_latch_emulation INT, exp_switch_delay_ms INT, PRIMARY KEY (model_id));
CREATE TABLE IF NOT EXISTS product ( id TEXT, type INT, readonly INT, rawdata BLOB, metadata BLOB, PRIMARY KEY(id));
CREATE TABLE IF NOT EXISTS product_preset ( id TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
CREATE TABLE IF NOT EXISTS product_nc ( id TEXT, hash TEXT, rawdata BLOB, metadata BLOB, PRIMARY KEY ( id ) );
ALTER TABLE product_preset RENAME TO downloads_product_preset;
ALTER TABLE product_nc RENAME TO downloads_product_nc;
CREATE INDEX downloads_product_nc_hash ON downloads_product_nc (hash);
DROP TABLE IF EXISTS preset_download_queue;
DROP TABLE IF EXISTS capture_download_queue;
DROP TABLE IF EXISTS product_cache_count;
DROP TABLE IF EXISTS folder_content;
DROP TABLE IF EXISTS folder;
CREATE TABLE IF NOT EXISTS folder ( id INTEGER PRIMARY KEY AUTOINCREMENT, pdType INT, folderType INT, name TEXT, parent INTEGER, readonly INT);
CREATE TABLE IF NOT EXISTS folder_content ( folderId INTEGER, productId TEXT, FOREIGN KEY(folderId) REFERENCES folder(id), PRIMARY KEY(folderId,productId));
CREATE TABLE IF NOT EXISTS nc_encryption ( nc_type TEXT, encryption INT, PRIMARY KEY ( nc_type ) );
CREATE TABLE IF NOT EXISTS pinned_models (hash TEXT);
PRAGMA foreign_keys = 1;",
    ),
    (
        COROS_4_1_SCHEMA,
        410,
        "CREATE TABLE IF NOT EXISTS model_presets ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, hash INT, expSwitchType INT, expSwitchInverted INT, expSwitchLatchEmulation INT, expSwitchDelayMs INT);
CREATE TABLE IF NOT EXISTS default_model_presets ( hash INT PRIMARY KEY, presetId INTEGER, isFactory INTEGER );
CREATE TABLE IF NOT EXISTS model_preset_parameters (presetId INT, paramIndex INT, value TEXT, exp INT, expTrimMax REAL, expTrimMin REAL, FOREIGN KEY (presetId) REFERENCES model_presets(id), PRIMARY KEY (presetId, paramIndex));",
    ),
];

/// Rows of one capture store, as a store translation reads and rewrites them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureBlobs {
    pub id: String,
    pub raw_data: Option<Vec<u8>>,
    pub metadata: Option<Vec<u8>>,
}

/// Result of [`StockDb::translate_capture_store`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreTranslation {
    /// The flag already named the target; nothing was read or written.
    AlreadyInTarget,
    /// Rows rewritten, then the flag set, in one transaction.
    Translated(usize),
}

const FACTORY_TABLES: &[&str] = &[
    "schema_version",
    "product",
    "folder",
    "folder_content",
    "model_presets",
    "model_preset_parameters",
];

/// Model presets per model hash (`0x001f58a0`: full when count > 31).
pub const MAX_MODEL_PRESETS_PER_MODEL: i64 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SchemaVersion {
    pub major: i64,
    pub minor: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct DbLimits {
    pub max_database_bytes: u64,
    /// Bounds any single text or BLOB value (SQLite length limit).
    pub max_value_bytes: usize,
    /// Bounds every list result.
    pub max_rows: usize,
}

impl Default for DbLimits {
    fn default() -> Self {
        Self {
            max_database_bytes: 1024 * 1024 * 1024,
            max_value_bytes: 32 * 1024 * 1024,
            max_rows: 100_000,
        }
    }
}

/// Stock `product.type` and `folder.pdType` (metadata switch in `0x007ee62c`).
/// Unknown codes, such as 3 (no metadata; likely plugin presets), stay `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProductType {
    Preset,
    /// Id `CIR_` + MD5 bytes ([`ir_product_id`]).
    Ir,
    /// Id is the capture key (64 hex), also its metadata `hash`.
    Capture,
    Other(i64),
}

impl ProductType {
    pub fn code(self) -> i64 {
        match self {
            Self::Preset => 0,
            Self::Ir => 1,
            Self::Capture => 2,
            Self::Other(code) => code,
        }
    }
}

impl From<i64> for ProductType {
    fn from(code: i64) -> Self {
        match code {
            0 => Self::Preset,
            1 => Self::Ir,
            2 => Self::Capture,
            other => Self::Other(other),
        }
    }
}

/// Stock `folder.folderType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderType {
    /// Read-only factory folders (`factory.db`).
    Factory,
    /// The user root of a product type ("My IRs", "My Captures"), `parent = 0`.
    UserRoot,
    /// A user-created folder (`0x0076d584`).
    User,
    Other(i64),
}

impl FolderType {
    pub fn code(self) -> i64 {
        match self {
            Self::Factory => 0,
            Self::UserRoot => 1,
            Self::User => 2,
            Self::Other(code) => code,
        }
    }
}

impl From<i64> for FolderType {
    fn from(code: i64) -> Self {
        match code {
            0 => Self::Factory,
            1 => Self::UserRoot,
            2 => Self::User,
            other => Self::Other(other),
        }
    }
}

/// User root folders that ZenUI gets-or-creates on every boot (`0x006a4140`).
pub const USER_ROOTS: [(ProductType, &str); 2] = [
    (ProductType::Capture, "My Captures"),
    (ProductType::Ir, "My IRs"),
];

/// Tetrad only (O): captures the corruption check took out of the library, with their bytes
/// as stored. Stock deletes them (`0x702f70`). Stock code reads no table it does not know, so
/// the extra table changes nothing for stock firmware.
pub const QUARANTINE_TABLE: &str = "tetrad_capture_quarantine";
/// [`QUARANTINE_TABLE`] as zen-rs (the earlier name of Tetrad) wrote it. A writable
/// [`StockDb::open`] moves it to the new name ([`StockWrite::migrate_legacy_quarantine`]).
const LEGACY_QUARANTINE_TABLE: &str = "zen_capture_quarantine";
const QUARANTINE_DDL: &str = "CREATE TABLE IF NOT EXISTS tetrad_capture_quarantine (store TEXT \
     NOT NULL, id TEXT NOT NULL, hash TEXT, rawdata BLOB, metadata BLOB, reason TEXT NOT NULL, \
     name TEXT)";

/// One row of [`QUARANTINE_TABLE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedCapture {
    pub store: CaptureStore,
    pub id: String,
    /// The name the Corrupt Captures dialog showed (r25-ui MR 3); the id for a row written
    /// before the table had the column.
    pub name: String,
    pub reason: String,
}

/// The IR product id: `CIR_` followed by each MD5 byte of the rawdata in
/// lowercase hex without zero padding (`0x00829468`: `QString::number(byte,
/// 16)` at `0x8294bc`). `tetrad_media::user::product_id` builds the same id.
pub fn ir_product_id(md5: &[u8; 16]) -> String {
    use std::fmt::Write;
    md5.iter().fold(String::from("CIR_"), |mut id, byte| {
        write!(id, "{byte:x}").expect("String write");
        id
    })
}

/// `nc_encryption.nc_type`: which capture store a flag describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStore {
    User,
    Downloads,
}

impl CaptureStore {
    fn key(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Downloads => "downloads",
        }
    }
}

/// `nc_encryption.encryption`. ZenUI re-encrypts a store and then sets the flag
/// (`0x006a68f4`, `0x006a6358`); `Global` is a backup or interrupted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureEncryption {
    /// No row (reads as 0). The rows are device-keyed: a CorOS 4.1.0 unit never writes the
    /// flag, and its boot translation runs only for `Global` (`0x006a6f78`, C; B13).
    Unset,
    /// Device key (serial-derived).
    Local,
    Global,
    Other(i64),
}

impl CaptureEncryption {
    /// Whether the store's rows read with the device cipher (`Local` or `Unset`).
    pub fn is_device_keyed(self) -> bool {
        matches!(self, Self::Local | Self::Unset)
    }

    fn code(self) -> i64 {
        match self {
            Self::Unset => 0,
            Self::Local => 1,
            Self::Global => 2,
            Self::Other(code) => code,
        }
    }
}

/// One `product` row. The id is the 64-hex content hash for user captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockProduct {
    pub id: String,
    pub product_type: ProductType,
    pub read_only: bool,
    pub raw_data: Option<Vec<u8>>,
    /// Device-encrypted for captures; opaque here.
    pub metadata: Option<Vec<u8>>,
}

/// One `folder` row. Stock roots use `parent = 0`, not NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockFolder {
    pub id: i64,
    pub product_type: ProductType,
    pub folder_type: FolderType,
    pub name: String,
    pub parent: i64,
    pub read_only: bool,
}

/// Fields for a new or updated folder; the id is assigned by AUTOINCREMENT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFolder<'a> {
    pub product_type: ProductType,
    pub folder_type: FolderType,
    pub name: &'a str,
    pub parent: i64,
    pub read_only: bool,
}

/// A parameter value row shared by user defaults and model presets.
/// `value` is the stock text form (`"0.24"`, `"1"` or a string parameter).
#[derive(Debug, Clone, PartialEq)]
pub struct ParamEntry {
    pub index: i64,
    pub value: String,
    pub exp: i64,
    pub exp_trim_max: f64,
    pub exp_trim_min: f64,
}

/// Expression-switch settings of a model default or model preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpSwitch {
    pub switch_type: i64,
    pub inverted: bool,
    pub latch_emulation: bool,
    pub delay_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPreset {
    pub id: i64,
    pub name: String,
    /// Stock `hash` column: the model identifier the preset belongs to.
    pub model: i64,
    pub exp_switch: ExpSwitch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultModelPreset {
    pub model: i64,
    pub preset_id: i64,
    pub factory: bool,
}

/// A cloud download row. `hash` is present only for captures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub id: String,
    pub hash: Option<String>,
    pub raw_data: Option<Vec<u8>>,
    pub metadata: Option<Vec<u8>>,
}

/// A Downloads row without its blobs, for a listing ([`StockView::download_keys`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadKey {
    pub rowid: i64,
    pub id: String,
    /// Captures only.
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadKind {
    /// `downloads_product_preset`: preset protobuf + plaintext JSON metadata.
    Preset,
    /// `downloads_product_nc`: device-encrypted capture + `hash` column.
    Capture,
}

impl DownloadKind {
    fn table(self) -> &'static str {
        match self {
            Self::Preset => "downloads_product_preset",
            Self::Capture => "downloads_product_nc",
        }
    }
}

/// An existing stock `zenui.db`. Opening never creates, migrates or rewrites it.
pub struct StockDb {
    connection: Connection,
    limits: DbLimits,
    read_only: bool,
    version: SchemaVersion,
    /// [`Self::path`], read once: an open connection keeps its file.
    path: Option<PathBuf>,
    roots: RootCache,
}

/// The [`StockView::root_folder`] results of a [`StockDb::view`] (product type code, root),
/// valid while the connection's change count ([`Connection::total_changes`]) stays at
/// `changes`. Each INSERT, UPDATE or DELETE on this connection counts, also one that a
/// rollback undoes, so any write of this handle to `folder` (a transaction, the boot
/// maintenance) drops the cache. Reads inside a transaction do not use it: a rollback could
/// undo what they saw.
/// Writes of other connections do not count. On the device only the library handle writes
/// the live `zenui.db`: the other handles open it read-only, the backup re-key writes the
/// staged copy, and a restore or a factory reset opens a new handle.
#[derive(Default)]
struct RootCache(RefCell<CachedRoots>);

#[derive(Default)]
struct CachedRoots {
    changes: u64,
    roots: Vec<(i64, Option<i64>)>,
}

impl StockDb {
    /// Opens an existing user `zenui.db` after file, header, sidecar and schema
    /// checks. A writable handle requires [`COROS_4_1_SCHEMA`]; other versions
    /// open read-only only.
    pub fn open(path: &Path, limits: DbLimits, read_only: bool) -> Result<Self> {
        Self::open_kind(path, limits, read_only, false)
    }

    /// The database file, as opened (canonical); `None` for an in-memory database.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Opens `/opt/neuraldsp/factory.db` read-only (factory captures and model
    /// presets). User-only tables are not required and never written.
    pub fn open_factory(path: &Path, limits: DbLimits) -> Result<Self> {
        Self::open_kind(path, limits, true, true)
    }

    /// Creates a new user `zenui.db` at [`COROS_4_1_SCHEMA`], as ZenUI's open
    /// (`0x006a576c`) leaves a missing file after the factory reset removed it
    /// (`0x006a551c`). The file and its sidecars must be absent; an existing
    /// database is never replaced. A failure can leave a partial new file.
    pub fn create(path: &Path, limits: DbLimits) -> Result<Self> {
        check_limits(limits)?;
        check_sidecars(path, true, limits.max_database_bytes)?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut connection = Connection::open_with_flags(path.canonicalize()?, WRITE_FLAGS)?;
        configure(&connection, limits, false)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(USER_SCHEMA)?;
        transaction.execute(
            "INSERT INTO schema_version VALUES (?1, ?2)",
            params![COROS_4_1_SCHEMA.major, COROS_4_1_SCHEMA.minor],
        )?;
        // Both capture stores flagged local: the state the stock boot translation leaves on
        // empty stores (`0x006a6358`, `0x006a68f4`). Without it, captures saved here
        // before a reboot would look global to [`Self::translate_capture_store`].
        transaction.execute_batch(
            "INSERT INTO nc_encryption VALUES ('user', 1); INSERT INTO nc_encryption VALUES ('downloads', 1);",
        )?;
        transaction.commit()?;
        drop(connection);
        file.sync_all()?;
        Self::open(path, limits, false)
    }

    fn open_kind(path: &Path, limits: DbLimits, read_only: bool, factory: bool) -> Result<Self> {
        let connection = connect(path, limits, read_only)?;
        let version = check_schema(&connection, factory)?;
        if !read_only && version != COROS_4_1_SCHEMA {
            return Err(Error::StockVersion(version));
        }
        let mut db = Self {
            path: connection.path().map(PathBuf::from),
            connection,
            limits,
            read_only,
            version,
            roots: RootCache::default(),
        };
        // Before the first use of the quarantine; a read-only handle leaves the file as it is.
        if !read_only && db.view().has_table(LEGACY_QUARANTINE_TABLE)? {
            db.transaction(|write| write.migrate_legacy_quarantine())?;
        }
        Ok(db)
    }

    pub fn version(&self) -> SchemaVersion {
        self.version
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Read access outside a write transaction.
    pub fn view(&self) -> StockView<'_> {
        StockView {
            connection: &self.connection,
            limits: self.limits,
            roots: Some(&self.roots),
        }
    }

    /// Runs `f` in one immediate transaction. `Ok` commits; any error rolls
    /// back every write that `f` made.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&StockWrite<'_>) -> Result<T>) -> Result<T> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = f(&StockWrite {
            view: StockView {
                connection: &transaction,
                limits: self.limits,
                roots: None,
            },
        })?;
        transaction.commit()?;
        Ok(value)
    }

    /// `pragma integrity_check` as ZenUI runs it (`0x011eed48`): true when "ok".
    pub fn integrity_ok(&self) -> Result<bool> {
        let first: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(first == "ok")
    }

    /// Brings a user `zenui.db` below [`COROS_4_1_SCHEMA`] to it with the stock
    /// patch chain (`0x00483b18`), then returns the versions applied (empty
    /// when none was due). Selection is stock: a major below 201 takes 300.1
    /// and 300.2; a major below 410 takes 410.1. A missing `schema_version`
    /// reads as 0.0 (`0x00482e08`). Each patch runs in its own transaction
    /// and then sets `schema_version` (`0x00a46d18`, `0x0048293c`).
    ///
    /// Deliberate difference: when a patch fails, stock deletes and recreates
    /// the database (`0x006a576c`, "Database Update Error"). This rolls the
    /// patch back and returns [`Error::StockSchema`]; the file keeps the
    /// earlier patches and the caller decides about [`Self::create`].
    pub fn upgrade(path: &Path, limits: DbLimits) -> Result<Vec<SchemaVersion>> {
        let mut connection = connect(path, limits, false)?;
        // Stock runs with foreign keys off (see `configure`).
        connection.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let current = connection
            .query_row("SELECT major, minor FROM schema_version", [], |row| {
                Ok(SchemaVersion {
                    major: int_at(row, 0)?,
                    minor: int_at(row, 1)?,
                })
            })
            .optional()
            .unwrap_or(None)
            .unwrap_or(SchemaVersion { major: 0, minor: 0 });
        let mut applied = Vec::new();
        for (version, _, sql) in USER_PATCHES
            .iter()
            .filter(|(_, below, _)| current.major < *below)
        {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for line in sql.lines().map(str::trim).filter(|line| !line.is_empty()) {
                transaction.execute_batch(line).map_err(|error| {
                    Error::StockSchema(format!(
                        "patch {}.{} failed: {error}",
                        version.major, version.minor
                    ))
                })?;
            }
            transaction.execute(
                "UPDATE schema_version SET major = ?1, minor = ?2",
                params![version.major, version.minor],
            )?;
            transaction.commit()?;
            applied.push(*version);
        }
        Ok(applied)
    }

    /// Re-encrypts one capture store and sets its `nc_encryption` flag, all in
    /// one transaction (`0x006a68f4` user store: type-2 rows with
    /// `readonly = 0`; `0x006a6358` downloads: every row), with the store's rows in
    /// [`QUARANTINE_TABLE`] (O). `translate`
    /// rewrites the blobs of one row (`tetrad_media::capture_store`); NULL stays
    /// NULL. When the flag already names `target` nothing runs
    /// (`0x006a46ec`). `target` is [`CaptureEncryption::Local`] or
    /// [`CaptureEncryption::Global`].
    pub fn translate_capture_store(
        &mut self,
        store: CaptureStore,
        target: CaptureEncryption,
        mut translate: impl FnMut(&mut CaptureBlobs) -> Result<()>,
    ) -> Result<StoreTranslation> {
        if !matches!(target, CaptureEncryption::Local | CaptureEncryption::Global) {
            return Err(Error::Format("capture store target is local or global"));
        }
        if self.view().capture_encryption(store)? == target {
            return Ok(StoreTranslation::AlreadyInTarget);
        }
        self.transaction(|write| {
            let rows = match store {
                CaptureStore::User => write.list(
                    "SELECT id, rawdata, metadata FROM product WHERE type = ?1 AND readonly = 0 AND id IS NOT NULL ORDER BY rowid",
                    [ProductType::Capture.code()],
                    capture_blobs,
                )?,
                CaptureStore::Downloads => write.list(
                    "SELECT id, rawdata, metadata FROM downloads_product_nc WHERE id IS NOT NULL ORDER BY rowid",
                    [],
                    capture_blobs,
                )?,
            };
            let count = rows.len();
            for mut row in rows {
                translate(&mut row)?;
                let sql = match store {
                    CaptureStore::User => {
                        "UPDATE product SET rawdata = ?2, metadata = ?3 WHERE id = ?1"
                    }
                    CaptureStore::Downloads => {
                        "UPDATE downloads_product_nc SET rawdata = ?2, metadata = ?3 WHERE id = ?1"
                    }
                };
                write.execute(sql, params![row.id, row.raw_data, row.metadata])?;
            }
            // Review r25 Q6 (O): the store's quarantined rows were written in its cipher (the
            // check runs only on a local store), so they follow it. A restore on another unit
            // can then put them back readable. `count` stays the store's rows.
            if write.has_table(QUARANTINE_TABLE)? {
                let held = write.list(
                    "SELECT CAST(rowid AS TEXT), rawdata, metadata FROM tetrad_capture_quarantine \
                     WHERE store = ?1 ORDER BY rowid",
                    [store.key()],
                    capture_blobs,
                )?;
                for mut row in held {
                    translate(&mut row)?;
                    write.execute(
                        "UPDATE tetrad_capture_quarantine SET rawdata = ?2, metadata = ?3 WHERE \
                         rowid = CAST(?1 AS INTEGER)",
                        params![row.id, row.raw_data, row.metadata],
                    )?;
                }
            }
            write.set_capture_encryption(store, target)?;
            Ok(StoreTranslation::Translated(count))
        })
    }

    /// Closes the connection and reports a failed close (`0x0069daac` before a backup
    /// restore). A drop also closes, but hides the error. After `Ok` no journal of this
    /// handle is left, so a restore can replace the file.
    pub fn close(self) -> Result<()> {
        self.connection.close().map_err(|(_, error)| error.into())
    }

    /// Writes a consistent copy of the database to `dest` (`0x004763b4` copies `zenui.db`
    /// for a backup stage) through the SQLite online backup, [`COPY_STEP_PAGES`] pages per
    /// step. Each step holds the read lock only for its pages, so a device commit on another
    /// connection waits one step, not the whole copy (review r19 F4; `VACUUM INTO` held the
    /// lock for the copy and a commit failed after the 500 ms busy timeout). A commit from
    /// another connection restarts the copy from its first page, so the copy is one committed
    /// state; after `COPY_RESTARTS` restarts it fails with [`Error::CopyContended`]. The copy
    /// keeps free pages (no vacuum). `dest` must not exist; a failure can leave a partial file.
    pub fn copy_to(&self, dest: &Path) -> Result<()> {
        use rusqlite::OpenFlags;
        use rusqlite::backup::{Backup, StepResult};
        if fs::symlink_metadata(dest).is_ok() {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists).into());
        }
        let mut target =
            Connection::open_with_flags(dest, WRITE_FLAGS | OpenFlags::SQLITE_OPEN_CREATE)?;
        target.busy_timeout(Duration::from_millis(500))?;
        let backup = Backup::new(&self.connection, &mut target)?;
        let (mut restarts, mut waits) = (CopyRestarts::default(), 0);
        loop {
            match backup.step(COPY_STEP_PAGES)? {
                StepResult::Done => return Ok(()),
                StepResult::More => {
                    restarts.step(backup.progress())?;
                    waits = 0;
                    std::thread::sleep(COPY_PAUSE);
                }
                // A writer holds the lock past the busy timeout of this connection.
                _ => {
                    waits += 1;
                    if waits > COPY_WAITS {
                        return Err(Error::CopyContended);
                    }
                    std::thread::sleep(COPY_WAIT);
                }
            }
        }
    }
}

/// The restart count of [`StockDb::copy_to`]. A step without a restart copies more pages;
/// any other count is a restart from page 1, also one right after the first step, where the
/// count stays equal (review r20 G7). So the copy loop is bounded: the copied pages by the
/// file, the restarts by [`COPY_RESTARTS`].
#[derive(Debug, Default)]
pub(crate) struct CopyRestarts {
    copied: i32,
    count: u32,
}

impl CopyRestarts {
    /// After a step that left pages to copy; [`Error::CopyContended`] after
    /// [`COPY_RESTARTS`] restarts.
    pub(crate) fn step(&mut self, progress: rusqlite::backup::Progress) -> Result<()> {
        let copied = progress.pagecount - progress.remaining;
        if copied <= self.copied {
            self.count += 1;
            if self.count > COPY_RESTARTS {
                return Err(Error::CopyContended);
            }
        }
        self.copied = copied;
        Ok(())
    }
}

/// Pages per [`StockDb::copy_to`] step (O): 1 MiB at the stock 4 KiB page size, from a host
/// measurement.
pub const COPY_STEP_PAGES: i32 = 256;
/// The pause after a step, so a waiting writer gets the lock (O; without it a writer on the
/// host waited the full 500 ms busy timeout).
const COPY_PAUSE: Duration = Duration::from_millis(2);
/// The wait after a busy step (O).
const COPY_WAIT: Duration = Duration::from_millis(10);
/// Restarts before [`StockDb::copy_to`] gives up (O).
pub(crate) const COPY_RESTARTS: u32 = 16;
/// Busy steps in a row before [`StockDb::copy_to`] gives up (O).
const COPY_WAITS: u32 = 50;

fn capture_blobs(row: &Row<'_>) -> rusqlite::Result<CaptureBlobs> {
    Ok(CaptureBlobs {
        id: text_at(row, 0)?,
        raw_data: blob_at(row, 1)?,
        metadata: blob_at(row, 2)?,
    })
}

/// File, header and sidecar checks, then a configured connection.
fn connect(path: &Path, limits: DbLimits, read_only: bool) -> Result<Connection> {
    check_limits(limits)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(Error::NotRegular);
    }
    if metadata.len() > limits.max_database_bytes {
        return Err(Error::Limit);
    }
    let mut header = [0_u8; 16];
    fs::File::open(path)?.read_exact(&mut header)?;
    if &header != b"SQLite format 3\0" {
        return Err(Error::StockSchema("not an SQLite database".into()));
    }
    check_sidecars(path, false, limits.max_database_bytes)?;
    let flags = if read_only { READ_FLAGS } else { WRITE_FLAGS };
    let connection = Connection::open_with_flags(path.canonicalize()?, flags)?;
    configure(&connection, limits, read_only)?;
    Ok(connection)
}

fn check_limits(limits: DbLimits) -> Result<()> {
    if limits.max_database_bytes < 64 * 1024
        || !(8_192..=i32::MAX as usize).contains(&limits.max_value_bytes)
        || limits.max_rows == 0
    {
        return Err(Error::Limit);
    }
    Ok(())
}

fn configure(connection: &Connection, limits: DbLimits, read_only: bool) -> Result<()> {
    use rusqlite::limits::Limit;
    connection.busy_timeout(Duration::from_millis(500))?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, limits.max_value_bytes as i32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 16_384)?;
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    // Deliberate difference: stock runs with foreign keys off. Its only
    // `PRAGMA foreign_keys = 1` is the last line of the 300.2 patch
    // (`0x00a453cc`), and a pragma lasts one connection. Every Tetrad write
    // satisfies the two user-schema keys. Journal mode and page size stay as
    // stock wrote them.
    connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")?;
    if read_only {
        connection.execute_batch("PRAGMA query_only=ON;")?;
    } else {
        connection.execute_batch("PRAGMA synchronous=FULL;")?;
        let page_size: u32 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
        connection.query_row(
            &format!(
                "PRAGMA max_page_count={}",
                limits.max_database_bytes / u64::from(page_size)
            ),
            [],
            |_| Ok(()),
        )?;
    }
    Ok(())
}

fn check_schema(connection: &Connection, factory: bool) -> Result<SchemaVersion> {
    for (table, columns) in TABLES
        .iter()
        .filter(|(table, _)| !factory || FACTORY_TABLES.contains(table))
    {
        let mut statement = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let actual = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if actual.is_empty() {
            return Err(Error::StockSchema(format!("missing table {table}")));
        }
        if actual
            .iter()
            .map(String::as_str)
            .ne(columns.iter().copied())
        {
            return Err(Error::StockSchema(format!("unexpected columns in {table}")));
        }
    }
    let mut statement = connection.prepare("SELECT major,minor FROM schema_version")?;
    let versions = statement
        .query_map([], |row| {
            Ok(SchemaVersion {
                major: int_at(row, 0)?,
                minor: int_at(row, 1)?,
            })
        })?
        .take(2)
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match versions.as_slice() {
        [version] => Ok(*version),
        _ => Err(Error::StockSchema(
            "schema_version must hold one row".into(),
        )),
    }
}

/// Reads an integer as Qt's `QVariant::toInt` would accept it: INTEGER or
/// decimal TEXT. NULL reads as 0.
fn int_at(row: &Row<'_>, column: usize) -> rusqlite::Result<i64> {
    match row.get_ref(column)? {
        ValueRef::Integer(value) => Ok(value),
        ValueRef::Null => Ok(0),
        ValueRef::Text(text) => std::str::from_utf8(text)
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .ok_or_else(|| conversion(column, "integer text")),
        _ => Err(conversion(column, "integer")),
    }
}

/// Reads a boolean as Qt's `QVariant::toBool`: stock stores some flags as INTEGER
/// 0/1 and others as TEXT `"false"`/`"true"`.
fn bool_at(row: &Row<'_>, column: usize) -> rusqlite::Result<bool> {
    match row.get_ref(column)? {
        ValueRef::Integer(value) => Ok(value != 0),
        ValueRef::Real(value) => Ok(value != 0.0),
        ValueRef::Null => Ok(false),
        ValueRef::Text(text) => {
            Ok(!(text.is_empty() || text == b"0" || text.eq_ignore_ascii_case(b"false")))
        }
        ValueRef::Blob(_) => Err(conversion(column, "boolean")),
    }
}

fn real_at(row: &Row<'_>, column: usize) -> rusqlite::Result<f64> {
    match row.get_ref(column)? {
        ValueRef::Real(value) => Ok(value),
        ValueRef::Integer(value) => Ok(value as f64),
        ValueRef::Null => Ok(0.0),
        _ => Err(conversion(column, "real")),
    }
}

/// Reads TEXT, or an INTEGER rendered as decimal text (stock `hash TEXT` columns
/// hold digits).
fn text_at(row: &Row<'_>, column: usize) -> rusqlite::Result<String> {
    match row.get_ref(column)? {
        ValueRef::Text(text) => {
            String::from_utf8(text.to_vec()).map_err(|_| conversion(column, "UTF-8 text"))
        }
        ValueRef::Integer(value) => Ok(value.to_string()),
        _ => Err(conversion(column, "text")),
    }
}

fn blob_at(row: &Row<'_>, column: usize) -> rusqlite::Result<Option<Vec<u8>>> {
    match row.get_ref(column)? {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(bytes) | ValueRef::Text(bytes) => Ok(Some(bytes.to_vec())),
        _ => Err(conversion(column, "blob")),
    }
}

fn conversion(column: usize, expected: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Null,
        format!("stock value is not {expected}").into(),
    )
}

/// Stock TEXT boolean (`"true"`/`"false"`), as found in exp-switch and
/// `isFactory` columns.
struct TextBool(bool);

impl ToSql for TextBool {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(if self.0 { "true" } else { "false" }))
    }
}

/// Read operations. Available on [`StockDb::view`] and inside a transaction.
pub struct StockView<'c> {
    connection: &'c Connection,
    limits: DbLimits,
    /// `None` inside a transaction.
    roots: Option<&'c RootCache>,
}

impl StockView<'_> {
    fn list<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut statement = self.connection.prepare(sql)?;
        let mut items = Vec::new();
        for item in statement.query_map(params, map)? {
            if items.len() == self.limits.max_rows {
                return Err(Error::Limit);
            }
            items.push(item?);
        }
        Ok(items)
    }

    /// `0x01204208`: `SELECT type, readonly, rawdata, metadata FROM product WHERE id`.
    pub fn product(&self, id: &str) -> Result<Option<StockProduct>> {
        Ok(self
            .connection
            .query_row(
                "SELECT type, readonly, rawdata, metadata FROM product WHERE id = ?1",
                [id],
                |row| {
                    Ok(StockProduct {
                        id: id.to_owned(),
                        product_type: int_at(row, 0)?.into(),
                        read_only: bool_at(row, 1)?,
                        raw_data: blob_at(row, 2)?,
                        metadata: blob_at(row, 3)?,
                    })
                },
            )
            .optional()?)
    }

    /// `0x01204318`: ids of one product type, rowid order. NULL ids are skipped.
    pub fn product_ids(&self, product_type: ProductType) -> Result<Vec<String>> {
        self.list(
            "SELECT id FROM product WHERE type = ?1 AND id IS NOT NULL ORDER BY rowid",
            [product_type.code()],
            |row| text_at(row, 0),
        )
    }

    /// The products of a type without their rawdata, rowid order (`raw_data` is `None`): the
    /// listing reads only the metadata (the Add device capture list).
    pub fn product_listing(&self, product_type: ProductType) -> Result<Vec<StockProduct>> {
        self.list(
            "SELECT id, type, readonly, metadata FROM product WHERE type = ?1 AND id IS NOT NULL ORDER BY rowid",
            [product_type.code()],
            |row| {
                Ok(StockProduct {
                    id: text_at(row, 0)?,
                    product_type: int_at(row, 1)?.into(),
                    read_only: bool_at(row, 2)?,
                    raw_data: None,
                    metadata: blob_at(row, 3)?,
                })
            },
        )
    }

    /// [`Self::product`] of each of [`Self::product_ids`], in one statement and without the
    /// rawdata (`raw_data` is `None`). A row that `product` refuses fails alone, with the
    /// same error: the columns keep its numbers, and a rawdata that is not BLOB, TEXT or NULL
    /// fails by its type (SQLite reads the type without the bytes).
    pub fn product_rows(
        &self,
        product_type: ProductType,
    ) -> Result<Vec<(String, Result<StockProduct>)>> {
        self.list(
            "SELECT id, readonly, typeof(rawdata), metadata FROM product WHERE type = ?1 AND id IS NOT NULL ORDER BY rowid",
            [product_type.code()],
            |row| {
                let id = text_at(row, 0)?;
                let product = (|| {
                    let read_only = bool_at(row, 1)?;
                    if !matches!(row.get_ref(2)?, ValueRef::Text(b"null" | b"blob" | b"text")) {
                        return Err(conversion(2, "blob"));
                    }
                    Ok(StockProduct {
                        id: id.clone(),
                        product_type,
                        read_only,
                        raw_data: None,
                        metadata: blob_at(row, 3)?,
                    })
                })();
                Ok((id, product.map_err(Error::from)))
            },
        )
    }

    /// The number of [`Self::product_ids`], without reading them: the same rows, the same
    /// [`Error::Limit`], and the same error for an id that is not TEXT or INTEGER (not for
    /// TEXT that is not UTF-8).
    pub fn product_count(&self, product_type: ProductType) -> Result<usize> {
        self.count(
            "SELECT COUNT(*), COUNT(*) FILTER (WHERE typeof(id) NOT IN ('text', 'integer')) FROM product WHERE type = ?1 AND id IS NOT NULL",
            [product_type.code()],
        )
    }

    /// The number of [`Self::download_ids`], as [`Self::product_count`].
    pub fn download_count(&self, kind: DownloadKind) -> Result<usize> {
        self.count(
            &format!(
                "SELECT COUNT(*), COUNT(*) FILTER (WHERE typeof(id) NOT IN ('text', 'integer')) FROM {} WHERE id IS NOT NULL",
                kind.table()
            ),
            [],
        )
    }

    /// Rows and unreadable ids of a `SELECT COUNT(*), COUNT(*) FILTER (...)`.
    fn count(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        let (rows, unreadable) = self.connection.query_row(sql, params, |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        if unreadable > 0 {
            return Err(conversion(0, "text").into());
        }
        match usize::try_from(rows) {
            Ok(rows) if rows <= self.limits.max_rows => Ok(rows),
            _ => Err(Error::Limit),
        }
    }

    /// `0x01204380`: all folders ordered by id.
    pub fn folders(&self) -> Result<Vec<StockFolder>> {
        self.list(
            "SELECT id, pdType, folderType, name, parent, readonly FROM folder ORDER BY id",
            [],
            |row| {
                Ok(StockFolder {
                    id: int_at(row, 0)?,
                    product_type: int_at(row, 1)?.into(),
                    folder_type: int_at(row, 2)?.into(),
                    name: text_at(row, 3)?,
                    parent: int_at(row, 4)?,
                    read_only: bool_at(row, 5)?,
                })
            },
        )
    }

    /// `0x01204554` (`0x00835204`): the writable user root of a product type. Outside a
    /// transaction the result stays cached until this connection writes ([`RootCache`]):
    /// Cortex Control reads both roots on every device step.
    pub fn root_folder(&self, product_type: ProductType) -> Result<Option<i64>> {
        let code = product_type.code();
        let changes = self.connection.total_changes();
        if let Some(roots) = self.roots {
            let cached = roots.0.borrow();
            if cached.changes == changes
                && let Some(&(_, root)) = cached.roots.iter().find(|(kind, _)| *kind == code)
            {
                return Ok(root);
            }
        }
        let root = self
            .connection
            .query_row(
                "SELECT id FROM folder WHERE pdType = ?1 AND folderType = ?2 AND parent = 0 AND readonly = 0",
                params![code, FolderType::UserRoot.code()],
                |row| int_at(row, 0),
            )
            .optional()?;
        if let Some(roots) = self.roots {
            let mut cached = roots.0.borrow_mut();
            if cached.changes != changes {
                *cached = CachedRoots {
                    changes,
                    roots: Vec::new(),
                };
            }
            cached.roots.push((code, root));
        }
        Ok(root)
    }

    /// `0x01204344`: product ids linked to a folder, insertion order.
    pub fn folder_products(&self, folder: i64) -> Result<Vec<String>> {
        self.list(
            "SELECT productId FROM folder_content WHERE folderId = ?1 ORDER BY rowid",
            [folder],
            |row| text_at(row, 0),
        )
    }

    /// `0x011ee9fc`: pinned model identifiers (stock stores decimal text).
    pub fn pinned_models(&self) -> Result<Vec<String>> {
        self.list("SELECT hash FROM pinned_models ORDER BY rowid", [], |row| {
            text_at(row, 0)
        })
    }

    /// `0x011eea50`: models that have user default parameters.
    pub fn default_param_models(&self) -> Result<Vec<String>> {
        self.list("SELECT DISTINCT model_id FROM default_param", [], |row| {
            text_at(row, 0)
        })
    }

    /// `0x011eea18`: user default parameters of one model, by parameter index.
    pub fn default_params(&self, model_id: &str) -> Result<Vec<ParamEntry>> {
        self.list(
            "SELECT param_id, value, exp, exp_trim_max, exp_trim_min FROM default_param WHERE model_id = ?1 ORDER BY param_id",
            [model_id],
            param_entry,
        )
    }

    /// `0x011eed0c`: the user default expression switch of one model.
    pub fn default_exp_switch(&self, model_id: &str) -> Result<Option<ExpSwitch>> {
        Ok(self
            .connection
            .query_row(
                "SELECT exp_switch_type, exp_switch_inverted, exp_switch_latch_emulation, exp_switch_delay_ms FROM default_exp_switch WHERE model_id = ?1",
                [model_id],
                exp_switch,
            )
            .optional()?)
    }

    /// `0x0120473c` plus the exp-switch columns: every model preset, by id.
    pub fn model_presets(&self) -> Result<Vec<ModelPreset>> {
        self.list(
            "SELECT id, name, hash, expSwitchType, expSwitchInverted, expSwitchLatchEmulation, expSwitchDelayMs FROM model_presets ORDER BY id",
            [],
            model_preset,
        )
    }

    pub fn model_preset(&self, id: i64) -> Result<Option<ModelPreset>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, name, hash, expSwitchType, expSwitchInverted, expSwitchLatchEmulation, expSwitchDelayMs FROM model_presets WHERE id = ?1",
                [id],
                model_preset,
            )
            .optional()?)
    }

    /// `0x0120490c`: parameters of one model preset, by parameter index.
    pub fn model_preset_params(&self, preset_id: i64) -> Result<Vec<ParamEntry>> {
        self.list(
            "SELECT paramIndex, value, exp, expTrimMax, expTrimMin FROM model_preset_parameters WHERE presetId = ?1 ORDER BY paramIndex",
            [preset_id],
            param_entry,
        )
    }

    /// `0x011ee9ac`: the default preset of each model.
    pub fn default_model_presets(&self) -> Result<Vec<DefaultModelPreset>> {
        self.list(
            "SELECT hash, presetId, isFactory FROM default_model_presets",
            [],
            |row| {
                Ok(DefaultModelPreset {
                    model: int_at(row, 0)?,
                    preset_id: int_at(row, 1)?,
                    factory: bool_at(row, 2)?,
                })
            },
        )
    }

    /// `0x011ee730` / `0x011ee784`: a preset by cloud id, or a capture whose
    /// cloud id or capture key (`hash`) equals `key`.
    pub fn download(&self, kind: DownloadKind, key: &str) -> Result<Option<Download>> {
        let sql = match kind {
            DownloadKind::Preset => {
                "SELECT id, NULL, rawdata, metadata FROM downloads_product_preset WHERE id = ?1"
            }
            DownloadKind::Capture => {
                "SELECT id, hash, rawdata, metadata FROM downloads_product_nc WHERE id = ?1 OR hash = ?1 ORDER BY rowid LIMIT 1"
            }
        };
        Ok(self.connection.query_row(sql, [key], download).optional()?)
    }

    /// The Downloads list rows without their rawdata, rowid order: `0x011eeab0` `SELECT id,
    /// metadata, hash FROM downloads_product_nc` (the NC folder cache `0x7ea828`) and
    /// `0x011eea7c` `SELECT id, metadata FROM downloads_product_preset` (`0x6a25b0`). NULL ids
    /// are skipped.
    pub fn download_listing(&self, kind: DownloadKind) -> Result<Vec<Download>> {
        let sql = match kind {
            DownloadKind::Preset => {
                "SELECT id, NULL, NULL, metadata FROM downloads_product_preset WHERE id IS NOT NULL ORDER BY rowid"
            }
            DownloadKind::Capture => {
                "SELECT id, hash, NULL, metadata FROM downloads_product_nc WHERE id IS NOT NULL ORDER BY rowid"
            }
        };
        self.list(sql, [], download)
    }

    /// The Downloads list rows as keys, rowid order; NULL ids are skipped. The columns before
    /// `rawdata` only: a row's `metadata` lies behind its 10 KB `rawdata` overflow pages, which
    /// SQLite walks to reach it, so a listing reads the metadata in parts
    /// ([`Self::download_metadata`]). Host, 389 capture rows: 0.09 ms against 0.28 ms with the
    /// metadata (warm page cache).
    pub fn download_keys(&self, kind: DownloadKind) -> Result<Vec<DownloadKey>> {
        let sql = match kind {
            DownloadKind::Preset => {
                "SELECT rowid, id, NULL FROM downloads_product_preset WHERE id IS NOT NULL ORDER BY rowid"
            }
            DownloadKind::Capture => {
                "SELECT rowid, id, hash FROM downloads_product_nc WHERE id IS NOT NULL ORDER BY rowid"
            }
        };
        self.list(sql, [], |row| {
            Ok(DownloadKey {
                rowid: row.get(0)?,
                id: text_at(row, 1)?,
                hash: row
                    .get_ref(2)?
                    .as_str_or_null()
                    .map_err(|_| conversion(2, "text"))?
                    .map(str::to_owned),
            })
        })
    }

    /// The `metadata` of the Downloads rows whose rowid is in `rows`, rowid order (one range
    /// read per listing part).
    pub fn download_metadata(
        &self,
        kind: DownloadKind,
        rows: std::ops::RangeInclusive<i64>,
    ) -> Result<Vec<(i64, Option<Vec<u8>>)>> {
        self.list(
            &format!(
                "SELECT rowid, metadata FROM {} WHERE rowid BETWEEN ?1 AND ?2 ORDER BY rowid",
                kind.table()
            ),
            [rows.start(), rows.end()],
            |row| Ok((row.get(0)?, blob_at(row, 1)?)),
        )
    }

    /// `0x011ee9e8`: download ids, rowid order. NULL ids are skipped.
    pub fn download_ids(&self, kind: DownloadKind) -> Result<Vec<String>> {
        self.list(
            &format!(
                "SELECT id FROM {} WHERE id IS NOT NULL ORDER BY rowid",
                kind.table()
            ),
            [],
            |row| text_at(row, 0),
        )
    }

    /// `0x011ee8b4`.
    pub fn capture_encryption(&self, store: CaptureStore) -> Result<CaptureEncryption> {
        let code = self
            .connection
            .query_row(
                "SELECT encryption FROM nc_encryption WHERE nc_type = ?1",
                [store.key()],
                |row| int_at(row, 0),
            )
            .optional()?;
        Ok(match code.unwrap_or(0) {
            0 => CaptureEncryption::Unset,
            1 => CaptureEncryption::Local,
            2 => CaptureEncryption::Global,
            other => CaptureEncryption::Other(other),
        })
    }

    fn has_table(&self, table: &str) -> Result<bool> {
        let tables = self.connection.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(tables > 0)
    }

    /// Whether a quarantine `table` has the `name` column (added in r25; older tables lack it).
    fn quarantine_has_name(&self, table: &str) -> Result<bool> {
        let columns = self.list(&format!("PRAGMA table_info({table})"), [], |row| {
            text_at(row, 1)
        })?;
        Ok(columns.iter().any(|column| column == "name"))
    }

    /// [`QUARANTINE_TABLE`] in insert order; empty when the table does not exist. A table
    /// without the `name` column (a read-only view of an older table) reads the id as name.
    pub fn quarantined_captures(&self) -> Result<Vec<QuarantinedCapture>> {
        if !self.has_table(QUARANTINE_TABLE)? {
            return Ok(Vec::new());
        }
        let sql = if self.quarantine_has_name(QUARANTINE_TABLE)? {
            "SELECT store, id, reason, coalesce(name, id) FROM tetrad_capture_quarantine ORDER BY \
             rowid"
        } else {
            "SELECT store, id, reason, id FROM tetrad_capture_quarantine ORDER BY rowid"
        };
        self.list(sql, [], |row| {
            Ok(QuarantinedCapture {
                store: match text_at(row, 0)?.as_str() {
                    "downloads" => CaptureStore::Downloads,
                    _ => CaptureStore::User,
                },
                id: text_at(row, 1)?,
                name: text_at(row, 3)?,
                reason: text_at(row, 2)?,
            })
        })
    }
}

fn param_entry(row: &Row<'_>) -> rusqlite::Result<ParamEntry> {
    Ok(ParamEntry {
        index: int_at(row, 0)?,
        value: text_at(row, 1)?,
        exp: int_at(row, 2)?,
        exp_trim_max: real_at(row, 3)?,
        exp_trim_min: real_at(row, 4)?,
    })
}

fn exp_switch(row: &Row<'_>) -> rusqlite::Result<ExpSwitch> {
    Ok(ExpSwitch {
        switch_type: int_at(row, 0)?,
        inverted: bool_at(row, 1)?,
        latch_emulation: bool_at(row, 2)?,
        delay_ms: int_at(row, 3)?,
    })
}

fn model_preset(row: &Row<'_>) -> rusqlite::Result<ModelPreset> {
    Ok(ModelPreset {
        id: int_at(row, 0)?,
        name: text_at(row, 1)?,
        model: int_at(row, 2)?,
        exp_switch: ExpSwitch {
            switch_type: int_at(row, 3)?,
            inverted: bool_at(row, 4)?,
            latch_emulation: bool_at(row, 5)?,
            delay_ms: int_at(row, 6)?,
        },
    })
}

fn download(row: &Row<'_>) -> rusqlite::Result<Download> {
    Ok(Download {
        id: text_at(row, 0)?,
        hash: row
            .get_ref(1)?
            .as_str_or_null()
            .map_err(|_| conversion(1, "text"))?
            .map(str::to_owned),
        raw_data: blob_at(row, 2)?,
        metadata: blob_at(row, 3)?,
    })
}

/// Write operations; only reachable inside [`StockDb::transaction`].
pub struct StockWrite<'c> {
    view: StockView<'c>,
}

impl<'c> std::ops::Deref for StockWrite<'c> {
    type Target = StockView<'c>;

    fn deref(&self) -> &StockView<'c> {
        &self.view
    }
}

impl StockWrite<'_> {
    fn execute(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        Ok(self.view.connection.execute(sql, params)?)
    }

    /// `0x0120428c`: stock `REPLACE INTO product` with all five columns. An
    /// existing read-only row is refused.
    pub fn put_product(&self, product: &StockProduct) -> Result<()> {
        check_key(&product.id)?;
        if self
            .product(&product.id)?
            .is_some_and(|existing| existing.read_only)
        {
            return Err(Error::ReadOnly);
        }
        self.execute(
            "REPLACE INTO product (id, type, readonly, rawdata, metadata) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                product.id,
                product.product_type.code(),
                i64::from(product.read_only),
                product.raw_data,
                product.metadata
            ],
        )?;
        Ok(())
    }

    /// `0x011ee950`: replaces only the metadata of a writable product.
    pub fn set_product_metadata(&self, id: &str, metadata: Option<&[u8]>) -> Result<()> {
        self.require_writable_product(id)?;
        self.execute(
            "UPDATE product SET metadata = ?2 WHERE id = ?1",
            params![id, metadata],
        )?;
        Ok(())
    }

    /// `0x011ee988`: deletes a writable product. Its folder links are removed in
    /// the same transaction so no dangling `folder_content` row stays.
    pub fn delete_product(&self, id: &str) -> Result<()> {
        self.require_writable_product(id)?;
        self.execute("DELETE FROM folder_content WHERE productId = ?1", [id])?;
        self.execute("DELETE FROM product WHERE id = ?1", [id])?;
        Ok(())
    }

    fn require_writable_product(&self, id: &str) -> Result<()> {
        match self.product(id)? {
            None => Err(Error::ProductMissing(id.to_owned())),
            Some(product) if product.read_only => Err(Error::ReadOnly),
            Some(_) => Ok(()),
        }
    }

    /// `0x012044dc`: inserts a folder and returns its AUTOINCREMENT id.
    pub fn create_folder(&self, folder: &NewFolder<'_>) -> Result<i64> {
        check_key(folder.name)?;
        if folder.parent != 0 {
            self.require_writable_folder(folder.parent)?;
        }
        self.execute(
            "INSERT INTO folder (pdType, folderType, name, parent, readonly) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                folder.product_type.code(),
                folder.folder_type.code(),
                folder.name,
                folder.parent,
                i64::from(folder.read_only)
            ],
        )?;
        Ok(self.view.connection.last_insert_rowid())
    }

    /// Stock get-or-create (`0x00834e10`): the first folder with all equal
    /// fields (`0x01204440`), else a new one.
    pub fn ensure_folder(&self, folder: &NewFolder<'_>) -> Result<i64> {
        let existing = self
            .view
            .connection
            .query_row(
                "SELECT id FROM folder WHERE pdType = ?1 AND folderType = ?2 AND name = ?3 AND parent = ?4 AND readonly = ?5 LIMIT 1",
                params![
                    folder.product_type.code(),
                    folder.folder_type.code(),
                    folder.name,
                    folder.parent,
                    i64::from(folder.read_only)
                ],
                |row| int_at(row, 0),
            )
            .optional()?;
        match existing {
            Some(id) => Ok(id),
            None => self.create_folder(folder),
        }
    }

    /// `0x0120467c`.
    pub fn rename_folder(&self, id: i64, name: &str) -> Result<()> {
        check_key(name)?;
        self.require_writable_folder(id)?;
        self.execute(
            "UPDATE folder SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    /// `0x012046ac` + `0x012046e4`: empties and deletes a writable folder that
    /// has no child folders. Linked products stay in `product`.
    pub fn delete_folder(&self, id: i64) -> Result<()> {
        self.require_writable_folder(id)?;
        let has_children: bool = self.view.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM folder WHERE parent = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if has_children {
            return Err(Error::FolderNotEmpty);
        }
        self.execute("DELETE FROM folder_content WHERE folderId = ?1", [id])?;
        self.execute("DELETE FROM folder WHERE id = ?1", [id])?;
        Ok(())
    }

    fn require_writable_folder(&self, id: i64) -> Result<()> {
        let read_only = self
            .view
            .connection
            .query_row("SELECT readonly FROM folder WHERE id = ?1", [id], |row| {
                bool_at(row, 0)
            })
            .optional()?
            .ok_or(Error::FolderMissing(id))?;
        if read_only {
            return Err(Error::ReadOnly);
        }
        Ok(())
    }

    /// `0x012045c8`: `REPLACE INTO folder_content`; the product must exist.
    pub fn link(&self, folder: i64, product: &str) -> Result<()> {
        self.require_writable_folder(folder)?;
        if self.product(product)?.is_none() {
            return Err(Error::ProductMissing(product.to_owned()));
        }
        self.execute(
            "REPLACE INTO folder_content (folderId, productId) VALUES (?1, ?2)",
            params![folder, product],
        )?;
        Ok(())
    }

    /// `0x01204628`.
    pub fn unlink(&self, folder: i64, product: &str) -> Result<()> {
        self.require_writable_folder(folder)?;
        self.execute(
            "DELETE FROM folder_content WHERE folderId = ?1 AND productId = ?2",
            params![folder, product],
        )?;
        Ok(())
    }

    /// `0x011ee1bc`. The stock table has no key; an existing pin is not repeated.
    pub fn pin_model(&self, model: &str) -> Result<()> {
        check_key(model)?;
        if !self.pinned_models()?.iter().any(|pinned| pinned == model) {
            self.execute("INSERT INTO pinned_models VALUES (?1)", [model])?;
        }
        Ok(())
    }

    /// `0x011ee1f0`.
    pub fn unpin_model(&self, model: &str) -> Result<()> {
        self.execute("DELETE FROM pinned_models WHERE hash = ?1", [model])?;
        Ok(())
    }

    /// Replaces all user default parameters of one model (`0x011ee5d4` then
    /// `0x011ee124` per row).
    pub fn set_default_params(&self, model_id: &str, params: &[ParamEntry]) -> Result<()> {
        check_key(model_id)?;
        self.execute("DELETE FROM default_param WHERE model_id = ?1", [model_id])?;
        self.replace_default_params(model_id, params)
    }

    fn replace_default_params(&self, model_id: &str, params: &[ParamEntry]) -> Result<()> {
        for param in params {
            self.execute(
                "REPLACE INTO default_param VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    model_id,
                    param.index,
                    param.value,
                    param.exp,
                    param.exp_trim_max,
                    param.exp_trim_min
                ],
            )?;
        }
        Ok(())
    }

    /// `0x011ee4c0`, or `0x011ee614` when `exp_switch` is `None`.
    pub fn set_default_exp_switch(
        &self,
        model_id: &str,
        exp_switch: Option<ExpSwitch>,
    ) -> Result<()> {
        check_key(model_id)?;
        match exp_switch {
            Some(exp) => self.execute(
                "REPLACE INTO default_exp_switch VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    model_id,
                    exp.switch_type,
                    TextBool(exp.inverted),
                    TextBool(exp.latch_emulation),
                    exp.delay_ms
                ],
            )?,
            None => self.execute(
                "DELETE FROM default_exp_switch WHERE model_id = ?1",
                [model_id],
            )?,
        };
        Ok(())
    }

    /// Imports one legacy `ZenjackConfig.ini` per-model default group
    /// (`0x006a2edc`): one `REPLACE INTO default_param` per parameter
    /// (`0x0069dcf8`; other stored params of the model stay), then one
    /// `REPLACE INTO default_exp_switch` (`0x0069f4f0`), which stock writes
    /// even when the group has no bypass keys. Stock runs this at boot when the
    /// schema major is not 300 (`0x006a576c` → `0x006a4140`).
    pub fn import_legacy_defaults(
        &self,
        model_id: &str,
        params: &[ParamEntry],
        exp_switch: ExpSwitch,
    ) -> Result<()> {
        check_key(model_id)?;
        self.replace_default_params(model_id, params)?;
        self.set_default_exp_switch(model_id, Some(exp_switch))
    }

    /// `0x011ee598` + `0x011ee5b4`: the "reset all defaults" action.
    pub fn clear_defaults(&self) -> Result<()> {
        self.execute("DELETE FROM default_param", [])?;
        self.execute("DELETE FROM default_exp_switch", [])?;
        Ok(())
    }

    /// `0x0120494c` + `0x01204a28`. Names are unique per model (`0x012047d4`)
    /// and a model holds at most [`MAX_MODEL_PRESETS_PER_MODEL`] presets.
    pub fn create_model_preset(
        &self,
        name: &str,
        model: i64,
        exp_switch: ExpSwitch,
        params: &[ParamEntry],
    ) -> Result<i64> {
        self.check_model_preset_name(name, model, None)?;
        let count: i64 = self.view.connection.query_row(
            "SELECT COUNT(id) FROM model_presets WHERE hash = ?1",
            [model],
            |row| row.get(0),
        )?;
        if count >= MAX_MODEL_PRESETS_PER_MODEL {
            return Err(Error::Limit);
        }
        self.execute(
            "INSERT INTO model_presets (name, hash, expSwitchType, expSwitchInverted, expSwitchLatchEmulation, expSwitchDelayMs) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                name,
                model,
                exp_switch.switch_type,
                TextBool(exp_switch.inverted),
                TextBool(exp_switch.latch_emulation),
                exp_switch.delay_ms
            ],
        )?;
        let id = self.view.connection.last_insert_rowid();
        for param in params {
            self.execute(
                "INSERT INTO model_preset_parameters VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    param.index,
                    param.value,
                    param.exp,
                    param.exp_trim_max,
                    param.exp_trim_min
                ],
            )?;
        }
        Ok(id)
    }

    fn check_model_preset_name(&self, name: &str, model: i64, except: Option<i64>) -> Result<()> {
        check_key(name)?;
        let taken: bool = self.view.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM model_presets WHERE hash = ?1 AND name = ?2 AND id IS NOT ?3)",
            params![model, name, except],
            |row| row.get(0),
        )?;
        if taken {
            return Err(Error::DuplicateName(name.to_owned()));
        }
        Ok(())
    }

    /// `0x011ee220`, with the stock same-model name check (`0x01204810`).
    pub fn rename_model_preset(&self, id: i64, name: &str) -> Result<()> {
        let preset = self.require_model_preset(id)?;
        self.check_model_preset_name(name, preset.model, Some(id))?;
        self.execute(
            "UPDATE model_presets SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    /// `0x01204aa8` + `0x01204b74`: overwrites a preset's switch and parameters.
    /// Parameters absent from the preset are inserted.
    pub fn update_model_preset(
        &self,
        id: i64,
        exp_switch: ExpSwitch,
        params: &[ParamEntry],
    ) -> Result<()> {
        self.require_model_preset(id)?;
        self.execute(
            "UPDATE model_presets SET expSwitchType = ?2, expSwitchInverted = ?3, expSwitchLatchEmulation = ?4, expSwitchDelayMs = ?5 WHERE id = ?1",
            params![
                id,
                exp_switch.switch_type,
                TextBool(exp_switch.inverted),
                TextBool(exp_switch.latch_emulation),
                exp_switch.delay_ms
            ],
        )?;
        for param in params {
            let changed = self.execute(
                "UPDATE model_preset_parameters SET value = ?3, exp = ?4, expTrimMax = ?5, expTrimMin = ?6 WHERE presetId = ?1 AND paramIndex = ?2",
                params![id, param.index, param.value, param.exp, param.exp_trim_max, param.exp_trim_min],
            )?;
            if changed == 0 {
                self.execute(
                    "INSERT INTO model_preset_parameters VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        id,
                        param.index,
                        param.value,
                        param.exp,
                        param.exp_trim_max,
                        param.exp_trim_min
                    ],
                )?;
            }
        }
        Ok(())
    }

    fn require_model_preset(&self, id: i64) -> Result<ModelPreset> {
        self.model_preset(id)?.ok_or(Error::ModelPresetMissing(id))
    }

    /// `0x011ee288` + `0x011ee260` (`0x0069e590`). Inferred addition: a user
    /// default mapping to this preset is removed too, so no default points at
    /// a missing preset.
    pub fn delete_model_preset(&self, id: i64) -> Result<()> {
        self.require_model_preset(id)?;
        self.execute(
            "DELETE FROM model_preset_parameters WHERE presetId = ?1",
            [id],
        )?;
        self.execute(
            "DELETE FROM default_model_presets WHERE presetId = ?1 AND isFactory IN (0, 'false')",
            [id],
        )?;
        self.execute("DELETE FROM model_presets WHERE id = ?1", [id])?;
        Ok(())
    }

    /// `0x011ee360` to set, `0x011ee3ac` to clear the default preset of a model.
    /// `factory` presets live in `factory.db`; a user preset must exist here.
    pub fn set_default_model_preset(&self, model: i64, preset: Option<(i64, bool)>) -> Result<()> {
        match preset {
            Some((preset_id, factory)) => {
                if !factory {
                    self.require_model_preset(preset_id)?;
                }
                self.execute(
                    "REPLACE INTO default_model_presets VALUES (?1, ?2, ?3)",
                    params![model, preset_id, TextBool(factory)],
                )?;
            }
            None => {
                self.execute("DELETE FROM default_model_presets WHERE hash = ?1", [model])?;
            }
        }
        Ok(())
    }

    /// `0x011ee428` + `0x011ee47c`: moves presets and defaults to a new model id.
    pub fn rehash_model_presets(&self, old: i64, new: i64) -> Result<()> {
        self.execute(
            "UPDATE model_presets SET hash = ?2 WHERE hash = ?1",
            params![old, new],
        )?;
        self.execute(
            "UPDATE default_model_presets SET hash = ?2 WHERE hash = ?1",
            params![old, new],
        )?;
        Ok(())
    }

    /// `0x011ee674` / `0x011eeee8`: stock `REPLACE INTO` a downloads table.
    /// Captures require a hash; presets have none.
    pub fn put_download(&self, kind: DownloadKind, item: &Download) -> Result<()> {
        check_key(&item.id)?;
        match (kind, &item.hash) {
            (DownloadKind::Preset, None) => self.execute(
                "REPLACE INTO downloads_product_preset VALUES (?1, ?2, ?3)",
                params![item.id, item.raw_data, item.metadata],
            )?,
            (DownloadKind::Capture, Some(hash)) => self.execute(
                "REPLACE INTO downloads_product_nc VALUES (?1, ?2, ?3, ?4)",
                params![item.id, hash, item.raw_data, item.metadata],
            )?,
            _ => return Err(Error::Identity),
        };
        Ok(())
    }

    /// `0x011eeca4` / `0x011eecc4`: by cloud id, or for captures also by key.
    pub fn delete_download(&self, kind: DownloadKind, key: &str) -> Result<()> {
        let sql = match kind {
            DownloadKind::Preset => "DELETE FROM downloads_product_preset WHERE id = ?1",
            DownloadKind::Capture => "DELETE FROM downloads_product_nc WHERE id = ?1 OR hash = ?1",
        };
        self.execute(sql, [key])?;
        Ok(())
    }

    /// `0x011ee904`. Only valid after the store's rows use `encryption`.
    pub fn set_capture_encryption(
        &self,
        store: CaptureStore,
        encryption: CaptureEncryption,
    ) -> Result<()> {
        self.execute(
            "REPLACE INTO nc_encryption VALUES (?1, ?2)",
            params![store.key(), encryption.code()],
        )?;
        Ok(())
    }

    /// Gives a quarantine `table` without the `name` column the column, with each row's id as
    /// its name.
    fn add_quarantine_name(&self, table: &str) -> Result<()> {
        if !self.quarantine_has_name(table)? {
            self.execute(&format!("ALTER TABLE {table} ADD COLUMN name TEXT"), [])?;
            self.execute(&format!("UPDATE {table} SET name = id"), [])?;
        }
        Ok(())
    }

    /// Moves [`LEGACY_QUARANTINE_TABLE`] to [`QUARANTINE_TABLE`] (O, the zen-rs to Tetrad
    /// rename). The rule keeps every quarantined byte and makes a second run a no-op:
    /// - Old table only: rename it (its rows and columns stay as they are).
    /// - Both (a zen-rs build ran after a Tetrad build, or a zen-rs backup came back): copy
    ///   each old row that the new table does not hold (same store, id, rawdata and metadata),
    ///   then drop the old table.
    /// - New table only, or neither: nothing ([`StockDb::open`] does not call this).
    fn migrate_legacy_quarantine(&self) -> Result<()> {
        if !self.has_table(QUARANTINE_TABLE)? {
            self.execute(
                "ALTER TABLE zen_capture_quarantine RENAME TO tetrad_capture_quarantine",
                [],
            )?;
            return Ok(());
        }
        self.add_quarantine_name(LEGACY_QUARANTINE_TABLE)?;
        self.add_quarantine_name(QUARANTINE_TABLE)?;
        self.execute(
            "INSERT INTO tetrad_capture_quarantine (store, id, hash, rawdata, metadata, reason, \
             name) SELECT store, id, hash, rawdata, metadata, reason, name FROM \
             zen_capture_quarantine AS legacy WHERE NOT EXISTS (SELECT 1 FROM \
             tetrad_capture_quarantine AS kept WHERE kept.store = legacy.store AND kept.id = \
             legacy.id AND kept.rawdata IS legacy.rawdata AND kept.metadata IS legacy.metadata) \
             ORDER BY legacy.rowid",
            [],
        )?;
        self.execute("DROP TABLE zen_capture_quarantine", [])?;
        Ok(())
    }

    /// Takes one capture out of its store as the corruption check `0x702f70` deletes it
    /// (downloads `0x6a48fc`: `id = :id OR hash = :id`; user `0x6a1600`, here with its folder
    /// links), but first copies the row into [`QUARANTINE_TABLE`] (O: Tetrad keeps the bytes).
    /// A user row must be a writable capture. `name` is the dialog's text for the row. An older
    /// table gets the `name` column first; its rows take their id as name.
    pub fn quarantine_capture(
        &self,
        store: CaptureStore,
        id: &str,
        name: &str,
        reason: &str,
    ) -> Result<()> {
        self.execute(QUARANTINE_DDL, [])?;
        self.add_quarantine_name(QUARANTINE_TABLE)?;
        let copied = match store {
            CaptureStore::Downloads => self.execute(
                "INSERT INTO tetrad_capture_quarantine (store, id, hash, rawdata, metadata, reason, \
                 name) SELECT 'downloads', id, hash, rawdata, metadata, ?2, ?3 FROM \
                 downloads_product_nc WHERE id = ?1 OR hash = ?1",
                params![id, reason, name],
            )?,
            CaptureStore::User => {
                if self.product(id)?.map(|row| row.product_type) != Some(ProductType::Capture) {
                    return Err(Error::ProductMissing(id.to_owned()));
                }
                self.require_writable_product(id)?;
                self.execute(
                    "INSERT INTO tetrad_capture_quarantine (store, id, hash, rawdata, metadata, \
                     reason, name) SELECT 'user', id, NULL, rawdata, metadata, ?2, ?3 FROM \
                     product WHERE id = ?1",
                    params![id, reason, name],
                )?
            }
        };
        if copied == 0 {
            return Err(Error::ProductMissing(id.to_owned()));
        }
        match store {
            CaptureStore::Downloads => self.delete_download(DownloadKind::Capture, id),
            CaptureStore::User => self.delete_product(id),
        }
    }

    /// The way back from [`Self::quarantine_capture`] (O, review r23 K5): the last
    /// [`QUARANTINE_TABLE`] row of `store` and `id` returns to its store with its bytes as
    /// stored, then leaves the table. A user capture is writable and goes into its user root
    /// (its other folder links are gone). An id (or Downloads key) that its store holds again
    /// is refused as a [`Error::Conflict`], so no row is replaced.
    pub fn restore_quarantined_capture(&self, store: CaptureStore, id: &str) -> Result<()> {
        let missing = || Error::ProductMissing(id.to_owned());
        if !self.has_table(QUARANTINE_TABLE)? {
            return Err(missing());
        }
        let (rowid, hash): (i64, Option<String>) = self
            .connection
            .query_row(
                "SELECT rowid, hash FROM tetrad_capture_quarantine WHERE store = ?1 AND id = ?2 \
                 ORDER BY rowid DESC LIMIT 1",
                params![store.key(), id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(missing)?;
        match store {
            CaptureStore::Downloads => {
                let keys = [Some(id), hash.as_deref()];
                for key in keys.into_iter().flatten() {
                    if self.download(DownloadKind::Capture, key)?.is_some() {
                        return Err(Error::Conflict(key.to_owned()));
                    }
                }
                self.execute(
                    "INSERT INTO downloads_product_nc (id, hash, rawdata, metadata) SELECT id, \
                     hash, rawdata, metadata FROM tetrad_capture_quarantine WHERE rowid = ?1",
                    [rowid],
                )?;
            }
            CaptureStore::User => {
                if self.product(id)?.is_some() {
                    return Err(Error::Conflict(id.to_owned()));
                }
                self.execute(
                    "INSERT INTO product (id, type, readonly, rawdata, metadata) SELECT id, ?2, \
                     0, rawdata, metadata FROM tetrad_capture_quarantine WHERE rowid = ?1",
                    params![rowid, ProductType::Capture.code()],
                )?;
                if let Some(root) = self.root_folder(ProductType::Capture)? {
                    self.link(root, id)?;
                }
            }
        }
        self.execute(
            "DELETE FROM tetrad_capture_quarantine WHERE rowid = ?1",
            [rowid],
        )?;
        Ok(())
    }

    /// The database part of ZenUI's boot maintenance: remove rows with NULL
    /// ids (`0x006a0598`, `0x006a077c`) and get-or-create the user roots
    /// (`0x006a4140`). Returns the capture and IR root ids. The other part of
    /// `0x006a4140`, the legacy `ZenjackConfig.ini` defaults migration, is
    /// [`Self::import_legacy_defaults`]; the caller supplies the groups.
    pub fn boot_maintenance(&self) -> Result<(i64, i64)> {
        self.execute("DELETE FROM downloads_product_preset WHERE id IS NULL", [])?;
        self.execute("DELETE FROM downloads_product_nc WHERE id IS NULL", [])?;
        self.execute("DELETE FROM product WHERE id IS NULL", [])?;
        let [captures, irs] = USER_ROOTS.map(|(product_type, name)| {
            self.ensure_folder(&NewFolder {
                product_type,
                folder_type: FolderType::UserRoot,
                name,
                parent: 0,
                read_only: false,
            })
        });
        Ok((captures?, irs?))
    }
}

fn check_key(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 4_096 || value.contains('\0') {
        return Err(Error::Identity);
    }
    Ok(())
}
