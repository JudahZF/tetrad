use super::numeric::{
    plain_relative_f32, raw_exact_f32, raw_exact_f64, relative32, same_double_f64, same_float_f32,
};
use super::*;
use std::fmt;

/// An explicit caller assertion, never inferred from a live process. Host Rust
/// arithmetic uses nearest-even/gradual underflow. No target FPSCR claim follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonPolicy {
    DefaultQtHandlerOrdinaryIeee,
    /// Opt-in assertion for the privately reviewed finite type4 scalar domain.
    ///
    /// The caller asserts static default Qt handler selection, ordinary non-null
    /// numeric payloads, and the reviewed Qt toFloat / selected libgcc integer
    /// conversion / selected libm finite lroundf semantics. The caller also
    /// asserts nearest-even binary32 division and Double narrowing, gradual
    /// underflow (no FTZ), no trapping exceptions, and stable immutable inputs.
    /// This does not detect a runtime policy, establish loader/helper binding,
    /// emulate exception state, or claim Qt/live target parity.
    ///
    /// ```
    /// use tetrad_core::comparison::*;
    /// let policy = ComparisonPolicy::AssertedFiniteType4DefaultQtHelpersOrdinaryIeee;
    /// let lhs = Parameter::new(ParameterFields {
    ///     type8: 4, step_count84: 1,
    ///     step144: F32::from_bits(0x3f800001)?,
    ///     current16: Variant::Float32(F32::from_bits(0x3fc00001)?),
    ///     ..ParameterFields::default()
    /// })?;
    /// let rhs = Parameter::new(ParameterFields {
    ///     current16: Variant::Float32(F32::from_bits(0x40000001)?),
    ///     ..lhs.fields().clone()
    /// })?;
    /// assert_eq!(compare_parameter(&lhs, &rhs, policy)?, Comparison::Equal);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    AssertedFiniteType4DefaultQtHelpersOrdinaryIeee,
    CustomOrLiveHandler,
    UnresolvedNumericEnvironment,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    Mismatch { path: Path },
}
/// Reached operation context, independent of the captured Variant's type ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantContext {
    OpaqueVariant64,
    Current16,
    Default32,
    SceneBank,
    PublicVariant,
    ToRealConversion,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedReason {
    HandlerPolicy,
    NumericEnvironment,
    VariantType {
        type_id: u32,
    },
    /// rhs must convert to lhs; parser/formatter parity is not recovered.
    RhsToLhsConversion {
        lhs_type: u32,
        rhs_type: u32,
    },
    /// Constructor Invalid is admitted only at direct Parameter field64 cmp.
    ConstructorInvalidOutside64 {
        side: OperandSide,
        context: VariantContext,
    },
    Type4Stepping,
    /// Deliberate finite-domain refusal at a reached type4 operation.
    FiniteType4 {
        side: OperandSide,
        reason: Type4Refusal,
    },
    /// Finite Double64 outside the admitted f32 conversion range.
    CrossRealOverflow,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    pub path: Path,
    pub reason: UnsupportedReason,
}
impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.path, self.reason)
    }
}
impl std::error::Error for Unsupported {}

