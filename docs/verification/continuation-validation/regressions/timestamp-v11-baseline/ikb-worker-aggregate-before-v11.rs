use invisible_keyboard::{
    artifact::KeyboardLayoutConfirmationReceipt,
    calibration_capture::{
        CALIBRATION_MATCHING_RULE_SCHEMA, CONTACT_DEBOUNCE_WINDOWS, CONTACT_FEATURE_SCHEMA,
        CONTACT_REARM_WINDOWS, CONTACT_RESPONSE_END_NS, CONTACT_RESPONSE_START_NS,
        CalibrationCaptureError, CalibrationMatchingRule, CalibrationSurfacePlanBinding,
        CapturedCalibrationEvent, HandContactThresholds, SurfacePresentationReceipt,
    },
    calibration_engine::{EvidenceDomain, EvidencePurpose, EvidenceRole},
    calibration_worker::{
        CalibrationCaptureWorker, CalibrationCaptureWorkerConfig, CalibrationIncidentReason,
        CalibrationWorkerError, CalibrationWorkerFault, KeyboardCaptureContext,
    },
    device_transport::{
        DeviceTransportConfig, DeviceTransportError, DeviceTransportEvent, DeviceTransportFault,
    },
    labels::{
        KeyboardSourceIdentity, LEFT_SHIFT_PHYSICAL_CODE, LabelDisposition, RawKeyState,
        RawKeyboardEvent,
    },
    layout::{Hand, all_slots},
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CORE28_LAYOUT_ID, HandManifest,
        ManifestHand, PROTOCOL_VERSION, SensorManifest, SensorRole, format_sha256,
    },
    neutral::{NeutralRecordWindow, NeutralWindowRole, StabilityVerdict},
    prompts::{HeldOutSurfacePlan, SurfaceSegmentKind, SurfaceTrainingPlan},
    protocol::encode_frame,
    session::{ControllerStream, RawSessionRecord, SessionRecordKind},
    simulation::SyntheticConfig,
    transport::encode_transport_frame,
};

const SESSION_ID: [u8; 16] = [0x71; 16];
const CREATED_UNIX_NS: u64 = 1_786_233_600_000_000_000;
const SURFACE_STARTED_NS: u64 = 6_000_000_000;
const SYNTHETIC_TRIAL_STEP_US: u64 = 250_000;

#[test]
fn first_frame_rejects_out_of_range_sensor_time_without_discarding_raw_bytes() {
    for hand in [Hand::Left, Hand::Right] {
        for overflow in [false, true] {
            let manifest = manifest();
            let mut worker = fitting_worker(manifest.clone(), keyboard_identity());
            let mut frame = synthetic_config(&manifest, hand, 17)
                .idle_window(0)
                .frames()[0];
            frame.device_time_us = if overflow { u64::MAX } else { 0 };
            for sensor in &mut frame.sensors {
                sensor.sample_time_offset_us = 0;
            }
            frame.sensors[0].sample_time_offset_us = if overflow { 1 } else { -1 };
            let error = worker
                .handle_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: 100,
                    bytes: encode_transport_frame(&frame).unwrap(),
                })
                .expect_err("a first sample cannot underflow or overflow device time");
            assert!(error.to_string().contains("sensor 0 time is out of range"));
            let incident = worker
                .close_incident(101, CalibrationIncidentReason::StreamCorruption)
                .unwrap();
            assert!(incident.journal.retention_complete());
            assert!(incident.journal.source_close_complete());
            let session = incident.session.into_admitted().unwrap();
            assert_eq!(
                session.reader().next_record().unwrap().unwrap().payload,
                encode_frame(&frame).unwrap()
            );
        }
    }
}

#[test]
fn worker_integrity_preserves_wrapping_sequences_and_strictly_advancing_sample_times() {
    let manifest = manifest();
    let mut worker = fitting_worker(manifest.clone(), keyboard_identity());
    for sequence in [u32::MAX, 0, 1] {
        for hand in [Hand::Left, Hand::Right] {
            let mut frame = synthetic_config(&manifest, hand, 17)
                .idle_window(0)
                .frames()[0];
            frame.sequence = sequence;
            frame.device_time_us = 100_000 + u64::from(sequence.wrapping_add(1)) * 5_000;
            for sensor in &mut frame.sensors {
                sensor.sample_time_offset_us = 0;
            }
            worker
                .handle_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: 100 + u64::from(sequence.wrapping_add(1)) * 100,
                    bytes: encode_transport_frame(&frame).unwrap(),
                })
                .unwrap();
        }
    }
    worker
        .handle_sensor(DeviceTransportEvent::Tick { host_time_ns: 311 })
        .unwrap();
    for hand in [Hand::Left, Hand::Right] {
        assert_eq!(worker.valid_frame_count(hand), 3);
    }
    let incident = worker
        .close_incident(312, CalibrationIncidentReason::Aborted)
        .unwrap();
    assert!(incident.journal.terminal_fault().is_none());
    assert!(incident.journal.retention_complete());
    assert_eq!(incident.journal.retained_records(), 6);
}

