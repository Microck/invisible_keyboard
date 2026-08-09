use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use invisible_keyboard::{
    artifact::{
        ArtifactDomain, ArtifactSensorBinding, ArtifactSource, ArtifactSourceRole,
        CalibrationArtifact, CalibrationArtifactInput, HandClockFit, KeyboardIdentity,
        KeyboardLayoutConfirmationReceipt,
    },
    calibration_capture::{
        CALIBRATION_MATCHING_RULE_SCHEMA, CONTACT_DEBOUNCE_WINDOWS, CONTACT_FEATURE_SCHEMA,
        CONTACT_REARM_WINDOWS, CONTACT_RESPONSE_END_NS, CONTACT_RESPONSE_START_NS,
        CalibrationCaptureInput, CalibrationCaptureJournal, CalibrationMatchingRule,
        CalibrationSurfacePlanBinding, CapturedCalibrationEvent, HandContactThresholds,
        MAX_CALIBRATION_CAPTURE_BYTES, SurfacePresentationReceipt,
    },
    calibration_engine::{EvidencePurpose, validate_calibration},
    clock::{ClockFit, ClockSample, MAX_ONLINE_CLOCK_QUALITY_WARMUP_US},
    clock_epoch::{ClockEpochSet, LiveClockReport},
    decoder::HandDecoder,
    gate0_trace::GATE0_CAPTURE_SIDECAR_SCHEMA,
    layout::{Hand, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots},
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, ACQUISITION_TEMPLATE_SCHEMA, AcquisitionManifest,
        AcquisitionManifestTemplate, AxisCharacterizationReport, CANONICAL_AXIS_CONVENTION,
        CORE28_LAYOUT_ID, HandManifest, MAX_MANIFEST_BYTES, ManifestHand, PROTOCOL_VERSION,
        RigEvidenceRecord, SensorManifest, SensorRole, format_sha256, sha256,
    },
    physical_study::{
        CandidateObservation, FrozenStudyPipeline, IndependentTruthSource,
        ObservationRepresentation, ObservationSource, PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
        PHYSICAL_STUDY_REPORT_SCHEMA, PhysicalStudyAnalysisPlan, PhysicalStudyEvidenceRow,
        PhysicalStudyGate, PhysicalStudySplitManifest, PromptTruthClock, StudyCondition,
        StudySplit, encode_physical_study_jsonl, seal_physical_study_evidence_row,
    },
    prompts::{HeldOutSurfacePlan, PromptCorpus, SurfaceSegmentKind},
    protocol::{
        ALL_SENSORS_VALID, QuiesceReceipt, QuiesceRequest, SENSOR_COUNT, SensorFrame, SensorReading,
    },
    session::{
        ControllerStream, FinalizedSession, HEADER_LEN, HandStreamSummary,
        MAX_GATE0A_SESSION_BYTES, MAX_SETUP_SESSION_BYTES, SessionHeader, SessionWriter,
        audit_session_reader,
    },
    simulation::SyntheticConfig,
    transport::{MAX_ENCODED_FRAME_LEN, encode_transport_frame},
    validation::ValidationReport,
};

fn run(arguments: &[&str]) -> std::process::Output {
    run_binary("INVISIBLE_KEYBOARD_RLOCATION", arguments)
}

fn run_binary(location_variable: &str, arguments: &[&str]) -> std::process::Output {
    command_binary(location_variable, arguments)
        .output()
        .expect("reference CLI starts")
}

fn command_binary(location_variable: &str, arguments: &[&str]) -> Command {
    let runfiles = std::env::var_os("TEST_SRCDIR").expect("Bazel supplies its runfiles root");
    let relative =
        std::env::var(location_variable).expect("Bazel supplies the binary runfiles path");
    let binary = PathBuf::from(runfiles).join(relative);
    let mut command = Command::new(binary);
    command.args(arguments);
    command
}

struct ManifestEvidenceFiles {
    rig: PathBuf,
    axis: PathBuf,
}

fn manifest_evidence_files(manifest_or_template: &Path) -> ManifestEvidenceFiles {
    ManifestEvidenceFiles {
        rig: manifest_or_template.with_extension("rig-evidence.json"),
        axis: manifest_or_template.with_extension("axis-characterization.json"),
    }
}

fn path_text(path: &Path) -> &str {
    path.to_str().expect("fixture path is UTF-8")
}

fn run_evidence_bound(
    command: &str,
    manifest_or_template: &Path,
    evidence_owner: &Path,
    trailing_arguments: &[&str],
) -> std::process::Output {
    let evidence = manifest_evidence_files(evidence_owner);
    let mut arguments = vec![
        command,
        path_text(manifest_or_template),
        path_text(&evidence.rig),
        path_text(&evidence.axis),
    ];
    arguments.extend_from_slice(trailing_arguments);
    run(&arguments)
}

fn run_record_stream(
    manifest: &str,
    session: &str,
    left_input: &[u8],
    right_input: &[u8],
) -> std::process::Output {
    static TEMPLATE_ID: AtomicU64 = AtomicU64::new(0);
    let template_id = TEMPLATE_ID.fetch_add(1, Ordering::Relaxed);
    let session_path = PathBuf::from(session);
    let directory = session_path.parent().expect("session has a parent");
    let template = directory.join(format!("acquisition-template-{template_id}.json"));
    let locked_manifest = directory.join(format!("locked-manifest-{template_id}.json"));
    write_template_from_manifest(manifest, &template);
    run_record_stream_paths(
        template.to_str().expect("template path is UTF-8"),
        locked_manifest
            .to_str()
            .expect("locked manifest path is UTF-8"),
        session,
        left_input,
        right_input,
    )
}

fn run_record_stream_paths(
    template: &str,
    locked_manifest: &str,
    session: &str,
    left_input: &[u8],
    right_input: &[u8],
) -> std::process::Output {
    static SOURCE_ID: AtomicU64 = AtomicU64::new(0);
    let source_id = SOURCE_ID.fetch_add(1, Ordering::Relaxed);
    let session_path = PathBuf::from(session);
    let directory = session_path.parent().expect("session has a parent");
    let left_path = directory.join(format!("left-source-{source_id}.cobs"));
    let right_path = directory.join(format!("right-source-{source_id}.cobs"));
    fs::write(&left_path, left_input).expect("left source writes");
    fs::write(&right_path, right_input).expect("right source writes");
    let evidence = manifest_evidence_files(Path::new(template));
    run(&[
        "record-stream",
        template,
        path_text(&evidence.rig),
        path_text(&evidence.axis),
        left_path.to_str().expect("left source path is UTF-8"),
        right_path.to_str().expect("right source path is UTF-8"),
        locked_manifest,
        session,
    ])
}

fn run_record_hand_paths(
    template: &str,
    locked_manifest: &str,
    session: &str,
    device_input: &[u8],
) -> std::process::Output {
    static SOURCE_ID: AtomicU64 = AtomicU64::new(0);
    let source_id = SOURCE_ID.fetch_add(1, Ordering::Relaxed);
    let session_path = PathBuf::from(session);
    let directory = session_path.parent().expect("session has a parent");
    let device_path = directory.join(format!("hand-source-{source_id}.cobs"));
    fs::write(&device_path, device_input).expect("hand source writes");
    let evidence = manifest_evidence_files(Path::new(template));
    run(&[
        "record-hand",
        template,
        path_text(&evidence.rig),
        path_text(&evidence.axis),
        device_path.to_str().expect("device path is UTF-8"),
        locked_manifest,
        session,
        "--ready-fd",
        "3",
        "--stop-fd",
        "4",
    ])
}

fn write_template_from_manifest(manifest: &str, template: &std::path::Path) {
    let manifest_evidence = manifest_evidence_files(Path::new(manifest));
    let rig_source = fs::read(&manifest_evidence.rig).expect("rig evidence reads");
    let axis_source = fs::read(&manifest_evidence.axis).expect("axis characterization reads");
    let mut pending = AcquisitionManifest::from_json_with_evidence(
        &fs::read(manifest).expect("manifest reads"),
        &rig_source,
        &axis_source,
    )
    .expect("evidence-bound manifest parses");
    pending.schema = ACQUISITION_TEMPLATE_SCHEMA.to_owned();
    for hand in &mut pending.hands {
        hand.boot_id = "00".repeat(16);
    }
    let template_source = serde_json::to_vec(&pending).expect("template serializes");
    AcquisitionManifestTemplate::from_json_with_evidence(
        &template_source,
        &rig_source,
        &axis_source,
    )
    .expect("evidence-bound template parses");
    fs::write(template, template_source).expect("template writes");
    let template_evidence = manifest_evidence_files(template);
    fs::write(template_evidence.rig, rig_source).expect("template rig evidence writes");
    fs::write(template_evidence.axis, axis_source).expect("template axis characterization writes");
}

