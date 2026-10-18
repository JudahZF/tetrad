//! Live-performance rules: footswitch modes, scenes, stomps, expression and tap
//! tempo. Pure state and decisions; callers own HAL input, DSP sends and UI.

pub mod bypass;
pub mod expression;
pub mod footswitch;
pub mod leds;
pub mod mode;
pub mod scene;
pub mod stomp;
pub mod tempo;

pub use bypass::{BlockBypass, BypassOverwriteMode, ChangeSource, StompWrite};
pub use expression::{
    AutoEngage, BypassInfo, BypassSwitchType, ExpressionRange, Pedal, PedalCalibration, PedalState,
};
pub use footswitch::{
    ChordReleases, Footswitch, FootswitchAction, FootswitchContext, GigButton, HalStomp,
};
pub use mode::{FootswitchKind, FootswitchMode, UnallowedMode};
pub use scene::{BypassValue, RecallBlock, RecallStep, SCENES, Scened, recall_plan};
pub use stomp::{AssignmentType, LabelEdit, LabelMap, StompAssignment, StompIndex};
pub use tempo::{TapContext, TapOutcome, TapTempo, accept_tempo_event, hold_outcome};