#[test]
fn decoded_same_identity_integrity_faults_retain_raw_suffix_and_first_cause() {
    for hand in [Hand::Left, Hand::Right] {
        for defect in 0..8 {
            let manifest = manifest();
            let keyboard = keyboard_identity();
            let mut worker = fitting_worker(manifest.clone(), keyboard.clone());
            let frames = [Hand::Left, Hand::Right].map(|source| {
                let mut frame = synthetic_config(&manifest, source, 17)
                    .idle_window(0)
                    .frames()[0];
                frame.device_time_us = 100_000;
                for sensor in &mut frame.sensors {
                    sensor.sample_time_offset_us = 0;
                }
                frame
            });
            for source in [Hand::Left, Hand::Right] {
                worker
                    .handle_sensor(DeviceTransportEvent::Bytes {
                        source,
                        receipt_time_ns: 100,
                        bytes: encode_transport_frame(&frames[source as usize]).unwrap(),
                    })
                    .unwrap();
            }
            worker
                .handle_sensor(DeviceTransportEvent::Tick { host_time_ns: 111 })
                .unwrap();
            let mut bad = frames[hand as usize];
            bad.sequence = bad.sequence.wrapping_add(1);
            bad.device_time_us += 1;
            match defect {
                0 => bad.valid_sensor_mask &= !1,
                1 => bad.flags |= invisible_keyboard::protocol::FLAG_CLOCK_UNSYNCED,
                2 => bad.flags |= invisible_keyboard::protocol::FLAG_ACQUISITION_OVERRUN,
                3 => bad.sensors[5].sample_time_offset_us = 5_001,
                4 => bad.sequence = bad.sequence.wrapping_add(1),
                5 => bad.device_time_us = 99_999,
                6 => bad.sensors[0].sample_time_offset_us = -2,
                7 => {
                    bad.device_time_us = 0;
                    bad.sensors[0].sample_time_offset_us = -1;
                }
                _ => unreachable!(),
            }
            let mut later_clean = frames[hand as usize];
            later_clean.sequence = later_clean.sequence.wrapping_add(2);
            later_clean.device_time_us += 10_000;
            let mut bytes = encode_transport_frame(&bad).unwrap();
            bytes.extend(encode_transport_frame(&later_clean).unwrap());
            let first = worker
                .handle_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: 200,
                    bytes,
                })
                .expect_err("bad completed frames must close authority before merge release")
                .to_string();
            assert_eq!(
                worker.checkpoint().err().unwrap().to_string(),
                first,
                "terminal capture cannot produce calibration evidence"
            );
            assert_eq!(
                worker
                    .handle_sensor(DeviceTransportEvent::Bytes {
                        source: hand,
                        receipt_time_ns: 300,
                        bytes: encode_transport_frame(&later_clean).unwrap(),
                    })
                    .unwrap_err()
                    .to_string(),
                first,
                "later clean input cannot reopen authority or replace the first cause",
            );
            assert!(
                worker
                    .handle_keyboard(
                        &keyboard,
                        raw_key(&keyboard.device_id, 30, None, false, RawKeyState::Down, 301),
                        KeyboardCaptureContext::new(
                            EvidenceRole::Training,
                            EvidenceDomain::Keyboard,
                            10
                        ),
                    )
                    .is_err()
            );
            let incident = worker
                .close_incident(302, CalibrationIncidentReason::StreamCorruption)
                .unwrap();
            assert!(incident.journal.source_close_complete());
            assert!(incident.journal.retention_complete());
            assert!(!incident.journal.evidence_eligible());
            assert_eq!(incident.journal.retained_records(), 4);
            assert_eq!(incident.journal.retained_events(), 0);
            let session = incident.session.into_admitted().unwrap();
            let mut reader = session.reader();
            let mut retained = Vec::new();
            while let Some(record) = reader.next_record().unwrap() {
                retained.push(record.payload);
            }
            assert!(
                retained
                    .iter()
                    .any(|payload| payload == &encode_frame(&bad).unwrap())
            );
            assert!(
                retained
                    .iter()
                    .any(|payload| payload == &encode_frame(&later_clean).unwrap())
            );
        }
    }
}