fn rig_evidence_source(manifest: &AcquisitionManifest) -> Vec<u8> {
    let artifact_sha256 = format_sha256(&[0xb1; 32]);
    let passed_fact = || {
        serde_json::json!({
            "status": "passed",
            "artifact_sha256": artifact_sha256.clone(),
        })
    };
    let hands = manifest
        .hands
        .iter()
        .map(|hand| {
            let side = match hand.hand {
                ManifestHand::Left => "left",
                ManifestHand::Right => "right",
            };
            let sensors = hand
                .sensors
                .iter()
                .map(|sensor| {
                    serde_json::json!({
                        "sensor_index": sensor.sensor_index,
                        "mux_channel": sensor.mux_channel,
                        "role": sensor.role,
                        "mount_id": sensor.mount_id.as_str(),
                        "wiring_id": sensor.wiring_id.as_str(),
                        "interrupt_pin": sensor.interrupt_pin.as_str(),
                        "mass_milligrams": 1_000,
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "hand": hand.hand,
                "assembly_id": format!("{side}-assembly-001"),
                "controller_id": hand.controller_id.as_str(),
                "total_mass_milligrams": 12_000,
                "mass_measurement": passed_fact(),
                "sensors": sensors,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&serde_json::json!({
        "schema": "invisible-keyboard-rig-evidence-v1",
        "record_id": "cli-contract-rig-001",
        "recorded_at_utc": "2026-08-30T00:00:00Z",
        "operator_id": "cli-contract-operator-001",
        "controller_board": manifest.controller_board.as_str(),
        "sensor_model": manifest.sensor_model.as_str(),
        "hands": hands,
        "continuity": {
            "fact": passed_fact(),
            "checked_path_count": 36,
        },
        "photos": {
            "fact": passed_fact(),
            "photo_count": 8,
        },
        "power": {
            "fact": passed_fact(),
            "supply_millivolts": 3_300,
            "idle_current_microamps": 18_000,
            "peak_current_microamps": 48_000,
        },
        "analyzer": {
            "fact": passed_fact(),
            "sample_rate_hz": 10_000_000,
            "capture_duration_milliseconds": 5_000,
        },
        "surface": {
            "surface_id": "laminate-surface-001",
            "material": "high-pressure-laminate",
            "thickness_micrometers": 12_000,
            "fact": passed_fact(),
        },
        "posture": {
            "posture_id": "seated-neutral-001",
            "description": "seated with supported forearms and neutral wrists",
            "fact": passed_fact(),
        },
        "truth_device": {
            "device_id": "truth-device-001",
            "model": "independent-contact-array-001",
            "capture_method": "hardware-timestamped-contact-edges",
            "fact": passed_fact(),
        },
        "alignment": {
            "fixture_id": "six-face-fixture-001",
            "maximum_error_millidegrees": 1_500,
            "fact": passed_fact(),
        },
    }))
    .expect("rig evidence fixture serializes")
}

fn axis_characterization_source(
    manifest: &AcquisitionManifest,
    rig_evidence_sha256: &str,
) -> Vec<u8> {
    let face_means = serde_json::json!([
        {"face": "positive-x", "sample_count": 600, "mean_acceleration_lsb": [8192, 0, 0]},
        {"face": "negative-x", "sample_count": 600, "mean_acceleration_lsb": [-8192, 0, 0]},
        {"face": "positive-y", "sample_count": 600, "mean_acceleration_lsb": [0, 8192, 0]},
        {"face": "negative-y", "sample_count": 600, "mean_acceleration_lsb": [0, -8192, 0]},
        {"face": "positive-z", "sample_count": 600, "mean_acceleration_lsb": [0, 0, 8192]},
        {"face": "negative-z", "sample_count": 600, "mean_acceleration_lsb": [0, 0, -8192]},
    ]);
    let hands = manifest
        .hands
        .iter()
        .map(|hand| {
            let sensors = hand
                .sensors
                .iter()
                .map(|sensor| {
                    serde_json::json!({
                        "sensor_index": sensor.sensor_index,
                        "role": sensor.role,
                        "raw_face_means": face_means.clone(),
                        "axis_map": sensor.axis_map,
                        "observed_maximum_component_residual_lsb": 0,
                        "passed": true,
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "hand": hand.hand,
                "positive_y_direction": match hand.hand {
                    ManifestHand::Left => "ulnar",
                    ManifestHand::Right => "radial",
                },
                "sensors": sensors,
            })
        })
        .collect::<Vec<_>>();
    let candidate_manifest_sha256 = format_sha256(
        &manifest
            .axis_characterization_candidate_sha256()
            .expect("axis candidate hashes"),
    );
    serde_json::to_vec(&serde_json::json!({
        "schema": "invisible-keyboard-axis-characterization-v1",
        "report_id": "cli-contract-axis-001",
        "completed_at_utc": "2026-08-30T00:00:00Z",
        "characterizer_build": "cli-contract-characterizer-001",
        "method_id": "fixed-order-600-stationary-iks3-v1",
        "coordinate_convention": CANONICAL_AXIS_CONVENTION,
        "candidate_manifest_sha256": candidate_manifest_sha256,
        "source_session_sha256": format_sha256(&[0xb2; 32]),
        "rig_evidence_sha256": rig_evidence_sha256,
        "criteria": {
            "expected_gravity_lsb": 8_192,
            "maximum_component_residual_lsb": 820,
            "minimum_samples_per_face": 600,
        },
        "hands": hands,
        "passed": true,
    }))
    .expect("axis characterization fixture serializes")
}

fn write_bound_manifest(path: &Path, mut manifest: AcquisitionManifest) -> AcquisitionManifest {
    let rig_source = rig_evidence_source(&manifest);
    let rig = RigEvidenceRecord::from_json(&rig_source).expect("rig evidence fixture validates");
    manifest.rig_evidence_sha256 =
        format_sha256(&rig.sha256().expect("rig evidence fixture hashes"));
    let axis_source = axis_characterization_source(&manifest, &manifest.rig_evidence_sha256);
    let axis = AxisCharacterizationReport::from_json(&axis_source)
        .expect("axis characterization fixture validates");
    manifest.axis_characterization_sha256 =
        format_sha256(&axis.sha256().expect("axis characterization fixture hashes"));
    let manifest_source = manifest
        .canonical_json()
        .expect("bound manifest fixture serializes");
    AcquisitionManifest::from_json_with_evidence(&manifest_source, &rig_source, &axis_source)
        .expect("manifest fixture binds exact physical evidence");

    let evidence = manifest_evidence_files(path);
    fs::write(path, manifest_source).expect("manifest fixture writes");
    fs::write(evidence.rig, rig_source).expect("rig evidence fixture writes");
    fs::write(evidence.axis, axis_source).expect("axis characterization fixture writes");
    manifest
}

fn read_bound_manifest(path: &Path) -> AcquisitionManifest {
    let evidence = manifest_evidence_files(path);
    AcquisitionManifest::from_json_with_evidence(
        &fs::read(path).expect("manifest reads"),
        &fs::read(evidence.rig).expect("rig evidence reads"),
        &fs::read(evidence.axis).expect("axis characterization reads"),
    )
    .expect("manifest binds fixture evidence")
}

fn rewrite_manifest_with_existing_evidence(path: &Path, manifest: &AcquisitionManifest) {
    let evidence = manifest_evidence_files(path);
    let rig_source = fs::read(evidence.rig).expect("rig evidence reads");
    let axis_source = fs::read(evidence.axis).expect("axis characterization reads");
    let manifest_source = manifest.canonical_json().expect("manifest serializes");
    AcquisitionManifest::from_json_with_evidence(&manifest_source, &rig_source, &axis_source)
        .expect("rewritten manifest keeps exact evidence bindings");
    fs::write(path, manifest_source).expect("manifest writes");
}

fn write_manifest(test_directory: &std::ffi::OsStr, label: &str, sensor_odr_hz: u16) -> PathBuf {
    let path =
        PathBuf::from(test_directory).join(format!("{label}-manifest-{}.json", std::process::id()));
    let manifest = AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![
            cli_hand_manifest(ManifestHand::Left, 0x11),
            cli_hand_manifest(ManifestHand::Right, 0x22),
        ],
        controller_board: "Seeed XIAO nRF52840 Plus SKU 102010672".to_owned(),
        firmware_build: "fixture-firmware-digest-a1b2c3d4".to_owned(),
        host_build: "fixture-host-digest-e5f60718".to_owned(),
        sensor_model: "LSM6DS3TR-C".to_owned(),
        sensors_per_hand: 6,
        sensor_odr_hz,
        accelerometer_range_g: 4,
        gyroscope_range_dps: 1_000,
        filter_profile: "gate0-raw-v1".to_owned(),
        i2c_bus_hz: 400_000,
        nominal_period_us: 5_000,
        prompt_corpus_sha256: format_sha256(&[0xaa; 32]),
        study_id: "gate0-stream-integrity".to_owned(),
        split_id: "engineering-only".to_owned(),
        participant_alias: "operator-001".to_owned(),
        consent_record: "self-test-2026-08-09".to_owned(),
        retention_policy: "local-delete-after-analysis-v1".to_owned(),
        rig_evidence_sha256: format_sha256(&[0xa1; 32]),
        axis_characterization_sha256: format_sha256(&[0xa2; 32]),
        calibration_artifact_sha256: None,
        calibration_validation_sha256: None,
    };
    write_bound_manifest(&path, manifest);
    path
}

fn write_one_hand_manifest(test_directory: &std::ffi::OsStr, label: &str, hand: Hand) -> PathBuf {
    let path = write_manifest(test_directory, label, 208);
    let mut manifest = read_bound_manifest(&path);
    let declared = match hand {
        Hand::Left => ManifestHand::Left,
        Hand::Right => ManifestHand::Right,
    };
    manifest
        .hands
        .retain(|candidate| candidate.hand == declared);
    write_bound_manifest(&path, manifest);
    path
}

fn write_one_hand_session(manifest_path: &std::path::Path, session_path: &std::path::Path) {
    let manifest = read_bound_manifest(manifest_path);
    let identity = manifest.hands[0]
        .frame_identity()
        .expect("one-hand identity exists");
    let hand = identity.hand;
    let session_id = [0x71; 16];
    let request_start_time_ns = 100_000_000;
    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us: manifest.nominal_period_us,
            manifest_sha256: manifest.sha256().expect("manifest hashes"),
            session_id,
        },
    )
    .expect("session starts");
    writer
        .append_frame(ControllerStream::from(hand), &sensor_frame(hand, 0), 0)
        .expect("initial one-hand frame writes");
    let request = QuiesceRequest {
        hand,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        session_id,
        request_start_time_ns,
    };
    writer
        .append_quiesce_request(
            ControllerStream::from(hand),
            &request,
            request_start_time_ns,
        )
        .expect("Gate 0A request writes");
    let final_sequence = 1;
    writer
        .append_frame(
            ControllerStream::from(hand),
            &sensor_frame(hand, final_sequence),
            request_start_time_ns + 5_000_000,
        )
        .expect("Gate 0A final frame writes");
    writer
        .append_quiesce_receipt(
            ControllerStream::from(hand),
            &QuiesceReceipt {
                hand,
                controller_id_sha256: request.controller_id_sha256,
                boot_id: request.boot_id,
                session_id,
                request_start_time_ns,
                final_sequence,
            },
            request_start_time_ns + 10_000_000,
        )
        .expect("Gate 0A receipt writes");
    let (bytes, _) = writer.finish().expect("session finishes");
    fs::write(session_path, bytes).expect("session fixture writes");
}

fn cli_hand_manifest(hand: ManifestHand, boot: u8) -> HandManifest {
    let side = match hand {
        ManifestHand::Left => "left",
        ManifestHand::Right => "right",
    };
    HandManifest {
        hand,
        controller_id: format!("xiao-{side}-001"),
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
            mount_id: format!("strap-{side}-{sensor_index}"),
            wiring_id: format!("wiring-{side}-{sensor_index}"),
            interrupt_pin: format!("gpio-{side}-{sensor_index}"),
            axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
        }),
    }
}

fn sensor_frame(hand: Hand, sequence: u32) -> SensorFrame {
    let side = match hand {
        Hand::Left => "left",
        Hand::Right => "right",
    };
    SensorFrame {
        hand,
        controller_id_sha256: sha256(format!("xiao-{side}-001").as_bytes()),
        boot_id: match hand {
            Hand::Left => [0x11; 16],
            Hand::Right => [0x22; 16],
        },
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence,
        device_time_us: u64::from(sequence) * 5_000,
        sensors: [SensorReading::default(); SENSOR_COUNT],
    }
}

#[derive(Clone, Copy)]
struct PhysicalStudyCliIdentity<'a> {
    participant: &'a str,
    mount: &'a str,
    session: &'a str,
}

impl<'a> PhysicalStudyCliIdentity<'a> {
    fn new(participant: &'a str, mount: &'a str, session: &'a str) -> Self {
        Self {
            participant,
            mount,
            session,
        }
    }
}

fn physical_study_cli_row(
    gate: PhysicalStudyGate,
    split: StudySplit,
    identity: PhysicalStudyCliIdentity<'_>,
    hand: Hand,
    condition: StudyCondition,
    label: Option<&str>,
    prompt_ordinal: u64,
) -> PhysicalStudyEvidenceRow {
    let PhysicalStudyCliIdentity {
        participant,
        mount,
        session,
    } = identity;
    let prompt_id = format!("{session}-prompt-{prompt_ordinal:06}");
    let base = prompt_ordinal * 61_000_000;
    let intentional = condition == StudyCondition::IntentionalTap;
    let truth_event_time_us = intentional.then_some(base + 200_000);
    let candidate_events = truth_event_time_us.map_or_else(Vec::new, |truth_time| {
        vec![CandidateObservation {
            event_id: format!("{prompt_id}-event"),
            event_time_us: truth_time + 10_000,
            features: Vec::new(),
        }]
    });
    PhysicalStudyEvidenceRow {
        schema: invisible_keyboard::physical_study::PHYSICAL_STUDY_EVIDENCE_SCHEMA.to_owned(),
        row_sha256: String::new(),
        gate,
        round_id: "cli-physical-study-round-1".to_owned(),
        split,
        split_manifest_sha256: String::new(),
        analysis_plan_sha256: String::new(),
        contact_detector_sha256: format_sha256(&sha256(b"cli-contact-detector")),
        contact_detector_fit_splits: vec![StudySplit::Train, StudySplit::Validation],
        participant_id: participant.to_owned(),
        session_id: session.to_owned(),
        rig_id: format!("rig-{participant}"),
        controller_id: format!("controller-{participant}-{hand:?}"),
        mount_id: mount.to_owned(),
        surface_id: "rigid-desk-cli".to_owned(),
        hand,
        condition,
        prompted_label: label.map(str::to_owned),
        truth_label: label.map(str::to_owned),
        truth_source: IndependentTruthSource::ContactSwitch,
        clock: PromptTruthClock {
            prompt_id,
            prompt_ordinal,
            timeline_id: format!("timeline-{session}"),
            clock_mapping_sha256: format_sha256(&sha256(format!("clock-{session}").as_bytes())),
            prompt_time_us: base,
            truth_start_us: base + 100_000,
            truth_event_time_us,
            truth_end_us: if intentional {
                base + 500_000
            } else {
                base + 60_100_000
            },
        },
        observation_source: ObservationSource {
            representation: ObservationRepresentation::Raw,
            artifact_sha256: format_sha256(&sha256(
                format!("artifact-{session}-{prompt_ordinal}").as_bytes(),
            )),
            transform_sha256: None,
            transform_fit_splits: Vec::new(),
        },
        candidate_events,
    }
}

fn gate0b_cli_study_rows() -> Vec<PhysicalStudyEvidenceRow> {
    let mut rows = Vec::new();
    for participant_index in 0..5 {
        let participant = format!("cli-participant-{participant_index}");
        for mount_index in 0..2 {
            let mount = format!("{participant}-mount-{mount_index}");
            for hand in [Hand::Left, Hand::Right] {
                let session = format!("{participant}-session-{mount_index}-{hand:?}");
                let identity = PhysicalStudyCliIdentity::new(&participant, &mount, &session);
                let mut ordinal = 0_u64;
                for finger in ["thumb", "index", "middle", "ring", "pinky"] {
                    for _ in 0..50 {
                        rows.push(physical_study_cli_row(
                            PhysicalStudyGate::Gate0B,
                            StudySplit::Heldout,
                            identity,
                            hand,
                            StudyCondition::IntentionalTap,
                            Some(finger),
                            ordinal,
                        ));
                        ordinal += 1;
                    }
                }
                for condition in [StudyCondition::Quiet, StudyCondition::ScriptedMotion] {
                    rows.push(physical_study_cli_row(
                        PhysicalStudyGate::Gate0B,
                        StudySplit::Heldout,
                        identity,
                        hand,
                        condition,
                        None,
                        ordinal,
                    ));
                    ordinal += 1;
                }
            }
        }
    }
    rows
}

fn freeze_physical_study_rows(rows: &mut [PhysicalStudyEvidenceRow]) -> (Vec<u8>, Vec<u8>) {
    let manifest =
        PhysicalStudySplitManifest::from_rows(rows).expect("physical-study roster freezes");
    let manifest_bytes = manifest
        .canonical_json_bytes()
        .expect("physical-study manifest serializes");
    let manifest_sha256 = format_sha256(&sha256(&manifest_bytes));
    let plan = PhysicalStudyAnalysisPlan::v1(
        manifest.gate,
        manifest.round_id.clone(),
        manifest_sha256.clone(),
        FrozenStudyPipeline::from(&rows[0]),
    );
    let plan_bytes = plan
        .canonical_json_bytes()
        .expect("physical-study plan serializes");
    let plan_sha256 = format_sha256(&sha256(&plan_bytes));
    for row in rows {
        row.split_manifest_sha256.clone_from(&manifest_sha256);
        row.analysis_plan_sha256.clone_from(&plan_sha256);
        seal_physical_study_evidence_row(row).expect("physical-study row seals");
    }
    (manifest_bytes, plan_bytes)
}

fn transport_frame(hand: Hand, sequence: u32) -> Vec<u8> {
    encode_transport_frame(&sensor_frame(hand, sequence)).expect("valid fixture frame encodes")
}

fn encoded_transport_frame(frame: &SensorFrame) -> Vec<u8> {
    encode_transport_frame(frame).expect("valid fixture frame encodes")
}

fn artifact_sensor_bindings(manifest: &AcquisitionManifest) -> Vec<ArtifactSensorBinding> {
    manifest
        .hands
        .iter()
        .flat_map(|hand| {
            let artifact_hand = match hand.hand {
                ManifestHand::Left => Hand::Left,
                ManifestHand::Right => Hand::Right,
            };
            hand.sensors
                .iter()
                .cloned()
                .map(move |sensor| ArtifactSensorBinding {
                    hand: artifact_hand,
                    sensor,
                })
        })
        .collect()
}

fn calibration_clock_fit(boot: u8) -> ClockFit {
    let samples = (0..24_u64)
        .map(|index| ClockSample {
            device_time_us: index * 5_000,
            host_time_ns: 1_000_000_000 + index * 5_000_000,
        })
        .collect::<Vec<_>>();
    ClockFit::fit([boot; 16], &samples).expect("calibration clock fixture fits")
}

fn calibration_matching_rule(manifest: &AcquisitionManifest) -> CalibrationMatchingRule {
    let rule = CalibrationMatchingRule {
        schema: CALIBRATION_MATCHING_RULE_SCHEMA.to_owned(),
        acquisition_manifest_sha256: format_sha256(
            &manifest.sha256().expect("matching-rule manifest hashes"),
        ),
        neutral_windows_sha256: format_sha256(&[0x41; 32]),
        neutral_source_sha256: format_sha256(&[0x42; 32]),
        feature_schema: CONTACT_FEATURE_SCHEMA.to_owned(),
        thresholds: [
            HandContactThresholds {
                hand: Hand::Left,
                low_energy: 12_500_000,
                high_energy: 50_000_000,
                motion_energy: 4_096,
            },
            HandContactThresholds {
                hand: Hand::Right,
                low_energy: 12_500_000,
                high_energy: 50_000_000,
                motion_energy: 4_096,
            },
        ],
        debounce_windows: CONTACT_DEBOUNCE_WINDOWS,
        rearm_windows: CONTACT_REARM_WINDOWS,
        response_start_ns: CONTACT_RESPONSE_START_NS,
        response_end_ns: CONTACT_RESPONSE_END_NS,
    };
    rule.validate().expect("matching-rule fixture is valid");
    rule
}

fn write_calibration_artifact(
    manifest: &AcquisitionManifest,
    path: &std::path::Path,
) -> CalibrationArtifact {
    static DECODERS: OnceLock<(HandDecoder, HandDecoder)> = OnceLock::new();
    let (left, right) = DECODERS.get_or_init(|| {
        let left = HandDecoder::calibrate(
            Hand::Left,
            &SyntheticConfig::new(Hand::Left, 0x434c_494c).calibration_examples(5, 20),
        )
        .expect("left calibration fixture fits");
        let right = HandDecoder::calibrate(
            Hand::Right,
            &SyntheticConfig::new(Hand::Right, 0x434c_4952).calibration_examples(5, 20),
        )
        .expect("right calibration fixture fits");
        (left, right)
    });
    let artifact = CalibrationArtifact::new(CalibrationArtifactInput {
        acquisition_manifest_sha256: manifest.sha256().expect("calibration manifest hashes"),
        fitting_capture_journal_sha256: [0x31; 32],
        fitting_evidence_sha256: [0x32; 32],
        prompt_corpus_sha256: PromptCorpus::v1().sha256(),
        neutral_evidence_sha256: [0x33; 32],
        calibration_matching_rule_sha256: calibration_matching_rule(manifest)
            .fingerprint()
            .expect("matching-rule fixture hashes"),
        sensor_bindings: artifact_sensor_bindings(manifest),
        keyboard: KeyboardIdentity {
            name: "CLI contract keyboard".to_owned(),
            physical_path: "fixture/input0".to_owned(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            logical_layout: "us-qwerty-v1".to_owned(),
            layout_confirmation: KeyboardLayoutConfirmationReceipt::us_qwerty(1, 1),
        },
        sources: vec![
            ArtifactSource::new(
                [0x10; 32],
                ArtifactSourceRole::Training,
                ArtifactDomain::Keyboard,
                vec![10],
            ),
            ArtifactSource::new(
                [0x11; 32],
                ArtifactSourceRole::Training,
                ArtifactDomain::PassiveSurface,
                vec![20],
            ),
            ArtifactSource::new(
                [0x12; 32],
                ArtifactSourceRole::ThresholdSelection,
                ArtifactDomain::PassiveSurface,
                vec![30],
            ),
        ],
        clock_fits: vec![
            HandClockFit {
                hand: Hand::Left,
                fit: calibration_clock_fit(0x11),
            },
            HandClockFit {
                hand: Hand::Right,
                fit: calibration_clock_fit(0x22),
            },
        ],
        left_decoder: Some(left.clone()),
        right_decoder: Some(right.clone()),
        accepted_position_counts: vec![4; PHYSICAL_POSITION_COUNT],
        rejected_examples: 0,
        setup_seconds: 60,
        created_unix_ns: 1_786_233_600_000_000_000,
        requires_wear_session_confirmation: true,
    })
    .expect("calibration artifact fixture is valid");
    fs::write(
        path,
        artifact.canonical_json().expect("artifact serializes"),
    )
    .expect("artifact fixture writes");
    artifact
}

const HELDOUT_SESSION_ID: [u8; 16] = [0x92; 16];
const HELDOUT_RUN_SEED: u64 = 73;
const HELDOUT_PLAN_START_NS: u64 = 10_000_000_000;

struct RuntimeCalibrationBundleBytes {
    manifest_sha256: [u8; 32],
    artifact_sha256: [u8; 32],
    validation_passed: bool,
    clocks: Vec<u8>,
    capture: Vec<u8>,
    surface_plan: Vec<u8>,
    evidence: Vec<u8>,
    session: Vec<u8>,
    report: Vec<u8>,
}

/// Build one real, replayable held-out bundle and share its immutable bytes
/// across the live CLI tests. Every fixture manifest and artifact has the same
/// canonical content, so this avoids rebuilding 60 seconds of evidence for
/// each test while still making the runtime reevaluate it in every process.
fn write_runtime_calibration_output(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    directory: &std::path::Path,
) {
    static BUNDLE: OnceLock<RuntimeCalibrationBundleBytes> = OnceLock::new();
    let bundle = BUNDLE.get_or_init(|| build_runtime_calibration_bundle(manifest, artifact));
    assert_eq!(
        bundle.manifest_sha256,
        manifest.sha256().expect("runtime fixture manifest hashes")
    );
    assert_eq!(
        bundle.artifact_sha256,
        artifact
            .fingerprint()
            .expect("runtime fixture artifact hashes")
    );
    assert!(bundle.validation_passed, "cached runtime fixture must pass");
    write_runtime_calibration_bundle(directory, bundle);
}

fn write_runtime_calibration_bundle(
    directory: &std::path::Path,
    bundle: &RuntimeCalibrationBundleBytes,
) {
    fs::create_dir(directory).expect("calibration output directory is exclusive");
    for (name, bytes) in [
        ("heldout-clocks.json", bundle.clocks.as_slice()),
        ("heldout-capture.json", bundle.capture.as_slice()),
        ("heldout-surface-plan.json", bundle.surface_plan.as_slice()),
        ("heldout-evidence.json", bundle.evidence.as_slice()),
        ("heldout-session.iks", bundle.session.as_slice()),
        ("validation-report.json", bundle.report.as_slice()),
    ] {
        fs::write(directory.join(name), bytes).expect("calibration output file writes");
    }
}

fn build_runtime_calibration_bundle(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
) -> RuntimeCalibrationBundleBytes {
    build_runtime_calibration_bundle_for_run(
        manifest,
        artifact,
        HELDOUT_SESSION_ID,
        HELDOUT_RUN_SEED,
        false,
    )
}

fn build_runtime_calibration_bundle_for_run(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    session_id: [u8; 16],
    run_seed: u64,
    shift_target_rows: bool,
) -> RuntimeCalibrationBundleBytes {
    let artifact_sha256 = artifact
        .fingerprint()
        .expect("runtime fixture artifact hashes");
    let plan = HeldOutSurfacePlan::v1(artifact_sha256, run_seed);
    let mut builder = HeldoutSessionBuilder::new(manifest, session_id);
    let events = builder.capture_plan(&plan, shift_target_rows);
    let session = builder.finish();
    let clocks = ClockEpochSet::fit_session(manifest, &session).expect("held-out clocks fit");
    let journal = CalibrationCaptureJournal::new(CalibrationCaptureInput {
        purpose: EvidencePurpose::HeldOutValidation,
        acquisition_manifest_sha256: manifest.sha256().expect("held-out manifest hashes"),
        clock_epochs_sha256: clocks.fingerprint().expect("held-out clocks hash"),
        session_sha256: session.sha256(),
        session_id,
        keyboard_device_id: artifact.keyboard.physical_path.clone(),
        keyboard: artifact.keyboard.clone(),
        setup_seconds: 390,
        created_unix_ns: 1_786_233_600_000_000_000,
        pre_rejected_examples: 0,
        neutral_windows: Vec::new(),
        matching_rule: Some(calibration_matching_rule(manifest)),
        surface_plan: Some(
            CalibrationSurfacePlanBinding::held_out(&plan, HELDOUT_PLAN_START_NS)
                .expect("held-out plan binding is valid"),
        ),
        events,
    })
    .expect("held-out capture journal is complete");
    journal
        .validate_complete_surface_plan()
        .expect("held-out capture covers the immutable plan");
    let validated = validate_calibration(manifest, &clocks, artifact, &journal, &session)
        .expect("held-out capture reevaluates");
    if shift_target_rows {
        assert!(
            !validated.heldout_validation_passed(),
            "row-shifted physical taps must fail held-out validation"
        );
    }
    RuntimeCalibrationBundleBytes {
        manifest_sha256: manifest.sha256().expect("runtime manifest hashes"),
        artifact_sha256,
        validation_passed: validated.heldout_validation_passed(),
        clocks: clocks.canonical_json().expect("held-out clocks serialize"),
        capture: journal
            .canonical_json()
            .expect("held-out capture serializes"),
        surface_plan: plan
            .canonical_json()
            .expect("held-out surface plan serializes"),
        evidence: validated
            .diagnostic_evidence
            .canonical_json()
            .expect("held-out diagnostic evidence serializes"),
        session: session.bytes().to_vec(),
        report: validated
            .report
            .canonical_json()
            .expect("validation report serializes"),
    }
}

struct HeldoutSessionBuilder<'a> {
    manifest: &'a AcquisitionManifest,
    writer: SessionWriter<Vec<u8>>,
    trials: [u64; 2],
    sequences: [u32; 2],
    next_host_time_ns: u64,
}

impl<'a> HeldoutSessionBuilder<'a> {
    fn new(manifest: &'a AcquisitionManifest, session_id: [u8; 16]) -> Self {
        let writer = SessionWriter::new(
            Vec::new(),
            SessionHeader {
                nominal_period_us: manifest.nominal_period_us,
                manifest_sha256: manifest.sha256().expect("held-out manifest hashes"),
                session_id,
            },
        )
        .expect("held-out session starts");
        Self {
            manifest,
            writer,
            trials: [0, 0],
            sequences: [0, 0],
            next_host_time_ns: HELDOUT_PLAN_START_NS,
        }
    }

    fn append_window(
        &mut self,
        hand: Hand,
        position: Option<PhysicalPosition>,
        not_before_host_time_ns: u64,
    ) -> u64 {
        let hand_index = hand as usize;
        let identity = self.manifest.hands[hand_index]
            .frame_identity()
            .expect("runtime fixture has both wire identities");
        let config = SyntheticConfig::new(hand, 0x2000 + hand as u64)
            .with_noise(0)
            .with_wire_identity(identity.controller_id_sha256, identity.boot_id);
        let trial = self.trials[hand_index];
        self.trials[hand_index] += 1;
        let window = position.map_or_else(
            || config.idle_window(trial),
            |position| config.tap_window(position, trial),
        );
        self.next_host_time_ns = self.next_host_time_ns.max(not_before_host_time_ns);
        let mut trigger_host_time_ns = 0;
        for frame in window.frames() {
            let mut frame = *frame;
            frame.sequence = self.sequences[hand_index];
            self.sequences[hand_index] = self.sequences[hand_index].wrapping_add(1);
            frame.device_time_us = self.next_host_time_ns / 1_000;
            trigger_host_time_ns = self.next_host_time_ns;
            self.writer
                .append_frame(ControllerStream::from(hand), &frame, trigger_host_time_ns)
                .expect("held-out frame writes");
            self.next_host_time_ns = self
                .next_host_time_ns
                .saturating_add(u64::from(self.manifest.nominal_period_us) * 1_000);
        }
        trigger_host_time_ns
    }

    fn capture_plan(
        &mut self,
        plan: &HeldOutSurfacePlan,
        shift_target_rows: bool,
    ) -> Vec<CapturedCalibrationEvent> {
        let mut events = Vec::new();
        let mut instruction_revision = 0_u64;
        let mut ordinary_segments_seen = [0_u32; 2];
        for segment in plan.segments() {
            let segment_start_ns = HELDOUT_PLAN_START_NS.saturating_add(segment.start_ns());
            for presentation in segment.scheduled_presentations() {
                let planned_host_time_ns = HELDOUT_PLAN_START_NS
                    .checked_add(presentation.at_ns())
                    .expect("held-out presentation time fits the host clock");
                instruction_revision = instruction_revision.saturating_add(1);
                events.push(CapturedCalibrationEvent::DisplayAcknowledged {
                    prompt_id: segment.id().clone(),
                    presentation: SurfacePresentationReceipt::new(
                        planned_host_time_ns,
                        planned_host_time_ns,
                        planned_host_time_ns,
                        instruction_revision,
                    )
                    .expect("surface presentation receipt is ordered"),
                });
            }
            match segment.kind() {
                SurfaceSegmentKind::PromptedTap => {
                    let target = plan
                        .scoring_targets()
                        .iter()
                        .find(|target| target.prompt_id() == segment.id())
                        .expect("every prompted segment has scoring metadata");
                    let position = target.position();
                    let sensed_position = if shift_target_rows {
                        shifted_row_position(position)
                    } else {
                        position
                    };
                    let cue_host_time_ns = HELDOUT_PLAN_START_NS.saturating_add(
                        segment
                            .cue_at_ns()
                            .expect("prompted surface segment has a cue"),
                    );
                    self.append_window(position.hand, None, cue_host_time_ns);
                    self.append_window(position.hand, None, 0);
                    self.append_window(position.hand, Some(sensed_position), 0);
                    // The impulse lands at the tail of the synthetic tap.
                    // Keep the physical stream continuous so the frozen
                    // two-window debounce completes inside the response gate.
                    self.append_window(position.hand, None, 0);
                }
                SurfaceSegmentKind::QuietNull => {
                    let left_trigger = self.append_window(Hand::Left, None, segment_start_ns);
                    events.push(CapturedCalibrationEvent::Idle {
                        prompt_id: segment.id().clone(),
                        sample_ordinal: 0,
                        trigger_host_time_ns: left_trigger,
                    });
                    let right_trigger = self.append_window(Hand::Right, None, 0);
                    events.push(CapturedCalibrationEvent::Idle {
                        prompt_id: segment.id().clone(),
                        sample_ordinal: 1,
                        trigger_host_time_ns: right_trigger,
                    });
                }
                SurfaceSegmentKind::OrdinaryMotion => {
                    let hand = segment
                        .motion_hand()
                        .expect("ordinary-motion segment names one hand");
                    let hand_index = hand as usize;
                    let sample_count = if ordinary_segments_seen[hand_index] < 2 {
                        267
                    } else {
                        266
                    };
                    ordinary_segments_seen[hand_index] += 1;
                    for sample_ordinal in 0..sample_count {
                        let trigger_host_time_ns = self.append_window(hand, None, segment_start_ns);
                        events.push(CapturedCalibrationEvent::Idle {
                            prompt_id: segment.id().clone(),
                            sample_ordinal,
                            trigger_host_time_ns,
                        });
                    }
                }
                SurfaceSegmentKind::ThresholdTap | SurfaceSegmentKind::ThresholdQuiet => {
                    panic!("held-out plan cannot contain fitting-only segments")
                }
            }
        }
        events
    }

    fn finish(self) -> FinalizedSession {
        self.writer
            .finish_finalized()
            .expect("held-out session finalizes")
    }
}

fn shifted_row_position(position: PhysicalPosition) -> PhysicalPosition {
    let same_finger = all_slots()
        .iter()
        .filter(|slot| {
            slot.position.hand == position.hand && slot.position.finger == position.finger
        })
        .map(|slot| slot.position)
        .collect::<Vec<_>>();
    let current = same_finger
        .iter()
        .position(|candidate| *candidate == position)
        .expect("fixture position belongs to its hand/finger group");
    same_finger[(current + 1) % same_finger.len()]
}

struct LiveCliFixture {
    calibration_manifest: PathBuf,
    rig_evidence: PathBuf,
    axis_characterization: PathBuf,
    runtime_template: PathBuf,
    artifact: PathBuf,
    calibration_output: PathBuf,
}

impl LiveCliFixture {
    fn new(test_directory: &std::ffi::OsStr, label: &str) -> Self {
        let calibration_manifest = write_manifest(test_directory, label, 208);
        let mut manifest = read_bound_manifest(&calibration_manifest);
        manifest.prompt_corpus_sha256 = format_sha256(&PromptCorpus::v1().sha256());
        rewrite_manifest_with_existing_evidence(&calibration_manifest, &manifest);
        let manifest_evidence = manifest_evidence_files(&calibration_manifest);
        let runtime_template = calibration_manifest.with_extension("runtime-template.json");
        write_template_from_manifest(
            calibration_manifest
                .to_str()
                .expect("calibration manifest path is UTF-8"),
            &runtime_template,
        );
        let artifact = calibration_manifest.with_extension("artifact.json");
        let artifact_value = write_calibration_artifact(&manifest, &artifact);
        let calibration_output = calibration_manifest.with_extension("calibration-output");
        write_runtime_calibration_output(&manifest, &artifact_value, &calibration_output);
        Self {
            calibration_manifest,
            rig_evidence: manifest_evidence.rig,
            axis_characterization: manifest_evidence.axis,
            runtime_template,
            artifact,
            calibration_output,
        }
    }
}

struct LiveCliOutputs {
    runtime_manifest: PathBuf,
    session: PathBuf,
    clock_report: PathBuf,
    left_fifo: PathBuf,
    right_fifo: PathBuf,
}

impl LiveCliOutputs {
    fn new(test_directory: &std::ffi::OsStr, label: &str) -> Self {
        static LIVE_OUTPUT_ID: AtomicU64 = AtomicU64::new(0);
        let output_id = LIVE_OUTPUT_ID.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(test_directory).join(format!(
            "{label}-live-{}-{}",
            std::process::id(),
            output_id
        ));
        Self {
            runtime_manifest: root.with_extension("manifest.json"),
            session: root.with_extension("iks"),
            clock_report: root.with_extension("live-clock-report.json"),
            left_fifo: root.with_extension("left.fifo"),
            right_fifo: root.with_extension("right.fifo"),
        }
    }

    fn create_fifos(&self) {
        let status = Command::new("mkfifo")
            .args([&self.left_fifo, &self.right_fifo])
            .status()
            .expect("mkfifo runs");
        assert!(status.success(), "live source FIFOs are created");
    }
}

fn spawn_live_cli(fixture: &LiveCliFixture, outputs: &LiveCliOutputs) -> (Child, File, File) {
    spawn_live_cli_with_mode(fixture, outputs, Some("recording"))
}

fn spawn_live_cli_with_mode(
    fixture: &LiveCliFixture,
    outputs: &LiveCliOutputs,
    mode: Option<&str>,
) -> (Child, File, File) {
    outputs.create_fifos();
    let mut arguments = vec![
        "run-calibrated",
        fixture
            .calibration_manifest
            .to_str()
            .expect("calibration manifest path is UTF-8"),
        fixture
            .rig_evidence
            .to_str()
            .expect("rig evidence path is UTF-8"),
        fixture
            .axis_characterization
            .to_str()
            .expect("axis characterization path is UTF-8"),
        fixture
            .runtime_template
            .to_str()
            .expect("runtime template path is UTF-8"),
        fixture.artifact.to_str().expect("artifact path is UTF-8"),
        fixture
            .calibration_output
            .to_str()
            .expect("calibration output path is UTF-8"),
        outputs.left_fifo.to_str().expect("left FIFO path is UTF-8"),
        outputs
            .right_fifo
            .to_str()
            .expect("right FIFO path is UTF-8"),
        outputs
            .runtime_manifest
            .to_str()
            .expect("runtime manifest output path is UTF-8"),
        outputs
            .session
            .to_str()
            .expect("session output path is UTF-8"),
        outputs
            .clock_report
            .to_str()
            .expect("live clock report output path is UTF-8"),
    ];
    if let Some(mode) = mode {
        arguments.push(mode);
    }
    arguments.push("--confirm-wearables-unmoved");
    let child = command_binary("INVISIBLE_KEYBOARD_RLOCATION", &arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("live process starts");
    // The fixture is valid through device admission, so each write-only open
    // pairs with the command's one read-only open of the same real FIFO.
    let left = fs::OpenOptions::new()
        .write(true)
        .open(&outputs.left_fifo)
        .expect("left FIFO writer opens");
    let right = fs::OpenOptions::new()
        .write(true)
        .open(&outputs.right_fifo)
        .expect("right FIFO writer opens");
    (child, left, right)
}

fn run_live_without_devices(
    fixture: &LiveCliFixture,
    outputs: &LiveCliOutputs,
) -> std::process::Output {
    run(&[
        "run-calibrated",
        fixture
            .calibration_manifest
            .to_str()
            .expect("calibration manifest path is UTF-8"),
        fixture
            .rig_evidence
            .to_str()
            .expect("rig evidence path is UTF-8"),
        fixture
            .axis_characterization
            .to_str()
            .expect("axis characterization path is UTF-8"),
        fixture
            .runtime_template
            .to_str()
            .expect("runtime template path is UTF-8"),
        fixture.artifact.to_str().expect("artifact path is UTF-8"),
        fixture
            .calibration_output
            .to_str()
            .expect("calibration output path is UTF-8"),
        "missing-left-controller",
        "missing-right-controller",
        outputs
            .runtime_manifest
            .to_str()
            .expect("runtime manifest output path is UTF-8"),
        outputs
            .session
            .to_str()
            .expect("session output path is UTF-8"),
        outputs
            .clock_report
            .to_str()
            .expect("clock report output path is UTF-8"),
        "recording",
        "--confirm-wearables-unmoved",
    ])
}

fn assert_no_live_outputs(outputs: &LiveCliOutputs) {
    assert!(!outputs.runtime_manifest.exists());
    assert!(!outputs.session.exists());
    assert!(!outputs.clock_report.exists());
}

fn runtime_frame(hand: Hand, boot: u8, sequence: u32, device_time_us: u64) -> Vec<u8> {
    let mut frame = sensor_frame(hand, sequence);
    frame.boot_id = [boot; 16];
    frame.device_time_us = device_time_us;
    encoded_transport_frame(&frame)
}

fn write_runtime_source_frame(
    source: &mut File,
    hand: Hand,
    boot: u8,
    sequence: u32,
    device_time_us: u64,
) -> std::io::Result<()> {
    source.write_all(&runtime_frame(hand, boot, sequence, device_time_us))?;
    source.flush()
}

fn spawn_continuous_runtime_writer(
    mut source: File,
    hand: Hand,
    boot: u8,
    keep_writing: Arc<AtomicBool>,
    stream_start: Instant,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut sequence = 0_u32;
        while keep_writing.load(Ordering::SeqCst) {
            let device_time_us =
                u64::try_from(stream_start.elapsed().as_micros()).unwrap_or(u64::MAX);
            if write_runtime_source_frame(&mut source, hand, boot, sequence, device_time_us)
                .is_err()
            {
                break;
            }
            sequence = sequence.wrapping_add(1);
            thread::sleep(Duration::from_millis(6));
        }
    })
}

fn write_live_runtime_frame(
    child: &mut Child,
    source: &mut File,
    hand: Hand,
    boot: u8,
    sequence: u32,
    device_time_us: u64,
    context: &str,
) {
    let written = source
        .write_all(&runtime_frame(hand, boot, sequence, device_time_us))
        .and_then(|()| source.flush());
    let Err(error) = written else {
        return;
    };
    if error.kind() != std::io::ErrorKind::BrokenPipe {
        panic!("{context} {hand:?} frame {sequence} failed: {error}");
    }

    wait_for_child(child, Duration::from_secs(2), context);
    let status = child.wait().expect("failed live process status collects");
    let mut stderr = String::new();
    child
        .stderr
        .as_mut()
        .expect("live process stderr is piped")
        .read_to_string(&mut stderr)
        .expect("failed live process stderr reads");
    panic!(
        "{context} {hand:?} frame {sequence} hit {error}; child exited with {status}; stderr:\n{stderr}"
    );
}

struct LiveRuntimeWriters<'a> {
    child: &'a mut Child,
    left: &'a mut File,
    right: &'a mut File,
}

const RUNTIME_PAIR_PERIOD_US: u64 = 8_000;

fn write_live_runtime_pairs(
    writers: LiveRuntimeWriters<'_>,
    boots: [u8; 2],
    first_sequences: [u32; 2],
    pair_count: u32,
    stream_start: Instant,
    context: &str,
) {
    for offset in 0..pair_count {
        thread::sleep(Duration::from_micros(RUNTIME_PAIR_PERIOD_US));
        let device_time_us = u64::try_from(stream_start.elapsed().as_micros()).unwrap_or(u64::MAX);
        write_live_runtime_frame(
            writers.child,
            writers.left,
            Hand::Left,
            boots[0],
            first_sequences[0] + offset,
            device_time_us,
            context,
        );
        write_live_runtime_frame(
            writers.child,
            writers.right,
            Hand::Right,
            boots[1],
            first_sequences[1] + offset,
            device_time_us,
            context,
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum PlannedProcessSignal {
    Interrupt,
    Terminate,
}

impl PlannedProcessSignal {
    const ALL: [Self; 2] = [Self::Interrupt, Self::Terminate];

    fn kill_argument(self) -> &'static str {
        match self {
            Self::Interrupt => "-INT",
            Self::Terminate => "-TERM",
        }
    }

    fn fixture_label(self) -> &'static str {
        match self {
            Self::Interrupt => "sigint",
            Self::Terminate => "sigterm",
        }
    }

    fn shutdown_label(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Terminate => "terminate",
        }
    }
}

fn send_planned_signal(child: &Child, signal: PlannedProcessSignal) {
    let _ = send_planned_signal_and_timestamp(child, signal);
}

fn send_planned_signal_and_timestamp(child: &Child, signal: PlannedProcessSignal) -> Instant {
    let started = Instant::now();
    let status = Command::new("kill")
        .args([signal.kill_argument(), &child.id().to_string()])
        .status()
        .expect("kill command runs");
    assert!(
        status.success(),
        "{} reaches the live process",
        signal.fixture_label()
    );
    started
}

fn assert_planned_shutdown_receipt(stdout: &str, signal: PlannedProcessSignal) {
    let expected_signal = format!("shutdown_signal={}", signal.shutdown_label());
    let observed_signals = stdout
        .lines()
        .filter(|line| line.starts_with("shutdown_signal="))
        .collect::<Vec<_>>();
    assert_eq!(observed_signals, [expected_signal.as_str()]);
    assert!(stdout.contains("shutdown_neutralization=confirmed"));
    assert!(stdout.contains("output_neutrality_confirmed=true"));
    assert!(stdout.contains("evidence_finalization=durable"));
    assert!(!stdout.contains("neutralization_failure_"));
    let neutralization_latency_ns = stdout
        .lines()
        .find_map(|line| line.strip_prefix("adapter_neutralization_latency_ns="))
        .expect("adapter neutralization latency is reported")
        .parse::<u64>()
        .expect("adapter neutralization latency is numeric");
    assert!(neutralization_latency_ns <= 250_000_000);
    assert!(
        stdout.find("shutdown_neutralization=confirmed")
            < stdout.find("evidence_finalization=durable")
    );
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    condition()
}

fn wait_for_child(child: &mut Child, timeout: Duration, context: &str) {
    if wait_until(timeout, || {
        child.try_wait().expect("child status polls").is_some()
    }) {
        return;
    }

    let child_id = child.id();
    let wait_channel: Option<String> = {
        #[cfg(target_os = "linux")]
        {
            fs::read_to_string(format!("/proc/{child_id}/wchan"))
                .ok()
                .map(|value| value.trim().to_owned())
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    };
    let mut stdout = child.stdout.take().expect("live process stdout is piped");
    let mut stderr = child.stderr.take().expect("live process stderr is piped");

    let kill_result = child.kill();
    let wait_result = child.wait();

    let mut captured_stdout = Vec::new();
    let stdout_read = stdout.read_to_end(&mut captured_stdout);
    let mut captured_stderr = Vec::new();
    let stderr_read = stderr.read_to_end(&mut captured_stderr);

    panic!(
        "{context} did not exit within {timeout:?}; child={child_id}; \
         wchan={wait_channel:?}; kill={kill_result:?}; wait={wait_result:?}; \
         stdout_read={stdout_read:?}; stderr_read={stderr_read:?}\n\
         stdout:\n{}\n\
         stderr:\n{}",
        String::from_utf8_lossy(&captured_stdout),
        String::from_utf8_lossy(&captured_stderr),
    );
}

struct ConcurrentChildOutput {
    child: Child,
    lines: mpsc::Receiver<(Instant, String)>,
    stdout_reader: Option<thread::JoinHandle<Vec<u8>>>,
    stderr_reader: Option<thread::JoinHandle<Vec<u8>>>,
    reaped: bool,
}

struct CapturedChildOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ConcurrentChildOutput {
    fn start(mut child: Child) -> Self {
        let stdout = child.stdout.take().expect("live process stdout is piped");
        let mut stderr = child.stderr.take().expect("live process stderr is piped");
        let (line_sender, lines) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut captured = Vec::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => return captured,
                    Ok(_) => {
                        captured.extend_from_slice(line.as_bytes());
                        let _ = line_sender.send((Instant::now(), line));
                    }
                    Err(error) => panic!("live stdout read failed: {error}"),
                }
            }
        });
        let stderr_reader = thread::spawn(move || {
            let mut captured = Vec::new();
            stderr
                .read_to_end(&mut captured)
                .expect("live stderr reads");
            captured
        });
        Self {
            child,
            lines,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            reaped: false,
        }
    }

    fn child(&self) -> &Child {
        &self.child
    }

    fn is_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("concurrently captured child status polls")
            .is_none()
    }

    fn wait_for_line_since(
        &self,
        started: Instant,
        timeout: Duration,
        expected: &str,
        context: &str,
    ) -> Result<Duration, String> {
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(format!(
                    "{context} did not publish '{expected}' within {timeout:?}"
                ));
            }
            match self.lines.recv_timeout(remaining) {
                Ok((observed_at, line))
                    if observed_at >= started && line.trim_end() == expected =>
                {
                    return Ok(observed_at.saturating_duration_since(started));
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "{context} did not publish '{expected}' within {timeout:?}"
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("{context} exited before publishing '{expected}'"));
                }
            }
        }
    }

    fn finish(mut self, timeout: Duration, context: &str) -> CapturedChildOutput {
        let completed = wait_until(timeout, || {
            self.child
                .try_wait()
                .expect("concurrently captured child status polls")
                .is_some()
        });
        let wait_channel = if completed {
            None
        } else {
            #[cfg(target_os = "linux")]
            {
                fs::read_to_string(format!("/proc/{}/wchan", self.child.id()))
                    .ok()
                    .map(|value| value.trim().to_owned())
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        };
        if !completed {
            let _ = self.child.kill();
        }
        let status = self.child.wait().expect("captured child is reaped");
        self.reaped = true;
        let stdout = self
            .stdout_reader
            .take()
            .expect("stdout reader is owned")
            .join()
            .expect("stdout reader joins");
        let stderr = self
            .stderr_reader
            .take()
            .expect("stderr reader is owned")
            .join()
            .expect("stderr reader joins");

        if !completed {
            let stderr_text = String::from_utf8_lossy(&stderr);
            panic!(
                "{context} finalization exceeded infrastructure watchdog {timeout:?}; \
                 wchan={wait_channel:?}; status={status}\n\
                 stdout:\n{}\n\
                stderr:\n{}",
                String::from_utf8_lossy(&stdout),
                stderr_text,
            );
        }
        CapturedChildOutput {
            status,
            stdout,
            stderr,
        }
    }
}

