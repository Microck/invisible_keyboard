use invisible_keyboard::{
    clock_epoch::{ClockEpochError, ClockEpochFitError, ClockEpochSet},
    layout::Hand,
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CORE28_LAYOUT_ID, HandManifest,
        ManifestHand, PROTOCOL_VERSION, SensorManifest, SensorRole, format_sha256, sha256,
    },
    neutral::{
        MIN_NEUTRAL_SAMPLES_PER_HAND, NeutralEvidenceError, NeutralEvidenceReport,
        NeutralRecordWindow, NeutralWindowRole, StabilityVerdict,
    },
    protocol::{
        ALL_SENSORS_VALID, FLAG_ACQUISITION_OVERRUN, FrameError, QuiesceRequest, SENSOR_COUNT,
        SensorFrame, SensorReading,
    },
    session::{
        ControllerStream, FinalizedSession, SessionError, SessionHeader, SessionRecordKind,
        SessionWriter,
    },
};

const SESSION_ID: [u8; 16] = [0x44; 16];
const LEFT_HOST_OFFSET_NS: u64 = 5_000_000_000;
const RIGHT_HOST_OFFSET_NS: u64 = 5_001_000_000;

fn manifest() -> AcquisitionManifest {
    AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![
            hand_manifest(ManifestHand::Left, "left-controller", 0x11),
            hand_manifest(ManifestHand::Right, "right-controller", 0x22),
        ],
        controller_board: "fixture-board".to_owned(),
        firmware_build: "fixture-firmware".to_owned(),
        host_build: "fixture-host".to_owned(),
        sensor_model: "fixture-six-axis-imu".to_owned(),
        sensors_per_hand: 6,
        sensor_odr_hz: 208,
        accelerometer_range_g: 4,
        gyroscope_range_dps: 1_000,
        filter_profile: "fixture-raw-v1".to_owned(),
        i2c_bus_hz: 400_000,
        nominal_period_us: 5_000,
        prompt_corpus_sha256: format_sha256(&[0xaa; 32]),
        study_id: "neutral-contract".to_owned(),
        split_id: "fixture".to_owned(),
        participant_alias: "fixture-operator".to_owned(),
        consent_record: "fixture-consent".to_owned(),
        retention_policy: "fixture-retention".to_owned(),
        rig_evidence_sha256: format_sha256(&[0xbb; 32]),
        axis_characterization_sha256: format_sha256(&[0xcc; 32]),
        calibration_artifact_sha256: None,
        calibration_validation_sha256: None,
    }
}

fn hand_manifest(hand: ManifestHand, controller_id: &str, boot: u8) -> HandManifest {
    HandManifest {
        hand,
        controller_id: controller_id.to_owned(),
        boot_id: format!("{boot:02x}").repeat(16),
        sensors: std::array::from_fn(|sensor_index| SensorManifest {
            sensor_index: sensor_index as u8,
            mux_channel: sensor_index as u8,
            role: [
                SensorRole::Thumb,
                SensorRole::Index,
                SensorRole::Middle,
                SensorRole::Ring,
                SensorRole::Pinky,
                SensorRole::Dorsal,
            ][sensor_index],
            mount_id: format!("{controller_id}-sensor-{sensor_index}-mount"),
            wiring_id: format!("{controller_id}-sensor-{sensor_index}-wiring"),
            interrupt_pin: format!("gpio-{sensor_index}"),
            axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
        }),
    }
}

fn clocks(manifest: &AcquisitionManifest, session_id: [u8; 16]) -> ClockEpochSet {
    let session = session_bytes(manifest, 40, 0, |_, _, _| {});
    let mut clocks =
        ClockEpochSet::fit_session(manifest, &session).expect("raw fixture clocks qualify");
    clocks.session_id = session_id;
    clocks
}

