use std::path::PathBuf;

use tetrad_media::factory::{
    DeviceCipher, DeviceSerial, FACTORY_IR_BYTES, FactoryIrError, FactoryIrSeed, SEED_LEN,
};
use tetrad_media::ir::{IrProvenance, IrSourceFormat, MAX_IR_SAMPLES};

// Synthetic seed and serial; not device material.
fn cipher() -> DeviceCipher {
    let seed: Vec<u8> = (0..SEED_LEN as u8)
        .map(|byte| byte.wrapping_mul(37))
        .collect();
    DeviceCipher::seed_and_serial(
        &FactoryIrSeed::new(&seed).unwrap(),
        &DeviceSerial::parse(b"QC1A2B3C4\n").unwrap(),
    )
}

fn encrypted(words: impl Fn(usize) -> f32) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..MAX_IR_SAMPLES)
        .flat_map(|index| words(index).to_le_bytes())
        .collect();
    cipher().apply(&mut bytes);
    bytes
}

#[test]
fn factory_round_trip_restores_exact_words_with_factory_provenance() {
    let wave = |index: usize| (index as f32 * 0.37).sin() * 0.5;
    let samples = cipher().decode_factory_ir(&encrypted(wave)).unwrap();
    assert_eq!(samples.bits().len(), MAX_IR_SAMPLES);
    assert!(!samples.is_truncated());
    assert!(samples.samples().enumerate().all(|(i, s)| s == wave(i)));
    assert_eq!(samples.provenance(), IrProvenance::FactoryBin);
    assert_eq!(
        samples.source_format(),
        IrSourceFormat::FactoryMono48kFloat32
    );
    assert_eq!(samples.source_format().sample_rate_hz(), Some(48_000));
}

#[test]
fn factory_decode_refuses_wrong_size_and_wrong_key_noise() {
    assert_eq!(
        cipher().decode_factory_ir(&[0; FACTORY_IR_BYTES - 1]),
        Err(FactoryIrError::PayloadSize(FACTORY_IR_BYTES - 1))
    );
    let other = DeviceCipher::seed_and_serial(
        &FactoryIrSeed::new(&[7; SEED_LEN]).unwrap(),
        &DeviceSerial::parse(b"QC1A2B3C4").unwrap(),
    );
    let bytes = encrypted(|index| if index == 0 { 1.0 } else { 0.0 });
    assert!(matches!(
        other.decode_factory_ir(&bytes),
        Err(FactoryIrError::Implausible { .. })
    ));
    let loud = encrypted(|index| if index == 9 { f32::NAN } else { 0.0 });
    assert!(matches!(
        cipher().decode_factory_ir(&loud),
        Err(FactoryIrError::Implausible { index: 9, .. })
    ));
}

#[test]
fn seed_and_serial_inputs_are_validated_and_never_debug_printed() {
    assert_eq!(
        FactoryIrSeed::new(&[0; SEED_LEN + 1]).unwrap_err(),
        FactoryIrError::SeedLength(SEED_LEN + 1)
    );
    let seed_offset = tetrad_firmware::running().zenui.media.cipher_seed;
    assert_eq!(
        FactoryIrSeed::from_stock_zenui(&vec![0; seed_offset]).unwrap_err(),
        FactoryIrError::ZenUiTooShort(seed_offset)
    );
    for bad in [&b"QC1A2B3C"[..], b"QC1A2B3C45", b"QC1A2B3C-", b"QC1A 2B3C"] {
        assert_eq!(DeviceSerial::parse(bad), Err(FactoryIrError::Serial));
    }
    let serial = DeviceSerial::parse(b"  QC1A2B3C4\r\n").unwrap();
    assert_eq!(serial.as_str(), "QC1A2B3C4");
    let shown = format!(
        "{serial:?} {:?} {:?}",
        cipher(),
        FactoryIrSeed::new(&[0x41; SEED_LEN])
    );
    assert!(!shown.contains("QC1A2B3C4") && !shown.contains("AAAA") && !shown.contains("65"));
}

#[test]
fn key_modes_build_the_stock_secrets() {
    let seed_bytes = [0x22; SEED_LEN];
    let seed = FactoryIrSeed::new(&seed_bytes).unwrap();
    let serial = DeviceSerial::parse(b"QC1A2B3C4").unwrap();
    let mut seed_and_serial_bytes = seed_bytes.to_vec();
    seed_and_serial_bytes.extend_from_slice(b"QC1A2B3C4");
    let plain = [0x5a_u8; 64];
    assert_eq!(
        encrypt(&DeviceCipher::seed(&seed), &plain),
        encrypt(&DeviceCipher::caller_secret(&seed_bytes), &plain)
    );
    assert_eq!(
        encrypt(&DeviceCipher::seed_and_serial(&seed, &serial), &plain),
        encrypt(&DeviceCipher::caller_secret(&seed_and_serial_bytes), &plain)
    );
    assert_ne!(
        encrypt(&DeviceCipher::seed(&seed), &plain),
        encrypt(&DeviceCipher::seed_and_serial(&seed, &serial), &plain)
    );
}

#[test]
fn caller_secret_matches_the_ndsp_known_answer() {
    let cipher = DeviceCipher::caller_secret(b"test-secret-123");
    assert_eq!(
        hex(&encrypt(&cipher, b"Quad Cortex known-answer test!!!")),
        "06376ded0e818033a8e0b27a2f9aefbe9144ce4e8ab2a76706aa6efe695bb634"
    );
}

fn encrypt(cipher: &DeviceCipher, plain: &[u8]) -> Vec<u8> {
    let mut bytes = plain.to_vec();
    cipher.apply(&mut bytes);
    bytes
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [*byte >> 4, *byte & 0x0f])
        .map(|nibble| char::from_digit(u32::from(nibble), 16).unwrap())
        .collect()
}

/// Local corpus check: decrypts every stock factory IR with the device's own
/// seed and serial. Paths are injected through the environment; nothing about
/// the key material is printed.
#[test]
#[ignore = "reads ZenUI, qc_sn and the factory IRs in $QC_CORPUS_DIR/rootfs"]
fn local_factory_corpus_decrypts_to_plausible_irs() {
    let Some(rootfs) =
        std::env::var_os("QC_CORPUS_DIR").map(|dir| PathBuf::from(dir).join("rootfs"))
    else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return;
    };
    let zenui = std::fs::read(rootfs.join("opt/neuraldsp/ZenUI")).unwrap();
    let serial = std::fs::read(rootfs.join("media/p4/qc_sn")).unwrap();
    let cipher = DeviceCipher::seed_and_serial(
        &FactoryIrSeed::from_stock_zenui(&zenui).unwrap(),
        &DeviceSerial::parse(&serial).unwrap(),
    );
    let dir = rootfs.join("opt/neuraldsp/impulse_responses");
    let mut decoded = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        cipher.decode_factory_ir(&bytes).unwrap();
        decoded += 1;
    }
    assert_eq!(decoded, 11_521);
}