impl Drop for ConcurrentChildOutput {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

fn write_live_pairs_until_session_grows(
    writers: LiveRuntimeWriters<'_>,
    boots: [u8; 2],
    mut next_sequences: [u32; 2],
    stream_start: Instant,
    path: &std::path::Path,
    previous_len: u64,
    context: &str,
) -> ([u32; 2], u64) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current_len = fs::metadata(path).map_or(0, |metadata| metadata.len());
        if current_len > previous_len {
            return (next_sequences, current_len);
        }
        assert!(
            Instant::now() < deadline,
            "{context} did not persist raw session progress beyond {previous_len} bytes"
        );

        // Waiting for a buffered session flush must not create a synthetic
        // controller outage. Keep both real FIFOs active inside the frozen
        // 250 ms source/clock horizon while polling the persistence boundary.
        write_live_runtime_pairs(
            LiveRuntimeWriters {
                child: &mut *writers.child,
                left: &mut *writers.left,
                right: &mut *writers.right,
            },
            boots,
            next_sequences,
            1,
            stream_start,
            context,
        );
        next_sequences[0] = next_sequences[0].wrapping_add(1);
        next_sequences[1] = next_sequences[1].wrapping_add(1);
    }
}

fn assert_live_bundle(
    fixture: &LiveCliFixture,
    outputs: &LiveCliOutputs,
    boots: [u8; 2],
) -> FinalizedSession {
    let runtime_manifest = AcquisitionManifest::from_json_with_evidence(
        &fs::read(&outputs.runtime_manifest).expect("runtime manifest reads"),
        &fs::read(&fixture.rig_evidence).expect("rig evidence reads"),
        &fs::read(&fixture.axis_characterization).expect("axis characterization reads"),
    )
    .expect("runtime manifest binds physical evidence");
    let artifact =
        CalibrationArtifact::from_json(&fs::read(&fixture.artifact).expect("artifact reads"))
            .expect("artifact parses");
    let validation = ValidationReport::from_json(
        &fs::read(fixture.calibration_output.join("validation-report.json"))
            .expect("validation report reads"),
    )
    .expect("validation report parses");
    assert_eq!(
        runtime_manifest.hands[0].parsed_boot_id(),
        Some([boots[0]; 16])
    );
    assert_eq!(
        runtime_manifest.hands[1].parsed_boot_id(),
        Some([boots[1]; 16])
    );
    assert_eq!(
        runtime_manifest.calibration_artifact_sha256,
        Some(format_sha256(
            &artifact.fingerprint().expect("artifact hashes")
        ))
    );
    assert_eq!(
        runtime_manifest.calibration_validation_sha256,
        Some(format_sha256(
            &validation.fingerprint().expect("validation report hashes")
        ))
    );

    let session = FinalizedSession::audit(fs::read(&outputs.session).expect("session reads"))
        .expect("session audits");
    let clock_report = LiveClockReport::from_json(
        &fs::read(&outputs.clock_report).expect("live clock report reads"),
    )
    .expect("live clock report parses");
    let session_audit = audit_session_reader(session.bytes()).expect("session re-audits");
    clock_report
        .bind(&runtime_manifest, &session_audit)
        .expect("live clock report binds runtime manifest and session");
    assert_eq!(clock_report.operational_clocks.len(), 2);
    session
}

fn assert_exact_hand_stream(summary: HandStreamSummary, expected: u32, context: &str) {
    assert_eq!(summary.received, u64::from(expected), "{context} count");
    assert_eq!(summary.first_sequence, Some(0), "{context} first sequence");
    assert_eq!(
        summary.last_in_order_sequence,
        expected.checked_sub(1),
        "{context} last sequence"
    );
    assert_eq!(summary.missing, 0, "{context} missing");
    assert_eq!(summary.duplicates, 0, "{context} duplicates");
    assert_eq!(summary.reordered, 0, "{context} reordered");
    assert_eq!(
        summary.device_time_regressions, 0,
        "{context} device-time regressions"
    );
    assert_eq!(
        summary.host_time_regressions, 0,
        "{context} host-time regressions"
    );
    assert_eq!(
        summary.frames_with_invalid_sensors, 0,
        "{context} invalid frames"
    );
    assert_eq!(
        summary.invalid_sensor_readings, 0,
        "{context} invalid sensor readings"
    );
    assert_eq!(summary.clock_unsynced, 0, "{context} clock faults");
    assert_eq!(summary.acquisition_overruns, 0, "{context} overruns");
}

fn assert_atomic_cutoff_prefix(summary: HandStreamSummary, sent: u32, context: &str) {
    let admitted = u32::try_from(summary.received).expect("test stream count fits u32");
    assert!(admitted >= 25, "{context} retains the armed prefix");
    assert!(
        admitted <= sent,
        "{context} cannot retain frames the FIFO writer never sent"
    );
    // A successful FIFO write proves kernel acceptance, not reader-mailbox
    // admission. The mailbox unit contract proves every publish before the
    // atomic close is received or drained; this integration check proves the
    // resulting session is one exact, gap-free prefix of the sent stream.
    assert_exact_hand_stream(summary, admitted, context);
}

#[derive(Default)]
struct LiveHandSessionDiagnostics {
    receipt_times_ns: BTreeSet<u64>,
    first_device_time_us: Option<u64>,
    last_device_time_us: Option<u64>,
}

impl LiveHandSessionDiagnostics {
    fn observe(&mut self, host_time_ns: u64, device_time_us: u64) {
        self.receipt_times_ns.insert(host_time_ns);
        self.first_device_time_us.get_or_insert(device_time_us);
        self.last_device_time_us = Some(device_time_us);
    }

    fn receipt_span_ns(&self) -> Option<u64> {
        Some(
            self.receipt_times_ns
                .last()?
                .saturating_sub(*self.receipt_times_ns.first()?),
        )
    }

    fn device_span_us(&self) -> Option<u64> {
        Some(
            self.last_device_time_us?
                .saturating_sub(self.first_device_time_us?),
        )
    }
}

fn live_session_diagnostics(path: &std::path::Path) -> String {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return format!("session unavailable: {error}"),
    };
    let session = match FinalizedSession::audit(bytes) {
        Ok(session) => session,
        Err(error) => return format!("session audit failed: {error}"),
    };
    let summary = session.summary();
    let mut left = LiveHandSessionDiagnostics::default();
    let mut right = LiveHandSessionDiagnostics::default();
    let mut reader = session.reader();
    loop {
        let record = match reader.next_record() {
            Ok(Some(record)) => record,
            Ok(None) => break,
            Err(error) => return format!("session record read failed: {error}"),
        };
        let Ok(frame) = record.decode() else {
            continue;
        };
        match record.source {
            ControllerStream::Left => left.observe(record.host_time_ns, frame.device_time_us),
            ControllerStream::Right => right.observe(record.host_time_ns, frame.device_time_us),
        }
    }

    format!(
        "session: left received={} sequence={:?}..{:?} receipts={} span_ns={:?} device_span_us={:?}; right received={} sequence={:?}..{:?} receipts={} span_ns={:?} device_span_us={:?}",
        summary.left.received,
        summary.left.first_sequence,
        summary.left.last_in_order_sequence,
        left.receipt_times_ns.len(),
        left.receipt_span_ns(),
        left.device_span_us(),
        summary.right.received,
        summary.right.first_sequence,
        summary.right.last_in_order_sequence,
        right.receipt_times_ns.len(),
        right.receipt_span_ns(),
        right.device_span_us(),
    )
}

#[test]
fn firmware_and_host_frame_fixtures_are_byte_identical() {
    let host = run(&["frame-fixture"]);
    let firmware = run_binary("FIRMWARE_FIXTURE_RLOCATION", &[]);

    assert!(host.status.success());
    assert!(firmware.status.success());
    assert_eq!(firmware.stdout, host.stdout);
}

#[test]
fn help_names_the_hardware_free_commands() {
    let output = run(&["help"]);
    let stdout = String::from_utf8(output.stdout).expect("help is UTF-8");

    assert!(output.status.success());
    assert!(stdout.contains("layout"));
    assert!(stdout.contains("simulate"));
    assert!(stdout.contains("frame-fixture"));
    #[cfg(target_os = "linux")]
    assert!(stdout.contains("uinput-check"));
    #[cfg(target_os = "linux")]
    assert!(stdout.contains("uinput-probe"));
    #[cfg(target_os = "linux")]
    assert!(stdout.contains("uinput-stress"));
    assert!(stdout.contains("session-fixture"));
    assert!(stdout.contains("verify-session"));
    assert!(stdout.contains("record-stream"));
    assert!(stdout.contains("record-hand"));
    assert!(stdout.contains("score-gate0a"));
    assert!(stdout.contains("score-pilot"));
    assert!(stdout.contains("physical-study-plan"));
    assert!(stdout.contains("physical-study-seal"));
    assert!(stdout.contains("physical-study-score"));
    assert!(stdout.contains("calibration-demo"));
    #[cfg(target_os = "linux")]
    assert!(stdout.contains("calibrate-hardware"));
    assert!(stdout.contains("fit-session-clocks"));
    assert!(stdout.contains("build-calibration-evidence"));
    assert!(stdout.contains("fit-calibration"));
    assert!(stdout.contains("validate-calibration"));
    assert!(stdout.contains("calibrate-replay"));
    assert!(stdout.contains("replay-calibrated"));
    assert!(stdout.contains("run-calibrated"));
    assert!(stdout.contains("capture one or two source-bound COBS streams in template order"));
    for command in [
        "record-stream",
        "record-hand",
        "score-gate0a",
        "score-pilot",
        "verify-session",
        "fit-session-clocks",
        "build-calibration-evidence",
        "fit-calibration",
        "validate-calibration",
        "calibrate-replay",
        "replay-calibrated",
        "run-calibrated",
    ] {
        let command_help = stdout
            .lines()
            .find(|line| line.trim_start().starts_with(command))
            .expect("operational command appears in help");
        assert!(
            command_help.contains("<rig-evidence> <axis-characterization>"),
            "{command} help must require both physical evidence paths"
        );
    }
    let fixture_help = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("session-fixture"))
        .expect("session fixture appears in help");
    assert!(!fixture_help.contains("rig-evidence"));
    #[cfg(target_os = "linux")]
    {
        assert!(stdout.contains("list-keyboards"));
        let hardware_help = stdout
            .lines()
            .find(|line| line.trim_start().starts_with("calibrate-hardware"))
            .expect("hardware calibration appears in help");
        assert!(hardware_help.contains("<rig-evidence> <axis-characterization>"));
    }
}

#[test]
fn physical_study_commands_reject_wrong_arity() {
    for (command, expected) in [
        (
            "physical-study-plan",
            "requires roster JSONL, split manifest output, and analysis plan output paths",
        ),
        (
            "physical-study-seal",
            "requires one row JSON input and an append-only evidence JSONL path",
        ),
        (
            "physical-study-score",
            "requires evidence JSONL, split manifest, analysis plan, and report output paths",
        ),
    ] {
        let rejected = run(&[command]);
        assert!(
            !rejected.status.success(),
            "{command} must reject missing paths"
        );
        assert!(
            String::from_utf8(rejected.stderr)
                .expect("physical-study usage error is UTF-8")
                .contains(expected)
        );
    }
}

#[test]
fn physical_study_plan_writes_the_exact_canonical_manifest_and_plan() {
    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let roster_path = test_directory.join("physical-study-exact-roster.jsonl");
    let manifest_path = test_directory.join("physical-study-exact-manifest.json");
    let plan_path = test_directory.join("physical-study-exact-plan.json");
    let rows = vec![
        physical_study_cli_row(
            PhysicalStudyGate::Gate0B,
            StudySplit::Heldout,
            PhysicalStudyCliIdentity::new("plan-person-b", "plan-person-b-mount", "plan-session-b"),
            Hand::Right,
            StudyCondition::IntentionalTap,
            Some("index"),
            0,
        ),
        physical_study_cli_row(
            PhysicalStudyGate::Gate0B,
            StudySplit::Heldout,
            PhysicalStudyCliIdentity::new("plan-person-a", "plan-person-a-mount", "plan-session-a"),
            Hand::Left,
            StudyCondition::IntentionalTap,
            Some("thumb"),
            0,
        ),
    ];
    fs::write(
        &roster_path,
        encode_physical_study_jsonl(&rows).expect("unsealed roster encodes"),
    )
    .expect("roster writes");
    let expected_manifest =
        PhysicalStudySplitManifest::from_rows(&rows).expect("expected manifest builds");
    let expected_manifest_bytes = expected_manifest
        .canonical_json_bytes()
        .expect("expected manifest serializes");
    let expected_manifest_sha256 = format_sha256(&sha256(&expected_manifest_bytes));
    let expected_plan = PhysicalStudyAnalysisPlan::v1(
        expected_manifest.gate,
        expected_manifest.round_id.clone(),
        expected_manifest_sha256.clone(),
        FrozenStudyPipeline::from(&rows[0]),
    );
    let expected_plan_bytes = expected_plan
        .canonical_json_bytes()
        .expect("expected plan serializes");

    let planned = run(&[
        "physical-study-plan",
        path_text(&roster_path),
        path_text(&manifest_path),
        path_text(&plan_path),
    ]);
    assert!(
        planned.status.success(),
        "physical-study planning failed: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    assert_eq!(
        fs::read(&manifest_path).expect("planned manifest reads"),
        expected_manifest_bytes
    );
    assert_eq!(
        fs::read(&plan_path).expect("planned analysis plan reads"),
        expected_plan_bytes
    );
    let stdout = String::from_utf8(planned.stdout).expect("planning status is UTF-8");
    assert!(stdout.contains("physical_study_round=frozen"));
    assert!(stdout.contains(&format!("manifest_sha256={expected_manifest_sha256}")));
    assert!(stdout.contains("expected_rows=2"));
}

#[test]
fn physical_study_plan_failures_leave_no_partial_authoritative_pair() {
    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));

    let empty_roster = test_directory.join("physical-study-empty-roster.jsonl");
    let empty_manifest = test_directory.join("physical-study-empty-manifest.json");
    let empty_plan = test_directory.join("physical-study-empty-plan.json");
    fs::write(&empty_roster, []).expect("empty roster writes");
    let empty = run(&[
        "physical-study-plan",
        path_text(&empty_roster),
        path_text(&empty_manifest),
        path_text(&empty_plan),
    ]);
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("evidence JSONL is empty"));
    assert!(!empty_manifest.exists());
    assert!(!empty_plan.exists());

    let mixed_roster = test_directory.join("physical-study-mixed-roster.jsonl");
    let mixed_manifest = test_directory.join("physical-study-mixed-manifest.json");
    let mixed_plan = test_directory.join("physical-study-mixed-plan.json");
    let mut mixed_rows = vec![
        physical_study_cli_row(
            PhysicalStudyGate::Gate0B,
            StudySplit::Heldout,
            PhysicalStudyCliIdentity::new("mixed-person-a", "mixed-mount-a", "mixed-session-a"),
            Hand::Left,
            StudyCondition::IntentionalTap,
            Some("thumb"),
            0,
        ),
        physical_study_cli_row(
            PhysicalStudyGate::Gate0B,
            StudySplit::Heldout,
            PhysicalStudyCliIdentity::new("mixed-person-b", "mixed-mount-b", "mixed-session-b"),
            Hand::Right,
            StudyCondition::IntentionalTap,
            Some("index"),
            0,
        ),
    ];
    mixed_rows[1].round_id = "another-round".to_owned();
    fs::write(
        &mixed_roster,
        encode_physical_study_jsonl(&mixed_rows).expect("mixed roster encodes"),
    )
    .expect("mixed roster writes");
    let mixed = run(&[
        "physical-study-plan",
        path_text(&mixed_roster),
        path_text(&mixed_manifest),
        path_text(&mixed_plan),
    ]);
    assert!(!mixed.status.success());
    assert!(String::from_utf8_lossy(&mixed.stderr).contains("mix gate or round"));
    assert!(!mixed_manifest.exists());
    assert!(!mixed_plan.exists());

    let occupied_roster = test_directory.join("physical-study-occupied-roster.jsonl");
    let uncreated_manifest = test_directory.join("physical-study-uncreated-manifest.json");
    let occupied_plan = test_directory.join("physical-study-occupied-plan.json");
    fs::write(
        &occupied_roster,
        encode_physical_study_jsonl(&mixed_rows[..1]).expect("occupied roster encodes"),
    )
    .expect("occupied roster writes");
    fs::write(&occupied_plan, b"existing plan").expect("plan sentinel writes");
    let occupied = run(&[
        "physical-study-plan",
        path_text(&occupied_roster),
        path_text(&uncreated_manifest),
        path_text(&occupied_plan),
    ]);
    assert!(!occupied.status.success());
    assert!(!uncreated_manifest.exists());
    assert_eq!(
        fs::read(&occupied_plan).expect("plan sentinel reads"),
        b"existing plan"
    );
}

#[test]
fn physical_study_seal_appends_exact_rows_and_preserves_failures() {
    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let row_path = test_directory.join("physical-study-row.json");
    let evidence_path = test_directory.join("physical-study-appended-evidence.jsonl");
    let mut first = physical_study_cli_row(
        PhysicalStudyGate::Gate0B,
        StudySplit::Heldout,
        PhysicalStudyCliIdentity::new("seal-person", "seal-mount", "seal-session"),
        Hand::Left,
        StudyCondition::IntentionalTap,
        Some("thumb"),
        0,
    );
    fs::write(
        &row_path,
        serde_json::to_vec_pretty(&first).expect("unsealed row serializes"),
    )
    .expect("unsealed row writes");
    let sealed = run(&[
        "physical-study-seal",
        path_text(&row_path),
        path_text(&evidence_path),
    ]);
    assert!(
        sealed.status.success(),
        "row seal failed: {}",
        String::from_utf8_lossy(&sealed.stderr)
    );
    seal_physical_study_evidence_row(&mut first).expect("expected first row seals");
    assert_eq!(
        fs::read(&evidence_path).expect("first evidence reads"),
        encode_physical_study_jsonl(std::slice::from_ref(&first))
            .expect("expected first JSONL encodes")
    );

    let mut second = physical_study_cli_row(
        PhysicalStudyGate::Gate0B,
        StudySplit::Heldout,
        PhysicalStudyCliIdentity::new("seal-person", "seal-mount", "seal-session"),
        Hand::Left,
        StudyCondition::IntentionalTap,
        Some("index"),
        1,
    );
    fs::write(
        &row_path,
        serde_json::to_vec(&second).expect("second unsealed row serializes"),
    )
    .expect("second row writes");
    let appended = run(&[
        "physical-study-seal",
        path_text(&row_path),
        path_text(&evidence_path),
    ]);
    assert!(appended.status.success());
    seal_physical_study_evidence_row(&mut second).expect("expected second row seals");
    assert_eq!(
        fs::read(&evidence_path).expect("appended evidence reads"),
        encode_physical_study_jsonl(&[first.clone(), second.clone()])
            .expect("expected appended JSONL encodes")
    );

    let evidence_before_rejection = fs::read(&evidence_path).expect("evidence snapshot reads");
    fs::write(
        &row_path,
        serde_json::to_vec(&second).expect("prefilled row serializes"),
    )
    .expect("prefilled row writes");
    let prefilled = run(&[
        "physical-study-seal",
        path_text(&row_path),
        path_text(&evidence_path),
    ]);
    assert!(!prefilled.status.success());
    assert!(String::from_utf8_lossy(&prefilled.stderr).contains("empty row_sha256"));
    assert_eq!(fs::read(&evidence_path).unwrap(), evidence_before_rejection);

    second.row_sha256.clear();
    fs::write(
        &row_path,
        serde_json::to_vec(&second).expect("unsealed retry row serializes"),
    )
    .expect("unsealed retry row writes");
    let broken_evidence = test_directory.join("physical-study-broken-evidence.jsonl");
    fs::write(&broken_evidence, b"visible-partial-row").expect("partial evidence writes");
    let missing_newline = run(&[
        "physical-study-seal",
        path_text(&row_path),
        path_text(&broken_evidence),
    ]);
    assert!(!missing_newline.status.success());
    assert!(String::from_utf8_lossy(&missing_newline.stderr).contains("does not end in a newline"));
    assert_eq!(
        fs::read(&broken_evidence).expect("partial evidence remains visible"),
        b"visible-partial-row"
    );
}

