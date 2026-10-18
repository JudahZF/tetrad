use std::collections::BTreeMap;
use std::str::FromStr;

use qc_format::metadata::{Element, attr, elements};

use crate::{
    Attributes, Catalog, CatalogError, Category, Hidden, Kernel, Model, Module, Parameter,
    ParameterReference, ParameterType, ParameterValue, ResourceTest, Resources, Result,
};

pub(crate) const MAX_XML_BYTES: usize = 16 * 1024 * 1024;
const MAX_MARKUP: usize = 100_000;
const MAX_ATTRIBUTES: usize = 128;
const MAX_LIST_ITEMS: usize = 4096;

pub(crate) fn parse(model_repo: &str, module_report: &str) -> Result<Catalog> {
    let model_root = check_document(model_repo, "Models")?;
    let report_root = check_document(module_report, "ModuleReport")?;
    let category_elements = bounded_elements(model_repo, "Category", 1024)?;
    let model_elements = bounded_elements(model_repo, "Model", 16_384)?;
    let parameter_elements = bounded_elements(model_repo, "Parameter", 65_536)?;
    let padding_elements = bounded_elements(model_repo, "Padding", 16_384)?;
    let module_elements = bounded_elements(module_report, "Module", 16_384)?;
    let kernel_elements = bounded_elements(module_report, "Kernel", 1024)?;
    let test_elements = bounded_elements(module_report, "Test", 65_536)?;
    // Known siblings cannot be nested inside one another. Parent ownership is
    // checked below, including entries outside any expected parent.
    disjoint(&category_elements)?;
    disjoint(&model_elements)?;
    disjoint(&parameter_elements)?;
    disjoint(&padding_elements)?;
    let mut report_siblings: Vec<_> = module_elements.iter().chain(&kernel_elements).collect();
    report_siblings.sort_by_key(|e| e.range.start);
    disjoint_refs(&report_siblings)?;
    disjoint(&test_elements)?;
    direct_children(&model_root, &category_elements.iter().collect::<Vec<_>>())?;
    direct_children(&report_root, &report_siblings)?;
    for category in &category_elements {
        let children = model_elements
            .iter()
            .filter(|m| category.range.start < m.range.start && m.range.end < category.range.end)
            .collect::<Vec<_>>();
        direct_children(category, &children)?;
    }

    let mut catalog = Catalog {
        categories: BTreeMap::new(),
        models: BTreeMap::new(),
        modules: BTreeMap::new(),
        kernels: BTreeMap::new(),
        category_order: Vec::new(),
        model_order: Vec::new(),
        module_order: Vec::new(),
        kernel_order: Vec::new(),
        model_repo_xml: model_repo.to_owned(),
        module_report_xml: module_report.to_owned(),
    };
    for element in &category_elements {
        let a = attributes(element.text)?;
        let id = required_number(&a, "id")?;
        insert_unique(
            &mut catalog.categories,
            id,
            Category {
                id,
                name: required(&a, "name")?,
                hidden: hidden(&a),
                model_ids: Vec::new(),
                attributes: a,
                raw_xml: element.text.to_owned(),
            },
            "category ID",
        )?;
        catalog.category_order.push(id);
    }
    let mut parameters_seen = 0;
    let mut padding_seen = 0;
    for element in &model_elements {
        let category_position = owner(
            element,
            &category_elements,
            "Model requires a Category parent",
        )?;
        let category_id = catalog.category_order[category_position];
        let a = attributes(element.text)?;
        let id = required_number(&a, "id")?;
        let parameter_nodes = bounded_elements(element.text, "Parameter", 4096)?;
        let padding_nodes = bounded_elements(element.text, "Padding", 1)?;
        let mut siblings: Vec<_> = parameter_nodes.iter().chain(&padding_nodes).collect();
        siblings.sort_by_key(|e| e.range.start);
        disjoint_refs(&siblings)?;
        direct_children(
            &Element {
                range: 0..element.text.len(),
                text: element.text,
            },
            &siblings,
        )?;
        let parameters = parameter_nodes
            .iter()
            .enumerate()
            .map(|(position, e)| parameter(e.text, position))
            .collect::<Result<Vec<_>>>()?;
        parameters_seen += parameters.len();
        padding_seen += padding_nodes.len();
        let padding = padding_nodes
            .first()
            .map(|p| resources(p.text))
            .transpose()?;
        insert_unique(
            &mut catalog.models,
            id,
            Model {
                id,
                category_id,
                name: required(&a, "name")?,
                blob: a.get("blob").cloned(),
                plugin_id: number(&a, "plugin_id")?,
                sku: a.get("sku").cloned(),
                style: a.get("style").cloned(),
                tm: a.get("tm").cloned(),
                hidden: hidden(&a),
                internal: boolean(&a, "internal")?,
                skip_self_test: boolean(&a, "skip_self_test")?,
                reuse_pmco: boolean(&a, "reuse_pmco")?,
                replaces: integer_list(&a, "replaces")?,
                clones: integer_list(&a, "clones")?,
                sidechain: number(&a, "sidechain")?,
                sidechain_in: number(&a, "sidechain_in")?,
                secondary_stomp_parameter: number(&a, "secondary_stomp_parameter")?,
                padding,
                parameters,
                attributes: a,
                raw_xml: element.text.to_owned(),
            },
            "model ID",
        )?;
        catalog.model_order.push(id);
        catalog
            .categories
            .get_mut(&category_id)
            .expect("inserted category")
            .model_ids
            .push(id);
    }
    if parameters_seen != parameter_elements.len() || padding_seen != padding_elements.len() {
        return Err(schema("Parameter and Padding require Model parents"));
    }
    let mut tests_seen = 0;
    for element in &module_elements {
        let a = attributes(element.text)?;
        let id = required_number(&a, "id")?;
        let test_nodes = bounded_elements(element.text, "Test", 4096)?;
        direct_children(
            &Element {
                range: 0..element.text.len(),
                text: element.text,
            },
            &test_nodes.iter().collect::<Vec<_>>(),
        )?;
        let tests = test_nodes
            .iter()
            .map(|e| resource_test(e.text))
            .collect::<Result<Vec<_>>>()?;
        tests_seen += tests.len();
        insert_unique(
            &mut catalog.modules,
            id,
            Module {
                id,
                category_id: number(&a, "category")?,
                name: required(&a, "name")?,
                blob_name: a.get("blob_name").cloned(),
                resources: resources(element.text)?,
                tests,
                attributes: a,
                raw_xml: element.text.to_owned(),
            },
            "module ID",
        )?;
        catalog.module_order.push(id);
    }
    if tests_seen != test_elements.len() {
        return Err(schema("Test requires a Module parent"));
    }
    for element in &kernel_elements {
        let a = attributes(element.text)?;
        let core = required_number(&a, "core")?;
        insert_unique(
            &mut catalog.kernels,
            core,
            Kernel {
                core,
                resources: resources(element.text)?,
                attributes: a,
                raw_xml: element.text.to_owned(),
            },
            "kernel core",
        )?;
        catalog.kernel_order.push(core);
    }
    Ok(catalog)
}

