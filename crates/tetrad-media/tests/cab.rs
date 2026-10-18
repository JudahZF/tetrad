use std::path::PathBuf;
use std::sync::Arc;

use tetrad_media::block::{REVERB_TABLE_BYTES, ReverbTable};
use tetrad_media::cab::{
    Biquad, CAB_IR_WORDS, CabBlock, CabDriver, CabError, CabIr, MicIrSet, STEREO_MIC_SCALE,
    StockCabData,
};
use tetrad_media::ir::IrChannel;

/// Node `k` holds `k + 1` at sample 0 and 1.0 at sample 1, so the reversed
/// output shows `Σ w(k+1)` at word 1023 and `Σ w` at word 1022.
fn marked(k: usize) -> Arc<CabIr> {
    let mut ir = [0.0; CAB_IR_WORDS];
    ir[0] = (k + 1) as f32;
    ir[1] = 1.0;
    Arc::new(ir)
}

fn set(n: usize) -> Vec<Arc<CabIr>> {
    (0..n).map(marked).collect()
}

fn stock() -> StockCabData {
    StockCabData {
        default_ir: marked(99),
        petrucci_rooms: [marked(10), marked(11), marked(12)],
        mono_reverb: ReverbTable::new(&[1; REVERB_TABLE_BYTES]).unwrap(),
        stereo_reverb: ReverbTable::new(&[2; REVERB_TABLE_BYTES]).unwrap(),
    }
}

fn head(mic: &mut MicIrSet) -> (f32, f32) {
    let out = mic.render();
    (out[CAB_IR_WORDS - 1], out[CAB_IR_WORDS - 2])
}

#[test]
fn grid_sets_blend_row_major_and_store_reversed() {
    let mut mic = MicIrSet::new(marked(99));
    assert_eq!(head(&mut mic), (100.0, 1.0), "ctor default node");
    mic.load("NG", &set(18)).unwrap();
    // Distance picks the row (letter), position the column (digit).
    mic.set_distance(0.5);
    mic.set_position(0.5);
    // Row 1, column 2.5: nodes 8 and 9 at 0.5 each.
    assert_eq!(head(&mut mic), (0.5 * 9.0 + 0.5 * 10.0, 1.0));
    mic.set_distance(1.0);
    mic.set_position(1.0);
    assert_eq!(head(&mut mic), (18.0, 1.0), "last row, last column clamps");
    mic.set_grid_mode(0.0);
    assert_eq!(head(&mut mic), (1.0, 1.0), "grid mode off uses node 0");
    // The ML 15-file layout: 01..14 in row 0 and FAR in row 1.
    mic.load("ML", &set(15)).unwrap();
    mic.set_distance(1.0);
    assert_eq!(head(&mut mic), (15.0, 1.0));
    assert_eq!(mic.load("NG", &[]), Err(CabError::EmptySet));
}

#[test]
fn ml_scattered_sets_copy_on_a_node_and_blend_inside_a_triangle() {
    let mut mic = MicIrSet::new(marked(99));
    mic.load("ML", &set(9)).unwrap();
    // The ctor position lies exactly on node 3: two axis points, weights 1/0.
    mic.set_distance(0.0);
    assert_eq!(head(&mut mic), (4.0, 1.0));

    // (0.5, 0.5): triangle FAR (0.5, 1), node 4 (0.571, 0), node 3 (0.4286, 0).
    mic.set_position(0.5);
    mic.set_distance(0.5);
    let (value, weight_sum) = head(&mut mic);
    let corners = [(0.5, 1.0, 9.0), (0.571, 0.0, 5.0), (0.4286, 0.0, 4.0)];
    let expected = barycentric(corners, (0.5, 0.5));
    assert!((f64::from(weight_sum) - 1.0).abs() < 1e-5);
    assert!(
        (f64::from(value) - expected).abs() < 1e-4,
        "{value} vs {expected}"
    );
}

fn barycentric(corners: [(f64, f64, f64); 3], (px, py): (f64, f64)) -> f64 {
    let [(x1, y1, v1), (x2, y2, v2), (x3, y3, v3)] = corners;
    let det = (y2 - y3) * (x1 - x3) + (x3 - x2) * (y1 - y3);
    let l1 = ((y2 - y3) * (px - x3) + (x3 - x2) * (py - y3)) / det;
    let l2 = ((y3 - y1) * (px - x3) + (x1 - x3) * (py - y3)) / det;
    l1 * v1 + l2 * v2 + (1.0 - l1 - l2) * v3
}

#[test]
fn filters_are_identity_when_off_and_have_unit_passband() {
    assert_eq!(Biquad::low_cut(20.0), Biquad::IDENTITY);
    assert_eq!(Biquad::high_cut(20_000.0), Biquad::IDENTITY);
    let dc_gain = |q: Biquad| (q.b0 + q.b1 + q.b2) / (1.0 + q.a1 + q.a2);
    let nyquist_gain = |q: Biquad| (q.b0 - q.b1 + q.b2) / (1.0 - q.a1 + q.a2);
    let hpf = Biquad::low_cut(100.0);
    assert!(dc_gain(hpf).abs() < 1e-6 && (nyquist_gain(hpf) - 1.0).abs() < 1e-5);
    let lpf = Biquad::high_cut(5_000.0);
    assert!((dc_gain(lpf) - 1.0).abs() < 1e-5 && nyquist_gain(lpf).abs() < 1e-6);
}