#[test]
fn concurrent_sensor_and_keyboard_capture_builds_source_bound_fitting_evidence() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let surface_plan = SurfaceTrainingPlan::v1();
    let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
        manifest: manifest.clone(),
        keyboard_source: keyboard.clone(),
        purpose: EvidencePurpose::Fitting,
        matching_rule: None,
        session_id: SESSION_ID,
        created_unix_ns: CREATED_UNIX_NS,
        started_host_time_ns: 1,
        surface_plan: Some(
            CalibrationSurfacePlanBinding::fitting(SURFACE_STARTED_NS).expect("fitting plan binds"),
        ),
        transport: transport_config(),
    })
    .expect("worker configuration is valid");
    let configs = [
        synthetic_config(&manifest, Hand::Left, 0x1000),
        synthetic_config(&manifest, Hand::Right, 0x2000),
    ];
    let mut trial = 0_u64;

    for _ in 0..3 {
        push_pair(
            &mut worker,
            &stable_neutral_window(&configs[0], trial),
            &stable_neutral_window(&configs[1], trial),
        );
        trial += 1;
    }
    let neutral_end = worker.record_count();
    assert!(worker.valid_frame_count(Hand::Left) >= 32);
    assert!(worker.valid_frame_count(Hand::Right) >= 32);
    let neutral_windows = vec![NeutralRecordWindow {
        block_id: "neutral-1".to_owned(),
        window_id: "both-hands-still".to_owned(),
        role: NeutralWindowRole::Neutral,
        start_record: 0,
        end_record_exclusive: neutral_end,
    }];
    assert_eq!(
        worker
            .derive_neutral_report(&neutral_windows)
            .expect("live prefix derives a neutral checkpoint")
            .verdict,
        StabilityVerdict::Stable
    );
    worker
        .set_neutral_windows(neutral_windows)
        .expect("neutral window is accepted");

    let identification =
        KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10);
    assert!(matches!(
        worker
            .handle_keyboard(
                &keyboard,
                raw_key(
                    &keyboard.device_id,
                    LEFT_SHIFT_PHYSICAL_CODE,
                    None,
                    true,
                    RawKeyState::Down,
                    1,
                ),
                identification,
            )
            .expect("shift down is recorded"),
        LabelDisposition::Modifier
    ));
    assert!(matches!(
        worker
            .handle_keyboard(
                &keyboard,
                raw_key(
                    &keyboard.device_id,
                    LEFT_SHIFT_PHYSICAL_CODE,
                    None,
                    true,
                    RawKeyState::Up,
                    2,
                ),
                identification,
            )
            .expect("shift up identifies the keyboard"),
        LabelDisposition::Modifier
    ));
    worker
        .confirm_keyboard_layout(KeyboardLayoutConfirmationReceipt::us_qwerty(1, 2))
        .expect("explicit layout receipt is bound before evidence checkpoints");

    let left_position = all_slots()
        .iter()
        .find(|slot| slot.position.hand == Hand::Left && slot.usage.is_some())
        .expect("left Core-28 position exists")
        .position;
    let right_position = all_slots()
        .iter()
        .find(|slot| slot.position.hand == Hand::Right && slot.usage.is_some())
        .expect("right Core-28 position exists")
        .position;
    let left_keyboard_time = push_pair(
        &mut worker,
        &configs[0].tap_window(left_position, trial),
        &configs[1].idle_window(trial),
    )[0];
    trial += 1;
    record_key_tap(
        &mut worker,
        &keyboard,
        left_position,
        left_keyboard_time,
        KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10),
    );
    let right_keyboard_time = push_pair(
        &mut worker,
        &configs[0].idle_window(trial),
        &configs[1].tap_window(right_position, trial),
    )[1];
    trial += 1;
    record_key_tap(
        &mut worker,
        &keyboard,
        right_position,
        right_keyboard_time,
        KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 11),
    );

    let mut presentation_revision = 1_u64;
    let mut final_surface_time = 0_u64;
    for segment in surface_plan.segments() {
        for scheduled in segment.scheduled_presentations() {
            let planned_host_time_ns = SURFACE_STARTED_NS + scheduled.at_ns();
            worker
                .record_display_acknowledgement(
                    segment.id().clone(),
                    surface_presentation(planned_host_time_ns, presentation_revision),
                )
                .expect("canonical display acknowledgement is recorded");
            presentation_revision += 1;
        }
        match segment.kind() {
            SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap => {
                let position = surface_plan
                    .scoring_targets()
                    .iter()
                    .find(|target| target.prompt_id() == segment.id())
                    .expect("scoring prompt has a target")
                    .position();
                let acknowledged_host_time_ns = SURFACE_STARTED_NS
                    + segment
                        .cue_at_ns()
                        .expect("tap segment owns a canonical cue");
                while next_pair_end_host_time_ns(&configs[0], trial)
                    < acknowledged_host_time_ns + 200_000_000
                {
                    push_pair(
                        &mut worker,
                        &configs[0].idle_window(trial),
                        &configs[1].idle_window(trial),
                    );
                    trial += 1;
                }
                let windows = match position.hand {
                    Hand::Left => (
                        configs[0].tap_window(position, trial),
                        configs[1].idle_window(trial),
                    ),
                    Hand::Right => (
                        configs[0].idle_window(trial),
                        configs[1].tap_window(position, trial),
                    ),
                };
                let times = push_pair(&mut worker, &windows.0, &windows.1);
                final_surface_time = times[position.hand as usize];
                assert!(
                    final_surface_time >= acknowledged_host_time_ns
                        && final_surface_time <= acknowledged_host_time_ns + 1_500_000_000,
                    "surface contact at {final_surface_time} must follow cue {acknowledged_host_time_ns}"
                );
                trial += 1;
            }
            SurfaceSegmentKind::QuietNull => {
                let earliest_host_time_ns = SURFACE_STARTED_NS + segment.start_ns() + 100_000_000;
                while next_pair_end_host_time_ns(&configs[0], trial) < earliest_host_time_ns {
                    push_pair(
                        &mut worker,
                        &configs[0].idle_window(trial),
                        &configs[1].idle_window(trial),
                    );
                    trial += 1;
                }
                let times = push_pair(
                    &mut worker,
                    &configs[0].idle_window(trial),
                    &configs[1].idle_window(trial),
                );
                final_surface_time = times[1];
                assert!(final_surface_time < SURFACE_STARTED_NS + segment.end_ns());
                for (sample_ordinal, hand) in [(0, Hand::Left), (1, Hand::Right)] {
                    worker
                        .record_idle(segment.id().clone(), sample_ordinal, times[hand as usize])
                        .expect("canonical quiet fact is recorded");
                }
                trial += 1;
            }
            SurfaceSegmentKind::ThresholdQuiet => {
                let hand = segment
                    .motion_hand()
                    .expect("threshold quiet names one hand");
                let earliest_host_time_ns = SURFACE_STARTED_NS + segment.start_ns() + 100_000_000;
                while next_pair_end_host_time_ns(&configs[0], trial) < earliest_host_time_ns {
                    push_pair(
                        &mut worker,
                        &configs[0].idle_window(trial),
                        &configs[1].idle_window(trial),
                    );
                    trial += 1;
                }
                let times = push_pair(
                    &mut worker,
                    &configs[0].idle_window(trial),
                    &configs[1].idle_window(trial),
                );
                final_surface_time = times[hand as usize];
                assert!(final_surface_time < SURFACE_STARTED_NS + segment.end_ns());
                worker
                    .record_idle(segment.id().clone(), 0, final_surface_time)
                    .expect("canonical threshold-quiet fact is recorded");
                trial += 1;
            }
            SurfaceSegmentKind::OrdinaryMotion => {
                panic!("fitting plans contain no ordinary-motion segments")
            }
        }
    }

    let bundle = worker
        .finish(final_surface_time.saturating_add(1_000_000))
        .expect("real contracts build a fitting bundle");

    assert_eq!(bundle.session.header().session_id, SESSION_ID);
    assert_eq!(
        bundle.session.header().manifest_sha256,
        manifest.sha256().expect("manifest hashes")
    );
    assert_eq!(bundle.evidence.session_id(), SESSION_ID);
    assert_eq!(bundle.evidence.observations().len(), 120);
    assert_eq!(bundle.capture_report.raw_keyboard_events, 6);
    assert_eq!(bundle.capture_report.accepted_key_downs, 2);
    assert_eq!(
        bundle
            .neutral_report
            .as_ref()
            .expect("fitting capture derives neutral evidence")
            .verdict,
        StabilityVerdict::Stable
    );
    assert_eq!(
        bundle.evidence.calibration_matching_rule_sha256(),
        format_sha256(
            &bundle
                .capture_report
                .matching_rule
                .fingerprint()
                .expect("matching rule hashes")
        )
    );
    assert!(
        bundle
            .journal
            .events
            .iter()
            .any(|event| { matches!(event, CapturedCalibrationEvent::DisplayAcknowledged { .. }) })
    );
}

