use super::domain::{Budget, check_limit};
use super::*;
use std::collections::HashSet;

pub const SCENES: usize = 8;
pub const ASSIGNMENTS: usize = 12;
pub const ASSIGNMENT_GROUPS: usize = 10;

/// Ordinary, non-null numeric QVariant payloads and QString payloads. Unknown
/// alternatives carry only their type ID so reached comparisons can refuse them;
/// they cannot produce an equality result. No live/custom payload is accepted.
/// `ConstructorInvalid` is an assertion of completed Parameter constructor64
/// provenance (or a proved immutable copy), Qt ID0, nonshared, null=true. It
/// contains no union payload: word68 is unspecified and never canonicalized.
/// Unproved raw type0, clears, setters, overlays and partial construction must
/// use an unsupported marker. This is no target-memory parser or producer proof.
/// Its comparison domain is only reached `ParameterFields::opaque_variant64`;
/// other storage may retain it as an unreached placeholder but operations refuse.
/// At comparison the caller also asserts stable reviewed Qt cmp/copy/destroy/
/// capability bindings and the default handler under the existing ordinary
/// policy, with complete coherent immutable operands. No live binding is checked.
/// Synthetic defaults remain unchanged. Values have no semantic `Eq`.
#[derive(Debug, Clone)]
pub enum Variant {
    Int32(i32),
    UInt32(u32),
    Float32(F32),
    Double64(F64),
    String(Utf16),
    /// Payload-free caller assertion; see the enum's provenance contract.
    ConstructorInvalid,
    UnsupportedTypeId(u32),
}
impl Default for Variant {
    fn default() -> Self {
        Self::Int32(0)
    }
}
impl Variant {
    pub fn type_id(&self) -> u32 {
        match self {
            Self::ConstructorInvalid => 0,
            Self::Int32(_) => 2,
            Self::UInt32(_) => 3,
            Self::Float32(_) => 135,
            Self::Double64(_) => 6,
            Self::String(_) => 10,
            Self::UnsupportedTypeId(id) => *id,
        }
    }
    fn validate(&self, budget: &mut Budget, path: &Path) -> Result<(), AdmissionError> {
        budget.scalars(1, path)?;
        if let Self::String(text) = self {
            budget.text(text, path)?;
        }
        Ok(())
    }
}

