//! Read-only checks against copies of a device's `/media/p4` and `/opt/neuraldsp` in
//! `$QC_CORPUS_DIR/rootfs` (docs/development.md, "Corpus tests"). Run with `QC_CORPUS_DIR=<dir>
//! cargo test -p tetrad-library -- --ignored`. Only read methods are called.

use tetrad_storage::media::{Collection, MediaRoot};

use crate::sidecar::Sidecar;
use crate::stock::{ItemList, StockLibrary, StockLimits};

/// `$QC_CORPUS_DIR/rootfs/<path>`, or `None` with a note when the variable is not set.
fn rootfs(path: &str) -> Option<std::path::PathBuf> {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return None;
    };
    Some(std::path::Path::new(&dir).join("rootfs").join(path))
}

#[test]
#[ignore = "reads $QC_CORPUS_DIR/rootfs/media/p4 and rootfs/opt/neuraldsp"]
fn corpus_library_loads_every_preset_and_sidecars_reencode_byte_exact() {
    let (Some(media), Some(factory)) = (rootfs("media/p4"), rootfs("opt/neuraldsp")) else {
        return;
    };
    let library = StockLibrary::open(MediaRoot::new(media), StockLimits::default())
        .unwrap()
        .with_factory(MediaRoot::factory(factory));
    let (mut presets, mut exact) = (0, 0);
    for setlist in library.setlists().unwrap() {
        for path in library.slots(&setlist.name).unwrap().into_iter().flatten() {
            let preset = library.load(&path).unwrap();
            presets += 1;
            let sidecar = preset.sidecar.expect("stock presets have sidecars");
            let original = std::fs::read(
                library
                    .media()
                    .resolve(&path)
                    .unwrap()
                    .with_extension("json"),
            )
            .unwrap();
            assert_eq!(Sidecar::parse(&original).unwrap(), sidecar);
            exact += usize::from(sidecar.encode() == original);
        }
    }
    assert!(presets > 0);
    assert_eq!(
        exact, presets,
        "sidecar layout differs from the stock writer"
    );
    for path in library.factory_slots().unwrap().into_iter().flatten() {
        library.load(&path).unwrap();
    }
    for collection in [Collection::Presets, Collection::Captures, Collection::Irs] {
        library.items(ItemList::Favorites(collection)).unwrap();
        library.items(ItemList::Recents(collection)).unwrap();
    }
    library.recent_searches().unwrap();
    assert!(library.search("a").unwrap().iter().any(|hit| hit.factory));
}
