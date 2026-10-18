use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tetrad_media::factory::{
    DeviceCipher, DeviceSerial, FACTORY_IR_BYTES, FactoryIrSeed, SEED_LEN,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    input: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-ir-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let input = root.join("input");
        let output = root.join("output");
        fs::write(&input, bytes).unwrap();
        Self {
            root,
            input,
            output,
        }
    }

    fn run(&self, export: bool, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"));
        command
            .args(["ir", if export { "export-payload" } else { "inspect" }])
            .arg(&self.input)
            .args(args);
        if export {
            command.arg("--output").arg(&self.output);
        }
        command.output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn report(result: Output) -> Value {
    assert!(result.status.success(), "{:?}", result);
    serde_json::from_slice(&result.stdout).unwrap()
}

fn wav(fmt_size: u32, partial: bool) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    bytes.extend(fmt_size.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48_000_u32.to_le_bytes());
    bytes.extend(192_000_u32.to_le_bytes());
    bytes.extend(4_u16.to_le_bytes());
    bytes.extend(32_u16.to_le_bytes());
    if fmt_size == 18 {
        bytes.extend(0_u16.to_le_bytes());
    }
    bytes.extend(b"data");
    bytes.extend(if partial { 5_u32 } else { 4_u32 }.to_le_bytes());
    bytes.extend(0x8000_0000_u32.to_le_bytes());
    if partial {
        bytes.push(0xab);
    }
    let size = u32::try_from(bytes.len() - 8).unwrap();
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
    bytes
}