/// Recovered compared Parameter fields. Offset names deliberately avoid schema
/// guesses. `Default` constructs a synthetic zero fixture only.
#[derive(Debug, Clone, Default)]
pub struct ParameterFields {
    pub opaque_word0: u32,
    pub opaque_bytes4: Vec<u8>,
    pub type8: u32,
    pub current16: Variant,
    pub default32: Variant,
    /// The sole comparison site that admits proved `Variant::ConstructorInvalid`.
    pub opaque_variant64: Variant,
    pub opaque_byte80: u8,
    pub step_count84: i32,
    pub opaque_string88: Utf16,
    pub opaque_bytes92: Vec<u8>,
    pub opaque_float96: F32,
    pub opaque_float100: F32,
    pub opaque_word140: u32,
    pub step144: F32,
    pub opaque_byte148: u8,
    pub opaque_float152: F32,
    pub suppress_current156: u32,
    pub expression_float160: F32,
    pub expression_float164: F32,
    pub opaque_byte168: u8,
    pub opaque_byte169: u8,
    pub opaque_word176: u32,
    pub opaque_word180: u32,
    pub opaque_word184: u32,
    pub opaque_string188: Utf16,
    pub opaque_string192: Utf16,
    pub opaque_string196: Utf16,
    pub opaque_string200: Utf16,
    pub scene_mode204: u8,
    pub opaque_vector208: Vec<i32>,
    pub opaque_vector220: Vec<i32>,
    pub opaque_vector232: Vec<i32>,
    pub linked_scene_mode244: Vec<i32>,
    pub opaque_word256: u32,
    /// Retains all four bytes; comparison masks bytes260/263.
    pub packed260: [u8; 4],
    pub points264: Vec<Point>,
    pub omitted: ParameterOmissions,
}
#[derive(Debug, Clone, Default)]
pub struct ParameterOmissions {
    pub vector104: Vec<i32>,
    pub vector116: Vec<i32>,
    pub vector128: Vec<i32>,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct Point {
    pub x: F32,
    pub y: F32,
}

#[derive(Debug, Clone)]
pub struct Parameter(ParameterFields);
impl Parameter {
    pub fn new(fields: ParameterFields) -> Result<Self, AdmissionError> {
        let parameter = Self(fields);
        parameter.validate(&mut Budget::default(), &Path::root("parameter"))?;
        Ok(parameter)
    }
    pub fn fields(&self) -> &ParameterFields {
        &self.0
    }
    fn validate(&self, budget: &mut Budget, p: &Path) -> Result<(), AdmissionError> {
        let f = &self.0;
        budget.parameters(p)?;
        // Fixed-width words, bytes, float values and packed bytes, including omissions.
        budget.scalars(24, p)?;
        for (name, value) in [
            ("current16", &f.current16),
            ("default32", &f.default32),
            ("opaque_variant64", &f.opaque_variant64),
        ] {
            value.validate(budget, &p.field(name))?;
        }
        for (name, bytes) in [
            ("opaque_bytes4", &f.opaque_bytes4),
            ("opaque_bytes92", &f.opaque_bytes92),
        ] {
            budget.bytes(bytes, &p.field(name))?;
        }
        for (name, text) in [
            ("opaque_string88", &f.opaque_string88),
            ("opaque_string188", &f.opaque_string188),
            ("opaque_string192", &f.opaque_string192),
            ("opaque_string196", &f.opaque_string196),
            ("opaque_string200", &f.opaque_string200),
        ] {
            budget.text(text, &p.field(name))?;
        }
        for (name, values) in [
            ("opaque_vector208", &f.opaque_vector208),
            ("opaque_vector220", &f.opaque_vector220),
            ("opaque_vector232", &f.opaque_vector232),
            ("linked_scene_mode244", &f.linked_scene_mode244),
            ("omitted.vector104", &f.omitted.vector104),
            ("omitted.vector116", &f.omitted.vector116),
            ("omitted.vector128", &f.omitted.vector128),
        ] {
            budget.vector(values.len(), &p.field(name))?;
        }
        check_limit(&p.field("points264"), f.points264.len(), MAX_ITEMS)?;
        budget.scalars(f.points264.len() * 2, &p.field("points264"))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ModelRecord {
    pub opaque_word0: u32,
    pub opaque_half4: u16,
    pub opaque_word8: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ModelOmissions {
    /// Excluded comparator field; transform research names this byte252.
    pub opaque_byte252: u8,
    /// Optional capture of the eight excluded primary banks/caches. Absence is
    /// metadata absence, not a missing compared second bank.
    pub primary40: Option<[Vec<Variant>; SCENES]>,
    pub primary_cache64: Option<[F32; SCENES]>,
}

#[derive(Debug, Clone)]
pub struct ModelFields {
    pub descriptors28: Vec<Parameter>,
    /// Checked to contain exactly eight scenes, each within its own descriptors.
    pub second52: Vec<Vec<Variant>>,
    pub second_cache96: [F32; SCENES],
    pub opaque_word24: u32,
    pub opaque_word0: u32,
    pub opaque_vector4: Vec<u32>,
    pub opaque_word16: u32,
    pub opaque_string20: Utf16,
    pub record188: ModelRecord,
    pub record200: ModelRecord,
    pub record212: ModelRecord,
    pub opaque_byte228: u8,
    pub opaque_word232: u32,
    pub opaque_word240: u32,
    pub opaque_word236: u32,
    pub omitted: ModelOmissions,
}
impl Default for ModelFields {
    fn default() -> Self {
        Self {
            descriptors28: Vec::new(),
            second52: vec![Vec::new(); SCENES],
            second_cache96: [F32::default(); SCENES],
            opaque_word24: 0,
            opaque_word0: 0,
            opaque_vector4: Vec::new(),
            opaque_word16: 0,
            opaque_string20: Utf16::default(),
            record188: ModelRecord::default(),
            record200: ModelRecord::default(),
            record212: ModelRecord::default(),
            opaque_byte228: 0,
            opaque_word232: 0,
            opaque_word240: 0,
            opaque_word236: 0,
            omitted: ModelOmissions::default(),
        }
    }
}
#[derive(Debug, Clone)]
pub struct Model(ModelFields);
impl Model {
    pub fn new(fields: ModelFields) -> Result<Self, AdmissionError> {
        let model = Self(fields);
        model.validate(&mut Budget::default(), &Path::root("model"))?;
        Ok(model)
    }
    pub fn fields(&self) -> &ModelFields {
        &self.0
    }
    fn validate(&self, budget: &mut Budget, p: &Path) -> Result<(), AdmissionError> {
        let f = &self.0;
        budget.models(p)?;
        check_limit(
            &p.field("descriptors28"),
            f.descriptors28.len(),
            MAX_PARAMETERS,
        )?;
        if f.second52.len() != SCENES {
            return Err(AdmissionError {
                path: p.field("second52"),
                reason: AdmissionReason::SceneBanks {
                    expected: SCENES,
                    actual: f.second52.len(),
                },
            });
        }
        for (i, scene) in f.second52.iter().enumerate() {
            let q = p.field("second52").index(i);
            if scene.len() > f.descriptors28.len() {
                return Err(AdmissionError {
                    path: q,
                    reason: AdmissionReason::BankExceedsDescriptors {
                        values: scene.len(),
                        descriptors: f.descriptors28.len(),
                    },
                });
            }
            budget.scene_values(scene.len(), &q)?;
            for (j, value) in scene.iter().enumerate() {
                value.validate(budget, &q.index(j))?;
            }
        }
        for (i, descriptor) in f.descriptors28.iter().enumerate() {
            descriptor.validate(budget, &p.field("descriptors28").index(i))?;
        }
        budget.vector(f.opaque_vector4.len(), &p.field("opaque_vector4"))?;
        budget.text(&f.opaque_string20, &p.field("opaque_string20"))?;
        budget.scalars(25, p)?;
        if let Some(banks) = &f.omitted.primary40 {
            for (i, bank) in banks.iter().enumerate() {
                let q = p.field("omitted.primary40").index(i);
                check_limit(&q, bank.len(), MAX_PARAMETERS)?;
                budget.scene_values(bank.len(), &q)?;
                for (j, value) in bank.iter().enumerate() {
                    value.validate(budget, &q.index(j))?;
                }
            }
        }
        if f.omitted.primary_cache64.is_some() {
            budget.scalars(SCENES, p)?;
        }
        Ok(())
    }
}

/// Node content in captured tree traversal order. The graph's tree comparator
/// and key schema are not guessed; all four recovered payload words are stored.
#[derive(Debug, Clone, Copy, Default)]
pub struct GraphNode {
    pub opaque_word24: u32,
    pub opaque_word20: u32,
    pub opaque_word16: u32,
    pub opaque_word28: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WordMapValue {
    pub opaque_word20: u32,
    pub opaque_word24: u32,
}
#[derive(Debug, Clone)]
pub struct ModelPair {
    pub model20: Model,
    pub model284: Model,
}
#[derive(Debug, Clone)]
pub struct PairedRecord {
    pub opaque_word0: u32,
    pub opaque_word4: u32,
    pub model8: Model,
    pub model272: Model,
}

#[derive(Debug, Clone)]
pub struct MapEntry<T> {
    pub key16: u32,
    pub value: T,
}
/// Unique keys in caller-captured `_Rb_tree_increment` order. Construction does
/// not sort or invent signedness/less-than semantics for stock tree keys.
#[derive(Debug, Clone)]
pub struct TraversalMap<T>(Vec<MapEntry<T>>);
impl<T> Default for TraversalMap<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<T> TraversalMap<T> {
    pub fn from_entries(entries: Vec<MapEntry<T>>) -> Result<Self, AdmissionError> {
        check_limit(&Path::root("map"), entries.len(), MAX_ITEMS)?;
        let mut keys = HashSet::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            if !keys.insert(entry.key16) {
                return Err(AdmissionError {
                    path: Path::root("map").index(i).field("key16"),
                    reason: AdmissionReason::DuplicateMapKey(entry.key16),
                });
            }
        }
        Ok(Self(entries))
    }
    pub fn entries(&self) -> &[MapEntry<T>] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Assignment {
    pub opaque_byte0: u8,
    pub opaque_word4: u32,
    pub opaque_word8: u32,
    pub opaque_word12: u32,
    pub opaque_word16: u32,
    pub omitted_timestamp24: u64,
}
#[derive(Debug, Clone, Default)]
pub struct PresetOmissions {
    pub name_key5392: Utf16,
    pub identity5404: u32,
    // Width of +5780 is not pinned; no invented scalar/byte representation.
    pub string5792: Utf16,
    pub string5796: Utf16,
    pub string5804: Utf16,
    pub string5808: Utf16,
    pub string5812: Utf16,
    pub string5816: Utf16,
    pub byte5824: u8,
    /// External UI selected scene is explicitly excluded from semantic fields.
    pub selected_scene: Option<u32>,
}
#[derive(Debug, Clone)]
pub struct PresetFields {
    pub grid0: Vec<Vec<Model>>,
    pub graph12: Vec<GraphNode>,
    pub paired36: Vec<PairedRecord>,
    pub strings48: Vec<Utf16>,
    pub map5344: TraversalMap<WordMapValue>,
    pub model_map5368: TraversalMap<ModelPair>,
    pub opaque_float5396: F32,
    pub opaque_float5400: F32,
    pub default_scene5408: u32,
    pub assignments64: [Assignment; ASSIGNMENTS],
    pub assignment_groups544: [[Assignment; ASSIGNMENTS]; ASSIGNMENT_GROUPS],
    pub scene_names5412: Vec<Utf16>,
    pub group_labels5424: TraversalMap<Utf16>,
    pub individual_labels5448: TraversalMap<Utf16>,
    pub momentary5472: TraversalMap<u8>,
    pub colors5496: Vec<u32>,
    pub expression5508: Model,
    pub opaque_string5800: Utf16,
    pub opaque_string5820: Utf16,
    pub opaque_byte5825: u8,
    pub omitted: PresetOmissions,
}

/// Complete local compared-field capture, within the owned bounded domain.
/// This is independent of SceneStorage/WorkingParameters and serialized blobs.
#[derive(Debug, Clone)]
pub struct AdmittedSnapshot(PresetFields);
impl AdmittedSnapshot {
    pub fn new(fields: PresetFields) -> Result<Self, AdmissionError> {
        let p = Path::root("preset");
        let mut budget = Budget::default();
        check_limit(&p.field("grid0"), fields.grid0.len(), MAX_ROWS)?;
        for (i, row) in fields.grid0.iter().enumerate() {
            let q = p.field("grid0").index(i);
            check_limit(&q, row.len(), MAX_ROW_MODELS)?;
            for (j, model) in row.iter().enumerate() {
                model.validate(&mut budget, &q.index(j))?;
            }
        }
        budget.vector(fields.graph12.len(), &p.field("graph12"))?;
        budget.scalars(fields.graph12.len() * 3, &p.field("graph12"))?;
        check_limit(&p.field("paired36"), fields.paired36.len(), MAX_ITEMS)?;
        for (i, record) in fields.paired36.iter().enumerate() {
            let q = p.field("paired36").index(i);
            budget.scalars(2, &q)?;
            record.model8.validate(&mut budget, &q.field("model8"))?;
            record
                .model272
                .validate(&mut budget, &q.field("model272"))?;
        }
        for (name, texts) in [
            ("strings48", &fields.strings48),
            ("scene_names5412", &fields.scene_names5412),
        ] {
            let q = p.field(name);
            check_limit(&q, texts.len(), MAX_ITEMS)?;
            for (i, text) in texts.iter().enumerate() {
                budget.text(text, &q.index(i))?;
            }
        }
        budget.scalars(fields.map5344.entries().len() * 3, &p.field("map5344"))?;
        for (i, entry) in fields.model_map5368.entries().iter().enumerate() {
            let q = p.field("model_map5368").index(i);
            budget.scalars(1, &q)?;
            entry
                .value
                .model20
                .validate(&mut budget, &q.field("model20"))?;
            entry
                .value
                .model284
                .validate(&mut budget, &q.field("model284"))?;
        }
        for (name, map) in [
            ("group_labels5424", &fields.group_labels5424),
            ("individual_labels5448", &fields.individual_labels5448),
        ] {
            for (i, entry) in map.entries().iter().enumerate() {
                let q = p.field(name).index(i);
                budget.scalars(1, &q)?;
                budget.text(&entry.value, &q.field("value"))?;
            }
        }
        budget.scalars(
            fields.momentary5472.entries().len() * 2,
            &p.field("momentary5472"),
        )?;
        budget.vector(fields.colors5496.len(), &p.field("colors5496"))?;
        fields
            .expression5508
            .validate(&mut budget, &p.field("expression5508"))?;
        budget.text(&fields.opaque_string5800, &p.field("opaque_string5800"))?;
        budget.text(&fields.opaque_string5820, &p.field("opaque_string5820"))?;
        let omitted = &fields.omitted;
        for text in [
            &omitted.name_key5392,
            &omitted.string5792,
            &omitted.string5796,
            &omitted.string5804,
            &omitted.string5808,
            &omitted.string5812,
            &omitted.string5816,
        ] {
            budget.text(text, &p.field("omitted"))?;
        }
        budget.scalars(4 + ASSIGNMENTS * (ASSIGNMENT_GROUPS + 1) * 6 + 3, &p)?;
        if fields.default_scene5408 >= SCENES as u32 {
            return Err(AdmissionError {
                path: p.field("default_scene5408"),
                reason: AdmissionReason::InvalidDefaultScene(fields.default_scene5408),
            });
        }
        Ok(Self(fields))
    }
    pub fn fields(&self) -> &PresetFields {
        &self.0
    }
}
