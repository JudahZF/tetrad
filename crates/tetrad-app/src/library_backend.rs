//! Synchronous owned-library requests and lossless local scene edits. No DSP load
//! or database write is performed; Product identity is separate from UI/DSP identity.

use tetrad_library::{PresetLibrary, PresetSession};
use tetrad_ui::{PresetItem, SceneIndex, UiCommand};

use crate::event_loop::{AdmittedSnapshot, CommandBackend};
use crate::session::{OfflineSession, SessionError};

pub const MAX_BATCH_COMMANDS: usize = 128;
pub const MAX_PRODUCT_ID_BYTES: usize = 1_024;

/// Caller-provided list entry. Neither the handle nor the Product ID is inferred
/// from wire hashes, metadata, document names or filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetBinding {
    pub ui_handle: u64,
    pub product_id: String,
    pub title: String,
}

/// Verified by a successful owned-library load, not by DSP activation. The UI
/// handle is caller-assigned; only `product_id` comes from the loaded Product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnedPresetProvenance<'a> {
    pub ui_handle: u64,
    pub product_id: &'a str,
    pub read_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryBackendError {
    #[error(transparent)]
    Library(#[from] tetrad_library::Error),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Ui(#[from] tetrad_ui::UiError),
    #[error("preset bindings must contain between one and 128 entries")]
    BindingCount,
    #[error("Product ID must contain 1..=1024 bytes and no NUL")]
    InvalidProductId,
    #[error("list title must contain 1..=4096 bytes and no NUL")]
    InvalidTitle,
    #[error("duplicate caller UI handle: {0}")]
    DuplicateHandle(u64),
    #[error("multiple UI handles cannot alias the same Product: {0}")]
    DuplicateProduct(String),
    #[error("caller UI handle is not mapped: {0}")]
    UnknownHandle(u64),
    #[error("command batch exceeds the owned 128-command limit")]
    BatchLimit,
    #[error("switch from handle {from} to {to} would discard local wire edits")]
    DirtySwitch { from: u64, to: u64 },
    #[error("read-only Product refuses local scene edits")]
    ReadOnlyProduct,
}

struct LoadedPreset {
    binding: usize,
    source: PresetSession,
    session: OfflineSession,
}

/// Owns one caller-opened library and one loaded, admitted local view. Mapping
/// and grid policy are immutable. Requests switch documents, never banks or DSP.
pub struct LibraryBackend {
    library: PresetLibrary,
    bindings: Vec<PresetBinding>,
    loaded: LoadedPreset,
    grid_policy: Option<tetrad_engine::ImportPolicy>,
}

impl LibraryBackend {
    /// Validate the entire mapping before the initial load. The initial handle
    /// and local scene selection are explicit, and every later switch retains
    /// the current local scene index. Titles label the list, not the wire source.
    pub fn new(
        library: PresetLibrary,
        bindings: Vec<PresetBinding>,
        initial_handle: u64,
        selected_scene: SceneIndex,
        grid_policy: Option<tetrad_engine::ImportPolicy>,
    ) -> Result<Self, LibraryBackendError> {
        validate_bindings(&bindings)?;
        let binding = bindings
            .iter()
            .position(|entry| entry.ui_handle == initial_handle)
            .ok_or(LibraryBackendError::UnknownHandle(initial_handle))?;
        let source = library.load(&bindings[binding].product_id)?;
        let session = OfflineSession::from_bytes(&source.document().to_bytes(), selected_scene)?;
        let backend = Self {
            library,
            bindings,
            loaded: LoadedPreset {
                binding,
                source,
                session,
            },
            grid_policy,
        };
        backend.snapshot()?;
        Ok(backend)
    }

    /// Local staged wire bytes and scene selection; this is not a mutable library
    /// session and cannot save itself. Export does not choose or write a path.
    pub fn session(&self) -> &OfflineSession {
        &self.loaded.session
    }

    pub fn provenance(&self) -> OwnedPresetProvenance<'_> {
        OwnedPresetProvenance {
            ui_handle: self.bindings[self.loaded.binding].ui_handle,
            product_id: self.loaded.source.id(),
            read_only: self.loaded.source.read_only(),
        }
    }

    fn project(&self, session: &OfflineSession) -> Result<AdmittedSnapshot, LibraryBackendError> {
        let mut view = match self.grid_policy {
            Some(policy) => session.grid_snapshot(policy)?,
            None => session.snapshot()?,
        };
        // An owned SQL Product load does not prove the loaded/DSP identity used
        // by the UI. Keep it absent, and expose verified provenance separately.
        view.preset_id = None;
        view.presets = self
            .bindings
            .iter()
            .map(|entry| PresetItem {
                id: entry.ui_handle,
                title: entry.title.clone(),
            })
            .collect();
        Ok(AdmittedSnapshot::new(view)?)
    }
}

