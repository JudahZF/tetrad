use super::*;
use std::process::Command;

#[test]
fn hot_journal_recovers_after_an_uncommitted_writer_exits() {
    const CHILD_PATH: &str = "TETRAD_STORAGE_SYNTHETIC_CRASH_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let mut library = Library::open_owned(Path::new(&path), Limits::default(), false).unwrap();
        library
            .connection
            .execute_batch("PRAGMA cache_size=1;")
            .unwrap();
        let transaction = library
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute(
                "UPDATE product SET rawdata=?1 WHERE id='crash-fixture'",
                [vec![42_u8; 256 * 1024]],
            )
            .unwrap();
        // Deliberately exit this test's child without Transaction/Connection drop.
        // Small cache forces dirty pages out while the rollback journal is live.
        std::process::exit(73);
    }

    let directory =
        std::env::temp_dir().join(format!("tetrad-storage-crash-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("library.sqlite");
    let original = Product {
        id: "crash-fixture".to_owned(),
        product_type: 1,
        read_only: false,
        data: ProductData {
            raw_data: Some(vec![17; 256 * 1024]),
            metadata: Some(vec![0, 128, 255]),
        },
    };
    let mut library = Library::create_owned(&path, Limits::default()).unwrap();
    library
        .apply(&[Mutation::InsertProduct(&original)])
        .unwrap();
    drop(library);
    let executable = std::env::current_exe().unwrap();
    let mut command = if let Some(runner) = std::env::var_os("TETRAD_STORAGE_TEST_RUNNER") {
        let mut command = Command::new(runner);
        command.arg(executable);
        command
    } else {
        Command::new(executable)
    };
    let status = command
        .args([
            "--exact",
            "recovery_tests::hot_journal_recovers_after_an_uncommitted_writer_exits",
            "--test-threads=1",
        ])
        .env(CHILD_PATH, &path)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(73));
    let journal = directory.join("library.sqlite-journal");
    assert!(fs::metadata(&journal).unwrap().len() > 512);
    let library = Library::open_owned(&path, Limits::default(), false).unwrap();
    assert_eq!(library.product(&original.id).unwrap(), Some(original));
    assert!(!journal.exists());
    drop(library);
    fs::remove_dir_all(directory).unwrap();
}
