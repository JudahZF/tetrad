use tetrad_media::factory::{DeviceCipher, DeviceSerial, FactoryIrSeed, SEED_LEN};
use tetrad_media::ir::{IrLimits, WavPolicy};
use tetrad_media::user::{
    DELETED_IR_LABEL, IrReference, IrReferenceParams, ProductRef, UserIrError, UserIrMetadata,
    UserIrProduct, dialog_name, display_name, name_variants, parse_product_list, product_id,
    product_list_text, reference_params,
};

const LIMITS: IrLimits = IrLimits {
    max_asset_bytes: 1 << 20,
    max_chunks: 16,
};

// Synthetic seed and serial; not device material.
fn cipher() -> DeviceCipher {
    DeviceCipher::seed_and_serial(
        &FactoryIrSeed::new(&[0x11; SEED_LEN]).unwrap(),
        &DeviceSerial::parse(b"ZZ0000001").unwrap(),
    )
}

/// Canonical 18-byte-fmt float WAV, the layout the stock extractor expects.
fn wav(samples: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..samples)
        .flat_map(|i| (i as f32 / samples as f32).to_le_bytes())
        .collect();
    let mut fmt = vec![3, 0, 1, 0];
    fmt.extend_from_slice(&48_000_u32.to_le_bytes());
    fmt.extend_from_slice(&192_000_u32.to_le_bytes());
    fmt.extend_from_slice(&[4, 0, 32, 0, 0, 0]);
    let mut body = b"WAVEfmt ".to_vec();
    body.extend_from_slice(&18_u32.to_le_bytes());
    body.extend_from_slice(&fmt);
    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(&data);
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    file
}

fn metadata(name: &str) -> UserIrMetadata {
    UserIrMetadata {
        name: name.into(),
        date: "2026-10-10T12:00:00Z".into(),
        cloud_id: String::new(),
        coros_version: "4.1.0".into(),
    }
}

#[test]
fn product_id_is_md5_hex_without_zero_padding() {
    // MD5("") = d4 1d 8c d9 8f 00 b2 04 e9 80 09 98 ec f8 42 7e.
    assert_eq!(product_id(b""), "CIR_d41d8cd98f0b24e980998ecf8427e");
}

#[test]
fn wav_import_truncates_and_round_trips_through_an_encrypted_row() {
    let product = UserIrProduct::import_wav(
        &wav(1500),
        WavPolicy::RecoveredHeader38,
        LIMITS,
        metadata("My \"Cab\" IR"),
    )
    .unwrap();
    assert_eq!(product.samples.bits().len(), 1024);
    assert!(product.samples.is_truncated());
    assert_eq!(product.id, product_id(&product.rawdata()));

    let blob = product.metadata.to_blob(&cipher());
    assert!(serde_json::from_slice::<serde_json::Value>(&blob).is_err());
    let row = UserIrProduct::from_row(&product.id, &product.rawdata(), &blob, &cipher()).unwrap();
    assert_eq!(row.samples.bits(), product.samples.bits());
    assert_eq!(row.metadata, product.metadata);

    assert_eq!(
        UserIrProduct::import_wav(&wav(4), WavPolicy::RecoveredHeader38, LIMITS, metadata("")),
        Err(UserIrError::EmptyName)
    );
    assert_eq!(
        UserIrProduct::from_row("CIR_x", &[0; 4100], &blob, &cipher()).unwrap_err(),
        UserIrError::RawdataLength(1025)
    );
}

#[test]
fn metadata_json_uses_the_stock_serializer_shape_and_reads_leniently() {
    assert_eq!(
        metadata("Cab").to_json(),
        r#"{"cloudId": "","corosVersion": "4.1.0","date": "2026-10-10T12:00:00Z","name": "Cab"}"#
    );
    let plain = br#"{"name":"Only name"}"#;
    let read = UserIrMetadata::from_blob(plain, &cipher()).unwrap();
    assert_eq!(read.name, "Only name");
    assert!(read.date.is_empty());
    assert_eq!(
        UserIrMetadata::from_json(br#"{"date":"x"}"#),
        Err(UserIrError::MetadataField("name"))
    );
}

#[test]
fn names_follow_the_rename_dialog_and_duplicate_lookup() {
    assert_eq!(dialog_name("  My \t  cab\n").unwrap(), "My cab");
    assert_eq!(dialog_name(" a b "), Err(UserIrError::NameLength(3)));
    assert_eq!(
        dialog_name(&"é".repeat(61)),
        Err(UserIrError::NameLength(61))
    );
    assert!(dialog_name(&"é".repeat(60)).is_ok());
    assert_eq!(name_variants("cab one"), ["cab one", "Cab one", "cab one"]);
}

#[test]
fn preset_references_classify_and_label_like_the_selector() {
    assert_eq!(IrReference::classify(""), IrReference::Empty);
    for custom in [
        "CIR_abc",
        "CustomIR",
        "/media/p4/CustomIRs/Impulse Responses/x",
    ] {
        assert_eq!(IrReference::classify(custom), IrReference::Custom(custom));
    }
    assert_eq!(
        IrReference::classify("ML_412 Cali_Dynamic 57"),
        IrReference::Factory {
            tag: "ML",
            cab: "412 Cali",
            mic: "Dynamic 57"
        }
    );
    assert_eq!(
        IrReference::classify("ML_412_Cali_Dynamic"),
        IrReference::Unexpected("ML_412_Cali_Dynamic")
    );
    assert_eq!(display_name(Some("Found"), "Stored"), "Found");
    assert_eq!(display_name(None, "Stored"), "Stored");
    assert_eq!(display_name(None, ""), DELETED_IR_LABEL);

    let params = |id, name| Some(IrReferenceParams { id, name });
    assert_eq!(reference_params(12, 12000), params([1, 9], [19, 20]));
    assert_eq!(reference_params(32, 32100), params([1, 9], [29, 30]));
    assert_eq!(reference_params(12, 12115), params([1, 9], [26, 27]));
    assert_eq!(reference_params(29, 29004), params([2, 10], [22, 23]));
    assert_eq!(reference_params(3, 3008), params([15, 23], [42, 43]));
    assert_eq!(reference_params(3, 3001), None);
}

#[test]
fn favorites_and_recents_are_line_pairs() {
    let entries = vec![ProductRef {
        folder_key: "folder".into(),
        product_id: "CIR_1".into(),
    }];
    let text = product_list_text(&entries);
    assert_eq!(text, "folder\nCIR_1\n");
    assert_eq!(parse_product_list(&text).unwrap(), entries);
    assert_eq!(
        parse_product_list("folder\nCIR_1\nleftover\n"),
        Err(UserIrError::CorruptList)
    );
}
