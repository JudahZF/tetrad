use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::json;
use tetrad_storage::{Folder, FolderId, Library, Limits, Mutation, Product, ProductData};

use crate::{Error, Result, io, print_json};

#[derive(Subcommand)]
pub enum Command {
    /// Create a new private owned database. Existing targets are refused.
    Create { path: PathBuf },
    /// List product identities using owned bytewise ordering.
    List {
        path: PathBuf,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        count: usize,
    },
    /// Read metadata and data sizes; never print opaque product bytes.
    Inspect {
        path: PathBuf,
        #[arg(long)]
        id: String,
    },
    /// Create one owned folder with caller-selected opaque type codes.
    Folder {
        path: PathBuf,
        #[arg(long)]
        id: i64,
        #[arg(long)]
        parent: Option<i64>,
        #[arg(long)]
        name: String,
        #[arg(long, allow_hyphen_values = true)]
        product_type: i64,
        #[arg(long, allow_hyphen_values = true)]
        folder_type: i64,
    },
    /// Insert opaque data and optionally link it in one transaction. No stock import.
    Import {
        path: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long, allow_hyphen_values = true)]
        product_type: i64,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        metadata: Option<PathBuf>,
        #[arg(long)]
        folder: Option<i64>,
    },
    /// Export raw product bytes to a new private file. No overwrite or conversion.
    Export {
        path: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
}

fn reject_source_alias(database: &Path, input: &Path) -> Result<()> {
    #[cfg(unix)]
    let aliases = {
        use std::os::unix::fs::MetadataExt;
        let target = database.metadata()?;
        let source = input.metadata()?;
        target.dev() == source.dev() && target.ino() == source.ino()
    };
    #[cfg(not(unix))]
    let aliases = database.canonicalize()? == input.canonicalize()?;
    if aliases {
        return Err(Error::DatabaseAlias {
            database: database.to_owned(),
            input: input.to_owned(),
        });
    }
    Ok(())
}

fn reject_reserved_path(database: &Path, selected: &Path) -> Result<()> {
    let database_path = database.canonicalize()?;
    let parent = selected
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = selected.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "expected a file path")
    })?;
    let location = parent.canonicalize()?.join(name);
    let resolved = match selected.canonicalize() {
        Ok(path) => Some(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    let selected_metadata = match selected.metadata() {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };

    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = database_path.as_os_str().to_os_string();
        name.push(suffix);
        let reserved = PathBuf::from(name);
        let aliases = location == reserved || resolved.as_ref() == Some(&reserved);
        #[cfg(unix)]
        let aliases = aliases || {
            use std::os::unix::fs::MetadataExt;
            match reserved.metadata() {
                Ok(target) => selected_metadata.as_ref().is_some_and(|source| {
                    source.dev() == target.dev() && source.ino() == target.ino()
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            }
        };
        if aliases {
            return Err(Error::DatabaseSidecar {
                database: database.to_owned(),
                path: selected.to_owned(),
            });
        }
    }
    Ok(())
}

pub fn run(command: Command) -> Result<bool> {
    let limits = Limits::default();
    match command {
        Command::Create { path } => {
            let _library = Library::create_owned(&path, limits)?;
            print_json(
                &json!({"database": path.to_string_lossy(), "created": true, "format": "tetrad_owned_sqlite_v1", "stock_database_mutated": false}),
            )?;
        }
        Command::List { path, after, count } => {
            let library = Library::open_owned(&path, limits, true)?;
            let ids = library.product_ids(after.as_deref(), count)?;
            print_json(
                &json!({"product_ids": ids, "ordering": "owned_binary_identity", "database_mutated": false}),
            )?;
        }
        Command::Inspect { path, id } => {
            let library = Library::open_owned(&path, limits, true)?;
            let product = library
                .product(&id)?
                .ok_or_else(|| tetrad_storage::Error::ProductMissing(id))?;
            print_json(&json!({
                "id": product.id, "product_type": product.product_type, "read_only": product.read_only,
                "raw_data_bytes": product.data.raw_data.as_ref().map(Vec::len),
                "metadata_bytes": product.data.metadata.as_ref().map(Vec::len),
                "database_mutated": false, "hardware_compatibility": "unverified"
            }))?;
        }
        Command::Folder {
            path,
            id,
            parent,
            name,
            product_type,
            folder_type,
        } => {
            let folder = Folder {
                id: FolderId::try_from(id)?,
                parent: parent.map(FolderId::try_from).transpose()?,
                name,
                product_type,
                folder_type,
                read_only: false,
            };
            let mut library = Library::open_owned(&path, limits, false)?;
            library.apply(&[Mutation::CreateFolder(&folder)])?;
            print_json(
                &json!({"folder_id": folder.id.value(), "database_mutated": true, "stock_database_mutated": false}),
            )?;
        }
        Command::Import {
            path,
            id,
            product_type,
            input,
            metadata,
            folder,
        } => {
            reject_reserved_path(&path, &input)?;
            reject_source_alias(&path, &input)?;
            if let Some(metadata) = &metadata {
                reject_reserved_path(&path, metadata)?;
                reject_source_alias(&path, metadata)?;
            }
            let raw_data = io::read_bounded(&input, limits.max_row_bytes - 4_096)?;
            let metadata = metadata
                .as_deref()
                .map(|path| io::read_bounded(path, limits.max_row_bytes - 4_096))
                .transpose()?;
            let folder = folder.map(FolderId::try_from).transpose()?;
            let product = Product {
                id,
                product_type,
                read_only: false,
                data: ProductData {
                    raw_data: Some(raw_data),
                    metadata,
                },
            };
            let mut library = Library::open_owned(&path, limits, false)?;
            let mut mutations = vec![Mutation::InsertProduct(&product)];
            if let Some(folder) = folder {
                mutations.push(Mutation::Link {
                    folder,
                    product: &product.id,
                });
            }
            library.apply(&mutations)?;
            print_json(
                &json!({"id": product.id, "database_mutated": true, "source_modified": false,
                "stock_database_mutated": false, "conversion_performed": false, "hardware_compatibility": "unverified"}),
            )?;
        }
        Command::Export { path, id, output } => {
            reject_reserved_path(&path, &output)?;
            let library = Library::open_owned(&path, limits, true)?;
            let product = library
                .product(&id)?
                .ok_or_else(|| tetrad_storage::Error::ProductMissing(id.clone()))?;
            let bytes = product.data.raw_data.ok_or(Error::RawDataAbsent(id))?;
            io::write_new(&output, &bytes)?;
            print_json(
                &json!({"output": output.to_string_lossy(), "byte_count": bytes.len(), "database_mutated": false, "conversion_performed": false}),
            )?;
        }
    }
    Ok(true)
}
