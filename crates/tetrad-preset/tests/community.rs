use tetrad_preset::{
    Chain, Error, Limits, Model, ParamValue, Preset, Unknown,
    community::{Missing, Stash, TETRAD_BLOCKS_FIELD, restore, stash},
    model::TETRAD_MAPPINGS_FIELD,
};

const COMMUNITY: u32 = 900_001;
const KEY: &str = "acme.fuzz";
const PLACEHOLDER: u32 = 10_002;
const STOCK: u32 = 100;
const MAPPINGS: &[u8] = br#"{"version":1,"mappings":[]}"#;

fn unknown(number: u32, data: &[u8]) -> Unknown {
    let mut unknown = Unknown::default();
    unknown
        .set_bytes_field(number, Some(data.to_vec()))
        .unwrap();
    unknown
}

/// Four rows of eight empty slots, one stock block and the mappings field.
fn grid() -> Preset {
    let chain = Chain {
        models: vec![Model::empty_slot(); 8],
        ..Chain::default()
    };
    let mut preset = Preset {
        chains: vec![chain; 4],
        unknown: unknown(TETRAD_MAPPINGS_FIELD, MAPPINGS),
        ..Preset::default()
    };
    preset.chains[0].models[0] = Model::block(STOCK, [ParamValue::Float(0.5)]);
    preset
}

/// A community block with scene values, an expression, a stored column and unknown fields.
fn community_block() -> Model {
    let mut model = Model::block(COMMUNITY, [ParamValue::Float(0.25), ParamValue::Int(3)]);
    let param = &mut model.params[0];
    param.set_scene_mode(true, 0).unwrap();
    param.set_value(5, ParamValue::Float(0.75)).unwrap();
    param.set_expression(2, 0.1, 0.9).unwrap();
    param.values[1].unknown = unknown(9, b"scene");
    param.unknown = unknown(42, b"param");
    model.column = Some(3);
    model.unknown = unknown(77, b"model");
    model
}

fn stashes(id: u32) -> Option<Stash> {
    (id == COMMUNITY).then(|| Stash {
        key: KEY.to_owned(),
        placeholder: Model::block(PLACEHOLDER, [0.1, 0.2, 0.3].map(ParamValue::Float)),
    })
}

/// Tetrad state with a community block at (1, 3), and the stock file Tetrad writes for it.
fn written() -> (Preset, Vec<u8>) {
    let mut preset = grid();
    preset.chains[1].models[3] = community_block();
    let original = preset.clone();
    stash(&mut preset, stashes).unwrap();
    (original, preset.to_bytes())
}

fn load(bytes: &[u8]) -> Preset {
    Preset::decode(bytes, Limits::default()).unwrap()
}

/// The written file holds a playable placeholder; restore gives back the exact model with the
/// locally installed ID, and the mappings field is not disturbed.
#[test]
fn stash_and_restore_round_trip_the_exact_model() {
    let (original, bytes) = written();
    let mut preset = load(&bytes);
    let placeholder = &preset.chains[1].models[3];
    assert_eq!(placeholder.hash, Some(PLACEHOLDER));
    assert_eq!(
        placeholder.column, None,
        "Model::block has no stored column"
    );
    assert_eq!(
        preset.unknown.bytes_field(TETRAD_MAPPINGS_FIELD).as_deref(),
        Some(MAPPINGS)
    );

    let local = COMMUNITY + 5;
    let missing = restore(&mut preset, |key| (key == KEY).then_some(local)).unwrap();
    assert!(missing.is_empty());
    let restored = &preset.chains[1].models[3];
    assert_eq!(restored.hash, Some(local));
    assert_eq!(
        Model {
            hash: Some(COMMUNITY),
            ..restored.clone()
        },
        original.chains[1].models[3]
    );
    assert_eq!(preset.chains[0], original.chains[0]);
    // The restored block is in its slot again: its record goes, and the next stash writes it anew.
    assert!(preset.unknown.bytes_field(TETRAD_BLOCKS_FIELD).is_none());
}

