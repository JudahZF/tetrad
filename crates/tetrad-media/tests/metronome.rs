use tetrad_media::metronome::{
    METRONOME_SOUND_DIR, MetronomeSoundError, STOCK_SOUND_FILES, load_stock_sounds,
};

#[test]
fn stock_sounds_load_in_bundle_order_with_missing_files_as_empty() {
    let mut requested = Vec::new();
    let sounds = load_stock_sounds(
        |path| {
            requested.push(path.to_owned());
            (!path.ends_with("Normal C.bin")).then(|| vec![0; 2 * requested.len()])
        },
        1 << 20,
    )
    .unwrap();
    assert_eq!(requested.len(), 18);
    assert_eq!(
        requested[16],
        format!("{METRONOME_SOUND_DIR}/TK Brush Normal C.bin")
    );
    assert_eq!(STOCK_SOUND_FILES[17], "TK Brush Accent 2 C.bin");
    assert!(sounds[16].missing && sounds[16].bytes.is_empty());
    assert_eq!(sounds[17].bytes.len(), 36);
}

#[test]
fn odd_or_oversized_sounds_are_refused_before_bundling() {
    assert_eq!(
        load_stock_sounds(|_| Some(vec![0; 3]), 16),
        Err(MetronomeSoundError::OddLength {
            file: STOCK_SOUND_FILES[0],
            bytes: 3
        })
    );
    assert!(matches!(
        load_stock_sounds(|_| Some(vec![0; 18]), 16),
        Err(MetronomeSoundError::TooLarge { .. })
    ));
}
