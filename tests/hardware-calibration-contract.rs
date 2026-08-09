use std::time::Duration;

use invisible_keyboard::{
    artifact::KeyboardLayoutConfirmationReceipt,
    calibration::KEYBOARD_EXAMPLES_PER_POSITION,
    calibration_app::ResultAcknowledgementReceipt,
    calibration_engine::{EvidenceDomain, EvidencePurpose, EvidenceRole},
    calibration_worker::{
        CalibrationCaptureWorker, CalibrationCaptureWorkerConfig, CalibrationIncidentReason,
        CalibrationWorkerError, CalibrationWorkerFault, KeyboardCaptureContext,
    },
    device_transport::{DeviceTransportConfig, DeviceTransportEvent},
    hardware_calibration::{
        HARDWARE_CALIBRATION_FINAL_STATE_SCHEMA, HardwareCalibrationFinalDisposition,
        HardwareCalibrationFinalState, HardwareCalibrationResultAcknowledgement,
        RESULT_ACKNOWLEDGEMENT_TIMEOUT, ResultAcknowledgementDecision,
        result_acknowledgement_decision,
    },
    labels::{KeyboardSourceIdentity, LEFT_SHIFT_PHYSICAL_CODE, RawKeyState, RawKeyboardEvent},
    layout::{Hand, PhysicalPosition, all_slots, keyboard_warm_start_eligible, position_index},
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CORE28_LAYOUT_ID, HandManifest,
        ManifestHand, PROTOCOL_VERSION, SensorManifest, SensorRole, format_sha256,
    },
    neutral::{NeutralRecordWindow, NeutralWindowRole},
    prompts::PromptCorpus,
    session::{ControllerStream, FOOTER_LEN, SessionError, SessionRecordKind},
    simulation::SyntheticConfig,
    transport::encode_transport_frame,
};

const CREATED_UNIX_NS: u64 = 1_786_233_600_000_000_000;

#[test]
fn saved_result_acknowledgement_uses_the_exact_bounded_deadline() {
    assert_eq!(RESULT_ACKNOWLEDGEMENT_TIMEOUT, Duration::from_secs(5 * 60));
    assert_eq!(
        result_acknowledgement_decision(
            RESULT_ACKNOWLEDGEMENT_TIMEOUT - Duration::from_nanos(1),
            None,
        ),
        ResultAcknowledgementDecision::Waiting
    );
    assert_eq!(
        result_acknowledgement_decision(RESULT_ACKNOWLEDGEMENT_TIMEOUT, None),
        ResultAcknowledgementDecision::TimedOut
    );

    let receipt = ResultAcknowledgementReceipt {
        instruction_revision: 44,
        acknowledged_host_time_ns: 123_456,
    };
    assert_eq!(
        result_acknowledgement_decision(RESULT_ACKNOWLEDGEMENT_TIMEOUT, Some(receipt)),
        ResultAcknowledgementDecision::Acknowledged(receipt),
        "an accepted acknowledgement wins at the deadline"
    );
}

#[test]
fn timed_out_acknowledgement_is_durable_and_does_not_claim_the_result_was_seen() {
    let state = HardwareCalibrationFinalState {
        schema: HARDWARE_CALIBRATION_FINAL_STATE_SCHEMA.to_owned(),
        heldout_validation_passed: true,
        completed_unix_ns: CREATED_UNIX_NS,
        disposition: HardwareCalibrationFinalDisposition::ValidationPassed {
            artifact_sha256: "11".repeat(32),
            validation_report_sha256: "22".repeat(32),
            result_acknowledgement: HardwareCalibrationResultAcknowledgement::TimedOut {
                instruction_revision: 44,
                timeout_seconds: RESULT_ACKNOWLEDGEMENT_TIMEOUT.as_secs(),
            },
        },
    };
    let value = serde_json::to_value(state).expect("final state serializes");
    assert_eq!(
        value["disposition"]["result_acknowledgement"]["status"],
        "timed-out"
    );
    assert_eq!(
        value["disposition"]["result_acknowledgement"]["timeout_seconds"],
        300
    );
    assert!(
        value["disposition"]["result_acknowledgement"]
            .get("acknowledged_host_time_ns")
            .is_none()
    );
}

