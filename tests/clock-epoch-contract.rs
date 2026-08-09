use invisible_keyboard::{
    clock::{ClockFit, ClockSample, OnlineClockDiagnostics},
    clock_epoch::{
        ClockEpochError, ClockEpochFitError, ClockEpochSet, ControllerClockEpoch, LiveClockReport,
        OperationalControllerClock,
    },
    layout::Hand,
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CORE28_LAYOUT_ID, FrameIdentityError,
        HandManifest, ManifestHand, PROTOCOL_VERSION, SensorManifest, SensorRole, format_sha256,
        sha256,
    },
    protocol::{ALL_SENSORS_VALID, FLAG_CLOCK_UNSYNCED, SENSOR_COUNT, SensorFrame, SensorReading},
    session::{ControllerStream, SessionAudit, SessionHeader, SessionWriter, audit_session_reader},
};

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
        sensor_model: "LSM6DS3TR-C".to_owned(),
        sensors_per_hand: 6,
        sensor_odr_hz: 208,
        accelerometer_range_g: 4,
        gyroscope_range_dps: 1_000,
        filter_profile: "gate0-raw-v1".to_owned(),
        i2c_bus_hz: 400_000,
        nominal_period_us: 5_000,
        prompt_corpus_sha256: format_sha256(&[0xaa; 32]),
        study_id: "runtime-replay".to_owned(),
        split_id: "held-out".to_owned(),
        participant_alias: "fixture-operator".to_owned(),
        consent_record: "fixture-consent".to_owned(),
        retention_policy: "local-delete-after-analysis-v1".to_owned(),
        rig_evidence_sha256: format_sha256(&[0xbb; 32]),
        axis_characterization_sha256: format_sha256(&[0xcc; 32]),
        calibration_artifact_sha256: Some(format_sha256(&[0xbb; 32])),
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
            interrupt_pin: format!("{controller_id}-sensor-{sensor_index}-int"),
            axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
        }),
    }
}

fn fit(boot: u8, offset_ns: u64) -> ClockFit {
    let samples = (0..20_u64)
        .map(|index| ClockSample {
            device_time_us: index * 10_000,
            host_time_ns: offset_ns + index * 10_000_000,
        })
        .collect::<Vec<_>>();
    ClockFit::fit([boot; 16], &samples).expect("fixture clock fits")
}

fn epoch_set(manifest: &AcquisitionManifest, session_id: [u8; 16]) -> ClockEpochSet {
    ClockEpochSet::new(
        manifest.sha256().expect("manifest hashes"),
        session_id,
        vec![
            ControllerClockEpoch {
                hand: Hand::Left,
                controller_id: "left-controller".to_owned(),
                fit: fit(0x11, 5_000_000_000),
            },
            ControllerClockEpoch {
                hand: Hand::Right,
                controller_id: "right-controller".to_owned(),
                fit: fit(0x22, 5_001_000_000),
            },
        ],
    )
    .expect("epoch set is valid")
}

fn session_audit(
    manifest: &AcquisitionManifest,
    session_id: [u8; 16],
    sequence: Option<u32>,
) -> SessionAudit {
    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id,
    };
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    if let Some(sequence) = sequence {
        writer
            .append_frame(
                ControllerStream::Left,
                &SensorFrame {
                    hand: Hand::Left,
                    controller_id_sha256: sha256(b"left-controller"),
                    boot_id: [0x11; 16],
                    flags: 0,
                    valid_sensor_mask: ALL_SENSORS_VALID,
                    sequence,
                    device_time_us: u64::from(sequence) * 5_000,
                    sensors: [SensorReading::default(); SENSOR_COUNT],
                },
                5_000_000_000 + u64::from(sequence) * 5_000_000,
            )
            .expect("session frame writes");
    }
    let session = writer
        .finish_finalized()
        .expect("session finishes and audits");
    audit_session_reader(session.bytes()).expect("session audit repeats")
}

