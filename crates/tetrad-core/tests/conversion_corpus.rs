//! Local corpus check: every ModelRepo parameter converts under the stock
//! parser rules. Reads `$QC_CORPUS_DIR/plain/ModelRepo.xml`
//! (docs/development.md, "Corpus tests"); run with
//! `cargo test -p tetrad-core --test conversion_corpus -- --ignored`.

use std::collections::BTreeMap;
use tetrad_core::conversion::{DefaultValue, ParameterScale, ParameterType, XmlParameter};

/// Minimal scanner for `<Parameter .../>` attributes with XML entity decoding.
fn parameters(xml: &str) -> Vec<BTreeMap<String, String>> {
    xml.split("<Parameter ")
        .skip(1)
        .map(|tag| {
            let tag = &tag[..tag.find('>').unwrap()];
            let mut attributes = BTreeMap::new();
            let mut rest = tag;
            while let Some(eq) = rest.find("=\"") {
                let key = rest[..eq].trim().to_owned();
                let value_end = rest[eq + 2..].find('"').unwrap();
                attributes.insert(key, decode(&rest[eq + 2..eq + 2 + value_end]));
                rest = &rest[eq + 3 + value_end..];
            }
            attributes
        })
        .collect()
}

fn decode(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let end = rest[amp..].find(';').unwrap() + amp;
        let entity = &rest[amp + 1..end];
        out.push(match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            numeric => {
                let code = match numeric.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16).unwrap(),
                    None => numeric[1..].parse().unwrap(),
                };
                char::from_u32(code).unwrap()
            }
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[test]
#[ignore = "reads $QC_CORPUS_DIR/plain/ModelRepo.xml"]
fn every_model_repo_parameter_converts_and_formats() {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return;
    };
    let xml = std::fs::read_to_string(std::path::Path::new(&dir).join("plain/ModelRepo.xml"))
        .expect("$QC_CORPUS_DIR/plain/ModelRepo.xml");
    let all = parameters(&xml);
    assert_eq!(all.len(), 4683);
    let mut last_x: Option<String> = None;
    let (mut curves, mut text_values) = (0, 0);
    for attributes in &all {
        let get = |key: &str| attributes.get(key).map(String::as_str);
        if let Some(x) = get("xValues") {
            last_x = Some(x.to_owned());
        }
        let scale = ParameterScale::from_xml(&XmlParameter {
            type_name: get("type"),
            steps: get("steps"),
            skew: get("skew"),
            min: get("min"),
            max: get("max"),
            default_value: get("defaultValue"),
            min_string: get("min_string"),
            mid_string: get("mid_string"),
            max_string: get("max_string"),
            units: get("units"),
            step_names: get("stepNames"),
            x_values: last_x.as_deref(),
            y_values: get("yValues"),
            show_as_integer: get("showAsInteger"),
            exp_assignable: get("expAssignable"),
            disabled: get("disabled"),
        })
        .unwrap_or_else(|error| panic!("{attributes:?}: {error}"));
        curves += usize::from(scale.piecewise.is_some());
        let DefaultValue::Normalized(default) = scale.default else {
            assert_eq!(scale.kind, ParameterType::String);
            continue;
        };
        let display = scale.to_display(default).unwrap();
        assert!(display.is_finite(), "{attributes:?}");
        match scale.display_text(default) {
            Ok(_) => text_values += 1,
            // Stock asserts `stepNames.size() == steps` for these types.
            Err(error) => assert!(
                !matches!(
                    scale.kind,
                    ParameterType::RotarySwitch | ParameterType::ComboBox
                ),
                "{attributes:?}: {error}"
            ),
        }
    }
    assert_eq!(curves, 5);
    assert!(text_values > 3000, "{text_values}");
}
