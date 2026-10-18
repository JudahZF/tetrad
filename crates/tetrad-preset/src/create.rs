//! New presets (`0x0051a370`) and save-time stamping (`0x51898c`, storage
//! writers such as `0x711aa4`). Clock, account and storage path are inputs.

use tetrad_core::PresetId;

use crate::{
    COLUMNS, Error, ROWS, Result, SCENES,
    edit::DEFAULT_SCENE_COLORS,
    migrate::{
        INPUT_CONTROL_ID, MIDI_GENERAL_GROUPS, MIDI_GENERAL_SLOTS, MIDI_MESSAGES, MIXER_ID,
        ModelCatalog, OUTPUT_CONTROL_ID, TEMPO_MODEL_ID, default_model, lane_control,
        parse_version,
    },
    model::{
        BypassRow, Chain, ColBypass, MidiMessage, Model, Preset, SceneBypass, SplitControlPoints,
    },
};

/// Row 0 reads input port 1 and writes port 0x13; other rows store 0/0.
const FIRST_ROW_PORTS: (u32, u32) = (1, 0x13);
/// The parameter import gates end at 3.0.0 (`0x00680614`); a saved preset
/// with an older modified version would be imported a second time.
const IMPORT_GATE_END: tetrad_core::Version = tetrad_core::Version {
    major: 3,
    minor: 0,
    patch: 0,
};

/// Save-time inputs that stock takes from the clock, UI and account.
#[derive(Debug, Clone, Copy)]
pub struct SaveStamp<'a> {
    /// UTC `%FT%TZ`, for example `2026-10-10T12:00:00Z` (`0x006695c4`).
    pub date: &'a str,
    /// Becomes the saved default scene.
    pub selected_scene: usize,
    /// Used when the stored author name is empty.
    pub account_name: &'a str,
    pub account_id: &'a str,
    /// Full device path such as `/media/p4/Presets/<folder>/<name>.pb`. The
    /// hash is its identifier (`PresetId::from_identifier`).
    pub path: &'a str,
    /// Display name the storage layer records with the file.
    pub name: &'a str,
    /// Firmware version, stamped as modified and oldest-compatible version
    /// when the preset changed (stock does it at the first edit, VA 0x50ec54).
    pub changed_in: Option<&'a str>,
}

impl Preset {
    /// The stock empty preset. Lane controls (23000/28000), the pair splitter
    /// (10004), mixer (11000) and tempo model (25000) take catalog defaults.
    pub fn new_default(firmware_version: &str, catalog: &impl ModelCatalog) -> Result<Self> {
        let output_control = lane_control(OUTPUT_CONTROL_ID, catalog)?;
        let input_control = lane_control(INPUT_CONTROL_ID, catalog)?;
        let mut chains = Vec::with_capacity(ROWS);
        for row in 0..ROWS {
            let (in_portid, out_portid) = if row == 0 { FIRST_ROW_PORTS } else { (0, 0) };
            let mut chain = Chain {
                in_portid: Some(in_portid),
                out_portid: Some(out_portid),
                models: vec![Model::empty_slot(); COLUMNS],
                output_control: vec![output_control.clone()],
                input_control: vec![input_control.clone()],
                ..Chain::default()
            };
            if row % 2 == 0 {
                chain.split_control_points = vec![SplitControlPoints {
                    split: -1,
                    mix: -1,
                    ..SplitControlPoints::default()
                }];
                chain.combined_splitter = vec![default_model(
                    tetrad_core::LegacySplitterPlan::TARGET_MODEL_ID,
                    catalog,
                )?];
                chain.mixer = vec![default_model(MIXER_ID, catalog)?];
                chain.split_bypass = vec![SceneBypass::default(); SCENES];
                chain.mix_bypass = vec![SceneBypass::default(); SCENES];
            }
            chains.push(chain);
        }
        let version = vec![firmware_version.to_owned()];
        Ok(Self {
            name: Some(String::new()),
            hash: Some(0),
            volume: Some(1.0),
            pan: Some(0.5),
            default_scene: Some(0),
            author_name: Some(String::new()),
            author_id: Some(String::new()),
            chains,
            scene_labels: vec![String::new(); SCENES],
            midi_messages: vec![MidiMessage::default(); MIDI_MESSAGES],
            midi_messages_general: vec![MidiMessage::default(); MIDI_GENERAL_GROUPS],
            bypass: vec![
                BypassRow {
                    col_bypass: vec![ColBypass::empty_slot(); COLUMNS],
                    ..BypassRow::default()
                };
                ROWS
            ],
            tempo_program_data: vec![default_model(TEMPO_MODEL_ID, catalog)?],
            layout_code_1: vec![0],
            layout_code_2: vec![0],
            created_version: version.clone(),
            modified_version: version.clone(),
            oldest_compatible_version: version,
            cloud_id: vec![String::new()],
            description: vec![String::new()],
            recompile_code_1: vec![0],
            recompile_code_2: vec![0],
            scene_colors: DEFAULT_SCENE_COLORS.to_vec(),
            midi_messages_general_v2: vec![
                MidiMessage::default();
                MIDI_GENERAL_GROUPS * MIDI_GENERAL_SLOTS
            ],
            side_chain_follow_exists: Some(true),
            zsk_7182_applied: Some(true),
            ..Self::default()
        })
    }

