//! Screenshot (25) over the owned render path.

use tetrad_catalog::Specs;
use tetrad_control::codecs::{CortexMessageType, MessageAction, ScreenshotMessage};
use tetrad_control::device::{DeviceError as ModelError, DeviceResult};
use tetrad_engine::{AbsentScalars, SceneIndex, import_layout};
use tetrad_preset::{Preset, PresetDocument};
use tetrad_ui::{PngColour, UiSnapshot, png};

use super::super::State;
use super::super::preset::{self, DeviceCatalog};
use super::super::ui::project::project_grid;

/// The offscreen grid of a stored preset as PNG (`0x691040` grabbed by `0x692260`; the same
/// item serves Screenshot (25) and the Directory upload image): migrated, in its default
/// scene, without a deploy (`tetrad_ui::UiState::preset_grid`), RGBA.
pub(in crate::device) fn grid_png(specs: &Specs, mut stored: Preset) -> Result<Vec<u8>, String> {
    stored
        .migrate(&DeviceCatalog(specs))
        .map_err(|error| error.to_string())?;
    let document = PresetDocument::parse(&stored.to_bytes(), tetrad_preset::Limits::default())
        .map_err(|error| error.to_string())?;
    let summary = document.summary().map_err(|error| error.to_string())?;
    let imported = import_layout(&summary, specs, AbsentScalars::Proto3Default)
        .map_err(|error| error.to_string())?;
    let scene = (stored.default_scene)
        .and_then(|scene| u8::try_from(scene).ok())
        .and_then(|scene| SceneIndex::new(scene).ok())
        .unwrap_or(SceneIndex::new(0).expect("scene 0"));
    // The device grid projection (`device/ui/project.rs`), without the live preset.
    let mut snapshot = UiSnapshot::default();
    project_grid(
        &mut snapshot,
        &imported.layout,
        &stored.chains,
        scene,
        Some(specs),
    );
    let rgba = tetrad_ui::UiState::preset_grid(snapshot).map_err(|error| error.to_string())?;
    png(
        tetrad_ui::PRESET_GRID_WIDTH,
        tetrad_ui::PRESET_GRID_HEIGHT,
        PngColour::Rgba,
        &rgba,
    )
    .ok_or_else(|| "the grid render has the wrong size".to_owned())
}

fn refused(what: impl Into<String>) -> ModelError {
    ModelError::Refused(CortexMessageType::Screenshot, what.into())
}

/// Screenshot READ `0x1b6058`: the stored preset at `folder_name`/`index` (the factory slots
/// when `is_factory`; index below 256, slot occupied) as the offscreen grid
/// (`tetrad_ui::UiState::preset_grid`, `0x691040`), in its default scene, without a deploy. The
/// reply `0x1b6a5c` is UPDATE {`png`, `is_factory`, `folder_name` (not factory), `index`}.
/// Stock renders later and sends it with the request's ID; here the render runs inside the
/// request, so the push carries that ID too (O).
pub(super) fn screenshot(
    state: &State,
    request: &ScreenshotMessage,
) -> DeviceResult<ScreenshotMessage> {
    let index = request.index.unwrap_or(0);
    let slot = usize::try_from(index)
        .ok()
        .filter(|&slot| slot < 256)
        .ok_or_else(|| refused(format!("preset index {index}")))?;
    let factory = request.is_factory == Some(true);
    let folder = request.folder_name.as_deref().unwrap_or_default();
    let library = (state.library.as_ref()).ok_or_else(|| refused("no preset library"))?;
    let specs = state.specs.as_ref().ok_or_else(|| refused("no catalog"))?;
    let slots = if factory {
        library.presets.factory_slots()
    } else {
        library.presets.slots(folder)
    }
    .map_err(|error| refused(error.to_string()))?;
    let path = (slots.get(slot).cloned().flatten())
        .ok_or_else(|| refused(format!("slot {slot} of {folder:?} is empty")))?;
    let stored = preset::read(&library.presets, Some(&path), specs)
        .map_err(|error| refused(error.to_string()))?;
    let png = grid_png(specs, stored).map_err(refused)?;
    Ok(ScreenshotMessage {
        action: Some(MessageAction::Update.into()),
        folder_name: (!factory).then(|| folder.to_owned()),
        is_factory: Some(factory),
        index: Some(index),
        png: Some(png),
        ..Default::default()
    })
}
