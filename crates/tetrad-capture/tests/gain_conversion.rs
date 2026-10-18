use tetrad_capture::reference::gain_target;
use tetrad_capture::tones::{V1Tone, v1_tone_gain_reference};

#[test]
fn gain_laws_use_recovered_f32_multipliers_not_division() {
    // Synthetic discriminators: each exponent differs by one ulp if division
    // replaces the recovered multiplication. powf is the same host reference on
    // both sides, not a portable libm-bit or stock-SHARC oracle.
    let external = std::hint::black_box(0.0006_f32);
    let peaking = std::hint::black_box(0.0028_f32);
    let shelf = std::hint::black_box(0.0009_f32);
    let cases = [
        (
            48.0 * external - 24.0,
            0x3d4c_cccd,
            20.0,
            0xbf99_6a6b,
            gain_target(external).unwrap(),
        ),
        (
            12.0 * peaking - 6.0,
            0x3d4c_cccd,
            20.0,
            0xbe98_bd67,
            v1_tone_gain_reference(V1Tone::First, peaking).unwrap(),
        ),
        (
            12.0 * peaking - 6.0,
            0x3d4c_cccd,
            20.0,
            0xbe98_bd67,
            v1_tone_gain_reference(V1Tone::Second, peaking).unwrap(),
        ),
        (
            12.0 * shelf - 6.0,
            0x3ccc_cccd,
            40.0,
            0xbe19_52d3,
            v1_tone_gain_reference(V1Tone::Third, shelf).unwrap(),
        ),
    ];
    for (decibels, scale_bits, divisor, exponent_bits, actual) in cases {
        let exponent = decibels * f32::from_bits(scale_bits);
        assert_eq!(exponent.to_bits(), exponent_bits);
        assert_ne!(exponent.to_bits(), (decibels / divisor).to_bits());
        assert_eq!(actual.to_bits(), 10.0_f32.powf(exponent).to_bits());
    }
}
