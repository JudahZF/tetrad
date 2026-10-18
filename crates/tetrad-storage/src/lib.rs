use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

pub mod media;
pub mod search_index;
pub mod stock;

const APPLICATION_ID: i32 = 0x5a45_4e52;
const SCHEMA_VERSION: i32 = 1;
const SCHEMA: &str = "
PRAGMA application_id = 0x5a454e52;
PRAGMA user_version = 1;
CREATE TABLE product (
    id TEXT PRIMARY KEY NOT NULL,
    type INTEGER NOT NULL,
    readonly INTEGER NOT NULL CHECK(readonly IN (0,1)),
    rawdata BLOB,
    metadata BLOB
) STRICT;
CREATE TABLE folder (
    id INTEGER PRIMARY KEY CHECK(id>0),
    pdType INTEGER NOT NULL,
    folderType INTEGER NOT NULL,
    name TEXT NOT NULL,
    parent INTEGER REFERENCES folder(id),
    readonly INTEGER NOT NULL CHECK(readonly IN (0,1))
) STRICT;
CREATE TABLE folder_content (
    folderId INTEGER NOT NULL REFERENCES folder(id),
    productId TEXT NOT NULL REFERENCES product(id),
    PRIMARY KEY(folderId,productId)
) STRICT;
CREATE INDEX folder_parent ON folder(parent);
CREATE INDEX content_product ON folder_content(productId);
";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("not a Tetrad owned database of the supported schema version")]
    NotOwned,
    #[error("database path must be a regular file, not a symlink")]
    NotRegular,
    #[error(
        "SQLite sidecar {0} must be absent for creation and a regular non-symlink file for opening"
    )]
    UnsafeSidecar(PathBuf),
    #[error("storage limit exceeded or invalid limits")]
    Limit,
    #[error("invalid product identity, folder identity or name")]
    Identity,
    #[error("product does not exist: {0}")]
    ProductMissing(String),
    #[error("folder does not exist: {0}")]
    FolderMissing(i64),
    #[error("operation targets read-only data")]
    ReadOnly,
    #[error("folder move would create a cycle")]
    Cycle,
    #[error("folder contains products or child folders")]
    FolderNotEmpty,
    #[error("product changed since it was read: {0}")]
    Conflict(String),
    #[error("batch contains duplicate product identity: {0}")]
    DuplicateProduct(String),
    #[error("not a supported stock zenui.db: {0}")]
    StockSchema(String),
    #[error("stock schema {}.{} is not writable; only 410.1 (CorOS 4.1.0) is", .0.major, .0.minor)]
    StockVersion(stock::SchemaVersion),
    #[error("name already used: {0}")]
    DuplicateName(String),
    #[error("model preset does not exist: {0}")]
    ModelPresetMissing(i64),
    #[error("not a device path below the library root: {0}")]
    DevicePath(String),
    #[error("malformed stock file: {0}")]
    Format(&'static str),
    /// [`stock::StockDb::copy_to`] restarted too often: other connections kept writing.
    #[error("the database kept changing during the copy")]
    CopyContended,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_database_bytes: u64,
    pub max_row_bytes: usize,
    pub max_batch_operations: usize,
    pub max_folder_depth: usize,
    pub max_list_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_database_bytes: 256 * 1024 * 1024,
            max_row_bytes: 32 * 1024 * 1024,
            max_batch_operations: 1_024,
            max_folder_depth: 256,
            max_list_entries: 1_000,
        }
    }
}

impl Limits {
    fn validate(self) -> Result<Self> {
        if self.max_database_bytes < 64 * 1024
            || !(8_192..=i32::MAX as usize).contains(&self.max_row_bytes)
            || !(1..=10_000).contains(&self.max_batch_operations)
            || !(1..=1_024).contains(&self.max_folder_depth)
            || !(1..=10_000).contains(&self.max_list_entries)
        {
            return Err(Error::Limit);
        }
        Ok(self)
    }