// Mismatch and unsupported remain distinct throughout nested traversal. Ignore
// can discard a proven descriptor mismatch, never an unsupported operation.
enum Failure {
    Mismatch(Path),
    Unsupported(Unsupported),
}
type Check = Result<(), Failure>;
fn finish(result: Check) -> Result<Comparison, Unsupported> {
    match result {
        Ok(()) => Ok(Comparison::Equal),
        Err(Failure::Mismatch(path)) => Ok(Comparison::Mismatch { path }),
        Err(Failure::Unsupported(error)) => Err(error),
    }
}
fn policy_check(policy: ComparisonPolicy) -> Result<(), Unsupported> {
    let reason = match policy {
        ComparisonPolicy::DefaultQtHandlerOrdinaryIeee
        | ComparisonPolicy::AssertedFiniteType4DefaultQtHelpersOrdinaryIeee => return Ok(()),
        ComparisonPolicy::CustomOrLiveHandler => UnsupportedReason::HandlerPolicy,
        ComparisonPolicy::UnresolvedNumericEnvironment => UnsupportedReason::NumericEnvironment,
    };
    Err(Unsupported {
        path: Path::root("policy"),
        reason,
    })
}
fn require(equal: bool, path: &Path) -> Check {
    if equal {
        Ok(())
    } else {
        Err(Failure::Mismatch(path.clone()))
    }
}
fn unsupported(path: &Path, reason: UnsupportedReason) -> Failure {
    Failure::Unsupported(Unsupported {
        path: path.clone(),
        reason,
    })
}
fn text(lhs: &Utf16, rhs: &Utf16, path: &Path) -> Check {
    // Deliberately ignores null storage flag; all UTF-16 units participate.
    require(
        lhs.units().len() == rhs.units().len(),
        &path.field("length"),
    )?;
    for (i, (a, b)) in lhs.units().iter().zip(rhs.units()).enumerate() {
        require(a == b, &path.index(i))?;
    }
    Ok(())
}
fn ordered<T: PartialEq>(lhs: &[T], rhs: &[T], path: &Path) -> Check {
    require(lhs.len() == rhs.len(), &path.field("length"))?;
    for (i, (a, b)) in lhs.iter().zip(rhs).enumerate() {
        require(a == b, &path.index(i))?;
    }
    Ok(())
}
fn strings(lhs: &[Utf16], rhs: &[Utf16], path: &Path) -> Check {
    require(lhs.len() == rhs.len(), &path.field("length"))?;
    for (i, (a, b)) in lhs.iter().zip(rhs).enumerate() {
        text(a, b, &path.index(i))?;
    }
    Ok(())
}

/// Default Qt cmp subset. Same Float/Double is raw VCMP. Cross Int/UInt uses
/// sign-/zero-extended i64. rhs-to-lhs conversion retains its original direction.
fn qt_variant(
    lhs: &Variant,
    rhs: &Variant,
    p: &Path,
    context: VariantContext,
) -> Result<bool, Failure> {
    if let Variant::UnsupportedTypeId(type_id) = lhs {
        return Err(unsupported(
            &p.field("lhs"),
            UnsupportedReason::VariantType { type_id: *type_id },
        ));
    }
    if let Variant::UnsupportedTypeId(type_id) = rhs {
        return Err(unsupported(
            &p.field("rhs"),
            UnsupportedReason::VariantType { type_id: *type_id },
        ));
    }
    // Both unknown markers refuse first, even if a ConstructorInvalid could
    // otherwise decide the result. Only this explicit field64 context admits it.
    for (side, name, value) in [
        (OperandSide::Lhs, "lhs", lhs),
        (OperandSide::Rhs, "rhs", rhs),
    ] {
        if matches!(value, Variant::ConstructorInvalid)
            && context != VariantContext::OpaqueVariant64
        {
            return Err(unsupported(
                &p.field(name),
                UnsupportedReason::ConstructorInvalidOutside64 { side, context },
            ));
        }
    }
    Ok(match (lhs, rhs) {
        (Variant::ConstructorInvalid, Variant::ConstructorInvalid) => true,
        (Variant::ConstructorInvalid, _) | (_, Variant::ConstructorInvalid) => false,
        (Variant::Int32(a), Variant::Int32(b)) => a == b,
        (Variant::UInt32(a), Variant::UInt32(b)) => a == b,
        (Variant::Int32(a), Variant::UInt32(b)) => i64::from(*a) == i64::from(*b),
        (Variant::UInt32(a), Variant::Int32(b)) => i64::from(*a) == i64::from(*b),
        (Variant::Float32(a), Variant::Float32(b)) => raw_exact_f32(*a, *b),
        (Variant::Double64(a), Variant::Double64(b)) => raw_exact_f64(*a, *b),
        (Variant::String(a), Variant::String(b)) => a.units() == b.units(),
        (Variant::String(_), _) | (_, Variant::String(_)) => {
            return Err(unsupported(
                p,
                UnsupportedReason::RhsToLhsConversion {
                    lhs_type: lhs.type_id(),
                    rhs_type: rhs.type_id(),
                },
            ));
        }
        _ => relative32(
            to_real(lhs, &p.field("lhs"), OperandSide::Lhs)?,
            to_real(rhs, &p.field("rhs"), OperandSide::Rhs)?,
        ),
    })
}

