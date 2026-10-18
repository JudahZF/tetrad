use super::*;

fn scale(xml: XmlParameter) -> ParameterScale {
    ParameterScale::from_xml(&xml).unwrap()
}

#[test]
fn absent_attributes_take_constructor_defaults() {
    let p = scale(XmlParameter::default());
    assert_eq!(p.kind, ParameterType::Float);
    assert_eq!((p.law.min, p.law.max, p.law.skew), (0.0, 1.0, 1.0));
    // No `steps`: 101 steps but step size 0, so the index divisor is 1e-8.
    assert_eq!((p.steps(), p.step_size()), (ABSENT_STEPS, 0.0));
    assert_eq!(p.step_index(0.5).unwrap(), 50_000_000);
    assert_eq!(p.default, DefaultValue::Normalized(0.0));
    assert_eq!(p.labels.step_names, ["Off", "On"]);
    assert!(p.exp_assignable && !p.show_as_integer);
    assert_eq!(ParameterType::from_xml("bogus"), ParameterType::Float);
}

#[test]
fn symbols_steps_and_defaults_convert_like_the_parser() {
    let mut p = scale(XmlParameter {
        min: Some("MIN_EQ_FREQ"),
        max: Some("MAX_EQ_FREQ"),
        skew: Some("LOG_SKEW"),
        steps: Some("1001"),
        default_value: Some("1000"),
        ..Default::default()
    });
    assert_eq!(p.step_size(), 1.0 / 1000.0);
    let DefaultValue::Normalized(default) = p.default else {
        panic!("numeric default");
    };
    assert_eq!(default, p.law.to_normalized(1000.0));
    assert!((p.to_display(default).unwrap() - 1000.0).abs() < 0.05);
    // An unknown macro falls back to min before conversion.
    let fallback = scale(XmlParameter {
        min: Some("-6"),
        max: Some("6"),
        default_value: Some("NOT_A_MACRO"),
        ..Default::default()
    });
    assert_eq!(fallback.default, DefaultValue::Normalized(0.0));
    p.set_steps(1);
    assert_eq!(p.step_size(), 1.0);
    assert_eq!(p.step_index(0.4).unwrap(), 0);
    p.set_steps(0);
    assert_eq!(p.step_index(0.9).unwrap(), 0);
}

#[test]
fn piecewise_metadata_pairs_points_and_keeps_raw_normalized_default() {
    let p = scale(XmlParameter {
        x_values: Some("0,0.5,bad,1"),
        y_values: Some("20,60,70,200"),
        default_value: Some("0.5"),
        min: Some("20"),
        max: Some("200"),
        ..Default::default()
    });
    let curve = p.piecewise.as_ref().unwrap();
    assert_eq!(curve.points().len(), 3);
    assert_eq!(p.default, DefaultValue::Normalized(0.5));
    assert_eq!(p.to_display(0.75).unwrap(), 130.0);
    assert_eq!(p.to_display(1.5), Err(ConversionError::OutsideBreakpoints));
    let mismatched = scale(XmlParameter {
        x_values: Some("0,1"),
        y_values: Some("1,2,3"),
        ..Default::default()
    });
    assert!(mismatched.piecewise.is_none());
}

#[test]
fn step_cycle_nudge_and_encoder_follow_stock_rounding() {
    let p = scale(XmlParameter {
        steps: Some("5"),
        ..Default::default()
    });
    assert_eq!(p.next_step(0.5, 1.0).unwrap(), 0.75);
    assert_eq!(p.next_step(1.0, 1.0).unwrap(), 0.0);
    assert_eq!(p.next_step(0.5, 0.0).unwrap(), 0.5);
    assert_eq!(p.nudge(0.0, -1.0).unwrap(), 0.0);
    assert_eq!(p.nudge(1.0, 1.0).unwrap(), 1.0);
    assert_eq!(p.nudge(0.25, 1.0).unwrap(), 0.5);
    assert_eq!(p.step_value(4).unwrap(), 1.0);
    assert!(p.step_value(5).is_err());
    // Single ticks move one step and snap; fast turns scale to 1% units.
    assert_eq!(p.encoder(0.5, 1), 0.75);
    assert_eq!(p.encoder(0.9, 1), 1.0);
    let fine = scale(XmlParameter {
        steps: Some("1001"),
        ..Default::default()
    });
    let moved = fine.encoder(0.5, 3);
    assert!((moved - 0.53).abs() < 1e-6, "{moved}");
}