fn parameter(text: &str, position: usize) -> Result<Parameter> {
    let a = attributes(text)?;
    let parameter_type = match required(&a, "type")?.as_str() {
        "float" => ParameterType::Float,
        "floatWithLed" => ParameterType::FloatWithLed,
        "int" => ParameterType::Int,
        "string" => ParameterType::String,
        "switch" => ParameterType::Switch,
        "rotarySwitch" => ParameterType::RotarySwitch,
        "comboBox" => ParameterType::ComboBox,
        "toggleButton" => ParameterType::ToggleButton,
        "fader" => ParameterType::Fader,
        "meter" => ParameterType::Meter,
        "stereoMeter" => ParameterType::StereoMeter,
        "grMeter" => ParameterType::GrMeter,
        "stereoGrMeter" => ParameterType::StereoGrMeter,
        "empty" => ParameterType::Empty,
        unknown => ParameterType::Unknown(unknown.to_owned()),
    };
    let default_value = a
        .get("defaultValue")
        .map(|v| {
            if matches!(
                parameter_type,
                ParameterType::String | ParameterType::Unknown(_)
            ) {
                Ok(ParameterValue::Text(v.clone()))
            } else {
                symbolic_number(v, "defaultValue")
            }
        })
        .transpose()?;
    Ok(Parameter {
        position,
        index: number(&a, "index")?,
        name: required(&a, "name")?,
        parameter_type,
        default_value,
        min: a
            .get("min")
            .map(|v| symbolic_number(v, "min"))
            .transpose()?,
        max: a
            .get("max")
            .map(|v| symbolic_number(v, "max"))
            .transpose()?,
        skew: a
            .get("skew")
            .map(|v| symbolic_number(v, "skew"))
            .transpose()?,
        steps: number(&a, "steps")?,
        step_names: a
            .get("stepNames")
            .map(|v| bounded_list(v).map(|items| items.map(str::to_owned).collect()))
            .transpose()?,
        x_values: float_list(&a, "xValues")?,
        y_values: float_list(&a, "yValues")?,
        hidden: hidden(&a),
        display_pos: number(&a, "displayPos")?,
        replaces: parameter_references(&a, "replaces")?,
        linked_scene_mode: integer_list(&a, "linkedSceneMode")?,
        toggle_on: integer_list(&a, "toggleOn")?,
        toggle_off: integer_list(&a, "toggleOff")?,
        toggle_step: integer_list(&a, "toggleStep")?,
        dynamic: boolean(&a, "dynamic")?,
        exp_assignable: boolean(&a, "expAssignable")?,
        show_as_integer: boolean(&a, "showAsInteger")?,
        units: a.get("units").cloned(),
        self_test_value: a.get("selfTestValue").cloned(),
        attributes: a,
        raw_xml: text.to_owned(),
    })
}

