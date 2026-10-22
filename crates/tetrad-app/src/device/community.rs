//! Community blocks on the device (O): qcdsp IR graphs from extension packages.
//!
//! [`install`] compiles each [`Source`] against the stock Volume framework of this unit
//! (`qcdsp::device::build`; the Volume blob and its per-core records come from the unit's own
//! firmware files, see [`super::dsp::Dsp::volume_template`]), gives it a local model ID and
//! puts its spec in the catalog (`tetrad_catalog::community`). The deploy then loads the
//! generated blob through the stock `BlobLoad` path ([`CommunityBlobs::load`]): no file, no
//! blob key. Tetrad compiles the IR itself, so a package never brings SHARC code. A block
//! can only change the audio in its own slot.

use std::collections::BTreeMap;

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

/// One compiled source, kept so that an unchanged source is never compiled again.
#[derive(Debug, Clone)]
pub struct Compiled {
    fingerprint: u64,
    spec: ModelSpec,
    built: Built,
}

/// Compiles `sources` (with `template`, or reports every new block as not ready without one)
/// and replaces the community models of `specs`. `cache` holds the last result by key: an
/// unchanged source reuses its blob and spec, and a key keeps its model ID for as long as it
/// is installed, so undo frames and mappings stay valid. A new key gets a stable hash ID in its
/// category ([`community::local_id`]).
pub fn install(
    sources: &[Source],
    template: Option<&VolumeTemplate>,
    specs: &mut Specs,
    cache: &mut BTreeMap<String, Compiled>,
) -> (CommunityBlobs, Vec<Status>) {
    specs.remove_community();
    let mut sources: Vec<&Source> = sources.iter().collect();
    sources.sort_by(|a, b| a.key.cmp(&b.key));
    sources.dedup_by(|a, b| a.key == b.key);
    let mut previous = std::mem::take(cache);
    // Keys that stay keep their IDs: reserve them before a new key probes.
    let reserved: BTreeMap<u32, &str> = (sources.iter())
        .filter_map(|source| Some((previous.get(&source.key)?.spec.id, source.key.as_str())))
        .collect();
    let mut blobs = CommunityBlobs::default();
    let mut statuses = Vec::with_capacity(sources.len());
    for source in sources {
        let fingerprint = fingerprint(std::slice::from_ref(source));
        let kept = previous
            .remove(&source.key)
            .filter(|compiled| compiled.fingerprint == fingerprint);
        let id = reserved
            .iter()
            .find(|(_, key)| **key == source.key)
            .map(|(&id, _)| id);
        let compiled = match (kept, template) {
            (Some(compiled), _) => Ok(compiled),
            (None, None) => Err("the DSP firmware files are not available".to_string()),
            (None, Some(template)) => {
                let taken = |id: u32| {
                    specs.models.contains_key(&id)
                        || reserved.get(&id).is_some_and(|key| *key != source.key)
                };
                build(source, template, specs, id, taken)
                    .map(|(spec, built)| Compiled {
                        fingerprint,
                        spec,
                        built,
                    })
                    .map_err(|error| error.to_string())
            }
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
    id: Option<u32>,
    taken: impl Fn(u32) -> bool,
) -> Result<(ModelSpec, Built), BuildError> {
    let nodes = source.ir.expanded().node_count();
    if nodes > MAX_COMPILED_NODES {
        return Err(BuildError::TooLarge(nodes));
    }
    let category = source.category.id();
    let id = match id.filter(|&id| id / 1000 == category) {
        Some(id) => id,
        None => community::local_id(category, &source.key, taken).ok_or(BuildError::NoId)?,
    };
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
        let (blobs, mut statuses) = install(sources, template, specs, &mut compiled);
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
        // Without the framework the next sync tries again, even with the same sources.
        self.state.community = Installed {
            statuses,
            fingerprint: template_error.is_none().then_some(fingerprint),
            compiled,
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
            let recalled = self.recall_preset(preset);
            self.report(recalled);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use qc_format::diff::DiffFile;

    use super::*;

    /// The stock Volume framework from `QC_CORPUS_DIR` (docs/development.md, "Corpus tests").
    /// `None` without it.
    fn template() -> Option<VolumeTemplate> {
        let root = PathBuf::from(std::env::var_os("QC_CORPUS_DIR")?);
        let plain = root.join("plain");
        let raw = std::fs::read(plain.join("blobs/core1/volume.raw")).ok()?;
        // As `Dsp::volume_template`: the decrypted blob is parsed, then written again; the
        // stock hash must survive that.
        let core1 = ModuleBlob::parse(&raw).unwrap().to_bytes().unwrap();
        assert_eq!(core1, raw);
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
            file.get("volume").unwrap().clone()
        });
        Some(VolumeTemplate::new(&core1, diffs).unwrap())
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
        let (blobs, statuses) = install(&sources, None, &mut specs, &mut BTreeMap::new());
        assert!(blobs.is_empty() && specs.models.is_empty());
        assert!(statuses[0].model.is_none() && statuses[0].error.is_some());
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
        // 40 000 samples of delay need far more than the 32 KB of block state.
        let mut big = qcdsp::BlockBuilder::new("big", "Big", qcdsp::Category::Delay);
        let line = big.delay(40_000).unwrap();
        big.update_delay(line, big.input()).unwrap();
        let big = big.finish(line.signal()).unwrap();
        let sources = [
            source("a/b:c/sat", qcdsp::examples::saturator().unwrap()),
            source("a/b:c/big", big),
        ];
        let mut specs = Specs::default();
        let mut cache = BTreeMap::new();
        let (blobs, statuses) = install(&sources, Some(&template), &mut specs, &mut cache);
        let sat = statuses.iter().find(|s| s.key == "a/b:c/sat").unwrap();
        let id = sat.model.expect("the saturator builds");
        assert_eq!(id / 1000, 0, "Guitar Overdrive band");
        assert!(sat.cpu.is_some_and(|cpu| cpu > 0));
        let big = statuses.iter().find(|s| s.key == "a/b:c/big").unwrap();
        assert!(big.model.is_none() && big.error.as_deref().is_some_and(|e| e.contains("0x7fff")));
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
        let (_, again) = install(&sources, None, &mut specs, &mut cache);
        assert_eq!(
            again[1].model,
            Some(id),
            "the cached block needs no compile"
        );
        // A new key whose hash takes the saturator's ID probes past it: the old key keeps it.
        let mut more = sources.to_vec();
        let thief = (0..10_000)
            .map(|n| format!("a/b:c/a{n}"))
            .find(|key| community::local_id(0, key, |_| false) == Some(id))
            .unwrap();
        more.push(source(&thief, qcdsp::examples::saturator().unwrap()));
        let (_, third) = install(&more, Some(&template), &mut specs, &mut cache);
        let status = |key: &str| third.iter().find(|s| s.key == key).unwrap().model;
        assert_eq!(status("a/b:c/sat"), Some(id));
        assert!(status(&thief).is_some_and(|other| other != id));
    }
}