#[test]
fn drivers_send_stock_messages_and_scale_per_model() {
    let stock = stock();
    let mono = CabBlock::new(CabDriver::Mono, 12000, &stock);
    assert_eq!(mono.ctor_messages(&stock, true)[0].argument, 2);
    assert!(mono.ctor_messages(&stock, false).is_empty());
    let parallax = CabBlock::new(CabDriver::Mono, 3008, &stock);
    assert!(parallax.ctor_messages(&stock, true).is_empty());

    let mut stereo = CabBlock::new(CabDriver::Stereo, 32000, &stock);
    let messages = stereo.ctor_messages(&stock, true);
    assert_eq!(
        (messages[0].argument, &messages[0].payload[..]),
        (3, &[2, 0, 0, 0, 2, 0, 0, 0][..])
    );
    assert_eq!(messages[1].payload, vec![2; REVERB_TABLE_BYTES]);
    let mic_b = stereo.mic_transfer(IrChannel::Channel1);
    assert_eq!(mic_b.argument, 1);
    let first = f32::from_le_bytes(mic_b.payload[4092..].try_into().unwrap());
    assert_eq!(first, 100.0 * STEREO_MIC_SCALE);

    let mut petrucci = CabBlock::new(CabDriver::Mono, 12115, &stock);
    assert!(petrucci.ctor_messages(&stock, true).is_empty());
    petrucci.gain = 0.5;
    let mic_a = petrucci.mic_transfer(IrChannel::Channel0);
    assert_eq!(
        f32::from_le_bytes(mic_a.payload[4092..].try_into().unwrap()),
        50.0
    );
    // Room index max(round(v*3) - 1, 0): 0.0 → IR 0, 0.5 → IR 1, 1.0 → IR 2.
    for (value, room) in [(0.0, 11.0), (0.5, 12.0), (1.0, 13.0)] {
        petrucci.room_distance[1] = value;
        let room_b = petrucci.room_transfer(&stock, IrChannel::Channel1);
        assert_eq!(room_b.argument, 3);
        let word = f32::from_le_bytes(room_b.payload[..4].try_into().unwrap());
        assert_eq!(word, room * 0.5, "room samples are not reversed");
    }
    assert_eq!(
        StockCabData::from_stock_zenui(&[0; 16]).unwrap_err(),
        CabError::ZenUiTooShort(16)
    );
}

/// End to end on local stock data: every catalog entity decrypts, loads and
/// renders through both drivers with default and swept parameters.
#[test]
#[ignore = "reads ZenUI, qc_sn, the IR catalog and the factory IRs in $QC_CORPUS_DIR/rootfs"]
fn local_factory_entities_render_finite_cab_irs() {
    use tetrad_media::catalog::{CatalogLimits, FactoryIrCatalog};
    use tetrad_media::factory::{DeviceCipher, DeviceSerial, FactoryIrSeed};

    let Some(rootfs) =
        std::env::var_os("QC_CORPUS_DIR").map(|dir| PathBuf::from(dir).join("rootfs"))
    else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return;
    };
    let read = |path: &str| std::fs::read(rootfs.join(path)).unwrap();
    let zenui = read("opt/neuraldsp/ZenUI");
    let cipher = DeviceCipher::seed_and_serial(
        &FactoryIrSeed::from_stock_zenui(&zenui).unwrap(),
        &DeviceSerial::parse(&read("media/p4/qc_sn")).unwrap(),
    );
    let stock = StockCabData::from_stock_zenui(&zenui).unwrap();
    assert!(stock.default_ir.iter().all(|w| w.is_finite()));
    let catalog = FactoryIrCatalog::decode(
        &read("opt/neuraldsp/factory_ir_entities.pb"),
        CatalogLimits {
            max_bytes: 4 << 20,
            max_entities: 4096,
            max_files_per_entity: 64,
        },
    )
    .unwrap();
    for entity in catalog.entities() {
        let irs: Vec<_> = entity
            .files
            .iter()
            .map(|file| {
                let bytes = std::fs::read(rootfs.join(&file.path[1..])).unwrap();
                tetrad_media::cab::cab_ir(&cipher.decode_factory_ir(&bytes).unwrap())
            })
            .collect();
        for driver in [CabDriver::Mono, CabDriver::Stereo] {
            let mut block = CabBlock::new(driver, 12000, &stock);
            block.low_cut = Biquad::low_cut(80.0);
            block.high_cut = Biquad::high_cut(8_000.0);
            block.mics[0].load(&entity.tag, &irs).unwrap();
            for (position, distance) in [(0.4, 0.0), (0.0, 1.0), (0.77, 0.31), (1.0, 0.5)] {
                block.mics[0].set_position(position);
                block.mics[0].set_distance(distance);
                let sent = block.mic_transfer(IrChannel::Channel0);
                assert!(
                    sent.payload
                        .chunks_exact(4)
                        .all(|w| f32::from_le_bytes(w.try_into().unwrap()).is_finite()),
                    "{}",
                    entity.readable_name
                );
            }
        }
    }
}
