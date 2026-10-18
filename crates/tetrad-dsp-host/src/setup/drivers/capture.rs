//! Capture loads for models 14000 (`nc_player`, no stock driver) and 14001
//! (`zenUI::Capture2Driver`, ctor VA `0x566134`, `+0x30` `0x566118`).
//!
//! Stock loads the capture from the block push (VA `0x5174a8`, outside the
//! NC2 capture grid): `file_name` (index 5) cut to 64 UTF-16 units names a
//! capture library record (`0x334390` → `0x6f6484`). Version `"2"` records
//! go to a Capture2Driver as an empty ReceiveData then the blob (`0x5128f4`
//! → `0x5661ec`); other records go to the block placement as one
//! ReceiveData (`0x51cef8`). All use buffer 0; core 0/1 add `0x17`.

use std::sync::Arc;

use tetrad_dsp_protocol::DeploymentTarget;

use super::super::{DspWrite, ModelIdentity, ParamSetMode, SetupError};
use super::media::{CaptureData, MediaLibrary};
use super::{Driver, DriverKind};

/// `file_name` string parameter of both capture models.
pub const FILE_NAME: u32 = 5;
/// Built-in self-test captures (VAs `0x333a34`, `0x333ee0`); not recovered.
const SELF_TESTS: [&str; 2] = ["eltron_self_test", "soldano_self_test"];
/// `QString::left(64)` (VA `0x517cac`).
const KEY_UNITS: usize = 64;

/// The two writes of a Capture 2 load (VA `0x5661ec`), also used by the NC2
/// audition (`0x515a64`, block (0, 0) of the capture grid).
pub fn capture2_writes(target: DeploymentTarget, blob: Arc<[u8]>) -> [DspWrite; 2] {
    [
        DspWrite::Data {
            target,
            buffer: 0,
            payload: Arc::from([]),
        },
        DspWrite::Data {
            target,
            buffer: 0,
            payload: blob,
        },
    ]
}

/// The first 64 UTF-16 units. Owned: a surrogate pair is never split.
fn library_key(text: &str) -> &str {
    let mut units = 0;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            units > KEY_UNITS
        })
        .map_or(text.len(), |(at, _)| at);
    &text[..end]
}

#[derive(Debug)]
pub(super) struct CaptureLoad {
    target: DeploymentTarget,
    model_id: u32,
    /// Model 14001: a Capture2Driver exists, so V2 records load.
    capture2: bool,
    key: Option<String>,
    capture: Option<CaptureData>,
    pending: bool,
}

impl CaptureLoad {
    pub(super) fn new(identity: &ModelIdentity, capture2: bool) -> Self {
        Self {
            target: identity.target,
            model_id: identity.model_id,
            capture2,
            key: None,
            capture: None,
            pending: false,
        }
    }

    /// The pending push's capture (VA `0x334390`). A record the library lacks
    /// sends nothing (stock logs and continues).
    fn load_pending(&mut self, out: &mut Vec<DspWrite>) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        let target = self.target;
        match &self.capture {
            Some(CaptureData::V1(blob)) => out.push(DspWrite::Data {
                target,
                buffer: 0,
                payload: Arc::clone(blob),
            }),
            Some(CaptureData::V2(blob)) if self.capture2 => {
                out.extend(capture2_writes(target, Arc::clone(blob)));
            }
            // 0x5128f4: the cell has no Capture2Driver.
            Some(CaptureData::V2(_)) | None => {}
        }
    }
}

impl Driver for CaptureLoad {
    fn kind(&self) -> DriverKind {
        if self.capture2 {
            DriverKind::Capture2
        } else {
            DriverKind::NeuralCapture
        }
    }

    fn begin_push(&mut self) {
        self.pending = true;
    }

    /// Capture2Driver `+0x30` drops parameter 5; model 14000 has no driver.
    /// The first value of a push follows the bypass, so the capture load
    /// goes out just before it (VA `0x5174a8`).
    fn update_parameter(
        &mut self,
        index: u32,
        _value: f32,
        _mode: ParamSetMode,
        out: &mut Vec<DspWrite>,
    ) -> bool {
        self.load_pending(out);
        !self.capture2 || index != FILE_NAME
    }

    /// Carries the load of a push with no float value.
    fn parameters_pushed(&mut self, out: &mut Vec<DspWrite>) {
        self.load_pending(out);
    }

    fn loads_capture(&self) -> bool {
        true
    }

