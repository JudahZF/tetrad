//! Factory IR catalog: `/opt/neuraldsp/factory_ir_entities.pb`.
//!
//! Message `zenui.CachedIREntities { repeated CachedIREntity entities = 1; }`
//! with `CachedIREntity { tag = 1; cab = 2; mic = 3; readable_name = 4;
//! repeated files = 5; }` (descriptor strings 0x1227258..0x12272d0, reader VA
//! 0x32922c). The pinned catalog has 690 entities and 11,515 file paths named
//! `<dir>/<tag>_<cab>_<mic>_<suffix>.bin`.

use crate::factory::FACTORY_IR_DIR;
use crate::proto::{ProtoError, Reader, put_bytes};

pub const FACTORY_IR_ENTITIES_PATH: &str = "/opt/neuraldsp/factory_ir_entities.pb";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogLimits {
    pub max_bytes: usize,
    pub max_entities: usize,
    pub max_files_per_entity: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("IR catalog has {actual} bytes, exceeding limit {limit}")]
    Size { actual: usize, limit: usize },
    #[error("IR catalog exceeds {0} entities")]
    EntityLimit(usize),
    #[error("IR catalog entity {entity} exceeds {limit} files")]
    FileLimit { entity: usize, limit: usize },
    #[error("IR catalog entity {entity} is missing field {field}")]
    MissingField { entity: usize, field: &'static str },
    #[error("IR catalog entity {entity} repeats field {field}")]
    RepeatedField { entity: usize, field: &'static str },
    #[error("factory IR file {file:?} does not belong to entity {entity}")]
    ForeignFile { entity: usize, file: String },
    #[error("factory IR file {file:?} has an unexpected position suffix")]
    UnexpectedName { file: String },
    #[error(transparent)]
    Proto(#[from] ProtoError),
}

/// Microphone placement encoded in a factory file suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MicPlacement {
    /// `A0`..`C5` or `A 0`..`C 5`: position row letter and distance step.
    Grid { position: u8, distance: u8 },
    /// `0`..`8` or `01`..`14`: a single numbered distance step.
    Step(u8),
    /// `FAR`: the room microphone of numbered sets.
    Far,
}

impl MicPlacement {
    /// Parses a suffix without the `.bin` extension. Letters are A..=Z (row
    /// 0..=25) and digits 0..=9; numbered steps are 0..=99.
    pub fn parse(suffix: &str) -> Option<Self> {
        let bytes = suffix.as_bytes();
        match bytes {
            b"FAR" => Some(Self::Far),
            [letter @ b'A'..=b'Z', digit @ b'0'..=b'9']
            | [letter @ b'A'..=b'Z', b' ', digit @ b'0'..=b'9'] => Some(Self::Grid {
                position: letter - b'A',
                distance: digit - b'0',
            }),
            [b'0'..=b'9'] | [b'0'..=b'9', b'0'..=b'9'] => suffix.parse().ok().map(Self::Step),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactoryIrFile {
    pub path: String,
    pub placement: MicPlacement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactoryIrEntity {
    pub tag: String,
    pub cab: String,
    pub mic: String,
    pub readable_name: String,
    pub files: Vec<FactoryIrFile>,
}

impl FactoryIrEntity {
    /// The `ir selector` parameter value used by cab blocks: `<tag>_<cab>_<mic>`.
    pub fn selector(&self) -> String {
        format!("{}_{}_{}", self.tag, self.cab, self.mic)
    }

    pub fn file(&self, placement: MicPlacement) -> Option<&FactoryIrFile> {
        self.files.iter().find(|file| file.placement == placement)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FactoryIrCatalog {
    entities: Vec<FactoryIrEntity>,
}

impl FactoryIrCatalog {
    pub fn new(entities: Vec<FactoryIrEntity>) -> Self {
        Self { entities }
    }

    pub fn entities(&self) -> &[FactoryIrEntity] {
        &self.entities
    }

    /// Looks up a cab block `ir selector` value (`<tag>_<cab>_<mic>`) the stock
    /// way (VA 0x3289c4): `_` becomes a space, then match `readable_name`.
    pub fn by_selector(&self, selector: &str) -> Option<&FactoryIrEntity> {
        let readable = selector.replace('_', " ");
        self.entities
            .iter()
            .find(|entity| entity.readable_name == readable)
    }

    /// Strict decode: every entity has each scalar field once, and every file
    /// path is `<FACTORY_IR_DIR>/<tag>_<cab>_<mic>_<suffix>.bin` with a known suffix.
    pub fn decode(bytes: &[u8], limits: CatalogLimits) -> Result<Self, CatalogError> {
        if bytes.len() > limits.max_bytes {
            return Err(CatalogError::Size {
                actual: bytes.len(),
                limit: limits.max_bytes,
            });
        }
        let mut entities = Vec::new();
        for field in Reader::new(bytes) {
            let (number, value) = field?;
            if number != 1 {
                continue;
            }
            if entities.len() == limits.max_entities {
                return Err(CatalogError::EntityLimit(limits.max_entities));
            }
            entities.push(decode_entity(value.bytes(1)?, entities.len(), limits)?);
        }
        Ok(Self { entities })
    }

    /// Stock cache rebuild (VA 0x32922c → 0x3271e8), used when the cache file
    /// is missing or does not parse. Takes the file names in
    /// [`FACTORY_IR_DIR`]; keeps `*.bin` (case-insensitive) and sorts them by
    /// lowercase name (Qt4 `QDir::Name | IgnoreCase`). Each base name (up to
    /// the first `.`) splits on `_`; the last part must contain a digit or be
    /// `FAR`. Consecutive files with the same prefix form one entity, whose
    /// readable name is the prefix joined with spaces.
    ///
    /// Owned policy: names whose prefix is not exactly `tag_cab_mic`, or
    /// whose suffix [`MicPlacement::parse`] rejects, are skipped and returned.
    pub fn rebuild<'a>(file_names: impl IntoIterator<Item = &'a str>) -> (Self, Vec<String>) {
        let mut names: Vec<&str> = file_names
            .into_iter()
            .filter(|name| {
                name.len() > 4
                    && name.is_char_boundary(name.len() - 4)
                    && name[name.len() - 4..].eq_ignore_ascii_case(".bin")
            })
            .collect();
        names.sort_by_cached_key(|name| name.to_lowercase());
        let mut entities: Vec<FactoryIrEntity> = Vec::new();
        let mut skipped = Vec::new();
        for name in names {
            let base = name.split('.').next().unwrap_or_default();
            let parts: Vec<&str> = base.split('_').collect();
            let Some((suffix, prefix)) = parts.split_last() else {
                continue;
            };
            if !(suffix.contains(|c: char| c.is_ascii_digit()) || *suffix == "FAR") {
                continue;
            }
            let (&[tag, cab, mic], Some(placement)) = (prefix, MicPlacement::parse(suffix)) else {
                skipped.push(name.to_owned());
                continue;
            };
            let file = FactoryIrFile {
                path: format!("{FACTORY_IR_DIR}/{name}"),
                placement,
            };
            match entities.last_mut() {
                Some(last) if last.tag == tag && last.cab == cab && last.mic == mic => {
                    last.files.push(file);
                }
                _ => entities.push(FactoryIrEntity {
                    tag: tag.to_owned(),
                    cab: cab.to_owned(),
                    mic: mic.to_owned(),
                    readable_name: format!("{tag} {cab} {mic}"),
                    files: vec![file],
                }),
            }
        }
        (Self { entities }, skipped)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for entity in &self.entities {
            let mut message = Vec::new();
            for (field, text) in [
                (1, &entity.tag),
                (2, &entity.cab),
                (3, &entity.mic),
                (4, &entity.readable_name),
            ] {
                put_bytes(&mut message, field, text.as_bytes());
            }
            for file in &entity.files {
                put_bytes(&mut message, 5, file.path.as_bytes());
            }
            put_bytes(&mut out, 1, &message);
        }
        out
    }
}

fn decode_entity(
    bytes: &[u8],
    entity: usize,
    limits: CatalogLimits,
) -> Result<FactoryIrEntity, CatalogError> {
    const NAMES: [&str; 4] = ["tag", "cab", "mic", "readable_name"];
    let mut scalars: [Option<String>; 4] = Default::default();
    let mut paths = Vec::new();
    for field in Reader::new(bytes) {
        let (number, value) = field?;
        match number {
            1..=4 => {
                let slot = &mut scalars[number as usize - 1];
                if slot.is_some() {
                    return Err(CatalogError::RepeatedField {
                        entity,
                        field: NAMES[number as usize - 1],
                    });
                }
                *slot = Some(value.string(number)?.to_owned());
            }
            5 => {
                if paths.len() == limits.max_files_per_entity {
                    return Err(CatalogError::FileLimit {
                        entity,
                        limit: limits.max_files_per_entity,
                    });
                }
                paths.push(value.string(5)?.to_owned());
            }
            _ => {}
        }
    }
    let [tag, cab, mic, readable_name] = scalars;
    let missing = |index: usize| CatalogError::MissingField {
        entity,
        field: NAMES[index],
    };
    let tag = tag.ok_or_else(|| missing(0))?;
    let cab = cab.ok_or_else(|| missing(1))?;
    let mic = mic.ok_or_else(|| missing(2))?;
    let readable_name = readable_name.ok_or_else(|| missing(3))?;
    let prefix = format!("{FACTORY_IR_DIR}/{tag}_{cab}_{mic}_");
    let files = paths
        .into_iter()
        .map(|path| {
            let Some(suffix) = path
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix(".bin"))
            else {
                return Err(CatalogError::ForeignFile { entity, file: path });
            };
            match MicPlacement::parse(suffix) {
                Some(placement) => Ok(FactoryIrFile { path, placement }),
                None => Err(CatalogError::UnexpectedName { file: path }),
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(FactoryIrEntity {
        tag,
        cab,
        mic,
        readable_name,
        files,
    })
}
