//! Remote (owned, Tetrad; docs/remote.md): the IR library page: the user IRs, their folders and
//! the saving destination. Factory IRs are not in `zenui.db`, so the page shows user IRs only.
//! Each write takes the path of the touch UI (a Directory request or an action), so the
//! Directory, Cortex Control and the settings stay in step. Each answer is the new listing.

use serde_json::{Value, json};
use tetrad_library::stock::{
    MAX_MEDIA_FOLDER_DEPTH, MAX_MEDIA_FOLDERS, media_folder_key, saving_destination,
};
use tetrad_media::factory::DeviceCipher;
use tetrad_media::user::{MAX_IR_NAME_UNITS, MIN_IR_NAME_UNITS, UserIrMetadata, dialog_name};
use tetrad_remote::{LibraryOp, Reply};
use tetrad_storage::stock::{ProductType, StockDb, StockFolder, StockView};
use tetrad_ui::{DirectoryRequest, MediaKind};

use super::actions::{self, Action};
use super::platform::Platform;
use super::{Device, DeviceError, Subsystem};

impl<P: Platform> Device<P> {
    pub(super) fn remote_library(&mut self, op: LibraryOp) -> Reply {
        let done = match op {
            LibraryOp::List => Ok(()),
            _ if !self.options.persist => Err("this run does not write the library".to_owned()),
            LibraryOp::Rename { id, name } => self.remote_rename_ir(&id, &name),
            LibraryOp::Delete { id } => self.remote_delete_ir(&id),
            LibraryOp::Link { id, folder, linked } => self.remote_link_ir(&id, folder, linked),
            LibraryOp::Destination { folder } => self
                .ir_folder(folder)
                .and_then(|_| self.remote_action(Action::SetSavingDestination { folder })),
            LibraryOp::CreateFolder { name, parent } => self.remote_create_folder(&name, parent),
        };
        match done.and_then(|()| self.ir_library()) {
            Ok(listing) => Reply::Json(listing),
            Err(reason) => Reply::Refused(reason),
        }
    }

    /// The listing: `destination` (a folder id), `folders` (the writable IR folders, the root
    /// first) and `irs` (the user IRs in `zenui.db` order, each with the folders that hold it).
    /// An IR whose metadata does not read is left out, as in the Directory.
    fn ir_library(&self) -> Result<Value, String> {
        let (db, cipher) = self.ir_store()?;
        let view = db.view();
        let folders = ir_folders(&view)?;
        let stored = &self.state.settings.settings().library;
        let destination = saving_destination(
            &view,
            ProductType::Ir,
            stored.download_destination_ir.as_deref(),
        )
        .map_err(|error| error.to_string())?;
        let mut held = std::collections::BTreeMap::<String, Vec<i64>>::new();
        for folder in &folders {
            let ids = view.folder_products(folder.id);
            for id in ids.map_err(|error| error.to_string())? {
                held.entry(id).or_default().push(folder.id);
            }
        }
        let ids = view.product_ids(ProductType::Ir);
        let mut irs = Vec::new();
        for id in ids.map_err(|error| error.to_string())? {
            let row = view.product(&id).map_err(|error| error.to_string())?;
            let blob = row.and_then(|row| row.metadata).unwrap_or_default();
            let Ok(metadata) = UserIrMetadata::from_blob(&blob, cipher) else {
                continue;
            };
            irs.push(json!({
                "id": id,
                "name": metadata.name,
                "date": metadata.date,
                "folders": held.remove(&id).unwrap_or_default(),
            }));
        }
        let folders: Vec<_> = (folders.iter())
            .map(|folder| {
                json!({
                    "id": folder.id,
                    "name": folder_name(folder),
                    "parent": folder.parent,
                    "root": folder.parent == 0,
                })
            })
            .collect();
        Ok(json!({ "destination": destination, "folders": folders, "irs": irs }))
    }

    fn remote_rename_ir(&mut self, id: &str, name: &str) -> Result<(), String> {
        let name = dialog_name(name).map_err(|_| name_rule("An IR name"))?;
        let notice = format!("Remote: IR {} renamed to {name}", self.user_ir_name(id)?);
        let kind = MediaKind::Ir;
        let id = id.to_owned();
        let request = DirectoryRequest::RenameMedia { kind, id, name };
        self.edit_directory(request)
            .map_err(|error| error.to_string())?;
        self.notice(Subsystem::Remote, notice);
        Ok(())
    }

