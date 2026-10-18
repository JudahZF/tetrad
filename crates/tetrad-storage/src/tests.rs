use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "tetrad-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> PathBuf {
        self.0.join("library.sqlite")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn product(id: &str) -> Product {
    Product {
        id: id.to_owned(),
        product_type: 42,
        read_only: false,
        data: ProductData {
            raw_data: Some(vec![0, 255, 128, 0]),
            metadata: None,
        },
    }
}

fn folder(id: i64, parent: Option<i64>) -> Folder {
    Folder {
        id: FolderId::try_from(id).unwrap(),
        product_type: 42,
        folder_type: -7,
        name: format!("Folder {id}"),
        parent: parent.map(|v| FolderId::try_from(v).unwrap()),
        read_only: false,
    }
}

#[test]
fn public_product_budget_validates_limits_before_subtraction() {
    for max_row_bytes in [0, 4_095, 4_096, 8_191, usize::MAX] {
        let limits = Limits {
            max_row_bytes,
            ..Limits::default()
        };
        assert!(matches!(
            limits.check_product_data(Some(b"abc"), None),
            Err(Error::Limit)
        ));
    }
    let limits = Limits {
        max_row_bytes: 8_192,
        ..Limits::default()
    };
    let payload = vec![0; 4_096];
    assert!(limits.check_product_data(Some(&payload), None).is_ok());
    assert!(matches!(
        limits.check_product_data(Some(&payload), Some(b"x")),
        Err(Error::Limit)
    ));
}