#[test]
fn nan_press_leaves_the_step_unchanged() {
    let p = scale(XmlParameter {
        steps: Some("5"),
        ..Default::default()
    });
    assert_eq!(p.next_step(0.5, f32::NAN).unwrap(), 0.5);
}

#[test]
fn units_join_to_the_value_text_with_one_space() {
    let hz = scale(XmlParameter {
        min: Some("0"),
        max: Some("10000"),
        units: Some("Hz"),
        ..Default::default()
    });
    assert_eq!(hz.display_with_units(0.575).unwrap(), "5750 Hz");
    let bare = scale(XmlParameter {
        min: Some("0"),
        max: Some("10000"),
        ..Default::default()
    });
    assert_eq!(bare.display_with_units(0.575).unwrap(), "5750");
}

#[test]
fn display_text_uses_labels_k_form_precision_and_pan() {
    let freq = scale(XmlParameter {
        min: Some("20"),
        max: Some("20000"),
        steps: Some("1000"),
        min_string: Some("Off"),
        ..Default::default()
    });
    assert_eq!(freq.value_text(0.0).unwrap(), "Off");
    assert_eq!(freq.number_text(12_500.0).unwrap(), "12k5");
    assert_eq!(freq.number_text(15_000.0).unwrap(), "15k0");
    assert_eq!(freq.number_text(10_000.0).unwrap(), "10k");
    assert_eq!(freq.number_text(440.4).unwrap(), "440");
    let db = scale(XmlParameter {
        min: Some("-12"),
        max: Some("12"),
        steps: Some("241"),
        ..Default::default()
    });
    assert_eq!(db.number_text(-3.25).unwrap(), "-3.2");
    assert_eq!(db.number_text(-0.00005).unwrap(), "0.0");
    let fine = scale(XmlParameter {
        steps: Some("1001"),
        ..Default::default()
    });
    assert_eq!(fine.number_text(0.125).unwrap(), "0.12");
    let pan = scale(XmlParameter {
        min_string: Some("L"),
        mid_string: Some("C"),
        max_string: Some("R"),
        ..Default::default()
    });
    assert_eq!(pan.value_text(0.5).unwrap(), "C");
    assert_eq!(pan.value_text(0.0).unwrap(), "50 L");
    assert_eq!(pan.value_text(0.755).unwrap(), "26 R");
    assert!(qt_fuzzy_compare(0.0, 0.0) && !qt_fuzzy_compare(0.0, 1e-30));
}

#[test]
fn step_name_types_index_names_and_strip_combo_prefixes() {
    let rotary = scale(XmlParameter {
        type_name: Some("rotarySwitch"),
        steps: Some("3"),
        step_names: Some("A, B ,C"),
        ..Default::default()
    });
    assert_eq!(rotary.display_text(0.5).unwrap(), " B ");
    let combo = scale(XmlParameter {
        type_name: Some("comboBox"),
        steps: Some("2"),
        step_names: Some("x\u{a4} First ,y\u{a4}Second"),
        ..Default::default()
    });
    assert_eq!(combo.display_text(0.0).unwrap(), "First");
    let short = scale(XmlParameter {
        type_name: Some("rotarySwitch"),
        steps: Some("4"),
        step_names: Some("A,B"),
        ..Default::default()
    });
    assert!(short.display_text(0.0).is_err());
    let switch = scale(XmlParameter {
        type_name: Some("switch"),
        ..Default::default()
    });
    assert_eq!(
        switch.display_text(1.0),
        Err(ConversionError::TextValue(ParameterType::Switch))
    );
}
