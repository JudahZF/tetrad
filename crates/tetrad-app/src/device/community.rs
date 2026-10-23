//! Community blocks on the device (O): qcdsp IR graphs from extension packages.
//!
//! [`install`] compiles each [`Source`] against the stock Volume framework of this unit
//! (`qcdsp::device::build`; the Volume blob and its per-core records come from the unit's own
//! firmware files, see `Dsp::volume_template`), gives it a local model ID and
//! puts its spec in the catalog (`tetrad_catalog::community`). The deploy then loads the
//! generated blob through the stock `BlobLoad` path ([`CommunityBlobs::load`]): no file, no
//! blob key. Tetrad compiles the IR itself, so a package never brings SHARC code. A block
//! can only change the audio in its own slot.

use std::collections::{BTreeMap, BTreeSet};

use qc_format::{blob::ModuleBlob, diff::DiffRecord};
use qcdsp::device::VolumeTemplate;
use tetrad_catalog::Specs;
use tetrad_catalog::community::{self, CommunitySpec, DEFAULT_PLACEHOLDER};
use tetrad_catalog::{ModelRole, ModelSpec};
use tetrad_dsp_host::blob;
use tetrad_dsp_host::session::BlobLoad;
use tetrad_dsp_protocol::DspCore;

/// One community block to install: a static block of a package, or a block that an
/// extension built from an asset.
#[derive(Debug, Clone)]
pub struct Source {
    /// `<installation key>/<block id>`, plus `#<variant>` for a block built from an asset.
    pub key: String,
    /// The display name in the picker and the editor.
    pub name: String,
    pub category: qcdsp::Category,
    /// The manifest's stock placeholder; `None` or an unusable model gives the stock Gain.
    pub placeholder: Option<u32>,
    /// Validated: the package decode, the `blocks.publish` check and the generated cache all
    /// call `qcdsp::Block::validate`.
    pub ir: qcdsp::Block,
}

/// The result of one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub key: String,
    pub name: String,
    /// The local model ID when the block is ready.
    pub model: Option<u32>,
    /// Why the block is not ready.
    pub error: Option<String>,
    /// CPU in percent of one DSP core, as the fit charges it.
    pub cpu: Option<u32>,
}

#[derive(Debug, Clone)]
struct Built {
    name: String,
    core1: ModuleBlob,
    /// Transport cores 1..=3 (`blobs/core2..4`).
    diffs: [DiffRecord; 3],
}

/// The generated blobs by local model ID.
#[derive(Debug, Clone, Default)]
pub struct CommunityBlobs(BTreeMap<u32, Built>);