    /// Checks the combined rawdata/metadata product budget. It reserves 4096 bytes
    /// of `max_row_bytes` for the identity and SQLite record headers.
    pub fn check_product_data(
        self,
        raw_data: Option<&[u8]>,
        metadata: Option<&[u8]>,
    ) -> Result<()> {
        let limits = self.validate()?;
        let bytes = raw_data
            .map_or(0, <[u8]>::len)
            .checked_add(metadata.map_or(0, <[u8]>::len))
            .ok_or(Error::Limit)?;
        if bytes > limits.max_row_bytes - 4_096 {
            return Err(Error::Limit);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderId(i64);

impl TryFrom<i64> for FolderId {
    type Error = Error;

    fn try_from(value: i64) -> Result<Self> {
        if value <= 0 {
            return Err(Error::Identity);
        }
        Ok(Self(value))
    }
}

impl FolderId {
    pub fn value(self) -> i64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductData {
    pub raw_data: Option<Vec<u8>>,
    pub metadata: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub id: String,
    /// Recovered SQLite integer, not an invented stock type enumeration.
    pub product_type: i64,
    pub read_only: bool,
    pub data: ProductData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub product_type: i64,
    pub folder_type: i64,
    pub name: String,
    pub parent: Option<FolderId>,
    pub read_only: bool,
}

#[derive(Debug)]
pub enum Mutation<'a> {
    InsertProduct(&'a Product),
    ReplaceProductData {
        id: &'a str,
        data: &'a ProductData,
    },
    DeleteProduct {
        id: &'a str,
    },
    CreateFolder(&'a Folder),
    RenameFolder {
        id: FolderId,
        name: &'a str,
    },
    MoveFolder {
        id: FolderId,
        parent: Option<FolderId>,
    },
    DeleteFolder {
        id: FolderId,
    },
    Link {
        folder: FolderId,
        product: &'a str,
    },
    Unlink {
        folder: FolderId,
        product: &'a str,
    },
}

/// Replaces only `rawdata` of a product that must still equal `expected` exactly.
#[derive(Debug)]
pub struct RawDataReplacement<'a> {
    pub expected: &'a Product,
    pub raw_data: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceOutcome {
    /// The row was fresh and already held the bytes; no UPDATE was issued.
    Unchanged,
    Updated,
}

/// An owned database, not a writer for CorOS databases. Schema identity is checked
/// before configuration or mutation. Callers must control the parent directory.
pub struct Library {
    connection: Connection,
    limits: Limits,
    read_only: bool,
}

impl Library {
    pub fn in_memory(limits: Limits) -> Result<Self> {
        let limits = limits.validate()?;
        let mut connection = Connection::open_in_memory()?;
        configure(&connection, limits, false)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SCHEMA)?;
        transaction.commit()?;
        Ok(Self {
            connection,
            limits,
            read_only: false,
        })
    }

    /// Exclusively creates a new private file. Failure can leave a partial new
    /// file; existing targets are never overwritten or automatically removed.
    pub fn create_owned(path: &Path, limits: Limits) -> Result<Self> {
        let limits = limits.validate()?;
        check_sidecars(path, true, limits.max_database_bytes)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        let database_path = path.canonicalize()?;
        let mut connection = Connection::open_with_flags(&database_path, WRITE_FLAGS)?;
        configure(&connection, limits, false)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SCHEMA)?;
        transaction.commit()?;
        file.sync_all()?;
        Ok(Self {
            connection,
            limits,
            read_only: false,
        })
    }

    pub fn open_owned(path: &Path, limits: Limits, read_only: bool) -> Result<Self> {
        let limits = limits.validate()?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(Error::NotRegular);
        }
        if metadata.len() > limits.max_database_bytes {
            return Err(Error::Limit);
        }
        // Read the durable SQLite header without invoking journal recovery. A
        // read-only SQLite probe cannot recover a hot rollback journal; a writer
        // may recover one only after this owned-format gate has passed.
        let mut header = [0_u8; 100];
        File::open(path)?.read_exact(&mut header)?;
        if &header[..16] != b"SQLite format 3\0"
            || header[60..64] != SCHEMA_VERSION.to_be_bytes()
            || header[68..72] != APPLICATION_ID.to_be_bytes()
        {
            return Err(Error::NotOwned);
        }
        check_sidecars(path, false, limits.max_database_bytes)?;
        let flags = if read_only { READ_FLAGS } else { WRITE_FLAGS };
        let database_path = path.canonicalize()?;
        let connection = Connection::open_with_flags(&database_path, flags)?;
        check_identity(&connection)?;
        configure(&connection, limits, read_only)?;
        Ok(Self {
            connection,
            limits,
            read_only,
        })
    }

    /// The entire batch commits or rolls back. Constraints and permission checks
    /// run inside an immediate transaction, including folder ancestry checks.
    pub fn apply(&mut self, mutations: &[Mutation<'_>]) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if mutations.len() > self.limits.max_batch_operations {
            return Err(Error::Limit);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for mutation in mutations {
            apply_mutation(&transaction, mutation, self.limits)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Optimistic rawdata replacement for editors that hold a loaded `Product`.
    /// In one immediate transaction, every row must still match its expected type,
    /// permission, rawdata and metadata, including NULL versus empty BLOBs. A fresh
    /// row whose rawdata already equals the replacement is not updated. Metadata,
    /// type, permission, identity and folder links never change. The batch commits
    /// or rolls back as a whole.
    pub fn replace_raw_data(
        &mut self,
        replacements: &[RawDataReplacement<'_>],
    ) -> Result<Vec<ReplaceOutcome>> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if replacements.len() > self.limits.max_batch_operations {
            return Err(Error::Limit);
        }
        let mut identities = HashSet::with_capacity(replacements.len());
        for RawDataReplacement { expected, raw_data } in replacements {
            if expected.read_only {
                return Err(Error::ReadOnly);
            }
            check_text(&expected.id, 1_024)?;
            if !identities.insert(expected.id.as_str()) {
                return Err(Error::DuplicateProduct(expected.id.clone()));
            }
            self.limits
                .check_product_data(Some(raw_data), expected.data.metadata.as_deref())?;
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut outcomes = Vec::with_capacity(replacements.len());
        for RawDataReplacement { expected, raw_data } in replacements {
            check_unchanged(&transaction, expected)?;
            if expected.data.raw_data.as_deref() == Some(*raw_data) {
                outcomes.push(ReplaceOutcome::Unchanged);
            } else {
                transaction.execute(
                    "UPDATE product SET rawdata=?2 WHERE id=?1",
                    params![expected.id, raw_data],
                )?;
                outcomes.push(ReplaceOutcome::Updated);
            }
        }
        transaction.commit()?;
        Ok(outcomes)
    }

    pub fn product(&self, id: &str) -> Result<Option<Product>> {
        check_text(id, 1_024)?;
        let mut statement = self
            .connection
            .prepare("SELECT id,type,readonly,rawdata,metadata FROM product WHERE id=?1")?;
        let mut rows = statement.query([id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let raw_data = borrowed_blob(row, 3)?;
        let metadata = borrowed_blob(row, 4)?;
        // SQLite's per-column limit does not enforce the combined product budget.
        self.limits.check_product_data(raw_data, metadata)?;
        Ok(Some(Product {
            id: bounded_text(row, 0, 1_024)?,
            product_type: row.get(1)?,
            read_only: row.get(2)?,
            data: ProductData {
                raw_data: raw_data.map(<[u8]>::to_vec),
                metadata: metadata.map(<[u8]>::to_vec),
            },
        }))
    }

    pub fn folder(&self, id: FolderId) -> Result<Option<Folder>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id,pdType,folderType,name,parent,readonly FROM folder WHERE id=?1",
                [id.0],
                |row| {
                    Ok(Folder {
                        id,
                        product_type: row.get(1)?,
                        folder_type: row.get(2)?,
                        name: bounded_text(row, 3, 4_096)?,
                        parent: row.get::<_, Option<i64>>(4)?.map(FolderId),
                        read_only: row.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Stable bytewise identity order with a checked page size and explicit cursor.
    /// This is owned-library ordering, not stock search/collation compatibility.
    pub fn product_ids(&self, after: Option<&str>, count: usize) -> Result<Vec<String>> {
        if count == 0 || count > self.limits.max_list_entries {
            return Err(Error::Limit);
        }
        if let Some(after) = after {
            check_text(after, 1_024)?;
        }
        let mut statement = self.connection.prepare(
            "SELECT id FROM product WHERE (?1 IS NULL OR id>?1 COLLATE BINARY) ORDER BY id COLLATE BINARY LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, count as i64], |row| {
            bounded_text(row, 0, 1_024)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn folder_products(&self, folder: FolderId, count: usize) -> Result<Vec<String>> {
        if count == 0 || count > self.limits.max_list_entries {
            return Err(Error::Limit);
        }
        folder_parent(&self.connection, folder)?;
        let mut statement = self.connection.prepare(
            "SELECT productId FROM folder_content WHERE folderId=?1 ORDER BY productId COLLATE BINARY LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![folder.0, count as i64], |row| {
                bounded_text(row, 0, 1_024)
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
}

pub(crate) const READ_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_ONLY
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_NOFOLLOW);
pub(crate) const WRITE_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_NOFOLLOW);

fn bounded_text(
    row: &rusqlite::Row<'_>,
    column: usize,
    maximum: usize,
) -> rusqlite::Result<String> {
    let value = row.get_ref(column)?;
    let text = value.as_str().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
    })?;
    check_text(text, maximum).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
    })?;
    Ok(text.to_owned())
}

fn borrowed_blob<'row>(
    row: &'row rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<&'row [u8]>> {
    let value = row.get_ref(column)?;
    value.as_blob_or_null().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column, value.data_type(), Box::new(error))
    })
}

fn check_sidecars(path: &Path, creating: bool, max_bytes: u64) -> Result<()> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        match fs::symlink_metadata(&sidecar) {
            Ok(metadata) if creating || !metadata.is_file() => {
                return Err(Error::UnsafeSidecar(sidecar));
            }
            Ok(metadata) if metadata.len() > max_bytes => {
                return Err(Error::Limit);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn check_identity(connection: &Connection) -> Result<()> {
    let id: i32 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if id != APPLICATION_ID || version != SCHEMA_VERSION {
        return Err(Error::NotOwned);
    }
    Ok(())
}

fn configure(connection: &Connection, limits: Limits, read_only: bool) -> Result<()> {
    use rusqlite::limits::Limit;
    connection.busy_timeout(Duration::from_millis(500))?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, limits.max_row_bytes as i32)?;
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 16_384)?;
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")?;
    if read_only {
        connection.execute_batch("PRAGMA query_only=ON;")?;
    } else {
        connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA journal_mode=DELETE;")?;
        let page_size: u32 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
        let pages = limits.max_database_bytes / u64::from(page_size);
        let accepted: u32 =
            connection.query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
                row.get(0)
            })?;
        if u64::from(accepted) > pages {
            return Err(Error::Limit);
        }
    }
    Ok(())
}

fn check_text(value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(Error::Identity);
    }
    Ok(())
}

fn check_data(id: &str, data: &ProductData, limits: Limits) -> Result<()> {
    check_text(id, 1_024)?;
    limits.check_product_data(data.raw_data.as_deref(), data.metadata.as_deref())
}

/// Compares borrowed row BLOBs with the expected product without copying them.
fn check_unchanged(connection: &Connection, expected: &Product) -> Result<()> {
    let mut statement =
        connection.prepare("SELECT type,readonly,rawdata,metadata FROM product WHERE id=?1")?;
    let mut rows = statement.query([&expected.id])?;
    let row = rows
        .next()?
        .ok_or_else(|| Error::ProductMissing(expected.id.clone()))?;
    if row.get::<_, bool>(1)? {
        return Err(Error::ReadOnly);
    }
    if row.get::<_, i64>(0)? != expected.product_type
        || borrowed_blob(row, 2)? != expected.data.raw_data.as_deref()
        || borrowed_blob(row, 3)? != expected.data.metadata.as_deref()
    {
        return Err(Error::Conflict(expected.id.clone()));
    }
    Ok(())
}

fn require_product(connection: &Connection, id: &str, writable: bool) -> Result<()> {
    check_text(id, 1_024)?;
    let readonly: Option<bool> = connection
        .query_row("SELECT readonly FROM product WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    match readonly {
        None => Err(Error::ProductMissing(id.to_owned())),
        Some(true) if writable => Err(Error::ReadOnly),
        Some(_) => Ok(()),
    }
}

fn folder_parent(connection: &Connection, id: FolderId) -> Result<(Option<FolderId>, bool)> {
    connection
        .query_row(
            "SELECT parent,readonly FROM folder WHERE id=?1",
            [id.0],
            |row| Ok((row.get::<_, Option<i64>>(0)?.map(FolderId), row.get(1)?)),
        )
        .optional()?
        .ok_or(Error::FolderMissing(id.0))
}

fn check_ancestry(
    connection: &Connection,
    start: FolderId,
    forbidden: Option<FolderId>,
    limits: Limits,
) -> Result<usize> {
    let mut current = Some(start);
    let mut depth = 0;
    while let Some(id) = current {
        if Some(id) == forbidden {
            return Err(Error::Cycle);
        }
        if depth >= limits.max_folder_depth {
            return Err(Error::Limit);
        }
        let (parent, readonly) = folder_parent(connection, id)?;
        if readonly {
            return Err(Error::ReadOnly);
        }
        current = parent;
        depth += 1;
    }
    Ok(depth)
}

fn apply_mutation(connection: &Connection, mutation: &Mutation<'_>, limits: Limits) -> Result<()> {
    match mutation {
        Mutation::InsertProduct(product) => {
            check_data(&product.id, &product.data, limits)?;
            connection.execute(
                "INSERT INTO product(id,type,readonly,rawdata,metadata) VALUES(?1,?2,?3,?4,?5)",
                params![
                    product.id,
                    product.product_type,
                    product.read_only,
                    product.data.raw_data,
                    product.data.metadata
                ],
            )?;
        }
        Mutation::ReplaceProductData { id, data } => {
            check_data(id, data, limits)?;
            require_product(connection, id, true)?;
            connection.execute(
                "UPDATE product SET rawdata=?2,metadata=?3 WHERE id=?1",
                params![id, data.raw_data, data.metadata],
            )?;
        }
        Mutation::DeleteProduct { id } => {
            require_product(connection, id, true)?;
            // Linked products must be explicitly unlinked first; no hidden cascades.
            connection.execute("DELETE FROM product WHERE id=?1", [id])?;
        }
        Mutation::CreateFolder(folder) => {
            check_text(&folder.name, 4_096)?;
            if let Some(parent) = folder.parent
                && check_ancestry(connection, parent, Some(folder.id), limits)?
                    >= limits.max_folder_depth
            {
                return Err(Error::Limit);
            }
            connection.execute("INSERT INTO folder(id,pdType,folderType,name,parent,readonly) VALUES(?1,?2,?3,?4,?5,?6)",
                params![folder.id.0, folder.product_type, folder.folder_type, folder.name, folder.parent.map(|p| p.0), folder.read_only])?;
        }
        Mutation::RenameFolder { id, name } => {
            check_text(name, 4_096)?;
            check_ancestry(connection, *id, None, limits)?;
            connection.execute("UPDATE folder SET name=?2 WHERE id=?1", params![id.0, name])?;
        }
        Mutation::MoveFolder { id, parent } => {
            check_ancestry(connection, *id, None, limits)?;
            if let Some(parent) = parent {
                check_ancestry(connection, *parent, Some(*id), limits)?;
            }
            // Reject a move whose entire subtree would exceed the depth budget.
            let parent_depth = parent
                .map(|p| check_ancestry(connection, p, Some(*id), limits))
                .transpose()?
                .unwrap_or(0);
            let height: i64 = connection.query_row(
                "WITH RECURSIVE descendants(id,depth) AS (SELECT ?1,1 UNION ALL SELECT f.id,d.depth+1 FROM folder f JOIN descendants d ON f.parent=d.id WHERE d.depth<?2) SELECT max(depth) FROM descendants",
                params![id.0, limits.max_folder_depth as i64 + 1], |row| row.get(0),
            )?;
            if parent_depth + height as usize > limits.max_folder_depth {
                return Err(Error::Limit);
            }
            connection.execute(
                "UPDATE folder SET parent=?2 WHERE id=?1",
                params![id.0, parent.map(|p| p.0)],
            )?;
        }
        Mutation::DeleteFolder { id } => {
            check_ancestry(connection, *id, None, limits)?;
            let occupied: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM folder WHERE parent=?1) OR EXISTS(SELECT 1 FROM folder_content WHERE folderId=?1)", [id.0], |row| row.get(0),
            )?;
            if occupied {
                return Err(Error::FolderNotEmpty);
            }
            connection.execute("DELETE FROM folder WHERE id=?1", [id.0])?;
        }
        Mutation::Link { folder, product } => {
            check_ancestry(connection, *folder, None, limits)?;
            require_product(connection, product, false)?;
            connection.execute(
                "INSERT INTO folder_content(folderId,productId) VALUES(?1,?2)",
                params![folder.0, product],
            )?;
        }
        Mutation::Unlink { folder, product } => {
            check_ancestry(connection, *folder, None, limits)?;
            require_product(connection, product, false)?;
            connection.execute(
                "DELETE FROM folder_content WHERE folderId=?1 AND productId=?2",
                params![folder.0, product],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod corpus_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod stock_tests;
#[cfg(test)]
mod tests;
