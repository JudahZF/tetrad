use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tetrad_storage::{Error as StorageError, Folder, FolderId, Mutation, ProductData};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Synthetic owned databases are created and mutated only through tetrad-storage.
struct Fixture {
    directory: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "tetrad-library-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("owned.sqlite");
        Library::create_owned(&path, tetrad_storage::Limits::default()).unwrap();
        Self { directory, path }
    }

    /// A second, independent tetrad-storage writer acting as a concurrent editor.
    fn storage(&self) -> Library {
        Library::open_owned(&self.path, tetrad_storage::Limits::default(), false).unwrap()
    }

    fn insert(&self, id: &str, raw: Option<&[u8]>, metadata: Option<&[u8]>) {
        self.insert_product(&product(id, 42, false, raw, metadata));
    }

    fn insert_product(&self, product: &Product) {
        self.storage()
            .apply(&[Mutation::InsertProduct(product)])
            .unwrap();
    }

    /// Replaces a row's identity-level fields, which tetrad-storage only allows by
    /// an explicit delete and reinsert in one batch.
    fn reinsert(&self, product: &Product) {
        self.storage()
            .apply(&[
                Mutation::DeleteProduct { id: &product.id },
                Mutation::InsertProduct(product),
            ])
            .unwrap();
    }

    fn library(&self, limits: Limits, access: Access) -> PresetLibrary {
        PresetLibrary::open_owned(&self.path, 42, limits, access).unwrap()
    }

    fn raw(&self, id: &str) -> Vec<u8> {
        self.storage()
            .product(id)
            .unwrap()
            .unwrap()
            .data
            .raw_data
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn product(
    id: &str,
    product_type: i64,
    read_only: bool,
    raw: Option<&[u8]>,
    metadata: Option<&[u8]>,
) -> Product {
    Product {
        id: id.to_owned(),
        product_type,
        read_only,
        data: ProductData {
            raw_data: raw.map(<[u8]>::to_vec),
            metadata: metadata.map(<[u8]>::to_vec),
        },
    }
}

fn wire() -> Vec<u8> {
    vec![
        0x12, 3, b'o', b'l', b'd', // Earlier duplicate name.
        0x92, 0, 0x83, 0, b'A', b'B', b'C', // Noncanonical final name tag/length.
        0x9b, 6, 8, 0x81, 0, 0x9c, 6, // Unknown group 99 and overlong varint.
        0xfa, 1, 2, 0, 7, // Packed scene colors (field 31).
        0xa0, 2, 0, 0xa8, 2, 1, // Explicit independent migration flags 36/37.
        0x7a, 1, b'A', // Scene label (field 15).
    ]
}

#[test]
fn load_edit_save_reopen_preserves_wire_metadata_and_folder_links() {
    let fixture = Fixture::new();
    let original = wire();
    let metadata = [0, 0xff, 0x80, 0, 3];
    fixture.insert("preset", Some(&original), Some(&metadata));
    let folder = Folder {
        id: FolderId::try_from(1).unwrap(),
        product_type: 42,
        folder_type: 7,
        name: "Synthetic".to_owned(),
        parent: None,
        read_only: false,
    };
    fixture
        .storage()
        .apply(&[
            Mutation::CreateFolder(&folder),
            Mutation::Link {
                folder: folder.id,
                product: "preset",
            },
        ])
        .unwrap();
    let mut library = fixture.library(Limits::default(), Access::ReadWrite);
    let mut session = library.load("preset").unwrap();
    assert_eq!(session.id(), "preset");
    assert_eq!(session.metadata(), Some(metadata.as_slice()));
    assert_eq!(session.document().to_bytes(), original);
    session
        .edit(|document| {
            document.rename("XYZ")?;
            document.set_scene_label(0, "B")
        })
        .unwrap();
    let mut expected = original.clone();
    expected[9..12].copy_from_slice(b"XYZ");
    *expected.last_mut().unwrap() = b'B';
    assert_eq!(session.document().to_bytes(), expected);
    assert_eq!(fixture.raw("preset"), original);
    assert_eq!(
        library.save(std::slice::from_mut(&mut session)).unwrap(),
        [SaveOutcome::Updated]
    );
    assert_eq!(
        library.save(std::slice::from_mut(&mut session)).unwrap(),
        [SaveOutcome::Unchanged]
    );
    drop(library);
    let library = fixture.library(Limits::default(), Access::ReadOnly);
    let session = library.load("preset").unwrap();
    assert_eq!(session.document().to_bytes(), expected);
    assert_eq!(session.metadata(), Some(metadata.as_slice()));
    let storage = fixture.storage();
    let stored = storage.product("preset").unwrap().unwrap();
    assert_eq!((stored.product_type, stored.read_only), (42, false));
    assert_eq!(storage.folder_products(folder.id, 10).unwrap(), ["preset"]);
}

#[test]
fn no_op_save_still_checks_metadata_freshness() {
    let fixture = Fixture::new();
    fixture.insert("preset", Some(&wire()), None);
    let mut library = fixture.library(Limits::default(), Access::ReadWrite);
    let mut session = library.load("preset").unwrap();
    session.edit(|document| document.rename("ABC")).unwrap();
    assert_eq!(
        library.save(std::slice::from_mut(&mut session)).unwrap(),
        [SaveOutcome::Unchanged]
    );
    // NULL to empty metadata with identical rawdata is still a concurrent change.
    fixture
        .storage()
        .apply(&[Mutation::ReplaceProductData {
            id: "preset",
            data: &ProductData {
                raw_data: Some(wire()),
                metadata: Some(Vec::new()),
            },
        }])
        .unwrap();
    assert!(matches!(
        library.save(std::slice::from_mut(&mut session)),
        Err(Error::Storage(StorageError::Conflict(_)))
    ));
    assert_eq!(session.metadata(), None);
    assert_eq!(fixture.raw("preset"), wire());
}

#[test]
fn independent_handles_detect_wire_conflicts_and_reject_foreign_sessions() {
    let fixture = Fixture::new();
    fixture.insert("preset", Some(&wire()), Some(&[]));
    let mut first = fixture.library(Limits::default(), Access::ReadWrite);
    let mut second = fixture.library(Limits::default(), Access::ReadWrite);
    let mut one = first.load("preset").unwrap();
    let mut two = second.load("preset").unwrap();
    one.edit(|document| document.rename("one")).unwrap();
    two.edit(|document| document.rename("two")).unwrap();
    first.save(std::slice::from_mut(&mut one)).unwrap();
    assert!(matches!(
        second.save(std::slice::from_mut(&mut two)),
        Err(Error::Storage(StorageError::Conflict(_)))
    ));
    assert_eq!(
        two.document().summary().unwrap().name.as_deref(),
        Some("two")
    );
    assert!(matches!(
        second.save(std::slice::from_mut(&mut one)),
        Err(Error::WrongLibrary)
    ));
    assert_eq!(fixture.raw("preset"), one.document().to_bytes());
}

#[test]
fn late_conflict_rolls_back_the_batch_and_does_not_advance_session_baselines() {
    let fixture = Fixture::new();
    for id in ["a", "b"] {
        fixture.insert(id, Some(&wire()), None);
    }
    let mut library = fixture.library(Limits::default(), Access::ReadWrite);
    let mut sessions = [library.load("a").unwrap(), library.load("b").unwrap()];
    for session in &mut sessions {
        session.edit(|document| document.rename("new")).unwrap();
    }
    fixture.reinsert(&product("b", -7, false, Some(&wire()), None));
    assert!(matches!(
        library.save(&mut sessions),
        Err(Error::Storage(StorageError::Conflict(_)))
    ));
    assert_eq!(fixture.raw("a"), wire());
    assert_eq!(fixture.raw("b"), wire());
    assert_eq!(
        library.save(&mut sessions[..1]).unwrap(),
        [SaveOutcome::Updated]
    );
    assert_eq!(fixture.raw("a"), sessions[0].document().to_bytes());
}

#[test]
fn product_permissions_are_rechecked_at_save_and_read_only_open_never_writes() {
    let fixture = Fixture::new();
    fixture.insert("preset", Some(&wire()), None);
    let mut reader = fixture.library(Limits::default(), Access::ReadOnly);
    let mut session = reader.load("preset").unwrap();
    session.edit(|document| document.rename("new")).unwrap();
    assert!(matches!(
        reader.save(std::slice::from_mut(&mut session)),
        Err(Error::Storage(StorageError::ReadOnly))
    ));
    let mut writer = fixture.library(Limits::default(), Access::ReadWrite);
    let mut session = writer.load("preset").unwrap();
    session.edit(|document| document.rename("new")).unwrap();
    fixture.reinsert(&product("preset", 42, true, Some(&wire()), None));
    assert!(matches!(
        writer.save(std::slice::from_mut(&mut session)),
        Err(Error::Storage(StorageError::ReadOnly))
    ));
    let mut read_only = writer.load("preset").unwrap();
    assert!(read_only.read_only());
    assert!(matches!(
        read_only.edit(|document| document.rename("new")),
        Err(Error::Storage(StorageError::ReadOnly))
    ));
    assert_eq!(fixture.raw("preset"), wire());
}

#[test]
fn missing_null_empty_wrong_type_and_malformed_wire_are_distinct() {
    let fixture = Fixture::new();
    fixture.insert("null", None, Some(&[]));
    fixture.insert("empty", Some(&[]), Some(&[]));
    fixture.insert("bad", Some(&[0]), None);
    fixture.insert_product(&product("wrong", -5, false, Some(&wire()), None));
    let library = fixture.library(Limits::default(), Access::ReadOnly);
    assert!(matches!(
        library.load("missing"),
        Err(Error::Storage(StorageError::ProductMissing(_)))
    ));
    assert!(matches!(
        library.load("null"),
        Err(Error::PresetDataMissing(_))
    ));
    let empty = library.load("empty").unwrap();
    assert!(empty.document().to_bytes().is_empty());
    assert_eq!(empty.metadata(), Some([].as_slice()));
    assert!(matches!(library.load("bad"), Err(Error::Preset(_))));
    assert!(matches!(
        library.load("wrong"),
        Err(Error::ProductType {
            actual: -5,
            expected: 42
        })
    ));
    assert!(matches!(
        library.load("invalid\0id"),
        Err(Error::Storage(StorageError::Identity))
    ));
}

#[test]
fn staged_edit_failures_and_replacement_documents_cannot_bypass_limits() {
    let fixture = Fixture::new();
    fixture.insert("preset", Some(&wire()), None);
    let limits = Limits {
        preset: tetrad_preset::Limits {
            max_bytes: wire().len(),
            ..Default::default()
        },
        ..Default::default()
    };
    let library = fixture.library(limits, Access::ReadWrite);
    let mut session = library.load("preset").unwrap();
    assert!(matches!(
        session.edit(|document| {
            document.rename("new")?;
            document.set_scene_label(1, "missing")
        }),
        Err(Error::Preset(tetrad_preset::Error::Index { .. }))
    ));
    assert_eq!(session.document().to_bytes(), wire());
    assert!(matches!(
        session.edit(|document| {
            *document = PresetDocument::parse(&wire(), tetrad_preset::Limits::default())?;
            document.rename("too large for this session")
        }),
        Err(Error::Preset(tetrad_preset::Error::Limit { .. }))
    ));
    assert_eq!(session.document().to_bytes(), wire());
}

#[test]
fn save_preserves_nonfinite_parameter_bits_without_implicit_sanitization() {
    let fixture = Fixture::new();
    let raw = vec![
        18, 3, b'A', b'B', b'C', // Name.
        90, 19, 42, 17, 8, 1, 18, 13, 8, 1, 42, 9, // Chain/model/parameter/value.
        21, 0, 0, 0xc0, 0x7f, // Synthetic quiet NaN, fixed32 float bits.
        0x98, 6, 0x81, 0, // Unknown nested field with noncanonical varint.
    ];
    fixture.insert("preset", Some(&raw), None);
    let mut library = fixture.library(Limits::default(), Access::ReadWrite);
    let mut session = library.load("preset").unwrap();
    let summary = session.document().summary().unwrap();
    assert!(matches!(
        summary.chains[0].models[0].params[0].values[0],
        Some(tetrad_preset::ParamValue::Float(value)) if value.to_bits() == 0x7fc0_0000
    ));
    assert!(!session.document().validate().unwrap().is_empty());
    session.edit(|document| document.rename("new")).unwrap();
    library.save(std::slice::from_mut(&mut session)).unwrap();
    let mut expected = raw;
    expected[2..5].copy_from_slice(b"new");
    assert_eq!(fixture.raw("preset"), expected);
}

#[test]
fn aggregate_blob_and_batch_budgets_are_checked_without_partial_writes() {
    let fixture = Fixture::new();
    fixture.insert("large", Some(&[0x12, 0]), Some(&vec![0; 4095]));
    fixture.insert("boundary", Some(&[0x12, 0]), Some(&vec![0; 4094]));
    for id in ["a", "b"] {
        fixture.insert(id, Some(&wire()), None);
    }
    let limits = Limits {
        storage: tetrad_storage::Limits {
            max_row_bytes: 8192,
            ..Default::default()
        },
        max_save_bytes: wire().len(),
        ..Default::default()
    };
    let mut library = fixture.library(limits, Access::ReadWrite);
    assert!(matches!(
        library.load("large"),
        Err(Error::Storage(StorageError::Limit))
    ));
    let mut boundary = library.load("boundary").unwrap();
    assert!(matches!(
        boundary.edit(|document| document.rename("x")),
        Err(Error::Storage(StorageError::Limit))
    ));
    assert_eq!(boundary.document().to_bytes(), [0x12, 0]);
    let mut sessions = [library.load("a").unwrap(), library.load("b").unwrap()];
    for session in &mut sessions {
        session.edit(|document| document.rename("new")).unwrap();
    }
    assert!(matches!(
        library.save(&mut sessions),
        Err(Error::Storage(StorageError::Limit))
    ));
    assert_eq!(fixture.raw("a"), wire());
    assert_eq!(fixture.raw("b"), wire());
    assert_eq!(
        library.save(&mut sessions[..1]).unwrap(),
        [SaveOutcome::Updated]
    );
    // Small sessions keep the duplicate within the save-byte budget.
    let mut duplicate = [
        library.load("boundary").unwrap(),
        library.load("boundary").unwrap(),
    ];
    assert!(matches!(
        library.save(&mut duplicate),
        Err(Error::Storage(StorageError::DuplicateProduct(_)))
    ));
}

#[test]
fn foreign_database_and_sidecar_paths_are_refused_before_sqlite_mutation() {
    let fixture = Fixture::new();
    // Clear the owned application ID in the synthetic SQLite header (offset 68).
    let mut before = fs::read(&fixture.path).unwrap();
    before[68..72].fill(0);
    fs::write(&fixture.path, &before).unwrap();
    let journal = fixture.directory.join("owned.sqlite-journal");
    fs::write(&journal, b"synthetic non-SQLite journal").unwrap();
    assert!(matches!(
        PresetLibrary::open_owned(&fixture.path, 42, Limits::default(), Access::ReadWrite),
        Err(Error::Storage(StorageError::NotOwned))
    ));
    assert_eq!(fs::read(&fixture.path).unwrap(), before);
    assert_eq!(fs::read(&journal).unwrap(), b"synthetic non-SQLite journal");
}

#[cfg(unix)]
#[test]
fn symlinked_database_and_dangling_sidecars_are_refused() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let alias = fixture.directory.join("alias.sqlite");
    symlink(&fixture.path, &alias).unwrap();
    assert!(matches!(
        PresetLibrary::open_owned(&alias, 42, Limits::default(), Access::ReadOnly),
        Err(Error::Storage(StorageError::NotRegular))
    ));
    symlink(
        fixture.directory.join("absent"),
        fixture.directory.join("owned.sqlite-wal"),
    )
    .unwrap();
    assert!(matches!(
        PresetLibrary::open_owned(&fixture.path, 42, Limits::default(), Access::ReadWrite),
        Err(Error::Storage(StorageError::UnsafeSidecar(_)))
    ));
}
