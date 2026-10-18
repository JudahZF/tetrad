use tetrad_media::capture_store::translate_capture;
use tetrad_media::factory::DeviceCipher;

/// `0x006a60e8`: metadata always moves to the target cipher; rawdata moves
/// only for `"version": "1"` records. Synthetic secrets, not device keys.
#[test]
fn translation_moves_rawdata_only_for_version_one_records() {
    let global = DeviceCipher::caller_secret(b"synthetic global");
    let local = DeviceCipher::caller_secret(b"synthetic local");
    let encrypt = |cipher: &DeviceCipher, plain: &[u8]| {
        let mut bytes = plain.to_vec();
        cipher.apply(&mut bytes);
        bytes
    };
    let raw = b"capture model bytes".to_vec();
    for (version, raw_moves) in [("1", true), ("2", false)] {
        let plain = format!(r#"{{"name": "Amp","version": "{version}"}}"#).into_bytes();
        let mut metadata = encrypt(&global, &plain);
        let mut rawdata = encrypt(&global, &raw);
        let moved = translate_capture(&global, &local, &mut rawdata, &mut metadata);
        assert_eq!(moved, raw_moves);
        assert_eq!(metadata, encrypt(&local, &plain));
        let expected = if raw_moves { &local } else { &global };
        assert_eq!(rawdata, encrypt(expected, &raw));
    }
}