fn resources(text: &str) -> Result<Resources> {
    let a = attributes(text)?;
    Ok(Resources {
        dm_heap: number(&a, "dm_heap")?,
        pm_heap: number(&a, "pm_heap")?,
        sd_heap: number(&a, "sd_heap")?,
        dm: number(&a, "dm")?,
        pm: number(&a, "pm")?,
        sw: number(&a, "sw")?,
        nw: number(&a, "nw")?,
        dma: floating(&a, "dma")?,
        cpu: floating(&a, "cpu")?,
        cpu_avg: floating(&a, "cpu_avg")?,
        cpu_pad: floating(&a, "cpu_pad")?,
        isr: floating(&a, "isr")?,
        latency: number(&a, "Latency")?,
        dm_heap_pad: number(&a, "dm_heap_pad")?,
        pm_heap_pad: number(&a, "pm_heap_pad")?,
        sd_heap_pad: number(&a, "sd_heap_pad")?,
        dm_pad: number(&a, "dm_pad")?,
        pm_pad: number(&a, "pm_pad")?,
        sw_pad: number(&a, "sw_pad")?,
        nw_pad: number(&a, "nw_pad")?,
        attributes: a,
        raw_xml: text.to_owned(),
    })
}

fn resource_test(text: &str) -> Result<ResourceTest> {
    let a = attributes(text)?;
    Ok(ResourceTest {
        name: required(&a, "name")?,
        min: floating(&a, "min")?,
        max: floating(&a, "max")?,
        avg: floating(&a, "avg")?,
        pk_l: floating(&a, "pk_l")?,
        pk_r: floating(&a, "pk_r")?,
        attributes: a,
        raw_xml: text.to_owned(),
    })
}

fn check_document<'a>(doc: &'a str, root: &str) -> Result<Element<'a>> {
    if doc.len() > MAX_XML_BYTES {
        return Err(CatalogError::Limit("XML bytes"));
    }
    // qc-format uses an iterative tokenizer. A markup cap bounds its tokens
    // and nesting storage before allocation, including opaque elements.
    if doc
        .bytes()
        .filter(|&b| b == b'<')
        .take(MAX_MARKUP + 1)
        .count()
        > MAX_MARKUP
    {
        return Err(CatalogError::Limit("XML markup count"));
    }
    check_attribute_limit(doc)?;
    let roots = elements(doc, root)?;
    if roots.len() != 1 {
        return Err(schema("unexpected XML root"));
    }
    // A matching descendant is not a matching document root. Asking qc-format
    // to validate the prefix plus a self-closing sentinel rejects open parents.
    let prefix = &doc[..roots[0].range.start];
    elements(
        &format!("{prefix}<TetradCatalogRootCheck/>"),
        "TetradCatalogRootCheck",
    )?;
    Ok(roots.into_iter().next().expect("one root checked"))
}

