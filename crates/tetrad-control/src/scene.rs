//! Bounded SceneMessage decoding and local state dispatch, not a transport or
//! a stock response encoder. Unknown fields remain in the original envelope.

use crate::wire::Cursor;
use crate::{Envelope, Error, Result, SceneCommand, SceneState};

pub const SCENE_MESSAGE_TYPE: u32 = 13;
pub const MAX_SCENE_GROUP_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct SceneLimits {
    pub max_payload_bytes: usize,
    pub max_fields: usize,
    pub max_group_depth: usize,
}

impl Default for SceneLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: 16 * 1024,
            max_fields: 1024,
            max_group_depth: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SceneWireError {
    #[error("truncated field")]
    Truncated,
    #[error("varint exceeds 64 bits")]
    VarintOverflow,
    #[error("invalid field number")]
    FieldNumber,
    #[error("invalid wire type {0}")]
    WireType(u8),
    #[error("unexpected end-group")]
    UnexpectedEndGroup,
    #[error("mismatched end-group")]
    MismatchedEndGroup,
    #[error("length cannot be represented")]
    LengthOverflow,
}

/// Wire presence is retained, including explicit zero. Action is the raw varint;
/// absent action has the schema's default CREATE (0), which dispatch refuses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SceneRequest {
    pub action: Option<u64>,
    pub request_id: Option<u64>,
    pub selected_scene: Option<u32>,
}

impl SceneRequest {
    pub fn decode(payload: &[u8], limits: SceneLimits) -> Result<Self> {
        if limits.max_group_depth > MAX_SCENE_GROUP_DEPTH {
            return Err(Error::SceneDepthConfiguration(limits.max_group_depth));
        }
        if payload.len() > limits.max_payload_bytes {
            return Err(Error::ScenePayloadLimit(limits.max_payload_bytes));
        }
        let mut cursor = Cursor {
            payload,
            offset: 0,
            fields: 0,
            limits,
        };
        let mut request = Self::default();
        while cursor.offset < payload.len() {
            let (number, wire) = cursor.key()?;
            if wire == 0 && matches!(number, 1..=3) {
                let value = cursor.varint()?;
                match number {
                    1 => request.action = Some(value),
                    2 => request.request_id = Some(value),
                    3 => {
                        request.selected_scene = Some(
                            u32::try_from(value)
                                .map_err(|_| Error::SceneInteger { field: 3, value })?,
                        );
                    }
                    _ => unreachable!("known scene field"),
                }
            } else {
                cursor.skip(number, wire, 0)?;
            }
        }
        Ok(request)
    }
}

/// An in-process result, not a recovered acknowledgement or protobuf response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneOutcome {
    pub request_id: Option<u64>,
    pub selected_scene: u32,
}

