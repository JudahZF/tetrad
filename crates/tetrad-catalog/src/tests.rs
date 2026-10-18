use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use qc_format::blob::{LinkRange, ModuleBlob, Section, SectionBytes, SectionWords};

use crate::{
    BlobStatus, Catalog, CatalogError, DiagnosticCode, Hidden, ParameterReference, ParameterValue,
    Severity, validate_blob,
};

const EMPTY_REPORT: &str = "<ModuleReport/>";

fn repo(models: &str) -> String {
    format!("<Models><Category id='7' name='Utility'>{models}</Category></Models>")
}

#[test]
fn preserves_parameter_order_presence_symbols_and_opaque_metadata() {
    let source = repo(
        "<Model id='9' name='Gain &amp; Trim' plugin_id='2' sku='s' future='a &gt; b' replaces='8, 6' clones='10' sidechain='1'><Padding cpu='0.4' dm_hp='64'/><Parameter name='B' type='float' index='4' defaultValue='DEFAULT_TEMPO' min='MIN_EQ_DB' max='12' skew='LIN_SKEW' steps='3' stepNames='Off,A &amp; B, On' xValues='0,0.5,1' yValues='0,10,20' isplayPos='18' hidden='atma' replaces='0,1' linkedSceneMode='1' future='&quot;v&quot;'/><Parameter name='A' type='string' index='0' defaultValue='' steps='' replaces='INTENSITY'/><Opaque flag='yes'><Inner/></Opaque></Model>",
    );
    let catalog = Catalog::parse(&source, EMPTY_REPORT).unwrap();
    let model = catalog.model(9).unwrap();
    assert_eq!(model.name, "Gain & Trim");
    assert_eq!(model.replaces, Some(vec![8, 6]));
    assert_eq!(model.clones, Some(vec![10]));
    assert_eq!(model.sidechain, Some(1));
    assert_eq!(model.attributes["future"], "a > b");
    assert_eq!(model.padding.as_ref().unwrap().attributes["dm_hp"], "64");
    assert!(
        model
            .raw_xml
            .contains("<Opaque flag='yes'><Inner/></Opaque>")
    );
    assert_eq!(catalog.model_repo_xml, source);
    let p = &model.parameters[0];
    assert_eq!(p.position, 0);
    assert_eq!(p.min, Some(ParameterValue::Symbol("MIN_EQ_DB".into())));
    assert_eq!(p.max, Some(ParameterValue::Number(12.0)));
    assert_eq!(p.index, Some(4));
    assert_eq!(
        p.default_value,
        Some(ParameterValue::Symbol("DEFAULT_TEMPO".into()))
    );
    assert_eq!(p.skew, Some(ParameterValue::Symbol("LIN_SKEW".into())));
    assert_eq!(
        p.step_names,
        Some(vec!["Off".into(), "A & B".into(), " On".into()])
    );
    assert_eq!(p.linked_scene_mode, Some(vec![1]));
    assert_eq!(p.x_values, Some(vec![0.0, 0.5, 1.0]));
    assert_eq!(p.hidden, Some(Hidden::Opaque("atma".into())));
    assert_eq!(p.display_pos, None);
    assert_eq!(p.attributes["isplayPos"], "18");
    assert_eq!(p.attributes["future"], "\"v\"");
    let second = &model.parameters[1];
    assert_eq!(second.name, "A");
    assert_eq!(
        second.default_value,
        Some(ParameterValue::Text(String::new()))
    );
    assert_eq!(second.steps, None);
    assert_eq!(second.attributes["steps"], "");
    assert_eq!(
        second.replaces,
        Some(vec![ParameterReference::Name("INTENSITY".into())])
    );
    assert_eq!(second.min, None);
    assert_eq!(second.max, None);
    assert!(
        catalog
            .diagnostics()
            .iter()
            .any(|d| d.code == DiagnosticCode::AttributeTypo)
    );
    assert!(
        catalog
            .diagnostics()
            .iter()
            .all(|d| d.severity != Severity::Error)
    );
}