/// Bound qc-format's duplicate-attribute scan before any tokenization, including
/// opaque tags. This linear preflight counts assignments, not XML syntax;
/// qc-format still checks names, values, entities, and document structure.
fn check_attribute_limit(doc: &str) -> Result<()> {
    let mut at = 0;
    while let Some(offset) = doc[at..].find('<') {
        let lt = at + offset;
        let rest = &doc[lt..];
        // Match qc-format's terminators exactly. End tags and declarations end
        // at the first '>', even if malformed text contains quote characters.
        let terminator = if rest.starts_with("<!--") {
            Some("-->")
        } else if rest.starts_with("<![CDATA[") {
            Some("]]>")
        } else if rest.starts_with("<?") {
            Some("?>")
        } else if rest.starts_with("</") || rest.starts_with("<!") {
            Some(">")
        } else {
            None
        };
        if let Some(terminator) = terminator {
            let Some(end) = rest.find(terminator) else {
                // The tokenizer reports unterminated markup.
                return Ok(());
            };
            at = lt + end + terminator.len();
            continue;
        }

        at = lt + 1;
        let mut quote = None;
        let mut assignments = 0;
        while let Some(&byte) = doc.as_bytes().get(at) {
            at += 1;
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
            } else {
                match byte {
                    b'\'' | b'"' => quote = Some(byte),
                    b'>' => break,
                    b'=' => {
                        assignments += 1;
                        if assignments > MAX_ATTRIBUTES {
                            return Err(CatalogError::Limit("XML attribute count"));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn bounded_elements<'a>(doc: &'a str, tag: &str, max: usize) -> Result<Vec<Element<'a>>> {
    let found = elements(doc, tag)?;
    if found.len() > max {
        return Err(CatalogError::Limit("XML entry count"));
    }
    Ok(found)
}

fn disjoint(nodes: &[Element<'_>]) -> Result<()> {
    if nodes.windows(2).any(|w| w[0].range.end > w[1].range.start) {
        return Err(schema("known sibling elements must not be nested"));
    }
    Ok(())
}

fn disjoint_refs(nodes: &[&Element<'_>]) -> Result<()> {
    if nodes.windows(2).any(|w| w[0].range.end > w[1].range.start) {
        return Err(schema("known sibling elements must not be nested"));
    }
    Ok(())
}

/// Validate the gaps between known children through qc-format. An unclosed
/// opaque wrapper in a gap reveals a misplaced known descendant. Each byte
/// is checked once at this ownership level; no second XML tree parser is used.
fn direct_children(parent: &Element<'_>, children: &[&Element<'_>]) -> Result<()> {
    let mut quote = None;
    let opening_end = parent
        .text
        .bytes()
        .enumerate()
        .find_map(|(index, byte)| {
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
            } else if matches!(byte, b'\'' | b'"') {
                quote = Some(byte);
            } else if byte == b'>' {
                return Some(index + 1);
            }
            None
        })
        .ok_or_else(|| schema("start tag has no end"))?;
    let inner_end = if opening_end == parent.text.len() {
        opening_end
    } else {
        parent
            .text
            .rfind("</")
            .ok_or_else(|| schema("missing parent end tag"))?
    };
    let mut at = opening_end;
    for child in children {
        let start = child
            .range
            .start
            .checked_sub(parent.range.start)
            .ok_or_else(|| schema("child outside parent"))?;
        let end = child
            .range
            .end
            .checked_sub(parent.range.start)
            .ok_or_else(|| schema("child outside parent"))?;
        if start < at || end > inner_end {
            return Err(schema("child outside parent content"));
        }
        validate_gap(&parent.text[at..start])?;
        at = end;
    }
    validate_gap(&parent.text[at..inner_end])
}

fn validate_gap(gap: &str) -> Result<()> {
    elements(
        &format!("<TetradCatalogChildren>{gap}</TetradCatalogChildren>"),
        "TetradCatalogChildren",
    )?;
    Ok(())
}

fn owner(child: &Element<'_>, parents: &[Element<'_>], message: &str) -> Result<usize> {
    parents
        .iter()
        .position(|p| p.range.start < child.range.start && child.range.end < p.range.end)
        .ok_or_else(|| schema(message))
}

fn insert_unique<T>(map: &mut BTreeMap<u32, T>, id: u32, entry: T, kind: &str) -> Result<()> {
    if map.insert(id, entry).is_some() {
        return Err(schema(&format!("duplicate {kind}: {id}")));
    }
    Ok(())
}

/// qc-format does not expose attribute names. This small scanner enumerates
/// names on an already validated start tag only; qc-format still validates
/// syntax, rejects duplicates, and decodes every value through `attr`.
fn attributes(text: &str) -> Result<Attributes> {
    let bytes = text.as_bytes();
    let mut at = bytes
        .iter()
        .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
        .ok_or_else(|| schema("start tag missing"))?;
    let mut out = Attributes::new();
    loop {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        if matches!(bytes.get(at), Some(b'/' | b'>')) {
            break;
        }
        if out.len() >= MAX_ATTRIBUTES {
            return Err(CatalogError::Limit("attributes per element"));
        }
        let eq = text
            .get(at..)
            .and_then(|s| s.find('='))
            .map(|n| at + n)
            .ok_or_else(|| schema("attribute missing value"))?;
        let name = text[at..eq].trim_end();
        let value = attr(text, name)?.ok_or_else(|| schema("attribute lookup failed"))?;
        out.insert(name.to_owned(), value);
        at = eq + 1;
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        let quote = *bytes
            .get(at)
            .ok_or_else(|| schema("attribute missing quote"))?;
        at += 1;
        let close = bytes
            .get(at..)
            .and_then(|s| s.iter().position(|&b| b == quote))
            .ok_or_else(|| schema("attribute missing closing quote"))?;
        at += close + 1;
    }
    Ok(out)
}

fn required(a: &Attributes, key: &str) -> Result<String> {
    a.get(key)
        .cloned()
        .ok_or_else(|| schema(&format!("missing {key} attribute")))
}

fn required_number<T: FromStr>(a: &Attributes, key: &str) -> Result<T> {
    number(a, key)?.ok_or_else(|| schema(&format!("missing or empty {key} attribute")))
}

fn number<T: FromStr>(a: &Attributes, key: &str) -> Result<Option<T>> {
    a.get(key)
        .filter(|v| !v.trim().is_empty())
        .map(|v| {
            let v = v.trim();
            if !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(schema(&format!("invalid unsigned integer {key}")));
            }
            v.parse()
                .map_err(|_| schema(&format!("unsigned integer overflow in {key}")))
        })
        .transpose()
}

fn finite(value: &str, key: &str) -> Result<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| schema(&format!("invalid finite number {key}")))
}

