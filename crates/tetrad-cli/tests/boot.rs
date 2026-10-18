use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(0);

const SEGMENT_VA: u32 = 0x12000;
const SEGMENT_OFFSET: u32 = 0x1000;
const FILE_END_VA: u32 = 0x15100;
const CONFIG_CORE1: u32 = 0x123c0;
const CONFIG_CORE2: u32 = 0x15040;
const NAME_CORE1: u32 = 0x13000;
const NAME_CORE2: u32 = 0x13100;
const WORDS_SOURCE: u32 = 0x13200;
const APP_SOURCE: u32 = 0x13300;

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "tetrad-boot-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tetrad-cli"))
            .arg("boot")
            .args(args)
            .output()
            .unwrap()
    }

    /// Writes the three synthetic images, then runs `boot plan` with `extra` flags.
    fn plan(&self, extra: &[&str]) -> Value {
        let loadsharc = self.root.join("loadsharc.elf");
        let core1 = self.root.join("core1.ldr");
        let core2 = self.root.join("core2.ldr");
        fs::write(&loadsharc, stock_shaped()).unwrap();
        fs::write(&core1, kernel(0xac, 32)).unwrap();
        fs::write(&core2, kernel(0xab, 32)).unwrap();
        let mut args = vec![
            "plan",
            "--loadsharc",
            loadsharc.to_str().unwrap(),
            "--core1",
            core1.to_str().unwrap(),
            "--core2",
            core2.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let output = self.cli(&args);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Invented loadsharc ELF: one PT_LOAD and two config tables, each with a
/// `SharcBooter_Core<n>.dxe` name section, one word-swapped section and one app-start section.
struct Elf(Vec<u8>);

impl Elf {
    fn new() -> Self {
        let filesz = FILE_END_VA - SEGMENT_VA;
        let mut elf = Self(vec![0; (SEGMENT_OFFSET + filesz) as usize]);
        elf.write(0, b"\x7fELF\x01\x01\x01\x00");
        elf.write(0x10, &2_u16.to_le_bytes());
        elf.write(0x12, &40_u16.to_le_bytes());
        elf.write(0x1c, &0x34_u32.to_le_bytes());
        elf.write(0x2a, &32_u16.to_le_bytes());
        elf.write(0x2c, &1_u16.to_le_bytes());
        let segment = [1, SEGMENT_OFFSET, SEGMENT_VA, SEGMENT_VA, filesz, filesz];
        for (index, word) in segment.into_iter().enumerate() {
            elf.write(0x34 + 4 * index, &word.to_le_bytes());
        }
        elf
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) {
        self.0[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn put(&mut self, va: u32, bytes: &[u8]) {
        self.write((SEGMENT_OFFSET + va - SEGMENT_VA) as usize, bytes);
    }

    fn put_words(&mut self, va: u32, words: &[u32]) {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        self.put(va, &bytes);
    }

    /// Seven config words, then `(target, source VA, size)` per section.
    fn table(&mut self, config: u32, mailbox: u32, app_start: u32, sections: &[(u32, u32, u32)]) {
        let words = [
            mailbox,
            mailbox + 0xa74,
            mailbox + 0xa70,
            mailbox + 0xa6c,
            mailbox + 0xa68,
            app_start,
            sections.len() as u32,
        ];
        self.put_words(config, &words);
        let entries: Vec<u32> = sections
            .iter()
            .flat_map(|&(target, source, size)| [target, source, size])
            .collect();
        self.put_words(config + 0x1c, &entries);
    }
}

fn stock_shaped() -> Vec<u8> {
    let mut elf = Elf::new();
    elf.put(NAME_CORE1, b"SharcBooter_Core1.dxe\0");
    elf.put(NAME_CORE2, b"SharcBooter_Core2.dxe\0");
    elf.put(WORDS_SOURCE, &[1, 2, 3, 4, 5, 6, 7, 8]);
    elf.put(APP_SOURCE, &[0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6]);
    elf.table(
        CONFIG_CORE1,
        0x200b_c330,
        0x00bb_d810,
        &[
            (0x200b_b008, NAME_CORE1, 22),
            (0x0802_ec00, WORDS_SOURCE, 5),
            (0x00bb_d810, APP_SOURCE, 6),
        ],
    );
    elf.table(
        CONFIG_CORE2,
        0x200a_e330,
        0x00bb_6810,
        &[
            (0x200a_e008, NAME_CORE2, 22),
            (0x0802_ec00, WORDS_SOURCE, 5),
            (0x00bb_6810, APP_SOURCE, 6),
        ],
    );
    elf.0
}

/// FIRST|IGNORE header, one literal of `payload` bytes, one fill, FINAL.
fn kernel(signature: u8, payload: usize) -> Vec<u8> {
    let header = |flags: u16, target: u32, count: u32, argument: u32| {
        let mut raw = [0_u8; 16];
        raw[..2].copy_from_slice(&flags.to_le_bytes());
        raw[3] = signature;
        raw[4..8].copy_from_slice(&target.to_le_bytes());
        raw[8..12].copy_from_slice(&count.to_le_bytes());
        raw[12..].copy_from_slice(&argument.to_le_bytes());
        raw[2] = raw.iter().fold(0, |xor, byte| xor ^ byte);
        raw
    };
    let final_offset = (16 + 16 + payload + 16) as u32;
    let mut bytes = header(0x5001, 0x0016_1894, 0, final_offset).to_vec();
    bytes.extend(header(0x0001, 0x2824_0000, payload as u32, 0));
    bytes.extend((0..payload).map(|index| index as u8));
    bytes.extend(header(0x0101, 0x2825_0000, 0x40, 0));
    bytes.extend(header(0x8001, 0x0016_1894, 0, 0));
    bytes
}

fn kind(step: &Value) -> &str {
    step["kind"].as_str().unwrap()
}

#[test]
fn plan_lists_core1_uploads_in_order_and_quiesces_only_on_restart() {
    let scratch = Scratch::new();

    let first = scratch.plan(&[]);
    assert_eq!(first["execution"], "none; plan only");
    assert_eq!(first["start"], "First");
    assert_eq!(first["phases"]["quiesce"], json!([]));
    let core1 = first["phases"]["core1"].as_array().unwrap();
    let uploads: Vec<(u64, u64)> = core1
        .iter()
        .filter(|step| kind(step) == "upload")
        .map(|step| {
            (
                step["address"].as_u64().unwrap(),
                step["length"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        uploads,
        [(0x200b_b008, 22), (0x200b_b000, 8), (0x200b_b020, 6)]
    );
    let tail: Vec<&str> = core1.iter().rev().take(2).map(kind).collect();
    assert_eq!(tail, ["arguments", "stream"]);

    let restart = scratch.plan(&["--restart", "--variant", "atma"]);
    assert_eq!(restart["variant"], "Atma");
    let quiesce = restart["phases"]["quiesce"].as_array().unwrap();
    let last_two = &quiesce[quiesce.len() - 2..];
    assert_eq!(last_two[0]["core"], "Core1");
    assert_eq!(last_two[0]["action"], "Stop");
    assert_eq!(last_two[1]["core"], "Core2");
    assert_eq!(last_two[1]["action"], "Stop");
}