#[test]
fn owned_file_reopens_with_opaque_blobs_and_absent_empty_values_distinct() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut library = Library::create_owned(&path, Limits::default()).unwrap();
    let first = product("opaque");
    let mut second = product("empty");
    second.data = ProductData {
        raw_data: None,
        metadata: Some(Vec::new()),
    };
    library
        .apply(&[
            Mutation::InsertProduct(&first),
            Mutation::InsertProduct(&second),
        ])
        .unwrap();
    drop(library);
    let mut library = Library::open_owned(&path, Limits::default(), true).unwrap();
    assert_eq!(library.product("opaque").unwrap(), Some(first));
    assert_eq!(library.product("empty").unwrap(), Some(second));
    assert!(matches!(library.apply(&[]), Err(Error::ReadOnly)));
    assert_eq!(library.product_ids(None, 1).unwrap(), ["empty"]);
    assert_eq!(library.product_ids(Some("empty"), 2).unwrap(), ["opaque"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn failed_batch_rolls_back_products_links_and_folder_changes_after_reopen() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut library = Library::create_owned(&path, Limits::default()).unwrap();
    let root = folder(1, None);
    library.apply(&[Mutation::CreateFolder(&root)]).unwrap();
    let item = product("new");
    let result = library.apply(&[
        Mutation::InsertProduct(&item),
        Mutation::Link {
            folder: root.id,
            product: &item.id,
        },
        Mutation::RenameFolder {
            id: root.id,
            name: "changed",
        },
        Mutation::Link {
            folder: root.id,
            product: "missing",
        },
    ]);
    assert!(matches!(result, Err(Error::ProductMissing(_))));
    drop(library);
    let library = Library::open_owned(&path, Limits::default(), false).unwrap();
    assert_eq!(library.product(&item.id).unwrap(), None);
    assert_eq!(library.folder(root.id).unwrap(), Some(root.clone()));
    assert!(library.folder_products(root.id, 10).unwrap().is_empty());
}

#[test]
fn foreign_keys_cycles_and_subtree_depth_cannot_break_the_owned_tree() {
    let limits = Limits {
        max_folder_depth: 3,
        ..Limits::default()
    };
    let mut library = Library::in_memory(limits).unwrap();
    let (a, b, c, d) = (
        folder(1, None),
        folder(2, Some(1)),
        folder(3, Some(2)),
        folder(4, None),
    );
    library
        .apply(&[
            Mutation::CreateFolder(&a),
            Mutation::CreateFolder(&b),
            Mutation::CreateFolder(&c),
            Mutation::CreateFolder(&d),
        ])
        .unwrap();
    assert!(matches!(
        library.apply(&[Mutation::MoveFolder {
            id: a.id,
            parent: Some(c.id)
        }]),
        Err(Error::Cycle)
    ));
    assert!(matches!(
        library.apply(&[Mutation::MoveFolder {
            id: a.id,
            parent: Some(d.id)
        }]),
        Err(Error::Limit)
    ));
    assert_eq!(library.folder(a.id).unwrap().unwrap().parent, None);
    assert!(matches!(
        library.apply(&[Mutation::CreateFolder(&folder(5, Some(3)))]),
        Err(Error::Limit)
    ));
    assert!(matches!(
        library.apply(&[Mutation::CreateFolder(&folder(6, Some(999)))]),
        Err(Error::FolderMissing(999))
    ));
    assert!(matches!(
        library.apply(&[Mutation::DeleteFolder { id: b.id }]),
        Err(Error::FolderNotEmpty)
    ));
    library
        .apply(&[
            Mutation::MoveFolder {
                id: c.id,
                parent: Some(d.id),
            },
            Mutation::DeleteFolder { id: b.id },
        ])
        .unwrap();
    let item = product("linked");
    library
        .apply(&[
            Mutation::InsertProduct(&item),
            Mutation::Link {
                folder: a.id,
                product: &item.id,
            },
        ])
        .unwrap();
    assert!(matches!(
        library.apply(&[Mutation::DeleteProduct { id: &item.id }]),
        Err(Error::Sqlite(_))
    ));
    assert_eq!(library.product(&item.id).unwrap(), Some(item.clone()));
    library
        .apply(&[
            Mutation::Unlink {
                folder: a.id,
                product: &item.id,
            },
            Mutation::DeleteProduct { id: &item.id },
        ])
        .unwrap();
    assert!(library.product(&item.id).unwrap().is_none());
}

#[test]
fn read_only_products_and_folder_ancestry_are_enforced_inside_batches() {
    let mut library = Library::in_memory(Limits::default()).unwrap();
    let mut item = product("factory-like");
    item.read_only = true;
    let mut root = folder(1, None);
    root.read_only = true;
    let destination = folder(2, None);
    library
        .apply(&[
            Mutation::InsertProduct(&item),
            Mutation::CreateFolder(&root),
            Mutation::CreateFolder(&destination),
        ])
        .unwrap();
    for mutation in [
        Mutation::DeleteProduct { id: &item.id },
        Mutation::ReplaceProductData {
            id: &item.id,
            data: &ProductData {
                raw_data: None,
                metadata: None,
            },
        },
        Mutation::RenameFolder {
            id: root.id,
            name: "renamed",
        },
        Mutation::MoveFolder {
            id: destination.id,
            parent: Some(root.id),
        },
        Mutation::Link {
            folder: root.id,
            product: &item.id,
        },
    ] {
        assert!(matches!(library.apply(&[mutation]), Err(Error::ReadOnly)));
    }
    // A read-only product may be referenced by a writable user folder.
    library
        .apply(&[Mutation::Link {
            folder: destination.id,
            product: &item.id,
        }])
        .unwrap();
    assert_eq!(
        library.folder_products(destination.id, 1).unwrap(),
        [item.id]
    );
}

#[test]
fn creation_refuses_existing_targets_and_stock_shaped_databases_are_not_owned() {
    let fixture = Fixture::new();
    let path = fixture.path();
    fs::write(&path, b"existing").unwrap();
    assert!(
        matches!(Library::create_owned(&path, Limits::default()), Err(Error::Io(error)) if error.kind()==std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read(&path).unwrap(), b"existing");
    let stock = fixture.0.join("synthetic-stock.sqlite");
    let connection = Connection::open(&stock).unwrap();
    connection.execute_batch("CREATE TABLE product(id TEXT PRIMARY KEY,rawdata BLOB); INSERT INTO product VALUES('synthetic',X'0001FF');").unwrap();
    drop(connection);
    let before = fs::read(&stock).unwrap();
    assert!(matches!(
        Library::open_owned(&stock, Limits::default(), false),
        Err(Error::NotOwned)
    ));
    assert_eq!(fs::read(&stock).unwrap(), before);
    #[cfg(unix)]
    {
        let alias = fixture.0.join("alias.sqlite");
        std::os::unix::fs::symlink(&stock, &alias).unwrap();
        assert!(matches!(
            Library::open_owned(&alias, Limits::default(), false),
            Err(Error::NotRegular)
        ));
        assert!(matches!(
            Library::create_owned(&alias, Limits::default()),
            Err(Error::Io(_))
        ));
        assert_eq!(fs::read(&stock).unwrap(), before);
    }
}

#[test]
fn creation_refuses_preexisting_sidecars_without_creating_a_database() {
    for suffix in ["-journal", "-wal", "-shm"] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let sidecar = fixture.0.join(format!("library.sqlite{suffix}"));
        fs::write(&sidecar, b"preserve this sidecar").unwrap();
        assert!(matches!(
            Library::create_owned(&path, Limits::default()),
            Err(Error::UnsafeSidecar(_))
        ));
        assert!(!path.exists());
        assert_eq!(fs::read(&sidecar).unwrap(), b"preserve this sidecar");
        #[cfg(unix)]
        {
            fs::rename(&sidecar, fixture.0.join("preserved-sidecar")).unwrap();
            let target = fixture.0.join("absent-target");
            std::os::unix::fs::symlink(&target, &sidecar).unwrap();
            assert!(matches!(
                Library::create_owned(&path, Limits::default()),
                Err(Error::UnsafeSidecar(_))
            ));
            assert!(!path.exists());
            assert!(!target.exists());
            assert_eq!(fs::read_link(&sidecar).unwrap(), target);
        }
    }
}

#[cfg(unix)]
#[test]
fn owned_reopen_rejects_present_and_dangling_sidecar_links_before_sqlite() {
    use std::os::unix::fs::symlink;
    for suffix in ["-journal", "-wal", "-shm"] {
        let fixture = Fixture::new();
        let path = fixture.path();
        drop(Library::create_owned(&path, Limits::default()).unwrap());
        let before = fs::read(&path).unwrap();
        let target = fixture.0.join("target");
        let intermediate = fixture.0.join("chain");
        symlink("target", &intermediate).unwrap();
        let sidecar = fixture.0.join(format!("library.sqlite{suffix}"));
        symlink("chain", &sidecar).unwrap();
        let directory_alias = fixture.0.join("directory-alias");
        symlink(&fixture.0, &directory_alias).unwrap();
        for present in [false, true] {
            if present {
                fs::write(&target, b"preserve target").unwrap();
            }
            for database in [&path, &directory_alias.join("library.sqlite")] {
                for read_only in [true, false] {
                    assert!(matches!(
                        Library::open_owned(database, Limits::default(), read_only),
                        Err(Error::UnsafeSidecar(_))
                    ));
                    assert_eq!(fs::read(&path).unwrap(), before);
                    assert_eq!(fs::read_link(&sidecar).unwrap(), PathBuf::from("chain"));
                    assert_eq!(
                        fs::read_link(&intermediate).unwrap(),
                        PathBuf::from("target")
                    );
                    if present {
                        assert_eq!(fs::read(&target).unwrap(), b"preserve target");
                    } else {
                        assert!(!target.exists());
                    }
                }
            }
        }
        fs::rename(&sidecar, fixture.0.join("preserved-link")).unwrap();
        drop(Library::open_owned(&path, Limits::default(), true).unwrap());
        drop(Library::open_owned(&path, Limits::default(), false).unwrap());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::read(&target).unwrap(), b"preserve target");
    }
}