#[test]
fn linked_scene_mode_preserves_order_duplicates_presence_and_raw_text() {
    for (attribute, expected) in [
        ("", None),
        (" linkedSceneMode=''", Some(vec![])),
        (" linkedSceneMode=' 2, 1,2,0 '", Some(vec![2, 1, 2, 0])),
    ] {
        let source = repo(&format!(
            "<Model id='9' name='M'><Parameter name='P' type='float'{attribute}/></Model>"
        ));
        let catalog = Catalog::parse(&source, EMPTY_REPORT).unwrap();
        let parameter = &catalog.model(9).unwrap().parameters[0];
        assert_eq!(parameter.linked_scene_mode, expected);
        assert_eq!(catalog.model_repo_xml, source);
        if !attribute.is_empty() {
            assert!(parameter.raw_xml.contains(attribute));
        }
    }
    for value in ["-1", "+1", "1,,2", "x", "4294967296"] {
        let source = repo(&format!(
            "<Model id='9' name='M'><Parameter name='P' type='float' linkedSceneMode='{value}'/></Model>"
        ));
        assert!(Catalog::parse(&source, EMPTY_REPORT).is_err());
    }
    let items = std::iter::repeat_n("0", 4097).collect::<Vec<_>>().join(",");
    let source = repo(&format!(
        "<Model id='9' name='M'><Parameter name='P' type='float' linkedSceneMode='{items}'/></Model>"
    ));
    assert!(matches!(
        Catalog::parse(&source, EMPTY_REPORT),
        Err(CatalogError::Limit("metadata list items"))
    ));
}

#[test]
fn joins_by_id_and_keeps_report_resources_and_test_order() {
    let catalog = Catalog::parse(&repo("<Model id='1' name='A' blob='shared'/><Model id='2' name='B' blob='shared'/><Model id='3' name='Plugin' plugin_id='1'/><Model id='4' name='Internal' blob=''/><Model id='5' name='Absent'/>"), "<ModuleReport><Kernel core='1' cpu='3.2' cpu_avg='3' isr='0' dm_heap='64' future='keep'/><Module id='2' name='B' blob_name='different' category='7' dma='1.85' dm_heap='8' Latency='16'><Test name='Second' min='2' max='3' avg='2.5' pk_l='-40' pk_r='-40'/><Test name='First' avg='1'/></Module><Module id='8' name='Report only' blob_name='shared'/></ModuleReport>").unwrap();
    assert_eq!(catalog.summary().models, 5);
    assert_eq!(catalog.summary().blob_attributes, 3);
    assert_eq!(catalog.summary().models_with_blobs, 2);
    assert_eq!(catalog.summary().unique_model_blobs, 1);
    assert_eq!(catalog.summary().matched_modules, 1);
    assert_eq!(catalog.summary().unmatched_modules, 1);
    assert_eq!(catalog.model_order, vec![1, 2, 3, 4, 5]);
    assert_eq!(catalog.modules[&2].resources.dma, Some(1.85));
    assert_eq!(catalog.modules[&2].resources.latency, Some(16));
    assert_eq!(catalog.modules[&2].tests[0].name, "Second");
    assert_eq!(catalog.kernels[&1].resources.cpu, Some(3.2));
    let diagnostics = catalog.diagnostics();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::BlobNameMismatch)
            .count(),
        1
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::MissingModuleReport)
            .count(),
        1
    );
    assert_eq!(
        diagnostics
            .iter()
            .filter(|d| d.code == DiagnosticCode::UnmatchedModule)
            .count(),
        1
    );
    assert!(catalog.model(999).is_none());
}