#[test]
fn fitting_finish_rejects_an_incomplete_surface_plan_before_matching() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let plan = SurfaceTrainingPlan::v1();
    let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
        manifest: manifest.clone(),
        keyboard_source: keyboard.clone(),
        purpose: EvidencePurpose::Fitting,
        matching_rule: None,
        session_id: [0x73; 16],
        created_unix_ns: CREATED_UNIX_NS,
        started_host_time_ns: 1,
        surface_plan: Some(CalibrationSurfacePlanBinding::fitting(1).expect("fitting plan binds")),
        transport: transport_config(),
    })
    .expect("worker configuration is valid");
    let configs = [
        synthetic_config(&manifest, Hand::Left, 0x9100),
        synthetic_config(&manifest, Hand::Right, 0x9200),
    ];
    let mut times = [0; 2];
    for trial in 0..3 {
        times = push_pair(
            &mut worker,
            &stable_neutral_window(&configs[0], trial),
            &stable_neutral_window(&configs[1], trial),
        );
    }
    worker
        .set_neutral_windows(vec![NeutralRecordWindow {
            block_id: "neutral-incomplete-plan".to_owned(),
            window_id: "both-hands-still".to_owned(),
            role: NeutralWindowRole::Neutral,
            start_record: 0,
            end_record_exclusive: worker.record_count(),
        }])
        .expect("neutral prefix is retained");
    for (state, host_time_ns) in [(RawKeyState::Down, 10), (RawKeyState::Up, 11)] {
        worker
            .handle_keyboard(
                &keyboard,
                raw_key(
                    &keyboard.device_id,
                    LEFT_SHIFT_PHYSICAL_CODE,
                    None,
                    true,
                    state,
                    host_time_ns,
                ),
                KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10),
            )
            .expect("keyboard identification fact is retained");
    }
    worker
        .confirm_keyboard_layout(KeyboardLayoutConfirmationReceipt::us_qwerty(1, 1))
        .expect("layout receipt is bound");
    worker
        .record_display_acknowledgement(plan.segments()[0].id().clone(), surface_presentation(1, 1))
        .expect("canonical first prompt is retained");

    assert!(matches!(
        worker.finish(times[1].saturating_add(1_000_000)),
        Err(CalibrationWorkerError::Capture(
            CalibrationCaptureError::MissingSurfacePrompt(_)
        ))
    ));
}

#[test]
fn worker_refuses_to_bind_a_surface_plan_after_the_first_surface_fact() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let plan = SurfaceTrainingPlan::v1();
    let mut worker = fitting_worker(manifest, keyboard);
    worker
        .record_display_acknowledgement(plan.segments()[0].id().clone(), surface_presentation(2, 1))
        .expect("the raw fact can be buffered before journal assembly");

    assert!(matches!(
        worker.bind_surface_plan(
            CalibrationSurfacePlanBinding::fitting(1).expect("fitting plan binds")
        ),
        Err(CalibrationWorkerError::InvalidConfiguration)
    ));
}

#[test]
fn selected_keyboard_identity_change_faults_the_capture_permanently() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let mut changed = keyboard.clone();
    changed.physical_path = "fixture/replugged-input0".to_owned();
    let mut worker = fitting_worker(manifest, keyboard.clone());

    let error = worker
        .handle_keyboard(
            &changed,
            raw_key(
                &keyboard.device_id,
                LEFT_SHIFT_PHYSICAL_CODE,
                None,
                true,
                RawKeyState::Down,
                1,
            ),
            KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10),
        )
        .expect_err("a changed physical source must stop capture");
    assert!(matches!(
        error,
        CalibrationWorkerError::Faulted(CalibrationWorkerFault::KeyboardSourceChanged)
    ));
    assert!(matches!(
        worker.record_display_acknowledgement(
            SurfaceTrainingPlan::v1().segments()[0].id().clone(),
            surface_presentation(2, 1),
        ),
        Err(CalibrationWorkerError::Faulted(
            CalibrationWorkerFault::KeyboardSourceChanged
        ))
    ));
}

fn surface_presentation(
    planned_host_time_ns: u64,
    instruction_revision: u64,
) -> SurfacePresentationReceipt {
    SurfacePresentationReceipt::new(
        planned_host_time_ns,
        planned_host_time_ns,
        planned_host_time_ns,
        instruction_revision,
    )
    .expect("fixture surface presentation is ordered")
}

fn next_pair_end_host_time_ns(config: &SyntheticConfig, trial: u64) -> u64 {
    let window = config.idle_window(trial);
    let frame = window
        .frames()
        .last()
        .expect("synthetic pair window is non-empty");
    1_000_000_000 + fixture_device_time_us(frame.device_time_us) * 1_000
}

fn fixture_device_time_us(synthetic_device_time_us: u64) -> u64 {
    let trial = synthetic_device_time_us / 1_000_000;
    let within_trial_us = synthetic_device_time_us % 1_000_000;
    trial * SYNTHETIC_TRIAL_STEP_US + within_trial_us
}

#[test]
fn terminal_capacity_rejection_retains_exact_worker_incident() {
    assert_terminal_transport_incident(DeviceTransportError::PendingCapacityExceeded {
        maximum: 2,
        required: 4,
    });
}

#[test]
fn terminal_late_publication_retains_exact_worker_incident() {
    assert_terminal_transport_incident(DeviceTransportError::MergeLatenessExceeded {
        source: Hand::Left,
        receipt_time_ns: 102,
        watermark_ns: 190,
    });
}

#[test]
fn terminal_receipt_regression_retains_exact_worker_incident() {
    assert_terminal_transport_incident(DeviceTransportError::ReceiptRegression {
        source: Hand::Left,
        previous: 101,
        actual: 90,
    });
}

#[test]
fn transport_rejection_retention_bound_preserves_primary_cause() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let mut config = transport_config();
    config.max_pending_candidates = 1;
    let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
        manifest: manifest.clone(),
        keyboard_source: keyboard,
        purpose: EvidencePurpose::Fitting,
        matching_rule: None,
        session_id: SESSION_ID,
        created_unix_ns: CREATED_UNIX_NS,
        started_host_time_ns: 1,
        surface_plan: None,
        transport: config,
    })
    .expect("bounded real worker starts");
    let window = synthetic_config(&manifest, Hand::Left, 0x3200).idle_window(0);
    let encoded = encode_transport_frame(&window.frames()[0]).expect("real fixture frame encodes");
    // Exceed the actual record-retention limit, without relaxing it or injecting
    // internal counters. The transport rejection remains the primary failure.
    let owned_candidates = 300_001;
    let expected_error = DeviceTransportError::PendingCapacityExceeded {
        maximum: 1,
        required: owned_candidates,
    };
    assert!(matches!(
        worker.handle_sensor(DeviceTransportEvent::Bytes {
            source: Hand::Left,
            receipt_time_ns: 100,
            bytes: encoded.repeat(owned_candidates),
        }),
        Err(CalibrationWorkerError::Transport(error)) if error == expected_error
    ));
    assert_eq!(worker.record_count(), 300_000);
    assert_eq!(worker.valid_frame_count(Hand::Left), 300_000);
    let incident = worker
        .close_incident(101, CalibrationIncidentReason::SourceFault)
        .expect("bounded diagnostic evidence closes after secondary retention loss");
    assert_eq!(incident.journal.retained_records(), 300_000);
    assert_eq!(incident.session.summary().decoded_candidates, 300_000);
    assert_eq!(incident.session.summary().left.received, 300_000);
    assert_eq!(incident.session.summary().left.duplicates, 299_999);
    assert!(incident.journal.source_close_complete());
    assert!(!incident.journal.evidence_eligible());
    assert_eq!(
        incident.journal.terminal_fault(),
        Some(format!("controller transport admission failed: {expected_error}").as_str())
    );
    let journal: serde_json::Value =
        serde_json::from_slice(incident.journal_bytes()).expect("incident journal reads");
    assert_eq!(
        journal["retention_complete"], false,
        "secondary raw retention loss must remain visible without replacing the first cause"
    );
}