/// A block that is not installed stays a placeholder, is reported, and keeps its record across
/// another write, so a later install restores it.
#[test]
fn missing_block_keeps_its_record_across_a_second_write() {
    let (original, bytes) = written();
    let mut preset = load(&bytes);
    let missing = restore(&mut preset, |_| None).unwrap();
    assert_eq!(
        missing,
        [Missing {
            row: 1,
            column: 3,
            key: KEY.to_owned()
        }]
    );
    assert_eq!(preset.chains[1].models[3].hash, Some(PLACEHOLDER));

    stash(&mut preset, stashes).unwrap();
    assert_eq!(preset.to_bytes(), bytes);

    let mut preset = load(&bytes);
    restore(&mut preset, |_| Some(COMMUNITY)).unwrap();
    assert_eq!(preset.chains, original.chains);
}

/// When stock replaces the placeholder, the stock block wins and the next write drops the record.
#[test]
fn slot_changed_by_stock_drops_the_record() {
    let (_, bytes) = written();
    let mut preset = load(&bytes);
    let stock = Model::block(STOCK, [ParamValue::Float(0.9)]);
    preset.chains[1].models[3] = stock.clone();

    assert!(
        restore(&mut preset, |_| Some(COMMUNITY))
            .unwrap()
            .is_empty()
    );
    assert_eq!(preset.chains[1].models[3], stock);

    stash(&mut preset, stashes).unwrap();
    assert_eq!(preset.unknown.bytes_field(TETRAD_BLOCKS_FIELD), None);
    assert_eq!(
        preset.unknown.bytes_field(TETRAD_MAPPINGS_FIELD).as_deref(),
        Some(MAPPINGS)
    );
}

/// A preset without community blocks gets no field and stays byte-identical.
#[test]
fn preset_without_community_blocks_is_unchanged() {
    let bytes = grid().to_bytes();
    let mut preset = load(&bytes);
    stash(&mut preset, stashes).unwrap();
    assert!(
        restore(&mut preset, |_| Some(COMMUNITY))
            .unwrap()
            .is_empty()
    );
    assert_eq!(preset.unknown.bytes_field(TETRAD_BLOCKS_FIELD), None);
    assert_eq!(preset.to_bytes(), bytes);
}

/// Restore ignores an unknown record version and refuses malformed records without a change.
#[test]
fn restore_ignores_new_versions_and_refuses_malformed_records() {
    let (_, bytes) = written();
    for (json, malformed) in [
        (&br#"{"version":2,"blocks":"later"}"#[..], false),
        (br#"{"version":1,"blocks":[{"row":1}]}"#, true),
        (
            br#"{"version":1,"blocks":[{"row":1,"column":3,"placeholder":10002,"key":"k","model":"!"}]}"#,
            true,
        ),
    ] {
        let mut preset = load(&bytes);
        preset
            .unknown
            .set_bytes_field(TETRAD_BLOCKS_FIELD, Some(json.to_vec()))
            .unwrap();
        let before = preset.clone();
        match restore(&mut preset, |_| Some(COMMUNITY)) {
            Ok(missing) => assert!(!malformed && missing.is_empty()),
            Err(error) => assert!(malformed && matches!(error, Error::CommunityRecords(_))),
        }
        assert_eq!(preset, before);
    }
}

/// The user replaces a restored community block with the same stock model as its placeholder
/// and saves: the old block must not come back at the next load.
#[test]
fn a_replaced_block_does_not_come_back() {
    let (_, bytes) = written();
    let mut preset = load(&bytes);
    assert!(
        restore(&mut preset, |_| Some(COMMUNITY))
            .unwrap()
            .is_empty()
    );
    let chosen = Model::block(PLACEHOLDER, [0.9, 0.2, 0.3].map(ParamValue::Float));
    preset.chains[1].models[3] = chosen.clone();
    stash(&mut preset, stashes).unwrap();
    assert!(preset.unknown.bytes_field(TETRAD_BLOCKS_FIELD).is_none());
    let mut loaded = load(&preset.to_bytes());
    assert!(
        restore(&mut loaded, |_| Some(COMMUNITY))
            .unwrap()
            .is_empty()
    );
    assert_eq!(loaded.chains[1].models[3], chosen);
}