#[test]
fn rejects_malformed_schema_duplicates_and_numeric_values() {
    for source in [
        "<Wrong><Models/></Wrong>",
        "<Models><Model id='1' name='A'/></Models>",
        "<Models><Wrapper><Category id='1' name='A'/></Wrapper></Models>",
        "<Models><Category id='1' name='A'><Wrapper><Model id='1' name='A'/></Wrapper></Category></Models>",
        "<Models><Category id='1' name='A'><Model id='1' name='A'><Wrapper><Parameter name='P' type='float'/></Wrapper></Model></Category></Models>",
        "<Models><Category id='1' name='A'><Category id='2' name='B'/></Category></Models>",
        "<Models><Category id='1' name='A'/><Category id='1' name='B'/></Models>",
        "<Models><Category id='1' name='A'><Model id='1' name='A'/><Model id='1' name='B'/></Category></Models>",
        "<Models><Category id='1' name='A'><Parameter name='P' type='float'/></Category></Models>",
        "<Models><Category id='1' name='A'><Model id='1' name='A'><Padding><Parameter name='P' type='float'/></Padding></Model></Category></Models>",
        "<Models><Category id='1' name='A'><Model id='1' name='A'></Category></Model></Models>",
        "<Models><Category id='4294967296' name='A'/></Models>",
    ] {
        assert!(
            Catalog::parse(source, EMPTY_REPORT).is_err(),
            "accepted bad schema"
        );
    }
    for attributes in [
        "min='NaN'",
        "max='inf'",
        "defaultValue='1oops'",
        "skew='0.2junk'",
        "steps='-1'",
        "index='4294967296'",
        "dynamic='yes'",
        "xValues='0,nope,1'",
        "replaces='1,,2'",
    ] {
        assert!(
            Catalog::parse(
                &repo(&format!(
                    "<Model id='1' name='A'><Parameter name='P' type='float' {attributes}/></Model>"
                )),
                EMPTY_REPORT
            )
            .is_err(),
            "accepted bad number or boolean"
        );
    }
    for report in [
        "<ModuleReport><Kernel core='1'/><Kernel core='1'/></ModuleReport>",
        "<ModuleReport><Test name='orphan'/></ModuleReport>",
        "<ModuleReport><Module id='1' name='A' dm_heap='-1'/></ModuleReport>",
    ] {
        assert!(Catalog::parse("<Models/>", report).is_err());
    }
}

#[test]
fn applies_byte_attribute_and_list_limits() {
    let oversized = " ".repeat(super::xml::MAX_XML_BYTES + 1);
    assert!(matches!(
        Catalog::parse(&oversized, EMPTY_REPORT),
        Err(CatalogError::Limit(_))
    ));
    let attrs = (0..129).map(|i| format!(" a{i}='v'")).collect::<String>();
    assert!(matches!(
        Catalog::parse(
            &repo(&format!("<Model id='1' name='A'{attrs}/>")),
            EMPTY_REPORT
        ),
        Err(CatalogError::Limit(_))
    ));
    let list = "1,".repeat(4096);
    assert!(matches!(
        Catalog::parse(
            &repo(&format!("<Model id='1' name='A' replaces='{list}'/>")),
            EMPTY_REPORT
        ),
        Err(CatalogError::Limit(_))
    ));
}

#[test]
fn rejects_large_known_and_opaque_tags_before_xml_tokenization() {
    let attrs = (0..8000).map(|i| format!(" a{i}='v'")).collect::<String>();
    for source in [
        format!("<Models{attrs}/>"),
        format!("<Models><Category id='7' name='Utility'{attrs}/></Models>"),
        repo(&format!("<Model id='1' name='A'{attrs}/>")),
        format!("<Models><Opaque{attrs}/></Models>"),
        // An invalid first value would make qc-format fail before reaching
        // the limit. The limit error proves the preflight runs first.
        format!("<Models><Opaque bad='&unknown;'{attrs}/></Models>"),
        format!("<!DOCTYPE Models '><Models{attrs}/>"),
    ] {
        assert!(matches!(
            Catalog::parse(&source, EMPTY_REPORT),
            Err(CatalogError::Limit("XML attribute count"))
        ));
    }
    for report in [
        format!("<ModuleReport{attrs}/>"),
        format!("<ModuleReport><Module id='1' name='A'{attrs}/></ModuleReport>"),
        format!("<ModuleReport><Opaque{attrs}/></ModuleReport>"),
    ] {
        assert!(matches!(
            Catalog::parse("<Models/>", &report),
            Err(CatalogError::Limit("XML attribute count"))
        ));
    }
}