#[test]
fn clock_epoch_round_trip_binds_manifest_session_controller_and_boot() {
    let manifest = manifest();
    let session_id = [9; 16];
    let epochs = epoch_set(&manifest, session_id);
    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id,
    };

    epochs
        .bind(&manifest, &header)
        .expect("all identities bind");
    let source = epochs.canonical_json().expect("epoch set serializes");
    let recovered = ClockEpochSet::from_json(&source).expect("epoch set parses");
    recovered
        .bind(&manifest, &header)
        .expect("recovered identities bind");
    assert_eq!(
        recovered.fingerprint().expect("recovered hashes"),
        epochs.fingerprint().expect("original hashes")
    );
    assert_eq!(
        recovered
            .fit_for(Hand::Right)
            .expect("right fit exists")
            .report()
            .boot_id,
        [0x22; 16]
    );
}

#[test]
fn live_clock_report_separates_recent_operational_fits_from_offline_qualification() {
    let manifest = manifest();
    let session_id = [9; 16];
    let session_audit = session_audit(&manifest, session_id, None);
    let operational_clocks = vec![
        OperationalControllerClock {
            hand: Hand::Left,
            controller_id: "left-controller".to_owned(),
            fit: Some(fit(0x11, 7_000_000_000)),
            diagnostics: OnlineClockDiagnostics {
                accepted_fits: 8,
                ..OnlineClockDiagnostics::default()
            },
        },
        OperationalControllerClock {
            hand: Hand::Right,
            controller_id: "right-controller".to_owned(),
            fit: Some(fit(0x22, 7_001_000_000)),
            diagnostics: OnlineClockDiagnostics {
                candidate_quality_failures: 2,
                holdover_frames: 3,
                accepted_fits: 7,
                maximum_holdover_age_us: 15_000,
            },
        },
    ];

    let qualified = LiveClockReport::new(
        &session_audit,
        operational_clocks.clone(),
        Ok(epoch_set(&manifest, session_id)),
    )
    .expect("qualified report is valid");
    qualified
        .bind(&manifest, &session_audit)
        .expect("qualified report binds");
    assert!(qualified.offline_evidence_qualified());

    let unqualified = LiveClockReport::new(
        &session_audit,
        operational_clocks,
        Err("full-session p95 residual exceeded 2 ms".to_owned()),
    )
    .expect("explicitly unqualified report is valid");
    let recovered = LiveClockReport::from_json(
        &unqualified
            .canonical_json()
            .expect("unqualified report serializes"),
    )
    .expect("unqualified report parses");
    recovered
        .bind(&manifest, &session_audit)
        .expect("unqualified report still binds raw session identity");
    assert!(!recovered.offline_evidence_qualified());
    assert!(recovered.full_session_epoch.is_none());
    assert_eq!(
        recovered.offline_qualification_error.as_deref(),
        Some("full-session p95 residual exceeded 2 ms")
    );

    let stopped_during_warmup = LiveClockReport::new(
        &session_audit,
        vec![
            OperationalControllerClock {
                hand: Hand::Left,
                controller_id: "left-controller".to_owned(),
                fit: None,
                diagnostics: OnlineClockDiagnostics::default(),
            },
            OperationalControllerClock {
                hand: Hand::Right,
                controller_id: "right-controller".to_owned(),
                fit: None,
                diagnostics: OnlineClockDiagnostics::default(),
            },
        ],
        Err("capture stopped before full-session timing qualified".to_owned()),
    )
    .expect("pre-arm stop remains an honest report");
    stopped_during_warmup
        .bind(&manifest, &session_audit)
        .expect("pre-arm report binds its raw evidence");
    assert!(!stopped_during_warmup.operational_clocks_ready());
}

#[test]
fn live_clock_report_rejects_different_bytes_with_the_same_session_header() {
    let manifest = manifest();
    let session_id = [0x5a; 16];
    let original = session_audit(&manifest, session_id, Some(1));
    let replacement = session_audit(&manifest, session_id, Some(2));
    assert_eq!(original.header(), replacement.header());
    assert_ne!(original.sha256(), replacement.sha256());

    let report = LiveClockReport::new(
        &original,
        vec![
            OperationalControllerClock {
                hand: Hand::Left,
                controller_id: "left-controller".to_owned(),
                fit: None,
                diagnostics: OnlineClockDiagnostics::default(),
            },
            OperationalControllerClock {
                hand: Hand::Right,
                controller_id: "right-controller".to_owned(),
                fit: None,
                diagnostics: OnlineClockDiagnostics::default(),
            },
        ],
        Err("capture stopped before full-session timing qualified".to_owned()),
    )
    .expect("report binds the original bytes");

    assert_eq!(
        report.bind(&manifest, &replacement),
        Err(invisible_keyboard::clock_epoch::LiveClockReportError::SessionContentMismatch)
    );
}