fn assert_terminal_transport_incident(expected_error: DeviceTransportError) {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let mut config = transport_config();
    if matches!(
        expected_error,
        DeviceTransportError::PendingCapacityExceeded { .. }
    ) {
        config.max_pending_candidates = 2;
    }
    let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
        manifest: manifest.clone(),
        keyboard_source: keyboard.clone(),
        purpose: EvidencePurpose::Fitting,
        matching_rule: None,
        session_id: SESSION_ID,
        created_unix_ns: CREATED_UNIX_NS,
        started_host_time_ns: 1,
        surface_plan: None,
        transport: config,
    })
    .expect("real manifest and bounded transport start");
    let context = KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10);
    let keyboard_event = raw_key(
        &keyboard.device_id,
        LEFT_SHIFT_PHYSICAL_CODE,
        None,
        true,
        RawKeyState::Down,
        10,
    );
    worker
        .handle_keyboard(&keyboard, keyboard_event.clone(), context)
        .expect("original keyboard fact is retained");
    let left = synthetic_config(&manifest, Hand::Left, 0x3100).idle_window(0);
    let right = synthetic_config(&manifest, Hand::Right, 0x4100).idle_window(0);
    let left_split = encode_transport_frame(&left.frames()[1]).expect("split frame encodes");
    let right_split = encode_transport_frame(&right.frames()[1]).expect("right frame encodes");
    let split = left_split.len() / 2;
    let right_prefix = right_split[..right_split.len() / 2].to_vec();
    for (source, receipt_time_ns, bytes) in [
        (
            Hand::Left,
            100,
            encode_transport_frame(&left.frames()[0]).expect("left prefix frame encodes"),
        ),
        (
            Hand::Right,
            100,
            encode_transport_frame(&right.frames()[0]).expect("right prefix frame encodes"),
        ),
        (Hand::Left, 101, left_split[..split].to_vec()),
        (Hand::Right, 101, right_prefix.clone()),
    ] {
        worker
            .handle_sensor(DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            })
            .expect("admitted prefix and split bytes remain in the original decoder");
    }
    assert_eq!(
        worker.record_count(),
        0,
        "both complete frames await the watermark"
    );
    if matches!(
        expected_error,
        DeviceTransportError::MergeLatenessExceeded { .. }
    ) {
        worker
            .handle_sensor(DeviceTransportEvent::Tick { host_time_ns: 200 })
            .expect("watermark publishes only the admitted complete frames");
        assert_eq!(worker.record_count(), 2);
    }
    let rejected_time = match expected_error {
        DeviceTransportError::ReceiptRegression { actual, .. } => actual,
        _ => 102,
    };
    let suffix = vec![0x03, 0x7f];
    let mut rejected = left_split[split..].to_vec();
    rejected.extend(encode_transport_frame(&left.frames()[2]).expect("second frame encodes"));
    rejected.extend(&suffix);
    assert!(matches!(
        worker.handle_sensor(DeviceTransportEvent::Bytes {
            source: Hand::Left,
            receipt_time_ns: rejected_time,
            bytes: rejected,
        }),
        Err(CalibrationWorkerError::Transport(error)) if error == expected_error
    ));
    assert_eq!(
        worker.record_count(),
        6,
        "terminal closure retains each candidate once"
    );
    assert_eq!(worker.valid_frame_count(Hand::Left), 3);
    assert_eq!(worker.valid_frame_count(Hand::Right), 1);
    assert!(
        matches!(
            worker.handle_keyboard(&keyboard, keyboard_event.clone(), context),
            Err(CalibrationWorkerError::Transport(error)) if error == expected_error
        ),
        "the first typed cause persists and labels cannot rearm"
    );
    let plan = SurfaceTrainingPlan::v1();
    let prompt = plan.segments()[0].id().clone();
    assert!(
        matches!(
            worker.record_display_acknowledgement(prompt.clone(), surface_presentation(300, 1)),
            Err(CalibrationWorkerError::Transport(error)) if error == expected_error
        ),
        "display acknowledgements cannot rearm labels or replace the first cause"
    );
    assert!(
        matches!(
            worker.record_idle(prompt, 0, 300),
            Err(CalibrationWorkerError::Transport(error)) if error == expected_error
        ),
        "scheduled nulls cannot rearm labels or replace the first cause"
    );
    assert!(
        matches!(
            worker.handle_sensor(DeviceTransportEvent::Bytes {
                source: Hand::Right,
                receipt_time_ns: 300,
                bytes: right_split,
            }),
            Err(CalibrationWorkerError::Transport(error)) if error == expected_error
        ),
        "later sensor input cannot reopen or duplicate the closed decoder"
    );
    assert_eq!(worker.record_count(), 6);
    let incident = worker
        .close_incident(400, CalibrationIncidentReason::SourceFault)
        .expect("terminal evidence closes without clocks or matching");
    assert_eq!(
        incident.journal.reason(),
        CalibrationIncidentReason::SourceFault
    );
    assert!(!incident.journal.evidence_eligible());
    assert!(incident.journal.source_close_complete());
    assert!(incident.journal.close_errors().is_empty());
    assert_eq!(
        incident.journal.terminal_fault(),
        Some(format!("controller transport admission failed: {expected_error}").as_str())
    );
    assert_eq!(incident.journal.retained_records(), 6);
    assert_eq!(incident.journal.retained_events(), 1);
    assert_eq!(incident.session.summary().total_records, 6);
    assert_eq!(incident.session.summary().invalid_records, 2);
    assert_eq!(incident.session.summary().decoded_candidates, 4);
    assert_eq!(incident.session.summary().unterminated_candidates, 2);
    assert_eq!(incident.session.summary().left.received, 3);
    assert_eq!(incident.session.summary().right.received, 1);
    assert_eq!(
        incident.session.summary().left.host_time_regressions,
        u64::from(rejected_time < 100),
    );
    let record = |source, kind, time, payload: Vec<u8>| RawSessionRecord {
        source,
        kind,
        original_len: u64::try_from(payload.len()).expect("fixture payload length fits"),
        host_time_ns: time,
        payload,
    };
    let expected = vec![
        record(
            ControllerStream::Left,
            SessionRecordKind::Decoded,
            100,
            encode_frame(&left.frames()[0])
                .expect("left frame payload encodes")
                .to_vec(),
        ),
        record(
            ControllerStream::Right,
            SessionRecordKind::Decoded,
            100,
            encode_frame(&right.frames()[0])
                .expect("right frame payload encodes")
                .to_vec(),
        ),
        record(
            ControllerStream::Left,
            SessionRecordKind::Decoded,
            rejected_time,
            encode_frame(&left.frames()[1])
                .expect("completed split payload encodes")
                .to_vec(),
        ),
        record(
            ControllerStream::Left,
            SessionRecordKind::Decoded,
            rejected_time,
            encode_frame(&left.frames()[2])
                .expect("rejected frame payload encodes")
                .to_vec(),
        ),
        record(
            ControllerStream::Left,
            SessionRecordKind::Unterminated,
            rejected_time,
            suffix,
        ),
        record(
            ControllerStream::Right,
            SessionRecordKind::Unterminated,
            101,
            right_prefix,
        ),
    ];
    let replay = incident
        .session
        .admit()
        .expect("diagnostic records pass structural session audit");
    let mut reader = replay.reader();
    let mut actual = Vec::new();
    while let Some(record) = reader
        .next_record()
        .expect("diagnostic record and footer read")
    {
        actual.push(record);
    }
    assert_eq!(
        actual, expected,
        "exact payloads, lengths, times and order survive rejection"
    );
    assert!(
        reader
            .next_record()
            .expect("footer remains closed")
            .is_none()
    );
    let journal: serde_json::Value =
        serde_json::from_slice(incident.journal_bytes()).expect("incident journal is JSON");
    assert_eq!(journal["retention_complete"], true);
    assert_eq!(journal["closed_host_time_ns"], 400);
    assert_eq!(journal["events"].as_array().expect("event array").len(), 1);
    assert_eq!(
        journal["events"][0]["event"],
        serde_json::to_value(&keyboard_event).expect("original keyboard fact serializes")
    );
    for forbidden in [
        "clock_epochs_sha256",
        "evidence",
        "artifact",
        "validation_report",
    ] {
        assert!(
            journal.get(forbidden).is_none(),
            "incident must not authorize {forbidden}"
        );
    }
}