#[test]
fn missing_causal_window_keeps_raw_attempts_out_of_matched_coverage() {
    let mut fixture = Fixture::new(0x10);
    let position = fixture.position(Hand::Left);
    fixture.record_tap(position, 4_000_000_000, 10);

    let checkpoint = fixture
        .worker
        .keyboard_evidence_checkpoint()
        .expect("a clean prefix with no causal window has exact zero coverage");
    let position_evidence = checkpoint.position(position).expect("position exists");
    assert_eq!(position_evidence.matched_examples, 0);
    assert!(position_evidence.matched_blocks.is_empty());
    assert_eq!(
        checkpoint.deficits(KEYBOARD_EXAMPLES_PER_POSITION)
            [position_index(position).expect("position has an index")],
        16,
    );
}

#[test]
fn repeated_labels_cannot_reuse_overlapping_causal_windows() {
    let mut fixture = Fixture::new(0x30);
    let position = fixture.position(Hand::Left);
    let trigger = fixture.push_tap_window(position);
    for attempt in 0..12 {
        // Keep the raw keyboard journal monotonic while every label still
        // overlaps the same sensor-owned causal window.
        fixture.record_tap(position, trigger + attempt * 2, 10);
    }

    let checkpoint = fixture
        .worker
        .keyboard_evidence_checkpoint()
        .expect("one sensor impulse supplies at most one non-overlapping causal window");
    let position_evidence = checkpoint.position(position).expect("position exists");
    assert_eq!(position_evidence.matched_examples, 1);
    assert_eq!(position_evidence.matched_blocks, vec![10]);
    assert_eq!(
        checkpoint.deficits(KEYBOARD_EXAMPLES_PER_POSITION)
            [position_index(position).expect("position has an index")],
        15,
    );
}

#[test]
fn invalid_sensor_frame_is_a_terminal_stream_integrity_failure() {
    let mut fixture = Fixture::new(0x40);
    let position = fixture.position(Hand::Left);
    let invalid_left = fixture.configs[Hand::Left as usize]
        .with_missing_sensor(0)
        .tap_window(position, fixture.trial);
    let frame = invalid_left.frames()[0];
    let receipt_time_ns = 1_000_000_000 + frame.device_time_us * 1_000;
    let error = fixture
        .worker
        .handle_sensor(DeviceTransportEvent::Bytes {
            source: Hand::Left,
            receipt_time_ns,
            bytes: encode_transport_frame(&frame).unwrap(),
        })
        .expect_err("missing sensors must fault at ingress, before any matching checkpoint");
    assert!(matches!(
        error,
        CalibrationWorkerError::Faulted(CalibrationWorkerFault::FrameQuality {
            source: Hand::Left,
            reason: invisible_keyboard::decoder::AbstainReason::MissingSensor,
            ..
        })
    ));
    assert!(fixture.worker.keyboard_evidence_checkpoint().is_err());
    let incident = fixture
        .worker
        .close_incident(
            receipt_time_ns + 1,
            CalibrationIncidentReason::StreamCorruption,
        )
        .unwrap();
    assert!(incident.journal.retention_complete());
    assert!(incident.journal.source_close_complete());
    let session = incident.session.into_admitted().unwrap();
    let mut reader = session.reader();
    let mut retained = false;
    while let Some(record) = reader.next_record().unwrap() {
        retained |= record.payload == invisible_keyboard::protocol::encode_frame(&frame).unwrap();
    }
    assert!(
        retained,
        "the offending decoded frame remains exact raw evidence"
    );
}

#[test]
fn matched_examples_across_two_blocks_clear_the_exact_position_deficit() {
    let mut fixture = Fixture::new(0x50);
    let position = fixture.position(Hand::Right);
    for example in 0..KEYBOARD_EXAMPLES_PER_POSITION {
        let trigger = fixture.push_tap_window(position);
        let block_id = if example < KEYBOARD_EXAMPLES_PER_POSITION / 2 {
            10
        } else {
            11
        };
        fixture.record_tap(position, trigger, block_id);
    }

    let checkpoint = fixture
        .worker
        .keyboard_evidence_checkpoint()
        .expect("valid synchronized windows produce exact coverage");
    let position_evidence = checkpoint.position(position).expect("position exists");
    assert_eq!(
        position_evidence.matched_examples,
        u32::from(KEYBOARD_EXAMPLES_PER_POSITION),
    );
    assert_eq!(position_evidence.matched_blocks, vec![10, 11]);
    assert_eq!(
        checkpoint.deficits(KEYBOARD_EXAMPLES_PER_POSITION)
            [position_index(position).expect("position has an index")],
        0,
    );
}