impl SceneState {
    /// Decode the entire plain scene payload before mutation. Only READ (3) and
    /// UPDATE (1) are implemented. READ ignores its optional selected-scene field.
    pub fn dispatch(&mut self, envelope: &Envelope, limits: SceneLimits) -> Result<SceneOutcome> {
        if envelope.message_type != SCENE_MESSAGE_TYPE {
            return Err(Error::UnsupportedSceneType(envelope.message_type));
        }
        let request = SceneRequest::decode(envelope.plain_payload()?, limits)?;
        let command = match request.action.unwrap_or(0) {
            1 => SceneCommand::Update {
                selected_scene: request.selected_scene,
            },
            3 => SceneCommand::Read,
            action => return Err(Error::UnsupportedSceneAction(action)),
        };
        Ok(SceneOutcome {
            request_id: request.request_id,
            selected_scene: self.apply(command)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(payload: &[u8]) -> Envelope {
        Envelope {
            message_type: SCENE_MESSAGE_TYPE,
            transform: 0,
            decompress: 0,
            opaque: [0xa5, 0x5a],
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn saved_reports_reach_local_update_and_read_without_changing_the_envelope() {
        let source = envelope(&[8, 1, 16, 42, 24, 7]);
        let reports = source.reports(crate::Limits::default()).unwrap();
        let mut decoder = crate::Reassembler::new(crate::Limits::default());
        let decoded = decoder.push(&reports[0]).unwrap().unwrap();
        let mut state = SceneState::default();
        assert_eq!(
            state.dispatch(&decoded, SceneLimits::default()).unwrap(),
            SceneOutcome {
                request_id: Some(42),
                selected_scene: 7,
            }
        );
        let read = envelope(&[8, 3, 24, 0xff, 0xff, 0xff, 0xff, 0x0f]);
        assert_eq!(
            state.dispatch(&read, SceneLimits::default()).unwrap(),
            SceneOutcome {
                request_id: None,
                selected_scene: 7,
            }
        );
        assert_eq!(decoded, source);
        assert_eq!(decoded.reports(crate::Limits::default()).unwrap(), reports);
    }

    #[test]
    fn optional_zero_duplicates_overlong_encodings_and_u64_ids_are_retained() {
        let limits = SceneLimits::default();
        assert_eq!(
            SceneRequest::decode(&[], limits).unwrap(),
            SceneRequest::default()
        );
        let request = SceneRequest::decode(
            &[
                8, 1, 16, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 1, 24, 7, 24, 0x80,
                0,
            ],
            limits,
        )
        .unwrap();
        assert_eq!(request.request_id, Some(u64::MAX));
        assert_eq!(request.selected_scene, Some(0));
        let mut state = SceneState::default();
        state
            .apply(SceneCommand::Update {
                selected_scene: Some(7),
            })
            .unwrap();
        assert_eq!(
            state
                .dispatch(&envelope(&[8, 1, 16, 0]), limits)
                .unwrap()
                .selected_scene,
            0
        );
        assert_eq!(
            SceneRequest::decode(&[8, 0, 16, 0, 24, 0], limits).unwrap(),
            SceneRequest {
                action: Some(0),
                request_id: Some(0),
                selected_scene: Some(0),
            }
        );
    }

    #[test]
    fn unknown_fields_and_known_fields_with_other_wire_types_do_not_supply_values() {
        // Unknown group includes a field numbered 3. It is not the root scene.
        let payload = [
            8, 1, 24, 6, 0x4b, 24, 2, 0x4c, 0x1a, 1, 7, 0x51, 0, 0, 0, 0, 0, 0, 0, 0, 0x5d, 0, 0,
            0, 0,
        ];
        let request = SceneRequest::decode(&payload, SceneLimits::default()).unwrap();
        assert_eq!(request.selected_scene, Some(6));
        let mut state = SceneState::default();
        assert_eq!(
            state
                .dispatch(&envelope(&payload), SceneLimits::default())
                .unwrap()
                .selected_scene,
            6
        );
    }

    #[test]
    fn unsupported_and_malformed_requests_never_mutate_local_state() {
        let mut state = SceneState::default();
        state
            .apply(SceneCommand::Update {
                selected_scene: Some(5),
            })
            .unwrap();
        for payload in [
            &[][..],
            &[8, 0],
            &[8, 2],
            &[8, 72],
            &[8, 1, 24, 8],
            &[8, 1, 24, 2, 0],
            &[8, 1, 24, 2, 0x80],
            &[8, 1, 24, 2, 0x4b, 0x54],
            &[8, 1, 24, 2, 0x4c],
            &[8, 1, 24, 2, 0x4e],
            &[8, 1, 24, 2, 0x4a, 3, 1],
            &[8, 1, 24, 0x80, 0x80, 0x80, 0x80, 0x10],
            &[
                8, 1, 24, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2,
            ],
        ] {
            assert!(
                state
                    .dispatch(&envelope(payload), SceneLimits::default())
                    .is_err()
            );
            assert_eq!(state.apply(SceneCommand::Read).unwrap(), 5);
        }
        let mut request = envelope(&[8, 1, 24, 2]);
        request.message_type = 52;
        assert_eq!(
            state.dispatch(&request, SceneLimits::default()),
            Err(Error::UnsupportedSceneType(52))
        );
        request.message_type = SCENE_MESSAGE_TYPE;
        request.transform = 1;
        assert_eq!(
            state.dispatch(&request, SceneLimits::default()),
            Err(Error::UnsupportedTransform(1))
        );
        request.transform = 0;
        request.decompress = 1;
        assert_eq!(
            state.dispatch(&request, SceneLimits::default()),
            Err(Error::UnsupportedCompression)
        );
        assert_eq!(state.apply(SceneCommand::Read).unwrap(), 5);
    }

    #[test]
    fn byte_field_and_nested_group_limits_bound_decoding_before_mutation() {
        let mut state = SceneState::default();
        let request = envelope(&[8, 1, 24, 7]);
        let limits = SceneLimits {
            max_group_depth: MAX_SCENE_GROUP_DEPTH + 1,
            ..SceneLimits::default()
        };
        assert_eq!(
            state.dispatch(&request, limits),
            Err(Error::SceneDepthConfiguration(MAX_SCENE_GROUP_DEPTH + 1))
        );
        let limits = SceneLimits {
            max_payload_bytes: 3,
            ..SceneLimits::default()
        };
        assert_eq!(
            state.dispatch(&request, limits),
            Err(Error::ScenePayloadLimit(3))
        );
        let limits = SceneLimits {
            max_fields: 1,
            ..SceneLimits::default()
        };
        assert_eq!(
            state.dispatch(&request, limits),
            Err(Error::SceneFieldLimit(1))
        );
        let grouped = envelope(&[8, 1, 24, 7, 0x4b, 0x53, 0x54, 0x4c]);
        let limits = SceneLimits {
            max_group_depth: 1,
            ..SceneLimits::default()
        };
        assert_eq!(
            state.dispatch(&grouped, limits),
            Err(Error::SceneDepthLimit(1))
        );
        assert_eq!(state.apply(SceneCommand::Read).unwrap(), 0);
        let limits = SceneLimits {
            max_group_depth: 2,
            max_fields: 6,
            ..SceneLimits::default()
        };
        assert_eq!(state.dispatch(&grouped, limits).unwrap().selected_scene, 7);
    }
}