#[test]
fn attribute_preflight_handles_quotes_markup_and_exact_boundary() {
    let value = "= >".repeat(256);
    let attrs = (0..126).map(|i| format!(" a{i}='v'")).collect::<String>();
    let opaque = format!("<Opaque single='{value}\"' double=\"{value}'\"{attrs}/>");
    let source = format!(
        "<?xml version='1.0'?><Models><!-- '{opaque} -->\
         <![CDATA[\"{opaque}]]><?opaque '{opaque}?>{opaque}</Models>"
    );
    let catalog = Catalog::parse(&source, EMPTY_REPORT).unwrap();
    assert_eq!(catalog.model_repo_xml, source);
    let root_attrs = (0..128).map(|i| format!(" a{i}='v'")).collect::<String>();
    assert!(
        Catalog::parse(
            &format!("<Models{root_attrs}/>"),
            &format!("<ModuleReport{root_attrs}/>")
        )
        .is_ok()
    );
    for source in [
        format!("<Models{root_attrs} extra='v'/>"),
        format!("<Models><Opaque{root_attrs} extra='v'/></Models>"),
    ] {
        assert!(matches!(
            Catalog::parse(&source, EMPTY_REPORT),
            Err(CatalogError::Limit("XML attribute count"))
        ));
    }
    // Skipped markup uses exact terminators; its quote characters must not
    // hide the over-limit tag which follows it.
    for prefix in ["<!-- ' -->", "<?opaque '?>", "<![CDATA[']]>", "</Bad'>"] {
        let source = format!("<Models>{prefix}<Opaque{root_attrs} extra='v'/></Models>");
        assert!(matches!(
            Catalog::parse(&source, EMPTY_REPORT),
            Err(CatalogError::Limit("XML attribute count"))
        ));
    }
    for source in [
        "<Models><Opaque value='unterminated></Models>",
        "<Models><!-- unterminated</Models>",
        "<Models><![CDATA[unterminated</Models>",
        "<?unterminated<Models/>",
        "<Models><Opaque value='<'/></Models>",
    ] {
        assert!(matches!(
            Catalog::parse(source, EMPTY_REPORT),
            Err(CatalogError::Format(_))
        ));
    }
}

fn valid_blob() -> ModuleBlob {
    ModuleBlob {
        contents: vec![SectionBytes {
            section: Section::Code,
            bytes: vec![0, 0],
        }],
        relocations: Vec::new(),
        ranges: vec![LinkRange::for_contents(Section::Code, 2).unwrap()],
        entry: Section::Code.base(),
    }
}

