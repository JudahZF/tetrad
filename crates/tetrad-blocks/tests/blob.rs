//! V3: the blob container, relocation and deployment through `tetrad-dsp-host`.
mod support;

use qc_format::blob::{ModuleBlob, Section};
use support::Gain;
use tetrad_blocks::{
    BlockError, BuiltModule, Callback, LinkedModule, OwnedBlock, ParameterSpec, RoutineLabels,
    SectionAddresses, StaticLayout,
};
use tetrad_dsp_host::blob::{place, relocate};
use tetrad_sharc::emit::{BranchRelocation, IsaProgram};
use tetrad_sharc::isa48::{BranchKind, Condition, Instruction48};

#[test]
fn builder_emits_static_state_visa_shim_and_relocation_free_isa() {
    let built = BuiltModule::build(&Gain).unwrap();
    let blob = &built.blob;
    let sections: Vec<_> = blob.contents.iter().map(|s| s.section).collect();
    assert_eq!(sections, [Section::Dm, Section::Code, Section::Isa]);
    assert_eq!(blob.entry, Section::Code.base());
    assert!(blob.relocation_words(Section::Isa).is_none());
    assert!(!built.stats.reuse_pmco);
    assert_eq!(built.stats.dm_bytes, 16);
    assert_eq!(
        (built.stats.visa_bytes, built.stats.isa_bytes),
        (
            blob.content(Section::Code).unwrap().len() as u32,
            blob.content(Section::Isa).unwrap().len() as u32
        )
    );
    // Channel A at 0, B at the stride; type 7 copies this at every deploy.
    let state = blob.content(Section::Dm).unwrap();
    let word = |offset: usize| u32::from_le_bytes(state[offset..offset + 4].try_into().unwrap());
    let b = built.layout.channel_b_offset_bytes.unwrap() as usize;
    assert_eq!([word(0), word(b)], [1.0_f32.to_bits(), 0.5_f32.to_bits()]);
    // One kind-8 pointer per installed ctx word; per ISA stub one kind-11
    // CJUMP and one kind-7 return literal (destroy has none: it frees nothing).
    let records = blob.relocations[0].code_relocations().unwrap();
    let count = |kind| records.iter().filter(|r| r.kind == kind).count();
    assert_eq!(
        (count(8), count(11), count(7)),
        (1 + Callback::ALL.len(), 4, 4)
    );
    assert_eq!(ModuleBlob::parse(&blob.to_bytes().unwrap()).unwrap(), *blob);
}

/// Relocating the link-base blob to a placement gives exactly the module
/// linked at that placement, which V2 executes. Two instances get two
/// placements; type 7 and `0x1c` carry them (support::deployment).
#[test]
fn relocated_blob_equals_the_module_linked_at_its_placement() {
    let built = BuiltModule::build(&Gain).unwrap();
    for placement in place(&[Some(&built.blob), Some(&built.blob)]).unwrap() {
        let placement = placement.unwrap();
        let placed = relocate(&built.blob, &placement).unwrap();
        let at = SectionAddresses {
            dm: placement.remap(Section::Dm.base()),
            visa: placement.remap(Section::Code.base()),
            isa: placement.remap(Section::Isa.base()),
        };
        let linked = LinkedModule::link(&Gain, at).unwrap();
        assert_eq!(placed.entry, at.visa);
        let transfer = |section| {
            &placed
                .transfers
                .iter()
                .find(|transfer| transfer.section == section)
                .unwrap()
                .bytes
        };
        let visa: Vec<u8> = linked.visa.iter().flat_map(|p| p.to_le_bytes()).collect();
        assert_eq!(transfer(Section::Code), &visa);
        assert_eq!(transfer(Section::Isa), &linked.isa.isa48_le_bytes());
        assert_eq!(transfer(Section::Dm), &linked.dm);
    }
    let placed = support::deployment::verify_deployment(&built.blob, Gain.model_id());
    assert_eq!(placed.len(), 2);
}

/// An absolute branch into its own code would need a section-3 relocation.
struct AbsoluteBody;

impl OwnedBlock for AbsoluteBody {
    fn model_id(&self) -> u32 {
        Gain.model_id()
    }
    fn parameters(&self) -> &'static [ParameterSpec] {
        &[]
    }
    fn static_layout(&self) -> StaticLayout {
        Gain.static_layout()
    }
    fn process_cycles(&self) -> u64 {
        Gain.process_cycles()
    }
    fn emit_body(&self, program: &mut IsaProgram) -> Result<RoutineLabels, BlockError> {
        let labels = Gain.emit_body(program)?;
        program.branch_to(
            BranchKind::Jump,
            Condition::Always,
            labels.reset.clone(),
            BranchRelocation::AbsPm24,
        )?;
        program.push(Instruction48::Rts)?;
        Ok(labels)
    }
}

#[test]
fn builder_rejects_absolute_references_instead_of_omitting_relocations() {
    assert!(matches!(
        BuiltModule::build(&AbsoluteBody),
        Err(BlockError::PositionDependentBody)
    ));
}