    /// `file_name` before the push. Stock reloads captures only in the push:
    /// a recall ignores the string (VA `0x526cc0`), and a live edit to a
    /// changed name is refused (not traced).
    fn update_text(
        &mut self,
        index: u32,
        text: &str,
        mode: ParamSetMode,
        library: &dyn MediaLibrary,
        _out: &mut Vec<DspWrite>,
    ) -> Result<(), SetupError> {
        if index != FILE_NAME || mode == ParamSetMode::Recall {
            return Ok(());
        }
        let refused = SetupError::CaptureLoad {
            model_id: self.model_id,
        };
        if SELF_TESTS.contains(&text) {
            return Err(refused);
        }
        let key = library_key(text);
        if mode == ParamSetMode::Live {
            return if self.key.as_deref() == Some(key) {
                Ok(())
            } else {
                Err(refused)
            };
        }
        self.capture = library.capture(key);
        self.key = Some(key.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tetrad_dsp_protocol::{DeploymentTarget, DspCore, ModuleSlot};

    use super::*;
    use crate::setup::drivers::media::test_media::TestMedia;
    use crate::setup::test_factory::build_with;
    use crate::setup::{Block, BlockBypass, SetupValue};

    fn identity(model_id: u32) -> ModelIdentity {
        ModelIdentity {
            target: DeploymentTarget {
                core: DspCore::Core1,
                slot: ModuleSlot::try_from(2).unwrap(),
            },
            model_id,
            category: 14,
            fx_loop_send: None,
        }
    }

    fn push(block: &mut Block, out: &mut Vec<DspWrite>) {
        let values = [SetupValue::Float(0.5_f32.to_bits()); 8];
        let mut values = values.to_vec();
        values[FILE_NAME as usize] = SetupValue::Text;
        block
            .push(&values, BlockBypass::default(), None, out)
            .unwrap();
    }

    /// 0x334390: V2 records load into 14001 as an empty write then the
    /// blob; V1 records into either model as one write; 14000 cannot take
    /// a V2 record (no Capture2Driver); the key is the first 64 units.
    #[test]
    fn push_loads_the_named_capture_by_record_version() {
        let mut media = TestMedia::default();
        let v2: Arc<[u8]> = Arc::from(&[7_u8; 12][..]);
        let v1: Arc<[u8]> = Arc::from(&[9_u8; 5][..]);
        let long = "a".repeat(64);
        media
            .captures
            .insert(long.clone(), CaptureData::V2(Arc::clone(&v2)));
        media
            .captures
            .insert("old".into(), CaptureData::V1(Arc::clone(&v1)));

        let mut out = Vec::new();
        let mut block = build_with(identity(14001), &[], &media, &mut out).unwrap();
        assert!(out.is_empty());
        let name = format!("{long}tail");
        block
            .set_text(FILE_NAME, &name, ParamSetMode::Push, &media, &mut out)
            .unwrap();
        push(&mut block, &mut out);
        let data: Vec<_> = out
            .iter()
            .filter_map(|w| match w {
                DspWrite::Data {
                    buffer, payload, ..
                } => Some((*buffer, payload.len())),
                _ => None,
            })
            .collect();
        assert_eq!(data, [(0, 0), (0, 12)]);
        // Parameter 5 never passes the Capture2Driver filter.
        assert!(
            !out.iter()
                .any(|w| matches!(w, DspWrite::Parameter { index: 5, .. }))
        );
        // A recall pushes nothing new.
        out.clear();
        block.recalled(&|_| None, &mut out);
        assert!(out.is_empty());

        let mut v1_block = build_with(identity(14000), &[], &media, &mut out).unwrap();
        assert_eq!(v1_block.driver_kind(), Some(DriverKind::NeuralCapture));
        v1_block
            .set_text(FILE_NAME, "old", ParamSetMode::Push, &media, &mut out)
            .unwrap();
        push(&mut v1_block, &mut out);
        assert!(out.contains(&DspWrite::Data {
            target: identity(14000).target,
            buffer: 0,
            payload: v1,
        }));

        out.clear();
        let mut wrong = build_with(identity(14000), &[], &media, &mut out).unwrap();
        wrong
            .set_text(FILE_NAME, &long, ParamSetMode::Push, &media, &mut out)
            .unwrap();
        push(&mut wrong, &mut out);
        assert!(!out.iter().any(|w| matches!(w, DspWrite::Data { .. })));
    }

    /// The load sits between the bypass and the values (VA `0x5174a8`), so
    /// the DSP has the capture before its first parameter, in bulk or not.
    #[test]
    fn capture_load_goes_between_the_bypass_and_the_values() {
        let mut media = TestMedia::default();
        let blob: Arc<[u8]> = Arc::from(&[7_u8; 12][..]);
        media.captures.insert("take".into(), CaptureData::V2(blob));
        let mut out = Vec::new();
        let mut block = build_with(identity(14001), &[], &media, &mut out).unwrap();
        block
            .set_text(FILE_NAME, "take", ParamSetMode::Push, &media, &mut out)
            .unwrap();
        push(&mut block, &mut out);

        let position = |matches: fn(&DspWrite) -> bool| out.iter().position(matches).unwrap();
        let bypass = position(|w| matches!(w, DspWrite::Bypass { .. }));
        let load = position(|w| matches!(w, DspWrite::Data { .. }));
        let value = position(|w| matches!(w, DspWrite::Parameter { .. }));
        assert!(bypass < load && load < value, "{out:?}");
    }

    /// Unrecovered capture paths are refused, never silently skipped. A
    /// recall ignores the name and keeps the loaded capture.
    #[test]
    fn live_capture_changes_are_refused_and_recall_ignores_the_name() {
        let media = TestMedia::default();
        let mut out = Vec::new();
        let mut block = build_with(identity(14001), &[], &media, &mut out).unwrap();
        let refused = Err(SetupError::CaptureLoad { model_id: 14001 });
        assert_eq!(
            block.set_text(
                FILE_NAME,
                "eltron_self_test",
                ParamSetMode::Push,
                &media,
                &mut out
            ),
            refused
        );
        block
            .set_text(FILE_NAME, "x", ParamSetMode::Push, &media, &mut out)
            .unwrap();
        assert_eq!(
            block.set_text(FILE_NAME, "x", ParamSetMode::Live, &media, &mut out),
            Ok(())
        );
        assert_eq!(
            block.set_text(FILE_NAME, "y", ParamSetMode::Live, &media, &mut out),
            refused
        );
        assert_eq!(
            block.set_text(FILE_NAME, "y", ParamSetMode::Recall, &media, &mut out),
            Ok(())
        );
        assert_eq!(
            block.set_text(FILE_NAME, "x", ParamSetMode::Live, &media, &mut out),
            Ok(())
        );
        assert!(out.is_empty());
    }
}