#[test]
fn wrong_session_controller_or_boot_fails_closed() {
    let manifest = manifest();
    let epochs = epoch_set(&manifest, [9; 16]);
    let wrong_session = SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id: [8; 16],
    };
    assert_eq!(
        epochs.bind(&manifest, &wrong_session),
        Err(ClockEpochError::SessionIdentityMismatch)
    );

    let mut wrong_controller = manifest.clone();
    wrong_controller.hands[1].controller_id = "different-controller".to_owned();
    let wrong_controller_epochs = ClockEpochSet::new(
        wrong_controller.sha256().expect("changed manifest hashes"),
        [9; 16],
        epochs.epochs.clone(),
    )
    .expect("epoch structure remains valid");
    let wrong_controller_header = SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: wrong_controller.sha256().expect("manifest hashes"),
        session_id: [9; 16],
    };
    assert_eq!(
        wrong_controller_epochs.bind(&wrong_controller, &wrong_controller_header),
        Err(ClockEpochError::ControllerIdentityMismatch(Hand::Right))
    );

    let mut wrong_boot = manifest.clone();
    wrong_boot.hands[0].boot_id = "33333333333333333333333333333333".to_owned();
    let wrong_boot_epochs = ClockEpochSet::new(
        wrong_boot.sha256().expect("changed manifest hashes"),
        [9; 16],
        epochs.epochs,
    )
    .expect("epoch structure remains valid");
    let wrong_boot_header = SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: wrong_boot.sha256().expect("manifest hashes"),
        session_id: [9; 16],
    };
    assert_eq!(
        wrong_boot_epochs.bind(&wrong_boot, &wrong_boot_header),
        Err(ClockEpochError::BootIdentityMismatch(Hand::Left))
    );
}

#[test]
fn unknown_fields_and_wrong_hand_order_are_rejected() {
    let manifest = manifest();
    let mut epochs = epoch_set(&manifest, [9; 16]);
    epochs.epochs.swap(0, 1);
    assert_eq!(epochs.validate(), Err(ClockEpochError::InvalidHandOrder));

    let valid = epoch_set(&manifest, [9; 16]);
    let mut source =
        String::from_utf8(valid.canonical_json().expect("serializes")).expect("JSON is UTF-8");
    source.insert_str(source.len() - 1, ",\"keyboard_angle\":15");
    assert!(matches!(
        ClockEpochSet::from_json(source.as_bytes()),
        Err(ClockEpochError::Json(_))
    ));
}

#[test]
fn fresh_startup_frames_bootstrap_both_controller_fits_from_host_receipts() {
    let manifest = manifest();
    let session_id = [7; 16];
    let header = SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id,
    };
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    for index in 0..24_u64 {
        for (hand, offset_ns) in [(Hand::Left, 5_000_000_000), (Hand::Right, 5_001_000_000)] {
            writer
                .append_frame(
                    ControllerStream::from(hand),
                    &SensorFrame {
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
                        sequence: u32::try_from(index).expect("fixture sequence fits"),
                        device_time_us: index * 5_000,
                        sensors: [SensorReading::default(); SENSOR_COUNT],
                    },
                    offset_ns + index * 5_000_000,
                )
                .expect("frame writes");
        }
    }
    let session = writer
        .finish_finalized()
        .expect("session finishes and audits");

    let epochs = ClockEpochSet::fit_session(&manifest, &session).expect("both clock epochs fit");

    epochs.bind(&manifest, &header).expect("fit binds session");
    epochs
        .qualify_session(&manifest, &session)
        .expect("canonical two-hand clocks grant offline evidence authority");
    let reopened = ClockEpochSet::from_json(&epochs.canonical_json().unwrap()).unwrap();
    reopened
        .qualify_session(&manifest, &session)
        .expect("canonical two-hand JSON round trip qualifies");
    assert_eq!(epochs.epochs[0].fit.report().sample_count, 24);
    assert_eq!(epochs.epochs[1].fit.report().sample_count, 24);
}