fn floating(a: &Attributes, key: &str) -> Result<Option<f64>> {
    a.get(key)
        .filter(|v| !v.trim().is_empty())
        .map(|v| finite(v, key))
        .transpose()
}

fn symbolic_number(value: &str, key: &str) -> Result<ParameterValue> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(ParameterValue::Empty);
    }
    // Symbols are stock constant identifiers, not malformed numeric prefixes.
    if trimmed
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && !matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "nan" | "inf" | "infinity"
        )
    {
        return Ok(ParameterValue::Symbol(value.to_owned()));
    }
    finite(value, key).map(ParameterValue::Number)
}

fn integer_list(a: &Attributes, key: &str) -> Result<Option<Vec<u32>>> {
    a.get(key)
        .map(|value| {
            if value.trim().is_empty() {
                return Ok(Vec::new());
            }
            bounded_list(value)?
                .map(|v| {
                    let v = v.trim();
                    if !v.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(schema(&format!("invalid integer list {key}")));
                    }
                    v.parse()
                        .map_err(|_| schema(&format!("invalid integer list {key}")))
                })
                .collect()
        })
        .transpose()
}

fn float_list(a: &Attributes, key: &str) -> Result<Option<Vec<f64>>> {
    a.get(key)
        .map(|value| {
            if value.trim().is_empty() {
                return Ok(Vec::new());
            }
            bounded_list(value)?.map(|v| finite(v, key)).collect()
        })
        .transpose()
}

fn parameter_references(a: &Attributes, key: &str) -> Result<Option<Vec<ParameterReference>>> {
    a.get(key)
        .map(|value| {
            if value.trim().is_empty() {
                return Ok(Vec::new());
            }
            bounded_list(value)?
                .map(|v| {
                    let trimmed = v.trim();
                    if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
                        return trimmed
                            .parse()
                            .map(ParameterReference::Index)
                            .map_err(|_| schema("parameter reference index overflow"));
                    }
                    if trimmed
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                        && trimmed
                            .chars()
                            .all(|c| c.is_alphanumeric() || matches!(c, '_' | ' ' | '-' | '.'))
                    {
                        return Ok(ParameterReference::Name(v.to_owned()));
                    }
                    Err(schema("invalid parameter reference"))
                })
                .collect()
        })
        .transpose()
}

fn bounded_list(value: &str) -> Result<std::str::Split<'_, char>> {
    if value
        .bytes()
        .filter(|&b| b == b',')
        .take(MAX_LIST_ITEMS)
        .count()
        >= MAX_LIST_ITEMS
    {
        return Err(CatalogError::Limit("metadata list items"));
    }
    Ok(value.split(','))
}

fn boolean(a: &Attributes, key: &str) -> Result<Option<bool>> {
    a.get(key)
        .map(|v| match v.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(schema(&format!("invalid boolean {key}"))),
        })
        .transpose()
}

fn hidden(a: &Attributes) -> Option<Hidden> {
    a.get("hidden").map(|v| match v.as_str() {
        "true" => Hidden::Boolean(true),
        "false" => Hidden::Boolean(false),
        _ => Hidden::Opaque(v.clone()),
    })
}

fn schema(message: &str) -> CatalogError {
    CatalogError::Schema(message.to_owned())
}