// Reviewed default handler: Int uses signed helper + __aeabi_l2f;
// UInt uses unsigned helper + __aeabi_ul2f; Double uses vcvt.f32.f64.
// The toReal result is f32 in this Qt build, even for Double64 input.
fn to_real(value: &Variant, p: &Path, side: OperandSide) -> Result<f32, Failure> {
    Ok(match value {
        Variant::Int32(value) => i64::from(*value) as f32,
        Variant::UInt32(value) => u64::from(*value) as f32,
        Variant::Float32(value) => value.value(),
        Variant::Double64(value) => {
            let value = value.value();
            if value.is_finite() && value.abs() > f64::from(f32::MAX) {
                return Err(unsupported(p, UnsupportedReason::CrossRealOverflow));
            }
            value as f32
        }
        Variant::ConstructorInvalid => {
            return Err(unsupported(
                p,
                UnsupportedReason::ConstructorInvalidOutside64 {
                    side,
                    context: VariantContext::ToRealConversion,
                },
            ));
        }
        _ => unreachable!("only admitted numeric alternatives reach toReal"),
    })
}

/// Supplied/current values use lhs descriptor type8. Default32 does not step.
fn supplied(
    descriptor: &ParameterFields,
    policy: ComparisonPolicy,
    lhs: &Variant,
    rhs: &Variant,
    p: &Path,
    context: VariantContext,
) -> Result<bool, Failure> {
    if descriptor.type8 == 4 {
        if policy != ComparisonPolicy::AssertedFiniteType4DefaultQtHelpersOrdinaryIeee {
            return Err(unsupported(p, UnsupportedReason::Type4Stepping));
        }
        let lhs = super::finite_type4::index(lhs, descriptor).map_err(|reason| {
            unsupported(
                &p.field("lhs"),
                UnsupportedReason::FiniteType4 {
                    side: OperandSide::Lhs,
                    reason,
                },
            )
        })?;
        let rhs = super::finite_type4::index(rhs, descriptor).map_err(|reason| {
            unsupported(
                &p.field("rhs"),
                UnsupportedReason::FiniteType4 {
                    side: OperandSide::Rhs,
                    reason,
                },
            )
        })?;
        return Ok(lhs == rhs);
    }
    regular_parameter_value(lhs, rhs, p, context)
}
fn regular_parameter_value(
    lhs: &Variant,
    rhs: &Variant,
    p: &Path,
    context: VariantContext,
) -> Result<bool, Failure> {
    match (lhs, rhs) {
        (Variant::Float32(a), Variant::Float32(b)) => Ok(same_float_f32(*a, *b)),
        (Variant::Double64(a), Variant::Double64(b)) => Ok(same_double_f64(*a, *b)),
        _ => qt_variant(lhs, rhs, p, context),
    }
}

macro_rules! exact_fields {
    ($lhs:ident, $rhs:ident, $p:ident; $($field:ident),+ $(,)?) => {
        $(require($lhs.$field == $rhs.$field, &$p.field(stringify!($field)))?;)+
    };
}
macro_rules! relative_fields {
    ($lhs:ident, $rhs:ident, $p:ident; $($field:ident),+ $(,)?) => {
        $(require(plain_relative_f32($lhs.$field, $rhs.$field), &$p.field(stringify!($field)))?;)+
    };
}
macro_rules! text_fields {
    ($lhs:ident, $rhs:ident, $p:ident; $($field:ident),+ $(,)?) => {
        $(text(&$lhs.$field, &$rhs.$field, &$p.field(stringify!($field)))?;)+
    };
}