#[test]
fn keyboard_checkpoint_can_freeze_the_rule_before_passive_threshold_evidence() {
    let mut fixture = Fixture::new(0x60);
    let position = fixture.position(Hand::Left);
    let trigger = fixture.push_tap_window(position);
    fixture.record_tap(position, trigger, 10);

    let bundle = fixture
        .worker
        .checkpoint()
        .expect("a keyboard-only prefix can freeze its neutral contact rule");
    assert!(bundle.evidence.observations().iter().all(|observation| {
        observation.role() == EvidenceRole::Training
            && observation.domain() == EvidenceDomain::Keyboard
    }));
}

#[test]
fn abort_closes_label_admission_before_incident_finalization() {
    let mut fixture = Fixture::new(0x70);
    let position = fixture.position(Hand::Left);
    fixture.worker.close_label_admission();

    assert!(matches!(
        fixture.worker.handle_keyboard(
            &fixture.keyboard,
            raw_key(
                &fixture.keyboard.device_id,
                30 + u16::from(position.slot),
                Some(position),
                false,
                RawKeyState::Down,
                9_000_000_000,
            ),
            KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10),
        ),
        Err(CalibrationWorkerError::LabelAdmissionClosed)
    ));

    let incident = fixture
        .worker
        .close_incident(9_000_000_001, CalibrationIncidentReason::Aborted)
        .expect("aborted input closes as an incident");
    assert_eq!(
        incident.journal.reason(),
        CalibrationIncidentReason::Aborted
    );
    assert!(!incident.journal.evidence_eligible());
    assert_eq!(incident.journal.retained_events(), 2);
    assert_eq!(
        &incident.session.bytes()[incident.session.bytes().len() - FOOTER_LEN..][..4],
        b"END3"
    );
    let journal: serde_json::Value = serde_json::from_slice(
        &incident
            .journal
            .canonical_json()
            .expect("incident journal serializes"),
    )
    .expect("incident journal is JSON");
    assert!(journal.get("clock_epochs_sha256").is_none());
    assert!(journal.get("evidence").is_none());
    assert!(journal.get("artifact").is_none());
    assert!(journal.get("validation_report").is_none());
}

#[test]
fn source_payload_mismatch_keeps_closed_diagnostic_bytes() {
    let mut fixture = Fixture::new(0x71);
    let wrong_frame = fixture.configs[Hand::Right as usize]
        .idle_window(fixture.trial)
        .frames()[0];
    assert!(
        fixture
            .worker
            .handle_sensor(DeviceTransportEvent::Bytes {
                source: Hand::Left,
                receipt_time_ns: 10_000_000_000,
                bytes: encode_transport_frame(&wrong_frame).expect("wrong-source frame encodes"),
            })
            .is_err()
    );

    let incident = fixture
        .worker
        .close_incident(10_000_000_001, CalibrationIncidentReason::StreamCorruption)
        .expect("faulted worker still closes its retained bytes");
    assert!(!incident.journal.evidence_eligible());
    assert!(incident.journal.terminal_fault().is_some());
    assert_eq!(
        &incident.session.bytes()[incident.session.bytes().len() - FOOTER_LEN..][..4],
        b"END3"
    );
    assert!(matches!(
        incident.session.admit(),
        Err(SessionError::SourcePayloadHandMismatch {
            source: ControllerStream::Left,
            claimed: Hand::Right,
            ..
        })
    ));
}

#[test]
fn abort_incident_closes_buffered_suffix_at_original_receipt() {
    let mut fixture = Fixture::new(0x72);
    let frame = fixture.configs[Hand::Right as usize]
        .idle_window(fixture.trial)
        .frames()[0];
    let encoded = encode_transport_frame(&frame).expect("real right-hand fixture frame encodes");
    let suffix = encoded[..encoded.len() / 2].to_vec();
    let original_receipt = 9_000_000_000;
    fixture
        .worker
        .handle_sensor(DeviceTransportEvent::Bytes {
            source: Hand::Right,
            receipt_time_ns: original_receipt,
            bytes: suffix.clone(),
        })
        .expect("unterminated bytes remain in the original decoder");
    let admitted_records = fixture.worker.record_count();
    fixture.worker.close_label_admission();
    let incident = fixture
        .worker
        .close_incident(original_receipt + 1, CalibrationIncidentReason::Aborted)
        .expect("abort retains the original decoder suffix");
    assert!(incident.journal.source_close_complete());
    assert!(!incident.journal.evidence_eligible());
    assert_eq!(incident.journal.retained_records(), admitted_records + 1);
    assert_eq!(incident.journal.retained_events(), 2);
    let replay = incident
        .session
        .admit()
        .expect("diagnostic records pass structural session audit");
    let mut reader = replay.reader();
    for _ in 0..admitted_records {
        let record = reader
            .next_record()
            .expect("prefix reads")
            .expect("prefix record exists");
        assert_eq!(record.kind, SessionRecordKind::Decoded);
    }
    let record = reader
        .next_record()
        .expect("suffix reads")
        .expect("one suffix exists");
    assert_eq!(record.source, ControllerStream::Right);
    assert_eq!(record.kind, SessionRecordKind::Unterminated);
    assert_eq!(
        record.original_len,
        u64::try_from(suffix.len()).expect("suffix length fits")
    );
    assert_eq!(record.host_time_ns, original_receipt);
    assert_eq!(record.payload, suffix);
    assert!(
        reader
            .next_record()
            .expect("complete footer reads")
            .is_none()
    );
    let journal: serde_json::Value =
        serde_json::from_slice(incident.journal_bytes()).expect("incident journal reads");
    assert_eq!(journal["retention_complete"], true);
}

