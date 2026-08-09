use invisible_keyboard::{
    evaluation::run_synthetic_stress_suite,
    layout::{CORE28_KEY_COUNT, PHYSICAL_POSITION_COUNT},
};

#[test]
fn default_synthetic_stress_suite_is_deterministic() {
    let first = run_synthetic_stress_suite().expect("synthetic calibration is complete");
    let second = run_synthetic_stress_suite().expect("synthetic calibration is complete");

    assert_eq!(first, second);
}

#[test]
fn stress_suite_covers_every_position_and_fault_dimension() {
    let report = run_synthetic_stress_suite().expect("synthetic calibration is complete");

    assert_eq!(
        report.nominal.intentional_positions,
        u64::try_from(PHYSICAL_POSITION_COUNT).expect("position count fits u64")
    );
    assert_eq!(report.nominal.position_errors, 0);
    assert_eq!(report.nominal.false_activations, 0);
    assert!(report.ordinary_motion_exposure_us >= 3_600_000_000);
    assert_eq!(report.ordinary_motion_false_activations, 0);
    assert_eq!(
        report.ordinary_motion_zero_event_upper_95_milli_per_hour,
        Some(3_000)
    );
    assert_eq!(report.simultaneous_windows, 2);
    assert_eq!(report.simultaneous_abstentions, 2);
    assert_eq!(report.dropout_windows, 12);
    assert_eq!(report.dropout_abstentions, 12);
    assert_eq!(report.whole_hand_dropout_windows, 2);
    assert_eq!(report.whole_hand_dropout_abstentions, 2);
    assert_eq!(
        report
            .scenarios
            .iter()
            .map(|scenario| scenario.name)
            .collect::<Vec<_>>(),
        vec!["noise", "remount", "packet-loss", "clock-drift"]
    );
    assert_eq!(
        report
            .breakpoints
            .iter()
            .map(|breakpoint| breakpoint.dimension)
            .collect::<Vec<_>>(),
        vec![
            "noise-amplitude",
            "remount-axis-mix",
            "contact-tail-loss",
            "unmodeled-finger-impulse",
            "differential-bias-ramp",
        ]
    );
}

#[test]
fn nominal_report_does_not_claim_physical_evidence() {
    let report = run_synthetic_stress_suite().expect("synthetic calibration is complete");

    assert_eq!(report.evidence_label, "simulated");
    assert!(report.limitations.contains("does not establish"));
    assert!(report.limitations.contains("real finger"));
}

#[test]
fn simulated_latency_reaches_the_recording_output_after_the_causal_delay() {
    let report = run_synthetic_stress_suite().expect("synthetic calibration is complete");
    let latency = report.simulated_sensor_to_output_latency;

    assert_eq!(latency.evidence_label, "simulated");
    assert!(latency.limitations.contains("not host processing time"));
    assert!(
        latency
            .limitations
            .contains("not a kernel uinput round-trip")
    );
    assert_eq!(
        latency.sample_count,
        u64::try_from(CORE28_KEY_COUNT).expect("Core-28 count fits u64")
    );

    // Contact begins three frame periods before the triggering window ends,
    // then the pipeline waits its full three-frame post-trigger delay. A
    // decoder-only timer would report neither half of this 30 ms interval.
    assert_eq!(latency.p50_us, 30_000);
    assert_eq!(latency.p95_us, 30_000);
    assert_eq!(latency.p99_us, 30_000);
    assert_eq!(latency.max_us, 30_000);
}

#[test]
fn breakpoint_sweeps_expose_model_failure_instead_of_staying_all_green() {
    let report = run_synthetic_stress_suite().expect("synthetic calibration is complete");

    for breakpoint in &report.breakpoints {
        assert!(breakpoint.levels_tested >= 2);
        assert!(breakpoint.last_zero_failure_level.is_some());
        assert!(
            breakpoint.first_failure_level.is_some(),
            "{} sweep must reach a failure",
            breakpoint.dimension
        );
        assert!(breakpoint.failures_at_first_level > 0);
    }

    let adversarial_negative = report
        .breakpoints
        .iter()
        .find(|breakpoint| breakpoint.dimension == "unmodeled-finger-impulse")
        .expect("adversarial negative breakpoint exists");
    assert_eq!(adversarial_negative.unit, "sensor-counts");
}