fn parameter(lhs: &Parameter, rhs: &Parameter, p: &Path, policy: ComparisonPolicy) -> Check {
    let lhs = lhs.fields();
    let rhs = rhs.fields();
    // Prepared before word0; a false value is only consumed after type8. An
    // unsupported preparation is a refusal even if word0 would later differ.
    let current = if lhs.suppress_current156 != 0 {
        true
    } else {
        supplied(
            lhs,
            policy,
            &lhs.current16,
            &rhs.current16,
            &p.field("current16"),
            VariantContext::Current16,
        )?
    };
    exact_fields!(lhs, rhs, p; opaque_word0);
    ordered(
        &lhs.opaque_bytes4,
        &rhs.opaque_bytes4,
        &p.field("opaque_bytes4"),
    )?;
    exact_fields!(lhs, rhs, p; type8);
    require(current, &p.field("current16"))?;
    require(
        regular_parameter_value(
            &lhs.default32,
            &rhs.default32,
            &p.field("default32"),
            VariantContext::Default32,
        )?,
        &p.field("default32"),
    )?;
    exact_fields!(lhs, rhs, p; step_count84);
    ordered(
        &lhs.opaque_bytes92,
        &rhs.opaque_bytes92,
        &p.field("opaque_bytes92"),
    )?;
    relative_fields!(lhs, rhs, p; opaque_float96, opaque_float100, step144);
    exact_fields!(lhs, rhs, p; opaque_byte148, opaque_word140, suppress_current156, opaque_byte168);
    relative_fields!(lhs, rhs, p; expression_float160, expression_float164, opaque_float152);
    exact_fields!(lhs, rhs, p; opaque_byte169, opaque_word176, opaque_word180, opaque_word184);
    text_fields!(lhs, rhs, p; opaque_string188, opaque_string192, opaque_string196, opaque_string200);
    exact_fields!(lhs, rhs, p; scene_mode204);
    ordered(
        &lhs.opaque_vector208,
        &rhs.opaque_vector208,
        &p.field("opaque_vector208"),
    )?;
    ordered(
        &lhs.opaque_vector220,
        &rhs.opaque_vector220,
        &p.field("opaque_vector220"),
    )?;
    exact_fields!(lhs, rhs, p; opaque_word256);
    ordered(
        &lhs.opaque_vector232,
        &rhs.opaque_vector232,
        &p.field("opaque_vector232"),
    )?;
    ordered(
        &lhs.linked_scene_mode244,
        &rhs.linked_scene_mode244,
        &p.field("linked_scene_mode244"),
    )?;
    text_fields!(lhs, rhs, p; opaque_string88);
    require(
        qt_variant(
            &lhs.opaque_variant64,
            &rhs.opaque_variant64,
            &p.field("opaque_variant64"),
            VariantContext::OpaqueVariant64,
        )?,
        &p.field("opaque_variant64"),
    )?;
    exact_fields!(lhs, rhs, p; opaque_byte80);
    // One masked packed-word comparison; do not include bytes260/263.
    require(
        lhs.packed260[1..3] == rhs.packed260[1..3],
        &p.field("packed261_262"),
    )?;
    require(
        lhs.points264.len() == rhs.points264.len(),
        &p.field("points264").field("length"),
    )?;
    for (i, (a, b)) in lhs.points264.iter().zip(&rhs.points264).enumerate() {
        let q = p.field("points264").index(i);
        require(raw_exact_f32(a.x, b.x), &q.field("x"))?;
        require(raw_exact_f32(a.y, b.y), &q.field("y"))?;
    }
    Ok(())
}
fn model_record(lhs: &ModelRecord, rhs: &ModelRecord, p: &Path) -> Check {
    exact_fields!(lhs, rhs, p; opaque_word0, opaque_half4, opaque_word8);
    Ok(())
}
fn model(lhs: &Model, rhs: &Model, p: &Path, policy: ComparisonPolicy) -> Check {
    let lhs = lhs.fields();
    let rhs = rhs.fields();
    for scene in 0..SCENES {
        let q = p.field("second52").index(scene);
        let a = &lhs.second52[scene];
        let b = &rhs.second52[scene];
        require(a.len() == b.len(), &q.field("length"))?;
        for (i, (a, b)) in a.iter().zip(b).enumerate() {
            // Constructors guarantee a positional lhs descriptor exists.
            require(
                supplied(
                    lhs.descriptors28[i].fields(),
                    policy,
                    a,
                    b,
                    &q.index(i),
                    VariantContext::SceneBank,
                )?,
                &q.index(i),
            )?;
        }
        require(
            raw_exact_f32(lhs.second_cache96[scene], rhs.second_cache96[scene]),
            &p.field("second_cache96").index(scene),
        )?;
    }
    let q = p.field("descriptors28");
    require(
        lhs.descriptors28.len() == rhs.descriptors28.len(),
        &q.field("length"),
    )?;
    for (i, (a, b)) in lhs.descriptors28.iter().zip(&rhs.descriptors28).enumerate() {
        match parameter(a, b, &q.index(i), policy) {
            Err(Failure::Mismatch(_)) if a.fields().packed260[1] != 0 => {}
            result => result?,
        }
    }
    exact_fields!(lhs, rhs, p; opaque_word24, opaque_word0);
    ordered(
        &lhs.opaque_vector4,
        &rhs.opaque_vector4,
        &p.field("opaque_vector4"),
    )?;
    exact_fields!(lhs, rhs, p; opaque_word16);
    text_fields!(lhs, rhs, p; opaque_string20);
    model_record(&lhs.record188, &rhs.record188, &p.field("record188"))?;
    model_record(&lhs.record200, &rhs.record200, &p.field("record200"))?;
    model_record(&lhs.record212, &rhs.record212, &p.field("record212"))?;
    exact_fields!(lhs, rhs, p; opaque_byte228, opaque_word232, opaque_word240, opaque_word236);
    Ok(())
}
fn expression_precheck(lhs: &Model, rhs: &Model, p: &Path) -> Check {
    for (i, a) in lhs.fields().descriptors28.iter().enumerate() {
        if let Some(b) = rhs.fields().descriptors28.get(i) {
            // The stock helper call passes (rhs,lhs) before full Model comparison.
            let lhs = b.fields();
            let rhs = a.fields();
            let q = p.field("expression_precheck").index(i);
            require(
                lhs.suppress_current156 == rhs.suppress_current156,
                &q.field("suppress_current156"),
            )?;
            require(
                plain_relative_f32(lhs.expression_float160, rhs.expression_float160),
                &q.field("expression_float160"),
            )?;
            require(
                plain_relative_f32(lhs.expression_float164, rhs.expression_float164),
                &q.field("expression_float164"),
            )?;
        }
    }
    Ok(())
}
fn assignment(lhs: &Assignment, rhs: &Assignment, p: &Path) -> Check {
    exact_fields!(lhs, rhs, p; opaque_byte0, opaque_word4, opaque_word8, opaque_word12, opaque_word16);
    Ok(())
}
fn label_map(lhs: &TraversalMap<Utf16>, rhs: &TraversalMap<Utf16>, p: &Path) -> Check {
    require(
        lhs.entries().len() == rhs.entries().len(),
        &p.field("length"),
    )?;
    for (i, (a, b)) in lhs.entries().iter().zip(rhs.entries()).enumerate() {
        let q = p.index(i);
        require(a.key16 == b.key16, &q.field("key16"))?;
        text(&a.value, &b.value, &q.field("value"))?;
    }
    Ok(())
}
fn preset(
    lhs: &AdmittedSnapshot,
    rhs: &AdmittedSnapshot,
    p: &Path,
    policy: ComparisonPolicy,
) -> Check {
    let lhs = lhs.fields();
    let rhs = rhs.fields();
    require(
        lhs.grid0.len() == rhs.grid0.len(),
        &p.field("grid0").field("length"),
    )?;
    for (i, (a, b)) in lhs.grid0.iter().zip(&rhs.grid0).enumerate() {
        let q = p.field("grid0").index(i);
        require(a.len() == b.len(), &q.field("length"))?;
        for (j, (a, b)) in a.iter().zip(b).enumerate() {
            model(a, b, &q.index(j), policy)?;
        }
    }
    let q = p.field("graph12");
    require(lhs.graph12.len() == rhs.graph12.len(), &q.field("length"))?;
    for (i, (lhs, rhs)) in lhs.graph12.iter().zip(&rhs.graph12).enumerate() {
        let p = q.index(i);
        exact_fields!(lhs, rhs, p; opaque_word24, opaque_word20, opaque_word16, opaque_word28);
    }
    let q = p.field("paired36");
    require(lhs.paired36.len() == rhs.paired36.len(), &q.field("length"))?;
    for (i, (lhs, rhs)) in lhs.paired36.iter().zip(&rhs.paired36).enumerate() {
        let p = q.index(i);
        exact_fields!(lhs, rhs, p; opaque_word0, opaque_word4);
        model(&lhs.model8, &rhs.model8, &p.field("model8"), policy)?;
        model(&lhs.model272, &rhs.model272, &p.field("model272"), policy)?;
    }
    strings(&lhs.strings48, &rhs.strings48, &p.field("strings48"))?;
    let q = p.field("map5344");
    require(
        lhs.map5344.entries().len() == rhs.map5344.entries().len(),
        &q.field("length"),
    )?;
    for (i, (a, b)) in lhs
        .map5344
        .entries()
        .iter()
        .zip(rhs.map5344.entries())
        .enumerate()
    {
        let q = q.index(i);
        require(a.key16 == b.key16, &q.field("key16"))?;
        require(
            a.value.opaque_word20 == b.value.opaque_word20,
            &q.field("opaque_word20"),
        )?;
        require(
            a.value.opaque_word24 == b.value.opaque_word24,
            &q.field("opaque_word24"),
        )?;
    }
    let q = p.field("model_map5368");
    require(
        lhs.model_map5368.entries().len() == rhs.model_map5368.entries().len(),
        &q.field("length"),
    )?;
    for (i, (a, b)) in lhs
        .model_map5368
        .entries()
        .iter()
        .zip(rhs.model_map5368.entries())
        .enumerate()
    {
        let q = q.index(i);
        require(a.key16 == b.key16, &q.field("key16"))?;
        model(
            &a.value.model20,
            &b.value.model20,
            &q.field("model20"),
            policy,
        )?;
        model(
            &a.value.model284,
            &b.value.model284,
            &q.field("model284"),
            policy,
        )?;
    }
    relative_fields!(lhs, rhs, p; opaque_float5396, opaque_float5400);
    exact_fields!(lhs, rhs, p; default_scene5408);
    for (i, (a, b)) in lhs.assignments64.iter().zip(&rhs.assignments64).enumerate() {
        assignment(a, b, &p.field("assignments64").index(i))?;
    }
    for (i, (a, b)) in lhs
        .assignment_groups544
        .iter()
        .zip(&rhs.assignment_groups544)
        .enumerate()
    {
        for (j, (a, b)) in a.iter().zip(b).enumerate() {
            assignment(a, b, &p.field("assignment_groups544").index(i).index(j))?;
        }
    }
    strings(
        &lhs.scene_names5412,
        &rhs.scene_names5412,
        &p.field("scene_names5412"),
    )?;
    label_map(
        &lhs.group_labels5424,
        &rhs.group_labels5424,
        &p.field("group_labels5424"),
    )?;
    label_map(
        &lhs.individual_labels5448,
        &rhs.individual_labels5448,
        &p.field("individual_labels5448"),
    )?;
    let q = p.field("momentary5472");
    require(
        lhs.momentary5472.entries().len() == rhs.momentary5472.entries().len(),
        &q.field("length"),
    )?;
    for (i, (a, b)) in lhs
        .momentary5472
        .entries()
        .iter()
        .zip(rhs.momentary5472.entries())
        .enumerate()
    {
        let q = q.index(i);
        require(a.key16 == b.key16, &q.field("key16"))?;
        require(a.value == b.value, &q.field("value"))?;
    }
    ordered(&lhs.colors5496, &rhs.colors5496, &p.field("colors5496"))?;
    let q = p.field("expression5508");
    expression_precheck(&lhs.expression5508, &rhs.expression5508, &q)?;
    model(&lhs.expression5508, &rhs.expression5508, &q, policy)?;
    text_fields!(lhs, rhs, p; opaque_string5800, opaque_string5820);
    exact_fields!(lhs, rhs, p; opaque_byte5825);
    Ok(())
}