#[test]
fn one_hand_clock_fit_preserves_the_declared_side() {
    for side in [ManifestHand::Left, ManifestHand::Right] {
        let mut manifest = manifest();
        manifest.hands.retain(|entry| entry.hand == side);
        let identity = manifest.hands[0].frame_identity().unwrap();
        let hand = Hand::from(side);
        let header = SessionHeader {
            nominal_period_us: 5_000,
            manifest_sha256: manifest.sha256().unwrap(),
            session_id: [0x75; 16],
        };
        let mut writer = SessionWriter::new(Vec::new(), header).unwrap();
        let mut samples = Vec::new();
        for index in 0..24_u32 {
            let device_time_us = u64::from(index) * 5_000;
            let host_time_ns = 5_000_000_000 + device_time_us * 1_000;
            writer
                .append_frame(
                    ControllerStream::from(hand),
                    &SensorFrame {
                        hand,
                        controller_id_sha256: identity.controller_id_sha256,
                        boot_id: identity.boot_id,
                        flags: 0,
                        valid_sensor_mask: ALL_SENSORS_VALID,
                        sequence: index,
                        device_time_us,
                        sensors: [SensorReading::default(); SENSOR_COUNT],
                    },
                    host_time_ns,
                )
                .unwrap();
            samples.push(ClockSample {
                device_time_us,
                host_time_ns,
            });
        }
        let session = writer.finish_finalized().unwrap();
        let epochs = ClockEpochSet::fit_session(&manifest, &session)
            .expect("either declared single-hand topology has a clock fit");
        assert_eq!(
            epochs.epochs,
            vec![ControllerClockEpoch {
                hand,
                controller_id: manifest.hands[0].controller_id.clone(),
                fit: ClockFit::fit(identity.boot_id, &samples).unwrap(),
            }]
        );
        let reopened = ClockEpochSet::from_json(&epochs.canonical_json().unwrap()).unwrap();
        reopened.bind(&manifest, &header).unwrap();
        assert_eq!(reopened, epochs);
        reopened
            .qualify_session(&manifest, &session)
            .expect("either canonical single-hand JSON round trip qualifies without changing side");
        let mut forged = serde_json::to_value(&reopened).unwrap();
        forged["epochs"][0]["fit"]["report"]["drift_ppm"] = serde_json::json!(1.0);
        let forged: ClockEpochSet = serde_json::from_value(forged).unwrap();
        assert!(matches!(
            forged.qualify_session(&manifest, &session),
            Err(ClockEpochFitError::NonCanonicalFit)
        ));
        let audit = audit_session_reader(session.bytes()).unwrap();
        let operational = vec![OperationalControllerClock {
            hand,
            controller_id: manifest.hands[0].controller_id.clone(),
            fit: epochs.fit_for(hand).copied(),
            diagnostics: OnlineClockDiagnostics::default(),
        }];
        let live_report = LiveClockReport::new(&audit, operational, Ok(epochs))
            .expect("operational reports accept either declared single-hand topology");
        let live_report =
            LiveClockReport::from_json(&live_report.canonical_json().unwrap()).unwrap();
        live_report.bind(&manifest, &audit).unwrap();
        assert!(live_report.offline_evidence_qualified());
    }
}

