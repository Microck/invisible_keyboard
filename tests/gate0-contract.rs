use invisible_keyboard::{
    gate0::{
        GATE0A_REPORT_SCHEMA, Gate0Criteria, Gate0Error, Gate0Status, MAX_SAMPLE_SPREAD_US,
        MIN_CLEAN_TRAILING_EXPOSURE_US, MIN_TIMED_EXPOSURE_US, MIN_TIMED_FRAME_COUNT,
        one_hand_capture_target,
    },
    layout::Hand,
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CANONICAL_ACQUISITION_PROFILE,
        CORE28_LAYOUT_ID, HandManifest, ManifestHand, SensorManifest,
    },
    protocol::PROTOCOL_VERSION,
    session::ControllerStream,
};

fn manifest(hand: Hand) -> AcquisitionManifest {
    let (hand_name, boot_byte, manifest_hand) = match hand {
        Hand::Left => ("left", "31", ManifestHand::Left),
        Hand::Right => ("right", "32", ManifestHand::Right),
    };
    let profile = CANONICAL_ACQUISITION_PROFILE;
    let sensors = std::array::from_fn(|sensor_index| SensorManifest {
        sensor_index: sensor_index as u8,
        mux_channel: sensor_index as u8,
        role: profile.sensor_order[sensor_index],
        mount_id: format!("{hand_name}-mount-{sensor_index}"),
        wiring_id: format!("{hand_name}-wiring-{sensor_index}"),
        interrupt_pin: format!("{hand_name}-interrupt-{sensor_index}"),
        axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
    });
    let value = AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![HandManifest {
            hand: manifest_hand,
            controller_id: format!("gate0-{hand_name}-controller"),
            boot_id: boot_byte.repeat(16),
            sensors,
        }],
        controller_board: "gate0-controller-board".to_owned(),
        firmware_build: "gate0-firmware-build".to_owned(),
        host_build: "gate0-host-build".to_owned(),
        sensor_model: "gate0-sensor-model".to_owned(),
        sensors_per_hand: 6,
        sensor_odr_hz: profile.sensor_odr_hz,
        accelerometer_range_g: profile.accelerometer_range_g,
        gyroscope_range_dps: profile.gyroscope_range_dps,
        filter_profile: "gate0-filter".to_owned(),
        i2c_bus_hz: profile.i2c_bus_hz,
        nominal_period_us: profile.nominal_period_us,
        prompt_corpus_sha256: "aa".repeat(32),
        study_id: "gate0-study".to_owned(),
        split_id: "gate0-split".to_owned(),
        participant_alias: "gate0-participant".to_owned(),
        consent_record: "gate0-consent".to_owned(),
        retention_policy: "gate0-retention".to_owned(),
        rig_evidence_sha256: "bb".repeat(32),
        axis_characterization_sha256: "cc".repeat(32),
        calibration_artifact_sha256: None,
        calibration_validation_sha256: None,
    };
    value.validate().expect("one-hand manifest is valid");
    value
}

#[test]
fn one_hand_capture_target_preserves_physical_identity() {
    for hand in [Hand::Left, Hand::Right] {
        let manifest = manifest(hand);
        let target = one_hand_capture_target(&manifest).expect("one hand has one target");
        assert_eq!(target.hand, hand);
        assert_eq!(target.source, ControllerStream::from(hand));
        assert_eq!(target.identity, manifest.frame_identities().unwrap()[0]);
    }
}

#[test]
fn one_hand_capture_target_rejects_two_hands() {
    let mut acquisition = manifest(Hand::Left);
    acquisition
        .hands
        .push(manifest(Hand::Right).hands.remove(0));
    acquisition.validate().expect("two-hand manifest is valid");

    assert!(matches!(
        one_hand_capture_target(&acquisition),
        Err(Gate0Error::ExpectedOneHand { actual: 2 })
    ));
}

#[test]
fn gate0_constants_encode_the_decisive_roadmap_contract() {
    assert_eq!(GATE0A_REPORT_SCHEMA, "invisible-keyboard-gate0a-report-v6");
    assert_eq!(MIN_TIMED_EXPOSURE_US, 3_600_000_000);
    assert_eq!(MIN_TIMED_FRAME_COUNT, 720_000);
    assert_eq!(MIN_CLEAN_TRAILING_EXPOSURE_US, 1_000_000);
    assert_eq!(MAX_SAMPLE_SPREAD_US, 5_000);
}

#[test]
fn verdict_precedence_is_fail_then_inconclusive_then_pass() {
    let mut criteria = Gate0Criteria {
        session_integrity: Gate0Status::Pass,
        manifest_session_binding: Gate0Status::Pass,
        one_hand_identity: Gate0Status::Pass,
        minimum_duration: Gate0Status::Pass,
        minimum_frame_count: Gate0Status::Pass,
        valid_receipt: Gate0Status::Pass,
        sequence_accounting: Gate0Status::Pass,
        timestamp_continuity: Gate0Status::Pass,
        explicit_fault_accounting: Gate0Status::Pass,
        timing_metrics_reported: Gate0Status::Pass,
        sample_spread: Gate0Status::Pass,
        clean_trailing_exposure: Gate0Status::Pass,
        external_evidence_binding: Gate0Status::Pass,
        logic_analyzer_timing: Gate0Status::Pass,
        deterministic_quiesce: Gate0Status::Pass,
    };
    assert_eq!(criteria.overall(), Gate0Status::Pass);

    criteria.logic_analyzer_timing = Gate0Status::Inconclusive;
    assert_eq!(criteria.overall(), Gate0Status::Inconclusive);

    criteria.valid_receipt = Gate0Status::Fail;
    assert_eq!(criteria.overall(), Gate0Status::Fail);
}
