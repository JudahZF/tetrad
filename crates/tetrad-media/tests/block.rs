use tetrad_media::block::{
    IrBlockError, IrLoaderLoadPlan, IrLoaderModel, IrSlotContent, RECEIVE_DATA_MESSAGE,
    REVERB_TABLE_BYTES, ReverbTable,
};
use tetrad_media::ir::{IrChannel, IrInputFormat, IrLimits, decode};

fn samples(words: usize) -> tetrad_media::ir::IrSamples {
    let bytes: Vec<u8> = (0..words as u32).flat_map(|w| w.to_le_bytes()).collect();
    decode(
        &bytes,
        IrInputFormat::RawLeFloat32,
        IrLimits {
            max_asset_bytes: 1 << 16,
            max_chunks: 0,
        },
    )
    .unwrap()
}

#[test]
fn ir_loader_models_match_recovered_masks_and_setter() {
    let dual = [29003, 29004, 29007, 29008];
    let stereo = [29002, 29004, 29006, 29008];
    for model in IrLoaderModel::ALL {
        let id = model.model_id();
        assert_eq!(IrLoaderModel::from_model_id(id), Some(model));
        assert_eq!(model.ir_count(), if dual.contains(&id) { 2 } else { 1 });
        assert_eq!(
            model.output_channels(),
            if stereo.contains(&id) { 2 } else { 1 }
        );
        assert_eq!(model.is_lite(), id >= 29005);
        assert_eq!(model.channel_for_param(2), Some(IrChannel::Channel0));
        let second = model.channel_for_param(10);
        assert_eq!(second.is_some(), dual.contains(&id));
        assert_eq!(model.channel_for_param(3), None);
    }
    assert_eq!(IrLoaderModel::from_model_id(29009), None);
    assert_eq!(
        IrLoaderModel::DualStereo.config().payload(),
        [2, 0, 0, 0, 2, 0, 0, 0]
    );
}

#[test]
fn load_plan_follows_stock_records_and_reverb_rules() {
    let ir = samples(1500);
    let table = ReverbTable::new(&[0x5a; REVERB_TABLE_BYTES]).unwrap();
    let plan = IrLoaderLoadPlan::new(
        IrLoaderModel::DualMono,
        Some(&table),
        &[IrSlotContent::Missing, IrSlotContent::Loaded(&ir)],
    )
    .unwrap();
    assert_eq!(plan.config.payload(), [2, 0, 0, 0, 1, 0, 0, 0]);
    assert_eq!(plan.reverb_table.as_ref(), Some(&table));
    assert_eq!(plan.transfers[0].argument(), 0);
    assert_eq!(plan.transfers[0].byte_count(), 0);
    assert_eq!(plan.transfers[1].argument(), 1);
    assert_eq!(plan.transfers[1].byte_count(), 4096);
    assert_eq!(plan.transfers[1].message_type(), RECEIVE_DATA_MESSAGE);

    let lite = IrLoaderLoadPlan::new(
        IrLoaderModel::SingleMonoLite,
        None,
        &[IrSlotContent::Default],
    )
    .unwrap();
    assert_eq!(lite.reverb_table, None);
    assert!(lite.transfers.is_empty());

    let dual_lite = IrLoaderLoadPlan::new(
        IrLoaderModel::DualMonoLite,
        None,
        &[IrSlotContent::Default, IrSlotContent::Loaded(&ir)],
    )
    .unwrap();
    assert_eq!(dual_lite.transfers.len(), 1);
    assert_eq!(dual_lite.transfers[0].argument(), 1);

    assert_eq!(
        IrLoaderLoadPlan::new(IrLoaderModel::SingleMono, None, &[IrSlotContent::Default]),
        Err(IrBlockError::MissingReverbTable(IrLoaderModel::SingleMono))
    );
    assert_eq!(
        IrLoaderLoadPlan::new(IrLoaderModel::SingleMonoLite, None, &[]),
        Err(IrBlockError::SlotCount {
            model: IrLoaderModel::SingleMonoLite,
            expected: 1,
            actual: 0
        })
    );
    assert_eq!(
        ReverbTable::new(&[0; 4]),
        Err(IrBlockError::ReverbTableSize(4))
    );
}