impl CommandBackend for LibraryBackend {
    type Error = LibraryBackendError;

    fn snapshot(&self) -> Result<AdmittedSnapshot, Self::Error> {
        self.project(&self.loaded.session)
    }

    fn apply(&mut self, commands: &[UiCommand]) -> Result<AdmittedSnapshot, Self::Error> {
        if commands.len() > MAX_BATCH_COMMANDS {
            return Err(LibraryBackendError::BatchLimit);
        }
        for command in commands {
            match command {
                UiCommand::SelectScene { .. }
                | UiCommand::RequestPreset { .. }
                | UiCommand::SetSceneColor { .. } => {}
                UiCommand::SetSceneLabel { label, .. } => {
                    if label.len() > tetrad_ui::MAX_TEXT_BYTES {
                        return Err(tetrad_ui::UiError::TextTooLong.into());
                    }
                }
                UiCommand::Rejected(error) => return Err((*error).into()),
                other => return Err(SessionError::Unsupported(other.clone()).into()),
            }
        }

        // PresetSession is deliberately not cloned or edited. A replacement
        // source remains pending while only a cloned OfflineSession is modified.
        let mut candidate = self.loaded.session.clone();
        let mut binding = self.loaded.binding;
        let mut replacement: Option<PresetSession> = None;
        for command in commands {
            if let UiCommand::RequestPreset { id } = command {
                let next = self
                    .bindings
                    .iter()
                    .position(|entry| entry.ui_handle == *id)
                    .ok_or(LibraryBackendError::UnknownHandle(*id))?;
                // Same-handle requests are not refreshes. Reloading here would
                // discard edits and reset the exact loaded wire baseline.
                if next == binding {
                    continue;
                }
                let view = candidate.snapshot()?;
                if view.dirty {
                    return Err(LibraryBackendError::DirtySwitch {
                        from: self.bindings[binding].ui_handle,
                        to: *id,
                    });
                }
                let source = self.library.load(&self.bindings[next].product_id)?;
                let session =
                    OfflineSession::from_bytes(&source.document().to_bytes(), view.selected_scene)?;
                self.project(&session)?;
                candidate = session;
                binding = next;
                replacement = Some(source);
            } else {
                let source = replacement.as_ref().unwrap_or(&self.loaded.source);
                if source.read_only()
                    && matches!(
                        command,
                        UiCommand::SetSceneLabel { .. } | UiCommand::SetSceneColor { .. }
                    )
                {
                    return Err(LibraryBackendError::ReadOnlyProduct);
                }
                candidate.apply(std::slice::from_ref(command))?;
            }
        }
        let snapshot = self.project(&candidate)?;
        if let Some(source) = replacement {
            self.loaded = LoadedPreset {
                binding,
                source,
                session: candidate,
            };
        } else {
            self.loaded.session = candidate;
        }
        Ok(snapshot)
    }
}