fn frame(hand: Hand, index: u32) -> SensorFrame {
    let mut sensors = [SensorReading::default(); SENSOR_COUNT];
    for (sensor_index, sensor) in sensors.iter_mut().enumerate() {
        let sensor_value = i16::try_from(sensor_index).expect("six sensors fit i16");
        let shared_variation = i16::try_from(index % 3).expect("small remainder") - 1;
        let local_variation =
            i16::try_from((index + sensor_index as u32) % 3).expect("small remainder") - 1;
        sensor.acceleration = [
            100 + sensor_value * 3 + shared_variation + local_variation,
            -50 + sensor_value,
            16_000 - sensor_value * 2,
        ];
        sensor.angular_velocity = [sensor_value, shared_variation, -sensor_value];
        sensor.sample_time_offset_us = sensor_value * 20 + local_variation;
    }
    SensorFrame {
        hand,
        controller_id_sha256: match hand {
            Hand::Left => sha256(b"left-controller"),
            Hand::Right => sha256(b"right-controller"),
        },
        boot_id: match hand {
            Hand::Left => [0x11; 16],
            Hand::Right => [0x22; 16],
        },
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence: index,
        device_time_us: u64::from(index) * 5_000,
        sensors,
    }
}

fn session_bytes(
    manifest: &AcquisitionManifest,
    frame_count_per_hand: u32,
    host_offset_adjustment_ns: u64,
    mutate: impl Fn(Hand, u32, &mut SensorFrame),
) -> FinalizedSession {
    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id: SESSION_ID,
    };
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    for index in 0..frame_count_per_hand {
        for (hand, host_offset_ns) in [
            (Hand::Left, LEFT_HOST_OFFSET_NS),
            (Hand::Right, RIGHT_HOST_OFFSET_NS),
        ] {
            let mut frame = frame(hand, index);
            mutate(hand, index, &mut frame);
            writer
                .append_frame(
                    ControllerStream::from(hand),
                    &frame,
                    host_offset_ns + host_offset_adjustment_ns + u64::from(index) * 5_000_000,
                )
                .expect("frame writes");
        }
    }
    writer
        .finish_finalized()
        .expect("session finishes and audits")
}

fn windows(record_count: u64) -> Vec<NeutralRecordWindow> {
    let midpoint = record_count / 2;
    vec![
        NeutralRecordWindow {
            block_id: "neutral-calibration".to_owned(),
            window_id: "idle-before".to_owned(),
            role: NeutralWindowRole::Idle,
            start_record: 0,
            end_record_exclusive: midpoint,
        },
        NeutralRecordWindow {
            block_id: "neutral-calibration".to_owned(),
            window_id: "neutral-hold".to_owned(),
            role: NeutralWindowRole::Neutral,
            start_record: midpoint,
            end_record_exclusive: record_count,
        },
    ]
}

#[test]
fn neutral_authority_rejects_raw_receipt_faults_despite_passing_clock_statistics() {
    let manifest = manifest();
    let passing_receipts = session_bytes(&manifest, 160, 0, |_, _, _| {});
    let clocks = ClockEpochSet::fit_session(&manifest, &passing_receipts).unwrap();
    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us: manifest.nominal_period_us,
            manifest_sha256: manifest.sha256().unwrap(),
            session_id: SESSION_ID,
        },
    )
    .unwrap();
    for index in 0..160_u32 {
        let phase = u64::from(index % 32);
        let delay_ns = phase.min(32 - phase) * 1_000_000;
        for (hand, offset) in [
            (Hand::Left, LEFT_HOST_OFFSET_NS),
            (Hand::Right, RIGHT_HOST_OFFSET_NS),
        ] {
            writer
                .append_frame(
                    ControllerStream::from(hand),
                    &frame(hand, index),
                    offset + u64::from(index) * 5_000_000 + delay_ns,
                )
                .unwrap();
        }
    }
    let session = writer.finish_finalized().unwrap();
    assert!(ClockEpochSet::fit_session(&manifest, &session).is_err());
    assert!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &session, &windows(320)).is_err(),
        "claimed passing residuals cannot authorize neutral evidence from bad raw receipts"
    );
}

#[test]
fn neutral_authority_rejects_altered_clock_report_and_rebound_fingerprint() {
    let manifest = manifest();
    let session = session_bytes(&manifest, 40, 0, |_, _, _| {});
    let clocks = ClockEpochSet::fit_session(&manifest, &session).unwrap();
    let report = NeutralEvidenceReport::derive(&manifest, &clocks, &session, &windows(80)).unwrap();
    for (field, value) in [
        ("slope_ns_per_us", serde_json::json!(1000.01)),
        ("drift_ppm", serde_json::json!(1.0)),
        ("residual_p95_ns", serde_json::json!(1)),
    ] {
        let mut json = serde_json::to_value(&clocks).unwrap();
        json["epochs"][0]["fit"]["report"][field] = value;
        let forged: ClockEpochSet = serde_json::from_value(json).unwrap();
        forged.validate().unwrap();
        let mut rebound = report.clone();
        rebound.source_clock_sha256 = format_sha256(&forged.fingerprint().unwrap());
        assert!(
            NeutralEvidenceReport::derive(&manifest, &forged, &session, &windows(80)).is_err(),
            "altered {field} cannot authorize neutral evidence"
        );
        assert!(
            rebound
                .verify_sources(&manifest, &forged, &session, &windows(80))
                .is_err(),
            "rebound fingerprint cannot authorize altered {field}"
        );
    }
}