#[test]
fn validates_code_and_isa_relocation_offsets_in_section_units() {
    for section in [Section::Code, Section::Isa] {
        let mut blob = valid_blob();
        let bytes = vec![0; 2 * section.unit_bytes() as usize];
        let range = LinkRange::for_contents(section, bytes.len()).unwrap();
        if section == Section::Code {
            blob.contents[0].bytes = bytes;
            blob.ranges[0] = range;
        } else {
            blob.contents.push(SectionBytes { section, bytes });
            blob.ranges.push(range);
        }
        blob.relocations.push(SectionWords {
            section,
            // Unknown kinds and targets are preserved, not guessed. Check
            // both the first unit and the last unit, including a second record.
            words: vec![
                u32::MAX,
                0,
                u32::MAX,
                u32::MAX,
                11,
                1,
                Section::Code.base(),
                0,
            ],
        });
        validate_blob(&blob).unwrap();
        for offset in [2, section.unit_bytes(), u32::MAX] {
            blob.relocations[0].words[5] = offset;
            blob.validate().unwrap();
            assert!(matches!(
                validate_blob(&blob),
                Err(qc_format::FormatError::InvalidBlob(_))
            ));
        }
    }
    let mut blob = valid_blob();
    blob.relocations.push(SectionWords {
        section: Section::Code,
        words: vec![11, u32::MAX, Section::Code.base(), 0],
    });
    blob.validate().unwrap();
    assert!(validate_blob(&blob).is_err());
    blob.relocations[0].words[1] = 0;
    blob.entry += 1;
    assert!(
        validate_blob(&blob).is_err(),
        "stock validation still applies"
    );
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tetrad-catalog-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn batch_distinguishes_missing_malformed_semantic_and_optional_models() {
    let directory = TestDirectory::new();
    std::fs::write(
        directory.0.join("shared.raw"),
        valid_blob().to_bytes().unwrap(),
    )
    .unwrap();
    std::fs::write(directory.0.join("bad.raw"), [0, 1, 2]).unwrap();
    let mut semantic = valid_blob();
    semantic.relocations.push(SectionWords {
        section: Section::Code,
        words: vec![11, u32::MAX, Section::Code.base(), 0],
    });
    semantic.validate().unwrap();
    std::fs::write(
        directory.0.join("semantic.raw"),
        semantic.to_bytes().unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.0.join("unreferenced.raw"),
        valid_blob().to_bytes().unwrap(),
    )
    .unwrap();
    let catalog = Catalog::parse(&repo("<Model id='1' name='A' blob='shared'/><Model id='2' name='B' blob='shared'/><Model id='3' name='P' plugin_id='1'/><Model id='4' name='I' blob=''/><Model id='5' name='M' blob='missing'/><Model id='6' name='B' blob='bad'/><Model id='7' name='S' blob='semantic'/>"), "<ModuleReport><Module id='20' name='Report only' blob_name='shared'/></ModuleReport>").unwrap();
    let report = catalog.inspect_blobs(&directory.0).unwrap();
    assert_eq!(report.files_found, 4);
    assert_eq!(report.parsed, 3);
    assert_eq!(report.roundtrip_ok, 3);
    assert_eq!(report.semantic_valid, 2);
    assert_eq!(report.semantic_failures, 1);
    assert_eq!(report.missing_referenced, 1);
    assert_eq!(report.malformed, 1);
    assert_eq!(report.non_blob_models, 2);
    assert_eq!(report.unreferenced_files, 1);
    assert!(report.has_errors());
    let shared = report
        .entries
        .iter()
        .find(|e| e.blob_name == "shared")
        .unwrap();
    assert_eq!(shared.model_ids, vec![1, 2]);
    assert_eq!(shared.module_ids, vec![20]);
    let semantic = report
        .entries
        .iter()
        .find(|e| e.blob_name == "semantic")
        .unwrap();
    assert_eq!(semantic.status, BlobStatus::Parsed);
    assert_eq!(semantic.roundtrip_ok, Some(true));
    assert_eq!(semantic.semantic_valid, Some(false));
    assert!(semantic.semantic_error.is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn refuses_traversal_symlinks_and_nonregular_blobs() {
    use std::os::unix::fs::symlink;
    let directory = TestDirectory::new();
    let outside = TestDirectory::new();
    std::fs::write(
        outside.0.join("private.raw"),
        valid_blob().to_bytes().unwrap(),
    )
    .unwrap();
    symlink(
        outside.0.join("private.raw"),
        directory.0.join("linked.raw"),
    )
    .unwrap();
    symlink(
        outside.0.join("does-not-exist"),
        directory.0.join("dangling.raw"),
    )
    .unwrap();
    std::fs::create_dir(directory.0.join("folder.raw")).unwrap();
    std::fs::File::create(directory.0.join("oversized.raw"))
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    let catalog = Catalog::parse(&repo("<Model id='1' name='T' blob='../private'/><Model id='2' name='L' blob='linked'/><Model id='3' name='D' blob='dangling'/><Model id='4' name='F' blob='folder'/><Model id='5' name='A' blob='/absolute'/>"), EMPTY_REPORT).unwrap();
    let report = catalog.inspect_blobs(&directory.0).unwrap();
    assert_eq!(report.unsafe_paths, 5);
    assert_eq!(report.read_errors, 1);
    assert_eq!(report.missing_referenced, 0);
    assert_eq!(report.parsed, 0);
    assert!(report.entries.iter().all(|entry| entry.byte_len.is_none()));
    assert!(report.has_errors());
    symlink(&outside.0, directory.0.join("root-link")).unwrap();
    assert!(
        catalog
            .inspect_blobs(&directory.0.join("root-link"))
            .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn optional_models_and_valid_unreferenced_files_do_not_fail_validation() {
    let directory = TestDirectory::new();
    std::fs::write(
        directory.0.join("extra.raw"),
        valid_blob().to_bytes().unwrap(),
    )
    .unwrap();
    let catalog = Catalog::parse(&repo("<Model id='1' name='P' plugin_id='1'/><Model id='2' name='I' blob=''/><Model id='3' name='N'/>"), EMPTY_REPORT).unwrap();
    assert!(catalog.diagnostics().is_empty());
    let report = catalog.inspect_blobs(&directory.0).unwrap();
    assert!(!report.has_errors());
    assert_eq!(report.non_blob_models, 3);
    assert_eq!(report.unreferenced_files, 1);
}

/// `$QC_CORPUS_DIR/plain` (README, "Corpus tests"), or `None` (with a note) when it has no
/// `ModelRepo.xml`.
fn corpus_dir() -> Option<PathBuf> {
    let root = std::env::var_os("QC_CORPUS_DIR").map(|dir| PathBuf::from(dir).join("plain"));
    if root
        .as_ref()
        .is_none_or(|root| !root.join("ModelRepo.xml").is_file())
    {
        eprintln!("skipped: set QC_CORPUS_DIR (with plain/ModelRepo.xml)");
        return None;
    }
    root
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires QC_CORPUS_DIR"]
fn plaintext_corpus_accepts_bounded_xml_and_relocation_offsets() {
    let Some(root) = corpus_dir() else { return };
    let models = std::fs::read_to_string(root.join("ModelRepo.xml")).unwrap();
    let modules = std::fs::read_to_string(root.join("moduleReport.xml")).unwrap();
    let catalog = Catalog::parse(&models, &modules).unwrap();
    let report = catalog.inspect_blobs(&root.join("blobs/core1")).unwrap();
    eprintln!(
        "catalog: {:?}; blobs: {} parsed, {} roundtrips, {} semantic passes",
        catalog.summary(),
        report.parsed,
        report.roundtrip_ok,
        report.semantic_valid
    );
    assert!(report.files_found > 0);
    assert!(!report.has_errors(), "blob corpus failures: {report:?}");
    assert_eq!(report.parsed, report.files_found);
    assert_eq!(report.roundtrip_ok, report.parsed);
    assert_eq!(report.semantic_valid, report.parsed);
}

/// Pins the resolved-spec contract the compiler and UI rely on: stock constants,
/// roles from categories, sidechain parameter checks and report costs.
#[test]
fn specs_resolve_constants_roles_sidechains_and_costs() {
    let models = "<Models><Category id='10' name='Splitter' hidden='true'><Model id='10000' name='Splitter AB'><Parameter name='LEVEL TO A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0.0' min_string='OFF' units='dB'/></Model></Category><Category id='5' name='Compressor'><Model id='5018' name='Comp' blob='comp' sidechain='1'><Parameter name='THRESHOLD' type='float' min='-48' max='0' defaultValue='-40' skew='LOG_SKEW'/><Parameter name='SOURCE' type='rotarySwitch' min='0' max='44' defaultValue='0' dynamic='true' stepNames='Off,Follow Input'/></Model></Category></Models>";
    let report = "<ModuleReport><Kernel core='1' cpu='3.7' isr='0' dm_heap='448' pm_heap='496' sd_heap='1152'/><Module id='5018' name='Comp' blob_name='comp' category='5' dm_heap='900' pm_heap='64' sd_heap='0' dma='0' dm='80' pm='0' sw='9000' nw='96' cpu_pad='1' Latency='16'><Test name='Stimulus Med' avg='3.0'/><Test name='Stimulus High' avg='91.5' max='95'/></Module></ModuleReport>";
    let specs = crate::Specs::resolve(&Catalog::parse(models, report).unwrap()).unwrap();
    let splitter = specs.model(10_000).unwrap();
    assert_eq!(splitter.role, crate::ModelRole::Splitter);
    assert!(specs.categories[&10].hidden);
    let level = &splitter.parameters[0];
    assert_eq!(
        level.range,
        Some(crate::ParamRange {
            min: -40.0,
            max: 12.0
        })
    );
    assert_eq!(level.labels.min.as_deref(), Some("OFF"));
    assert_eq!(splitter.stats, None);
    let comp = specs.model(5018).unwrap();
    assert_eq!(comp.role, crate::ModelRole::Block);
    assert_eq!(comp.parameters[0].skew, Some(0.3));
    assert_eq!(
        comp.sidechain,
        Some(crate::SidechainSpec {
            source_param: 1,
            input_param: None
        })
    );
    assert!(comp.parameters[1].dynamic);
    // CPU is the clamped "Stimulus High" average; code memory is sw + nw.
    let stats = comp.stats.unwrap();
    assert_eq!(stats.cpu, crate::ModuleStats::CPU_CLAMP);
    assert_eq!((stats.dm_heap, stats.pmco, stats.latency), (900, 9096, 16));
    assert!(stats.has_sidechain && stats.reuse_pmco);
    assert_eq!(specs.kernels[&1].pm_heap, 496);

    let broken = models.replace("sidechain='1'", "sidechain='2'");
    assert!(matches!(
        crate::Specs::resolve(&Catalog::parse(&broken, report).unwrap()),
        Err(crate::SpecError::SidechainParam { index: 2, .. })
    ));
    // Stock warns on an unknown macro and keeps the attribute default (max 1).
    let unknown = models.replace("MAX_MIXER_DB", "MAX_UNKNOWN_DB");
    let specs = crate::Specs::resolve(&Catalog::parse(&unknown, report).unwrap()).unwrap();
    assert_eq!(
        specs.model(10_000).unwrap().parameters[0].range,
        Some(crate::ParamRange {
            min: -40.0,
            max: 1.0
        })
    );
}

/// Piecewise parameters store a normalized default (models 4008, 6029); the
/// power law applies only without `yValues`, and `xValues` carries over to the
/// next parameter like the stock parser (`0x4e9ed8`, `tetrad_core::conversion`).
#[test]
fn piecewise_defaults_stay_normalized_and_x_values_carry_over() {
    let models = repo(
        "<Model id='4008' name='Parametric'><Parameter name='FREQ' type='float' min='20' max='200' defaultValue='0.5' xValues='0,0.5,1' yValues='20,60,200'/><Parameter name='FREQ 2' type='float' min='20' max='200' defaultValue='0.25' yValues='20,60,200'/><Parameter name='GAIN' type='float' min='MIN_EQ_DB' max='MAX_EQ_DB' defaultValue='6'/></Model>",
    );
    let specs = crate::Specs::resolve(&Catalog::parse(&models, EMPTY_REPORT).unwrap()).unwrap();
    let p = &specs.model(4008).unwrap().parameters;
    assert_eq!(p[0].default_normalized(), 0.5);
    assert_eq!(p[0].default_number(), Some(60.0));
    assert_eq!(p[1].default_normalized(), 0.25);
    assert_eq!(p[1].normalize(130.0), Some(0.75));
    assert_eq!(p[2].default_normalized(), 0.75);
}

#[test]
#[ignore = "requires QC_CORPUS_DIR"]
fn plaintext_corpus_resolves_every_model_spec() {
    let Some(root) = corpus_dir() else { return };
    let models = std::fs::read_to_string(root.join("ModelRepo.xml")).unwrap();
    let modules = std::fs::read_to_string(root.join("moduleReport.xml")).unwrap();
    let catalog = Catalog::parse(&models, &modules).unwrap();
    let specs = crate::Specs::resolve(&catalog).unwrap();
    assert_eq!(specs.models.len(), catalog.models.len());
    let mut uncosted = Vec::new();
    for model in specs.models.values() {
        if let Some(sidechain) = model.sidechain {
            let source = &model.parameters[sidechain.source_param as usize];
            assert!(source.dynamic, "model {} sidechain source", model.id);
            assert_eq!(source.step_names.len(), 45, "model {}", model.id);
        }
        if model.blob.is_some() && model.role == crate::ModelRole::Block && model.stats.is_none() {
            uncosted.push(model.id);
        }
    }
    // Only the hidden base reverb lacks a report row; plugin stereo cabs
    // inherit their cloned engine's stats.
    assert_eq!(uncosted, [8015]);
    let cab = &specs.models[&12_083];
    assert_eq!(cab.parameters.len(), specs.models[&12_100].parameters.len());
    assert_eq!(cab.parameters[1].name, "ir selector");
    assert_eq!(specs.kernels.len(), 4);
}

/// Clones run their base model (`0x5507c4`): the effective parameter list is the
/// base list with `replaces="i"` overrides and appended extras; blob and stats
/// fall back to the base. Stored presets follow this effective list.
#[test]
fn clones_inherit_base_parameters_blob_and_stats() {
    let models = repo(
        "<Model id='1' name='Engine' blob='engine'><Parameter name='bypass' type='switch' min='0' max='1' defaultValue='0'/><Parameter name='ir selector' type='string' defaultValue=''/><Parameter name='LEVEL' type='float' min='-40' max='6' defaultValue='0'/></Model>\
         <Model id='2' name='Cab' clones='1'><Parameter name='ir selector' type='string' defaultValue='IR A' replaces='1'/><Parameter name='EXTRA' type='float' min='0' max='1' defaultValue='1'/></Model>\
         <Model id='3' name='Broken' clones='2'/>",
    );
    let report = "<ModuleReport><Module id='1' name='Engine' blob_name='engine' category='7' dm_heap='1' pm_heap='1' sd_heap='0' dma='0' dm='1' pm='0' sw='1' nw='0' Latency='0'><Test name='Stimulus High' avg='2'/></Module></ModuleReport>";
    let catalog = Catalog::parse(&models, report).unwrap();
    assert!(matches!(
        crate::Specs::resolve(&catalog),
        Err(crate::SpecError::CloneBase { model: 3, base: 2 })
    ));
    let models = models.replace("<Model id='3' name='Broken' clones='2'/>", "");
    let specs = crate::Specs::resolve(&Catalog::parse(&models, report).unwrap()).unwrap();
    let cab = specs.model(2).unwrap();
    let names: Vec<_> = cab.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["bypass", "ir selector", "LEVEL", "EXTRA"]);
    assert_eq!(
        cab.parameters[1].default,
        Some(crate::ParamDefault::Text("IR A".into()))
    );
    assert_eq!(cab.parameters[3].index, 3);
    assert_eq!(cab.blob.as_deref(), Some("engine"));
    assert_eq!(cab.stats.unwrap().cpu, 2.0);
}