#[test]
fn physical_study_seal_serializes_concurrent_writers_at_the_total_byte_cap() {
    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let first_row_path = test_directory.join("physical-study-cap-first-row.json");
    let second_row_path = test_directory.join("physical-study-cap-second-row.json");
    let evidence_path = test_directory.join("physical-study-cap-evidence.jsonl");
    let first_identity = PhysicalStudyCliIdentity::new("cap-person", "cap-mount", "cap-session-a");
    let second_identity = PhysicalStudyCliIdentity::new("cap-person", "cap-mount", "cap-session-b");
    let first = physical_study_cli_row(
        PhysicalStudyGate::Gate0B,
        StudySplit::Heldout,
        first_identity,
        Hand::Left,
        StudyCondition::IntentionalTap,
        Some("thumb"),
        0,
    );
    let second = physical_study_cli_row(
        PhysicalStudyGate::Gate0B,
        StudySplit::Heldout,
        second_identity,
        Hand::Left,
        StudyCondition::IntentionalTap,
        Some("index"),
        0,
    );
    fs::write(
        &first_row_path,
        serde_json::to_vec(&first).expect("first cap row serializes"),
    )
    .expect("first cap row writes");
    fs::write(
        &second_row_path,
        serde_json::to_vec(&second).expect("second cap row serializes"),
    )
    .expect("second cap row writes");

    let sealed_bytes = [first, second].map(|mut row| {
        seal_physical_study_evidence_row(&mut row).expect("cap row seals");
        encode_physical_study_jsonl(std::slice::from_ref(&row)).expect("cap row encodes")
    });
    assert_eq!(sealed_bytes[0].len(), sealed_bytes[1].len());
    let row_length = sealed_bytes[0].len();
    let initial_length = PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES - row_length;
    let mut evidence = File::create(&evidence_path).expect("sparse evidence creates");
    evidence
        .set_len(u64::try_from(initial_length).expect("cap fits u64"))
        .expect("sparse evidence extends");
    evidence
        .seek(SeekFrom::End(-1))
        .expect("sparse evidence seeks to final byte");
    evidence
        .write_all(b"\n")
        .expect("sparse evidence ends in newline");
    evidence.sync_all().expect("sparse evidence syncs");

    // Hold the same OS lock while both real CLI processes start so both
    // writers contend on one pre-append state after the lock is released.
    evidence.lock().expect("test gate locks evidence");
    let first_child = command_binary(
        "INVISIBLE_KEYBOARD_RLOCATION",
        &[
            "physical-study-seal",
            path_text(&first_row_path),
            path_text(&evidence_path),
        ],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("first concurrent seal starts");
    let second_child = command_binary(
        "INVISIBLE_KEYBOARD_RLOCATION",
        &[
            "physical-study-seal",
            path_text(&second_row_path),
            path_text(&evidence_path),
        ],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("second concurrent seal starts");
    thread::sleep(Duration::from_millis(50));
    evidence.unlock().expect("test gate unlocks evidence");

    let first_output = first_child
        .wait_with_output()
        .expect("first concurrent seal exits");
    let second_output = second_child
        .wait_with_output()
        .expect("second concurrent seal exits");
    let outputs = [first_output, second_output];
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1,
        "exactly one row fits at the evidence cap"
    );
    let rejected = outputs
        .iter()
        .find(|output| !output.status.success())
        .expect("one concurrent append is rejected");
    let rejected_stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(rejected_stderr.contains(&format!(
        "evidence JSONL is {} bytes; maximum is {} bytes",
        PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES + row_length,
        PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
    )));

    assert_eq!(
        fs::metadata(&evidence_path)
            .expect("capped evidence metadata reads")
            .len(),
        u64::try_from(PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES).expect("cap fits u64")
    );
    let mut evidence = File::open(&evidence_path).expect("capped evidence opens");
    evidence
        .seek(SeekFrom::End(
            -i64::try_from(row_length).expect("row length fits i64"),
        ))
        .expect("capped evidence seeks to appended row");
    let mut appended = Vec::new();
    evidence
        .read_to_end(&mut appended)
        .expect("appended row reads");
    assert!(sealed_bytes.contains(&appended));
}

#[test]
fn physical_study_score_writes_a_valid_bound_report_and_never_overwrites_it() {
    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let evidence_path = test_directory.join("physical-study-valid-evidence.jsonl");
    let manifest_path = test_directory.join("physical-study-valid-manifest.json");
    let plan_path = test_directory.join("physical-study-valid-plan.json");
    let report_path = test_directory.join("physical-study-valid-report.json");
    let mut rows = gate0b_cli_study_rows();
    let (manifest, plan) = freeze_physical_study_rows(&mut rows);
    let evidence = encode_physical_study_jsonl(&rows).expect("valid study JSONL encodes");
    fs::write(&evidence_path, &evidence).expect("valid study evidence writes");
    fs::write(&manifest_path, manifest).expect("valid study manifest writes");
    fs::write(&plan_path, plan).expect("valid study plan writes");

    let scored = run(&[
        "physical-study-score",
        path_text(&evidence_path),
        path_text(&manifest_path),
        path_text(&plan_path),
        path_text(&report_path),
    ]);
    assert!(
        scored.status.success(),
        "valid physical study failed: {}",
        String::from_utf8_lossy(&scored.stderr)
    );
    let report_bytes = fs::read(&report_path).expect("physical-study report reads");
    let report: serde_json::Value =
        serde_json::from_slice(&report_bytes).expect("physical-study report parses");
    assert_eq!(report["schema"], PHYSICAL_STUDY_REPORT_SCHEMA);
    assert_eq!(report["disposition"], "go");
    assert_eq!(
        report["source"]["exact_jsonl_sha256"],
        format_sha256(&sha256(&evidence))
    );
    assert_eq!(report_bytes.last(), Some(&b'\n'));

    let report_snapshot = report_bytes;
    let occupied = run(&[
        "physical-study-score",
        path_text(&evidence_path),
        path_text(&manifest_path),
        path_text(&plan_path),
        path_text(&report_path),
    ]);
    assert!(!occupied.status.success());
    assert_eq!(
        fs::read(&report_path).expect("existing report remains readable"),
        report_snapshot
    );
}

#[cfg(target_os = "linux")]
#[test]
fn physical_study_score_waits_for_a_writer_and_never_publishes_its_stale_bytes() {
    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let evidence_path = test_directory.join("physical-study-locked-evidence.jsonl");
    let manifest_path = test_directory.join("physical-study-locked-manifest.json");
    let plan_path = test_directory.join("physical-study-locked-plan.json");
    let report_path = test_directory.join("physical-study-locked-report.json");
    let mut rows = gate0b_cli_study_rows();
    let (manifest, plan) = freeze_physical_study_rows(&mut rows);
    let evidence = encode_physical_study_jsonl(&rows).expect("valid study JSONL encodes");
    fs::write(&evidence_path, evidence).expect("valid study evidence writes");
    fs::write(&manifest_path, manifest).expect("valid study manifest writes");
    fs::write(&plan_path, plan).expect("valid study plan writes");

    let mut writer = File::options()
        .read(true)
        .write(true)
        .open(&evidence_path)
        .expect("evidence writer opens");
    writer
        .lock()
        .expect("evidence writer takes its exclusive lock");
    let mut scorer = ConcurrentChildOutput::start(
        command_binary(
            "INVISIBLE_KEYBOARD_RLOCATION",
            &[
                "physical-study-score",
                path_text(&evidence_path),
                path_text(&manifest_path),
                path_text(&plan_path),
                path_text(&report_path),
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("concurrent physical-study scorer starts"),
    );

    let scorer_id = scorer.child().id();
    let evidence_path = fs::canonicalize(&evidence_path).expect("evidence path canonicalizes");
    assert!(
        wait_until(Duration::from_secs(2), || {
            fs::read_dir(format!("/proc/{scorer_id}/fd")).is_ok_and(|descriptors| {
                descriptors.filter_map(Result::ok).any(|descriptor| {
                    fs::read_link(descriptor.path()).is_ok_and(|target| target == evidence_path)
                })
            })
        }),
        "scorer opens the evidence descriptor before waiting for its shared lock"
    );
    assert!(
        scorer.is_running(),
        "the scorer cannot pass an exclusive evidence writer"
    );
    assert!(
        !report_path.exists(),
        "a blocked scorer cannot publish a report"
    );

    // Change the exact bytes while still owning the writer lock. A scorer
    // that read before locking would now publish a report for stale bytes;
    // the cooperating scorer must instead read this tampered snapshot and
    // reject it after the writer releases the lock.
    rows[0].candidate_events[0].event_time_us += 1;
    let tampered = encode_physical_study_jsonl(&rows).expect("tampered study JSONL encodes");
    writer.set_len(0).expect("locked evidence truncates");
    writer.rewind().expect("locked evidence rewinds");
    writer
        .write_all(&tampered)
        .expect("locked evidence replacement writes");
    writer
        .sync_all()
        .expect("locked evidence replacement syncs");
    writer.unlock().expect("evidence writer releases its lock");

    let rejected = scorer.finish(
        Duration::from_secs(10),
        "physical-study scorer exits after the writer",
    );
    assert!(!rejected.status.success());
    assert!(
        !report_path.exists(),
        "tampered evidence produces no report"
    );
}

#[test]
fn physical_study_score_rejects_semantic_row_tamper_without_a_report() {
    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let evidence_path = test_directory.join("physical-study-tampered-evidence.jsonl");
    let manifest_path = test_directory.join("physical-study-tampered-manifest.json");
    let plan_path = test_directory.join("physical-study-tampered-plan.json");
    let report_path = test_directory.join("physical-study-tampered-report.json");
    let mut rows = vec![physical_study_cli_row(
        PhysicalStudyGate::Gate0B,
        StudySplit::Heldout,
        PhysicalStudyCliIdentity::new("tamper-person", "tamper-mount", "tamper-session"),
        Hand::Left,
        StudyCondition::IntentionalTap,
        Some("thumb"),
        0,
    )];
    let (manifest, plan) = freeze_physical_study_rows(&mut rows);
    rows[0].candidate_events[0].event_time_us += 1;
    fs::write(
        &evidence_path,
        encode_physical_study_jsonl(&rows).expect("tampered evidence encodes"),
    )
    .expect("tampered evidence writes");
    fs::write(&manifest_path, manifest).expect("tamper manifest writes");
    fs::write(&plan_path, plan).expect("tamper plan writes");

    let rejected = run(&[
        "physical-study-score",
        path_text(&evidence_path),
        path_text(&manifest_path),
        path_text(&plan_path),
        path_text(&report_path),
    ]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("evidence row hash mismatch"));
    assert!(!report_path.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn hardware_calibration_requires_two_controllers_keyboard_and_output_directory() {
    let output = run(&["calibrate-hardware", "only-one-path"]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains(
        "calibrate-hardware requires template, rig evidence, axis characterization, left device, right device, keyboard device, output directory, and optional port"
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn hardware_calibration_rejects_reused_device_paths_before_opening_them() {
    let output = run(&[
        "calibrate-hardware",
        "template.json",
        "rig-evidence.json",
        "axis-characterization.json",
        "same-device",
        "same-device",
        "keyboard-device",
        "output-directory",
    ]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains("left and right controller paths must differ"));
}

#[test]
fn layout_prints_all_34_positions_and_core28_count() {
    let output = run(&["layout"]);
    let stdout = String::from_utf8(output.stdout).expect("layout is UTF-8");

    assert!(output.status.success());
    assert!(stdout.contains("physical_positions=34"));
    assert!(stdout.contains("core28_enabled=28"));
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("slot "))
            .count(),
        34
    );
}

#[test]
fn simulation_output_cannot_be_mistaken_for_physical_proof() {
    let output = run(&["simulate"]);
    let stdout = String::from_utf8(output.stdout).expect("report is UTF-8");

    assert!(output.status.success());
    assert!(stdout.contains("evidence=simulated"));
    assert!(stdout.contains("does not establish"));
    assert!(stdout.contains("scenario=packet-loss"));
    assert!(stdout.contains("breakpoint dimension=contact-tail-loss"));
    assert!(stdout.contains("breakpoint dimension=unmodeled-finger-impulse"));
    assert!(stdout.contains("breakpoint dimension=differential-bias-ramp"));
    assert!(stdout.contains("whole_hand_dropout windows=2 abstentions=2"));
    assert!(!stdout.contains("hardware pass"));
}

#[test]
fn unknown_command_fails_with_a_direct_error() {
    let output = run(&["unknown"]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains("unknown command 'unknown'"));
}

#[test]
fn missing_input_is_reported_as_file_io_not_output_failure() {
    let output = run(&[
        "fit-session-clocks",
        "/definitely-missing/invisible-keyboard-manifest.json",
        "/definitely-missing/invisible-keyboard-rig-evidence.json",
        "/definitely-missing/invisible-keyboard-axis-characterization.json",
        "/definitely-missing/invisible-keyboard-session.iks",
        "/definitely-missing/invisible-keyboard-clocks.json",
    ]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains("file I/O failed"));
    assert!(!stderr.contains("output failed"));
}

#[test]
fn operand_free_commands_reject_unexpected_arguments() {
    for command in ["help", "layout", "simulate", "frame-fixture"] {
        let output = run(&[command, "unexpected"]);
        let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

        assert!(!output.status.success(), "{command} must reject an operand");
        assert!(
            stderr.contains(&format!("{command} does not accept arguments")),
            "unexpected error for {command}: {stderr}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn keyboard_discovery_is_a_runnable_bounded_diagnostic() {
    let output = run(&["list-keyboards"]);
    let stdout = String::from_utf8(output.stdout).expect("output is UTF-8");

    assert!(output.status.success());
    assert!(
        stdout
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("keyboards="))
    );
}

#[test]
fn calibration_demo_rejects_invalid_ports_before_binding() {
    for operands in [
        ["calibration-demo", "invalid"],
        ["calibration-demo", "70000"],
    ] {
        let output = run(&operands);
        let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

        assert!(!output.status.success());
        assert!(stderr.contains("calibration-demo port must be an integer from 0 to 65535"));
    }
}

#[test]
fn calibrated_replay_requires_every_identity_sidecar() {
    let output = run(&["replay-calibrated", "only-one-path"]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains(
        "replay-calibrated requires calibration manifest, rig evidence, axis characterization, runtime manifest, clock epochs, artifact, calibration output directory, and session paths"
    ));
}

#[test]
fn calibrated_replay_rejects_a_runtime_manifest_from_another_physical_rig() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let calibration_manifest = write_manifest(&test_directory, "shared-rig-calibration", 208);
    let other_rig_manifest = write_manifest(&test_directory, "other-rig-runtime", 208);
    let mut other_rig = read_bound_manifest(&other_rig_manifest);
    other_rig.hands[0].sensors[0].mount_id = "other-rig-left-thumb-mount".to_owned();
    write_bound_manifest(&other_rig_manifest, other_rig);
    let evidence = manifest_evidence_files(&calibration_manifest);

    let rejected = run(&[
        "replay-calibrated",
        path_text(&calibration_manifest),
        path_text(&evidence.rig),
        path_text(&evidence.axis),
        path_text(&other_rig_manifest),
        "unread-clock-epochs.json",
        "unread-artifact.json",
        "unread-calibration-output",
        "unread-session.iks",
    ]);
    let stderr = String::from_utf8(rejected.stderr).expect("error is UTF-8");

    assert!(!rejected.status.success());
    assert!(
        stderr
            .contains("manifest rig_evidence_sha256 does not bind the passed rig evidence record")
    );
    assert!(!stderr.contains("file I/O failed"));
}

#[test]
fn evidence_bound_calibration_replay_requires_both_disjoint_bundles() {
    let output = run(&["calibrate-replay", "only-one-path"]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains(
        "calibrate-replay requires manifest, rig evidence, axis characterization, fitting clocks/capture/surface-plan/session, neutral report, held-out clocks/capture/surface-plan/session, and an output directory"
    ));
}

#[test]
fn calibrated_live_run_requires_artifact_and_calibration_output_directory() {
    let output = run(&["run-calibrated", "only-one-path"]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains(
        "run-calibrated requires calibration manifest, rig evidence, axis characterization, runtime template, artifact, calibration output directory, left device, right device, runtime manifest output, session output, live clock report output, optional output mode, and --confirm-wearables-unmoved"
    ));
}

#[test]
fn calibrated_live_run_requires_fresh_wear_confirmation_before_reading_inputs() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let outputs = LiveCliOutputs::new(&test_directory, "missing-wear-confirmation");
    let output = run(&[
        "run-calibrated",
        "missing-calibration.json",
        "missing-rig-evidence.json",
        "missing-axis-characterization.json",
        "missing-template.json",
        "missing-artifact.json",
        "missing-calibration-output",
        "missing-left-controller",
        "missing-right-controller",
        outputs
            .runtime_manifest
            .to_str()
            .expect("runtime manifest output path is UTF-8"),
        outputs
            .session
            .to_str()
            .expect("session output path is UTF-8"),
        outputs
            .clock_report
            .to_str()
            .expect("clock report output path is UTF-8"),
        "recording",
        "wrong-confirmation",
    ]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains("requires a fresh --confirm-wearables-unmoved attestation"));
    assert!(!stderr.contains("file I/O failed"));
    assert_no_live_outputs(&outputs);
}

#[test]
fn calibrated_live_run_rejects_reused_or_existing_outputs_before_device_open() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let existing = PathBuf::from(&test_directory)
        .join(format!("existing-live-output-{}.json", std::process::id()));
    fs::write(&existing, b"existing evidence").expect("existing output writes");
    let existing = existing.to_str().expect("existing output path is UTF-8");

    let reused = run(&[
        "run-calibrated",
        "missing-calibration.json",
        "missing-rig-evidence.json",
        "missing-axis-characterization.json",
        "missing-template.json",
        "missing-artifact.json",
        "missing-calibration-output",
        "left-device",
        "right-device",
        "same-output",
        "same-output",
        "clock-output",
        "--confirm-wearables-unmoved",
    ]);
    assert!(!reused.status.success());
    assert!(
        String::from_utf8(reused.stderr)
            .expect("error is UTF-8")
            .contains(
                "runtime manifest, session, and live clock report output paths must be distinct"
            )
    );

    let occupied = run(&[
        "run-calibrated",
        "missing-calibration.json",
        "missing-rig-evidence.json",
        "missing-axis-characterization.json",
        "missing-template.json",
        "missing-artifact.json",
        "missing-calibration-output",
        "left-device",
        "right-device",
        existing,
        "new-session-output",
        "new-clock-output",
        "--confirm-wearables-unmoved",
    ]);
    assert!(!occupied.status.success());
    let stderr = String::from_utf8(occupied.stderr).expect("error is UTF-8");
    assert!(stderr.contains("output already exists"));
    assert_eq!(
        fs::read(existing).expect("existing output reads"),
        b"existing evidence"
    );
}

#[test]
fn calibrated_replay_runs_only_after_recomputing_the_canonical_heldout_bundle() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "replay-admission");
    let artifact =
        CalibrationArtifact::from_json(&fs::read(&fixture.artifact).expect("artifact reads"))
            .expect("artifact parses");
    let validation = ValidationReport::from_json(
        &fs::read(fixture.calibration_output.join("validation-report.json"))
            .expect("validation report reads"),
    )
    .expect("validation report parses");
    let mut runtime_manifest = read_bound_manifest(&fixture.calibration_manifest);
    runtime_manifest.calibration_artifact_sha256 = Some(format_sha256(
        &artifact.fingerprint().expect("artifact hashes"),
    ));
    runtime_manifest.calibration_validation_sha256 = Some(format_sha256(
        &validation.fingerprint().expect("validation hashes"),
    ));
    let runtime_manifest_path = fixture
        .calibration_manifest
        .with_extension("admitted-runtime.json");
    let runtime_manifest_source = runtime_manifest
        .canonical_json()
        .expect("runtime manifest serializes");
    AcquisitionManifest::from_json_with_evidence(
        &runtime_manifest_source,
        &fs::read(&fixture.rig_evidence).expect("rig evidence reads"),
        &fs::read(&fixture.axis_characterization).expect("axis characterization reads"),
    )
    .expect("runtime manifest binds physical evidence");
    fs::write(&runtime_manifest_path, runtime_manifest_source).expect("runtime manifest writes");

    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us: runtime_manifest.nominal_period_us,
            manifest_sha256: runtime_manifest.sha256().expect("runtime manifest hashes"),
            session_id: [0x95; 16],
        },
    )
    .expect("runtime session starts");
    for hand in [Hand::Left, Hand::Right] {
        let host_offset_ns = match hand {
            Hand::Left => 1_000_000_000,
            Hand::Right => 2_000_000_000,
        };
        for sequence in 0..32 {
            writer
                .append_frame(
                    ControllerStream::from(hand),
                    &sensor_frame(hand, sequence),
                    host_offset_ns + u64::from(sequence) * 5_000_000,
                )
                .expect("runtime frame writes");
        }
    }
    let runtime_session = writer
        .finish_finalized()
        .expect("runtime session finalizes");
    let runtime_clocks = ClockEpochSet::fit_session(&runtime_manifest, &runtime_session)
        .expect("runtime clocks fit");
    let runtime_session_path = fixture.calibration_manifest.with_extension("runtime.iks");
    let runtime_clocks_path = fixture
        .calibration_manifest
        .with_extension("runtime-clocks.json");
    fs::write(&runtime_session_path, runtime_session.bytes()).expect("runtime session writes");
    fs::write(
        &runtime_clocks_path,
        runtime_clocks
            .canonical_json()
            .expect("runtime clocks serialize"),
    )
    .expect("runtime clocks write");
    fs::write(
        fixture.calibration_output.join("heldout-evidence.json"),
        b"corrupt diagnostic evidence is ignored",
    )
    .expect("diagnostic evidence replacement writes");

    let replayed = run(&[
        "replay-calibrated",
        fixture
            .calibration_manifest
            .to_str()
            .expect("calibration manifest path is UTF-8"),
        fixture
            .rig_evidence
            .to_str()
            .expect("rig evidence path is UTF-8"),
        fixture
            .axis_characterization
            .to_str()
            .expect("axis characterization path is UTF-8"),
        runtime_manifest_path
            .to_str()
            .expect("runtime manifest path is UTF-8"),
        runtime_clocks_path
            .to_str()
            .expect("runtime clocks path is UTF-8"),
        fixture.artifact.to_str().expect("artifact path is UTF-8"),
        fixture
            .calibration_output
            .to_str()
            .expect("calibration output path is UTF-8"),
        runtime_session_path
            .to_str()
            .expect("runtime session path is UTF-8"),
    ]);
    assert!(
        replayed.status.success(),
        "valid held-out validation failed: {}",
        String::from_utf8_lossy(&replayed.stderr)
    );
    let stdout = String::from_utf8(replayed.stdout).expect("replay output is UTF-8");
    assert!(stdout.contains("evidence=replay-tested"));
    assert!(stdout.contains("final_key_state_neutral=true"));
}

#[test]
fn runtime_ignores_missing_corrupt_and_replaced_diagnostic_evidence() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "diagnostic-evidence");
    let evidence_path = fixture.calibration_output.join("heldout-evidence.json");
    let baseline_outputs = LiveCliOutputs::new(&test_directory, "diagnostic-baseline");
    let baseline = run_live_without_devices(&fixture, &baseline_outputs);
    assert!(!baseline.status.success());
    let baseline_stderr = baseline.stderr;
    assert_no_live_outputs(&baseline_outputs);

    fs::remove_file(&evidence_path).expect("diagnostic evidence removes");
    let missing_outputs = LiveCliOutputs::new(&test_directory, "diagnostic-missing");
    let missing = run_live_without_devices(&fixture, &missing_outputs);
    assert_eq!(missing.stderr, baseline_stderr);
    assert_no_live_outputs(&missing_outputs);

    fs::write(&evidence_path, b"not JSON").expect("corrupt diagnostic evidence writes");
    let corrupt_outputs = LiveCliOutputs::new(&test_directory, "diagnostic-corrupt");
    let corrupt = run_live_without_devices(&fixture, &corrupt_outputs);
    assert_eq!(corrupt.stderr, baseline_stderr);
    assert_no_live_outputs(&corrupt_outputs);

    fs::write(
        &evidence_path,
        br#"{"schema":"diagnostic-from-an-unrelated-run"}"#,
    )
    .expect("replacement diagnostic evidence writes");
    let replaced_outputs = LiveCliOutputs::new(&test_directory, "diagnostic-replaced");
    let replaced = run_live_without_devices(&fixture, &replaced_outputs);
    assert_eq!(replaced.stderr, baseline_stderr);
    assert_no_live_outputs(&replaced_outputs);
}

#[test]
fn runtime_rejects_invalid_capture_and_surface_plan_before_device_open() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");

    for (label, mutation) in [
        ("capture-missing", 0_u8),
        ("capture-corrupt", 1),
        ("capture-truncated", 2),
        ("capture-oversized", 3),
    ] {
        let fixture = LiveCliFixture::new(&test_directory, label);
        let capture_path = fixture.calibration_output.join("heldout-capture.json");
        match mutation {
            0 => fs::remove_file(&capture_path).expect("held-out capture removes"),
            1 => fs::write(&capture_path, b"not JSON").expect("corrupt capture writes"),
            2 => {
                let capture = fs::read(&capture_path).expect("held-out capture reads");
                fs::write(&capture_path, &capture[..capture.len() / 2])
                    .expect("truncated capture writes");
            }
            3 => fs::write(&capture_path, vec![b' '; MAX_CALIBRATION_CAPTURE_BYTES + 1])
                .expect("oversized capture writes"),
            _ => unreachable!(),
        }
        let outputs = LiveCliOutputs::new(&test_directory, label);
        let rejected = run_live_without_devices(&fixture, &outputs);
        assert!(!rejected.status.success(), "{label} must fail admission");
        assert_no_live_outputs(&outputs);
    }

    let swapped = LiveCliFixture::new(&test_directory, "capture-swapped");
    let manifest = read_bound_manifest(&swapped.calibration_manifest);
    let artifact =
        CalibrationArtifact::from_json(&fs::read(&swapped.artifact).expect("artifact reads"))
            .expect("artifact parses");
    let other_run = build_runtime_calibration_bundle_for_run(
        &manifest,
        &artifact,
        [0x99; 16],
        HELDOUT_RUN_SEED + 1,
        false,
    );
    fs::write(
        swapped.calibration_output.join("heldout-capture.json"),
        other_run.capture,
    )
    .expect("swapped capture writes");
    let swapped_outputs = LiveCliOutputs::new(&test_directory, "capture-swapped");
    let rejected = run_live_without_devices(&swapped, &swapped_outputs);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8(rejected.stderr)
            .expect("swapped-capture error is UTF-8")
            .contains("surface plan")
    );
    assert_no_live_outputs(&swapped_outputs);

    for (label, replacement) in [
        ("plan-missing", None),
        ("plan-corrupt", Some(b"not JSON".to_vec())),
        (
            "plan-cross-artifact",
            Some(
                HeldOutSurfacePlan::v1([0xff; 32], HELDOUT_RUN_SEED)
                    .canonical_json()
                    .expect("cross-artifact plan serializes"),
            ),
        ),
    ] {
        let fixture = LiveCliFixture::new(&test_directory, label);
        let plan_path = fixture.calibration_output.join("heldout-surface-plan.json");
        if let Some(bytes) = replacement {
            fs::write(&plan_path, bytes).expect("replacement surface plan writes");
        } else {
            fs::remove_file(&plan_path).expect("surface plan removes");
        }
        let outputs = LiveCliOutputs::new(&test_directory, label);
        let rejected = run_live_without_devices(&fixture, &outputs);
        assert!(!rejected.status.success(), "{label} must fail admission");
        assert_no_live_outputs(&outputs);
    }
}

#[test]
fn runtime_recomputes_and_rejects_a_real_failed_heldout_capture() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "admission-failed-capture");
    let manifest = read_bound_manifest(&fixture.calibration_manifest);
    let artifact =
        CalibrationArtifact::from_json(&fs::read(&fixture.artifact).expect("artifact reads"))
            .expect("artifact parses");
    let failed = build_runtime_calibration_bundle_for_run(
        &manifest,
        &artifact,
        HELDOUT_SESSION_ID,
        HELDOUT_RUN_SEED,
        true,
    );
    assert!(!failed.validation_passed);
    for (name, bytes) in [
        ("heldout-clocks.json", failed.clocks.as_slice()),
        ("heldout-capture.json", failed.capture.as_slice()),
        ("heldout-surface-plan.json", failed.surface_plan.as_slice()),
        ("heldout-evidence.json", failed.evidence.as_slice()),
        ("heldout-session.iks", failed.session.as_slice()),
        ("validation-report.json", failed.report.as_slice()),
    ] {
        fs::write(fixture.calibration_output.join(name), bytes)
            .expect("failed held-out bundle writes");
    }
    let outputs = LiveCliOutputs::new(&test_directory, "admission-failed-capture");
    let rejected = run_live_without_devices(&fixture, &outputs);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8(rejected.stderr)
            .expect("failed-validation error is UTF-8")
            .contains("held-out validation failed")
    );
    assert_no_live_outputs(&outputs);
}