#[test]
fn report_derives_inspectable_two_hand_six_sensor_evidence() {
    let manifest = manifest();
    let clocks = clocks(&manifest, SESSION_ID);
    let session = session_bytes(&manifest, 40, 0, |_, _, _| {});
    let windows = windows(80);

    let report = NeutralEvidenceReport::derive(&manifest, &clocks, &session, &windows)
        .expect("stable evidence derives");

    assert_eq!(
        report.source_session_sha256,
        format_sha256(&session.sha256())
    );
    assert_eq!(
        report.source_manifest_sha256,
        format_sha256(&manifest.sha256().expect("manifest hashes"))
    );
    assert_eq!(
        report.source_clock_sha256,
        format_sha256(&clocks.fingerprint().expect("clock set hashes"))
    );
    assert_eq!(report.windows.len(), 2);
    assert_eq!(report.windows[0].block_id, "neutral-calibration");
    assert_eq!(report.windows[1].window_id, "neutral-hold");
    assert_eq!(report.verdict, StabilityVerdict::Stable);
    assert!(report.faults.is_empty());

    for hand in &report.hands {
        assert_eq!(hand.sample_count, 40);
        assert_eq!(hand.sensors.len(), SENSOR_COUNT);
        assert_eq!(hand.verdict, StabilityVerdict::Stable);
        assert!(hand.shared_motion.acceleration_shared_motion_energy > 0.0);
        assert!(hand.shared_motion.acceleration_independent_noise_energy > 0.0);
        for (sensor_index, sensor) in hand.sensors.iter().enumerate() {
            assert_eq!(sensor.sensor_index, sensor_index as u8);
            assert_eq!(sensor.sample_count, 40);
            assert!(sensor.acceleration_noise_energy > 0.0);
            assert!(sensor.sample_time_jitter_rms_us > 0.0);
            assert_eq!(sensor.verdict, StabilityVerdict::Stable);
        }
        assert!(hand.finger_to_dorsal.iter().all(|value| {
            value.observable_sample_count == 40 && value.verdict == StabilityVerdict::Stable
        }));
        assert_eq!(hand.sensors[5].sample_time_skew_to_dorsal_mean_us, 0.0);
        assert!(hand.sensors[0].sample_time_skew_to_dorsal_mean_us < -90.0);
    }

    assert!(report.derived_stable_duration_ns() >= 180_000_000);
    assert!(report.meets_stable_duration_ns(150_000_000));
    assert!(!report.meets_stable_duration_ns(1_000_000_000));
}

#[test]
fn canonical_json_round_trip_and_source_bound_mutation_detection() {
    let manifest = manifest();
    let clocks = clocks(&manifest, SESSION_ID);
    let session = session_bytes(&manifest, 40, 0, |_, _, _| {});
    let windows = windows(80);
    let report = NeutralEvidenceReport::derive(&manifest, &clocks, &session, &windows)
        .expect("stable evidence derives");

    let canonical = report.canonical_json().expect("report serializes");
    let recovered = NeutralEvidenceReport::from_json(&canonical).expect("report parses");
    assert_eq!(recovered, report);
    assert_eq!(
        recovered.fingerprint().expect("recovered hashes"),
        report.fingerprint().expect("original hashes")
    );
    recovered
        .verify_sources(&manifest, &clocks, &session, &windows)
        .expect("round trip remains source-bound");

    let mut mutation: serde_json::Value =
        serde_json::from_slice(&canonical).expect("canonical report is JSON");
    mutation["hands"][0]["sensors"][0]["acceleration_mean_gravity"][0] = serde_json::json!(999.0);
    let mutated = NeutralEvidenceReport::from_json(
        &serde_json::to_vec(&mutation).expect("mutation serializes"),
    )
    .expect("finite mutation remains structurally valid");
    assert!(matches!(
        mutated.verify_sources(&manifest, &clocks, &session, &windows),
        Err(NeutralEvidenceError::ReportMismatch)
    ));

    mutation["caller_supplied_key_state_neutral"] = serde_json::json!(true);
    assert!(matches!(
        NeutralEvidenceReport::from_json(
            &serde_json::to_vec(&mutation).expect("unknown-field mutation serializes")
        ),
        Err(NeutralEvidenceError::Json(_))
    ));

    let mut nonfinite = report;
    nonfinite.hands[0].sensors[0].acceleration_noise_energy = f64::NAN;
    assert!(matches!(
        nonfinite.validate(),
        Err(NeutralEvidenceError::NonFiniteReport(_))
    ));
}