#[test]
fn wrong_controller_frame_is_retained_and_faults_the_capture() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let mut worker = fitting_worker(manifest.clone(), keyboard);
    let configs = [
        synthetic_config(&manifest, Hand::Left, 0x3000),
        synthetic_config(&manifest, Hand::Right, 0x4000),
    ];
    let mut wrong_left = configs[0].idle_window(0).frames()[0];
    wrong_left.controller_id_sha256 = [0xee; 32];
    let valid_right = configs[1].idle_window(0).frames()[0];

    let events = [
        DeviceTransportEvent::Bytes {
            source: Hand::Left,
            receipt_time_ns: 1_000_000_000,
            bytes: encode_transport_frame(&wrong_left).expect("wrong-identity frame encodes"),
        },
        DeviceTransportEvent::Bytes {
            source: Hand::Right,
            receipt_time_ns: 1_000_000_000,
            bytes: encode_transport_frame(&valid_right).expect("right frame encodes"),
        },
        DeviceTransportEvent::Tick {
            host_time_ns: 1_000_000_011,
        },
    ];
    let mut terminal = None;
    for event in events {
        if let Err(error) = worker.handle_sensor(event) {
            terminal = Some(error);
            break;
        }
    }
    assert!(
        matches!(
            &terminal,
            Some(CalibrationWorkerError::Faulted(
                CalibrationWorkerFault::Transport(DeviceTransportFault::IdentityMismatch {
                    source: Hand::Left,
                    source_ordinal: 0,
                    ..
                })
            ))
        ),
        "unexpected terminal state: {terminal:?}"
    );
    assert_eq!(
        worker.record_count(),
        1,
        "the terminal identity mismatch must remain in raw evidence"
    );
}

#[test]
fn fitting_requires_a_retained_neutral_window_before_finalization() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let mut worker = fitting_worker(manifest.clone(), keyboard.clone());
    let configs = [
        synthetic_config(&manifest, Hand::Left, 0x5000),
        synthetic_config(&manifest, Hand::Right, 0x6000),
    ];
    push_pair(
        &mut worker,
        &configs[0].idle_window(0),
        &configs[1].idle_window(0),
    );
    worker
        .handle_keyboard(
            &keyboard,
            raw_key(
                &keyboard.device_id,
                LEFT_SHIFT_PHYSICAL_CODE,
                None,
                true,
                RawKeyState::Down,
                1_100_000_000,
            ),
            KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10),
        )
        .expect("keyboard event is retained");

    assert!(matches!(
        worker.finish(1_200_000_000),
        Err(CalibrationWorkerError::InvalidNeutralWindows)
    ));
}

#[test]
fn fitting_checkpoint_refuses_to_infer_layout_without_a_confirmation_receipt() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let mut worker = fitting_worker(manifest.clone(), keyboard.clone());
    let configs = [
        synthetic_config(&manifest, Hand::Left, 0x7000),
        synthetic_config(&manifest, Hand::Right, 0x8000),
    ];

    for trial in 0..3 {
        push_pair(
            &mut worker,
            &stable_neutral_window(&configs[0], trial),
            &stable_neutral_window(&configs[1], trial),
        );
    }
    worker
        .set_neutral_windows(vec![NeutralRecordWindow {
            block_id: "neutral-layout-receipt".to_owned(),
            window_id: "both-hands-still".to_owned(),
            role: NeutralWindowRole::Neutral,
            start_record: 0,
            end_record_exclusive: worker.record_count(),
        }])
        .expect("the retained neutral prefix is valid");

    let context = KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10);
    for (state, host_time_ns) in [(RawKeyState::Down, 10), (RawKeyState::Up, 11)] {
        worker
            .handle_keyboard(
                &keyboard,
                raw_key(
                    &keyboard.device_id,
                    LEFT_SHIFT_PHYSICAL_CODE,
                    None,
                    true,
                    state,
                    host_time_ns,
                ),
                context,
            )
            .expect("the selected keyboard identification event is retained");
    }
    assert!(worker.keyboard_identified());

    assert!(matches!(
        worker.checkpoint(),
        Err(CalibrationWorkerError::KeyboardLayoutNotConfirmed)
    ));
}

