//! Owned QC touch UI for the 800×480 display: screen framework (navigation
//! stack, touch gestures, scroll lists, keyboard, dialogs, toasts, knob and
//! footswitch routing), every screen, and the view models the application
//! fills. Commands are requests, never acknowledgements of loads or saves.
#![forbid(unsafe_code)]

mod actions;
mod boot_select;
mod control;
mod custom_mode;
mod damage;
mod draw;
mod extensions;
mod layout;
mod mappings;
mod png;
mod remote;
mod render;
mod screens;
mod settings;
mod state;
mod types;
mod view;
mod widgets;

pub use boot_select::{BootChoice, BootMenu};
pub use control::*;
pub use custom_mode::*;
pub use damage::Renderer;
pub use extensions::*;
pub use mappings::*;
pub use png::{PngColour, png};
pub use remote::{RemoteView, ToastView};
pub use render::{Frame, HEIGHT, PRESET_GRID_HEIGHT, PRESET_GRID_WIDTH, WIDTH};
pub use settings::{
    DeviceAction, InputJack, IoTab, LoopJack, OutputJack, SettingId, SettingKind, SettingValue,
    SettingsGroup, SettingsPage,
};
pub use settings::{EqBand, EqTab, ExcessDownloads};
pub use state::{HitRegion, UiState};
pub use types::*;
pub use view::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_frames;
#[cfg(test)]
mod tests_polish;