#[test]
fn missing_hands_sensors_invalid_frames_and_loss_fail_closed() {
    let mut one_hand = manifest();
    one_hand.hands.pop();
    let original_manifest = manifest();
    let original_clocks = clocks(&original_manifest, SESSION_ID);
    let session = session_bytes(&original_manifest, 40, 0, |_, _, _| {});
    assert!(matches!(
        NeutralEvidenceReport::derive(&one_hand, &original_clocks, &session, &windows(80)),
        Err(NeutralEvidenceError::MissingHand(Hand::Right))
    ));

    let manifest = manifest();
    let clocks = clocks(&manifest, SESSION_ID);
    let missing_sensor = session_bytes(&manifest, 40, 0, |hand, index, frame| {
        if hand == Hand::Left && index == 4 {
            frame.valid_sensor_mask &= !(1 << 2);
        }
    });
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &missing_sensor, &windows(80)),
        Err(NeutralEvidenceError::MissingSensor {
            hand: Hand::Left,
            sensor_index: 2,
            ..
        })
    ));

    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id: SESSION_ID,
    };
    let mut invalid_writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    let mut invalid_frame = frame(Hand::Right, 5);
    invalid_frame.flags = 1 << 31;
    assert!(matches!(
        invalid_writer.append_frame(ControllerStream::Right, &invalid_frame, 0),
        Err(SessionError::Frame(FrameError::UnknownFlags(_)))
    ));

    let loss = session_bytes(&manifest, 40, 0, |hand, index, frame| {
        if hand == Hand::Left && index == 20 {
            frame.sequence += 1;
        }
    });
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &loss, &windows(80)),
        Err(NeutralEvidenceError::SequenceDiscontinuity {
            hand: Hand::Left,
            ..
        })
    ));
}

#[test]
fn acquisition_faults_bad_clocks_short_evidence_and_any_invalid_candidate_are_rejected() {
    let manifest = manifest();
    let clocks = clocks(&manifest, SESSION_ID);
    let fault = session_bytes(&manifest, 40, 0, |hand, index, frame| {
        if hand == Hand::Right && index == 7 {
            frame.flags = FLAG_ACQUISITION_OVERRUN;
        }
    });
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &fault, &windows(80)),
        Err(NeutralEvidenceError::AcquisitionFault {
            hand: Hand::Right,
            ..
        })
    ));

    let delayed = session_bytes(&manifest, 40, 50_000_000, |_, _, _| {});
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &delayed, &windows(80)),
        Err(NeutralEvidenceError::ClockQualification(
            ClockEpochFitError::NonCanonicalFit
        ))
    ));

    let short = session_bytes(&manifest, 24, 0, |_, _, _| {});
    let short_clocks = ClockEpochSet::fit_session(&manifest, &short).unwrap();
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &short_clocks, &short, &windows(48)),
        Err(NeutralEvidenceError::TooFewSamples {
            actual: 24,
            minimum: MIN_NEUTRAL_SAMPLES_PER_HAND,
            ..
        })
    ));

    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id: SESSION_ID,
    };
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    for index in 0..40_u32 {
        for (hand, host_offset) in [
            (Hand::Left, LEFT_HOST_OFFSET_NS),
            (Hand::Right, RIGHT_HOST_OFFSET_NS),
        ] {
            writer
                .append_frame(
                    ControllerStream::from(hand),
                    &frame(hand, index),
                    host_offset + u64::from(index) * 5_000_000,
                )
                .expect("frame writes");
        }
    }
    writer
        .append_candidate(
            ControllerStream::Left,
            SessionRecordKind::Malformed,
            &[1, 2, 3],
            3,
            9_000_000_000,
        )
        .expect("malformed evidence writes");
    let with_malformed_tail = writer
        .finish_finalized()
        .expect("session finishes and audits");
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &with_malformed_tail, &windows(80)),
        Err(NeutralEvidenceError::InvalidCandidate {
            record_index: 80,
            ..
        })
    ));
}