    /// Gives the pair whose master is chain `master` the new-preset junction where its model
    /// is empty or absent: the combined splitter 10004 (`mixer` false) or the mixer 11000,
    /// with eight unbypassed scenes. Confirmed models: the stock junction editor builds its
    /// default splitter and mixer as 10004 and 11000 (`0x3dfefc`, `0x3de444` via the model
    /// factory `0x519cc0`). Inferred: an older preset without a junction model takes them
    /// when a split or mix point is placed. Stock also applies the model's user default
    /// parameters there (`0x668354`); the catalog defaults here do not. Returns whether the
    /// preset changed.
    pub fn default_junction(
        &mut self,
        master: usize,
        mixer: bool,
        catalog: &impl ModelCatalog,
    ) -> Result<bool> {
        let chain = self
            .chains
            .get_mut(master)
            .filter(|_| master.is_multiple_of(2))
            .ok_or(Error::Layout("a pair junction lives on an even chain"))?;
        let (models, bypass, id) = if mixer {
            (&mut chain.mixer, &mut chain.mix_bypass, MIXER_ID)
        } else {
            (
                &mut chain.combined_splitter,
                &mut chain.split_bypass,
                tetrad_core::LegacySplitterPlan::TARGET_MODEL_ID,
            )
        };
        if models.first().is_some_and(|model| !model.is_empty()) {
            return Ok(false);
        }
        let model = default_model(id, catalog)?;
        match models.first_mut() {
            Some(first) => *first = model,
            None => models.push(model),
        }
        bypass.resize(SCENES, SceneBypass::default());
        Ok(true)
    }

    /// Applies what the stock save pipeline writes besides the edited state:
    /// name, path identity, date, default scene, author fallback and versions.
    /// The momentary-stomp bypass reset (`0x51898c`) needs the held footswitches: the caller
    /// applies it to the copy it saves.
    ///
    /// The preset must be in loaded form ([`Preset::is_loaded_form`]), so the
    /// saved flags 36/37 are true only on migrated data, and the saved modified
    /// version must be 3.0.0 or later, so stock does not import the parameters
    /// again. Both refusals are owned policy.
    pub fn stamp_save(&mut self, stamp: &SaveStamp) -> Result<()> {
        if stamp.selected_scene >= SCENES {
            return Err(Error::Index {
                kind: "scene",
                index: stamp.selected_scene,
                len: SCENES,
            });
        }
        if !self.is_loaded_form() {
            return Err(Error::Unsupported("run migrate before save"));
        }
        let modified = stamp
            .changed_in
            .or_else(|| self.modified_version.first().map(String::as_str));
        if modified.is_none_or(|version| parse_version(version) < IMPORT_GATE_END) {
            return Err(Error::Unsupported(
                "a saved preset needs a modified version of 3.0.0 or later",
            ));
        }
        self.name = Some(stamp.name.to_owned());
        self.hash = Some(PresetId::from_identifier(stamp.path).0);
        self.date = Some(stamp.date.to_owned());
        self.default_scene = Some(stamp.selected_scene as u32);
        if self.author_name.as_deref().unwrap_or_default().is_empty() {
            self.author_name = Some(stamp.account_name.to_owned());
            self.author_id = Some(stamp.account_id.to_owned());
        }
        if let Some(version) = stamp.changed_in {
            self.modified_version = vec![version.to_owned()];
            self.oldest_compatible_version = vec![version.to_owned()];
        }
        Ok(())
    }
}