#[test]
fn held_out_capture_inherits_the_frozen_rule_and_rejects_keyboard_labels() {
    let manifest = manifest();
    let keyboard = keyboard_identity();
    let plan = HeldOutSurfacePlan::v1([0x44; 32], 9);
    let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
        manifest,
        keyboard_source: keyboard.clone(),
        purpose: EvidencePurpose::HeldOutValidation,
        matching_rule: Some(matching_rule()),
        session_id: [0x72; 16],
        created_unix_ns: CREATED_UNIX_NS,
        started_host_time_ns: 1,
        surface_plan: Some(
            CalibrationSurfacePlanBinding::held_out(&plan, 1).expect("held-out plan binds"),
        ),
        transport: transport_config(),
    })
    .expect("held-out capture accepts one inherited rule");

    assert!(matches!(
        worker.handle_keyboard(
            &keyboard,
            raw_key(
                &keyboard.device_id,
                30,
                Some(all_slots()[0].position),
                false,
                RawKeyState::Down,
                10,
            ),
            KeyboardCaptureContext::new(
                EvidenceRole::HeldOutValidation,
                EvidenceDomain::Keyboard,
                40,
            ),
        ),
        Err(CalibrationWorkerError::KeyboardNotAllowed)
    ));

    let state_only = KeyboardCaptureContext::new(
        EvidenceRole::HeldOutValidation,
        EvidenceDomain::Keyboard,
        40,
    )
    .state_only();
    for (state, host_time_ns) in [(RawKeyState::Down, 11), (RawKeyState::Up, 12)] {
        worker
            .handle_keyboard(
                &keyboard,
                raw_key(
                    &keyboard.device_id,
                    30,
                    Some(all_slots()[0].position),
                    false,
                    state,
                    host_time_ns,
                ),
                state_only,
            )
            .expect("held-out capture retains keyboard activity without making a candidate");
    }
    assert_eq!(worker.held_key_count(), 0);
}

fn push_pair(
    worker: &mut CalibrationCaptureWorker,
    left: &invisible_keyboard::decoder::TapWindow,
    right: &invisible_keyboard::decoder::TapWindow,
) -> [u64; 2] {
    let mut last = [0_u64; 2];
    for index in 0..left.frames().len() {
        for (hand, frame) in [
            (Hand::Left, left.frames()[index]),
            (Hand::Right, right.frames()[index]),
        ] {
            let mut frame = frame;
            frame.device_time_us = fixture_device_time_us(frame.device_time_us);
            let receipt_time_ns = 1_000_000_000 + frame.device_time_us * 1_000;
            last[hand as usize] = receipt_time_ns;
            worker
                .handle_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns,
                    bytes: encode_transport_frame(&frame).expect("frame encodes"),
                })
                .expect("sensor bytes are retained");
        }
    }
    worker
        .handle_sensor(DeviceTransportEvent::Tick {
            host_time_ns: last.into_iter().max().expect("pair has times") + 11,
        })
        .expect("merge watermark advances");
    last
}

fn record_key_tap(
    worker: &mut CalibrationCaptureWorker,
    keyboard: &KeyboardSourceIdentity,
    position: invisible_keyboard::layout::PhysicalPosition,
    host_time_ns: u64,
    context: KeyboardCaptureContext,
) {
    let physical_code = 30 + u16::from(position.slot);
    assert!(matches!(
        worker
            .handle_keyboard(
                keyboard,
                raw_key(
                    &keyboard.device_id,
                    physical_code,
                    Some(position),
                    false,
                    RawKeyState::Down,
                    host_time_ns,
                ),
                context,
            )
            .expect("key down is retained"),
        LabelDisposition::Accepted(_)
    ));
    assert!(matches!(
        worker
            .handle_keyboard(
                keyboard,
                raw_key(
                    &keyboard.device_id,
                    physical_code,
                    Some(position),
                    false,
                    RawKeyState::Up,
                    host_time_ns + 1,
                ),
                context,
            )
            .expect("key up is retained"),
        LabelDisposition::Accepted(_)
    ));
}

fn raw_key(
    device_id: &str,
    physical_code: u16,
    position: Option<invisible_keyboard::layout::PhysicalPosition>,
    modifier: bool,
    state: RawKeyState,
    host_time_ns: u64,
) -> RawKeyboardEvent {
    RawKeyboardEvent {
        device_id: device_id.to_owned(),
        physical_code,
        position,
        modifier,
        state,
        kernel_time_ns: Some(host_time_ns),
        host_time_ns,
    }
}

fn keyboard_identity() -> KeyboardSourceIdentity {
    KeyboardSourceIdentity {
        device_id: "/dev/input/event9:fixture".to_owned(),
        name: "Fixture USB Keyboard".to_owned(),
        physical_path: "fixture/input0".to_owned(),
        vendor_id: 0x1234,
        product_id: 0x5678,
    }
}

fn synthetic_config(manifest: &AcquisitionManifest, hand: Hand, seed: u64) -> SyntheticConfig {
    let identity = manifest.hands[hand as usize]
        .frame_identity()
        .expect("manifest has a wire identity");
    SyntheticConfig::new(hand, seed)
        .with_noise(0)
        .with_shared_motion(0)
        .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
}

fn fitting_worker(
    manifest: AcquisitionManifest,
    keyboard_source: KeyboardSourceIdentity,
) -> CalibrationCaptureWorker {
    CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
        manifest,
        keyboard_source,
        purpose: EvidencePurpose::Fitting,
        matching_rule: None,
        session_id: SESSION_ID,
        created_unix_ns: CREATED_UNIX_NS,
        started_host_time_ns: 1,
        surface_plan: None,
        transport: transport_config(),
    })
    .expect("fitting worker configuration is valid")
}