#[test]
fn bounded_label_retention_reports_an_incomplete_incident() {
    let mut fixture = Fixture::new(0x73);
    let context = KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10)
        .state_only();
    for index in 2..20_000 {
        fixture
            .worker
            .handle_keyboard(
                &fixture.keyboard,
                raw_key(
                    &fixture.keyboard.device_id,
                    LEFT_SHIFT_PHYSICAL_CODE,
                    None,
                    true,
                    RawKeyState::Up,
                    9_000_000_000 + index,
                ),
                context,
            )
            .expect("each real raw-key fact fits the bounded journal");
    }
    assert!(matches!(
        fixture.worker.handle_keyboard(
            &fixture.keyboard,
            raw_key(
                &fixture.keyboard.device_id,
                LEFT_SHIFT_PHYSICAL_CODE,
                None,
                true,
                RawKeyState::Up,
                9_000_020_000,
            ),
            context,
        ),
        Err(CalibrationWorkerError::Faulted(
            CalibrationWorkerFault::RetentionLimitExceeded
        ))
    ));
    let incident = fixture
        .worker
        .close_incident(9_000_020_001, CalibrationIncidentReason::SourceFault)
        .expect("bounded retained input still closes as diagnostic material");
    assert_eq!(incident.journal.retained_events(), 20_000);
    assert!(incident.journal.source_close_complete());
    assert!(!incident.journal.evidence_eligible());
    assert_eq!(
        incident.journal.terminal_fault(),
        Some(
            CalibrationWorkerFault::RetentionLimitExceeded
                .to_string()
                .as_str()
        )
    );
    let journal: serde_json::Value =
        serde_json::from_slice(incident.journal_bytes()).expect("incident journal reads");
    assert_eq!(
        journal["retention_complete"], false,
        "closed parsers cannot claim all evidence was retained after a bound was exceeded"
    );
}

struct Fixture {
    worker: CalibrationCaptureWorker,
    keyboard: KeyboardSourceIdentity,
    configs: [SyntheticConfig; 2],
    trial: u64,
}

