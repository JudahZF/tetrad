//! `sdk.d.ts` must describe what `prelude.js` and the protocol actually provide, and the
//! copies the extensions type-check against must match it.

use std::path::Path;

use tetrad_extensions::protocol::{ErrorCode, MidiEvent, PUBLISH_BLOCK, Pair, Site};
use tetrad_extensions::sdk::SDK_DECLARATIONS;

const PRELUDE: &str = include_str!("../src/prelude.js");

/// Member names of the ctx and scope objects: `name: value` and `name(args) {` lines.
fn prelude_members() -> Vec<&'static str> {
    let start = PRELUDE.find("const makeScope").expect("makeScope");
    let end = PRELUDE
        .find("globalThis.__tetrad_activate")
        .expect("activate");
    let mut names = Vec::new();
    for line in PRELUDE[start..end].lines() {
        let line = line.trim_start();
        let ident = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(line, |end| &line[..end]);
        let rest = &line[ident.len()..];
        let member = rest.starts_with(':') || (rest.starts_with('(') && line.ends_with('{'));
        if !ident.is_empty() && member {
            names.push(ident);
        }
    }
    names
}

/// Host method names such as `controls.set`, from the prelude's string literals. The prelude
/// itself calls [`PUBLISH_BLOCK`]; extensions never see it.
fn prelude_methods() -> Vec<&'static str> {
    PRELUDE
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|s| {
            s.split_once('.').is_some_and(|(a, b)| {
                !a.is_empty()
                    && !b.is_empty()
                    && s.chars().all(|c| c.is_ascii_alphabetic() || c == '.')
            })
        })
        .filter(|s| *s != PUBLISH_BLOCK)
        .collect()
}

fn declared(name: &str) -> bool {
    [":", "(", "?:"]
        .iter()
        .any(|suffix| SDK_DECLARATIONS.contains(&format!("{name}{suffix}")))
}

#[test]
fn every_prelude_member_is_declared() {
    let members = prelude_members();
    for expected in [
        "extension",
        "config",
        "lifecycle",
        "reserveChannel",
        "onChange",
        "ui",
        "provide",
        "updateDelay",
        "finish",
    ] {
        assert!(members.contains(&expected), "parser missed {expected}");
    }
    for name in members {
        assert!(declared(name), "sdk.d.ts does not declare {name}");
    }
    let methods = prelude_methods();
    assert!(methods.contains(&"controls.toggleBypass"), "{methods:?}");
    for method in methods {
        let (namespace, member) = method.split_once('.').unwrap();
        assert!(
            declared(namespace) && declared(member),
            "sdk.d.ts does not declare {method}"
        );
    }
}

#[test]
fn every_wire_name_is_declared() {
    let codes = [
        ErrorCode::UnknownMethod,
        ErrorCode::InvalidParams,
        ErrorCode::PermissionDenied,
        ErrorCode::NotRunning,
        ErrorCode::ScopeClosed,
        ErrorCode::StaleContext,
        ErrorCode::Unresolved,
        ErrorCode::Unsupported,
        ErrorCode::LimitExceeded,
        ErrorCode::Failed,
    ];
    // A new variant fails to compile here until it is added to the list above.
    match codes[0] {
        ErrorCode::UnknownMethod
        | ErrorCode::InvalidParams
        | ErrorCode::PermissionDenied
        | ErrorCode::NotRunning
        | ErrorCode::ScopeClosed
        | ErrorCode::StaleContext
        | ErrorCode::Unresolved
        | ErrorCode::Unsupported
        | ErrorCode::LimitExceeded
        | ErrorCode::Failed => {}
    }
    let mut names: Vec<String> = codes
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();

    let midi = [
        MidiEvent::ControlChange {
            channel: 1,
            controller: 0,
            value: 0,
        },
        MidiEvent::ProgramChange {
            channel: 1,
            program: 0,
        },
        MidiEvent::NoteOn {
            channel: 1,
            key: 0,
            velocity: 0,
        },
        MidiEvent::NoteOff {
            channel: 1,
            key: 0,
            velocity: 0,
        },
    ];
    for event in midi {
        match event {
            MidiEvent::ControlChange { .. }
            | MidiEvent::ProgramChange { .. }
            | MidiEvent::NoteOn { .. }
            | MidiEvent::NoteOff { .. } => {}
        }
        names.push(
            serde_json::to_value(event).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

    let sites = [
        Site::Slot { row: 0, column: 0 },
        Site::Input { row: 0 },
        Site::Output { row: 0 },
        Site::Splitter { pair: Pair::Upper },
        Site::Mixer { pair: Pair::Lower },
    ];
    for site in sites {
        match site {
            Site::Slot { .. }
            | Site::Input { .. }
            | Site::Output { .. }
            | Site::Splitter { .. }
            | Site::Mixer { .. } => {}
        }
        names.push(
            serde_json::to_value(site).unwrap()["site"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

    for name in names {
        assert!(
            SDK_DECLARATIONS.contains(&format!("\"{name}\"")),
            "sdk.d.ts does not declare \"{name}\""
        );
    }
}

#[test]
fn extension_copies_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions");
    for dir in ["midi-mapper", "fixtures/control-macro"] {
        let path = root.join(dir).join("tetrad.d.ts");
        let copy = std::fs::read_to_string(&path).unwrap();
        assert!(
            copy == SDK_DECLARATIONS,
            "{} is stale; run `tetrad-ext sdk --out {0}`",
            path.display()
        );
    }
}