/// Compare baseline lhs with effective rhs, without mutation or lifecycle effects.
pub fn compare(
    lhs: &AdmittedSnapshot,
    rhs: &AdmittedSnapshot,
    policy: ComparisonPolicy,
) -> Result<Comparison, Unsupported> {
    policy_check(policy)?;
    finish(preset(lhs, rhs, &Path::root("preset"), policy))
}
/// Standalone full Model traversal; this does not apply the expression precheck.
pub fn compare_model(
    lhs: &Model,
    rhs: &Model,
    policy: ComparisonPolicy,
) -> Result<Comparison, Unsupported> {
    policy_check(policy)?;
    finish(model(lhs, rhs, &Path::root("model"), policy))
}
pub fn compare_parameter(
    lhs: &Parameter,
    rhs: &Parameter,
    policy: ComparisonPolicy,
) -> Result<Comparison, Unsupported> {
    policy_check(policy)?;
    finish(parameter(lhs, rhs, &Path::root("parameter"), policy))
}
/// Direct Qt comparison, distinct from supplied/default parameter float rules.
/// ConstructorInvalid refuses here: its accepted direct domain is only field64.
pub fn compare_variant(
    lhs: &Variant,
    rhs: &Variant,
    policy: ComparisonPolicy,
) -> Result<Comparison, Unsupported> {
    policy_check(policy)?;
    let p = Path::root("variant");
    finish(
        qt_variant(lhs, rhs, &p, VariantContext::PublicVariant)
            .and_then(|equal| require(equal, &p)),
    )
}