#[test]
fn streamed_clock_fit_matches_the_in_memory_reference_exactly() {
    let manifest = manifest();
    let header = SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id: [0x74; 16],
    };
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    let mut left_samples = Vec::new();
    let mut right_samples = Vec::new();
    for index in 0..257_u64 {
        for (hand, offset_ns, samples) in [
            (Hand::Left, 5_000_000_000_u64, &mut left_samples),
            (Hand::Right, 5_001_000_000_u64, &mut right_samples),
        ] {
            let device_time_us = index * 5_000;
            let hand_phase = match hand {
                Hand::Left => 17,
                Hand::Right => 43,
            };
            let jitter_ns = i64::try_from((index * 37 + hand_phase) % 101)
                .expect("fixture jitter fits i64")
                - 50;
            let host_time_ns = (offset_ns + device_time_us * 1_000)
                .checked_add_signed(jitter_ns)
                .expect("fixture host time remains positive");
            samples.push(ClockSample {
                device_time_us,
                host_time_ns,
            });
            writer
                .append_frame(
                    ControllerStream::from(hand),
                    &SensorFrame {
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
                        sequence: u32::try_from(index).expect("fixture sequence fits"),
                        device_time_us,
                        sensors: [SensorReading::default(); SENSOR_COUNT],
                    },
                    host_time_ns,
                )
                .expect("frame writes");
        }
    }
    let session = writer
        .finish_finalized()
        .expect("session finishes and audits");
    let expected_left = ClockFit::fit([0x11; 16], &left_samples).expect("left reference fit");
    let expected_right = ClockFit::fit([0x22; 16], &right_samples).expect("right reference fit");

    let qualification = ClockEpochSet::audit_and_fit_reader(&manifest, session.bytes())
        .expect("streamed session audit succeeds");
    assert_eq!(qualification.session_audit.sha256(), session.sha256());
    assert_eq!(qualification.session_audit.header(), session.header());
    assert_eq!(qualification.session_audit.summary(), session.summary());
    let streamed = qualification
        .clock_epochs
        .expect("streamed clock fit succeeds");

    assert_eq!(streamed.fit_for(Hand::Left), Some(&expected_left));
    assert_eq!(streamed.fit_for(Hand::Right), Some(&expected_right));
    assert_eq!(
        ClockEpochSet::fit_session(&manifest, &session).expect("finalized fit succeeds"),
        streamed
    );
    let reopened = ClockEpochSet::from_json(&streamed.canonical_json().unwrap()).unwrap();
    reopened
        .qualify_session(&manifest, &session)
        .expect("noninteger canonical slope and drift survive JSON admission exactly");
}

#[test]
fn explicitly_unreliable_controller_clock_prevents_epoch_fit() {
    let manifest = manifest();
    let header = SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id: [6; 16],
    };
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    writer
        .append_frame(
            ControllerStream::Left,
            &SensorFrame {
                hand: Hand::Left,
                controller_id_sha256: sha256(b"left-controller"),
                boot_id: [0x11; 16],
                flags: FLAG_CLOCK_UNSYNCED,
                valid_sensor_mask: ALL_SENSORS_VALID,
                sequence: 0,
                device_time_us: 0,
                sensors: [SensorReading::default(); SENSOR_COUNT],
            },
            5_000_000_000,
        )
        .expect("unreliable clock evidence writes");
    let session = writer
        .finish_finalized()
        .expect("session finishes and audits");

    assert!(matches!(
        ClockEpochSet::fit_session(&manifest, &session),
        Err(ClockEpochFitError::UnreliableControllerClock {
            record_index: 0,
            hand: Hand::Left,
        })
    ));
}

#[test]
fn clock_fit_rejects_frames_from_an_undeclared_hand() {
    let mut manifest = manifest();
    manifest.hands.pop();
    let header = SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: manifest.sha256().expect("manifest hashes"),
        session_id: [8; 16],
    };
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    for hand in [Hand::Left, Hand::Right] {
        writer
            .append_frame(
                ControllerStream::from(hand),
                &SensorFrame {
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
                    sequence: 0,
                    device_time_us: 0,
                    sensors: [SensorReading::default(); SENSOR_COUNT],
                },
                5_000_000_000,
            )
            .expect("frame writes");
    }
    let session = writer
        .finish_finalized()
        .expect("session finishes and audits");

    assert!(matches!(
        ClockEpochSet::fit_session(&manifest, &session),
        Err(ClockEpochFitError::FrameIdentity {
            record_index: 1,
            error: FrameIdentityError::UndeclaredHand(Hand::Right),
        })
    ));
}