#[test]
fn tighter_reopen_limits_reject_combined_blobs_before_copying() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut library = Library::create_owned(&path, Limits::default()).unwrap();
    let mut item = product("aggregate-limit");
    item.data = ProductData {
        raw_data: Some(vec![7; 6_000]),
        metadata: Some(vec![8; 6_000]),
    };
    library.apply(&[Mutation::InsertProduct(&item)]).unwrap();
    drop(library);
    let before = fs::read(&path).unwrap();
    let limits = Limits {
        max_row_bytes: 8_192,
        ..Limits::default()
    };
    for read_only in [true, false] {
        let library = Library::open_owned(&path, limits, read_only).unwrap();
        assert!(matches!(library.product(&item.id), Err(Error::Limit)));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let library = Library::open_owned(&path, Limits::default(), true).unwrap();
    assert_eq!(library.product(&item.id).unwrap(), Some(item));
}

#[test]
fn bounds_duplicates_and_missing_objects_fail_without_partial_updates() {
    let limits = Limits {
        max_row_bytes: 8_192,
        max_batch_operations: 2,
        max_list_entries: 2,
        ..Limits::default()
    };
    let mut library = Library::in_memory(limits).unwrap();
    let mut item = product("bounded");
    item.data.raw_data = Some(vec![0; 4_097]);
    assert!(matches!(
        library.apply(&[Mutation::InsertProduct(&item)]),
        Err(Error::Limit)
    ));
    item.data.raw_data = Some(vec![0; 4_096]);
    library.apply(&[Mutation::InsertProduct(&item)]).unwrap();
    let replacement = ProductData {
        raw_data: Some(vec![42]),
        metadata: Some(vec![255]),
    };
    assert!(matches!(
        library.apply(&[
            Mutation::ReplaceProductData {
                id: &item.id,
                data: &replacement
            },
            Mutation::InsertProduct(&item),
        ]),
        Err(Error::Sqlite(_))
    ));
    assert_eq!(library.product(&item.id).unwrap(), Some(item));
    assert!(matches!(
        library.apply(&[Mutation::DeleteProduct { id: "absent" }]),
        Err(Error::ProductMissing(_))
    ));
    assert!(matches!(library.product_ids(None, 3), Err(Error::Limit)));
    assert!(matches!(library.product(""), Err(Error::Identity)));
    assert!(FolderId::try_from(0).is_err());
    assert!(
        Library::in_memory(Limits {
            max_folder_depth: 0,
            ..Limits::default()
        })
        .is_err()
    );
}