#[test]
fn raw_inspection_and_export_preserve_bits_without_disclosing_samples() {
    let bytes: Vec<u8> = [0x8000_0000_u32, 0x7fc0_1234, 0xff80_0000, 1]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let fixture = Fixture::new(&bytes);
    let inspect = report(fixture.run(false, &["--format", "raw-le-f32", "--channel", "1"]));
    assert_eq!(inspect["classification"], "raw_le_f32");
    assert_eq!(inspect["sample_rate_hz"], Value::Null);
    assert_eq!(inspect["source_channel_count"], Value::Null);
    assert_eq!(inspect["original_sample_count"], 4);
    assert_eq!(inspect["selected_sample_count"], 4);
    assert_eq!(inspect["truncated"], false);
    assert_eq!(inspect["classification_authenticated"], false);
    assert_eq!(inspect["audio_validated"], false);
    assert_eq!(inspect["hardware_compatibility"], "unverified");
    assert_eq!(inspect["payload_exported"], false);
    assert_eq!(
        inspect["transfer_plan"],
        json!({
            "parameter_id": 11, "channel": 1, "sample_count": 4,
            "byte_count": 16, "final_flag": 1, "inert": true, "sent": false
        })
    );
    assert!(inspect.get("bits").is_none());
    assert!(inspect.get("samples").is_none());
    assert!(!fixture.output.exists());
    let exported = report(fixture.run(true, &["--format", "raw-le-f32"]));
    assert_eq!(exported["payload_exported"], true);
    assert_eq!(fs::read(&fixture.output).unwrap(), bytes);
    assert_eq!(fs::read(&fixture.input).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fixture.output.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn empty_and_truncated_exports_have_exact_selected_payload_lengths() {
    for count in [0_usize, 1025] {
        let bytes: Vec<u8> = (0..count)
            .flat_map(|i| u32::try_from(i).unwrap().to_le_bytes())
            .collect();
        let fixture = Fixture::new(&bytes);
        let result = report(fixture.run(true, &["--format", "raw-le-f32"]));
        assert_eq!(result["original_sample_count"], count);
        assert_eq!(result["selected_sample_count"], count.min(1024));
        assert_eq!(result["truncated"], count > 1024);
        assert_eq!(
            fs::read(&fixture.output).unwrap(),
            bytes[..count.min(1024) * 4]
        );
        assert_eq!(fs::read(&fixture.input).unwrap(), bytes);
    }
}

#[test]
fn wav_policies_are_explicit_and_report_partial_bytes_separately() {
    for (fmt_size, partial, policy, success) in [
        (16, false, "wav-standard", true),
        (16, false, "wav-recovered38", false),
        (18, true, "wav-recovered38", true),
        (18, true, "wav-standard", false),
    ] {
        let bytes = wav(fmt_size, partial);
        let fixture = Fixture::new(&bytes);
        let result = fixture.run(true, &["--format", policy]);
        if success {
            let report = report(result);
            assert_eq!(report["sample_rate_hz"], 48_000);
            assert_eq!(report["source_channel_count"], 1);
            assert_eq!(report["ignored_partial_bytes"], usize::from(partial));
            assert_eq!(report["truncated"], false);
            assert_eq!(
                fs::read(&fixture.output).unwrap(),
                0x8000_0000_u32.to_le_bytes()
            );
        } else {
            assert_eq!(result.status.code(), Some(1));
            assert!(!fixture.output.exists());
            assert!(serde_json::from_slice::<Value>(&result.stderr).unwrap()["error"].is_string());
        }
        assert_eq!(fs::read(&fixture.input).unwrap(), bytes);
    }
}

#[test]
fn argument_errors_and_key_misuse_never_create_output() {
    let fixture = Fixture::new(&[]);
    for args in [
        vec![],
        vec!["--format", "auto"],
        vec!["--format", "raw-le-f32", "--channel", "2"],
    ] {
        assert_eq!(fixture.run(true, &args).status.code(), Some(2));
        assert!(!fixture.output.exists());
    }
    for args in [
        vec!["--format", "factory-bin"],
        vec!["--format", "factory-bin", "--seed-file", "missing"],
        vec!["--format", "raw-le-f32", "--seed-file", "missing"],
    ] {
        let result = fixture.run(true, &args);
        assert_eq!(result.status.code(), Some(1));
        assert!(
            serde_json::from_slice::<Value>(&result.stderr).unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("--seed-file")
        );
        assert!(!fixture.output.exists());
    }
}

#[test]
fn factory_input_decrypts_only_with_matching_seed_and_serial() {
    let seed = [0x42_u8; SEED_LEN];
    let serial = b"ABC123XYZ";
    let plain: Vec<u8> = (0..FACTORY_IR_BYTES / 4)
        .flat_map(|_| 0.25_f32.to_le_bytes())
        .collect();
    let mut encrypted = plain.clone();
    DeviceCipher::seed_and_serial(
        &FactoryIrSeed::new(&seed).unwrap(),
        &DeviceSerial::parse(serial).unwrap(),
    )
    .apply(&mut encrypted);
    let fixture = Fixture::new(&encrypted);
    let seed_path = fixture.root.join("seed");
    let serial_path = fixture.root.join("qc_sn");
    fs::write(&seed_path, seed).unwrap();
    fs::write(&serial_path, serial).unwrap();
    let key_args = [
        "--seed-file",
        seed_path.to_str().unwrap(),
        "--serial-file",
        serial_path.to_str().unwrap(),
    ];
    let mut args = vec!["--format", "factory-bin"];
    args.extend(key_args);

    let result = report(fixture.run(true, &args));
    assert_eq!(result["classification"], "factory_bin");
    assert_eq!(result["classification_authenticated"], false);
    assert_eq!(result["original_sample_count"], 1024);
    assert_eq!(fs::read(&fixture.output).unwrap(), plain);
    assert_eq!(fs::read(&fixture.input).unwrap(), encrypted);

    fs::write(&serial_path, b"WRONG0000").unwrap();
    fs::remove_file(&fixture.output).unwrap();
    let mut args = vec!["--format", "factory-bin"];
    args.extend(key_args);
    let result = fixture.run(true, &args);
    assert_eq!(result.status.code(), Some(1));
    assert!(
        serde_json::from_slice::<Value>(&result.stderr).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("wrong seed or serial")
    );
    assert!(!fixture.output.exists());
}

#[test]
fn malformed_oversized_and_nonfile_inputs_fail_before_export() {
    let fixture = Fixture::new(&[0, 0, 0, 0, 1]);
    assert_eq!(
        fixture.run(true, &["--format", "raw-le-f32"]).status.code(),
        Some(1)
    );
    assert!(!fixture.output.exists());
    let file = fs::File::create(&fixture.input).unwrap();
    file.set_len(16 * 1024 * 1024 + 1).unwrap();
    drop(file);
    assert_eq!(
        fixture.run(true, &["--format", "raw-le-f32"]).status.code(),
        Some(1)
    );
    assert!(!fixture.output.exists());
    fs::remove_file(&fixture.input).unwrap();
    fs::create_dir(&fixture.input).unwrap();
    assert_eq!(
        fixture.run(true, &["--format", "raw-le-f32"]).status.code(),
        Some(1)
    );
    assert!(!fixture.output.exists());
}

#[test]
fn export_refuses_existing_targets_and_input_aliases() {
    let bytes = 0x3f80_0000_u32.to_le_bytes();
    let fixture = Fixture::new(&bytes);
    fs::write(&fixture.output, b"keep").unwrap();
    assert_eq!(
        fixture.run(true, &["--format", "raw-le-f32"]).status.code(),
        Some(1)
    );
    assert_eq!(fs::read(&fixture.output).unwrap(), b"keep");
    fs::remove_file(&fixture.output).unwrap();
    fs::hard_link(&fixture.input, &fixture.output).unwrap();
    assert_eq!(
        fixture.run(true, &["--format", "raw-le-f32"]).status.code(),
        Some(1)
    );
    fs::remove_file(&fixture.output).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&fixture.input, &fixture.output).unwrap();
        assert_eq!(
            fixture.run(true, &["--format", "raw-le-f32"]).status.code(),
            Some(1)
        );
    }
    let result = Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
        .args(["ir", "export-payload"])
        .arg(&fixture.input)
        .args(["--format", "raw-le-f32", "--output"])
        .arg(&fixture.input)
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(fs::read(&fixture.input).unwrap(), bytes);
}