    /// Stock lets an IR in use go: a block that names it shows its stored name or "DELETED IR".
    fn remote_delete_ir(&mut self, id: &str) -> Result<(), String> {
        let notice = format!("Remote: IR {} deleted", self.user_ir_name(id)?);
        let kind = MediaKind::Ir;
        let id = id.to_owned();
        self.edit_directory(DirectoryRequest::DeleteMedia { kind, id })
            .map_err(|error| error.to_string())?;
        self.notice(Subsystem::Remote, notice);
        Ok(())
    }

    /// Every user IR stays in the IR root ("My IRs"), as in the Directory. Only an IR folder
    /// takes an IR: `StockWrite::link` checks that a folder is writable, not its product type.
    fn remote_link_ir(&mut self, id: &str, folder: i64, linked: bool) -> Result<(), String> {
        self.user_ir_name(id)?;
        let root = self.ir_folder(folder)?.parent == 0;
        if root && !linked {
            return Err("Every IR stays in the IR root folder".into());
        }
        let products = vec![id.to_owned()];
        self.remote_action(Action::LinkMedia {
            folder,
            products,
            linked,
        })
    }

    /// A new folder in `parent`, or in the IR root. Names follow the touch keyboard (4 to 60).
    fn remote_create_folder(&mut self, name: &str, parent: Option<i64>) -> Result<(), String> {
        let name = dialog_name(name).map_err(|_| name_rule("A folder name"))?;
        let parent = match parent {
            Some(parent) => self.ir_folder(parent)?.id,
            None => (self.ir_folders()?.first())
                .map(|root| root.id)
                .ok_or("the IR root folder is missing")?,
        };
        let kind = MediaKind::Ir;
        match self.edit_directory(DirectoryRequest::CreateFolder { kind, parent, name }) {
            Ok(()) => Ok(()),
            Err(DeviceError::Library(tetrad_library::Error::Storage(
                tetrad_storage::Error::Limit,
            ))) => Err(format!(
                "There can be {MAX_MEDIA_FOLDERS} IR folders, {MAX_MEDIA_FOLDER_DEPTH} levels deep"
            )),
            Err(error) => Err(error.to_string()),
        }
    }

    /// One action as a touch sends it. The touch path adds a Control push and an extension
    /// event only for other actions (`control::action_push`, `extensions::native_change`).
    fn remote_action(&mut self, action: Action) -> Result<(), String> {
        let effects =
            actions::reduce(action, &mut self.state).map_err(|error| error.to_string())?;
        self.apply(Ok::<_, DeviceError>(effects));
        Ok(())
    }

    /// The name of user IR `id`. Refuses an id that is not a user IR.
    fn user_ir_name(&self, id: &str) -> Result<String, String> {
        let (db, cipher) = self.ir_store()?;
        let row = (db.view().product(id).map_err(|error| error.to_string())?)
            .filter(|row| row.product_type == ProductType::Ir)
            .ok_or("This IR is not in the library")?;
        UserIrMetadata::from_blob(&row.metadata.unwrap_or_default(), cipher)
            .map(|metadata| metadata.name)
            .map_err(|error| error.to_string())
    }

    /// Writable IR folder `id`.
    fn ir_folder(&self, id: i64) -> Result<StockFolder, String> {
        (self.ir_folders()?.into_iter())
            .find(|folder| folder.id == id)
            .ok_or_else(|| "This IR folder is not in the library".to_owned())
    }

    fn ir_folders(&self) -> Result<Vec<StockFolder>, String> {
        ir_folders(&self.ir_store()?.0.view())
    }

    /// `zenui.db` and the cipher of the IR metadata.
    fn ir_store(&self) -> Result<(&StockDb, &DeviceCipher), String> {
        let db = (self.state.library.as_ref())
            .and_then(|library| library.database.as_ref())
            .ok_or("the library is closed")?;
        let cipher = (self.media.as_ref())
            .map(|media| media.cipher())
            .ok_or("no device media cipher")?;
        Ok((db, cipher))
    }
}

/// The IR folders that hold user IRs and take a saving destination: the root first.
fn ir_folders(view: &StockView<'_>) -> Result<Vec<StockFolder>, String> {
    let mut folders: Vec<_> = (view.folders().map_err(|error| error.to_string())?)
        .into_iter()
        .filter(|folder| {
            folder.product_type == ProductType::Ir
                && !folder.read_only
                && media_folder_key(folder).is_some()
        })
        .collect();
    folders.sort_by_key(|folder| folder.parent != 0);
    Ok(folders)
}

/// As `remote::folder_named`: an unnamed folder shows as "IRs".
fn folder_name(folder: &StockFolder) -> &str {
    match folder.name.as_str() {
        "" => "IRs",
        name => name,
    }
}

fn name_rule(what: &str) -> String {
    format!("{what} has {MIN_IR_NAME_UNITS} to {MAX_IR_NAME_UNITS} characters")
}