const fn transport_config() -> DeviceTransportConfig {
    DeviceTransportConfig {
        merge_lateness_ns: 10,
        source_silence_timeout_ns: u64::MAX / 2,
        max_pending_candidates: 4_096,
    }
}

fn matching_rule() -> CalibrationMatchingRule {
    CalibrationMatchingRule {
        schema: CALIBRATION_MATCHING_RULE_SCHEMA.to_owned(),
        acquisition_manifest_sha256: format_sha256(&[0x11; 32]),
        neutral_windows_sha256: format_sha256(&[0x22; 32]),
        neutral_source_sha256: format_sha256(&[0x33; 32]),
        feature_schema: CONTACT_FEATURE_SCHEMA.to_owned(),
        thresholds: [
            HandContactThresholds {
                hand: Hand::Left,
                low_energy: 1_024,
                high_energy: 4_096,
                motion_energy: 4_096,
            },
            HandContactThresholds {
                hand: Hand::Right,
                low_energy: 1_024,
                high_energy: 4_096,
                motion_energy: 4_096,
            },
        ],
        debounce_windows: CONTACT_DEBOUNCE_WINDOWS,
        rearm_windows: CONTACT_REARM_WINDOWS,
        response_start_ns: CONTACT_RESPONSE_START_NS,
        response_end_ns: CONTACT_RESPONSE_END_NS,
    }
}

fn stable_neutral_window(
    config: &SyntheticConfig,
    trial: u64,
) -> invisible_keyboard::decoder::TapWindow {
    let mut frames = config.idle_window(trial).frames().to_vec();
    for (sample_index, frame) in frames.iter_mut().enumerate() {
        let small_variation =
            i16::try_from(sample_index % 3).expect("small remainder fits i16") - 1;
        for (sensor_index, sensor) in frame.sensors.iter_mut().enumerate() {
            // SyntheticConfig generates decoder fixtures around zero. Neutral
            // evidence instead needs an observable gravity vector, so this
            // fixture mirrors the stable corpus used by the engine contract.
            sensor.acceleration = [0, 0, 16_000 + small_variation];
            sensor.angular_velocity = [0, 0, 0];
            sensor.sample_time_offset_us =
                i16::try_from(sensor_index).expect("sensor index fits i16") * 20;
        }
    }
    invisible_keyboard::decoder::TapWindow::new(frames).expect("neutral frames form a window")
}

fn manifest() -> AcquisitionManifest {
    AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![
            hand_manifest(ManifestHand::Left, 0x11),
            hand_manifest(ManifestHand::Right, 0x22),
        ],
        controller_board: "Seeed XIAO nRF52840 Plus SKU 102010672".to_owned(),
        firmware_build: "independent-firmware-v2".to_owned(),
        host_build: "independent-host-v2".to_owned(),
        sensor_model: "LSM6DS3TR-C".to_owned(),
        sensors_per_hand: 6,
        sensor_odr_hz: 208,
        accelerometer_range_g: 4,
        gyroscope_range_dps: 1_000,
        filter_profile: "gate0-raw-v1".to_owned(),
        i2c_bus_hz: 400_000,
        nominal_period_us: 5_000,
        prompt_corpus_sha256: format_sha256(
            &invisible_keyboard::prompts::PromptCorpus::v1().sha256(),
        ),
        study_id: "worker-contract".to_owned(),
        split_id: "fitting".to_owned(),
        participant_alias: "fixture".to_owned(),
        consent_record: "fixture-consent".to_owned(),
        retention_policy: "local-test-only".to_owned(),
        rig_evidence_sha256: format_sha256(&[0xbb; 32]),
        axis_characterization_sha256: format_sha256(&[0xcc; 32]),
        calibration_artifact_sha256: None,
        calibration_validation_sha256: None,
    }
}

fn hand_manifest(hand: ManifestHand, boot: u8) -> HandManifest {
    let side = match hand {
        ManifestHand::Left => "left",
        ManifestHand::Right => "right",
    };
    HandManifest {
        hand,
        controller_id: format!("xiao-{side}-001"),
        boot_id: format!("{boot:02x}").repeat(16),
        sensors: std::array::from_fn(|index| SensorManifest {
            sensor_index: index as u8,
            mux_channel: index as u8,
            role: [
                SensorRole::Thumb,
                SensorRole::Index,
                SensorRole::Middle,
                SensorRole::Ring,
                SensorRole::Pinky,
                SensorRole::Dorsal,
            ][index],
            mount_id: format!("strap-{side}-{index}"),
            wiring_id: format!("wiring-{side}-{index}"),
            interrupt_pin: format!("gpio-{side}-{index}"),
            axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
        }),
    }
}


#[test]
fn repeated_aggregate_with_advancing_offsets_faults_worker_and_retains_evidence() {
    for hand in [Hand::Left, Hand::Right] {
        let manifest = manifest();
        let mut worker = fitting_worker(manifest.clone(), keyboard_identity());
        let mut first = synthetic_config(&manifest, hand, 17).idle_window(0).frames()[0];
        first.device_time_us = 100_000;
        for sensor in &mut first.sensors { sensor.sample_time_offset_us = -1_000; }
        worker.handle_sensor(DeviceTransportEvent::Bytes {
            source: hand, receipt_time_ns: 100, bytes: encode_transport_frame(&first).unwrap(),
        }).unwrap();
        let mut bad = first;
        bad.sequence = bad.sequence.wrapping_add(1);
        for sensor in &mut bad.sensors { sensor.sample_time_offset_us = -999; }
        let error = worker.handle_sensor(DeviceTransportEvent::Bytes {
            source: hand, receipt_time_ns: 200, bytes: encode_transport_frame(&bad).unwrap(),
        }).expect_err("duplicate raw aggregate must fail before merge release").to_string();
        assert_eq!(worker.checkpoint().err().expect("terminal worker rejects checkpoints").to_string(), error);
        let incident = worker.close_incident(201, CalibrationIncidentReason::StreamCorruption).unwrap();
        assert!(incident.journal.retention_complete());
        assert!(incident.journal.source_close_complete());
        assert!(!incident.journal.evidence_eligible());
        let session = incident.session.into_admitted().unwrap();
        let mut reader = session.reader();
        assert_eq!(reader.next_record().unwrap().unwrap().payload, encode_frame(&first).unwrap());
        assert_eq!(reader.next_record().unwrap().unwrap().payload, encode_frame(&bad).unwrap());
        assert!(reader.next_record().unwrap().is_none());
    }
}