#[test]
fn raw_data_replacement_checks_whole_snapshot_and_updates_only_rawdata() {
    let mut library = Library::in_memory(Limits::default()).unwrap();
    let a = product("a");
    let mut b = product("b");
    b.data.metadata = Some(Vec::new());
    let root = folder(1, None);
    library
        .apply(&[
            Mutation::InsertProduct(&a),
            Mutation::InsertProduct(&b),
            Mutation::CreateFolder(&root),
            Mutation::Link {
                folder: root.id,
                product: "a",
            },
        ])
        .unwrap();
    library
        .connection
        .execute_batch(
            "CREATE TRIGGER forbid_update BEFORE UPDATE ON product
             BEGIN SELECT RAISE(ABORT,'no UPDATE expected'); END;",
        )
        .unwrap();
    let same = a.data.raw_data.clone().unwrap();
    assert_eq!(
        library
            .replace_raw_data(&[RawDataReplacement {
                expected: &a,
                raw_data: &same,
            }])
            .unwrap(),
        [ReplaceOutcome::Unchanged]
    );
    library
        .connection
        .execute_batch("DROP TRIGGER forbid_update;")
        .unwrap();

    // Each stale field of the second snapshot rolls back the valid first update.
    let mut stale_type = b.clone();
    stale_type.product_type = -1;
    let mut stale_raw = b.clone();
    stale_raw.data.raw_data = None;
    let mut stale_metadata = b.clone();
    stale_metadata.data.metadata = None;
    for stale in [&stale_type, &stale_raw, &stale_metadata] {
        assert!(matches!(
            library.replace_raw_data(&[
                RawDataReplacement {
                    expected: &a,
                    raw_data: b"new",
                },
                RawDataReplacement {
                    expected: stale,
                    raw_data: b"new",
                },
            ]),
            Err(Error::Conflict(id)) if id == "b"
        ));
        assert_eq!(library.product("a").unwrap().unwrap(), a);
    }
    let missing = product("missing");
    assert!(matches!(
        library.replace_raw_data(&[RawDataReplacement {
            expected: &missing,
            raw_data: b"new",
        }]),
        Err(Error::ProductMissing(_))
    ));

    assert_eq!(
        library
            .replace_raw_data(&[
                RawDataReplacement {
                    expected: &a,
                    raw_data: b"new",
                },
                RawDataReplacement {
                    expected: &b,
                    raw_data: &[],
                },
            ])
            .unwrap(),
        [ReplaceOutcome::Updated, ReplaceOutcome::Updated]
    );
    let mut expected_a = a.clone();
    expected_a.data.raw_data = Some(b"new".to_vec());
    let mut expected_b = b.clone();
    expected_b.data.raw_data = Some(Vec::new());
    assert_eq!(library.product("a").unwrap(), Some(expected_a));
    assert_eq!(library.product("b").unwrap(), Some(expected_b));
    assert_eq!(library.folder_products(root.id, 10).unwrap(), ["a"]);
}