fn validate_bindings(bindings: &[PresetBinding]) -> Result<(), LibraryBackendError> {
    if bindings.is_empty() || bindings.len() > tetrad_ui::MAX_PRESETS {
        return Err(LibraryBackendError::BindingCount);
    }
    for (index, entry) in bindings.iter().enumerate() {
        if entry.product_id.is_empty()
            || entry.product_id.len() > MAX_PRODUCT_ID_BYTES
            || entry.product_id.contains('\0')
        {
            return Err(LibraryBackendError::InvalidProductId);
        }
        if entry.title.is_empty()
            || entry.title.len() > tetrad_ui::MAX_TEXT_BYTES
            || entry.title.contains('\0')
        {
            return Err(LibraryBackendError::InvalidTitle);
        }
        if bindings[..index]
            .iter()
            .any(|other| other.ui_handle == entry.ui_handle)
        {
            return Err(LibraryBackendError::DuplicateHandle(entry.ui_handle));
        }
        if bindings[..index]
            .iter()
            .any(|other| other.product_id == entry.product_id)
        {
            return Err(LibraryBackendError::DuplicateProduct(
                entry.product_id.clone(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::event_loop::{BatchDisposition, EventLoop, LoopEvent, StepResult};
    use tetrad_storage::{Library, Mutation, Product, ProductData};
    use tetrad_ui::{Control, InputEvent, Screen, UiState};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        directory: PathBuf,
        database: PathBuf,
        original: Vec<u8>,
        products: Vec<Product>,
    }

    impl Fixture {
        fn new(products: Vec<Product>) -> Self {
            let directory = loop {
                let path = std::env::temp_dir().join(format!(
                    "tetrad-app-library-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => break path,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create synthetic fixture directory: {error}"),
                }
            };
            let database = directory.join("owned.sqlite");
            let mut storage = Library::create_owned(&database, Default::default()).unwrap();
            let mutations: Vec<_> = products.iter().map(Mutation::InsertProduct).collect();
            storage.apply(&mutations).unwrap();
            drop(storage);
            let original = fs::read(&database).unwrap();
            Self {
                directory,
                database,
                original,
                products,
            }
        }

        fn library(&self) -> PresetLibrary {
            PresetLibrary::open_owned(
                &self.database,
                42,
                Default::default(),
                tetrad_library::Access::ReadOnly,
            )
            .unwrap()
        }

        fn bindings(&self) -> Vec<PresetBinding> {
            self.products
                .iter()
                .enumerate()
                .map(|(index, product)| PresetBinding {
                    ui_handle: 11 * (index as u64 + 1),
                    product_id: product.id.clone(),
                    title: format!("Caller title {index}"),
                })
                .collect()
        }

        fn backend(&self, policy: Option<tetrad_engine::ImportPolicy>) -> LibraryBackend {
            LibraryBackend::new(self.library(), self.bindings(), 11, scene(0), policy).unwrap()
        }

        fn assert_unchanged(&self) {
            assert_eq!(fs::read(&self.database).unwrap(), self.original);
            let storage = Library::open_owned(&self.database, Default::default(), true).unwrap();
            for product in &self.products {
                assert_eq!(
                    storage.product(&product.id).unwrap().as_ref(),
                    Some(product)
                );
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn scene(index: u8) -> SceneIndex {
        SceneIndex::new(index).unwrap()
    }

    fn product(id: &str, wire: &[u8]) -> Product {
        Product {
            id: id.into(),
            product_type: 42,
            read_only: false,
            data: ProductData {
                raw_data: Some(wire.to_vec()),
                metadata: Some(vec![0xff, 0, 0x80, 3]),
            },
        }
    }

    fn wire(name: &[u8; 3]) -> Vec<u8> {
        let mut bytes = vec![
            8, 42, // Same stored hash in both Products: never used for identity.
            0x12, 3, b'o', b'l', b'd', 0x92, 0, 0x83,
            0, // Noncanonical final name tag/length.
        ];
        bytes.extend(name);
        bytes.extend([
            0x7a, 1, b'A', 0x7a, 1, b'B', 0xf8, 1, 1, 0xf8, 1, 2, 0x9b, 6, 8, 0x81, 0, 0x9c,
            6, // Unknown group and overlong varint.
        ]);
        bytes
    }

    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        while value >= 128 {
            bytes.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        bytes.push(value as u8);
        bytes
    }

    fn scalar(field: u32, value: u64) -> Vec<u8> {
        [varint(u64::from(field) << 3), varint(value)].concat()
    }

    fn message(field: u32, bytes: &[u8]) -> Vec<u8> {
        [
            varint((u64::from(field) << 3) | 2),
            varint(bytes.len() as u64),
            bytes.to_vec(),
        ]
        .concat()
    }

    fn grid_wire(occupied: bool) -> Vec<u8> {
        let mut preset = wire(b"One");
        for row in 0..4 {
            let mut chain = [scalar(1, 10 + row), scalar(2, 20 + row), scalar(14, row)].concat();
            for column in 0..8 {
                let hash = if occupied && row == 0 && column == 0 {
                    7
                } else {
                    0
                };
                chain.extend(message(5, &[scalar(1, hash), scalar(5, column)].concat()));
            }
            if row % 2 == 0 {
                chain.extend(message(
                    6,
                    &[scalar(1, u64::MAX), scalar(2, u64::MAX)].concat(),
                ));
                chain.extend(message(7, &scalar(1, 0)));
                chain.extend(message(8, &scalar(1, 0)));
            }
            chain.extend(message(9, &scalar(1, 0)));
            chain.extend(message(13, &scalar(1, 0)));
            preset.extend(message(11, &chain));
        }
        if occupied {
            let col = [message(1, &scalar(1, 0)), scalar(2, 0), scalar(3, 0)].concat();
            preset.extend(message(18, &[message(1, &col), scalar(2, 0)].concat()));
        }
        preset
    }

    #[test]
    fn real_ui_requests_switch_two_owned_products_and_keep_list_and_wire() {
        let fixture = Fixture::new(vec![
            product("caller/product-A", &wire(b"One")),
            product("different-product-id", &wire(b"Two")),
        ]);
        let mut backend =
            LibraryBackend::new(fixture.library(), fixture.bindings(), 22, scene(1), None).unwrap();
        assert_eq!(backend.provenance().product_id, "different-product-id");
        assert_eq!(
            backend.snapshot().unwrap().snapshot().selected_scene,
            scene(1)
        );
        let expected_list = backend.snapshot().unwrap().snapshot().presets.clone();
        backend
            .apply(&[UiCommand::RequestPreset { id: 11 }])
            .unwrap();
        let mut event_loop = EventLoop::new(UiState::new(), backend).unwrap();
        event_loop.step(LoopEvent::Input(InputEvent::Activate(Control::Navigate(
            Screen::Presets,
        ))));
        for (handle, index, name) in [(22, 1, "Two"), (11, 0, "One")] {
            let bounds = event_loop
                .ui()
                .hit_regions()
                .into_iter()
                .find(|region| region.control == Control::SelectPreset(handle) && region.enabled)
                .unwrap()
                .bounds;
            let x = bounds.x + bounds.width as i32 / 2;
            let y = bounds.y + bounds.height as i32 / 2;
            event_loop.step(LoopEvent::Input(InputEvent::PointerDown { x, y }));
            assert!(matches!(
                event_loop.step(LoopEvent::Input(InputEvent::PointerUp { x, y })),
                StepResult::Frame {
                    batch: BatchDisposition::Applied { commands: 1 },
                    ..
                }
            ));
            let view = event_loop.ui().snapshot();
            assert_eq!(view.preset_title, name);
            assert_eq!(view.preset_id, None);
            assert_eq!(view.selected_scene, scene(1));
            assert_eq!(view.presets, expected_list);
            assert!(!view.dirty);
            assert_eq!(event_loop.backend().provenance().ui_handle, handle);
            assert_eq!(
                event_loop.backend().provenance().product_id,
                fixture.products[index].id
            );
            assert_eq!(
                event_loop.backend().session().export(),
                fixture.products[index].data.raw_data.clone().unwrap()
            );
        }
        fixture.assert_unchanged();
    }

    #[test]
    fn mapping_is_bounded_validated_and_unaliased_before_initial_load() {
        let fixture = Fixture::new(vec![product("first", &wire(b"One"))]);
        let original = fixture.bindings();
        let cases = [
            ("", "Title", true),
            ("bad\0id", "Title", true),
            ("first", "", false),
            ("first", "bad\0title", false),
        ];
        for (id, title, invalid_id) in cases {
            let result = LibraryBackend::new(
                fixture.library(),
                vec![PresetBinding {
                    ui_handle: 11,
                    product_id: id.into(),
                    title: title.into(),
                }],
                11,
                scene(0),
                None,
            );
            if invalid_id {
                assert!(matches!(result, Err(LibraryBackendError::InvalidProductId)));
            } else {
                assert!(matches!(result, Err(LibraryBackendError::InvalidTitle)));
            }
        }
        let mut long_id = original.clone();
        long_id[0].product_id = "x".repeat(MAX_PRODUCT_ID_BYTES + 1);
        assert!(matches!(
            LibraryBackend::new(fixture.library(), long_id, 11, scene(0), None),
            Err(LibraryBackendError::InvalidProductId)
        ));
        let mut long_title = original.clone();
        long_title[0].title = "x".repeat(tetrad_ui::MAX_TEXT_BYTES + 1);
        assert!(matches!(
            LibraryBackend::new(fixture.library(), long_title, 11, scene(0), None),
            Err(LibraryBackendError::InvalidTitle)
        ));
        for bindings in [
            Vec::new(),
            vec![original[0].clone(); tetrad_ui::MAX_PRESETS + 1],
        ] {
            assert!(matches!(
                LibraryBackend::new(fixture.library(), bindings, 11, scene(0), None),
                Err(LibraryBackendError::BindingCount)
            ));
        }
        let mut duplicates = vec![original[0].clone(), original[0].clone()];
        duplicates[0].product_id = "missing-before-duplicate-validation".into();
        assert!(matches!(
            LibraryBackend::new(fixture.library(), duplicates, 11, scene(0), None),
            Err(LibraryBackendError::DuplicateHandle(11))
        ));
        let mut aliases = vec![original[0].clone(), original[0].clone()];
        aliases[1].ui_handle = 22;
        assert!(matches!(
            LibraryBackend::new(fixture.library(), aliases, 11, scene(0), None),
            Err(LibraryBackendError::DuplicateProduct(_))
        ));
        assert!(matches!(
            LibraryBackend::new(fixture.library(), original, 99, scene(0), None),
            Err(LibraryBackendError::UnknownHandle(99))
        ));
        fixture.assert_unchanged();
    }

    #[test]
    fn actual_load_and_view_failures_leave_entire_mixed_batch_unchanged() {
        let mut wrong_type = product("wrong-type", &wire(b"Two"));
        wrong_type.product_type = 7;
        let mut null = product("null", &[]);
        null.data.raw_data = None;
        let fixture = Fixture::new(vec![
            product("first", &wire(b"One")),
            wrong_type,
            product("malformed", &[0x80]),
            null,
            product("invalid-view", &[0x7a, 1, 0xff]),
            product(
                "long-title",
                &message(2, &vec![b'x'; tetrad_ui::MAX_TEXT_BYTES + 1]),
            ),
            product(
                "too-many-scenes",
                &[0x7a, 0].repeat(tetrad_ui::SCENE_COUNT + 1),
            ),
        ]);
        let mut bindings = fixture.bindings();
        bindings.push(PresetBinding {
            ui_handle: 88,
            product_id: "missing".into(),
            title: "Missing".into(),
        });
        let mut backend =
            LibraryBackend::new(fixture.library(), bindings, 11, scene(0), None).unwrap();
        let original_view = backend.snapshot().unwrap().snapshot().clone();
        for handle in [22, 33, 44, 55, 66, 77, 88, 999] {
            let error = backend
                .apply(&[
                    UiCommand::SelectScene { scene: scene(1) },
                    UiCommand::RequestPreset { id: handle },
                ])
                .unwrap_err();
            match handle {
                22 => assert!(matches!(
                    error,
                    LibraryBackendError::Library(tetrad_library::Error::ProductType { .. })
                )),
                33 => assert!(matches!(
                    error,
                    LibraryBackendError::Library(tetrad_library::Error::Preset(_))
                )),
                44 => assert!(matches!(
                    error,
                    LibraryBackendError::Library(tetrad_library::Error::PresetDataMissing(_))
                )),
                55 => assert!(matches!(
                    error,
                    LibraryBackendError::Session(SessionError::Preset(_))
                )),
                66 => assert!(matches!(
                    error,
                    LibraryBackendError::Session(SessionError::Ui(tetrad_ui::UiError::TextTooLong))
                )),
                77 => assert!(matches!(
                    error,
                    LibraryBackendError::Session(SessionError::SceneCount(9))
                )),
                88 => assert!(matches!(
                    error,
                    LibraryBackendError::Library(tetrad_library::Error::Storage(
                        tetrad_storage::Error::ProductMissing(_)
                    ))
                )),
                _ => assert!(matches!(error, LibraryBackendError::UnknownHandle(999))),
            }
            assert_eq!(backend.snapshot().unwrap().snapshot(), &original_view);
            assert_eq!(backend.provenance().ui_handle, 11);
            assert_eq!(backend.session().export(), wire(b"One"));
        }
        fixture.assert_unchanged();
    }

    #[test]
    fn dirty_switch_is_refused_same_handle_keeps_baseline_and_exact_revert_can_switch() {
        let fixture = Fixture::new(vec![
            product("first", &wire(b"One")),
            product("second", &wire(b"Two")),
        ]);
        let mut backend = fixture.backend(None);
        let edit = UiCommand::SetSceneLabel {
            scene: scene(0),
            label: "Bridge".into(),
        };
        assert!(matches!(
            backend.apply(&[edit.clone(), UiCommand::RequestPreset { id: 22 }]),
            Err(LibraryBackendError::DirtySwitch { from: 11, to: 22 })
        ));
        assert_eq!(backend.session().export(), wire(b"One"));
        backend.apply(&[edit]).unwrap();
        let edited = backend.session().export();
        assert!(edited.ends_with(&[0x9b, 6, 8, 0x81, 0, 0x9c, 6]));
        assert!(backend.apply(&[]).unwrap().snapshot().dirty);
        assert!(
            backend
                .apply(&[UiCommand::RequestPreset { id: 11 }])
                .unwrap()
                .snapshot()
                .dirty
        );
        assert_eq!(backend.session().export(), edited);
        assert!(matches!(
            backend.apply(&[UiCommand::RequestPreset { id: 22 }]),
            Err(LibraryBackendError::DirtySwitch { .. })
        ));
        assert_eq!(backend.session().export(), edited);
        backend
            .apply(&[
                UiCommand::SetSceneLabel {
                    scene: scene(0),
                    label: "A".into(),
                },
                UiCommand::RequestPreset { id: 22 },
                UiCommand::SetSceneColor {
                    scene: scene(1),
                    raw_color: 3,
                },
            ])
            .unwrap();
        assert_eq!(backend.provenance().ui_handle, 22);
        assert!(backend.snapshot().unwrap().snapshot().dirty);
        assert_eq!(
            backend.snapshot().unwrap().snapshot().scenes[1].raw_color,
            3
        );
        fixture.assert_unchanged();
    }

    #[test]
    fn semantic_revert_that_changes_wire_encoding_does_not_allow_dirty_switch() {
        let first = [0x7a, 0x81, 0, b'A'];
        let fixture = Fixture::new(vec![
            product("first", &first),
            product("second", &wire(b"Two")),
        ]);
        let mut backend = fixture.backend(None);
        backend
            .apply(&[
                UiCommand::SetSceneLabel {
                    scene: scene(0),
                    label: "Longer".into(),
                },
                UiCommand::SetSceneLabel {
                    scene: scene(0),
                    label: "A".into(),
                },
            ])
            .unwrap();
        assert_ne!(backend.session().export(), first);
        assert!(backend.snapshot().unwrap().snapshot().dirty);
        assert!(matches!(
            backend.apply(&[UiCommand::RequestPreset { id: 22 }]),
            Err(LibraryBackendError::DirtySwitch { .. })
        ));
        assert_eq!(backend.provenance().ui_handle, 11);
        fixture.assert_unchanged();
    }

    #[test]
    fn late_errors_after_successful_candidate_loads_do_not_commit_sources_or_views() {
        let fixture = Fixture::new(vec![
            product("first", &wire(b"One")),
            product("second", &wire(b"Two")),
        ]);
        let mut backend = fixture.backend(None);
        let original_view = backend.snapshot().unwrap().snapshot().clone();
        for last in [
            UiCommand::RequestPreset { id: 999 },
            UiCommand::SetSceneLabel {
                scene: scene(7),
                label: "Absent".into(),
            },
        ] {
            assert!(
                backend
                    .apply(&[
                        UiCommand::RequestPreset { id: 22 },
                        UiCommand::SelectScene { scene: scene(1) },
                        last,
                    ])
                    .is_err()
            );
            assert_eq!(backend.snapshot().unwrap().snapshot(), &original_view);
            assert_eq!(backend.session().export(), wire(b"One"));
            assert_eq!(backend.provenance().product_id, "first");
        }
        assert!(matches!(
            backend.apply(&[
                UiCommand::RequestPreset { id: 22 },
                UiCommand::SetSceneLabel {
                    scene: scene(0),
                    label: "Changed second".into()
                },
                UiCommand::RequestPreset { id: 11 },
            ]),
            Err(LibraryBackendError::DirtySwitch { from: 22, to: 11 })
        ));
        assert_eq!(backend.snapshot().unwrap().snapshot(), &original_view);
        assert_eq!(backend.loaded.source.document().to_bytes(), wire(b"One"));
        assert_eq!(
            backend.loaded.source.metadata(),
            Some([0xff, 0, 0x80, 3].as_slice())
        );
        fixture.assert_unchanged();
    }

    #[test]
    fn explicit_grid_policy_admits_metadata_and_projection_failures_are_inert() {
        let fixture = Fixture::new(vec![
            product("empty-grid", &grid_wire(false)),
            product("occupied-grid", &grid_wire(true)),
        ]);
        let mut strict = fixture.backend(Some(tetrad_engine::ImportPolicy::STRICT));
        let original_view = strict.snapshot().unwrap().snapshot().clone();
        assert!(matches!(
            strict.apply(&[
                UiCommand::SelectScene { scene: scene(1) },
                UiCommand::RequestPreset { id: 22 },
            ]),
            Err(LibraryBackendError::Session(SessionError::GridImport(_)))
        ));
        assert_eq!(strict.snapshot().unwrap().snapshot(), &original_view);
        assert_eq!(strict.provenance().ui_handle, 11);
        assert_eq!(strict.session().export(), grid_wire(false));
        assert!(
            LibraryBackend::new(
                fixture.library(),
                fixture.bindings(),
                22,
                scene(0),
                Some(tetrad_engine::ImportPolicy::STRICT)
            )
            .is_err()
        );
        let policy = tetrad_engine::ImportPolicy {
            absent: tetrad_engine::AbsentScalars::Refuse,
            bypass: tetrad_engine::BypassPolarity::StoredTrueBypasses,
        };
        let mut backend = fixture.backend(Some(policy));
        let view = backend
            .apply(&[UiCommand::RequestPreset { id: 22 }])
            .unwrap();
        assert_eq!(view.snapshot().grid[0].label, "MODEL 7");
        assert_eq!(view.snapshot().preset_id, None);
        assert_eq!(view.snapshot().presets.len(), 2);
        // Fault injection reaches the final projection AFTER candidate.apply.
        backend.grid_policy = Some(tetrad_engine::ImportPolicy::STRICT);
        assert!(matches!(
            backend.apply(&[UiCommand::SelectScene { scene: scene(1) }]),
            Err(LibraryBackendError::Session(SessionError::GridImport(_)))
        ));
        assert_eq!(
            backend.session().snapshot().unwrap().selected_scene,
            scene(0)
        );
        assert_eq!(backend.provenance().ui_handle, 22);
        assert_eq!(backend.session().export(), grid_wire(true));
        fixture.assert_unchanged();
    }

    #[test]
    fn unsupported_commands_limits_and_read_only_edits_never_fake_effects() {
        let mut read_only = product("locked", &wire(b"Two"));
        read_only.read_only = true;
        let fixture = Fixture::new(vec![product("first", &wire(b"One")), read_only]);
        let mut backend = fixture.backend(None);
        let original_view = backend.snapshot().unwrap().snapshot().clone();
        for unsupported in [
            UiCommand::PreviewPreset { id: 22 },
            UiCommand::CycleMode,
            UiCommand::BeginBankPreview,
            UiCommand::EndBankPreview,
            UiCommand::CancelBankPreview,
            UiCommand::InspectCell {
                position: tetrad_ui::GridPosition::new(0, 0).unwrap(),
            },
        ] {
            let error = backend
                .apply(&[
                    UiCommand::SetSceneLabel {
                        scene: scene(0),
                        label: "Never applied".into(),
                    },
                    unsupported.clone(),
                ])
                .unwrap_err();
            assert!(
                matches!(error, LibraryBackendError::Session(SessionError::Unsupported(command)) if command == unsupported)
            );
            assert_eq!(backend.snapshot().unwrap().snapshot(), &original_view);
        }
        assert!(matches!(
            backend.apply(&vec![
                UiCommand::RequestPreset { id: 11 };
                MAX_BATCH_COMMANDS + 1
            ]),
            Err(LibraryBackendError::BatchLimit)
        ));
        assert!(matches!(
            backend.apply(&[UiCommand::SetSceneLabel {
                scene: scene(0),
                label: "x".repeat(tetrad_ui::MAX_TEXT_BYTES + 1)
            }]),
            Err(LibraryBackendError::Ui(tetrad_ui::UiError::TextTooLong))
        ));
        assert!(matches!(
            backend.apply(&[UiCommand::Rejected(tetrad_ui::UiError::StaleSceneDraft)]),
            Err(LibraryBackendError::Ui(tetrad_ui::UiError::StaleSceneDraft))
        ));
        backend
            .apply(&[
                UiCommand::RequestPreset { id: 22 },
                UiCommand::SelectScene { scene: scene(1) },
            ])
            .unwrap();
        assert!(backend.provenance().read_only);
        let locked_view = backend.snapshot().unwrap().snapshot().clone();
        for edit in [
            UiCommand::SetSceneLabel {
                scene: scene(0),
                label: "A".into(),
            },
            UiCommand::SetSceneColor {
                scene: scene(0),
                raw_color: 9,
            },
        ] {
            assert!(matches!(
                backend.apply(&[UiCommand::SelectScene { scene: scene(0) }, edit]),
                Err(LibraryBackendError::ReadOnlyProduct)
            ));
            assert_eq!(backend.snapshot().unwrap().snapshot(), &locked_view);
        }
        backend
            .apply(&[UiCommand::RequestPreset { id: 11 }])
            .unwrap();
        assert!(!backend.provenance().read_only);
        assert_eq!(backend.session().export(), wire(b"One"));
        fixture.assert_unchanged();
    }
}