#[test]
fn runtime_validation_rejects_forged_and_non_neutral_reports_in_live_and_replay() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    for (label, make_non_neutral) in [("forged", false), ("non-neutral", true)] {
        let fixture = LiveCliFixture::new(&test_directory, label);
        let report_path = fixture.calibration_output.join("validation-report.json");
        let mut report =
            ValidationReport::from_json(&fs::read(&report_path).expect("validation report reads"))
                .expect("validation report parses");
        if make_non_neutral {
            report.key_state_neutral = false;
        } else {
            // This remains a passing, structurally valid report. A report-only
            // admission path would accept it even though raw evidence does not
            // derive the claimed exposure.
            report.ordinary_motion_exposure_ms += 1;
        }
        fs::write(
            &report_path,
            report.canonical_json().expect("forged report serializes"),
        )
        .expect("forged report writes");

        let outputs = LiveCliOutputs::new(&test_directory, label);
        let live = run_live_without_devices(&fixture, &outputs);
        assert!(!live.status.success());
        assert!(
            String::from_utf8(live.stderr)
                .expect("live admission error is UTF-8")
                .contains(
                    "validation report does not equal the report recomputed from held-out capture"
                )
        );
        assert_no_live_outputs(&outputs);

        let replay = run(&[
            "replay-calibrated",
            fixture
                .calibration_manifest
                .to_str()
                .expect("calibration manifest path is UTF-8"),
            fixture
                .rig_evidence
                .to_str()
                .expect("rig evidence path is UTF-8"),
            fixture
                .axis_characterization
                .to_str()
                .expect("axis characterization path is UTF-8"),
            fixture
                .calibration_manifest
                .to_str()
                .expect("runtime manifest path is UTF-8"),
            fixture
                .calibration_output
                .join("heldout-clocks.json")
                .to_str()
                .expect("held-out clocks path is UTF-8"),
            fixture.artifact.to_str().expect("artifact path is UTF-8"),
            fixture
                .calibration_output
                .to_str()
                .expect("calibration output path is UTF-8"),
            fixture
                .calibration_output
                .join("heldout-session.iks")
                .to_str()
                .expect("held-out session path is UTF-8"),
        ]);
        assert!(!replay.status.success());
        assert!(
            String::from_utf8(replay.stderr)
                .expect("replay admission error is UTF-8")
                .contains(
                    "validation report does not equal the report recomputed from held-out capture"
                )
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn calibrated_live_run_locks_current_and_reset_boots_into_artifact_bound_outputs() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-current-and-reset");
    // Recorder flushes prove consumption, not clock readiness. Cover the full
    // quality warmup before requiring a successful run to have armed.
    let warmup_pairs =
        u32::try_from(MAX_ONLINE_CLOCK_QUALITY_WARMUP_US / RUNTIME_PAIR_PERIOD_US + 2)
            .expect("clock warmup fixture count fits u32");

    for (label, boots, mode) in [
        ("current", [0x11, 0x22], None),
        ("reset", [0x31, 0x32], Some("recording")),
    ] {
        let outputs = LiveCliOutputs::new(&test_directory, label);
        let (mut child, mut left, mut right) = spawn_live_cli_with_mode(&fixture, &outputs, mode);
        let stream_start = Instant::now();
        write_live_runtime_pairs(
            LiveRuntimeWriters {
                child: &mut child,
                left: &mut left,
                right: &mut right,
            },
            boots,
            [0, 0],
            warmup_pairs,
            stream_start,
            label,
        );
        // One persisted BufWriter batch proves that a substantial prefix has
        // crossed identity lock and entered the raw session. A later batch
        // cannot be appended until the live runtime consumes that prefix.
        let (mut next_sequences, prefix_len) = write_live_pairs_until_session_grows(
            LiveRuntimeWriters {
                child: &mut child,
                left: &mut left,
                right: &mut right,
            },
            boots,
            [warmup_pairs, warmup_pairs],
            stream_start,
            &outputs.session,
            u64::try_from(HEADER_LEN).expect("header length fits u64"),
            label,
        );
        write_live_runtime_pairs(
            LiveRuntimeWriters {
                child: &mut child,
                left: &mut left,
                right: &mut right,
            },
            boots,
            next_sequences,
            24,
            stream_start,
            label,
        );
        next_sequences[0] = next_sequences[0].wrapping_add(24);
        next_sequences[1] = next_sequences[1].wrapping_add(24);
        let (final_sequences, _) = write_live_pairs_until_session_grows(
            LiveRuntimeWriters {
                child: &mut child,
                left: &mut left,
                right: &mut right,
            },
            boots,
            next_sequences,
            stream_start,
            &outputs.session,
            prefix_len,
            label,
        );
        let live_child = ConcurrentChildOutput::start(child);
        let signal_started =
            send_planned_signal_and_timestamp(live_child.child(), PlannedProcessSignal::Interrupt);
        let neutralization_observation = live_child.wait_for_line_since(
            signal_started,
            Duration::from_secs(10),
            "shutdown_neutralization=confirmed",
            label,
        );
        let completed = live_child.finish(Duration::from_secs(10), label);
        assert!(
            neutralization_observation.is_ok(),
            "{}\nstdout:\n{}\nstderr:\n{}\n{}",
            neutralization_observation.expect_err("failed observation has a diagnostic"),
            String::from_utf8_lossy(&completed.stdout),
            String::from_utf8_lossy(&completed.stderr),
            live_session_diagnostics(&outputs.session),
        );
        assert!(
            completed.status.success(),
            "{label} boot admission failed: {}\n{}",
            String::from_utf8_lossy(&completed.stderr),
            live_session_diagnostics(&outputs.session),
        );
        let stdout = String::from_utf8(completed.stdout).expect("live output is UTF-8");
        assert!(stdout.contains("controller_identity=locked"));
        assert!(stdout.contains("wear_session=user-attested-unmoved"));
        assert!(stdout.contains(
            "User-attested wear session active. Stop typing before moving or removing either wearable."
        ));
        assert!(stdout.contains("wear_session_confirmation=revoked"));
        assert_planned_shutdown_receipt(&stdout, PlannedProcessSignal::Interrupt);
        assert!(stdout.contains(
            "Typing stopped and output is neutral. This wear-session confirmation is no longer active. Confirm again before starting a new live process. Revalidate first if either wearable moved."
        ));
        assert!(stdout.contains("clock_warmup_frames="));
        assert!(!stdout.contains("clock_warmup_frames=0"));
        assert!(
            stdout.contains("armed_transitions=1"),
            "{label} did not arm exactly once:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&completed.stderr),
        );
        let session = assert_live_bundle(&fixture, &outputs, boots);
        assert_atomic_cutoff_prefix(
            session.summary().left,
            final_sequences[0],
            &format!("{label} left shutdown stream"),
        );
        assert_atomic_cutoff_prefix(
            session.summary().right,
            final_sequences[1],
            &format!("{label} right shutdown stream"),
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn calibrated_live_run_retains_bootstrap_bytes_once_and_creates_nothing_before_lock() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-retained-bootstrap");
    let outputs = LiveCliOutputs::new(&test_directory, "retained-bootstrap");
    let boots = [0x41, 0x42];
    let (mut child, mut left, mut right) = spawn_live_cli(&fixture, &outputs);
    let stream_start = Instant::now();

    write_live_runtime_frame(
        &mut child,
        &mut left,
        Hand::Left,
        boots[0],
        0,
        0,
        "retained bootstrap",
    );
    // Keep the pre-lock asymmetry below the live source-silence window even
    // when the test runner is heavily loaded. The contract needs two retained
    // left frames before the right identity, not a scheduler-timed delay.
    let second_left_device_time_us = u64::try_from(stream_start.elapsed().as_micros())
        .unwrap_or(u64::MAX)
        .max(1);
    write_live_runtime_frame(
        &mut child,
        &mut left,
        Hand::Left,
        boots[0],
        1,
        second_left_device_time_us,
        "retained bootstrap",
    );

    assert!(!outputs.runtime_manifest.exists());
    assert!(!outputs.session.exists());
    assert!(!outputs.clock_report.exists());

    write_live_runtime_frame(
        &mut child,
        &mut right,
        Hand::Right,
        boots[1],
        0,
        u64::try_from(stream_start.elapsed().as_micros()).unwrap_or(u64::MAX),
        "retained bootstrap",
    );
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [2, 1],
        24,
        stream_start,
        "retained bootstrap",
    );
    let (mut next_sequences, prefix_len) = write_live_pairs_until_session_grows(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [26, 25],
        stream_start,
        &outputs.session,
        u64::try_from(HEADER_LEN).expect("header length fits u64"),
        "retained bootstrap prefix",
    );
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        next_sequences,
        24,
        stream_start,
        "retained bootstrap",
    );
    next_sequences[0] = next_sequences[0].wrapping_add(24);
    next_sequences[1] = next_sequences[1].wrapping_add(24);
    let (final_sequences, _) = write_live_pairs_until_session_grows(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        next_sequences,
        stream_start,
        &outputs.session,
        prefix_len,
        "retained bootstrap tail",
    );
    send_planned_signal(&child, PlannedProcessSignal::Interrupt);
    wait_for_child(
        &mut child,
        Duration::from_secs(2),
        "retained bootstrap live process",
    );
    let completed = child.wait_with_output().expect("live process exits");
    assert!(
        completed.status.success(),
        "retained bootstrap admission failed: {}\n{}",
        String::from_utf8_lossy(&completed.stderr),
        live_session_diagnostics(&outputs.session),
    );
    let session = assert_live_bundle(&fixture, &outputs, boots);
    assert_exact_hand_stream(
        session.summary().left,
        final_sequences[0],
        "retained bootstrap left",
    );
    assert_exact_hand_stream(
        session.summary().right,
        final_sequences[1],
        "retained bootstrap right",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn calibrated_live_run_rejects_swapped_and_wrong_controller_identity_without_outputs() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-identity-failure");

    for label in ["swapped", "wrong-controller"] {
        let outputs = LiveCliOutputs::new(&test_directory, label);
        let (mut child, mut left, _right) = spawn_live_cli(&fixture, &outputs);
        if label == "swapped" {
            left.write_all(&runtime_frame(Hand::Right, 0x52, 0, 0))
                .expect("swapped right frame writes to left source");
        } else {
            let mut wrong = sensor_frame(Hand::Left, 0);
            wrong.boot_id = [0x51; 16];
            wrong.controller_id_sha256 = [0x77; 32];
            left.write_all(&encoded_transport_frame(&wrong))
                .expect("wrong controller frame writes");
        }
        left.flush().expect("left FIFO flushes");
        wait_for_child(&mut child, Duration::from_secs(2), label);
        let completed = child.wait_with_output().expect("rejected process exits");
        assert!(!completed.status.success(), "{label} identity must fail");
        let stderr = String::from_utf8(completed.stderr).expect("error is UTF-8");
        if label == "swapped" {
            assert!(stderr.contains("supplied a frame claiming"));
        } else {
            assert!(stderr.contains("controller does not match the template"));
        }
        assert!(!outputs.runtime_manifest.exists());
        assert!(!outputs.session.exists());
        assert!(!outputs.clock_report.exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn calibrated_live_run_times_out_a_missing_identity_without_outputs() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-identity-timeout");
    let outputs = LiveCliOutputs::new(&test_directory, "identity-timeout");
    let (mut child, mut left, _right) = spawn_live_cli(&fixture, &outputs);
    left.write_all(&runtime_frame(Hand::Left, 0x61, 0, 0))
        .expect("left identity frame writes");
    left.flush().expect("left FIFO flushes");

    wait_for_child(
        &mut child,
        Duration::from_secs(3),
        "missing identity bootstrap",
    );
    let completed = child.wait_with_output().expect("bootstrap timeout exits");
    assert!(!completed.status.success());
    assert!(
        String::from_utf8(completed.stderr)
            .expect("error is UTF-8")
            .contains("identity bootstrap timed out")
    );
    assert!(!outputs.runtime_manifest.exists());
    assert!(!outputs.session.exists());
    assert!(!outputs.clock_report.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn planned_signals_finalize_calibrated_live_while_both_fifos_are_busy() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    for (signal, boots) in [
        (PlannedProcessSignal::Interrupt, [0x71, 0x72]),
        (PlannedProcessSignal::Terminate, [0x75, 0x76]),
    ] {
        let context = format!("busy FIFO {} shutdown", signal.fixture_label());
        let fixture = LiveCliFixture::new(
            &test_directory,
            &format!("live-bounded-stop-{}", signal.fixture_label()),
        );
        let outputs = LiveCliOutputs::new(
            &test_directory,
            &format!("bounded-stop-{}", signal.fixture_label()),
        );
        let (child, left, right) = spawn_live_cli(&fixture, &outputs);
        let keep_writing = Arc::new(AtomicBool::new(true));
        let stream_start = Instant::now();
        // The controllers are independent physical producers. Keep their test
        // writers independent too, so backpressure on one FIFO cannot fabricate a
        // silence interval on the other source.
        let left_writer = spawn_continuous_runtime_writer(
            left,
            Hand::Left,
            boots[0],
            Arc::clone(&keep_writing),
            stream_start,
        );
        let right_writer = spawn_continuous_runtime_writer(
            right,
            Hand::Right,
            boots[1],
            Arc::clone(&keep_writing),
            stream_start,
        );
        assert!(
            wait_until(Duration::from_secs(1), || outputs.session.exists()),
            "{context} reaches identity lock under continuous traffic"
        );
        thread::sleep(Duration::from_millis(250));
        let live_child = ConcurrentChildOutput::start(child);
        let signal_started = send_planned_signal_and_timestamp(live_child.child(), signal);
        let neutralization_observation = live_child.wait_for_line_since(
            signal_started,
            Duration::from_secs(10),
            "shutdown_neutralization=confirmed",
            &context,
        );
        keep_writing.store(false, Ordering::SeqCst);
        left_writer.join().expect("left FIFO writer joins");
        right_writer.join().expect("right FIFO writer joins");
        let completed = live_child.finish(Duration::from_secs(10), &context);
        assert!(
            neutralization_observation.is_ok(),
            "{}\nstdout:\n{}\nstderr:\n{}\n{}",
            neutralization_observation.expect_err("failed observation has a diagnostic"),
            String::from_utf8_lossy(&completed.stdout),
            String::from_utf8_lossy(&completed.stderr),
            live_session_diagnostics(&outputs.session),
        );
        assert!(
            completed.status.success(),
            "{context} failed: {}",
            String::from_utf8_lossy(&completed.stderr)
        );
        let stdout = String::from_utf8(completed.stdout).expect("live output is UTF-8");
        assert_planned_shutdown_receipt(&stdout, signal);
        assert_live_bundle(&fixture, &outputs, boots);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_status_disconnect_cannot_erase_a_durable_live_bundle() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-closed-status");
    let outputs = LiveCliOutputs::new(&test_directory, "closed-status");
    let boots = [0x73, 0x74];
    let (mut child, mut left, mut right) = spawn_live_cli(&fixture, &outputs);
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [0, 0],
        25,
        Instant::now(),
        "closed terminal status",
    );

    let mut startup = BufReader::new(
        child
            .stdout
            .take()
            .expect("live process startup stdout is piped"),
    );
    let mut startup_text = String::new();
    startup
        .read_line(&mut startup_text)
        .expect("wear-session status reads");
    startup
        .read_line(&mut startup_text)
        .expect("wear-session warning reads");
    assert!(startup_text.contains("wear_session=user-attested-unmoved"));
    drop(startup);

    assert!(
        wait_until(Duration::from_secs(2), || outputs.session.exists()),
        "live session starts after readiness publication"
    );
    send_planned_signal_and_timestamp(&child, PlannedProcessSignal::Interrupt);
    drop(left);
    drop(right);
    wait_for_child(
        &mut child,
        Duration::from_secs(10),
        "closed terminal status",
    );
    let completed = child
        .wait_with_output()
        .expect("closed-status live process exits");
    assert!(!completed.status.success());
    let stderr = String::from_utf8(completed.stderr).expect("status error is UTF-8");
    assert!(
        stderr.contains(
            "the live evidence bundle is durable, but terminal status publication failed"
        )
    );
    assert_live_bundle(&fixture, &outputs, boots);
}

#[cfg(target_os = "linux")]
#[test]
fn live_transport_fault_remains_primary_when_secondary_finalization_fails() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-double-failure");
    let mut outputs = LiveCliOutputs::new(&test_directory, "double-failure");
    let clock_directory = PathBuf::from(&test_directory)
        .join(format!("removed-clock-directory-{}", std::process::id()));
    fs::create_dir(&clock_directory).expect("clock report directory creates");
    outputs.clock_report = clock_directory.join("live-clock-report.json");

    let boots = [0x81, 0x82];
    let (mut child, mut left, mut right) = spawn_live_cli(&fixture, &outputs);
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [0, 0],
        4,
        Instant::now(),
        "double failure",
    );
    assert!(
        wait_until(Duration::from_secs(2), || outputs.session.exists()),
        "live session reaches identity lock"
    );
    fs::remove_dir(&clock_directory).expect("empty clock report directory removes");
    drop(left);
    drop(right);

    // The recording adapter cannot fail neutralization. Driving that exact secondary
    // failure needs a real uinput device that opens successfully and then rejects release.
    wait_for_child(&mut child, Duration::from_secs(2), "double failure");
    let completed = child.wait_with_output().expect("failed live process exits");
    assert!(!completed.status.success());
    let stderr = String::from_utf8(completed.stderr).expect("error is UTF-8");
    assert!(stderr.contains("device transport fault: SourceDisconnected"));
    assert!(stderr.contains("live evidence finalization also failed: file I/O failed"));
    assert!(outputs.runtime_manifest.exists());
    assert!(outputs.session.exists());
    assert!(!outputs.clock_report.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn source_fault_remains_primary_when_sigint_arrives_after_the_session_footer() {
    const FRAME_COUNT: u32 = 100;

    fn send_signal(child: &Child, signal: &str) -> bool {
        Command::new("kill")
            .args([signal, &child.id().to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }

    fn process_is_stopped(child: &Child) -> bool {
        fs::read_to_string(format!("/proc/{}/status", child.id())).is_ok_and(|status| {
            status
                .lines()
                .any(|line| line.starts_with("State:") && line.contains("T (stopped)"))
        })
    }

    fn session_footer_is_visible(path: &Path) -> bool {
        let Ok(mut session) = File::open(path) else {
            return false;
        };
        let Ok(length) = session.metadata().map(|metadata| metadata.len()) else {
            return false;
        };
        if length < 64 || session.seek(SeekFrom::End(-64)).is_err() {
            return false;
        }
        let mut tail = [0_u8; 64];
        session.read_exact(&mut tail).is_ok() && tail.windows(4).any(|window| window == b"END3")
    }

    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "late-signal-after-source-fault");
    let outputs = LiveCliOutputs::new(&test_directory, "late-signal-after-source-fault");
    let boots = [0x83, 0x84];
    let (mut child, mut left, mut right) = spawn_live_cli(&fixture, &outputs);
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [0, 0],
        FRAME_COUNT,
        Instant::now(),
        "late signal prefix",
    );

    // Freeze the whole process while both sources are still valid. Once their
    // writers close, advance the process in short slices and inspect only while
    // it is stopped. This creates an exact late-signal boundary without a test
    // hook in the production shutdown path.
    assert!(
        send_signal(&child, "-STOP"),
        "initial SIGSTOP reaches child"
    );
    assert!(
        wait_until(Duration::from_secs(2), || process_is_stopped(&child)),
        "live process reaches its initial stopped state"
    );
    drop(left);
    drop(right);

    let mut footer_observed_while_stopped = false;
    for _ in 0..500 {
        assert!(send_signal(&child, "-CONT"), "source-fault child advances");
        thread::sleep(Duration::from_micros(250));
        if !send_signal(&child, "-STOP") {
            break;
        }
        assert!(
            wait_until(Duration::from_secs(2), || process_is_stopped(&child)),
            "source-fault child returns to the stopped state"
        );
        if session_footer_is_visible(&outputs.session) {
            footer_observed_while_stopped = true;
            break;
        }
    }
    assert!(
        footer_observed_while_stopped,
        "source-fault child exited before a stopped END3 checkpoint; {}",
        live_session_diagnostics(&outputs.session),
    );
    send_planned_signal(&child, PlannedProcessSignal::Interrupt);
    assert!(send_signal(&child, "-CONT"), "source-fault process resumes");

    wait_for_child(
        &mut child,
        Duration::from_secs(30),
        "late signal after source fault",
    );
    let completed = child
        .wait_with_output()
        .expect("late-signal source-fault process exits");
    assert!(!completed.status.success());
    let stdout = String::from_utf8(completed.stdout).expect("late-signal status is UTF-8");
    let stderr = String::from_utf8(completed.stderr).expect("late-signal error is UTF-8");
    assert!(stderr.contains("device transport fault:"), "{stderr}");
    assert!(
        stderr.contains("SourceDisconnected") || stderr.contains("SourceSilent"),
        "{stderr}"
    );
    assert!(!stdout.contains("shutdown_signal=interrupt"));
    assert!(stdout.contains("evidence_finalization=durable"));
    let session = assert_live_bundle(&fixture, &outputs, boots);
    assert_atomic_cutoff_prefix(
        session.summary().left,
        FRAME_COUNT,
        "late-signal left stream",
    );
    assert_atomic_cutoff_prefix(
        session.summary().right,
        FRAME_COUNT,
        "late-signal right stream",
    );
}

#[test]
fn record_stream_requires_a_template_and_two_distinct_outputs() {
    let output = run(&["record-stream", "only-one-path"]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains(
        "record-stream requires template, rig evidence, axis characterization, one or two devices in template order, locked manifest, and session output paths"
    ));
}

#[test]
fn record_hand_requires_one_template_device_and_two_distinct_outputs() {
    let output = run(&["record-hand", "only-one-path"]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains(
        "record-hand requires template, rig evidence, axis characterization, device, locked manifest partial, session partial, --ready-fd, and --stop-fd operands"
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn record_hand_rejects_non_tty_character_device_before_identity_bootstrap() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_one_hand_manifest(
        &test_directory,
        "record-hand-non-tty-character-device",
        Hand::Left,
    );
    let template = manifest.with_extension("template.json");
    let locked_manifest = manifest.with_extension("locked.json");
    let session = manifest.with_extension("iks");
    write_template_from_manifest(manifest.to_str().expect("path is UTF-8"), &template);
    let evidence = manifest_evidence_files(&template);

    let rejected = run(&[
        "record-hand",
        template.to_str().expect("template path is UTF-8"),
        path_text(&evidence.rig),
        path_text(&evidence.axis),
        "/dev/null",
        locked_manifest
            .to_str()
            .expect("locked manifest path is UTF-8"),
        session.to_str().expect("session path is UTF-8"),
        "--ready-fd",
        "3",
        "--stop-fd",
        "4",
    ]);
    let stderr = String::from_utf8(rejected.stderr).expect("error is UTF-8");

    assert!(!rejected.status.success());
    assert!(stderr.contains("controller source /dev/null is a character device but not a TTY"));
    assert!(!stderr.contains("identity bootstrap"));
    assert!(!locked_manifest.exists());
    assert!(!session.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn record_hand_rejects_fifo_fixtures_because_gate0_requires_a_real_duplex_tty() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    for (label, hand) in [("left-hand", Hand::Left), ("right-hand", Hand::Right)] {
        let expected = write_one_hand_manifest(&test_directory, label, hand);
        let template = expected.with_extension("template.json");
        let locked = expected.with_extension("locked.json");
        let session = expected.with_extension("iks");
        let device = expected.with_extension("fifo");
        write_template_from_manifest(expected.to_str().expect("path is UTF-8"), &template);
        let fifo = Command::new("mkfifo")
            .arg(&device)
            .status()
            .expect("mkfifo runs");
        assert!(fifo.success(), "one-hand FIFO is created");
        let evidence = manifest_evidence_files(&template);

        let rejected = run(&[
            "record-hand",
            template.to_str().expect("template path is UTF-8"),
            path_text(&evidence.rig),
            path_text(&evidence.axis),
            device.to_str().expect("device path is UTF-8"),
            locked.to_str().expect("locked path is UTF-8"),
            session.to_str().expect("session path is UTF-8"),
            "--ready-fd",
            "3",
            "--stop-fd",
            "4",
        ]);
        assert!(!rejected.status.success(), "{label} FIFO must fail closed");
        assert!(
            String::from_utf8(rejected.stderr)
                .expect("error is UTF-8")
                .contains("is not a TTY")
        );
        assert!(!locked.exists());
        assert!(!session.exists());
    }
}

#[test]
fn record_hand_rejects_wrong_topology_before_opening_any_controller_or_pipe() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");

    let two_hand = write_manifest(&test_directory, "hand-wrong-topology", 208);
    let two_hand_template = two_hand.with_extension("template.json");
    let topology_locked = two_hand.with_extension("topology-locked.json");
    let topology_session = two_hand.with_extension("topology.iks");
    write_template_from_manifest(
        two_hand.to_str().expect("path is UTF-8"),
        &two_hand_template,
    );
    let topology_evidence = manifest_evidence_files(&two_hand_template);
    let topology = run(&[
        "record-hand",
        two_hand_template.to_str().expect("path is UTF-8"),
        path_text(&topology_evidence.rig),
        path_text(&topology_evidence.axis),
        "unused-device",
        topology_locked.to_str().expect("path is UTF-8"),
        topology_session.to_str().expect("path is UTF-8"),
        "--ready-fd",
        "3",
        "--stop-fd",
        "4",
    ]);
    assert!(!topology.status.success());
    assert!(
        String::from_utf8(topology.stderr)
            .expect("error is UTF-8")
            .contains("requires a one-hand acquisition template")
    );
    assert!(!topology_locked.exists());
    assert!(!topology_session.exists());
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod gate0_quiesce_cli_contract {
    use super::*;
    use invisible_keyboard::{
        protocol::{QuiesceReceipt, QuiesceRequest, decode_quiesce_request},
        session::{FOOTER_LEN, audit_gate0a_session_reader},
        transport::{TransportCandidate, TransportDecoder, encode_transport_quiesce_receipt},
    };
    #[cfg(target_arch = "x86_64")]
    use std::ffi::c_void;
    use std::{
        ffi::CStr,
        io,
        mem::MaybeUninit,
        os::{
            fd::{AsRawFd, FromRawFd, RawFd},
            unix::ffi::OsStrExt,
        },
    };
    const CHILD_TIMEOUT: Duration = Duration::from_secs(4);
    const IO_TIMEOUT: Duration = Duration::from_secs(2);

    struct Gate0CliRig {
        child: Child,
        master: File,
        output_control: File,
        stop_writer: Option<File>,
        locked_manifest: PathBuf,
        session: PathBuf,
        hand: Hand,
    }

    impl Gate0CliRig {
        fn start(test_directory: &std::ffi::OsStr, label: &str, hand: Hand) -> Self {
            Self::start_after_enumeration_delay(test_directory, label, hand, Duration::ZERO)
        }

        fn start_after_enumeration_delay(
            test_directory: &std::ffi::OsStr,
            label: &str,
            hand: Hand,
            delay: Duration,
        ) -> Self {
            let manifest = write_one_hand_manifest(test_directory, label, hand);
            let template = manifest.with_extension("template.json");
            let locked_manifest = manifest.with_extension("locked-manifest.partial");
            let session = manifest.with_extension("iks3.partial");
            write_template_from_manifest(path_text(&manifest), &template);
            let evidence = manifest_evidence_files(&template);
            let (mut master, pty_path) = open_pty();
            // Keep a pre-exclusive handle so fault cases can stop terminal
            // transmission without changing the recorder's raw discipline.
            let output_control = File::open(&pty_path).expect("PTY flow-control handle opens");
            let device_path = manifest.with_extension("tty");
            if delay.is_zero() {
                std::os::unix::fs::symlink(&pty_path, &device_path).unwrap();
            }
            let (mut ready_reader, ready_writer) = pipe_pair();
            let (stop_reader, stop_writer) = pipe_pair();

            inherit_descriptor(ready_writer.as_raw_fd());
            inherit_descriptor(stop_reader.as_raw_fd());
            let mut command = command_binary("INVISIBLE_KEYBOARD_RLOCATION", &[]);
            command
                .args([
                    "record-hand",
                    path_text(&template),
                    path_text(&evidence.rig),
                    path_text(&evidence.axis),
                    path_text(&device_path),
                    path_text(&locked_manifest),
                    path_text(&session),
                    "--ready-fd",
                    &ready_writer.as_raw_fd().to_string(),
                    "--stop-fd",
                    &stop_reader.as_raw_fd().to_string(),
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().expect("Gate 0A recorder starts");
            drop(ready_writer);
            drop(stop_reader);
            if !delay.is_zero() {
                thread::sleep(delay);
                std::os::unix::fs::symlink(&pty_path, &device_path).unwrap();
            }

            wait_for_raw_921600_8n1(&master, &mut child);
            master
                .write_all(&transport_frame(hand, 0))
                .expect("identity bootstrap frame writes through PTY");
            assert_eq!(
                read_pipe_to_eof(&mut ready_reader, &mut child),
                [1],
                "recorder publishes one private readiness byte and closes its end"
            );
            assert!(
                locked_manifest.exists(),
                "readiness follows locked manifest creation"
            );
            assert!(
                fs::metadata(&session)
                    .expect("readiness follows session header creation")
                    .len()
                    >= u64::try_from(HEADER_LEN).expect("header length fits u64")
            );

            Self {
                child,
                master,
                output_control,
                stop_writer: Some(stop_writer),
                locked_manifest,
                session,
                hand,
            }
        }

        fn request_quiesce(&mut self) -> QuiesceRequest {
            self.stop_writer
                .as_mut()
                .expect("analyzer guard remains open")
                .write_all(&[1])
                .expect("wrapper writes the sole stop byte");
            let request = read_request(&mut self.master, &mut self.child);
            assert_eq!(request.hand, self.hand);
            assert_eq!(
                request.controller_id_sha256,
                sensor_frame(self.hand, 0).controller_id_sha256
            );
            assert_eq!(request.boot_id, sensor_frame(self.hand, 0).boot_id);
            request
        }

        fn write_final_frame_and_receipt(
            &mut self,
            request: QuiesceRequest,
            trailing_bytes: &[u8],
        ) {
            let final_sequence = 1;
            let receipt = QuiesceReceipt {
                hand: request.hand,
                controller_id_sha256: request.controller_id_sha256,
                boot_id: request.boot_id,
                session_id: request.session_id,
                request_start_time_ns: request.request_start_time_ns,
                final_sequence,
            };
            let mut response = transport_frame(self.hand, final_sequence);
            response
                .extend(encode_transport_quiesce_receipt(&receipt).expect("valid receipt encodes"));
            response.extend_from_slice(trailing_bytes);
            self.master
                .write_all(&response)
                .expect("controller response writes through PTY");
        }

        fn wait_until_receipt_is_flushed(&mut self) {
            let deadline = Instant::now() + IO_TIMEOUT;
            while Instant::now() < deadline {
                if fs::read(&self.session)
                    .is_ok_and(|bytes| bytes.windows(4).any(|window| window == b"IKR2"))
                {
                    return;
                }
                assert_child_running(&mut self.child, "waiting for the receipt record");
                thread::sleep(Duration::from_millis(2));
            }
            panic!("receipt record was not flushed before {IO_TIMEOUT:?}");
        }

        fn close_analyzer_guard(&mut self) {
            drop(self.stop_writer.take());
        }

        fn disconnect_controller(&mut self) {
            let placeholder = File::open("/dev/null").expect("disconnect placeholder opens");
            drop(std::mem::replace(&mut self.master, placeholder));
        }

        fn finish(mut self) -> std::process::Output {
            wait_for_child(&mut self.child, CHILD_TIMEOUT);
            self.child
                .wait_with_output()
                .expect("Gate 0A recorder output is collected")
        }
    }

    #[test]
    fn record_hand_waits_for_initial_usb_enumeration() {
        let directory = std::env::var_os("TEST_TMPDIR").unwrap();
        let mut rig = Gate0CliRig::start_after_enumeration_delay(
            &directory,
            "gate0-delayed-usb",
            Hand::Right,
            Duration::from_millis(200),
        );
        let request = rig.request_quiesce();
        rig.write_final_frame_and_receipt(request, &[]);
        rig.wait_until_receipt_is_flushed();
        rig.close_analyzer_guard();
        let session = rig.session.clone();
        let completed = rig.finish();
        assert!(
            completed.status.success(),
            "{}",
            String::from_utf8_lossy(&completed.stderr)
        );
        let audit = audit_gate0a_session_reader(File::open(session).unwrap()).unwrap();
        assert_eq!(audit.terminal().request, request);
        assert_eq!(audit.terminal().final_frame, sensor_frame(Hand::Right, 1));
    }

    #[test]
    fn record_stream_signal_finishes_a_fragmented_record_without_a_partial_tail() {
        let directory = std::env::var_os("TEST_TMPDIR").unwrap();
        let manifest = write_one_hand_manifest(&directory, "raw-fragment-stop", Hand::Right);
        let template = manifest.with_extension("template.json");
        let locked = manifest.with_extension("locked.json");
        let session = manifest.with_extension("iks");
        write_template_from_manifest(path_text(&manifest), &template);
        let evidence = manifest_evidence_files(&template);
        let (mut master, device) = open_pty();
        let mut child = command_binary(
            "INVISIBLE_KEYBOARD_RLOCATION",
            &[
                "record-stream",
                path_text(&template),
                path_text(&evidence.rig),
                path_text(&evidence.axis),
                path_text(&device),
                path_text(&locked),
                path_text(&session),
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
        wait_for_raw_921600_8n1(&master, &mut child);
        master.write_all(&transport_frame(Hand::Right, 0)).unwrap();
        // Ordinary capture buffers its header; file creation follows identity lock.
        assert!(super::wait_until(IO_TIMEOUT, || session.is_file()));

        // Queue a real signal and only a frame prefix while the child is stopped.
        // The final write also starts another record, outside the stop boundary.
        send_signal(&child, libc::SIGSTOP);
        let mut status = 0;
        // SAFETY: waitpid observes this owned child's stop into valid storage.
        assert_eq!(
            unsafe { libc::waitpid(child.id() as i32, &mut status, libc::WUNTRACED) },
            child.id() as i32
        );
        assert!(libc::WIFSTOPPED(status));
        send_signal(&child, libc::SIGINT);
        let frame = transport_frame(Hand::Right, 1);
        master.write_all(&frame[..20]).unwrap();
        send_signal(&child, libc::SIGCONT);
        thread::sleep(Duration::from_millis(10));
        let mut suffix = frame[20..].to_vec();
        suffix.extend_from_slice(&transport_frame(Hand::Right, 2)[..20]);
        master.write_all(&suffix).unwrap();
        wait_for_child(&mut child, CHILD_TIMEOUT);
        let completed = child.wait_with_output().unwrap();
        assert!(
            completed.status.success(),
            "{}",
            String::from_utf8_lossy(&completed.stderr)
        );
        let finalized = FinalizedSession::audit(fs::read(session).unwrap()).unwrap();
        assert_eq!(finalized.summary().right.received, 2);
        assert_eq!(finalized.summary().invalid_records, 0);
        assert_eq!(finalized.summary().unterminated_candidates, 0);
    }

    #[test]
    fn record_hand_completes_one_identity_bound_quiesce_transaction_over_real_kernel_io() {
        let test_directory =
            std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");

        for hand in [Hand::Left, Hand::Right] {
            let mut rig = Gate0CliRig::start(
                &test_directory,
                &format!("gate0-quiesce-success-{hand:?}"),
                hand,
            );
            let request = rig.request_quiesce();
            rig.write_final_frame_and_receipt(request, &[]);
            rig.wait_until_receipt_is_flushed();
            rig.close_analyzer_guard();
            let session = rig.session.clone();
            let locked_manifest = rig.locked_manifest.clone();
            let completed = rig.finish();
            let stdout = String::from_utf8(completed.stdout).expect("success output is UTF-8");

            assert!(
                completed.status.success(),
                "stderr: {}",
                String::from_utf8_lossy(&completed.stderr)
            );
            assert!(stdout.contains("deterministic_quiesce=host-confirmed"));
            assert!(stdout.contains("quiesce_requests=1"));
            assert!(stdout.contains("quiesce_receipts=1"));
            assert!(stdout.contains("final_sequence=1"));
            assert!(locked_manifest.exists());

            let audit = audit_gate0a_session_reader(
                File::open(&session).expect("finalized Gate 0A session opens"),
            )
            .expect("finalized Gate 0A session passes the strict terminal audit");
            assert_eq!(audit.session().summary().quiesce_requests, 1);
            assert_eq!(audit.session().summary().quiesce_receipts, 1);
            assert_eq!(audit.terminal().request, request);
            assert_eq!(audit.terminal().receipt.final_sequence, 1);
            assert_eq!(audit.terminal().final_frame, sensor_frame(hand, 1));
            assert!(audit.terminal().response_time_ns <= 750_000_000);
            assert_has_footer(&session);
        }
    }

    #[test]
    fn record_hand_signals_before_the_footer_commit_abort_without_a_footer() {
        let test_directory =
            std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");

        for (label, signal, expected) in [
            ("sigint", libc::SIGINT, "aborted by Interrupt"),
            ("sigterm", libc::SIGTERM, "aborted by Terminate"),
        ] {
            let mut rig = Gate0CliRig::start(
                &test_directory,
                &format!("gate0-pre-footer-{label}"),
                Hand::Left,
            );
            let request = rig.request_quiesce();
            rig.write_final_frame_and_receipt(request, &[]);
            rig.wait_until_receipt_is_flushed();

            // The analyzer guard is still open, so the recorder cannot have
            // claimed its footer boundary when the real handler accepts this
            // process signal.
            send_signal(&rig.child, signal);
            assert_aborted_without_footer(rig, expected);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn record_hand_signal_after_the_footer_commit_does_not_revoke_end3() {
        let test_directory =
            std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
        let mut rig = Gate0CliRig::start(
            &test_directory,
            "gate0-post-footer-commit-signal",
            Hand::Left,
        );
        let request = rig.request_quiesce();
        rig.write_final_frame_and_receipt(request, &[]);
        rig.wait_until_receipt_is_flushed();
        let session = rig.session.clone();

        inject_signal_at_footer_write_entry(&mut rig, libc::SIGTERM);
        let completed = rig.finish();

        assert!(
            completed.status.success(),
            "a post-boundary signal cannot revoke the session: {}",
            String::from_utf8_lossy(&completed.stderr)
        );
        assert_has_footer(&session);
    }

    #[test]
    fn record_hand_bounds_a_stalled_request_write_and_drain() {
        let directory = std::env::var_os("TEST_TMPDIR").unwrap();
        let mut rig = Gate0CliRig::start(&directory, "gate0-stalled-request", Hand::Right);
        // SAFETY: this live slave descriptor names the same terminal as the
        // recorder. TCOOFF suspends output while leaving sensor input active.
        assert_eq!(
            unsafe { libc::tcflow(rig.output_control.as_raw_fd(), libc::TCOOFF) },
            0
        );
        rig.stop_writer.as_mut().unwrap().write_all(&[1]).unwrap();
        let deadline = Instant::now() + Duration::from_millis(1500);
        while rig.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        let exited_while_stalled = rig.child.try_wait().unwrap().is_some();
        if !exited_while_stalled {
            rig.child.kill().expect("failed fixture child is stopped");
        }
        let session = rig.session.clone();
        let completed = rig.finish();
        assert!(
            exited_while_stalled,
            "a stalled request must not block deadline supervision"
        );
        assert!(!completed.status.success());
        assert!(
            String::from_utf8_lossy(&completed.stderr).contains("750000000 ns"),
            "{}",
            String::from_utf8_lossy(&completed.stderr)
        );
        assert_no_footer(&session);
    }

    #[test]
    fn record_hand_observes_control_faults_during_stalled_transmission() {
        let directory = std::env::var_os("TEST_TMPDIR").unwrap();
        for (label, signal, expected) in [
            ("guard", None, "closed before a matching receipt"),
            ("signal", Some(libc::SIGTERM), "aborted by Terminate"),
        ] {
            let mut rig = Gate0CliRig::start(&directory, label, Hand::Right);
            // SAFETY: the owned slave descriptor is live; TCOOFF stops only
            // terminal transmission, not the independent guard or signal.
            assert_eq!(
                unsafe { libc::tcflow(rig.output_control.as_raw_fd(), libc::TCOOFF) },
                0
            );
            rig.stop_writer.as_mut().unwrap().write_all(&[1]).unwrap();
            let deadline = Instant::now() + IO_TIMEOUT;
            while !fs::read(&rig.session)
                .unwrap()
                .windows(4)
                .any(|bytes| bytes == b"IKQ2")
            {
                assert_child_running(&mut rig.child, "waiting for the outbound request record");
                assert!(
                    Instant::now() < deadline,
                    "request record must be observable"
                );
                thread::sleep(Duration::from_millis(2));
            }
            if let Some(signal) = signal {
                send_signal(&rig.child, signal);
            } else {
                rig.close_analyzer_guard();
            }
            assert_aborted_without_footer(rig, expected);
        }
    }

    #[test]
    fn record_hand_receipt_deadline_applies_while_sensor_frames_keep_arriving() {
        let directory = std::env::var_os("TEST_TMPDIR").unwrap();
        let mut rig = Gate0CliRig::start(&directory, "gate0-busy-timeout", Hand::Right);
        let _request = rig.request_quiesce();
        let deadline = Instant::now() + Duration::from_millis(1500);
        let mut sequence = 1;
        while rig.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            rig.master
                .write_all(&transport_frame(Hand::Right, sequence))
                .expect("ordinary acquisition continues without a quiesce receipt");
            sequence += 1;
            thread::sleep(Duration::from_millis(1));
        }
        let exited_while_receiving = rig.child.try_wait().unwrap().is_some();
        if !exited_while_receiving {
            rig.child.kill().expect("failed fixture child is stopped");
        }
        assert_aborted_without_footer(rig, "receipt was not observed within 750000000 ns");
        assert!(
            exited_while_receiving,
            "sensor traffic must not extend the receipt deadline"
        );
    }

    #[test]
    fn record_hand_abort_paths_never_write_a_footer() {
        let test_directory =
            std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");

        let mut early_guard = Gate0CliRig::start(&test_directory, "gate0-early-guard", Hand::Left);
        early_guard.close_analyzer_guard();
        assert_aborted_without_footer(early_guard, "closed before a matching receipt");

        let mut duplicate_stop =
            Gate0CliRig::start(&test_directory, "gate0-duplicate-stop", Hand::Left);
        duplicate_stop
            .stop_writer
            .as_mut()
            .expect("stop pipe remains open")
            .write_all(&[1, 1])
            .expect("invalid stop sequence writes");
        assert_aborted_without_footer(duplicate_stop, "expected exactly one local 0x01 stop byte");

        let mut post_receipt =
            Gate0CliRig::start(&test_directory, "gate0-post-receipt", Hand::Left);
        let request = post_receipt.request_quiesce();
        post_receipt.write_final_frame_and_receipt(request, &[0x7f]);
        assert_aborted_without_footer(post_receipt, "after its receipt");

        let mut delayed_byte =
            Gate0CliRig::start(&test_directory, "gate0-delayed-byte", Hand::Left);
        let request = delayed_byte.request_quiesce();
        delayed_byte.write_final_frame_and_receipt(request, &[]);
        delayed_byte.wait_until_receipt_is_flushed();
        thread::sleep(Duration::from_millis(100));
        delayed_byte
            .master
            .write_all(&[0x55])
            .expect("delayed post-receipt byte writes");
        assert_aborted_without_footer(delayed_byte, "after its receipt");

        let mut final_silence_byte =
            Gate0CliRig::start(&test_directory, "gate0-final-silence-byte", Hand::Left);
        let request = final_silence_byte.request_quiesce();
        final_silence_byte.write_final_frame_and_receipt(request, &[]);
        final_silence_byte.wait_until_receipt_is_flushed();
        final_silence_byte.close_analyzer_guard();
        thread::sleep(Duration::from_millis(80));
        final_silence_byte
            .master
            .write_all(&[0xaa])
            .expect("near-EOF post-receipt byte writes");
        assert_aborted_without_footer(final_silence_byte, "after its receipt");

        let mut disconnected =
            Gate0CliRig::start(&test_directory, "gate0-disconnected", Hand::Left);
        disconnected.disconnect_controller();
        assert_aborted_without_footer(disconnected, "controller disconnected");

        let mut timed_out = Gate0CliRig::start(&test_directory, "gate0-timeout", Hand::Left);
        let _request = timed_out.request_quiesce();
        assert_aborted_without_footer(timed_out, "receipt was not observed within 750000000 ns");
    }

    fn assert_aborted_without_footer(rig: Gate0CliRig, expected_error: &str) {
        let session = rig.session.clone();
        let completed = rig.finish();
        let stderr = String::from_utf8(completed.stderr).expect("abort error is UTF-8");
        assert!(!completed.status.success(), "Gate 0A abort must be nonzero");
        assert!(stderr.contains(expected_error), "stderr: {stderr}");
        assert_no_footer(&session);
    }

    fn assert_has_footer(path: &Path) {
        let bytes = fs::read(path).expect("session remains readable");
        assert!(bytes.len() >= FOOTER_LEN);
        assert_eq!(&bytes[bytes.len() - FOOTER_LEN..][..4], b"END3");
    }

    fn assert_no_footer(path: &Path) {
        let bytes = fs::read(path).expect("aborted partial session remains inspectable");
        assert!(
            bytes.len() < FOOTER_LEN || &bytes[bytes.len() - FOOTER_LEN..][..4] != b"END3",
            "aborted Gate 0A session must not contain a terminal footer"
        );
    }

    fn read_request(master: &mut File, child: &mut Child) -> QuiesceRequest {
        let encoded = read_delimited_record(master, child);
        let mut decoder = TransportDecoder::new();
        let candidates = decoder.push(&encoded);
        assert_eq!(candidates.len(), 1, "one request record is written");
        match &candidates[0] {
            TransportCandidate::Decoded(raw) => {
                decode_quiesce_request(raw).expect("outbound record is one valid IKQ2 request")
            }
            candidate => panic!("outbound request did not decode: {candidate:?}"),
        }
    }

    fn read_delimited_record(master: &mut File, child: &mut Child) -> Vec<u8> {
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut encoded = Vec::new();
        while Instant::now() < deadline {
            if wait_readable(master.as_raw_fd(), Duration::from_millis(10)) {
                let mut chunk = [0_u8; 256];
                match master.read(&mut chunk) {
                    Ok(0) => panic!("PTY closed before the request record"),
                    Ok(count) => {
                        encoded.extend_from_slice(&chunk[..count]);
                        if let Some(delimiter) = encoded.iter().position(|byte| *byte == 0) {
                            assert_eq!(delimiter + 1, encoded.len(), "one exact write is observed");
                            return encoded;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => panic!("request read fails: {error}"),
                }
            }
            assert_child_running(child, "waiting for the outbound request");
        }
        panic!("outbound request was not observed before {IO_TIMEOUT:?}");
    }

    fn read_pipe_to_eof(reader: &mut File, child: &mut Child) -> Vec<u8> {
        let deadline = Instant::now() + IO_TIMEOUT;
        let mut bytes = Vec::new();
        while Instant::now() < deadline {
            if wait_readable(reader.as_raw_fd(), Duration::from_millis(10)) {
                let mut chunk = [0_u8; 16];
                match reader.read(&mut chunk) {
                    Ok(0) => return bytes,
                    Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => panic!("ready pipe read fails: {error}"),
                }
            }
            assert_child_running(child, "waiting for private readiness");
        }
        panic!("ready pipe did not close before {IO_TIMEOUT:?}");
    }

    fn wait_for_raw_921600_8n1(master: &File, child: &mut Child) {
        let deadline = Instant::now() + IO_TIMEOUT;
        while Instant::now() < deadline {
            let mut settings = MaybeUninit::<libc::termios>::uninit();
            // SAFETY: `master` is a live PTY descriptor and `settings` points
            // to writable storage for one complete termios value.
            if unsafe { libc::tcgetattr(master.as_raw_fd(), settings.as_mut_ptr()) } == 0 {
                // SAFETY: successful `tcgetattr` initialized every field.
                let settings = unsafe { settings.assume_init() };
                let raw = settings.c_lflag & (libc::ECHO | libc::ICANON | libc::ISIG) == 0;
                let eight_bits = settings.c_cflag & libc::CSIZE == libc::CS8;
                let one_stop = settings.c_cflag & libc::CSTOPB == 0;
                let no_parity = settings.c_cflag & libc::PARENB == 0;
                // SAFETY: the initialized termios value remains live for both reads.
                let correct_speed = unsafe {
                    libc::cfgetispeed(&settings) == libc::B921600
                        && libc::cfgetospeed(&settings) == libc::B921600
                };
                if raw && eight_bits && one_stop && no_parity && correct_speed {
                    return;
                }
            }
            assert_child_running(child, "waiting for raw 921600 8N1 PTY setup");
            thread::sleep(Duration::from_millis(2));
        }
        panic!("PTY was not configured before {IO_TIMEOUT:?}");
    }

    fn wait_readable(descriptor: RawFd, timeout: Duration) -> bool {
        let timeout_ms = i32::try_from(timeout.as_millis()).expect("poll timeout fits i32");
        let mut descriptor = libc::pollfd {
            fd: descriptor,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the pointer references one initialized pollfd for the call.
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return false;
            }
            panic!("poll fails: {error}");
        }
        ready != 0
    }

    fn assert_child_running(child: &mut Child, context: &str) {
        if let Some(status) = child.try_wait().expect("child status is readable") {
            panic!("Gate 0A recorder exited {status} while {context}");
        }
    }

    fn wait_for_child(child: &mut Child, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if child
                .try_wait()
                .expect("Gate 0A recorder status is readable")
                .is_some()
            {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        child.kill().expect("hung Gate 0A recorder is terminated");
        panic!("Gate 0A recorder did not exit before {timeout:?}");
    }

    #[cfg(target_arch = "x86_64")]
    fn inject_signal_at_footer_write_entry(rig: &mut Gate0CliRig, signal: i32) {
        let process_id = i32::try_from(rig.child.id()).expect("child PID fits i32");
        let null = std::ptr::null_mut::<c_void>();
        // SAFETY: the trace target is this test's live child process.
        assert_eq!(
            unsafe { libc::ptrace(libc::PTRACE_ATTACH, process_id, null, null) },
            0
        );
        let mut status = 0;
        // SAFETY: `status` points to writable storage and the PID names the
        // attached child, so waitpid cannot write outside this stack value.
        assert_eq!(
            unsafe { libc::waitpid(process_id, &mut status, 0) },
            process_id
        );
        assert!(
            libc::WIFSTOPPED(status),
            "attached child reaches a ptrace stop"
        );
        // SAFETY: the child is ptrace-stopped and this option only marks
        // syscall stops with bit 0x80.
        assert_eq!(
            unsafe {
                libc::ptrace(
                    libc::PTRACE_SETOPTIONS,
                    process_id,
                    null,
                    libc::PTRACE_O_TRACESYSGOOD as usize as *mut c_void,
                )
            },
            0
        );
        rig.close_analyzer_guard();

        let syscall_stop = libc::SIGTRAP | 0x80;
        let syscall_entry_rax = (-(libc::ENOSYS as i64)) as u64;
        let mut resume_signal = 0;
        for _ in 0..10_000 {
            // SAFETY: the child remains ptrace-stopped between waitpid and
            // this resume operation; the optional data value is a signal.
            assert_eq!(
                unsafe {
                    libc::ptrace(
                        libc::PTRACE_SYSCALL,
                        process_id,
                        null,
                        resume_signal as usize as *mut c_void,
                    )
                },
                0
            );
            resume_signal = 0;
            // SAFETY: as above, waitpid writes one status integer for the
            // traced child and blocks only until its next syscall or exit.
            assert_eq!(
                unsafe { libc::waitpid(process_id, &mut status, 0) },
                process_id
            );
            assert!(
                !libc::WIFEXITED(status) && !libc::WIFSIGNALED(status),
                "recorder stays live until its footer write"
            );
            assert!(libc::WIFSTOPPED(status), "traced recorder stops");
            let stop_signal = libc::WSTOPSIG(status);
            if stop_signal != syscall_stop {
                if stop_signal != libc::SIGSTOP {
                    resume_signal = stop_signal;
                }
                continue;
            }

            let mut registers = MaybeUninit::<libc::user_regs_struct>::uninit();
            // SAFETY: the tracee is stopped and `registers` has enough
            // writable storage for the x86_64 register set.
            assert_eq!(
                unsafe {
                    libc::ptrace(
                        libc::PTRACE_GETREGS,
                        process_id,
                        null,
                        registers.as_mut_ptr().cast::<c_void>(),
                    )
                },
                0
            );
            // SAFETY: successful PTRACE_GETREGS initialized every field.
            let registers = unsafe { registers.assume_init() };
            if registers.orig_rax != libc::SYS_write as u64
                || registers.rax != syscall_entry_rax
                || registers.rdx < 4
            {
                continue;
            }

            // SAFETY: at a syscall-entry stop, RSI is the tracee's readable
            // write buffer. PEEKDATA copies one machine word into this test.
            let word = unsafe {
                libc::ptrace(
                    libc::PTRACE_PEEKDATA,
                    process_id,
                    registers.rsi as usize as *mut c_void,
                    null,
                )
            };
            if !word.to_ne_bytes().starts_with(b"END3") {
                continue;
            }

            // PTRACE reaches write entry only after Gate0FooterCommit::claim
            // has won. Delivering the signal as the child resumes therefore
            // exercises the real production process immediately after the
            // atomic boundary and before the footer syscall executes.
            // SAFETY: the child is stopped at the identified syscall entry;
            // DETACH resumes it and injects one valid process signal.
            assert_eq!(
                unsafe {
                    libc::ptrace(
                        libc::PTRACE_DETACH,
                        process_id,
                        null,
                        signal as usize as *mut c_void,
                    )
                },
                0
            );
            return;
        }
        panic!("recorder did not reach the END3 write syscall");
    }

    fn send_signal(child: &Child, signal: i32) {
        let process_id = i32::try_from(child.id()).expect("child PID fits i32");
        // SAFETY: `process_id` names the live child created by this test, and
        // SIGINT and SIGTERM have no pointer or memory-safety preconditions.
        assert_eq!(unsafe { libc::kill(process_id, signal) }, 0);
    }

    fn inherit_descriptor(descriptor: RawFd) {
        // SAFETY: `descriptor` is owned by a live File for the duration of both calls.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        assert!(flags >= 0, "descriptor flags are readable");
        // SAFETY: changing only FD_CLOEXEC preserves the pipe open-file description.
        assert_eq!(
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) },
            0,
            "descriptor is inherited by the recorder"
        );
    }

    fn pipe_pair() -> (File, File) {
        let mut descriptors = [-1; 2];
        // SAFETY: `descriptors` has storage for the two descriptors returned by pipe2.
        assert_eq!(
            unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
            0,
            "private pipe is created"
        );
        // SAFETY: successful pipe2 returned two new, independently owned descriptors.
        unsafe {
            (
                File::from_raw_fd(descriptors[0]),
                File::from_raw_fd(descriptors[1]),
            )
        }
    }

    pub(super) fn open_pty() -> (File, PathBuf) {
        // SAFETY: `posix_openpt` accepts this constant flag set without pointers.
        let descriptor =
            unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
        assert!(
            descriptor >= 0,
            "PTY master opens: {}",
            io::Error::last_os_error()
        );
        // SAFETY: `descriptor` is the live PTY master returned above.
        assert_eq!(unsafe { libc::grantpt(descriptor) }, 0, "PTY is granted");
        // SAFETY: `descriptor` is the granted live PTY master.
        assert_eq!(unsafe { libc::unlockpt(descriptor) }, 0, "PTY is unlocked");
        let mut path: [libc::c_char; 256] = [0; 256];
        // SAFETY: `path` is writable for its full length and `descriptor` is a live PTY master.
        assert_eq!(
            unsafe { libc::ptsname_r(descriptor, path.as_mut_ptr(), path.len()) },
            0,
            "PTY slave path resolves"
        );
        // SAFETY: successful `ptsname_r` wrote a NUL-terminated string into `path`.
        let path = unsafe { CStr::from_ptr(path.as_ptr()) };
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes()));
        // SAFETY: ownership of the fresh PTY master descriptor moves into this File.
        (unsafe { File::from_raw_fd(descriptor) }, path)
    }
}

#[cfg(target_os = "linux")]
#[test]
fn commissioning_cli_rejects_noninteractive_capture_before_outputs_or_devices() {
    let test_directory = std::env::var_os("TEST_TMPDIR").unwrap();
    let manifest = write_one_hand_manifest(&test_directory, "axis-no-terminal", Hand::Right);
    let template = manifest.with_extension("template.json");
    write_template_from_manifest(path_text(&manifest), &template);
    let evidence = manifest_evidence_files(&template);
    let directory = manifest.with_extension("capture");
    let rejected = run(&[
        "characterize-axes",
        path_text(&template),
        path_text(&evidence.rig),
        path_text(&directory),
        "/missing-controller",
    ]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("needs a terminal"));
    assert!(!directory.exists());
    assert!(rejected.stdout.is_empty());
}

#[test]
fn commissioning_rig_hash_cli_uses_typed_canonical_bytes_not_input_formatting() {
    let test_directory = std::env::var_os("TEST_TMPDIR").unwrap();
    let manifest = write_manifest(&test_directory, "axis-rig-hash", 208);
    let evidence = manifest_evidence_files(&manifest);
    let rig = RigEvidenceRecord::from_json(&fs::read(&evidence.rig).unwrap()).unwrap();
    let fields: serde_json::Value =
        serde_json::from_slice(&fs::read(&evidence.rig).unwrap()).unwrap();
    let reversed = fields
        .as_object()
        .unwrap()
        .iter()
        .rev()
        .map(|(key, value)| {
            format!(
                "{}: {}",
                serde_json::to_string(key).unwrap(),
                serde_json::to_string_pretty(value).unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    fs::write(&evidence.rig, format!("{{\n{reversed}\n}}\n")).unwrap();
    let output = run(&["hash-rig-evidence", path_text(&evidence.rig)]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{}\n", format_sha256(&rig.sha256().unwrap()))
    );
    fs::write(&evidence.rig, b"{}").unwrap();
    let rejected = run(&["hash-rig-evidence", path_text(&evidence.rig)]);
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
}

/// Own a real subprocess and stop its PTY producer even when a test assertion
/// fails. This is an I/O fixture, not a mock of the commissioning algorithm.
#[cfg(target_os = "linux")]
struct AxisCliProcess {
    child: Child,
    producer_stop: Arc<AtomicBool>,
}

#[cfg(target_os = "linux")]
impl Drop for AxisCliProcess {
    fn drop(&mut self) {
        self.producer_stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn commissioning_cli_abort_paths_retain_partial_evidence_without_publishing_reports() {
    use std::fs::OpenOptions;
    let test_directory = std::env::var_os("TEST_TMPDIR").unwrap();
    for failure in ["disconnect", "acknowledgement", "signal"] {
        let manifest =
            write_one_hand_manifest(&test_directory, &format!("axis-{failure}"), Hand::Left);
        let template = manifest.with_extension("template.json");
        write_template_from_manifest(path_text(&manifest), &template);
        let evidence = manifest_evidence_files(&template);
        let (device_master, device_path) = gate0_quiesce_cli_contract::open_pty();
        let (mut terminal_master, terminal_path) = gate0_quiesce_cli_contract::open_pty();
        let terminal = OpenOptions::new()
            .read(true)
            .write(true)
            .open(terminal_path)
            .unwrap();
        let directory = manifest.with_extension("capture");
        let child = command_binary(
            "INVISIBLE_KEYBOARD_RLOCATION",
            &[
                "characterize-axes",
                path_text(&template),
                path_text(&evidence.rig),
                path_text(&directory),
                path_text(&device_path),
            ],
        )
        .stdin(terminal)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
        let mut process = AxisCliProcess {
            child,
            producer_stop: Arc::new(AtomicBool::new(false)),
        };
        let partial = directory.join("axis-session.partial");
        assert!(
            wait_until(Duration::from_secs(5), || fs::metadata(&partial)
                .is_ok_and(|metadata| metadata.len() >= HEADER_LEN as u64)),
            "capture writes its header"
        );
        let mut device_master = Some(device_master);
        match failure {
            "disconnect" => drop(device_master.take()),
            "acknowledgement" => terminal_master.write_all(b"not-ready\n").unwrap(),
            "signal" => {
                assert!(
                    Command::new("kill")
                        .args(["-TERM", &process.child.id().to_string()])
                        .status()
                        .unwrap()
                        .success()
                );
            }
            _ => unreachable!(),
        }
        wait_for_child(&mut process.child, Duration::from_secs(3), "axis abort");
        assert!(!process.child.wait().unwrap().success());
        let mut stderr = String::new();
        process
            .child
            .stderr
            .as_mut()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        let expected = match failure {
            "disconnect" => "disconnected",
            "acknowledgement" => "press Enter once",
            "signal" => "cancelled",
            _ => unreachable!(),
        };
        assert!(stderr.contains(expected), "{stderr}");
        assert_eq!(fs::metadata(partial).unwrap().len(), HEADER_LEN as u64);
        assert!(!directory.join("axis-report.json").exists());
        assert!(!directory.join("acquisition-template.json").exists());
        assert!(!directory.join("axis-session.iks").exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
#[allow(unsafe_code)]
fn commissioning_cli_guides_all_right_hand_poses_and_replay_reproduces_the_report() {
    use invisible_keyboard::manifest::AxisCommissioningCandidate;
    use std::os::fd::AsRawFd;
    use std::{fs::OpenOptions, sync::atomic::AtomicUsize};
    let test_directory = std::env::var_os("TEST_TMPDIR").unwrap();
    let manifest = write_one_hand_manifest(&test_directory, "axis-pty-full", Hand::Right);
    let template = manifest.with_extension("template.json");
    write_template_from_manifest(path_text(&manifest), &template);
    let evidence = manifest_evidence_files(&template);
    let candidate = AxisCommissioningCandidate::from_json(
        &fs::read(&template).unwrap(),
        &fs::read(&evidence.rig).unwrap(),
    )
    .unwrap();
    let (mut device_master, device_path) = gate0_quiesce_cli_contract::open_pty();
    // A PTY master can block after the last slave closes. The producer must
    // observe its stop flag during teardown instead of hanging in write_all.
    // SAFETY: F_GETFL/F_SETFL use this live owned descriptor and integer flags.
    let flags = unsafe { libc::fcntl(device_master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: the descriptor remains owned and O_NONBLOCK is a valid file flag.
    assert_eq!(
        unsafe {
            libc::fcntl(
                device_master.as_raw_fd(),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            )
        },
        0
    );
    let (mut terminal_master, terminal_path) = gate0_quiesce_cli_contract::open_pty();
    let terminal = OpenOptions::new()
        .read(true)
        .write(true)
        .open(terminal_path)
        .unwrap();
    let directory = manifest.with_extension("capture");
    let child = command_binary(
        "INVISIBLE_KEYBOARD_RLOCATION",
        &[
            "characterize-axes",
            path_text(&template),
            path_text(&evidence.rig),
            path_text(&directory),
            path_text(&device_path),
        ],
    )
    .stdin(terminal)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut process = AxisCliProcess {
        child,
        producer_stop: Arc::clone(&stop),
    };
    let stderr = process.child.stderr.take().unwrap();
    let (sender, prompts) = mpsc::channel();
    let stderr_reader = thread::spawn(move || {
        let mut captured = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            captured.push_str(&line);
            captured.push('\n');
            let _ = sender.send(line);
        }
        captured
    });
    let first = prompts
        .recv_timeout(Duration::from_secs(5))
        .expect("initial pose prompt");
    assert!(
        first.starts_with("pose=1 hand=Right sensor=0 face=+X"),
        "{first}"
    );
    let pose = Arc::new(AtomicUsize::new(0));
    let active_pose = Arc::clone(&pose);
    let expected_controller = candidate.expected_controllers()[0].1;
    let producer = thread::spawn(move || {
        let mut sequence = 0_u32;
        while !stop.load(Ordering::Relaxed) {
            let face = active_pose.load(Ordering::Relaxed) % 6;
            let mut acceleration = [0; 3];
            acceleration[face / 2] = if face.is_multiple_of(2) { 8192 } else { -8192 };
            let frame = SensorFrame {
                hand: Hand::Right,
                controller_id_sha256: expected_controller,
                boot_id: [0x51; 16],
                flags: 0,
                valid_sensor_mask: ALL_SENSORS_VALID,
                sequence,
                device_time_us: 10000 + u64::from(sequence) * 5000,
                sensors: [SensorReading {
                    acceleration,
                    angular_velocity: [0; 3],
                    sample_time_offset_us: -100,
                }; SENSOR_COUNT],
            };
            if device_master
                .write_all(&encode_transport_frame(&frame).unwrap())
                .is_err()
            {
                break;
            }
            sequence = sequence.wrapping_add(1);
            thread::sleep(Duration::from_millis(5));
        }
    });
    for current_pose in 0..36 {
        if current_pose != 0 {
            let prompt = prompts
                .recv_timeout(Duration::from_secs(10))
                .expect("next pose prompt within 10 seconds");
            assert!(
                prompt.starts_with(&format!("pose={} ", current_pose + 1)),
                "{prompt}"
            );
        }
        pose.store(current_pose, Ordering::Relaxed);
        terminal_master.write_all(b"\n").unwrap();
    }
    assert!(
        wait_until(Duration::from_secs(10), || process
            .child
            .try_wait()
            .unwrap()
            .is_some()),
        "final pose completes"
    );
    let status = process.child.wait().unwrap();
    process.producer_stop.store(true, Ordering::Relaxed);
    producer.join().unwrap();
    let stderr = stderr_reader.join().unwrap();
    assert!(status.success(), "{stderr}");
    let report_source = fs::read(directory.join("axis-report.json")).unwrap();
    let report = AxisCharacterizationReport::from_json(&report_source).unwrap();
    assert_eq!(
        report.hands[0].positive_y_direction,
        invisible_keyboard::manifest::PositiveYDirection::Radial
    );
    assert!(
        report.hands[0]
            .sensors
            .iter()
            .all(|sensor| sensor.axis_map == [1, 0, 0, 0, 1, 0, 0, 0, 1])
    );
    let session_path = directory.join("axis-session.iks");
    let session = FinalizedSession::audit(fs::read(&session_path).unwrap()).unwrap();
    assert_eq!(session.header().manifest_sha256, candidate.sha256());
    assert_eq!(
        report.source_session_sha256,
        format_sha256(&session.sha256())
    );
    assert!(!directory.join("axis-session.partial").exists());
    AcquisitionManifestTemplate::from_json_with_evidence(
        &fs::read(directory.join("acquisition-template.json")).unwrap(),
        &fs::read(&evidence.rig).unwrap(),
        &report_source,
    )
    .unwrap();
    let replay_directory = manifest.with_extension("reduced");
    let arguments = [
        "derive-axes",
        path_text(&template),
        path_text(&evidence.rig),
        path_text(&session_path),
        path_text(&replay_directory),
    ];
    let replay = run(&arguments);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay_report = AxisCharacterizationReport::from_json(
        &fs::read(replay_directory.join("axis-report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report.hands, replay_report.hands);
    assert_eq!(
        report.characterizer_build,
        replay_report.characterizer_build
    );
    assert!(
        !run(&arguments).status.success(),
        "existing output directory is never overwritten"
    );
    assert_eq!(
        replay_report,
        AxisCharacterizationReport::from_json(
            &fs::read(replay_directory.join("axis-report.json")).unwrap()
        )
        .unwrap()
    );
}

#[test]
fn score_pilot_requires_the_complete_operand_layout() {
    let rejected = run(&["score-pilot", "manifest.json"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains(
        "score-pilot requires manifest, rig evidence, axis characterization, session, truth JSON, truth artifact, clock mapping, and report output paths"
    ));
}

#[test]
fn score_pilot_reduces_bound_raw_evidence_without_authorizing_output() {
    let directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest_path = write_one_hand_manifest(&directory, "pilot-cli", Hand::Right);
    let manifest = read_bound_manifest(&manifest_path);
    let session_path = manifest_path.with_extension("iks");
    let truth_path = manifest_path.with_extension("truth.json");
    let artifact_path = manifest_path.with_extension("truth-artifact");
    let mapping_path = manifest_path.with_extension("clock-mapping");
    let report_path = manifest_path.with_extension("pilot-report.json");
    let session_id = [0x73; 16];
    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us: manifest.nominal_period_us,
            manifest_sha256: manifest.sha256().unwrap(),
            session_id,
        },
    )
    .unwrap();
    // Quiet replay deliberately misses all independently declared contacts.
    // This exercises a valid diagnostic stop, not fabricated hardware success.
    for sequence in 0..101_600 {
        writer
            .append_frame(
                ControllerStream::Right,
                &sensor_frame(Hand::Right, sequence),
                u64::from(sequence) * 5_000_000,
            )
            .unwrap();
    }
    let (session_bytes, _) = writer.finish().unwrap();
    fs::write(&session_path, &session_bytes).unwrap();
    let artifact_bytes = b"independent annotation fixture; not hardware evidence";
    let mapping_bytes = b"controller-us alignment fixture; declared error 1000us";
    fs::write(&artifact_path, artifact_bytes).unwrap();
    fs::write(&mapping_path, mapping_bytes).unwrap();
    let contacts = all_slots()
        .iter()
        .filter(|slot| slot.position.hand == Hand::Right)
        .flat_map(|slot| std::iter::repeat_n(slot.position, 10))
        .enumerate()
        .map(|(index, position)| {
            serde_json::json!({
                "time_us": 3_000_000 + index as u64 * 500_000,
                "position": position,
            })
        })
        .collect::<Vec<_>>();
    let negatives = [
        "mouse-use",
        "phone-handling",
        "scratching",
        "gesturing",
        "object-handling",
        "hand-repositioning",
        "quiet-rest",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, kind)| {
        let start = 88_000_000 + index as u64 * 60_000_000;
        serde_json::json!({"kind": kind, "interval": {
            "start_us": start, "end_us": start + 60_000_000,
        }})
    })
    .collect::<Vec<_>>();
    let truth = serde_json::json!({
        "schema": "invisible-keyboard-one-hand-pilot-truth-v1",
        "session_sha256": format_sha256(&sha256(&session_bytes)),
        "manifest_sha256": format_sha256(&manifest.sha256().unwrap()),
        "session_id": "73".repeat(16),
        "hand": Hand::Right,
        "source": "contact-switch",
        "truth_artifact_sha256": format_sha256(&sha256(artifact_bytes)),
        "clock_mapping_sha256": format_sha256(&sha256(mapping_bytes)),
        "maximum_alignment_error_us": 1_000,
        "neutral": {"start_us": 0, "end_us": 3_000_000},
        "observation": {"start_us": 3_000_000, "end_us": 508_000_000},
        "contacts": contacts,
        "negatives": negatives,
    });
    fs::write(&truth_path, serde_json::to_vec(&truth).unwrap()).unwrap();
    let invoke = |destination: &Path| {
        run_evidence_bound(
            "score-pilot",
            &manifest_path,
            &manifest_path,
            &[
                path_text(&session_path),
                path_text(&truth_path),
                path_text(&artifact_path),
                path_text(&mapping_path),
                path_text(destination),
            ],
        )
    };
    let scored = invoke(&report_path);
    assert!(
        scored.status.success(),
        "{}",
        String::from_utf8_lossy(&scored.stderr)
    );
    let stdout = String::from_utf8(scored.stdout).unwrap();
    assert!(stdout.contains("pilot_disposition=stop"));
    assert!(stdout.contains("evidence=replay-tested"));
    assert!(stdout.contains("hardware_truth=not-asserted"));
    assert!(stdout.contains("diagnostic=non-armable"));
    let report_bytes = fs::read(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(report["disposition"], "stop");
    let report_text = String::from_utf8(report_bytes.clone()).unwrap();
    for forbidden in [
        "validation-passed",
        "calibration_artifact",
        "output_authorization",
    ] {
        assert!(!report_text.contains(forbidden));
    }
    assert!(stdout.contains(&format!(
        "report_sha256={}",
        format_sha256(&sha256(&report_bytes))
    )));
    assert!(!invoke(&report_path).status.success());
    assert_eq!(fs::read(&report_path).unwrap(), report_bytes);
    let rejected_report = manifest_path.with_extension("rejected-report.json");
    for changed_source in [&artifact_path, &mapping_path] {
        let original = fs::read(changed_source).unwrap();
        fs::write(changed_source, b"different archived bytes").unwrap();
        let rejected = invoke(&rejected_report);
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("one-hand pilot failed"));
        assert!(!rejected_report.exists());
        fs::write(changed_source, original).unwrap();
    }
    let mut misbound = truth.clone();
    misbound["session_sha256"] = serde_json::json!("00".repeat(32));
    fs::write(&truth_path, serde_json::to_vec(&misbound).unwrap()).unwrap();
    assert!(!invoke(&rejected_report).status.success());
    assert!(!rejected_report.exists());
    fs::write(&truth_path, b"{}").unwrap();
    assert!(!invoke(&rejected_report).status.success());
    assert!(!rejected_report.exists());
    fs::write(&truth_path, serde_json::to_vec(&truth).unwrap()).unwrap();
    fs::write(&session_path, &session_bytes[..session_bytes.len() - 1]).unwrap();
    assert!(!invoke(&rejected_report).status.success());
    assert!(!rejected_report.exists());
}

#[test]
fn score_gate0a_requires_native_capture_and_never_overwrites_a_report() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_one_hand_manifest(&test_directory, "gate0-score-input", Hand::Left);
    let session = manifest.with_extension("iks");
    write_one_hand_session(&manifest, &session);
    let report = manifest.with_extension("gate0-report.json");
    let missing_raw = manifest.with_extension("missing.raw");
    let missing_sidecar = manifest.with_extension("missing.json");

    let missing = run_evidence_bound(
        "score-gate0a",
        &manifest,
        &manifest,
        &[
            session.to_str().expect("path is UTF-8"),
            missing_raw.to_str().expect("path is UTF-8"),
            missing_sidecar.to_str().expect("path is UTF-8"),
            report.to_str().expect("path is UTF-8"),
        ],
    );
    assert!(!missing.status.success());
    assert!(
        String::from_utf8(missing.stderr)
            .expect("error is UTF-8")
            .contains("Gate 0A capture failed")
    );
    assert!(!report.exists());

    let (raw, sidecar) = write_gate0_cli_evidence(&manifest, &session);
    fs::write(&report, b"existing report").expect("report sentinel writes");
    let occupied = run_evidence_bound(
        "score-gate0a",
        &manifest,
        &manifest,
        &[
            session.to_str().expect("path is UTF-8"),
            raw.to_str().expect("path is UTF-8"),
            sidecar.to_str().expect("path is UTF-8"),
            report.to_str().expect("path is UTF-8"),
        ],
    );
    assert!(!occupied.status.success());
    assert_eq!(fs::read(&report).unwrap(), b"existing report");
}

#[test]
fn score_gate0a_reduces_raw_samples_and_writes_a_bound_report() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_one_hand_manifest(&test_directory, "gate0-score", Hand::Right);
    let session = manifest.with_extension("iks");
    write_one_hand_session(&manifest, &session);
    let (raw, sidecar) = write_gate0_cli_evidence(&manifest, &session);
    let report = manifest.with_extension("gate0-bound-report.json");

    let scored = run_evidence_bound(
        "score-gate0a",
        &manifest,
        &manifest,
        &[
            session.to_str().expect("path is UTF-8"),
            raw.to_str().expect("path is UTF-8"),
            sidecar.to_str().expect("path is UTF-8"),
            report.to_str().expect("path is UTF-8"),
        ],
    );
    assert!(
        scored.status.success(),
        "raw score failed: {}",
        String::from_utf8_lossy(&scored.stderr)
    );
    let stdout = String::from_utf8(scored.stdout).expect("score output is UTF-8");
    assert!(stdout.contains(&format!("raw_capture={}", raw.display())));
    assert!(stdout.contains("raw_capture_sha256="));
    assert!(stdout.contains(&format!("capture_sidecar={}", sidecar.display())));
    assert!(stdout.contains("capture_sidecar_sha256="));
    assert!(stdout.contains(&format!("report={}", report.display())));

    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&report).expect("report reads")).unwrap();
    assert_eq!(value["criteria"]["external_evidence_binding"], "pass");
    assert_eq!(value["criteria"]["logic_analyzer_timing"], "fail");
    assert_eq!(value["verdict"], "fail");
    assert!(value["trace"]["metrics"]["raw_samples"].as_u64().unwrap() > 0);

    let mut changed = fs::read(&raw).unwrap();
    changed[0] ^= 1;
    fs::write(&raw, changed).unwrap();
    let mismatch_report = manifest.with_extension("gate0-mismatch-report.json");
    let mismatch = run_evidence_bound(
        "score-gate0a",
        &manifest,
        &manifest,
        &[
            session.to_str().expect("path is UTF-8"),
            raw.to_str().expect("path is UTF-8"),
            sidecar.to_str().expect("path is UTF-8"),
            mismatch_report.to_str().expect("path is UTF-8"),
        ],
    );
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("hash does not match"));
    assert!(!mismatch_report.exists());
}

fn write_gate0_cli_evidence(manifest: &Path, session: &Path) -> (PathBuf, PathBuf) {
    let mut words = vec![SDA_MASK | SCL_MASK; 9];
    let mut state = SDA_MASK | SCL_MASK;
    append_gate0_marker(&mut words, &mut state, D8_MASK, 40);
    words.extend(std::iter::repeat_n(state, 39));
    append_gate0_marker(&mut words, &mut state, D9_MASK, 40);
    words.extend(std::iter::repeat_n(state, 40));
    let raw_bytes = words
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();

    let raw = manifest.with_extension("gate0.raw");
    let sidecar = manifest.with_extension("gate0-sidecar.json");
    let capture_binary = manifest.with_extension("gate0-capture-tool");
    let capture_log = manifest.with_extension("gate0-capture.log");
    let firmware = manifest.with_extension("gate0-firmware.bundle");
    let capture_binary_bytes = b"static patched capture tool";
    let firmware_bytes = b"deterministic device firmware bundle";
    let manifest_bytes = fs::read(manifest).expect("bound manifest reads");
    let session_bytes = fs::metadata(session)
        .expect("terminal session metadata reads")
        .len();
    let log = format!(
        concat!(
            "IKB_DSLOGIC_TRANSFER_V1 completed_transfers=1 anomalies=0\n",
            "IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fpga.fw bytes=341160 sha256=e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7\n",
            "IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fx2.fw bytes=8120 sha256=9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e\n",
            "IKB_GATE0_CAPTURE_V2 samples={} completed_transfers=1 anomalies=0 stop_observed_raw_bytes=14416000000 session_bytes={} locked_manifest_sha256={} firmware_bundle_sha256={}\n",
        ),
        words.len(),
        session_bytes,
        format_sha256(&sha256(&manifest_bytes)),
        format_sha256(&sha256(firmware_bytes)),
    );
    fs::write(&raw, &raw_bytes).unwrap();
    fs::write(&capture_binary, capture_binary_bytes).unwrap();
    fs::write(&capture_log, log.as_bytes()).unwrap();
    fs::write(&firmware, firmware_bytes).unwrap();

    let file_name = |path: &Path| {
        path.file_name()
            .expect("fixture has a file name")
            .to_str()
            .expect("file name is UTF-8")
            .to_owned()
    };
    let artifact = |path: &Path, bytes: &[u8]| {
        serde_json::json!({
            "path": file_name(path),
            "sha256": format_sha256(&sha256(bytes)),
            "bytes": bytes.len()
        })
    };
    let capture_command = format!(".{}.partial", file_name(&capture_binary));
    let raw_command_output = format!(".{}.partial", file_name(&raw));
    let value = serde_json::json!({
        "schema": GATE0_CAPTURE_SIDECAR_SCHEMA,
        "raw_capture_sha256": format_sha256(&sha256(&raw_bytes)),
        "raw_capture_bytes": raw_bytes.len(),
        "word_bytes": 2,
        "byte_order": "little",
        "sample_rate_hz": 2_000_000,
        "channel_map": {
            "int1": [0, 1, 2, 3, 4, 5],
            "d8": 6,
            "d9": 7,
            "sda": 8,
            "scl": 9
        },
        "voltage_threshold_low_mv": 1_400,
        "voltage_threshold_high_mv": 3_600,
        "requested_samples": words.len(),
        "capture_started_unix_ms": 1_000,
        "capture_finished_unix_ms": 3_611_000,
        "capture_binary": artifact(&capture_binary, capture_binary_bytes),
        "capture_log": artifact(&capture_log, log.as_bytes()),
        "device_firmware_bundle": artifact(&firmware, firmware_bytes),
        "driver_base_commit": "0bc2487778e660f4d3116729b6f4aee2b1996bb0",
        "sigrok_cli_base_commit": "f44dd91347e7ac797cefc23162b9fcf0b7329f1f",
        "driver_patch_sha256": format_sha256(&sha256(b"fixture patch")),
        "device_model": "DSLogic Plus",
        "capture_exit_status": 0,
        "command": [
            capture_command,
            "--driver", "dreamsourcelab-dslogic",
            "--channels", "0,1,2,3,4,5,6,7,8,9",
            "--config", "samplerate=2m:voltage_threshold=1.4-3.6:continuous=true",
            "--samples", words.len().to_string(),
            "--output-format", "binary",
            "--output-file", raw_command_output,
            "--loglevel", "4"
        ]
    });
    fs::write(&sidecar, serde_json::to_vec(&value).unwrap()).unwrap();
    (raw, sidecar)
}

const D8_MASK: u16 = 1 << 6;
const D9_MASK: u16 = 1 << 7;
const SDA_MASK: u16 = 1 << 8;
const SCL_MASK: u16 = 1 << 9;

fn append_gate0_marker(words: &mut Vec<u16>, state: &mut u16, mask: u16, width: usize) {
    *state |= mask;
    words.push(*state);
    words.extend(std::iter::repeat_n(*state, width - 1));
    *state &= !mask;
    words.push(*state);
}

#[test]
fn session_fixture_round_trips_through_cli_file_io() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_manifest(&test_directory, "reference", 208);
    let manifest_text = manifest.to_str().expect("test path is UTF-8");
    let fixture = PathBuf::from(&test_directory)
        .join(format!("reference-session-{}.iks", std::process::id()));
    let fixture_text = fixture.to_str().expect("test path is UTF-8");

    let write = run(&["session-fixture", manifest_text, fixture_text]);
    let write_stdout = String::from_utf8(write.stdout).expect("fixture output is UTF-8");
    assert!(write.status.success());
    assert!(write_stdout.contains("records=64"));

    let verify = run_evidence_bound("verify-session", &manifest, &manifest, &[fixture_text]);
    let verify_stdout = String::from_utf8(verify.stdout).expect("verify output is UTF-8");
    assert!(verify.status.success());
    assert!(verify_stdout.contains("evidence=replay-tested"));
    assert!(verify_stdout.contains("total_records=64"));
    assert!(verify_stdout.contains("invalid_records=0"));
    assert!(verify_stdout.contains("left_received=32"));
    assert!(verify_stdout.contains("right_received=32"));

    let clock_epochs = PathBuf::from(&test_directory).join(format!(
        "reference-clock-epochs-{}.json",
        std::process::id()
    ));
    let clock_epochs_text = clock_epochs.to_str().expect("test path is UTF-8");
    let fit = run_evidence_bound(
        "fit-session-clocks",
        &manifest,
        &manifest,
        &[fixture_text, clock_epochs_text],
    );
    let fit_stdout = String::from_utf8(fit.stdout).expect("fit output is UTF-8");
    assert!(fit.status.success());
    assert!(fit_stdout.contains("clock_epochs=fitted"));
    let fitted = ClockEpochSet::from_json(&fs::read(clock_epochs).expect("epochs read"))
        .expect("CLI writes valid epochs");
    assert_eq!(fitted.epochs.len(), 2);
    assert_eq!(fitted.epochs[0].fit.report().sample_count, 32);
    assert_eq!(fitted.epochs[1].fit.report().sample_count, 32);
}

#[test]
fn two_hand_session_fixture_rejects_a_one_hand_manifest_before_file_creation() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let source = write_manifest(&test_directory, "one-hand-source", 208);
    let mut manifest = read_bound_manifest(&source);
    manifest.hands.pop();
    let manifest_path = PathBuf::from(&test_directory)
        .join(format!("one-hand-manifest-{}.json", std::process::id()));
    let manifest = write_bound_manifest(&manifest_path, manifest);
    let session_path =
        PathBuf::from(&test_directory).join(format!("one-hand-session-{}.iks", std::process::id()));

    let output = run(&[
        "session-fixture",
        manifest_path.to_str().expect("manifest path is UTF-8"),
        session_path.to_str().expect("session path is UTF-8"),
    ]);
    let stderr = String::from_utf8(output.stderr).expect("error is UTF-8");

    assert!(!output.status.success());
    assert!(stderr.contains("requires a two-hand manifest"));
    assert!(!session_path.exists());

    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us: manifest.nominal_period_us,
            manifest_sha256: manifest.sha256().expect("manifest hashes"),
            session_id: [0x31; 16],
        },
    )
    .expect("external session starts");
    for hand in [Hand::Left, Hand::Right] {
        writer
            .append_frame(ControllerStream::from(hand), &sensor_frame(hand, 0), 0)
            .expect("external frame writes");
    }
    let (invalid_session, _) = writer.finish().expect("external session finishes");
    fs::write(&session_path, invalid_session).expect("external invalid session writes");

    let verify = run_evidence_bound(
        "verify-session",
        &manifest_path,
        &manifest_path,
        &[session_path.to_str().expect("session path is UTF-8")],
    );
    let verify_stderr = String::from_utf8(verify.stderr).expect("error is UTF-8");
    assert!(!verify.status.success());
    assert!(verify_stderr.contains("undeclared Right hand"));
}

#[test]
fn delimited_stdin_capture_preserves_valid_and_invalid_candidates() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_manifest(&test_directory, "delimited", 208);
    let manifest_text = manifest.to_str().expect("test path is UTF-8");
    let session =
        PathBuf::from(test_directory).join(format!("stream-session-{}.iks", std::process::id()));
    let session_text = session.to_str().expect("test path is UTF-8");

    let mut stream = Vec::new();
    for sequence in [0_u32, 1, 3] {
        stream.extend(transport_frame(Hand::Left, sequence));
    }
    stream.extend([5, 1, 2, 0]);

    let capture = run_record_stream(
        manifest_text,
        session_text,
        &stream,
        &transport_frame(Hand::Right, 0),
    );
    let capture_stdout = String::from_utf8(capture.stdout).expect("capture output is UTF-8");
    assert!(
        capture.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&capture.stderr)
    );
    assert!(capture_stdout.contains("evidence=recorded"));
    assert!(capture_stdout.contains("total_records=5"));
    assert!(capture_stdout.contains("invalid_records=1"));
    assert!(capture_stdout.contains("oversized_candidates=0"));
    assert!(capture_stdout.contains("unterminated_candidates=0"));

    let verify = run_evidence_bound("verify-session", &manifest, &manifest, &[session_text]);
    let verify_stdout = String::from_utf8(verify.stdout).expect("verify output is UTF-8");
    assert!(verify.status.success());
    assert!(verify_stdout.contains("total_records=5"));
    assert!(verify_stdout.contains("invalid_records=1"));
    assert!(verify_stdout.contains("decoded_candidates=4"));
    assert!(verify_stdout.contains("malformed_candidates=1"));
    assert!(verify_stdout.contains("left_received=3"));
    assert!(verify_stdout.contains("right_received=1"));
    assert!(verify_stdout.contains("left_missing=1"));
}

#[test]
fn record_stream_finalizes_each_declared_single_hand_with_observed_boot_binding() {
    let directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    for (label, hand, other) in [
        ("raw-single-left", Hand::Left, Hand::Right),
        ("raw-single-right", Hand::Right, Hand::Left),
    ] {
        let expected = write_one_hand_manifest(&directory, label, hand);
        let template = expected.with_extension("template.json");
        let locked = expected.with_extension("locked.json");
        let session = expected.with_extension("iks");
        let device = expected.with_extension("cobs");
        write_template_from_manifest(path_text(&expected), &template);
        let stream = (0..3)
            .flat_map(|sequence| transport_frame(hand, sequence))
            .collect::<Vec<_>>();
        fs::write(&device, stream).expect("single-hand source writes");
        let capture = run_evidence_bound(
            "record-stream",
            &template,
            &template,
            &[path_text(&device), path_text(&locked), path_text(&session)],
        );
        assert!(
            capture.status.success(),
            "{}",
            String::from_utf8_lossy(&capture.stderr)
        );
        let stdout = String::from_utf8(capture.stdout).expect("capture output is UTF-8");
        let side = if hand == Hand::Left { "left" } else { "right" };
        assert!(stdout.contains(&format!("{side}_device={}", path_text(&device))));
        assert!(stdout.contains("controller_identity=locked"));
        assert_eq!(fs::read(&locked).unwrap(), fs::read(&expected).unwrap());
        let finalized = FinalizedSession::audit(fs::read(&session).unwrap())
            .expect("single-hand capture has a complete strict footer");
        let manifest = read_bound_manifest(&expected);
        assert_eq!(
            finalized.header().manifest_sha256,
            manifest.sha256().unwrap()
        );
        assert_eq!(finalized.summary().total_records, 3);
        assert_eq!(finalized.summary().invalid_records, 0);
        assert_eq!(finalized.summary().quiesce_requests, 0);
        assert_eq!(finalized.summary().quiesce_receipts, 0);
        let (declared, absent) = if hand == Hand::Left {
            (finalized.summary().left, finalized.summary().right)
        } else {
            (finalized.summary().right, finalized.summary().left)
        };
        assert_eq!(declared.received, 3);
        assert_eq!(absent.received, 0);
        let verified =
            run_evidence_bound("verify-session", &locked, &template, &[path_text(&session)]);
        assert!(
            verified.status.success(),
            "{}",
            String::from_utf8_lossy(&verified.stderr)
        );

        // Binding follows the template even when only the right hand exists.
        let rejected_locked = expected.with_extension("wrong-hand.locked.json");
        let rejected_session = expected.with_extension("wrong-hand.iks");
        fs::write(&device, transport_frame(other, 0)).expect("wrong-hand source writes");
        let rejected = run_evidence_bound(
            "record-stream",
            &template,
            &template,
            &[
                path_text(&device),
                path_text(&rejected_locked),
                path_text(&rejected_session),
            ],
        );
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("identity bootstrap"));
        assert!(!rejected_locked.exists());
        assert!(!rejected_session.exists());
    }
}

#[test]
fn record_stream_rejects_device_count_mismatch_and_duplicate_sources_without_outputs() {
    let directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    for single in [false, true] {
        let manifest = if single {
            write_one_hand_manifest(&directory, "raw-count-one", Hand::Right)
        } else {
            write_manifest(&directory, "raw-count-two", 208)
        };
        let template = manifest.with_extension("template.json");
        let locked = manifest.with_extension("locked.json");
        let session = manifest.with_extension("iks");
        let first = manifest.with_extension("first.cobs");
        let second = manifest.with_extension("second.cobs");
        write_template_from_manifest(path_text(&manifest), &template);
        fs::write(&first, transport_frame(Hand::Left, 0)).unwrap();
        fs::write(&second, transport_frame(Hand::Right, 0)).unwrap();
        let devices = if single {
            vec![path_text(&first), path_text(&second)]
        } else {
            vec![path_text(&first)]
        };
        let mut operands = devices;
        operands.extend([path_text(&locked), path_text(&session)]);
        let rejected = run_evidence_bound("record-stream", &template, &template, &operands);
        assert!(!rejected.status.success());
        assert!(
            String::from_utf8_lossy(&rejected.stderr)
                .contains("device(s) in template controller order")
        );
        assert!(!locked.exists());
        assert!(!session.exists());
        if !single {
            let reversed = run_evidence_bound(
                "record-stream",
                &template,
                &template,
                &[
                    path_text(&second),
                    path_text(&first),
                    path_text(&locked),
                    path_text(&session),
                ],
            );
            assert!(!reversed.status.success());
            assert!(
                String::from_utf8_lossy(&reversed.stderr).contains("supplied a frame claiming")
            );
            assert!(!locked.exists());
            assert!(!session.exists());
        }

        let duplicate = run_evidence_bound(
            "record-stream",
            &template,
            &template,
            &[
                path_text(&first),
                path_text(&first),
                path_text(&locked),
                path_text(&session),
            ],
        );
        assert!(!duplicate.status.success());
        assert!(String::from_utf8_lossy(&duplicate.stderr).contains("distinct sources"));
        assert!(!locked.exists());
        assert!(!session.exists());
        #[cfg(unix)]
        {
            let alias = manifest.with_extension("alias.cobs");
            std::os::unix::fs::symlink(&first, &alias).unwrap();
            let duplicate = run_evidence_bound(
                "record-stream",
                &template,
                &template,
                &[
                    path_text(&first),
                    path_text(&alias),
                    path_text(&locked),
                    path_text(&session),
                ],
            );
            assert!(!duplicate.status.success());
            assert!(String::from_utf8_lossy(&duplicate.stderr).contains("distinct sources"));
            assert!(!locked.exists());
            assert!(!session.exists());
        }
    }
}

#[test]
fn record_stream_locks_wire_boots_into_the_manifest_before_recording() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let expected = write_manifest(&test_directory, "bootstrap-lock", 208);
    let template = expected.with_extension("template.json");
    let locked = expected.with_extension("observed.json");
    let session = expected.with_extension("iks");
    write_template_from_manifest(expected.to_str().expect("path is UTF-8"), &template);

    let capture = run_record_stream_paths(
        template.to_str().expect("path is UTF-8"),
        locked.to_str().expect("path is UTF-8"),
        session.to_str().expect("path is UTF-8"),
        &transport_frame(Hand::Left, 0),
        &transport_frame(Hand::Right, 0),
    );
    let stdout = String::from_utf8(capture.stdout).expect("output is UTF-8");

    assert!(
        capture.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&capture.stderr)
    );
    assert!(stdout.contains("controller_identity=locked"));
    assert_eq!(
        fs::read(&locked).expect("locked manifest reads"),
        fs::read(&expected).expect("expected manifest reads")
    );
    let verify = run_evidence_bound(
        "verify-session",
        &locked,
        &template,
        &[session.to_str().expect("path is UTF-8")],
    );
    assert!(verify.status.success());
}

#[test]
fn identity_bootstrap_rejects_the_wrong_controller_without_outputs() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let expected = write_manifest(&test_directory, "wrong-controller", 208);
    let template = expected.with_extension("template.json");
    let locked = expected.with_extension("observed.json");
    let session = expected.with_extension("iks");
    write_template_from_manifest(expected.to_str().expect("path is UTF-8"), &template);
    let mut wrong = sensor_frame(Hand::Left, 0);
    wrong.controller_id_sha256 = [0x5a; 32];

    let capture = run_record_stream_paths(
        template.to_str().expect("path is UTF-8"),
        locked.to_str().expect("path is UTF-8"),
        session.to_str().expect("path is UTF-8"),
        &encoded_transport_frame(&wrong),
        &transport_frame(Hand::Right, 0),
    );
    let stderr = String::from_utf8(capture.stderr).expect("error is UTF-8");

    assert!(!capture.status.success());
    assert!(stderr.contains("identity bootstrap Left controller does not match the template"));
    assert!(!locked.exists());
    assert!(!session.exists());
}

#[test]
fn identity_bootstrap_rejects_a_boot_change_without_outputs() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let expected = write_manifest(&test_directory, "boot-change", 208);
    let template = expected.with_extension("template.json");
    let locked = expected.with_extension("observed.json");
    let session = expected.with_extension("iks");
    write_template_from_manifest(expected.to_str().expect("path is UTF-8"), &template);
    let mut changed = sensor_frame(Hand::Left, 1);
    changed.boot_id = [0x33; 16];
    let mut left = transport_frame(Hand::Left, 0);
    left.extend(encoded_transport_frame(&changed));

    let capture = run_record_stream_paths(
        template.to_str().expect("path is UTF-8"),
        locked.to_str().expect("path is UTF-8"),
        session.to_str().expect("path is UTF-8"),
        &left,
        &transport_frame(Hand::Right, 0),
    );
    let stderr = String::from_utf8(capture.stderr).expect("error is UTF-8");

    assert!(!capture.status.success());
    assert!(stderr.contains("identity bootstrap Left controller changed boot identity"));
    assert!(!locked.exists());
    assert!(!session.exists());
}

#[test]
fn stream_capture_persists_oversized_and_unterminated_provenance() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_manifest(&test_directory, "faulted", 208);
    let manifest_text = manifest.to_str().expect("test path is UTF-8");
    let session = PathBuf::from(test_directory)
        .join(format!("faulted-stream-session-{}.iks", std::process::id()));
    let session_text = session.to_str().expect("test path is UTF-8");
    let mut stream = transport_frame(Hand::Left, 0);
    stream.extend(vec![1_u8; MAX_ENCODED_FRAME_LEN + 7]);
    stream.push(0);
    stream.extend([3, 1]);

    let capture = run_record_stream(
        manifest_text,
        session_text,
        &stream,
        &transport_frame(Hand::Right, 0),
    );
    let capture_stdout = String::from_utf8(capture.stdout).expect("capture output is UTF-8");
    assert!(
        capture.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&capture.stderr)
    );
    assert!(capture_stdout.contains("total_records=4"));
    assert!(capture_stdout.contains("invalid_records=2"));
    assert!(capture_stdout.contains("oversized_candidates=1"));
    assert!(capture_stdout.contains("unterminated_candidates=1"));

    let verify = run_evidence_bound("verify-session", &manifest, &manifest, &[session_text]);
    let verify_stdout = String::from_utf8(verify.stdout).expect("verify output is UTF-8");
    assert!(verify.status.success());
    assert!(verify_stdout.contains("total_records=4"));
    assert!(verify_stdout.contains("invalid_records=2"));
    assert!(verify_stdout.contains("oversized_candidates=1"));
    assert!(verify_stdout.contains("unterminated_candidates=1"));
}

#[test]
fn capture_commands_never_overwrite_an_existing_session() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_manifest(&test_directory, "overwrite", 208);
    let manifest_text = manifest.to_str().expect("test path is UTF-8");
    let sentinel = b"existing evidence";

    for command in ["session-fixture", "record-stream"] {
        let path = PathBuf::from(&test_directory)
            .join(format!("existing-{command}-{}.iks", std::process::id()));
        fs::write(&path, sentinel).expect("sentinel writes");
        let path_text = path.to_str().expect("test path is UTF-8");
        let output = if command == "record-stream" {
            run_record_stream(manifest_text, path_text, &[], &[])
        } else {
            run(&[command, manifest_text, path_text])
        };

        assert!(!output.status.success(), "{command} must reject the path");
        assert_eq!(
            fs::read(&path).expect("sentinel remains readable"),
            sentinel
        );
    }

    let one_hand_manifest = write_one_hand_manifest(&test_directory, "hand-overwrite", Hand::Left);
    let template = one_hand_manifest.with_extension("template.json");
    write_template_from_manifest(
        one_hand_manifest.to_str().expect("test path is UTF-8"),
        &template,
    );
    for occupied_output in ["locked-manifest", "session"] {
        let locked_manifest = PathBuf::from(&test_directory).join(format!(
            "existing-record-hand-{occupied_output}-locked-{}.json",
            std::process::id()
        ));
        let session = PathBuf::from(&test_directory).join(format!(
            "existing-record-hand-{occupied_output}-session-{}.iks",
            std::process::id()
        ));
        let occupied = if occupied_output == "locked-manifest" {
            &locked_manifest
        } else {
            &session
        };
        let unoccupied = if occupied_output == "locked-manifest" {
            &session
        } else {
            &locked_manifest
        };
        fs::write(occupied, sentinel).expect("record-hand sentinel writes");

        let output = run_record_hand_paths(
            template.to_str().expect("template path is UTF-8"),
            locked_manifest
                .to_str()
                .expect("locked manifest path is UTF-8"),
            session.to_str().expect("session path is UTF-8"),
            &transport_frame(Hand::Left, 0),
        );

        assert!(
            !output.status.success(),
            "record-hand must reject {occupied_output}"
        );
        assert_eq!(
            fs::read(occupied).expect("record-hand sentinel remains readable"),
            sentinel
        );
        assert!(
            !unoccupied.exists(),
            "record-hand creates no partial output"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn planned_signals_finalize_an_open_stream_capture() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let manifest = write_manifest(&test_directory, "planned-signal-capture", 208);
    let manifest_text = manifest.to_str().expect("test path is UTF-8");
    let template = manifest.with_extension("template.json");
    write_template_from_manifest(manifest_text, &template);

    for signal in PlannedProcessSignal::ALL {
        let label = format!("{}-capture", signal.fixture_label());
        let session = PathBuf::from(&test_directory)
            .join(format!("{label}-stream-session-{}.iks", std::process::id()));
        let session_text = session.to_str().expect("test path is UTF-8");
        let locked_manifest = session.with_extension("locked-manifest.json");
        let left_device = session.with_extension("left-fifo");
        let right_device = session.with_extension("right-fifo");
        let fifo_status = Command::new("mkfifo")
            .args([&left_device, &right_device])
            .status()
            .expect("mkfifo runs");
        assert!(fifo_status.success(), "{label} source FIFOs are created");
        let evidence = manifest_evidence_files(&template);
        let child = command_binary(
            "INVISIBLE_KEYBOARD_RLOCATION",
            &[
                "record-stream",
                template.to_str().expect("template path is UTF-8"),
                path_text(&evidence.rig),
                path_text(&evidence.axis),
                left_device.to_str().expect("left FIFO path is UTF-8"),
                right_device.to_str().expect("right FIFO path is UTF-8"),
                locked_manifest
                    .to_str()
                    .expect("locked manifest path is UTF-8"),
                session_text,
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("capture process starts");
        let mut left_input = fs::OpenOptions::new()
            .write(true)
            .open(&left_device)
            .expect("left FIFO pairs with the reader");
        let mut right_input = fs::OpenOptions::new()
            .write(true)
            .open(&right_device)
            .expect("right FIFO pairs with the reader");

        for (hand, input) in [
            (Hand::Left, &mut left_input),
            (Hand::Right, &mut right_input),
        ] {
            input
                .write_all(&transport_frame(hand, 0))
                .expect("one frame reaches each controller reader");
            input.flush().expect("controller input flushes");
        }

        assert!(
            wait_until(Duration::from_secs(2), || session.exists()),
            "{label} capture creates its exclusive destination"
        );

        send_planned_signal(&child, signal);
        drop(left_input);
        drop(right_input);

        let completed = child.wait_with_output().expect("capture process exits");
        let stderr = String::from_utf8(completed.stderr).expect("error is UTF-8");
        assert!(
            completed.status.success(),
            "graceful {label} failed: {stderr}"
        );

        let verify = run_evidence_bound(
            "verify-session",
            &locked_manifest,
            &template,
            &[session_text],
        );
        let verify_stdout = String::from_utf8(verify.stdout).expect("verify output is UTF-8");
        assert!(verify.status.success(), "{label} session must replay");
        assert!(verify_stdout.contains("total_records=2"));
        assert!(verify_stdout.contains("invalid_records=0"));
    }
}

#[test]
fn verification_rejects_a_different_acquisition_manifest() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let recorded_manifest = write_manifest(&test_directory, "recorded", 208);
    let changed_manifest = write_manifest(&test_directory, "changed", 208);
    let mut changed = read_bound_manifest(&changed_manifest);
    changed.study_id = "different-valid-study".to_owned();
    rewrite_manifest_with_existing_evidence(&changed_manifest, &changed);
    let recorded_text = recorded_manifest.to_str().expect("test path is UTF-8");
    let session = PathBuf::from(&test_directory)
        .join(format!("manifest-bound-session-{}.iks", std::process::id()));
    let session_text = session.to_str().expect("test path is UTF-8");

    let capture = run_record_stream(
        recorded_text,
        session_text,
        &transport_frame(Hand::Left, 0),
        &transport_frame(Hand::Right, 0),
    );
    assert!(
        capture.status.success(),
        "capture failed: {}",
        String::from_utf8_lossy(&capture.stderr)
    );

    let mismatch = run_evidence_bound(
        "verify-session",
        &changed_manifest,
        &changed_manifest,
        &[session_text],
    );
    let stderr = String::from_utf8(mismatch.stderr).expect("error is UTF-8");
    assert!(!mismatch.status.success());
    assert!(stderr.contains("manifest hash does not match session header"));
}

#[test]
fn capture_fails_before_creating_output_when_template_is_missing_or_invalid() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let missing = PathBuf::from(&test_directory).join("missing-manifest.json");
    let invalid = PathBuf::from(&test_directory).join("invalid-manifest.json");
    fs::write(&invalid, b"{}").expect("invalid fixture writes");
    let evidence_manifest = write_manifest(&test_directory, "invalid-template-evidence", 208);
    let source_evidence = manifest_evidence_files(&evidence_manifest);

    for template in [&missing, &invalid] {
        let target_evidence = manifest_evidence_files(template);
        fs::copy(&source_evidence.rig, target_evidence.rig)
            .expect("valid rig evidence copies beside malformed template");
        fs::copy(&source_evidence.axis, target_evidence.axis)
            .expect("valid axis characterization copies beside malformed template");
    }

    for (label, template) in [("missing", missing), ("invalid", invalid)] {
        let session = PathBuf::from(&test_directory).join(format!(
            "{label}-manifest-session-{}.iks",
            std::process::id()
        ));
        let locked_manifest = session.with_extension("locked.json");
        let output = run_record_stream_paths(
            template.to_str().expect("test path is UTF-8"),
            locked_manifest.to_str().expect("test path is UTF-8"),
            session.to_str().expect("test path is UTF-8"),
            &[],
            &[],
        );
        assert!(!output.status.success());
        assert!(
            !session.exists(),
            "invalid provenance must not create evidence"
        );
        assert!(!locked_manifest.exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn sparse_oversized_json_and_gate0_session_fail_before_allocation() {
    use std::os::unix::fs::MetadataExt;

    const SPARSE_BYTES: u64 = 1_u64 << 40;

    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let sparse_manifest = test_directory.join(format!(
        "sparse-oversized-manifest-{}.json",
        std::process::id()
    ));
    File::create(&sparse_manifest)
        .expect("sparse manifest creates")
        .set_len(SPARSE_BYTES)
        .expect("sparse manifest length sets");
    assert!(
        sparse_manifest
            .metadata()
            .expect("sparse manifest metadata reads")
            .blocks()
            * 512
            < 1_048_576,
        "the sparse fixture must not allocate its claimed terabyte"
    );
    let session_output = sparse_manifest.with_extension("iks");
    let rejected = run(&[
        "session-fixture",
        sparse_manifest
            .to_str()
            .expect("sparse manifest path is UTF-8"),
        session_output
            .to_str()
            .expect("session output path is UTF-8"),
    ]);
    let stderr = String::from_utf8(rejected.stderr).expect("manifest error is UTF-8");
    assert!(!rejected.status.success());
    assert!(stderr.contains(&format!(
        "is {SPARSE_BYTES} bytes; maximum is {MAX_MANIFEST_BYTES}"
    )));
    assert!(!session_output.exists());

    let manifest = write_manifest(test_directory.as_os_str(), "sparse-gate0-session", 208);
    let sparse_session = test_directory.join(format!(
        "sparse-oversized-gate0-session-{}.iks",
        std::process::id()
    ));
    File::create(&sparse_session)
        .expect("sparse Gate 0A session creates")
        .set_len(SPARSE_BYTES)
        .expect("sparse Gate 0A session length sets");
    assert!(
        sparse_session
            .metadata()
            .expect("sparse Gate 0A metadata reads")
            .blocks()
            * 512
            < 1_048_576,
        "the sparse Gate 0A fixture must not allocate its claimed terabyte"
    );
    let report_output = sparse_session.with_extension("report.json");
    let raw_capture = sparse_session.with_extension("raw");
    let capture_sidecar = sparse_session.with_extension("sidecar.json");
    let rejected = run_evidence_bound(
        "score-gate0a",
        &manifest,
        &manifest,
        &[
            sparse_session
                .to_str()
                .expect("sparse session path is UTF-8"),
            raw_capture.to_str().expect("raw path is UTF-8"),
            capture_sidecar.to_str().expect("sidecar path is UTF-8"),
            report_output.to_str().expect("report output path is UTF-8"),
        ],
    );
    let stderr = String::from_utf8(rejected.stderr).expect("Gate 0A error is UTF-8");
    assert!(!rejected.status.success());
    assert!(stderr.contains(&format!(
        "Gate 0A session is {SPARSE_BYTES} bytes; maximum for the 3610-second acquisition contract is {MAX_GATE0A_SESSION_BYTES}"
    )));
    assert!(!report_output.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn generic_clock_fit_streams_a_healthy_session_beyond_the_setup_cap() {
    const RECORD_COUNT: u32 = 482_501;
    const MAX_RSS_BYTES: u64 = 64 * 1_024 * 1_024;
    const MIN_SOURCE_THROUGHPUT_BYTES_PER_SECOND: f64 = 2.0 * 1_024.0 * 1_024.0;

    fn memory_status(process_id: u32) -> Option<(u64, String)> {
        let status = fs::read_to_string(format!("/proc/{process_id}/status")).ok()?;
        let peak_rss = ["VmHWM:", "VmRSS:"].into_iter().find_map(|field| {
            status.lines().find_map(|line| {
                let value = line.strip_prefix(field)?.trim();
                value
                    .strip_suffix(" kB")?
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .and_then(|kibibytes| kibibytes.checked_mul(1_024))
            })
        })?;
        Some((peak_rss, status))
    }

    fn memory_breach_diagnostics(process_id: u32, status: &str) -> String {
        let process_path = format!("/proc/{process_id}");
        let executable = fs::read_link(format!("{process_path}/exe"));
        let executable_sha256 = executable.as_ref().ok().and_then(|path| {
            fs::read(path)
                .ok()
                .map(|bytes| format_sha256(&sha256(&bytes)))
        });
        let selected_environment = fs::read(format!("{process_path}/environ"))
            .ok()
            .map(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .filter_map(|entry| std::str::from_utf8(entry).ok())
                    .filter(|entry| {
                        [
                            "LD_PRELOAD=",
                            "GLIBC_TUNABLES=",
                            "MALLOC_",
                            "RUSTFLAGS=",
                            "ASAN_OPTIONS=",
                            "LSAN_OPTIONS=",
                        ]
                        .iter()
                        .any(|prefix| entry.starts_with(prefix))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        format!(
            "status:\n{status}\nsmaps_rollup:\n{}\nmaps:\n{}\nstat:\n{}\ncgroup:\n{}\nexe={executable:?}\nexe_sha256={executable_sha256:?}\nselected_environment:\n{selected_environment}\nkernel:\n{}",
            fs::read_to_string(format!("{process_path}/smaps_rollup")).unwrap_or_default(),
            fs::read_to_string(format!("{process_path}/maps")).unwrap_or_default(),
            fs::read_to_string(format!("{process_path}/stat")).unwrap_or_default(),
            fs::read_to_string(format!("{process_path}/cgroup")).unwrap_or_default(),
            fs::read_to_string("/proc/version").unwrap_or_default(),
        )
    }

    let test_directory =
        PathBuf::from(std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory"));
    let manifest_path = write_manifest(test_directory.as_os_str(), "long-streaming-fit", 208);
    let mut manifest = read_bound_manifest(&manifest_path);
    manifest.hands.truncate(1);
    let manifest = write_bound_manifest(&manifest_path, manifest);
    let identity = manifest.hands[0]
        .frame_identity()
        .expect("fixture hand has a wire identity");
    let session_path = test_directory.join(format!(
        "long-streaming-fit-session-{}.iks",
        std::process::id()
    ));
    let session_file = File::create(&session_path).expect("long session creates");
    let mut writer = SessionWriter::new(
        std::io::BufWriter::with_capacity(64 * 1_024, session_file),
        SessionHeader {
            nominal_period_us: manifest.nominal_period_us,
            manifest_sha256: manifest.sha256().expect("manifest hashes"),
            session_id: [0x91; 16],
        },
    )
    .expect("long session starts");
    for sequence in 0..RECORD_COUNT {
        let device_time_us = u64::from(sequence) * u64::from(manifest.nominal_period_us);
        writer
            .append_frame(
                ControllerStream::Left,
                &SensorFrame {
                    hand: Hand::Left,
                    controller_id_sha256: identity.controller_id_sha256,
                    boot_id: identity.boot_id,
                    flags: 0,
                    valid_sensor_mask: ALL_SENSORS_VALID,
                    sequence,
                    device_time_us,
                    sensors: [SensorReading::default(); SENSOR_COUNT],
                },
                5_000_000_000 + device_time_us * 1_000,
            )
            .expect("long-session frame writes");
    }
    let (session_output, summary) = writer.finish().expect("long session finishes");
    session_output
        .get_ref()
        .sync_all()
        .expect("long session syncs");
    assert_eq!(summary.total_records, u64::from(RECORD_COUNT));
    let session_bytes = session_path
        .metadata()
        .expect("long session metadata reads")
        .len();
    assert!(
        session_bytes > MAX_SETUP_SESSION_BYTES,
        "generic test session must exceed the setup-only cap"
    );

    let verified = run_evidence_bound(
        "verify-session",
        &manifest_path,
        &manifest_path,
        &[session_path.to_str().expect("session path is UTF-8")],
    );
    assert!(
        verified.status.success(),
        "generic streaming audit rejected a healthy long session: {}",
        String::from_utf8_lossy(&verified.stderr)
    );

    let clocks_path = session_path.with_extension("clocks.json");
    let evidence = manifest_evidence_files(&manifest_path);
    let started = Instant::now();
    let mut child = command_binary(
        "INVISIBLE_KEYBOARD_RLOCATION",
        &[
            "fit-session-clocks",
            manifest_path.to_str().expect("manifest path is UTF-8"),
            path_text(&evidence.rig),
            path_text(&evidence.axis),
            session_path.to_str().expect("session path is UTF-8"),
            clocks_path.to_str().expect("clock output path is UTF-8"),
        ],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("streaming clock fit starts");
    let mut observed_peak_rss = 0_u64;
    let mut breach_diagnostics = String::new();
    loop {
        if let Some((rss, status)) = memory_status(child.id())
            && rss > observed_peak_rss
        {
            observed_peak_rss = rss;
            if rss > MAX_RSS_BYTES {
                breach_diagnostics = memory_breach_diagnostics(child.id(), &status);
            }
        }
        if child
            .try_wait()
            .expect("streaming clock fit polls")
            .is_some()
        {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let completed = child
        .wait_with_output()
        .expect("streaming clock fit output reads");
    let elapsed = started.elapsed();
    assert!(
        completed.status.success(),
        "streaming clock fit failed: {}",
        String::from_utf8_lossy(&completed.stderr)
    );
    assert!(
        observed_peak_rss > 0,
        "the Linux RSS probe observed the child"
    );
    assert!(
        observed_peak_rss <= MAX_RSS_BYTES,
        "streaming clock fit peak RSS was {observed_peak_rss} bytes; limit is {MAX_RSS_BYTES}\n{breach_diagnostics}"
    );
    let source_throughput = session_bytes as f64 / elapsed.as_secs_f64();
    let compact_spool_bytes = u64::from(RECORD_COUNT) * 16;
    let derived_io_bytes = session_bytes + compact_spool_bytes * 6;
    eprintln!(
        "streaming_resource_metrics session_bytes={session_bytes} raw_session_scans=1 compact_spool_bytes={compact_spool_bytes} compact_spool_passes=5 derived_io_bytes={derived_io_bytes} elapsed_ms={} peak_rss_bytes={observed_peak_rss} source_throughput_bytes_per_second={source_throughput:.0}",
        elapsed.as_millis()
    );
    assert!(
        source_throughput >= MIN_SOURCE_THROUGHPUT_BYTES_PER_SECOND,
        "streaming clock fit processed {source_throughput:.0} source bytes/s; minimum is {MIN_SOURCE_THROUGHPUT_BYTES_PER_SECOND:.0}"
    );
    let clocks = ClockEpochSet::from_json(&fs::read(&clocks_path).expect("clock output reads"))
        .expect("clock output parses");
    assert_eq!(
        clocks
            .fit_for(Hand::Left)
            .expect("left fit exists")
            .report()
            .sample_count,
        usize::try_from(RECORD_COUNT).expect("record count fits usize")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn calibrated_live_ingress_fault_retains_the_whole_chunk_after_clock_warmup() {
    use invisible_keyboard::{
        protocol::{FLAG_ACQUISITION_OVERRUN, decode_frame, encode_frame},
        session::SessionRecordKind,
    };

    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-ingress-acquisition-fault");
    let outputs = LiveCliOutputs::new(&test_directory, "ingress-acquisition-fault");
    let boots = [0x91, 0x92];
    let (mut child, mut left, mut right) =
        spawn_live_cli_with_mode(&fixture, &outputs, Some("recording"));
    let stream_start = Instant::now();
    let warmup_pairs =
        u32::try_from(MAX_ONLINE_CLOCK_QUALITY_WARMUP_US / RUNTIME_PAIR_PERIOD_US + 2).unwrap();
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [0, 0],
        warmup_pairs,
        stream_start,
        "ingress acquisition fault warmup",
    );
    let (mut next_sequences, prefix_len) = write_live_pairs_until_session_grows(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [warmup_pairs, warmup_pairs],
        stream_start,
        &outputs.session,
        u64::try_from(HEADER_LEN).unwrap(),
        "ingress acquisition fault warmup",
    );
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        next_sequences,
        24,
        stream_start,
        "ingress acquisition fault arming",
    );
    next_sequences[0] = next_sequences[0].wrapping_add(24);
    next_sequences[1] = next_sequences[1].wrapping_add(24);
    let (next_sequences, _) = write_live_pairs_until_session_grows(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        next_sequences,
        stream_start,
        &outputs.session,
        prefix_len,
        "ingress acquisition fault arming",
    );
    let mut fault = sensor_frame(Hand::Left, next_sequences[0]);
    fault.boot_id = [boots[0]; 16];
    fault.device_time_us = u64::try_from(stream_start.elapsed().as_micros()).unwrap();
    fault.flags = FLAG_ACQUISITION_OVERRUN;
    let fault_payload = encode_frame(&fault).unwrap().to_vec();
    let mut suffix = fault;
    suffix.sequence = suffix.sequence.wrapping_add(1);
    suffix.device_time_us += RUNTIME_PAIR_PERIOD_US;
    suffix.flags = 0;
    let suffix_payload = encode_frame(&suffix).unwrap().to_vec();
    let partial = vec![2, 3, 4];
    let mut chunk = encoded_transport_frame(&fault);
    chunk.extend(encoded_transport_frame(&suffix));
    chunk.extend_from_slice(&partial);
    left.write_all(&chunk)
        .expect("one incident chunk writes to the actual live source");
    left.flush().unwrap();
    // Keep both real source descriptors open. EOF must not become the first
    // cause before the CLI admits the faulty complete frame.
    wait_for_child(
        &mut child,
        Duration::from_secs(2),
        "live ingress acquisition fault",
    );
    let completed = child.wait_with_output().unwrap();
    assert!(!completed.status.success());
    let stdout = String::from_utf8(completed.stdout).unwrap();
    let stderr = String::from_utf8(completed.stderr).unwrap();
    assert!(
        stderr.contains("EvidenceInvalidatingAbstention(AcquisitionFault)"),
        "{stderr}"
    );
    assert!(
        stdout.contains("output_neutrality_confirmed=true"),
        "{stdout}"
    );
    assert!(
        stdout.contains("wear_session_confirmation=revoked"),
        "{stdout}"
    );
    let retained = assert_live_bundle(&fixture, &outputs, boots);
    let clock_report =
        LiveClockReport::from_json(&fs::read(&outputs.clock_report).unwrap()).unwrap();
    assert!(
        clock_report
            .operational_clocks
            .iter()
            .all(|clock| clock.fit.is_none()),
        "terminal acquisition fault must clear live clock authority"
    );
    let mut reader = retained.reader();
    let mut left_records = Vec::new();
    let mut right_frames = Vec::new();
    while let Some(record) = reader.next_record().unwrap() {
        if record.source == ControllerStream::Left {
            left_records.push((record.kind, record.host_time_ns, record.payload));
        } else {
            right_frames.push(decode_frame(&record.payload).unwrap());
        }
    }
    assert_eq!(
        left_records.len(),
        usize::try_from(next_sequences[0]).unwrap() + 3
    );
    assert!(left_records.len() - 3 >= usize::try_from(warmup_pairs).unwrap());
    assert!(right_frames.len() >= usize::try_from(warmup_pairs).unwrap());
    let left_first = decode_frame(&left_records[0].2).unwrap();
    let left_last = decode_frame(&left_records[left_records.len() - 4].2).unwrap();
    assert!(
        left_last.device_time_us - left_first.device_time_us >= MAX_ONLINE_CLOCK_QUALITY_WARMUP_US
    );
    assert!(
        right_frames.last().unwrap().device_time_us - right_frames.first().unwrap().device_time_us
            >= MAX_ONLINE_CLOCK_QUALITY_WARMUP_US
    );
    let incident = &left_records[left_records.len() - 3..];
    assert_eq!(incident[0].0, SessionRecordKind::Decoded);
    assert_eq!(incident[0].2, fault_payload);
    assert_eq!(incident[1].0, SessionRecordKind::Decoded);
    assert_eq!(incident[1].2, suffix_payload);
    assert_eq!(incident[2].0, SessionRecordKind::Unterminated);
    assert_eq!(incident[2].2, partial);
    assert_eq!(incident[0].1, incident[1].1);
    assert_eq!(incident[1].1, incident[2].1);
    assert_eq!(
        left_records
            .iter()
            .filter(|(_, _, payload)| *payload == incident[0].2)
            .count(),
        1
    );
    for (sequence, (kind, _, payload)) in left_records[..left_records.len() - 3].iter().enumerate()
    {
        assert_eq!(*kind, SessionRecordKind::Decoded);
        assert_eq!(
            decode_frame(payload).unwrap().sequence,
            u32::try_from(sequence).unwrap()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn calibrated_live_watchdog_revokes_during_continuous_empty_transport_traffic() {
    let test_directory = std::env::var_os("TEST_TMPDIR").expect("Bazel supplies a test directory");
    let fixture = LiveCliFixture::new(&test_directory, "live-empty-transport-watchdog");
    let outputs = LiveCliOutputs::new(&test_directory, "empty-transport-watchdog");
    let boots = [0xa1, 0xa2];
    let (mut child, mut left, mut right) =
        spawn_live_cli_with_mode(&fixture, &outputs, Some("recording"));
    let stream_start = Instant::now();
    let warmup_pairs =
        u32::try_from(MAX_ONLINE_CLOCK_QUALITY_WARMUP_US / RUNTIME_PAIR_PERIOD_US + 2).unwrap();
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [0, 0],
        warmup_pairs,
        stream_start,
        "empty transport watchdog warmup",
    );
    let (mut next_sequences, prefix_len) = write_live_pairs_until_session_grows(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        [warmup_pairs, warmup_pairs],
        stream_start,
        &outputs.session,
        u64::try_from(HEADER_LEN).unwrap(),
        "empty transport watchdog warmup",
    );
    write_live_runtime_pairs(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        next_sequences,
        24,
        stream_start,
        "empty transport watchdog arming",
    );
    next_sequences[0] = next_sequences[0].wrapping_add(24);
    next_sequences[1] = next_sequences[1].wrapping_add(24);
    write_live_pairs_until_session_grows(
        LiveRuntimeWriters {
            child: &mut child,
            left: &mut left,
            right: &mut right,
        },
        boots,
        next_sequences,
        stream_start,
        &outputs.session,
        prefix_len,
        "empty transport watchdog arming",
    );

    // Empty COBS delimiters keep both readers active without producing a frame.
    // Keep the descriptors open so EOF cannot supply the missing revocation.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut starved = false;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            starved = true;
            child.kill().unwrap();
            break;
        }
        if left.write_all(&[0; 64]).is_err() || right.write_all(&[0; 64]).is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    wait_for_child(
        &mut child,
        Duration::from_secs(2),
        "empty transport watchdog",
    );
    let completed = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(completed.stdout).unwrap();
    let stderr = String::from_utf8(completed.stderr).unwrap();
    assert!(
        !starved,
        "valid-frame watchdog starved by raw bytes\n{stdout}\n{stderr}"
    );
    assert!(!completed.status.success());
    assert!(stderr.contains("WatchdogTimeout"), "{stderr}");
    assert!(
        stdout.contains("output_neutrality_confirmed=true"),
        "{stdout}"
    );
    assert!(
        stdout.contains("wear_session_confirmation=revoked"),
        "{stdout}"
    );
    assert_live_bundle(&fixture, &outputs, boots);
}