impl CommunityBlobs {
    /// The load of `model` on `core`, or `None` when `model` is not a community block.
    pub fn load(&self, core: DspCore, model: u32) -> Option<Result<BlobLoad, blob::BlobError>> {
        let built = self.0.get(&model)?;
        let diff = match core {
            DspCore::Core0 => None,
            core => built.diffs.get(core as usize - 1),
        };
        Some(
            blob::for_core(&built.name, &built.core1, core, diff).map(|blob| BlobLoad {
                model_id: model,
                blob: Some(blob),
            }),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The largest graph Tetrad compiles, counted after macro expansion (one `tanh` is about 30
/// nodes). It bounds the compile time on the device loop: about 0.2 s on the host.
pub const MAX_COMPILED_NODES: usize = 24_576;

#[derive(Debug, thiserror::Error)]
enum BuildError {
    #[error(transparent)]
    Dsp(#[from] qcdsp::Error),
    #[error(transparent)]
    Spec(#[from] community::CommunityError),
    #[error("no free model ID in this category")]
    NoId,
    #[error("the graph has {0} nodes after expansion; the limit is {MAX_COMPILED_NODES}")]
    TooLarge(usize),
}

/// Saved model IDs of community blocks by key, a JSON object ([`Ids`]).
pub const IDS_FILE: &str = "/media/p4/tetrad/community-ids.json";
const MAX_IDS_FILE: u64 = 1 << 20;

/// Model IDs by source key. Mappings and undo frames hold model IDs, so a key keeps its ID
/// across restarts and across the installs of other keys.
pub type Ids = BTreeMap<String, u32>;

/// The model ID of each of `sources` (sorted by key), `None` when its range is full. A key
/// keeps its saved ID while that ID is free of stock models (`stock`) and in its category's
/// range. A new key takes [`community::local_id`] past the stock IDs and every saved ID; only
/// when the range is full does it take the ID of a saved key that is not installed now.
/// `ids` gets the new assignments.
pub fn assign_ids(
    sources: &[&Source],
    stock: impl Fn(u32) -> bool,
    ids: &mut Ids,
) -> Vec<Option<u32>> {
    let mut assigned = BTreeSet::new();
    let mut out: Vec<Option<u32>> = (sources.iter())
        .map(|source| {
            let id = *ids.get(&source.key)?;
            let fits = id / 1000 == source.category.id() && community::is_local(id);
            (fits && !stock(id) && assigned.insert(id)).then_some(id)
        })
        .collect();
    let saved: BTreeSet<u32> = ids.values().copied().collect();
    for (slot, source) in out.iter_mut().zip(sources) {
        if slot.is_some() {
            continue;
        }
        let category = source.category.id();
        let free = |id: u32| !stock(id) && !assigned.contains(&id);
        *slot = community::local_id(category, &source.key, |id| !free(id) || saved.contains(&id))
            .or_else(|| community::local_id(category, &source.key, |id| !free(id)));
        if let Some(id) = *slot {
            assigned.insert(id);
            ids.retain(|_, saved| *saved != id);
            ids.insert(source.key.clone(), id);
        }
    }
    out
}

/// One compiled source, kept so that an unchanged source is never compiled again.
#[derive(Debug, Clone)]
pub struct Compiled {
    fingerprint: u64,
    spec: ModelSpec,
    built: Built,
}

/// Compiles `sources` (with `template`, or reports every new block as not ready without one)
/// and replaces the community models of `specs`. `cache` holds the last result by key: an
/// unchanged source reuses its blob and spec. `ids` gives the model IDs ([`assign_ids`]).
pub fn install(
    sources: &[Source],
    template: Option<&VolumeTemplate>,
    specs: &mut Specs,
    cache: &mut BTreeMap<String, Compiled>,
    ids: &mut Ids,
) -> (CommunityBlobs, Vec<Status>) {
    specs.remove_community();
    let mut sources: Vec<&Source> = sources.iter().collect();
    sources.sort_by(|a, b| a.key.cmp(&b.key));
    sources.dedup_by(|a, b| a.key == b.key);
    let ids = assign_ids(&sources, |id| specs.models.contains_key(&id), ids);
    let mut previous = std::mem::take(cache);
    let mut blobs = CommunityBlobs::default();
    let mut statuses = Vec::with_capacity(sources.len());
    for (source, id) in sources.into_iter().zip(ids) {
        let fingerprint = fingerprint(std::slice::from_ref(source));
        let kept = (previous.remove(&source.key))
            .filter(|compiled| compiled.fingerprint == fingerprint && Some(compiled.spec.id) == id);
        let compiled = match (kept, template, id) {
            (Some(compiled), ..) => Ok(compiled),
            (None, None, _) => Err("the DSP firmware files are not available".to_string()),
            (None, Some(_), None) => Err(BuildError::NoId.to_string()),
            (None, Some(template), Some(id)) => build(source, template, specs, id)
                .map(|(spec, built)| Compiled {
                    fingerprint,
                    spec,
                    built,
                })
                .map_err(|error| error.to_string()),
        };
        let mut status = Status {
            key: source.key.clone(),
            name: source.name.clone(),
            model: None,
            error: None,
            cpu: None,
        };
        match compiled {
            Ok(compiled) => {
                let id = compiled.spec.id;
                status.model = Some(id);
                status.cpu = compiled.spec.stats.map(|stats| stats.cpu.ceil() as u32);
                blobs.0.insert(id, compiled.built.clone());
                specs.models.insert(id, compiled.spec.clone());
                cache.insert(source.key.clone(), compiled);
            }
            Err(error) => status.error = Some(error),
        }
        statuses.push(status);
    }
    (blobs, statuses)
}

fn build(
    source: &Source,
    template: &VolumeTemplate,
    specs: &Specs,
    id: u32,
) -> Result<(ModelSpec, Built), BuildError> {
    // `validate` bounds the expanded size of a block, so this expansion is bounded too.
    let nodes = source.ir.expanded().node_count();
    if nodes > MAX_COMPILED_NODES {
        return Err(BuildError::TooLarge(nodes));
    }
    // The blob name holds the ID and an IR hash: a changed block never reuses the blob that
    // the deploy kept for its old version.
    let ir = source.ir.to_json()?;
    let hash = ir.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    let block =
        source
            .ir
            .clone()
            .renamed(format!("tx{id}_{hash:08x}"), &source.name, source.category)?;
    let device = qcdsp::device::build(&block, template, id, &source.name)?;
    let placeholder = placeholder(source.placeholder, specs);
    let spec = community::spec(
        &block,
        id,
        &device.blob_name,
        &device.module_element,
        CommunitySpec {
            key: source.key.clone(),
            placeholder,
        },
    )?;
    let built = Built {
        name: device.blob_name,
        core1: device.core1,
        diffs: device.diffs,
    };
    Ok((spec, built))
}

/// The manifest's placeholder when it is a stock grid block, else the stock Gain.
fn placeholder(wanted: Option<u32>, specs: &Specs) -> u32 {
    wanted
        .filter(|&id| {
            specs.model(id).is_some_and(|spec| {
                spec.role == ModelRole::Block && spec.community.is_none() && !spec.hidden
            })
        })
        .unwrap_or(DEFAULT_PLACEHOLDER)
}

/// The installed community blocks: what [`install`] last built, and a fingerprint of its
/// sources so that an unchanged set is not built again.
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub statuses: Vec<Status>,
    fingerprint: Option<u64>,
    compiled: BTreeMap<String, Compiled>,
    /// [`IDS_FILE`], read at the first install.
    ids: Option<Ids>,
}

impl Installed {
    /// The status of the block with `key`.
    pub fn status(&self, key: &str) -> Option<&Status> {
        self.statuses.iter().find(|status| status.key == key)
    }
}

/// FNV-1a over every source's key, name, category, placeholder and IR.
fn fingerprint(sources: &[Source]) -> u64 {
    let mut keys: Vec<String> = sources
        .iter()
        .map(|source| {
            format!(
                "{}\u{0}{}\u{0}{}\u{0}{:?}\u{0}{}",
                source.key,
                source.name,
                source.category.id(),
                source.placeholder,
                source.ir.to_json().unwrap_or_default()
            )
        })
        .collect();
    keys.sort();
    keys.iter()
        .flat_map(|key| key.bytes().chain([0xff]))
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// [`IDS_FILE`]; empty when it is missing or bad (the IDs then follow from the keys alone).
fn read_ids(platform: &mut impl super::platform::Platform) -> Ids {
    use super::platform::ServiceBackends;
    use tetrad_services::exec::Files;
    let files = platform.services().files();
    (files
        .read_bounded(std::path::Path::new(IDS_FILE), MAX_IDS_FILE)
        .ok()
        .flatten())
    .and_then(|bytes: Vec<u8>| serde_json::from_slice(&bytes).ok())
    .unwrap_or_default()
}

fn save_ids(platform: &mut impl super::platform::Platform, ids: &Ids) -> Result<(), String> {
    use super::platform::ServiceBackends;
    use tetrad_services::exec::Files;
    let bytes = serde_json::to_vec(ids).map_err(|error| error.to_string())?;
    let files = platform.services().files();
    let path = std::path::Path::new(IDS_FILE);
    (path.parent())
        .map_or(Ok(()), |dir| files.create_dirs(dir))
        .and_then(|()| files.write_atomic(path, &bytes))
        .map_err(|error| error.to_string())
}

impl<P: super::platform::Platform> super::Device<P> {
    /// Installs `sources` as the community blocks. When they changed: the loaded preset leaves
    /// with the old catalog (each community block becomes its placeholder and a record), the
    /// catalog and the generated blobs change, and the preset is recalled. The recall puts
    /// back every block that is still installed, so a block that arrives late (after the boot
    /// preset, or when an extension built it from an asset) starts to play.
    pub(super) fn sync_community(&mut self, sources: &[Source]) {
        let fingerprint = fingerprint(sources);
        if self.state.community.fingerprint == Some(fingerprint) {
            return;
        }
        let Some(specs) = self.state.specs.as_mut() else {
            return;
        };
        let exported = (self.state.current.as_ref())
            .and_then(|current| super::preset::exported(&current.preset, specs).ok());
        let mut template_error = None;
        // The firmware files are read only when there is a block to build.
        let dsp = self.dsp.as_mut().filter(|_| !sources.is_empty());
        let template = match dsp {
            Some(dsp) => match dsp.volume_template(&mut self.platform) {
                Ok(template) => Some(template),
                Err(error) => {
                    template_error = Some(error.to_string());
                    None
                }
            },
            None => None,
        };
        let mut compiled = std::mem::take(&mut self.state.community.compiled);
        let mut ids = match self.state.community.ids.take() {
            Some(ids) => ids,
            None => read_ids(&mut self.platform),
        };
        let saved = ids.clone();
        let (blobs, mut statuses) = install(sources, template, specs, &mut compiled, &mut ids);
        let ids_error = (ids != saved)
            .then(|| save_ids(&mut self.platform, &ids))
            .and_then(Result::err);
        if let Some(error) = &template_error {
            for status in statuses.iter_mut().filter(|status| status.model.is_none()) {
                status.error = Some(format!("the stock Volume framework: {error}"));
            }
        }
        if let Some(dsp) = self.dsp.as_mut() {
            dsp.community = blobs;
        }
        for status in &statuses {
            let detail = match (&status.model, &status.error) {
                (Some(model), _) => format!(
                    "community block {} is model {model} ({}% CPU)",
                    status.key,
                    status.cpu.unwrap_or_default()
                ),
                (None, error) => format!(
                    "community block {} is not ready: {}",
                    status.key,
                    error.as_deref().unwrap_or("unknown error")
                ),
            };
            self.notice(super::Subsystem::Extensions, detail);
        }
        if let Some(error) = ids_error {
            self.notice(
                super::Subsystem::Extensions,
                format!("community block IDs not saved: {error}"),
            );
        }
        // Without the framework the next sync tries again, even with the same sources.
        self.state.community = Installed {
            statuses,
            fingerprint: template_error.is_none().then_some(fingerprint),
            compiled,
            ids: Some(ids),
        };
        self.state.module_stats = std::cell::OnceCell::new();
        self.bridges.ui.invalidate_models();
        self.bridges.extensions.view_stale = true;
        let records = exported.filter(|preset| {
            preset
                .unknown
                .bytes_field(tetrad_preset::community::TETRAD_BLOCKS_FIELD)
                .is_some()
        });
        if let Some(preset) = records {
            // A clean preset stays clean: the blocks arrived, the user changed nothing.
            let was_clean = !self.state.preset_dirty();
            let recalled = self.recall_preset(preset);
            if self.report(recalled).is_some() && was_clean {
                *self.state.clean.get_mut() =
                    self.state.current.as_ref().map(super::preset::Snapshot::of);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use qc_format::diff::{DiffFile, DiffRecord};

    use super::*;

    /// The stock Volume framework from `QC_CORPUS_DIR` (docs/development.md, "Corpus tests"),
    /// with the kernel map that arena blocks need. `None` without it.
    fn template() -> Option<VolumeTemplate> {
        let root = PathBuf::from(std::env::var_os("QC_CORPUS_DIR")?);
        let plain = root.join("plain");
        let read = |name: &str| -> (Vec<u8>, [DiffRecord; 3]) {
            let raw = std::fs::read(plain.join(format!("blobs/core1/{name}.raw"))).unwrap();
            let diffs = [2, 3, 4].map(|core| {
                let path = format!("blobs/core{core}/diffs.blob");
                let path = [
                    plain.join(&path),
                    root.join("rootfs/opt/neuraldsp").join(&path),
                ]
                .into_iter()
                .find(|path| path.is_file())
                .expect("diffs.blob in plain/ or rootfs/");
                let file = DiffFile::parse(&std::fs::read(path).unwrap()).unwrap();
                file.get(name).unwrap().clone()
            });
            (raw, diffs)
        };
        let (raw, diffs) = read("volume");
        // As `Dsp::volume_template`: the decrypted blob is parsed, then written again; the
        // stock hash must survive that.
        let core1 = ModuleBlob::parse(&raw).unwrap().to_bytes().unwrap();
        assert_eq!(core1, raw);
        let (calls, call_diffs) = read(qcdsp::device::template::KERNEL_CALL_SOURCE);
        let mut kernel = qcdsp::device::KernelMap::default();
        kernel
            .add_stock_calls(&ModuleBlob::parse(&calls).unwrap(), &call_diffs)
            .unwrap();
        Some(
            VolumeTemplate::new(&core1, diffs)
                .unwrap()
                .with_kernel(kernel),
        )
    }

    fn source(key: &str, ir: qcdsp::Block) -> Source {
        Source {
            key: key.into(),
            name: "Saturator".into(),
            category: qcdsp::Category::GuitarOverdrive,
            placeholder: Some(1),
            ir,
        }
    }

    /// Without the DSP firmware files nothing is built and the catalog keeps no community model.
    #[test]
    fn no_template_builds_nothing() {
        let mut specs = Specs::default();
        let sources = [source("a/b:c/sat", qcdsp::examples::saturator().unwrap())];
        let (blobs, statuses) = install(
            &sources,
            None,
            &mut specs,
            &mut BTreeMap::new(),
            &mut Ids::new(),
        );
        assert!(blobs.is_empty() && specs.models.is_empty());
        assert!(statuses[0].model.is_none() && statuses[0].error.is_some());
    }

    /// A key keeps its saved ID when a new key's hash collides with it, when it is not installed
    /// for a while, and after a restart; it moves only off a stock ID, and a full range reuses
    /// the ID of a key that is not installed.
    #[test]
    fn saved_ids_keep_each_key_on_its_model() {
        let ir = qcdsp::examples::saturator().unwrap();
        let key = "a/b:c/sat";
        let id = community::local_id(0, key, |_| false).unwrap();
        let thief = (0..10_000)
            .map(|n| format!("a/b:c/a{n}"))
            .find(|other| community::local_id(0, other, |_| false) == Some(id))
            .unwrap();
        let (sat, other) = (source(key, ir.clone()), source(&thief, ir.clone()));
        let mut ids = Ids::new();
        assert_eq!(assign_ids(&[&sat], |_| false, &mut ids), [Some(id)]);
        // The thief sorts first and is new: it probes past the saved ID, also while the
        // saturator is not installed.
        let moved = assign_ids(&[&other], |_| false, &mut ids)[0].unwrap();
        assert_ne!(moved, id);
        let both = [Some(moved), Some(id)];
        assert_eq!(assign_ids(&[&other, &sat], |_| false, &mut ids), both);
        assert_eq!(
            assign_ids(&[&other, &sat], |_| false, &mut ids.clone()),
            both
        );
        // A stock model on the saved ID moves the key, and the new ID is saved.
        let after = assign_ids(&[&sat], |stock| stock == id, &mut ids)[0].unwrap();
        assert!(after != id && ids[key] == after);
        // A full range: the one free ID is the saturator's, and the thief takes it.
        let full = |stock| stock != after;
        assert_eq!(assign_ids(&[&other], full, &mut ids), [Some(after)]);
        assert!(!ids.contains_key(key));
    }

    /// Each block links into a blob for every DSP core with the same stable ID; a block that does
    /// not fit the framework reports why; an unknown placeholder falls back to the stock Gain.
    #[test]
    #[ignore = "reads the Volume blob from $QC_CORPUS_DIR"]
    fn installs_blocks_for_every_core() {
        let Some(template) = template() else {
            eprintln!("skipped: set QC_CORPUS_DIR");
            return;
        };
        let line = |samples| {
            let mut block = qcdsp::BlockBuilder::new("line", "Line", qcdsp::Category::Delay);
            let line = block.delay(samples).unwrap();
            block.update_delay(line, block.input()).unwrap();
            block.finish(line.signal()).unwrap()
        };
        // 9000 samples of stereo delay (72 KB) need a heap-0 arena; 40 000 (320 KB) do not fit
        // heap 0 and take an SD arena; 2.2 M (17.6 MB) do not fit the SD heap either.
        let sources = [
            source("a/b:c/sat", qcdsp::examples::saturator().unwrap()),
            source("a/b:c/huge", line(2_200_000)),
            source("a/b:c/sd", line(40_000)),
            source("a/b:c/arena", qcdsp::examples::delay(9000).unwrap()),
        ];
        let mut specs = Specs::default();
        let (mut cache, mut ids) = (BTreeMap::new(), Ids::new());
        let (blobs, statuses) =
            install(&sources, Some(&template), &mut specs, &mut cache, &mut ids);
        let sat = statuses.iter().find(|s| s.key == "a/b:c/sat").unwrap();
        let id = sat.model.expect("the saturator builds");
        assert_eq!(id / 1000, 0, "Guitar Overdrive band");
        assert!(sat.cpu.is_some_and(|cpu| cpu > 0));
        let huge = statuses.iter().find(|s| s.key == "a/b:c/huge").unwrap();
        assert!(huge.model.is_none() && huge.error.is_some(), "{huge:?}");
        let sd = statuses.iter().find(|s| s.key == "a/b:c/sd").unwrap();
        let sd_stats = specs
            .model(sd.model.expect("an SD arena block builds"))
            .unwrap()
            .stats
            .unwrap();
        assert!(
            sd_stats.sd_heap > 320_000 && sd_stats.dm_heap < 1_000,
            "the SD arena is charged to the SD heap: {sd_stats:?}"
        );
        let arena = statuses.iter().find(|s| s.key == "a/b:c/arena").unwrap();
        let arena_id = arena.model.expect("an arena block builds");
        let arena_stats = specs.model(arena_id).unwrap().stats.unwrap();
        assert!(
            arena_stats.dm_heap > 72_000,
            "the arena is charged: {arena_stats:?}"
        );
        for core in [
            DspCore::Core0,
            DspCore::Core1,
            DspCore::Core2,
            DspCore::Core3,
        ] {
            blobs
                .load(core, arena_id)
                .unwrap()
                .unwrap()
                .blob
                .unwrap()
                .validate()
                .unwrap();
        }
        let spec = specs.model(id).unwrap();
        let community = spec.community.as_ref().unwrap();
        assert_eq!(community.placeholder, DEFAULT_PLACEHOLDER);
        for core in [
            DspCore::Core0,
            DspCore::Core1,
            DspCore::Core2,
            DspCore::Core3,
        ] {
            let load = blobs.load(core, id).unwrap().unwrap();
            assert_eq!(load.model_id, id);
            load.blob.unwrap().validate().unwrap();
        }
        assert!(blobs.load(DspCore::Core0, id + 1).is_none());
        // Unchanged: no compile (no template needed), the same ID.
        let (_, again) = install(&sources, None, &mut specs, &mut cache, &mut ids);
        let sat = again.iter().find(|s| s.key == "a/b:c/sat").unwrap();
        assert_eq!(sat.model, Some(id), "the cached block needs no compile");
        // A new key whose hash takes the saturator's ID: the saturator keeps it, and after a
        // restart (an empty cache, the saved IDs) every key has the ID it had live.
        let mut more = sources.to_vec();
        let thief = (0..10_000)
            .map(|n| format!("a/b:c/a{n}"))
            .find(|key| community::local_id(0, key, |_| false) == Some(id))
            .unwrap();
        more.push(source(&thief, qcdsp::examples::saturator().unwrap()));
        let (_, live) = install(&more, Some(&template), &mut specs, &mut cache, &mut ids);
        let model =
            |statuses: &[Status], key: &str| statuses.iter().find(|s| s.key == key).unwrap().model;
        assert_eq!(model(&live, "a/b:c/sat"), Some(id));
        assert!(model(&live, &thief).is_some_and(|other| other != id));
        let (_, restarted) = install(
            &more,
            Some(&template),
            &mut Specs::default(),
            &mut BTreeMap::new(),
            &mut ids.clone(),
        );
        assert_eq!(restarted, live);
    }
}
