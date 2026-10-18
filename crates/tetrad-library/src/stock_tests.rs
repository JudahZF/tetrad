use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use tetrad_preset::PresetDocument;
use tetrad_storage::media::{Collection, MediaRoot, SETLIST_SLOTS, TEMP_MARKER, encode_setlist};
use tetrad_storage::search_index::{SearchChanges, SearchEntry, SearchIndex, folder_key};

use crate::Error;
use crate::sidecar::Sidecar;
use crate::stock::*;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A synthetic `/media/p4` copy with an empty "My Presets" setlist.
struct Media(PathBuf);

impl Media {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-library-stock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("Presets/My Presets")).unwrap();
        fs::write(
            root.join("Presets/My Presets/setlist"),
            encode_setlist(&vec![None; SETLIST_SLOTS]).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("user_preset_folders"),
            "/media/p4/Presets/My Presets\n",
        )
        .unwrap();
        Self(root)
    }

    fn library(&self) -> StockLibrary {
        StockLibrary::open(MediaRoot::new(&self.0), StockLimits::default()).unwrap()
    }

    fn read(&self, relative: &str) -> String {
        String::from_utf8(fs::read(self.0.join(relative)).unwrap()).unwrap()
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn document(name: &str) -> PresetDocument {
    let mut wire = vec![0x12, name.len() as u8];
    wire.extend_from_slice(name.as_bytes());
    PresetDocument::parse(&wire, tetrad_preset::Limits::default()).unwrap()
}

fn sidecar(name: &str) -> Sidecar {
    Sidecar::new_preset(
        name,
        UNIX_EPOCH + Duration::from_secs(1_791_635_696),
        "4.1.0",
    )
}

/// The value of a saved change whose search index followed it.
fn saved<T>(result: crate::Result<Saved<T>>) -> T {
    let saved = result.unwrap();
    assert!(saved.search_index.is_none(), "{:?}", saved.search_index);
    saved.value
}

/// The value of a committed change whose tail and search index followed it.
fn committed<T>(result: crate::Result<Committed<T>>) -> T {
    let committed = result.unwrap();
    assert!(
        committed.search_index.is_none(),
        "{:?}",
        committed.search_index
    );
    assert!(committed.tail_error.is_none(), "{:?}", committed.tail_error);
    committed.value
}

fn item(path: &str) -> ItemRef {
    ItemRef {
        folder: path.rsplit_once('/').unwrap().0.to_owned(),
        item: path.to_owned(),
    }
}

#[test]
fn sidecar_uses_the_stock_writer_layout_and_utc_date() {
    let bytes = sidecar("Verse \"A\"").encode();
    assert_eq!(
        String::from_utf8(bytes.clone()).unwrap(),
        r#"{"author": "","authorId": "","cloudId": "","corosVersion": "4.1.0","creator": "","date": "2026-10-10T12:34:56Z","description": "","instrument_type": "instrument_not_set","name": "Verse \"A\"","tags": []}"#
    );
    assert_eq!(Sidecar::parse(&bytes).unwrap(), sidecar("Verse \"A\""));
    assert!(matches!(Sidecar::parse(b"[]"), Err(Error::Sidecar)));
}

#[test]
fn store_names_files_after_presets_and_saves_only_changes() {
    let media = Media::new();
    let mut library = media.library();
    let paths: Vec<_> = (0..3)
        .map(|slot| {
            saved(library.store(
                DEFAULT_SETLIST,
                slot,
                "Lead",
                &document("Lead"),
                &sidecar("Lead"),
            ))
        })
        .collect();
    assert_eq!(
        paths,
        [
            "/media/p4/Presets/My Presets/Lead.pb",
            "/media/p4/Presets/My Presets/Lead_1.pb",
            "/media/p4/Presets/My Presets/Lead_2.pb",
        ]
    );
    assert!(
        media
            .read("Presets/My Presets/setlist")
            .starts_with(&format!(
                "{}\n{}\n{}\nempty\n",
                paths[0], paths[1], paths[2]
            ))
    );
    assert!(matches!(
        library.store(DEFAULT_SETLIST, 0, "X", &document("X"), &sidecar("X")),
        Err(Error::SlotOccupied(0))
    ));

    let mut preset = library.load(&paths[0]).unwrap();
    assert_eq!(preset.sidecar.as_ref().unwrap().text("name"), Some("Lead"));
    assert_eq!(library.save(&mut preset).unwrap(), StockSave::Unchanged);
    preset
        .sidecar
        .as_mut()
        .unwrap()
        .set_text("description", "loud");
    assert_eq!(library.save(&mut preset).unwrap(), StockSave::Written);
    assert!(
        media
            .read("Presets/My Presets/Lead.json")
            .contains(r#""description": "loud""#)
    );
    assert_eq!(library.save(&mut preset).unwrap(), StockSave::Unchanged);
}

#[test]
fn favorites_recents_and_searches_follow_stock_order_and_caps() {
    let media = Media::new();
    let mut library = media.library();
    let recents = ItemList::Recents(Collection::Presets);
    for n in 0..=MAX_FAVORITES_OR_RECENTS {
        library
            .push_item(
                recents,
                &item(&format!("/media/p4/Presets/My Presets/{n}.pb")),
            )
            .unwrap();
    }
    library
        .push_item(recents, &item("/media/p4/Presets/My Presets/10.pb"))
        .unwrap();
    let entries = library.items(recents).unwrap();
    assert_eq!(entries.len(), MAX_FAVORITES_OR_RECENTS);
    assert_eq!(entries[0].item, "/media/p4/Presets/My Presets/10.pb");
    assert_eq!(entries[1].item, "/media/p4/Presets/My Presets/64.pb");
    assert!(
        media
            .read("recents")
            .starts_with("/media/p4/Presets/My Presets\n/media/p4/Presets/My Presets/10.pb\n")
    );

    let captures = ItemList::Favorites(Collection::Captures);
    let capture = ItemRef {
        folder: "local_nc_root".into(),
        item: "ab".repeat(32),
    };
    library.push_item(captures, &capture).unwrap();
    assert_eq!(
        media.read("favorites-nc"),
        format!("local_nc_root\n{}\n", capture.item)
    );
    library.remove_item(captures, &capture).unwrap();
    assert_eq!(media.read("favorites-nc"), "");

    for n in 0..=MAX_RECENT_SEARCHES {
        library.push_recent_search(&format!("q{n}")).unwrap();
    }
    library.push_recent_search("Q21").unwrap();
    let searches = library.recent_searches().unwrap();
    assert_eq!(searches.len(), MAX_RECENT_SEARCHES);
    assert_eq!(searches[..2], ["Q21", "q21"]);

    // `0x007d0224`: a trailing odd line is left out; the pairs before it stay (review r11 N5).
    let pair = "/media/p4/Presets/My Presets\n/media/p4/Presets/My Presets/1.pb\n";
    fs::write(
        media.0.join("favorites"),
        format!("{pair}/media/p4/Presets\n"),
    )
    .unwrap();
    assert_eq!(
        library
            .items(ItemList::Favorites(Collection::Presets))
            .unwrap(),
        [item("/media/p4/Presets/My Presets/1.pb")]
    );
}

#[test]
fn setlist_and_preset_renames_rewrite_every_reference() {
    let media = Media::new();
    let mut library = media.library();
    library.create_setlist("Live").unwrap();
    let path = saved(library.store("Live", 5, "Solo", &document("Solo"), &sidecar("Solo")));
    library
        .push_item(ItemList::Favorites(Collection::Presets), &item(&path))
        .unwrap();
    saved(library.rename_setlist("Live", "Tour"));
    let moved = "/media/p4/Presets/Tour/Solo.pb";
    assert_eq!(library.slots("Tour").unwrap()[5].as_deref(), Some(moved));
    assert_eq!(
        media.read("favorites"),
        format!("/media/p4/Presets/Tour\n{moved}\n")
    );
    assert_eq!(
        media.read("user_preset_folders"),
        "/media/p4/Presets/My Presets\n/media/p4/Presets/Tour\n"
    );

    let renamed = saved(library.rename_preset("Tour", 5, "Lead"));
    assert_eq!(renamed, "/media/p4/Presets/Tour/Lead.pb");
    assert!(!media.0.join("Presets/Tour/Solo.pb").exists());
    let preset = library.load(&renamed).unwrap();
    assert_eq!(
        preset.document.summary().unwrap().name.as_deref(),
        Some("Lead")
    );
    assert_eq!(preset.sidecar.unwrap().text("name"), Some("Lead"));
    assert!(media.read("favorites").ends_with(&format!("{renamed}\n")));

    assert!(matches!(
        library.delete_setlist("Tour"),
        Err(Error::SetlistNotEmpty(_))
    ));
    saved(library.delete("Tour", 5));
    assert_eq!(media.read("favorites"), "");
    assert!(!media.0.join("Presets/Tour/Lead.json").exists());
    library.delete_setlist("Tour").unwrap();
    assert_eq!(library.setlists().unwrap().len(), 1);
}

#[test]
fn setlists_follow_user_folder_order_then_names_and_are_capped() {
    let media = Media::new();
    for name in ["b", "A", "C"] {
        fs::create_dir(media.0.join("Presets").join(name)).unwrap();
    }
    fs::write(
        media.0.join("user_preset_folders"),
        "/media/p4/Presets/C\n/media/p4/Presets/Gone\n/media/p4/Presets/My Presets\n",
    )
    .unwrap();
    let mut library = media.library();
    let names: Vec<_> = library
        .setlists()
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["C", "My Presets", "A", "b"]);
    for n in names.len()..MAX_SETLISTS {
        library.create_setlist(&format!("S{n}")).unwrap();
    }
    assert!(matches!(
        library.create_setlist("Extra"),
        Err(Error::SetlistsFull)
    ));
    assert!(library.create_setlist("../x").is_err());
}

#[test]
fn search_uses_index_and_changes_and_rebuilds_in_stock_order() {
    let media = Media::new();
    let capture = "cd".repeat(32);
    let gone = "/media/p4/Presets/My Presets/Old Plexi.pb";
    let index = SearchIndex::build(vec![
        SearchEntry {
            name: "old plexi".into(),
            folder_key: folder_key("/media/p4/Presets/My Presets"),
            path: gone.into(),
        },
        SearchEntry {
            name: "plexi capture".into(),
            folder_key: folder_key("local_nc_root"),
            path: capture.clone(),
        },
    ])
    .unwrap();
    fs::write(media.0.join("searchIndexUser"), index.encode()).unwrap();
    let mut library = media.library();
    let added = saved(library.store(
        DEFAULT_SETLIST,
        1,
        "Plexi Crunch",
        &document("P"),
        &sidecar("P"),
    ));
    saved(library.store(DEFAULT_SETLIST, 0, "Clean", &document("C"), &sidecar("C")));
    let changes = SearchChanges {
        removed: vec![(folder_key("/media/p4/Presets/My Presets"), gone.into())],
        ..SearchChanges::decode(
            &fs::read(media.0.join("searchRecentChangesUser")).unwrap(),
            9,
        )
        .unwrap()
    };
    fs::write(media.0.join("searchRecentChangesUser"), changes.encode()).unwrap();

    let hits = |library: &StockLibrary| -> Vec<String> {
        library
            .search("PLEXI")
            .unwrap()
            .into_iter()
            .map(|hit| hit.path)
            .collect()
    };
    assert_eq!(hits(&library), [capture.clone(), added.clone()]);

    library.rebuild_search_index().unwrap();
    let rebuilt =
        SearchIndex::decode(&fs::read(media.0.join("searchIndexUser")).unwrap(), 9).unwrap();
    let names: Vec<_> = rebuilt.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["clean", "plexi crunch", "plexi capture"]);
    assert!(
        fs::read(media.0.join("searchRecentChangesUser"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(hits(&library), [capture, added]);
}

/// r50: the read-only folders a save checks (`0x7054f0`): the Factory Library and factory
/// plugin folders, but not a plugin's `User` tree (linked to the user root) or user presets.
#[test]
fn read_only_folders_are_the_factory_library_and_factory_plugins() {
    let media = Media::new();
    let library = media.library();
    let factory_path = "/opt/neuraldsp/Factory Library/Brit.pb";
    assert_eq!(
        library.read_only_folder(factory_path),
        None,
        "no factory root"
    );
    let library = library.with_factory(MediaRoot::factory(media.0.join("opt")));
    for (path, folder) in [
        (factory_path, Some(ReadOnlyFolder::FactoryLibrary)),
        (
            "/opt/neuraldsp/Plugins/Amp/Clean.pb",
            Some(ReadOnlyFolder::FactoryPlugin),
        ),
        ("/opt/neuraldsp/Plugins/Amp/User/Mine.pb", None),
        ("/media/p4/Presets/Live/Mine.pb", None),
    ] {
        assert_eq!(library.read_only_folder(path), folder, "{path}");
    }
}

/// Review r35 G5: a library without a factory root says so by type (the upload walk reads it
/// as "no factory presets"); with one, the factory presets load but never save.
#[test]
fn factory_presets_load_but_never_save() {
    let media = Media::new();
    assert!(matches!(
        media.library().factory_slots(),
        Err(Error::NoFactoryRoot)
    ));
    let factory = media.0.join("opt");
    fs::create_dir_all(factory.join("Factory Library")).unwrap();
    let path = "/opt/neuraldsp/Factory Library/Brit.pb";
    fs::write(
        factory.join("Factory Library/Brit.pb"),
        document("Brit").to_bytes(),
    )
    .unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[0] = Some(path.to_owned());
    fs::write(
        factory.join("Factory Library/setlist"),
        encode_setlist(&slots).unwrap(),
    )
    .unwrap();
    let mut library = media.library().with_factory(MediaRoot::factory(&factory));
    assert_eq!(library.factory_slots().unwrap(), slots);
    let mut preset = library.load(path).unwrap();
    assert!(preset.sidecar.is_none());
    preset.document.rename("Mine").unwrap();
    assert!(matches!(
        library.save(&mut preset),
        Err(Error::Storage(tetrad_storage::Error::ReadOnly))
    ));
}

#[test]
fn search_index_failures_never_fail_a_saved_change() {
    let media = Media::new();
    // Stock indexes a name with '|' by start offset (review F1 case).
    let index = SearchIndex::build(vec![SearchEntry {
        name: "a|b".into(),
        folder_key: folder_key("/media/p4/Presets/My Presets"),
        path: "/media/p4/Presets/My Presets/a|b.pb".into(),
    }])
    .unwrap();
    fs::write(media.0.join("searchIndexUser"), index.encode()).unwrap();
    // A setlist without a file and one with a bad file must not stop a rebuild.
    fs::create_dir(media.0.join("Presets/Bare")).unwrap();
    fs::create_dir(media.0.join("Presets/Bad")).unwrap();
    fs::write(media.0.join("Presets/Bad/setlist"), "empty\n").unwrap();
    let mut library = media.library();
    saved(library.store(DEFAULT_SETLIST, 0, "Lead", &document("L"), &sidecar("L")));
    assert_eq!(library.search("B").unwrap()[0].name, "a|b");
    library.rebuild_search_index().unwrap();

    library.crash_before = Some("search: changes");
    let stale = library
        .store(DEFAULT_SETLIST, 1, "Solo", &document("S"), &sidecar("S"))
        .unwrap();
    assert!(stale.search_index.is_some());
    assert_eq!(
        library.slots(DEFAULT_SETLIST).unwrap()[1].as_deref(),
        Some(stale.value.as_str())
    );
    assert!(!media.0.join("searchIndexUser").exists());
    assert_eq!(library.search("solo").unwrap()[0].path, stale.value);

    library.crash_before = None;
    assert!(library.prepare_search_index().unwrap());
    assert!(!library.prepare_search_index().unwrap());
    let rebuilt =
        SearchIndex::decode(&fs::read(media.0.join("searchIndexUser")).unwrap(), 9).unwrap();
    let names: Vec<_> = rebuilt.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["lead", "solo"]);
}

#[test]
fn rename_setlist_crash_at_any_step_leaves_every_slot_resolvable() {
    for step in [
        "rename_setlist: slots",
        "rename_setlist: folders",
        "rename_setlist: items",
    ] {
        let media = Media::new();
        let mut library = media.library();
        library.create_setlist("Live").unwrap();
        saved(library.store("Live", 5, "Solo", &document("S"), &sidecar("S")));
        library.crash_before = Some(step);
        assert!(library.rename_setlist("Live", "Tour").is_err(), "{step}");

        let mut library = media.library();
        let names: Vec<_> = library
            .setlists()
            .unwrap()
            .into_iter()
            .map(|setlist| setlist.name)
            .collect();
        assert_eq!(names, [DEFAULT_SETLIST, "Tour"], "{step}");
        let slot = library.slots("Tour").unwrap()[5].clone().unwrap();
        assert_eq!(slot, "/media/p4/Presets/Tour/Solo.pb", "{step}");
        library.load(&slot).unwrap();
        // Delete then finds and removes the real files: nothing is orphaned.
        saved(library.delete("Tour", 5));
        library.delete_setlist("Tour").unwrap();
    }
}

/// Review r20 G4: a replace writes the new files before it touches the slot, so a write
/// error (here a directory where the new sidecar goes) keeps the old preset in its slot.
/// After a good replace the old files are gone and `<name>.pb` of a sole slot is reused.
#[test]
fn a_failed_replace_keeps_the_old_preset_in_its_slot() {
    let media = Media::new();
    let mut library = media.library();
    let old = saved(library.store(DEFAULT_SETLIST, 0, "Old", &document("Old"), &sidecar("Old")));
    fs::create_dir(media.0.join("Presets/My Presets/New.json")).unwrap();
    let replaced = library.replace(DEFAULT_SETLIST, 0, "New", &document("New"), &sidecar("New"));
    assert!(replaced.is_err());
    assert_eq!(
        library.slots(DEFAULT_SETLIST).unwrap()[0].as_deref(),
        Some(&*old)
    );
    assert!(library.load(&old).is_ok(), "the old files stay");
    assert!(!media.0.join("Presets/My Presets/New.pb").exists());

    fs::remove_dir(media.0.join("Presets/My Presets/New.json")).unwrap();
    let new = saved(library.replace(DEFAULT_SETLIST, 0, "New", &document("New"), &sidecar("New")));
    assert_eq!(new, "/media/p4/Presets/My Presets/New.pb");
    assert_eq!(
        library.slots(DEFAULT_SETLIST).unwrap()[0].as_deref(),
        Some(&*new)
    );
    assert!(!media.0.join("Presets/My Presets/Old.pb").exists());
    let again =
        saved(library.replace(DEFAULT_SETLIST, 0, "New", &document("New"), &sidecar("New")));
    assert_eq!(again, new, "a sole slot keeps its file name");
}

/// Review-r21 H5: an in-place replace (`<name>.pb`, sole slot) stages both files before it
/// renames either, so a failed sidecar write (here a symlink, which writes refuse) keeps the
/// old document bytes.
#[test]
fn a_failed_in_place_replace_keeps_the_old_document() {
    let media = Media::new();
    let mut library = media.library();
    saved(library.store(
        DEFAULT_SETLIST,
        0,
        "Lead",
        &document("Old"),
        &sidecar("Lead"),
    ));
    let pb = media.0.join("Presets/My Presets/Lead.pb");
    let json = pb.with_extension("json");
    let old_bytes = fs::read(&pb).unwrap();
    let kept = media.0.join("kept.json");
    fs::rename(&json, &kept).unwrap();
    std::os::unix::fs::symlink(&kept, &json).unwrap();

    let replaced = library.replace(
        DEFAULT_SETLIST,
        0,
        "Lead",
        &document("New"),
        &sidecar("Lead"),
    );
    assert!(replaced.is_err());
    assert_eq!(fs::read(&pb).unwrap(), old_bytes);
    assert!(fs::symlink_metadata(&json).unwrap().is_symlink());
    let names: Vec<_> = fs::read_dir(pb.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|name| !name.contains(TEMP_MARKER)),
        "{names:?}"
    );
}

#[test]
fn delete_and_rename_keep_files_that_another_slot_names() {
    let media = Media::new();
    let mut library = media.library();
    let lead = saved(library.store(DEFAULT_SETLIST, 0, "Lead", &document("L"), &sidecar("L")));
    library.create_setlist("Live").unwrap();
    let mut slots = vec![None; SETLIST_SLOTS];
    slots[0] = Some(lead.clone());
    fs::write(
        media.0.join("Presets/Live/setlist"),
        encode_setlist(&slots).unwrap(),
    )
    .unwrap();
    let lead_file = media.0.join("Presets/My Presets/Lead.pb");

    saved(library.rename_preset(DEFAULT_SETLIST, 0, "Solo"));
    assert!(lead_file.exists());
    saved(library.delete(DEFAULT_SETLIST, 0));
    assert!(!media.0.join("Presets/My Presets/Solo.pb").exists());
    assert_eq!(library.load(&lead).unwrap().path, lead);

    saved(library.delete("Live", 0));
    assert!(!lead_file.exists());
    assert!(!lead_file.with_extension("json").exists());
}

#[test]
fn delete_setlist_refuses_unknown_files_before_any_change() {
    let media = Media::new();
    let mut library = media.library();
    library.create_setlist("Live").unwrap();
    let directory = media.0.join("Presets/Live");
    fs::write(directory.join("Orphan.pb"), b"").unwrap();
    fs::write(directory.join("setlist.tetrad-tmp-1-2"), b"").unwrap();
    // A zen-rs leftover (the earlier name of Tetrad) goes the same way.
    fs::write(directory.join("setlist.zen-tmp-1-3"), b"").unwrap();
    let folders = media.read("user_preset_folders");
    assert!(matches!(
        library.delete_setlist("Live"),
        Err(Error::SetlistHasFiles { files, .. }) if files == ["Orphan.pb"]
    ));
    assert_eq!(media.read("user_preset_folders"), folders);
    assert!(directory.join("setlist").exists());

    fs::remove_file(directory.join("Orphan.pb")).unwrap();
    library.delete_setlist("Live").unwrap();
    assert!(!directory.exists());
    assert_eq!(
        media.read("user_preset_folders"),
        "/media/p4/Presets/My Presets\n"
    );
}

#[test]
fn directory_occupancy_and_sort_roles_come_from_slots_and_sidecars() {
    let media = Media::new();
    let mut library = media.library();
    let mut metadata = sidecar("Empty");
    metadata.set_text("author", "Player");
    metadata.set_text("date", "2026-10-09T10:00:00Z");
    let path = saved(library.store(DEFAULT_SETLIST, 7, "Empty", &document("Empty"), &metadata));
    let summaries = library.directory_slots(DEFAULT_SETLIST).unwrap();
    assert!(summaries[0].path.is_none());
    assert_eq!(summaries[7].path.as_deref(), Some(path.as_str()));
    assert_eq!(
        (&summaries[7].name, &summaries[7].author, &summaries[7].date),
        (
            &"Empty".to_owned(),
            &"Player".to_owned(),
            &"2026-10-09T10:00:00Z".to_owned()
        )
    );
}

#[test]
fn row_move_onto_an_occupied_slot_swaps_or_overwrites_as_chosen() {
    let media = Media::new();
    let mut library = media.library();
    let mut store = |slot, name: &str| {
        saved(library.store(DEFAULT_SETLIST, slot, name, &document(name), &sidecar(name)))
    };
    let (a, b, c) = (store(0, "First"), store(1, "Second"), store(2, "Third"));
    let bytes = fs::read(media.0.join("Presets/My Presets/First.pb")).unwrap();
    saved(library.move_slot(DEFAULT_SETLIST, 0, 1, MoveMode::Swap));
    let slots = library.slots(DEFAULT_SETLIST).unwrap();
    assert_eq!(
        (slots[0].as_deref(), slots[1].as_deref()),
        (Some(b.as_str()), Some(a.as_str()))
    );
    // Overwrite deletes the destination preset and its files; the mover keeps its bytes.
    saved(library.move_slot(DEFAULT_SETLIST, 1, 2, MoveMode::Overwrite));
    let slots = library.slots(DEFAULT_SETLIST).unwrap();
    assert!(slots[1].is_none());
    assert_eq!(slots[2].as_deref(), Some(a.as_str()));
    assert!(!media.0.join("Presets/My Presets/Third.pb").exists(), "{c}");
    assert_eq!(
        fs::read(media.0.join("Presets/My Presets/First.pb")).unwrap(),
        bytes
    );
    // An empty destination is a plain move in either mode.
    saved(library.move_slot(DEFAULT_SETLIST, 2, 255, MoveMode::Overwrite));
    assert_eq!(
        library.slots(DEFAULT_SETLIST).unwrap()[255].as_deref(),
        Some(a.as_str())
    );
    assert!(matches!(
        library.move_slot(DEFAULT_SETLIST, 1, 0, MoveMode::Swap),
        Err(Error::SlotEmpty(1))
    ));
}

#[test]
fn populated_setlist_delete_removes_known_files_and_list_references() {
    let media = Media::new();
    let mut library = media.library();
    library.create_setlist("Tour").unwrap();
    let path = saved(library.store("Tour", 0, "Lead", &document("Lead"), &sidecar("Lead")));
    for list in [
        ItemList::Favorites(Collection::Presets),
        ItemList::Recents(Collection::Presets),
    ] {
        library.push_item(list, &item(&path)).unwrap();
    }
    // Preflight must refuse unrelated files before it deletes even one slot.
    fs::write(media.0.join("Presets/Tour/keep.txt"), b"owned test data").unwrap();
    assert!(matches!(
        library.delete_setlist_with_presets("Tour"),
        Err(Error::SetlistHasFiles { .. })
    ));
    assert!(library.slots("Tour").unwrap()[0].is_some());
    fs::remove_file(media.0.join("Presets/Tour/keep.txt")).unwrap();
    saved(library.delete_setlist_with_presets("Tour"));
    assert!(!media.0.join("Presets/Tour").exists());
    assert!(media.read("favorites").is_empty());
    assert!(media.read("recents").is_empty());
    assert!(
        library
            .setlists()
            .unwrap()
            .iter()
            .all(|setlist| setlist.name != "Tour")
    );
}

#[test]
fn plugin_listing_merges_the_user_twin_and_lists_presets_in_stock_folder_order() {
    let media = Media::new();
    let factory = media.0.join("factory");
    let plugin = factory.join("Plugins/Owned Plugin");
    for folder in ["Artists/Someone", "Neural DSP", "Main Artist"] {
        fs::create_dir_all(plugin.join(folder)).unwrap();
    }
    for file in [
        "Default.pb",
        "__metadata.pb",
        "Neural DSP/Clean.pb",
        "Artists/Someone/Lead.json",
    ] {
        fs::write(plugin.join(file), b"owned").unwrap();
    }
    // A sidecar gives the listing name and roles; an unreadable one keeps the stem.
    fs::write(
        plugin.join("Neural DSP/Clean.json"),
        br#"{"name":"Clean Tone","author":"Owned","instrument_type":"bass"}"#,
    )
    .unwrap();
    fs::write(plugin.join("Artists/Someone/Lead.pb"), b"owned").unwrap();
    fs::create_dir_all(media.0.join("Plugins/Owned Plugin")).unwrap();
    fs::write(media.0.join("Plugins/Owned Plugin/Mine.pb"), b"owned").unwrap();
    fs::create_dir_all(media.0.join("Plugins/User Only")).unwrap();
    fs::write(media.0.join("Plugins/ignored.txt"), b"owned").unwrap();
    let library = media.library().with_factory(MediaRoot::factory(&factory));
    let folders = library.plugin_folders().unwrap();
    assert_eq!(folders.len(), 2);
    let owned = &folders[0];
    assert_eq!(
        (owned.path.as_str(), owned.read_only),
        ("/opt/neuraldsp/Plugins/Owned Plugin", true)
    );
    assert_eq!(
        owned.children,
        ["Main Artist", "Artists", "Neural DSP", "User"]
    );
    let presets: Vec<_> = owned
        .presets
        .iter()
        .map(|p| (p.folder.as_str(), p.name.as_str(), p.path.as_str()))
        .collect();
    assert_eq!(
        presets,
        [
            (
                "",
                "Default",
                "/opt/neuraldsp/Plugins/Owned Plugin/Default.pb"
            ),
            (
                "Artists/Someone",
                "Lead",
                "/opt/neuraldsp/Plugins/Owned Plugin/Artists/Someone/Lead.pb"
            ),
            (
                "Neural DSP",
                "Clean Tone",
                "/opt/neuraldsp/Plugins/Owned Plugin/Neural DSP/Clean.pb"
            ),
            ("User", "Mine", "/media/p4/Plugins/Owned Plugin/Mine.pb"),
        ]
    );
    let clean = &owned.presets[2];
    assert_eq!(
        (clean.author.as_str(), clean.instrument.as_str()),
        ("Owned", "bass")
    );
    assert_eq!(
        (folders[1].name.as_str(), folders[1].read_only),
        ("User Only", false)
    );
}

/// Review r11 N12 (inferred, stock not read): an unreadable plugin subfolder is left out and
/// named, and the rest of the plugin still lists.
#[cfg(unix)]
#[test]
fn unreadable_plugin_folders_are_left_out_and_named() {
    use std::os::unix::fs::PermissionsExt;
    let media = Media::new();
    let plugin = media.0.join("Plugins/Owned");
    let locked = plugin.join("Locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(plugin.join("Main.pb"), b"owned").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads the folder anyway; the skip is then not observable.
    let unreadable = fs::read_dir(&locked).is_err();
    let folders = media.library().plugin_folders();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let folders = folders.unwrap();
    assert_eq!(folders[0].presets.len(), 1);
    if unreadable {
        assert_eq!(folders[0].unreadable, ["/media/p4/Plugins/Owned/Locked"]);
    }
}

#[test]
fn each_media_recent_file_caps_at_64_and_delete_cleans_all_folder_references() {
    let media = Media::new();
    let mut library = media.library();
    for collection in [Collection::Captures, Collection::Irs] {
        let list = ItemList::Recents(collection);
        for n in 0..65 {
            library
                .push_item(
                    list,
                    &ItemRef {
                        folder: "owned_root".into(),
                        item: format!("item{n}"),
                    },
                )
                .unwrap();
        }
        let target = ItemRef {
            folder: "owned_root".into(),
            item: "item12".into(),
        };
        library.push_item(list, &target).unwrap();
        assert_eq!(library.items(list).unwrap().len(), 64);
        assert_eq!(library.items(list).unwrap()[0], target);
        for folder in ["owned_root", "owned_child"] {
            library
                .push_item(
                    ItemList::Favorites(collection),
                    &ItemRef {
                        folder: folder.into(),
                        item: "item12".into(),
                    },
                )
                .unwrap();
        }
        library.forget_media(collection, "item12").unwrap();
        assert!(
            library
                .items(ItemList::Favorites(collection))
                .unwrap()
                .is_empty()
        );
        assert!(
            library
                .items(list)
                .unwrap()
                .iter()
                .all(|entry| entry.item != "item12")
        );
    }
}

/// The stored hash of a preset file.
fn stored_hash(library: &StockLibrary, path: &str) -> Option<u32> {
    library.load(path).unwrap().document.summary().unwrap().hash
}

#[test]
fn swap_between_moves_each_preset_into_the_other_setlist_as_a_new_file() {
    let media = Media::new();
    let mut library = media.library();
    library.create_setlist("Live").unwrap();
    let lead = saved(library.store(
        DEFAULT_SETLIST,
        3,
        "Lead",
        &document("Lead"),
        &sidecar("Lead"),
    ));
    let solo = saved(library.store("Live", 7, "Solo", &document("Solo"), &sidecar("Solo")));
    let favorite = item(&lead);
    library
        .push_item(ItemList::Favorites(Collection::Presets), &favorite)
        .unwrap();
    // The new path of each preset: Lead (from My Presets), then Solo (from Live).
    let [into_live, into_mine] = committed(library.swap_between(DEFAULT_SETLIST, 3, "Live", 7));
    assert_eq!(into_mine, "/media/p4/Presets/My Presets/Solo.pb");
    assert_eq!(into_live, "/media/p4/Presets/Live/Lead.pb");
    assert_eq!(
        library.slots(DEFAULT_SETLIST).unwrap()[3],
        Some(into_mine.clone())
    );
    assert_eq!(library.slots("Live").unwrap()[7], Some(into_live.clone()));
    let name = |path: &str| library.load(path).unwrap().document.summary().unwrap().name;
    assert_eq!(name(&into_mine).as_deref(), Some("Solo"));
    assert_eq!(name(&into_live).as_deref(), Some("Lead"));
    // `0x21ee8c`: the hash follows the new path.
    assert_eq!(
        stored_hash(&library, &into_live),
        Some(tetrad_core::PresetId::from_identifier(&into_live).0)
    );
    for old in [&lead, &solo] {
        assert!(library.load(old).is_err(), "{old} is gone");
    }
    assert_eq!(
        library
            .items(ItemList::Favorites(Collection::Presets))
            .unwrap(),
        [item(&into_live)],
        "the favorite follows the preset"
    );
    assert!(matches!(
        library.swap_between("Live", 7, "Live", 0),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        library.swap_between(DEFAULT_SETLIST, 3, "Live", 0),
        Err(Error::SlotEmpty(0))
    ));
}

#[test]
fn swap_between_keeps_both_presets_when_the_second_setlist_write_fails() {
    let media = Media::new();
    let mut library = media.library();
    library.create_setlist("Live").unwrap();
    let lead = saved(library.store(
        DEFAULT_SETLIST,
        3,
        "Lead",
        &document("Lead"),
        &sidecar("Lead"),
    ));
    let solo = saved(library.store("Live", 7, "Solo", &document("Solo"), &sidecar("Solo")));
    let before = |library: &StockLibrary| {
        (
            library.slots(DEFAULT_SETLIST).unwrap(),
            library.slots("Live").unwrap(),
        )
    };
    let slots = before(&library);
    library.crash_before = Some("swap_between: b");
    assert!(library.swap_between(DEFAULT_SETLIST, 3, "Live", 7).is_err());
    assert_eq!(before(&library), slots, "setlist a is written back");
    for path in [&lead, &solo] {
        assert!(library.load(path).is_ok(), "{path} stays");
    }
    let mut files: Vec<_> = (fs::read_dir(media.0.join("Presets/Live")).unwrap())
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["Solo.json", "Solo.pb", "setlist"],
        "the new files go"
    );
}

/// Review r30 C3: when setlist `a` cannot be written back either, the error names the preset
/// that is in no setlist now, and the new file of `b` is gone (dropped before the rollback).
#[test]
fn a_swap_that_cannot_write_its_first_setlist_back_names_the_orphan() {
    let media = Media::new();
    let mut library = media.library();
    library.create_setlist("Live").unwrap();
    let lead = saved(library.store(
        DEFAULT_SETLIST,
        3,
        "Lead",
        &document("Lead"),
        &sidecar("Lead"),
    ));
    saved(library.store("Live", 7, "Solo", &document("Solo"), &sidecar("Solo")));
    library.crash_before = Some("swap_between: ");
    let error = library.swap_between(DEFAULT_SETLIST, 3, "Live", 7);
    let Err(Error::SwapIncomplete { orphans, .. }) = error else {
        panic!("expected SwapIncomplete, got {error:?}");
    };
    assert_eq!(orphans, std::slice::from_ref(&lead));
    assert!(library.load(&lead).is_ok(), "the orphan file stays");
    assert!(
        !media.0.join("Presets/Live/Lead.pb").exists(),
        "the copy of Lead in Live is dropped first"
    );
}

/// Review r30 C2: once both setlists are written the swap is done; a failed favorites step
/// comes back with the new paths, and the old files stay (no slot names them).
#[test]
fn a_swap_returns_its_new_paths_when_a_later_step_fails() {
    let media = Media::new();
    let mut library = media.library();
    library.create_setlist("Live").unwrap();
    let lead = saved(library.store(
        DEFAULT_SETLIST,
        3,
        "Lead",
        &document("Lead"),
        &sidecar("Lead"),
    ));
    saved(library.store("Live", 7, "Solo", &document("Solo"), &sidecar("Solo")));
    fs::create_dir(media.0.join("favorites")).unwrap();
    let swapped = library.swap_between(DEFAULT_SETLIST, 3, "Live", 7).unwrap();
    assert!(swapped.tail_error.is_some());
    let [into_live, into_mine] = swapped.value;
    assert_eq!(library.slots("Live").unwrap()[7], Some(into_live));
    assert_eq!(library.slots(DEFAULT_SETLIST).unwrap()[3], Some(into_mine));
    assert!(
        library.load(&lead).is_ok(),
        "the tail stopped before the old files"
    );
}

/// `0x1dd61c` → `0x1dcf2c` → `0x742990`: a cloud preset goes into an empty slot as a free
/// `<name>.pb` with the hash of its path and the Downloads-row sidecar (`cloudId` set); an
/// occupied slot without an agreed overwrite is refused before any write.
#[test]
fn a_cloud_preset_is_stored_into_an_empty_slot_only() {
    use tetrad_services::cloud::products::{CloudProduct, ProductPayload};
    let media = Media::new();
    let mut library = media.library();
    let product = CloudProduct::from_product(&serde_json::json!({
        "id": "cloud-9",
        "name": "Shared Lead",
        "type": "preset",
        "author_username": "someone",
        "created": "1970-01-02T00:00:01Z",
    }))
    .unwrap();
    let payload = ProductPayload::Preset(document("Lead").to_bytes());
    let path = committed(library.store_cloud_preset(DEFAULT_SETLIST, 4, &product, &payload, None));
    assert_eq!(path, "/media/p4/Presets/My Presets/Shared Lead.pb");
    assert_eq!(
        library.slots(DEFAULT_SETLIST).unwrap()[4],
        Some(path.clone())
    );
    assert_eq!(
        stored_hash(&library, &path),
        Some(tetrad_core::PresetId::from_identifier(&path).0)
    );
    let stored = library.sidecar(&path).unwrap().unwrap();
    assert_eq!(
        (stored.text("cloudId"), stored.text("author")),
        (Some("cloud-9"), Some("someone"))
    );
    let before = media.read("Presets/My Presets/setlist");
    assert!(matches!(
        library.store_cloud_preset(DEFAULT_SETLIST, 4, &product, &payload, None),
        Err(Error::SlotOccupied(4))
    ));
    assert_eq!(media.read("Presets/My Presets/setlist"), before);
    assert!(!media.0.join("Presets/My Presets/Shared Lead_1.pb").exists());
}

#[test]
fn plugin_presets_add_with_a_free_name_and_replace_by_key() {
    let media = Media::new();
    let factory = media.0.join("factory");
    fs::create_dir_all(factory.join("Plugins/Owned/Neural DSP")).unwrap();
    fs::create_dir_all(media.0.join("Plugins/Owned/Live")).unwrap();
    let mut library = media.library().with_factory(MediaRoot::factory(&factory));
    let user = "/opt/neuraldsp/Plugins/Owned/User";
    let added = library
        .store_plugin_preset(
            user,
            Some("cortex-key"),
            &document("Lead"),
            &sidecar("Lead"),
        )
        .unwrap();
    assert_eq!(
        (added.path.as_str(), added.replaced),
        ("/media/p4/Plugins/Owned/Lead.pb", None)
    );
    assert_eq!(
        stored_hash(&library, &added.path),
        Some(tetrad_core::PresetId::from_identifier(&added.path).0)
    );
    // Another preset of the same name never overwrites the first (O).
    let twin = library
        .store_plugin_preset(user, None, &document("Lead"), &sidecar("Lead"))
        .unwrap();
    assert_eq!(twin.path, "/media/p4/Plugins/Owned/Lead_1.pb");
    let mut louder = sidecar("Lead");
    louder.set_text("author", "Owned");
    let in_place = library
        .store_plugin_preset(user, Some(&added.path), &document("Lead"), &louder)
        .unwrap();
    assert_eq!(in_place.path, added.path);
    assert_eq!(
        library
            .sidecar(&added.path)
            .unwrap()
            .unwrap()
            .text("author"),
        Some("Owned")
    );
    let favorites = ItemList::Favorites(Collection::Presets);
    library.push_item(favorites, &item(&twin.path)).unwrap();
    let renamed = library
        .store_plugin_preset(user, Some(&twin.path), &document("Solo"), &sidecar("Solo"))
        .unwrap();
    assert_eq!(
        (renamed.path.as_str(), renamed.replaced.as_deref()),
        ("/media/p4/Plugins/Owned/Solo.pb", Some(twin.path.as_str()))
    );
    assert!(renamed.tail_error.is_none());
    assert!(library.load(&twin.path).is_err(), "the old file goes");
    // Review r30 C5: the favorite follows the renamed file.
    assert_eq!(library.items(favorites).unwrap(), [item(&renamed.path)]);
    let live = library
        .store_plugin_preset(
            "/media/p4/Plugins/Owned/Live",
            None,
            &document("Lead"),
            &sidecar("Lead"),
        )
        .unwrap();
    assert_eq!(live.path, "/media/p4/Plugins/Owned/Live/Lead.pb");
    for read_only in [
        "/opt/neuraldsp/Plugins/Owned",
        "/opt/neuraldsp/Plugins/Owned/Neural DSP",
    ] {
        assert!(matches!(
            library.store_plugin_preset(read_only, None, &document("X"), &sidecar("X")),
            Err(Error::Storage(tetrad_storage::Error::ReadOnly))
        ));
    }
}

/// A9: plugin sub-folders (`0x76f2b8`, `0x76aed8`, `0x76ec04`, C). CREATE makes the directory
/// (one that exists is refused); a rename moves the directory and the favorites follow its
/// presets (the same name does nothing); DELETE removes the directory and its favorites. O:
/// read-only folders, the plugin root, names that are no path part and folders below
/// `PLUGIN_DEPTH` are refused.
#[test]
fn plugin_sub_folders_create_rename_and_delete() {
    let media = Media::new();
    let factory = media.0.join("factory");
    fs::create_dir_all(factory.join("Plugins/Owned/Neural DSP")).unwrap();
    fs::create_dir_all(media.0.join("Plugins/Owned")).unwrap();
    let mut library = media.library().with_factory(MediaRoot::factory(&factory));
    let user = "/opt/neuraldsp/Plugins/Owned/User";
    let live = library.create_plugin_folder(user, "Live").unwrap();
    assert_eq!(live, "/opt/neuraldsp/Plugins/Owned/User/Live");
    assert!(media.0.join("Plugins/Owned/Live").is_dir());
    assert!(
        library.create_plugin_folder(user, "Live").is_err(),
        "exists"
    );
    let deep = library.create_plugin_folder(&live, "Deep").unwrap();
    assert!(
        library.create_plugin_folder(&deep, "Deeper").is_err(),
        "depth"
    );
    for (parent, name) in [
        (user, ""),
        (user, ".."),
        (user, "a/b"),
        ("/opt/neuraldsp/Plugins/Owned/Neural DSP", "X"),
    ] {
        assert!(
            library.create_plugin_folder(parent, name).is_err(),
            "{parent} {name}"
        );
    }

    let stored = library
        .store_plugin_preset(&live, None, &document("Lead"), &sidecar("Lead"))
        .unwrap();
    assert_eq!(stored.path, "/media/p4/Plugins/Owned/Live/Lead.pb");
    let favorites = ItemList::Favorites(Collection::Presets);
    let entry = ItemRef {
        folder: live.clone(),
        item: stored.path.clone(),
    };
    library.push_item(favorites, &entry).unwrap();
    let same = library.rename_plugin_folder(&live, "Live").unwrap();
    assert_eq!(same.key, live);
    let renamed = library.rename_plugin_folder(&live, "Stage").unwrap();
    assert_eq!(renamed.key, "/opt/neuraldsp/Plugins/Owned/User/Stage");
    assert!(renamed.tail_error.is_none());
    assert!(media.0.join("Plugins/Owned/Stage/Lead.pb").is_file());
    let moved = ItemRef {
        folder: renamed.key.clone(),
        item: "/media/p4/Plugins/Owned/Stage/Lead.pb".into(),
    };
    assert_eq!(library.items(favorites).unwrap(), [moved]);
    for folder in [
        user,
        "/media/p4/Plugins/Owned",
        "/opt/neuraldsp/Plugins/Owned/Neural DSP",
    ] {
        assert!(
            library.rename_plugin_folder(folder, "X").is_err(),
            "{folder}"
        );
        assert!(library.delete_plugin_folder(folder).is_err(), "{folder}");
    }

    let gone = library.delete_plugin_folder(&renamed.key).unwrap();
    assert!(gone.tail_error.is_none());
    assert!(!media.0.join("Plugins/Owned/Stage").exists());
    assert!(library.items(favorites).unwrap().is_empty());
}

#[test]
fn plugin_preset_copies_into_a_setlist_slot_and_deletes_from_a_user_folder() {
    let media = Media::new();
    let factory = media.0.join("factory");
    fs::create_dir_all(factory.join("Plugins/Owned")).unwrap();
    fs::create_dir_all(media.0.join("Plugins/Owned")).unwrap();
    let mut library = media.library().with_factory(MediaRoot::factory(&factory));
    let source = library
        .store_plugin_preset(
            "/media/p4/Plugins/Owned",
            None,
            &document("Lead"),
            &sidecar("Lead"),
        )
        .unwrap()
        .path;
    let occupant =
        saved(library.store(DEFAULT_SETLIST, 2, "Old", &document("Old"), &sidecar("Old")));
    let (slot, copy) = committed(library.copy_plugin_preset(&source, DEFAULT_SETLIST, Some(2)));
    assert_eq!(
        (slot, copy.as_str()),
        (2, "/media/p4/Presets/My Presets/Lead.pb")
    );
    assert_eq!(
        library.slots(DEFAULT_SETLIST).unwrap()[2],
        Some(copy.clone())
    );
    assert!(
        library.load(&occupant).is_err(),
        "the overwritten preset goes"
    );
    assert!(library.load(&source).is_ok(), "the source stays");
    let (first, _) = committed(library.copy_plugin_preset(&source, DEFAULT_SETLIST, None));
    assert_eq!(first, 0, "no slot: the first empty one");
    assert!(
        library
            .copy_plugin_preset(&copy, DEFAULT_SETLIST, None)
            .is_err()
    );
    library.delete_plugin_preset(&source).unwrap();
    assert!(library.load(&source).is_err());
    fs::write(factory.join("Plugins/Owned/Clean.pb"), b"").unwrap();
    assert!(matches!(
        library.delete_plugin_preset("/opt/neuraldsp/Plugins/Owned/Clean.pb"),
        Err(Error::Storage(tetrad_storage::Error::ReadOnly))
    ));
}

#[test]
fn sidecar_instrument_follows_the_stock_texts() {
    use crate::sidecar::Instrument;
    let mut metadata = sidecar("Lead");
    assert_eq!(metadata.instrument(), Instrument::NotSet);
    metadata.set_instrument(Instrument::Bass);
    assert_eq!(metadata.text("instrument_type"), Some("bass"));
    let read = Sidecar::parse(&metadata.encode()).unwrap();
    assert_eq!(read.instrument(), Instrument::Bass);
    metadata.set_text("instrument_type", "unknown");
    assert_eq!(metadata.instrument(), Instrument::NotSet, "`0x7ec714`");
    assert_eq!(Instrument::from_code(4), Some(Instrument::Vocal));
    assert_eq!(Instrument::from_code(6), None);
}