#[test]
fn neutral_evidence_rejects_gate0_control_records_as_their_own_context_error() {
    let manifest = manifest();
    let clocks = clocks(&manifest, SESSION_ID);
    let identity = manifest.hands[0]
        .frame_identity()
        .expect("left manifest identity is valid");
    let request = QuiesceRequest {
        hand: Hand::Left,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        session_id: SESSION_ID,
        request_start_time_ns: 9_000_000_000,
    };
    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us: manifest.nominal_period_us,
            manifest_sha256: manifest.sha256().expect("manifest hashes"),
            session_id: SESSION_ID,
        },
    )
    .expect("control session starts");
    writer
        .append_quiesce_request(ControllerStream::Left, &request, 9_000_000_000)
        .expect("valid request evidence writes");
    let session = writer
        .finish_finalized()
        .expect("control session finalizes and audits");

    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &session, &windows(2)),
        Err(NeutralEvidenceError::UnexpectedControlRecord {
            record_index: 0,
            kind: SessionRecordKind::QuiesceRequest,
        })
    ));
}

#[test]
fn manifest_clock_session_and_window_identities_must_all_match() {
    let manifest = manifest();
    let session = session_bytes(&manifest, 40, 0, |_, _, _| {});
    let wrong_session_clocks = clocks(&manifest, [0x55; 16]);
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &wrong_session_clocks, &session, &windows(80)),
        Err(NeutralEvidenceError::ClockEpoch(
            ClockEpochError::SessionIdentityMismatch
        ))
    ));

    let clocks = clocks(&manifest, SESSION_ID);
    let mut overlapping = windows(80);
    overlapping[1].start_record = 20;
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &session, &overlapping),
        Err(NeutralEvidenceError::InvalidWindow { .. })
    ));

    let mut duplicate_identity = windows(80);
    duplicate_identity[1].window_id = duplicate_identity[0].window_id.clone();
    assert!(matches!(
        NeutralEvidenceReport::derive(&manifest, &clocks, &session, &duplicate_identity),
        Err(NeutralEvidenceError::DuplicateWindowIdentity { .. })
    ));
}


#[test]
fn independently_derived_neutral_rejects_repeated_raw_sample_outside_selected_windows() {
    let manifest = manifest();
    let mut writer = SessionWriter::new(Vec::new(), SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().unwrap(), session_id: SESSION_ID,
    }).unwrap();
    for index in 0..42 {
        for hand in [Hand::Left, Hand::Right] {
            let mut frame = frame(hand, index);
            frame.device_time_us = 100_000 + u64::from(index) * 5_000;
            for sensor in &mut frame.sensors { sensor.sample_time_offset_us = 0; }
            if hand == Hand::Left && index == 1 { frame.sensors[0].sample_time_offset_us = -5_000; }
            let host_offset = if hand == Hand::Left { LEFT_HOST_OFFSET_NS } else { RIGHT_HOST_OFFSET_NS };
            writer.append_frame(ControllerStream::from(hand), &frame,
                host_offset + frame.device_time_us * 1_001).unwrap();
        }
    }
    let session = writer.finish_finalized().unwrap();
    let clocks = ClockEpochSet::fit_session(&manifest, &session).expect("genuine canonical 1001ns/us clocks fit");
    for hand in [Hand::Left, Hand::Right] {
        assert!((clocks.fit_for(hand).unwrap().report().slope_ns_per_us - 1_001.0).abs() < 0.01);
    }
    let selected = vec![NeutralRecordWindow {
        block_id: "neutral-source-admission".to_owned(), window_id: "after-fault".to_owned(),
        role: NeutralWindowRole::Neutral, start_record: 4, end_record_exclusive: 84,
    }];
    let error = NeutralEvidenceReport::derive(&manifest, &clocks, &session, &selected)
        .expect_err("independent neutral authority must admit every raw sample before selecting intervals");
    let context = format!("{error:?}");
    assert!(context.contains("record_index: 2"), "{context}");
    assert!(context.contains("SensorTime"), "{context}");
    assert!(context.contains("Left"), "{context}");
}