impl Fixture {
    fn new(seed: u8) -> Self {
        let manifest = manifest();
        let keyboard = keyboard_identity();
        let configs = [
            synthetic_config(&manifest, Hand::Left, u64::from(seed) << 8),
            synthetic_config(&manifest, Hand::Right, (u64::from(seed) << 8) + 1),
        ];
        let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
            manifest,
            keyboard_source: keyboard.clone(),
            purpose: EvidencePurpose::Fitting,
            matching_rule: None,
            session_id: [seed; 16],
            created_unix_ns: CREATED_UNIX_NS,
            started_host_time_ns: 1,
            surface_plan: None,
            transport: DeviceTransportConfig {
                merge_lateness_ns: 10,
                source_silence_timeout_ns: u64::MAX / 2,
                max_pending_candidates: 16_384,
            },
        })
        .expect("fixture worker starts");
        let mut trial = 0;
        for _ in 0..3 {
            push_pair(
                &mut worker,
                &stable_neutral_window(&configs[0], trial),
                &stable_neutral_window(&configs[1], trial),
            );
            trial += 1;
        }
        let neutral_end_record = worker.record_count();
        worker
            .set_neutral_windows(vec![NeutralRecordWindow {
                block_id: "neutral-1".to_owned(),
                window_id: "stable-home-pose".to_owned(),
                role: NeutralWindowRole::Neutral,
                start_record: 0,
                end_record_exclusive: neutral_end_record,
            }])
            .expect("stable neutral prefix is retained");
        identify_keyboard(&mut worker, &keyboard);
        Self {
            worker,
            keyboard,
            configs,
            trial,
        }
    }

    fn position(&self, hand: Hand) -> PhysicalPosition {
        all_slots()
            .iter()
            .find(|slot| slot.position.hand == hand && keyboard_warm_start_eligible(slot.position))
            .expect("hand has an addressable position")
            .position
    }

    fn push_tap_window(&mut self, position: PhysicalPosition) -> u64 {
        let windows = match position.hand {
            Hand::Left => (
                self.configs[0].tap_window(position, self.trial),
                self.configs[1].idle_window(self.trial),
            ),
            Hand::Right => (
                self.configs[0].idle_window(self.trial),
                self.configs[1].tap_window(position, self.trial),
            ),
        };
        let times = push_pair(&mut self.worker, &windows.0, &windows.1);
        self.trial += 1;
        times[position.hand as usize]
    }

    fn record_tap(&mut self, position: PhysicalPosition, host_time_ns: u64, block_id: u32) {
        let context =
            KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, block_id);
        let physical_code = 30 + u16::from(position.slot);
        for (state, offset) in [(RawKeyState::Down, 0), (RawKeyState::Up, 1)] {
            self.worker
                .handle_keyboard(
                    &self.keyboard,
                    raw_key(
                        &self.keyboard.device_id,
                        physical_code,
                        Some(position),
                        false,
                        state,
                        host_time_ns + offset,
                    ),
                    context,
                )
                .expect("raw keyboard attempt is retained");
        }
    }
}

fn identify_keyboard(worker: &mut CalibrationCaptureWorker, keyboard: &KeyboardSourceIdentity) {
    let context = KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10);
    for (state, host_time_ns) in [(RawKeyState::Down, 1), (RawKeyState::Up, 2)] {
        worker
            .handle_keyboard(
                keyboard,
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
            .expect("selected keyboard is identified");
    }
    worker
        .confirm_keyboard_layout(KeyboardLayoutConfirmationReceipt::us_qwerty(1, 2))
        .expect("the fixture confirms the selected keyboard layout");
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
            let receipt_time_ns = 1_000_000_000 + frame.device_time_us * 1_000;
            last[hand as usize] = receipt_time_ns;
            worker
                .handle_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns,
                    bytes: encode_transport_frame(&frame).expect("sensor frame encodes"),
                })
                .expect("sensor frame is retained");
        }
    }
    worker
        .handle_sensor(DeviceTransportEvent::Tick {
            host_time_ns: last.into_iter().max().expect("pair has timestamps") + 11,
        })
        .expect("merge watermark advances");
    last
}

fn stable_neutral_window(
    config: &SyntheticConfig,
    trial: u64,
) -> invisible_keyboard::decoder::TapWindow {
    let mut frames = config.idle_window(trial).frames().to_vec();
    for (sample_index, frame) in frames.iter_mut().enumerate() {
        let variation = i16::try_from(sample_index % 3).expect("small value fits i16") - 1;
        for (sensor_index, sensor) in frame.sensors.iter_mut().enumerate() {
            sensor.acceleration = [0, 0, 16_000 + variation];
            sensor.angular_velocity = [0, 0, 0];
            sensor.sample_time_offset_us =
                i16::try_from(sensor_index).expect("sensor index fits i16") * 20;
        }
    }
    invisible_keyboard::decoder::TapWindow::new(frames).expect("neutral window is valid")
}

fn raw_key(
    device_id: &str,
    physical_code: u16,
    position: Option<PhysicalPosition>,
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
        device_id: "/dev/input/event9:hardware-contract".to_owned(),
        name: "Hardware Contract Keyboard".to_owned(),
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

fn manifest() -> AcquisitionManifest {
    AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![
            hand_manifest(ManifestHand::Left, 0x11),
            hand_manifest(ManifestHand::Right, 0x22),
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
        prompt_corpus_sha256: format_sha256(&PromptCorpus::v1().sha256()),
        study_id: "hardware-calibration-contract".to_owned(),
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
        controller_id: format!("controller-{side}"),
        boot_id: format!("{boot:02x}").repeat(16),
        sensors: std::array::from_fn(|index| SensorManifest {
            sensor_index: u8::try_from(index).expect("sensor index fits u8"),
            mux_channel: u8::try_from(index).expect("mux channel fits u8"),
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