#[test]
fn raw_data_replacement_refusals_and_sqlite_failures_leave_rows_unchanged() {
    let limits = Limits {
        max_row_bytes: 8_192,
        max_batch_operations: 2,
        ..Limits::default()
    };
    let mut library = Library::in_memory(limits).unwrap();
    let a = product("a");
    let b = product("b");
    let mut locked = product("locked");
    locked.read_only = true;
    for product in [&a, &b, &locked] {
        library.apply(&[Mutation::InsertProduct(product)]).unwrap();
    }
    let replace = |expected| RawDataReplacement {
        expected,
        raw_data: b"new",
    };
    let refused = [
        library.replace_raw_data(&[replace(&a), replace(&a)]),
        library.replace_raw_data(&[replace(&a), replace(&b), replace(&a)]),
        library.replace_raw_data(&[replace(&locked)]),
        library.replace_raw_data(&[RawDataReplacement {
            expected: &a,
            raw_data: &[0; 4_097],
        }]),
    ];
    assert!(matches!(refused[0], Err(Error::DuplicateProduct(_))));
    assert!(matches!(refused[1], Err(Error::Limit)));
    assert!(matches!(refused[2], Err(Error::ReadOnly)));
    assert!(matches!(refused[3], Err(Error::Limit)));

    // A row made read-only after the caller's read is refused at commit time.
    let mut writable = locked.clone();
    writable.read_only = false;
    assert!(matches!(
        library.replace_raw_data(&[replace(&writable)]),
        Err(Error::ReadOnly)
    ));

    library
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_b BEFORE UPDATE ON product WHEN NEW.id='b'
             BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        library.replace_raw_data(&[replace(&a), replace(&b)]),
        Err(Error::Sqlite(_))
    ));
    assert_eq!(library.product("a").unwrap(), Some(a.clone()));
    assert_eq!(library.product("b").unwrap(), Some(b.clone()));
    library
        .connection
        .execute_batch("DROP TRIGGER fail_b;")
        .unwrap();
    assert_eq!(
        library
            .replace_raw_data(&[replace(&a), replace(&b)])
            .unwrap(),
        [ReplaceOutcome::Updated, ReplaceOutcome::Updated]
    );
}
