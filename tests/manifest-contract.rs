use invisible_keyboard::manifest::{
    ACQUISITION_MANIFEST_SCHEMA, ACQUISITION_TEMPLATE_SCHEMA, AcquisitionManifest,
    AcquisitionManifestTemplate, AxisCharacterizationReport, CANONICAL_ACQUISITION_PROFILE,
    CANONICAL_AXIS_CONVENTION, EvidenceArtifactError, EvidenceStatus, FrameCanonicalizationError,
    FrameCanonicalizer, HandManifest, ManifestError, ManifestHand, ObservedTopologyError,
    PositiveYDirection, RigEvidenceRecord, SensorManifest, SensorRole, SensorVector, format_id128,
    format_sha256, parse_id128, parse_sha256, sha256,
};
use invisible_keyboard::{
    layout::Hand,
    prompts::PromptCorpus,
    protocol::{ALL_SENSORS_VALID, FrameIdentity, SENSOR_COUNT, SensorFrame, SensorReading},
};

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SENSOR_ROLES: [&str; SENSOR_COUNT] = ["thumb", "index", "middle", "ring", "pinky", "dorsal"];

#[test]
fn fixed_width_lowercase_hex_identities_round_trip_and_reject_other_forms() {
    let hash = [0xab; 32];
    let id = [0xcd; 16];

    assert_eq!(parse_sha256(&format_sha256(&hash)), Some(hash));
    assert_eq!(parse_id128(&format_id128(&id)), Some(id));
    assert_eq!(parse_sha256(&format_sha256(&hash).to_uppercase()), None);
    assert_eq!(parse_id128("cd"), None);
}

fn lsm6_odr_hz(register: u8) -> u16 {
    match (register >> 4) & 0x0f {
        0b0101 => 208,
        bits => panic!("fixture does not define LSM6 ODR bits {bits:04b}"),
    }
}

fn lsm6_accelerometer_range_g(ctrl1_xl: u8) -> u16 {
    match (ctrl1_xl >> 2) & 0b11 {
        0b10 => 4,
        bits => panic!("fixture does not define LSM6 accelerometer range bits {bits:02b}"),
    }
}

fn lsm6_gyroscope_range_dps(ctrl2_g: u8) -> u16 {
    match (ctrl2_g >> 2) & 0b11 {
        0b10 => 1_000,
        bits => panic!("fixture does not define LSM6 gyroscope range bits {bits:02b}"),
    }
}

fn sensors_json(hand: &str, roles: &[&str]) -> String {
    roles
        .iter()
        .enumerate()
        .map(|(index, role)| {
            format!(
                r#"    {{
      "sensor_index": {index},
      "mux_channel": {index},
      "role": "{role}",
      "mount_id": "{hand}-{role}-mount-v1",
      "wiring_id": "{hand}-{role}-wiring-v1",
      "interrupt_pin": "{hand}-{role}-int1-v1",
      "axis_map": [1, 0, 0, 0, 1, 0, 0, 0, 1]
    }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n")
}

fn rig_evidence_json(status: &str) -> Vec<u8> {
    let was_run = status != "not-yet-run";
    let artifact = was_run.then_some(HASH_B);
    let sensor_masses = was_run.then_some(1_000_u32);
    let sensors = SENSOR_ROLES
        .iter()
        .enumerate()
        .map(|(sensor_index, role)| {
            serde_json::json!({
                "sensor_index": sensor_index,
                "mux_channel": sensor_index,
                "role": role,
                "mount_id": format!("left-{role}-mount-v1"),
                "wiring_id": format!("left-{role}-wiring-v1"),
                "interrupt_pin": format!("left-{role}-int1-v1"),
                "mass_milligrams": sensor_masses,
            })
        })
        .collect::<Vec<_>>();
    let fact = || {
        serde_json::json!({
            "status": status,
            "artifact_sha256": artifact,
        })
    };
    serde_json::to_vec(&serde_json::json!({
        "schema": "invisible-keyboard-rig-evidence-v1",
        "record_id": "manifest-contract-rig-001",
        "recorded_at_utc": "2026-08-30T00:00:00Z",
        "operator_id": "manifest-contract-operator-001",
        "controller_board": "Seeed XIAO nRF52840 Plus SKU 102010672",
        "sensor_model": "LSM6DS3TR-C",
        "hands": [{
            "hand": "left",
            "assembly_id": "left-assembly-001",
            "controller_id": "xiao-left-001",
            "total_mass_milligrams": was_run.then_some(12_000_u32),
            "mass_measurement": fact(),
            "sensors": sensors,
        }],
        "continuity": {
            "fact": fact(),
            "checked_path_count": was_run.then_some(36_u16),
        },
        "photos": {
            "fact": fact(),
            "photo_count": if was_run { 8 } else { 0 },
        },
        "power": {
            "fact": fact(),
            "supply_millivolts": was_run.then_some(3_300_u32),
            "idle_current_microamps": was_run.then_some(18_000_u32),
            "peak_current_microamps": was_run.then_some(48_000_u32),
        },
        "analyzer": {
            "fact": fact(),
            "sample_rate_hz": was_run.then_some(10_000_000_u32),
            "capture_duration_milliseconds": was_run.then_some(5_000_u32),
        },
        "surface": {
            "surface_id": "laminate-surface-001",
            "material": "high-pressure-laminate",
            "thickness_micrometers": was_run.then_some(12_000_u32),
            "fact": fact(),
        },
        "posture": {
            "posture_id": "seated-neutral-001",
            "description": "seated with supported forearms and neutral wrists",
            "fact": fact(),
        },
        "truth_device": {
            "device_id": "truth-device-001",
            "model": "independent-contact-array-001",
            "capture_method": "hardware-timestamped-contact-edges",
            "fact": fact(),
        },
        "alignment": {
            "fixture_id": "six-face-fixture-001",
            "maximum_error_millidegrees": was_run.then_some(1_500_u32),
            "fact": fact(),
        },
    }))
    .expect("rig evidence fixture serializes")
}

fn axis_characterization_json(candidate_hash: &str, rig_hash: &str) -> Vec<u8> {
    let face_means = serde_json::json!([
        {"face": "positive-x", "sample_count": 600, "mean_acceleration_lsb": [8192, 0, 0]},
        {"face": "negative-x", "sample_count": 600, "mean_acceleration_lsb": [-8192, 0, 0]},
        {"face": "positive-y", "sample_count": 600, "mean_acceleration_lsb": [0, 8192, 0]},
        {"face": "negative-y", "sample_count": 600, "mean_acceleration_lsb": [0, -8192, 0]},
        {"face": "positive-z", "sample_count": 600, "mean_acceleration_lsb": [0, 0, 8192]},
        {"face": "negative-z", "sample_count": 600, "mean_acceleration_lsb": [0, 0, -8192]},
    ]);
    let sensors = SENSOR_ROLES
        .iter()
        .enumerate()
        .map(|(sensor_index, role)| {
            serde_json::json!({
                "sensor_index": sensor_index,
                "role": role,
                "raw_face_means": face_means.clone(),
                "axis_map": [1, 0, 0, 0, 1, 0, 0, 0, 1],
                "observed_maximum_component_residual_lsb": 0,
                "passed": true,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&serde_json::json!({
        "schema": "invisible-keyboard-axis-characterization-v1",
        "report_id": "manifest-contract-axis-001",
        "completed_at_utc": "2026-08-30T00:00:00Z",
        "characterizer_build": "manifest-contract-characterizer-001",
        "method_id": "fixed-order-600-stationary-iks3-v1",
        "coordinate_convention": CANONICAL_AXIS_CONVENTION,
        "candidate_manifest_sha256": candidate_hash,
        "source_session_sha256": HASH_B,
        "rig_evidence_sha256": rig_hash,
        "criteria": {
            "expected_gravity_lsb": 8192,
            "maximum_component_residual_lsb": 820,
            "minimum_samples_per_face": 600,
        },
        "hands": [{
            "hand": "left",
            "positive_y_direction": "ulnar",
            "sensors": sensors,
        }],
        "passed": true,
    }))
    .expect("axis characterization fixture serializes")
}

fn manifest_json(sensor_odr_hz: u16) -> Vec<u8> {
    manifest_json_with_roles(sensor_odr_hz, &SENSOR_ROLES)
}

fn template_json(sensor_odr_hz: u16) -> Vec<u8> {
    String::from_utf8(manifest_json(sensor_odr_hz))
        .expect("fixture is UTF-8")
        .replace(ACQUISITION_MANIFEST_SCHEMA, ACQUISITION_TEMPLATE_SCHEMA)
        .replace(
            "11111111111111111111111111111111",
            "00000000000000000000000000000000",
        )
        .into_bytes()
}

fn manifest_json_with_roles(sensor_odr_hz: u16, roles: &[&str]) -> Vec<u8> {
    let sensors = sensors_json("left", roles);
    format!(
        r#"{{
  "schema": "invisible-keyboard-acquisition-manifest-v4",
  "layout_id": "invisible-keyboard-core28-v1",
  "protocol_version": 2,
  "hands": [
    {{
      "hand": "left",
      "controller_id": "xiao-left-001",
      "boot_id": "11111111111111111111111111111111",
      "sensors": [
{sensors}
      ]
    }}
  ],
  "controller_board": "Seeed XIAO nRF52840 Plus SKU 102010672",
  "firmware_build": "manifest-contract-firmware-build-001",
  "host_build": "manifest-contract-host-build-001",
  "sensor_model": "LSM6DS3TR-C",
  "sensors_per_hand": 6,
  "sensor_odr_hz": {sensor_odr_hz},
  "accelerometer_range_g": 4,
  "gyroscope_range_dps": 1000,
  "filter_profile": "gate0-raw-v1",
  "i2c_bus_hz": 400000,
  "nominal_period_us": 5000,
  "prompt_corpus_sha256": "{HASH_A}",
  "study_id": "gate0-stream-integrity",
  "split_id": "engineering-only",
  "participant_alias": "manifest-contract-participant-001",
  "consent_record": "self-test-2026-08-09",
  "retention_policy": "local-delete-after-analysis-v1",
  "rig_evidence_sha256": "{HASH_A}",
  "axis_characterization_sha256": "{HASH_A}",
  "calibration_artifact_sha256": null,
  "calibration_validation_sha256": null
}}
"#
    )
    .into_bytes()
}

fn manifest_json_with_evidence_hashes(rig_hash: &str, axis_hash: &str) -> Vec<u8> {
    String::from_utf8(manifest_json(208))
        .expect("manifest fixture is UTF-8")
        .replace(
            &format!("\"rig_evidence_sha256\": \"{HASH_A}\""),
            &format!("\"rig_evidence_sha256\": \"{rig_hash}\""),
        )
        .replace(
            &format!("\"axis_characterization_sha256\": \"{HASH_A}\""),
            &format!("\"axis_characterization_sha256\": \"{axis_hash}\""),
        )
        .into_bytes()
}

fn bound_artifacts() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    bound_artifacts_for_rig_status("passed")
}

fn bound_artifacts_for_rig_status(status: &str) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let rig_source = rig_evidence_json(status);
    let rig = RigEvidenceRecord::from_json(&rig_source).expect("rig evidence parses");
    let rig_hash = format_sha256(&rig.sha256().expect("rig evidence hashes"));

    let candidate_source = manifest_json_with_evidence_hashes(&rig_hash, HASH_A);
    let candidate =
        AcquisitionManifest::from_json(&candidate_source).expect("physical candidate parses");
    let candidate_hash = format_sha256(
        &candidate
            .axis_characterization_candidate_sha256()
            .expect("physical candidate hashes"),
    );

    let axis_source = axis_characterization_json(&candidate_hash, &rig_hash);
    let axis =
        AxisCharacterizationReport::from_json(&axis_source).expect("axis characterization parses");
    let axis_hash = format_sha256(&axis.sha256().expect("axis report hashes"));
    let manifest_source = manifest_json_with_evidence_hashes(&rig_hash, &axis_hash);
    (manifest_source, rig_source, axis_source)
}

fn bound_template_artifacts() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (manifest_source, rig_source, axis_source) = bound_artifacts();
    let template_source = String::from_utf8(manifest_source)
        .expect("bound manifest fixture is UTF-8")
        .replace(ACQUISITION_MANIFEST_SCHEMA, ACQUISITION_TEMPLATE_SCHEMA)
        .replace(
            "11111111111111111111111111111111",
            "00000000000000000000000000000000",
        )
        .into_bytes();
    (template_source, rig_source, axis_source)
}

fn commissioning_session(
    candidate: &invisible_keyboard::manifest::AxisCommissioningCandidate,
    matrix: [i8; 9],
    mut alter: impl FnMut(usize, &mut SensorFrame),
) -> invisible_keyboard::session::FinalizedSession {
    use invisible_keyboard::{
        axis_characterization::{RECORDS_PER_HAND, SAMPLES_PER_FACE},
        session::{SessionHeader, SessionWriter},
    };
    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us: 5000,
            manifest_sha256: candidate.sha256(),
            session_id: [0x73; 16],
        },
    )
    .unwrap();
    for (hand_index, (hand, controller)) in candidate.expected_controllers().into_iter().enumerate()
    {
        for ordinal in 0..RECORDS_PER_HAND {
            let face = (ordinal / SAMPLES_PER_FACE) % 6;
            let mut canonical = [0_i16; 3];
            canonical[face / 2] = if face.is_multiple_of(2) { 8192 } else { -8192 };
            // The inverse of an orthogonal signed permutation is its transpose.
            let raw = std::array::from_fn(|column| {
                (0..3)
                    .map(|row| i16::from(matrix[row * 3 + column]) * canonical[row])
                    .sum()
            });
            let mut frame = SensorFrame {
                hand,
                controller_id_sha256: controller,
                boot_id: [0x61; 16],
                flags: 0,
                valid_sensor_mask: ALL_SENSORS_VALID,
                sequence: ordinal as u32,
                device_time_us: 10000 + ordinal as u64 * 5000,
                sensors: [SensorReading {
                    acceleration: raw,
                    angular_velocity: [0; 3],
                    sample_time_offset_us: -100,
                }; SENSOR_COUNT],
            };
            alter(ordinal, &mut frame);
            writer
                .append_frame(
                    hand.into(),
                    &frame,
                    (hand_index * RECORDS_PER_HAND + ordinal) as u64 * 5_000_000,
                )
                .unwrap();
        }
    }
    writer.finish_finalized().unwrap()
}

#[test]
fn commissioning_bootstraps_without_axis_evidence_and_replays_into_operational_binding() {
    use invisible_keyboard::{
        axis_characterization::derive_axes, manifest::AxisCommissioningCandidate,
    };
    let (template, rig, _) = bound_template_artifacts();
    // No existing report is supplied or consulted by commissioning.
    let candidate = AxisCommissioningCandidate::from_json(&template, &rig).unwrap();
    let rotation = [0, 1, 0, -1, 0, 0, 0, 0, 1];
    let session = commissioning_session(&candidate, rotation, |_, _| {});
    let report = derive_axes(
        &candidate,
        &session,
        "2026-09-07T00:00:00Z",
        "simulated-test-reducer",
    )
    .unwrap();
    assert!(report.passed);
    assert_eq!(
        report.source_session_sha256,
        format_sha256(&session.sha256())
    );
    assert!(
        report.hands[0]
            .sensors
            .iter()
            .all(|sensor| sensor.axis_map == rotation)
    );
    assert_eq!(
        report,
        derive_axes(
            &candidate,
            &session,
            "2026-09-07T00:00:00Z",
            "simulated-test-reducer"
        )
        .unwrap()
    );
    let bound = candidate.bound_template_json(&report).unwrap();
    let operational = AcquisitionManifestTemplate::from_json_with_evidence(
        &bound,
        &rig,
        &report.canonical_json().unwrap(),
    )
    .unwrap()
    .lock(&[FrameIdentity {
        hand: Hand::Left,
        controller_id_sha256: sha256(b"xiao-left-001"),
        boot_id: [0x61; 16],
    }])
    .unwrap();
    assert_ne!(
        operational.sha256().unwrap(),
        session.header().manifest_sha256
    );
    assert_eq!(operational.hands[0].sensors[0].axis_map, rotation);
}

#[test]
fn commissioning_rejects_unperformed_rig_checks_and_exact_binding_changes() {
    use invisible_keyboard::manifest::AxisCommissioningCandidate;
    let (template, rig, _) = bound_template_artifacts();
    let mut changed: serde_json::Value = serde_json::from_slice(&template).unwrap();
    changed["hands"][0]["sensors"][0]["mount_id"] = "different-mounted-carrier".into();
    assert!(
        AxisCommissioningCandidate::from_json(&serde_json::to_vec(&changed).unwrap(), &rig)
            .is_err()
    );
    let (pending, not_run, _) = bound_artifacts_for_rig_status("not-yet-run");
    let mut pending: serde_json::Value = serde_json::from_slice(&pending).unwrap();
    pending["schema"] = ACQUISITION_TEMPLATE_SCHEMA.into();
    pending["hands"][0]["boot_id"] = "00000000000000000000000000000000".into();
    assert!(matches!(
        AxisCommissioningCandidate::from_json(&serde_json::to_vec(&pending).unwrap(), &not_run),
        Err(ManifestError::RigEvidenceNotPassed(_))
    ));
}

#[test]
fn commissioning_two_hand_dataset_preserves_order_and_anatomical_conventions() {
    use invisible_keyboard::{
        axis_characterization::derive_axes, manifest::AxisCommissioningCandidate,
    };
    let (template_source, rig_source, _) = bound_template_artifacts();
    let mut template: AcquisitionManifest = serde_json::from_slice(&template_source).unwrap();
    let mut rig = RigEvidenceRecord::from_json(&rig_source).unwrap();
    let mut right_template = template.hands[0].clone();
    right_template.hand = ManifestHand::Right;
    right_template.controller_id = "xiao-right-001".to_owned();
    for sensor in &mut right_template.sensors {
        sensor.mount_id = sensor.mount_id.replace("left-", "right-");
        sensor.wiring_id = sensor.wiring_id.replace("left-", "right-");
        sensor.interrupt_pin = sensor.interrupt_pin.replace("left-", "right-");
    }
    template.hands.push(right_template);
    let mut right_rig = rig.hands[0].clone();
    right_rig.hand = ManifestHand::Right;
    right_rig.controller_id = "xiao-right-001".to_owned();
    right_rig.assembly_id = "right-assembly-001".to_owned();
    for sensor in &mut right_rig.sensors {
        sensor.mount_id = sensor.mount_id.replace("left-", "right-");
        sensor.wiring_id = sensor.wiring_id.replace("left-", "right-");
        sensor.interrupt_pin = sensor.interrupt_pin.replace("left-", "right-");
    }
    rig.hands.push(right_rig);
    template.rig_evidence_sha256 = format_sha256(&rig.sha256().unwrap());
    let rig_source = rig.canonical_json().unwrap();
    let candidate =
        AxisCommissioningCandidate::from_json(&serde_json::to_vec(&template).unwrap(), &rig_source)
            .unwrap();
    let session = commissioning_session(&candidate, [0, 0, 1, 1, 0, 0, 0, 1, 0], |_, _| {});
    let report = derive_axes(
        &candidate,
        &session,
        "2026-09-07T00:00:00Z",
        "simulated-test-reducer",
    )
    .unwrap();
    assert_eq!(
        report
            .hands
            .iter()
            .map(|hand| (hand.hand, hand.positive_y_direction))
            .collect::<Vec<_>>(),
        vec![
            (ManifestHand::Left, PositiveYDirection::Ulnar),
            (ManifestHand::Right, PositiveYDirection::Radial)
        ]
    );
    AcquisitionManifestTemplate::from_json_with_evidence(
        &candidate.bound_template_json(&report).unwrap(),
        &rig_source,
        &report.canonical_json().unwrap(),
    )
    .unwrap();
}

#[test]
fn commissioning_rejects_raw_motion_identity_timing_and_reflection_faults() {
    use invisible_keyboard::{
        axis_characterization::derive_axes, manifest::AxisCommissioningCandidate,
    };
    let (template, rig, _) = bound_template_artifacts();
    let candidate = AxisCommissioningCandidate::from_json(&template, &rig).unwrap();
    let identity = [1, 0, 0, 0, 1, 0, 0, 0, 1];
    for fault in 0..9 {
        let session = commissioning_session(&candidate, identity, |ordinal, frame| {
            if ordinal != 1 {
                return;
            }
            match fault {
                0 => frame.sensors[0].angular_velocity[0] = 101,
                1 => frame.sensors[0].acceleration[0] += 821,
                2 => frame.sensors[0].acceleration[0] = i16::MAX,
                3 => frame.valid_sensor_mask = 0x1f,
                4 => frame.flags |= invisible_keyboard::protocol::FLAG_ACQUISITION_OVERRUN,
                5 => frame.sequence += 1,
                6 => frame.boot_id = [0x62; 16],
                7 => frame.sensors[0].sample_time_offset_us = -5100,
                8 => frame.flags = invisible_keyboard::protocol::FLAG_CLOCK_UNSYNCED,
                _ => unreachable!(),
            }
        });
        assert!(
            derive_axes(
                &candidate,
                &session,
                "2026-09-07T00:00:00Z",
                "simulated-test-reducer"
            )
            .is_err(),
            "fault {fault}"
        );
    }
    let reflected = commissioning_session(&candidate, [-1, 0, 0, 0, 1, 0, 0, 0, 1], |_, _| {});
    assert!(
        derive_axes(
            &candidate,
            &reflected,
            "2026-09-07T00:00:00Z",
            "simulated-test-reducer"
        )
        .is_err()
    );
}

#[test]
fn typed_manifest_round_trips_through_one_canonical_identity() {
    let source = manifest_json(208);
    let manifest = AcquisitionManifest::from_json(&source).expect("valid manifest parses");

    assert_eq!(manifest.schema, ACQUISITION_MANIFEST_SCHEMA);
    assert_eq!(manifest.hands.len(), 1);
    assert_eq!(manifest.sensor_odr_hz, 208);
    assert_eq!(manifest.hands[0].sensors.len(), 6);
    assert_eq!(
        manifest.hands[0]
            .sensors
            .iter()
            .map(|sensor| (sensor.sensor_index, sensor.role))
            .collect::<Vec<_>>(),
        vec![
            (0, SensorRole::Thumb),
            (1, SensorRole::Index),
            (2, SensorRole::Middle),
            (3, SensorRole::Ring),
            (4, SensorRole::Pinky),
            (5, SensorRole::Dorsal),
        ]
    );

    let canonical = manifest.canonical_json().expect("manifest serializes");
    let reparsed = AcquisitionManifest::from_json(&canonical).expect("canonical JSON parses");
    assert_eq!(manifest, reparsed);
    assert_eq!(
        manifest.sha256().expect("manifest hashes"),
        reparsed.sha256().expect("same manifest hashes")
    );
    assert!(!canonical.contains(&b'\n'));
}

#[test]
fn register_bits_and_manifest_declarations_share_one_canonical_profile() {
    let profile = CANONICAL_ACQUISITION_PROFILE;

    assert_eq!(profile.ctrl1_xl.address, 0x10);
    assert_eq!(profile.ctrl1_xl.value, 0x58);
    assert_eq!(profile.ctrl2_g.address, 0x11);
    assert_eq!(profile.ctrl2_g.value, 0x58);
    assert_eq!(lsm6_odr_hz(profile.ctrl1_xl.value), profile.sensor_odr_hz);
    assert_eq!(lsm6_odr_hz(profile.ctrl2_g.value), profile.sensor_odr_hz);
    assert_eq!(
        lsm6_accelerometer_range_g(profile.ctrl1_xl.value),
        profile.accelerometer_range_g
    );
    assert_eq!(
        lsm6_gyroscope_range_dps(profile.ctrl2_g.value),
        profile.gyroscope_range_dps
    );
    assert_eq!(profile.i2c_bus_hz, 400_000);
    assert_eq!(profile.nominal_period_us, 5_000);
    assert_eq!(
        profile.sensor_order,
        [
            SensorRole::Thumb,
            SensorRole::Index,
            SensorRole::Middle,
            SensorRole::Ring,
            SensorRole::Pinky,
            SensorRole::Dorsal,
        ]
    );

    let manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("profile parses");
    assert_eq!(manifest.sensor_odr_hz, profile.sensor_odr_hz);
    assert_eq!(
        manifest.accelerometer_range_g,
        profile.accelerometer_range_g
    );
    assert_eq!(manifest.gyroscope_range_dps, profile.gyroscope_range_dps);
    assert_eq!(manifest.i2c_bus_hz, profile.i2c_bus_hz);
    assert_eq!(manifest.nominal_period_us, profile.nominal_period_us);
    assert_eq!(
        usize::from(manifest.sensors_per_hand),
        profile.sensor_order.len()
    );
    assert!(
        manifest.hands[0]
            .sensors
            .iter()
            .map(|sensor| sensor.role)
            .eq(profile.sensor_order)
    );
}

#[test]
fn checked_in_template_requires_real_identities_before_it_can_lock() {
    let example = include_bytes!("../configs/acquisition-template.example.json");
    let example_fields: serde_json::Value =
        serde_json::from_slice(example).expect("checked-in template is valid JSON");
    assert_eq!(
        example_fields["prompt_corpus_sha256"],
        format_sha256(&PromptCorpus::v1().sha256())
    );
    assert_eq!(
        AcquisitionManifestTemplate::from_json_with_evidence(example, b"{}", b"{}"),
        Err(ManifestError::PlaceholderIdentity("firmware_build"))
    );

    let (source, rig_source, axis_source) = bound_template_artifacts();
    let template =
        AcquisitionManifestTemplate::from_json_with_evidence(&source, &rig_source, &axis_source)
            .expect("evidence-bound hardware template parses");
    let observed = template
        .expected_controllers()
        .into_iter()
        .enumerate()
        .map(|(index, (hand, controller_id_sha256))| FrameIdentity {
            hand,
            controller_id_sha256,
            boot_id: [u8::try_from(index + 1).expect("fixture index fits u8"); 16],
        })
        .collect::<Vec<_>>();
    let manifest = template.lock(&observed).expect("observed boots lock");

    assert_eq!(manifest.hands.len(), 1);
    assert_eq!(manifest.protocol_version, 2);
    assert_eq!(manifest.sensor_odr_hz, 208);
    assert_eq!(manifest.accelerometer_range_g, 4);
    assert_eq!(manifest.gyroscope_range_dps, 1_000);
    assert_eq!(manifest.i2c_bus_hz, 400_000);
    assert_eq!(manifest.nominal_period_us, 5_000);
    assert_eq!(manifest.prompt_corpus_sha256, HASH_A);
}

#[test]
fn a_semantic_provenance_change_changes_the_manifest_identity() {
    let first = AcquisitionManifest::from_json(&manifest_json(208)).expect("first parses");
    let mut second = first.clone();
    second.participant_alias = "manifest-contract-participant-002".to_owned();

    assert_ne!(
        first.sha256().expect("first hashes"),
        second.sha256().expect("second hashes")
    );
}

#[test]
fn every_declared_setting_must_match_the_canonical_acquisition_profile() {
    let valid = AcquisitionManifest::from_json(&manifest_json(208)).expect("profile parses");
    let mut mismatches = Vec::new();

    let mut sensors_per_hand = valid.clone();
    sensors_per_hand.sensors_per_hand = 5;
    mismatches.push(("sensors_per_hand", sensors_per_hand));

    let mut odr = valid.clone();
    odr.sensor_odr_hz = 416;
    mismatches.push(("sensor_odr_hz", odr));

    let mut accelerometer = valid.clone();
    accelerometer.accelerometer_range_g = 8;
    mismatches.push(("accelerometer_range_g", accelerometer));

    let mut gyroscope = valid.clone();
    gyroscope.gyroscope_range_dps = 500;
    mismatches.push(("gyroscope_range_dps", gyroscope));

    let mut bus = valid.clone();
    bus.i2c_bus_hz = 100_000;
    mismatches.push(("i2c_bus_hz", bus));

    let mut period = valid;
    period.nominal_period_us = 4_808;
    mismatches.push(("nominal_period_us", period));

    for (field, manifest) in mismatches {
        assert_eq!(
            manifest.validate(),
            Err(ManifestError::InvalidSetting(field)),
            "{field} mismatch must fail closed"
        );
    }

    let template = String::from_utf8(template_json(208)).expect("fixture is UTF-8");
    for (field, declaration, mismatch) in [
        (
            "sensors_per_hand",
            "\"sensors_per_hand\": 6",
            "\"sensors_per_hand\": 5",
        ),
        (
            "sensor_odr_hz",
            "\"sensor_odr_hz\": 208",
            "\"sensor_odr_hz\": 416",
        ),
        (
            "accelerometer_range_g",
            "\"accelerometer_range_g\": 4",
            "\"accelerometer_range_g\": 8",
        ),
        (
            "gyroscope_range_dps",
            "\"gyroscope_range_dps\": 1000",
            "\"gyroscope_range_dps\": 500",
        ),
        (
            "i2c_bus_hz",
            "\"i2c_bus_hz\": 400000",
            "\"i2c_bus_hz\": 100000",
        ),
        (
            "nominal_period_us",
            "\"nominal_period_us\": 5000",
            "\"nominal_period_us\": 4808",
        ),
    ] {
        let mismatch = template.replacen(declaration, mismatch, 1);
        assert_eq!(
            AcquisitionManifest::axis_characterization_candidate_sha256_from_json(
                mismatch.as_bytes(),
            ),
            Err(ManifestError::InvalidSetting(field)),
            "template {field} mismatch must fail before capture"
        );
    }
}

#[test]
fn placeholder_build_and_participant_identities_are_rejected() {
    let valid = String::from_utf8(manifest_json(208)).expect("fixture is UTF-8");
    for (field, identity, sentinel) in [
        (
            "firmware_build",
            "manifest-contract-firmware-build-001",
            "replace-with-exact-firmware-build-id",
        ),
        (
            "firmware_build",
            "manifest-contract-firmware-build-001",
            "independent-firmware-unvalidated-v1",
        ),
        (
            "host_build",
            "manifest-contract-host-build-001",
            "replace-with-exact-host-build-id",
        ),
        (
            "host_build",
            "manifest-contract-host-build-001",
            "independent-host-v1",
        ),
        (
            "host_build",
            "manifest-contract-host-build-001",
            "independent-host-bazel-v1",
        ),
        (
            "participant_alias",
            "manifest-contract-participant-001",
            "replace-with-local-alias",
        ),
        (
            "participant_alias",
            "manifest-contract-participant-001",
            "TODO",
        ),
    ] {
        let source = valid.replacen(identity, sentinel, 1);
        assert_eq!(
            AcquisitionManifest::from_json(source.as_bytes()),
            Err(ManifestError::PlaceholderIdentity(field)),
            "{field} sentinel must not become evidence identity"
        );
    }
}

#[test]
fn template_locks_only_observed_controller_boot_identities() {
    let (template_source, rig_source, axis_source) = bound_template_artifacts();
    let template = AcquisitionManifestTemplate::from_json_with_evidence(
        &template_source,
        &rig_source,
        &axis_source,
    )
    .expect("pending template parses");
    let locked = template
        .lock(&[FrameIdentity {
            hand: Hand::Left,
            controller_id_sha256: sha256(b"xiao-left-001"),
            boot_id: [0x44; 16],
        }])
        .expect("observed identity locks template");

    assert_eq!(locked.schema, ACQUISITION_MANIFEST_SCHEMA);
    assert_eq!(locked.hands[0].boot_id, "44".repeat(16));
    locked.validate().expect("locked manifest is canonical");
}

#[test]
fn template_rejects_prelocked_and_wrong_controller_identity() {
    assert!(matches!(
        AcquisitionManifestTemplate::from_json_with_evidence(&manifest_json(208), b"{}", b"{}",),
        Err(ManifestError::InvalidTemplateSchema)
    ));

    let (template_source, rig_source, axis_source) = bound_template_artifacts();
    let prelocked = String::from_utf8(template_source.clone())
        .expect("fixture is UTF-8")
        .replace(
            "00000000000000000000000000000000",
            "11111111111111111111111111111111",
        );
    assert!(matches!(
        AcquisitionManifestTemplate::from_json_with_evidence(
            prelocked.as_bytes(),
            &rig_source,
            &axis_source,
        ),
        Err(ManifestError::TemplateBootNotPending { hand_index: 0 })
    ));

    let template = AcquisitionManifestTemplate::from_json_with_evidence(
        &template_source,
        &rig_source,
        &axis_source,
    )
    .expect("pending template parses");
    assert!(matches!(
        template.lock(&[FrameIdentity {
            hand: Hand::Left,
            controller_id_sha256: [0x99; 32],
            boot_id: [0x44; 16],
        }]),
        Err(ManifestError::ObservedControllerIdentityMismatch(
            Hand::Left
        ))
    ));
}

#[test]
fn unknown_and_missing_fields_fail_closed() {
    let mut unknown = String::from_utf8(manifest_json(208)).expect("fixture is UTF-8");
    unknown = unknown.replacen("\n}", ",\n  \"keyboard_angle_degrees\": 15\n}", 1);
    assert!(matches!(
        AcquisitionManifest::from_json(unknown.as_bytes()),
        Err(ManifestError::Json(_))
    ));

    let missing = String::from_utf8(manifest_json(208))
        .expect("fixture is UTF-8")
        .replace("  \"sensor_model\": \"LSM6DS3TR-C\",\n", "");
    assert!(matches!(
        AcquisitionManifest::from_json(missing.as_bytes()),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn legacy_hand_aggregate_sensor_fields_fail_closed() {
    let legacy = String::from_utf8(manifest_json(208))
        .expect("fixture is UTF-8")
        .replacen(
            "      \"sensors\": [",
            "      \"mount_id\": \"strap-left-a\",\n      \"mux_channels\": [0, 1, 2, 3, 4, 5],\n      \"wiring_revision\": \"one-hand-rig-v1\",\n      \"axis_map\": [1, 0, 0, 0, 1, 0, 0, 0, 1],\n      \"sensors\": [",
            1,
        );

    assert!(matches!(
        AcquisitionManifest::from_json(legacy.as_bytes()),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn every_hand_declares_exactly_six_sensors() {
    let five = manifest_json_with_roles(208, &SENSOR_ROLES[..5]);
    assert!(matches!(
        AcquisitionManifest::from_json(&five),
        Err(ManifestError::Json(_))
    ));

    let seven_roles = [
        "thumb", "index", "middle", "ring", "pinky", "dorsal", "thumb",
    ];
    let seven = manifest_json_with_roles(208, &seven_roles);
    assert!(matches!(
        AcquisitionManifest::from_json(&seven),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn invalid_hand_topology_and_empty_capture_provenance_are_rejected() {
    let mut duplicate = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    duplicate.hands.push(duplicate.hands[0].clone());
    assert_eq!(duplicate.validate(), Err(ManifestError::DuplicateHand));

    let mut empty = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    empty.firmware_build = "   ".to_owned();
    assert_eq!(
        empty.validate(),
        Err(ManifestError::EmptyIdentity("firmware_build"))
    );
}

#[test]
fn sensor_indices_roles_and_mux_channels_are_unique() {
    let mut index = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    index.hands[0].sensors[1].sensor_index = 0;
    assert_eq!(
        index.validate(),
        Err(ManifestError::DuplicateSensorIndex {
            hand_index: 0,
            sensor_index: 0,
        })
    );

    let mut role = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    role.hands[0].sensors[1].role = SensorRole::Thumb;
    assert_eq!(
        role.validate(),
        Err(ManifestError::DuplicateSensorRole {
            hand_index: 0,
            role: SensorRole::Thumb,
        })
    );

    let mut channel = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    channel.hands[0].sensors[5].mux_channel = 4;
    assert_eq!(
        channel.validate(),
        Err(ManifestError::DuplicateMuxChannel {
            hand_index: 0,
            mux_channel: 4,
        })
    );
}

#[test]
fn exactly_one_sensor_per_hand_is_the_dorsal_reference() {
    let mut missing = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    missing.hands[0].sensors[5].role = SensorRole::Pinky;
    assert_eq!(
        missing.validate(),
        Err(ManifestError::InvalidDorsalCount {
            hand_index: 0,
            actual: 0,
        })
    );

    let mut duplicate = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    duplicate.hands[0].sensors[0].role = SensorRole::Dorsal;
    assert_eq!(
        duplicate.validate(),
        Err(ManifestError::InvalidDorsalCount {
            hand_index: 0,
            actual: 2,
        })
    );
}

#[test]
fn sensor_declarations_follow_canonical_index_and_role_order() {
    let mut entries = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    entries.hands[0].sensors.swap(0, 1);
    assert_eq!(
        entries.validate(),
        Err(ManifestError::InvalidSensorOrder {
            hand_index: 0,
            sensor_position: 0,
        })
    );

    let mut roles = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    let first_role = roles.hands[0].sensors[0].role;
    let second_role = roles.hands[0].sensors[1].role;
    roles.hands[0].sensors[0].role = second_role;
    roles.hands[0].sensors[1].role = first_role;
    assert_eq!(
        roles.validate(),
        Err(ManifestError::InvalidSensorOrder {
            hand_index: 0,
            sensor_position: 0,
        })
    );
}

#[test]
fn physical_mux_routing_can_differ_from_canonical_sensor_order() {
    let mut manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    for (sensor, mux_channel) in manifest.hands[0].sensors.iter_mut().zip([5, 4, 3, 2, 1, 0]) {
        sensor.mux_channel = mux_channel;
    }

    manifest
        .validate()
        .expect("unique physical channels remain valid provenance");
}

#[test]
fn both_hands_use_canonical_roles_with_controller_local_interrupt_pins() {
    let mut manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    for (sensor, interrupt_pin) in manifest.hands[0]
        .sensors
        .iter_mut()
        .zip(["P0.02", "P0.03", "P0.28", "P0.29", "P1.11", "P1.12"])
    {
        sensor.interrupt_pin = interrupt_pin.to_owned();
    }

    let mut right = manifest.hands[0].clone();
    right.hand = ManifestHand::Right;
    right.controller_id = "xiao-right-001".to_owned();
    right.boot_id = "22".repeat(16);
    for sensor in &mut right.sensors {
        sensor.mount_id = sensor.mount_id.replacen("left-", "right-", 1);
        sensor.wiring_id = sensor.wiring_id.replacen("left-", "right-", 1);
    }
    manifest.hands.push(right);

    manifest
        .validate()
        .expect("the same pin names are distinct on different controllers");
}

#[test]
fn sensor_mount_wiring_and_interrupt_identities_are_required_and_unique() {
    let mut mount = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    mount.hands[0].sensors[2].mount_id = "   ".to_owned();
    assert_eq!(
        mount.validate(),
        Err(ManifestError::EmptyIdentity("hands.sensors.mount_id"))
    );

    let mut wiring = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    wiring.hands[0].sensors[2].wiring_id.clear();
    assert_eq!(
        wiring.validate(),
        Err(ManifestError::EmptyIdentity("hands.sensors.wiring_id"))
    );

    let mut interrupt = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    interrupt.hands[0].sensors[2].interrupt_pin.clear();
    assert_eq!(
        interrupt.validate(),
        Err(ManifestError::EmptyIdentity("hands.sensors.interrupt_pin"))
    );

    let mut duplicate_mount =
        AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    let first_mount_id = duplicate_mount.hands[0].sensors[0].mount_id.clone();
    duplicate_mount.hands[0].sensors[1].mount_id = first_mount_id;
    assert_eq!(
        duplicate_mount.validate(),
        Err(ManifestError::DuplicateIdentity("hands.sensors.mount_id"))
    );

    let mut duplicate_wiring =
        AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    let first_wiring_id = duplicate_wiring.hands[0].sensors[0].wiring_id.clone();
    duplicate_wiring.hands[0].sensors[1].wiring_id = first_wiring_id;
    assert_eq!(
        duplicate_wiring.validate(),
        Err(ManifestError::DuplicateIdentity("hands.sensors.wiring_id"))
    );

    let mut duplicate_interrupt =
        AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    let first_interrupt_pin = duplicate_interrupt.hands[0].sensors[0]
        .interrupt_pin
        .clone();
    duplicate_interrupt.hands[0].sensors[1].interrupt_pin = first_interrupt_pin;
    assert_eq!(
        duplicate_interrupt.validate(),
        Err(ManifestError::DuplicateIdentity(
            "hands.sensors.interrupt_pin"
        ))
    );
}

#[test]
fn axis_maps_must_be_proper_signed_permutation_matrices() {
    let mut manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    manifest.hands[0].sensors[2].axis_map = [1, 1, 0, 0, 1, 0, 0, 0, 1];

    assert_eq!(
        manifest.validate(),
        Err(ManifestError::InvalidAxisMap {
            hand_index: 0,
            sensor_index: 2,
        })
    );

    let mut reflection = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    reflection.hands[0].sensors[2].axis_map = [-1, 0, 0, 0, 1, 0, 0, 0, 1];
    assert_eq!(
        reflection.validate(),
        Err(ManifestError::InvalidAxisMap {
            hand_index: 0,
            sensor_index: 2,
        })
    );

    let mut rotation = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    rotation.hands[0].sensors[2].axis_map = [0, -1, 0, 1, 0, 0, 0, 0, 1];
    rotation
        .validate()
        .expect("determinant-positive signed rotations are valid");
}

#[test]
fn frame_canonicalizer_rotates_raw_vectors_once_and_preserves_metadata() {
    let mut manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    manifest.hands[0].sensors[0].axis_map = [0, -1, 0, 1, 0, 0, 0, 0, 1];
    let identity = manifest.hands[0]
        .frame_identity()
        .expect("manifest identity exists");
    let mut sensors = [SensorReading::default(); SENSOR_COUNT];
    sensors[0] = SensorReading {
        acceleration: [1, 2, 3],
        angular_velocity: [-4, 5, 6],
        sample_time_offset_us: -7,
    };
    let raw = SensorFrame {
        hand: Hand::Left,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence: 42,
        device_time_us: 123_000,
        sensors,
    };

    let canonical = FrameCanonicalizer::new(&manifest)
        .expect("canonicalizer caches a valid manifest")
        .canonicalize(&raw)
        .expect("proper rotation is representable");

    assert_eq!(canonical.sensors[0].acceleration, [-2, 1, 3]);
    assert_eq!(canonical.sensors[0].angular_velocity, [-5, -4, 6]);
    assert_eq!(canonical.sensors[0].sample_time_offset_us, -7);
    assert_eq!(canonical.sequence, raw.sequence);
    assert_eq!(canonical.device_time_us, raw.device_time_us);
    assert_eq!(raw.sensors[0].acceleration, [1, 2, 3]);
}

#[test]
fn frame_canonicalizer_fails_closed_when_negation_exceeds_i16() {
    let mut manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    manifest.hands[0].sensors[0].axis_map = [-1, 0, 0, 0, -1, 0, 0, 0, 1];
    let identity = manifest.hands[0]
        .frame_identity()
        .expect("manifest identity exists");
    let mut sensors = [SensorReading::default(); SENSOR_COUNT];
    sensors[0].acceleration = [i16::MIN, 0, 0];
    let raw = SensorFrame {
        hand: Hand::Left,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence: 1,
        device_time_us: 5_000,
        sensors,
    };

    assert_eq!(
        FrameCanonicalizer::new(&manifest)
            .expect("canonicalizer caches a valid manifest")
            .canonicalize(&raw),
        Err(FrameCanonicalizationError::AxisOverflow {
            hand: Hand::Left,
            sensor_index: 0,
            vector: SensorVector::Acceleration,
            axis: 0,
        })
    );
}

#[test]
fn manifest_size_is_bounded_before_json_allocation() {
    let oversized = vec![b' '; 65_537];
    assert_eq!(
        AcquisitionManifest::from_json(&oversized),
        Err(ManifestError::TooLarge { actual: 65_537 })
    );
}

#[test]
fn hand_manifest_type_remains_owned_by_the_manifest_contract() {
    let manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");
    let hand: &HandManifest = &manifest.hands[0];
    let sensor: &SensorManifest = &hand.sensors[0];
    assert_eq!(hand.controller_id, "xiao-left-001");
    assert_eq!(hand.parsed_boot_id(), Some([0x11; 16]));
    assert_eq!(sensor.role, SensorRole::Thumb);
    assert_eq!(sensor.mount_id, "left-thumb-mount-v1");
    assert_eq!(sensor.wiring_id, "left-thumb-wiring-v1");
    assert_eq!(sensor.interrupt_pin, "left-thumb-int1-v1");
}

#[test]
fn controller_boot_identity_is_required_and_canonical() {
    let mut invalid = String::from_utf8(manifest_json(208)).expect("fixture is UTF-8");
    invalid = invalid.replace(
        "11111111111111111111111111111111",
        "1111111111111111111111111111111G",
    );

    assert_eq!(
        AcquisitionManifest::from_json(invalid.as_bytes()),
        Err(ManifestError::InvalidBootId { hand_index: 0 })
    );
}

#[test]
fn observed_session_hands_must_match_the_manifest_exactly() {
    let manifest = AcquisitionManifest::from_json(&manifest_json(208)).expect("valid parses");

    manifest
        .validate_observed_topology(24, 0)
        .expect("declared left-only evidence binds");
    assert_eq!(
        manifest.validate_observed_topology(0, 0),
        Err(ObservedTopologyError::MissingDeclaredHand(
            ManifestHand::Left
        ))
    );
    assert_eq!(
        manifest.validate_observed_topology(24, 1),
        Err(ObservedTopologyError::UndeclaredObservedHand(
            ManifestHand::Right
        ))
    );
}

#[test]
fn rig_evidence_template_keeps_every_unrun_physical_check_explicit() {
    let source = include_bytes!("../configs/rig-evidence-template.example.json");
    let record = RigEvidenceRecord::from_json(source).expect("pending rig record parses");

    assert!(!record.is_complete());
    assert!(
        record
            .hands
            .iter()
            .all(|hand| hand.mass_measurement.status == EvidenceStatus::NotYetRun)
    );
    assert_eq!(record.continuity.fact.status, EvidenceStatus::NotYetRun);
    assert_eq!(record.photos.fact.status, EvidenceStatus::NotYetRun);
    assert_eq!(record.power.fact.status, EvidenceStatus::NotYetRun);
    assert_eq!(record.analyzer.fact.status, EvidenceStatus::NotYetRun);
    assert_eq!(record.surface.fact.status, EvidenceStatus::NotYetRun);
    assert_eq!(record.posture.fact.status, EvidenceStatus::NotYetRun);
    assert_eq!(record.truth_device.fact.status, EvidenceStatus::NotYetRun);
    assert_eq!(record.alignment.fact.status, EvidenceStatus::NotYetRun);

    let canonical = record.canonical_json().expect("pending record serializes");
    assert_eq!(
        RigEvidenceRecord::from_json(&canonical).expect("canonical record parses"),
        record
    );
}

#[test]
fn rig_evidence_is_bounded_strict_and_status_consistent() {
    let complete = rig_evidence_json("passed");
    RigEvidenceRecord::from_json(&complete).expect("completed rig evidence parses");

    let mut inconsistent: serde_json::Value =
        serde_json::from_slice(&complete).expect("fixture JSON parses");
    inconsistent["power"]["fact"]["status"] = serde_json::json!("not-yet-run");
    inconsistent["power"]["fact"]["artifact_sha256"] = serde_json::Value::Null;
    assert_eq!(
        RigEvidenceRecord::from_json(
            &serde_json::to_vec(&inconsistent).expect("fixture serializes")
        ),
        Err(EvidenceArtifactError::InconsistentField(
            "power.supply_millivolts"
        ))
    );

    let mut unknown: serde_json::Value =
        serde_json::from_slice(&complete).expect("fixture JSON parses");
    unknown["keyboard_angle_degrees"] = serde_json::json!(15);
    assert!(matches!(
        RigEvidenceRecord::from_json(&serde_json::to_vec(&unknown).expect("fixture serializes")),
        Err(EvidenceArtifactError::Json { .. })
    ));

    let oversized = vec![b' '; 65_537];
    assert!(matches!(
        RigEvidenceRecord::from_json(&oversized),
        Err(EvidenceArtifactError::TooLarge {
            artifact: "rig evidence",
            actual: 65_537,
            maximum: 65_536,
        })
    ));
}

#[test]
fn axis_report_recomputes_residuals_and_freezes_the_hand_carrier_convention() {
    let (_, _, source) = bound_artifacts();
    let report = AxisCharacterizationReport::from_json(&source).expect("axis report parses");
    assert!(report.passed);
    assert_eq!(report.coordinate_convention, CANONICAL_AXIS_CONVENTION);
    assert_eq!(report.hands[0].hand, ManifestHand::Left);
    assert_eq!(
        report.hands[0].positive_y_direction,
        PositiveYDirection::Ulnar
    );

    let mut wrong_hand_direction: serde_json::Value =
        serde_json::from_slice(&source).expect("fixture JSON parses");
    wrong_hand_direction["hands"][0]["positive_y_direction"] = serde_json::json!("radial");
    assert_eq!(
        AxisCharacterizationReport::from_json(
            &serde_json::to_vec(&wrong_hand_direction).expect("fixture serializes")
        ),
        Err(EvidenceArtifactError::PositiveYDirectionMismatch {
            hand: ManifestHand::Left,
        })
    );

    let mut right_hand: serde_json::Value =
        serde_json::from_slice(&source).expect("fixture JSON parses");
    right_hand["hands"][0]["hand"] = serde_json::json!("right");
    right_hand["hands"][0]["positive_y_direction"] = serde_json::json!("radial");
    AxisCharacterizationReport::from_json(
        &serde_json::to_vec(&right_hand).expect("fixture serializes"),
    )
    .expect("right-hand +Y is radial");
    right_hand["hands"][0]["positive_y_direction"] = serde_json::json!("ulnar");
    assert_eq!(
        AxisCharacterizationReport::from_json(
            &serde_json::to_vec(&right_hand).expect("fixture serializes")
        ),
        Err(EvidenceArtifactError::PositiveYDirectionMismatch {
            hand: ManifestHand::Right,
        })
    );

    let mut wrong_convention: serde_json::Value =
        serde_json::from_slice(&source).expect("fixture JSON parses");
    wrong_convention["coordinate_convention"] = serde_json::json!("x-distal-y-radial-both-hands");
    assert_eq!(
        AxisCharacterizationReport::from_json(
            &serde_json::to_vec(&wrong_convention).expect("fixture serializes")
        ),
        Err(EvidenceArtifactError::InvalidCoordinateConvention)
    );

    let mut false_residual: serde_json::Value =
        serde_json::from_slice(&source).expect("fixture JSON parses");
    false_residual["hands"][0]["sensors"][0]["observed_maximum_component_residual_lsb"] =
        serde_json::json!(1);
    assert_eq!(
        AxisCharacterizationReport::from_json(
            &serde_json::to_vec(&false_residual).expect("fixture serializes")
        ),
        Err(EvidenceArtifactError::ResidualMismatch {
            hand_index: 0,
            sensor_index: 0,
            declared: 1,
            computed: 0,
        })
    );

    let mut reflected_map: serde_json::Value =
        serde_json::from_slice(&source).expect("fixture JSON parses");
    reflected_map["hands"][0]["sensors"][0]["axis_map"] =
        serde_json::json!([-1, 0, 0, 0, 1, 0, 0, 0, 1]);
    assert_eq!(
        AxisCharacterizationReport::from_json(
            &serde_json::to_vec(&reflected_map).expect("fixture serializes")
        ),
        Err(EvidenceArtifactError::InvalidAxisMap {
            hand_index: 0,
            sensor_index: 0,
        })
    );

    let mut unknown_field: serde_json::Value =
        serde_json::from_slice(&source).expect("fixture JSON parses");
    unknown_field["axis_guess"] = serde_json::json!("identity");
    assert!(matches!(
        AxisCharacterizationReport::from_json(
            &serde_json::to_vec(&unknown_field).expect("fixture serializes")
        ),
        Err(EvidenceArtifactError::Json { .. })
    ));

    let template = include_bytes!("../configs/axis-characterization-template.example.json");
    assert!(matches!(
        AxisCharacterizationReport::from_json(template),
        Err(EvidenceArtifactError::InvalidField("characterizer_build"))
    ));
}

#[test]
fn axis_admission_rejects_report_selected_profiles_even_with_matching_hashes() {
    let (manifest_source, rig_source, axis_source) = bound_artifacts();
    for (pointer, replacement, field) in [
        (
            "/method_id",
            serde_json::json!("six-face-static-mean-v1"),
            "method_id",
        ),
        (
            "/criteria/expected_gravity_lsb",
            serde_json::json!(1),
            "criteria",
        ),
        (
            "/criteria/maximum_component_residual_lsb",
            serde_json::json!(8191),
            "criteria",
        ),
        (
            "/criteria/minimum_samples_per_face",
            serde_json::json!(1),
            "criteria",
        ),
        (
            "/hands/0/sensors/0/raw_face_means/0/sample_count",
            serde_json::json!(599),
            "hands.sensors.raw_face_means.sample_count",
        ),
        (
            "/hands/0/sensors/0/raw_face_means/0/sample_count",
            serde_json::json!(601),
            "hands.sensors.raw_face_means.sample_count",
        ),
    ] {
        let mut report: serde_json::Value = serde_json::from_slice(&axis_source).unwrap();
        *report.pointer_mut(pointer).unwrap() = replacement;
        // Hash the typed bytes directly, without the validator, so a matching
        // manifest identity cannot hide an unsupported measurement profile.
        let report: AxisCharacterizationReport = serde_json::from_value(report).unwrap();
        let changed_source = serde_json::to_vec(&report).unwrap();
        let mut manifest: serde_json::Value = serde_json::from_slice(&manifest_source).unwrap();
        manifest["axis_characterization_sha256"] =
            serde_json::json!(format_sha256(&sha256(&changed_source)));
        assert_eq!(
            AcquisitionManifest::from_json_with_evidence(
                &serde_json::to_vec(&manifest).unwrap(),
                &rig_source,
                &changed_source,
            ),
            Err(ManifestError::EvidenceArtifact(
                EvidenceArtifactError::InvalidField(field)
            )),
            "{pointer}",
        );
    }
}

#[test]
fn operational_manifest_binds_both_exact_artifacts_and_characterized_maps() {
    let (manifest_source, rig_source, axis_source) = bound_artifacts();
    let manifest =
        AcquisitionManifest::from_json_with_evidence(&manifest_source, &rig_source, &axis_source)
            .expect("exact evidence bindings authorize the manifest");
    let report = AxisCharacterizationReport::from_json(&axis_source).expect("axis report parses");

    assert_eq!(
        manifest.hands[0].sensors[0].axis_map,
        [1, 0, 0, 0, 1, 0, 0, 0, 1]
    );
    assert_eq!(
        manifest.hands[0].sensors[0].axis_map,
        report.hands[0].sensors[0].axis_map
    );
}

#[test]
fn evidence_byte_tampering_and_candidate_drift_fail_closed() {
    let (manifest_source, rig_source, axis_source) = bound_artifacts();

    let mut changed_rig: serde_json::Value =
        serde_json::from_slice(&rig_source).expect("rig JSON parses");
    changed_rig["record_id"] = serde_json::json!("manifest-contract-rig-002");
    assert_eq!(
        AcquisitionManifest::from_json_with_evidence(
            &manifest_source,
            &serde_json::to_vec(&changed_rig).expect("rig serializes"),
            &axis_source,
        ),
        Err(ManifestError::RigEvidenceHashMismatch)
    );

    let mut changed_axis: serde_json::Value =
        serde_json::from_slice(&axis_source).expect("axis JSON parses");
    changed_axis["report_id"] = serde_json::json!("manifest-contract-axis-002");
    assert_eq!(
        AcquisitionManifest::from_json_with_evidence(
            &manifest_source,
            &rig_source,
            &serde_json::to_vec(&changed_axis).expect("axis serializes"),
        ),
        Err(ManifestError::AxisCharacterizationHashMismatch)
    );

    let changed_manifest = String::from_utf8(manifest_source)
        .expect("manifest is UTF-8")
        .replace(
            "manifest-contract-firmware-build-001",
            "manifest-contract-firmware-build-002",
        );
    assert_eq!(
        AcquisitionManifest::from_json_with_evidence(
            changed_manifest.as_bytes(),
            &rig_source,
            &axis_source,
        ),
        Err(ManifestError::AxisCandidateManifestHashMismatch)
    );
}

#[test]
fn rehashed_but_different_characterized_axis_map_cannot_authorize_manifest_map() {
    let (manifest_source, rig_source, axis_source) = bound_artifacts();
    let mut changed_axis: serde_json::Value =
        serde_json::from_slice(&axis_source).expect("axis JSON parses");
    let rotated_means = [
        [0, -8192, 0],
        [0, 8192, 0],
        [8192, 0, 0],
        [-8192, 0, 0],
        [0, 0, 8192],
        [0, 0, -8192],
    ];
    for sensor in changed_axis["hands"][0]["sensors"]
        .as_array_mut()
        .expect("sensors are an array")
    {
        sensor["axis_map"] = serde_json::json!([0, -1, 0, 1, 0, 0, 0, 0, 1]);
        for (face, mean) in sensor["raw_face_means"]
            .as_array_mut()
            .expect("face means are an array")
            .iter_mut()
            .zip(rotated_means)
        {
            face["mean_acceleration_lsb"] = serde_json::json!(mean);
        }
    }
    let changed_axis_source =
        serde_json::to_vec(&changed_axis).expect("changed axis report serializes");
    let changed_report = AxisCharacterizationReport::from_json(&changed_axis_source)
        .expect("self-consistent rotated axis report parses");
    let changed_hash = format_sha256(&changed_report.sha256().expect("changed report hashes"));
    let manifest_source = String::from_utf8(manifest_source)
        .expect("manifest is UTF-8")
        .replace(
            &format!(
                "\"axis_characterization_sha256\": \"{}\"",
                format_sha256(
                    &AxisCharacterizationReport::from_json(&axis_source)
                        .expect("original report parses")
                        .sha256()
                        .expect("original report hashes")
                )
            ),
            &format!("\"axis_characterization_sha256\": \"{changed_hash}\""),
        );

    assert_eq!(
        AcquisitionManifest::from_json_with_evidence(
            manifest_source.as_bytes(),
            &rig_source,
            &changed_axis_source,
        ),
        Err(ManifestError::CharacterizedAxisMapMismatch {
            hand_index: 0,
            sensor_index: 0,
        })
    );
}

#[test]
fn nonpassing_physical_or_axis_evidence_never_authorizes_operation() {
    let (manifest_source, rig_source, axis_source) = bound_artifacts_for_rig_status("not-yet-run");
    assert_eq!(
        AcquisitionManifest::from_json_with_evidence(&manifest_source, &rig_source, &axis_source,),
        Err(ManifestError::RigEvidenceNotPassed(
            "hands.mass_measurement"
        ))
    );

    let (manifest_source, rig_source, axis_source) = bound_artifacts();
    let mut failed_axis: serde_json::Value =
        serde_json::from_slice(&axis_source).expect("axis JSON parses");
    failed_axis["hands"][0]["sensors"][0]["raw_face_means"][0]["mean_acceleration_lsb"] =
        serde_json::json!([9000, 900, 0]);
    failed_axis["hands"][0]["sensors"][0]["observed_maximum_component_residual_lsb"] =
        serde_json::json!(900);
    failed_axis["hands"][0]["sensors"][0]["passed"] = serde_json::json!(false);
    failed_axis["passed"] = serde_json::json!(false);
    let failed_axis_source = serde_json::to_vec(&failed_axis).expect("failed report serializes");
    let failed_report = AxisCharacterizationReport::from_json(&failed_axis_source)
        .expect("honest failed report remains valid evidence");
    let failed_hash = format_sha256(&failed_report.sha256().expect("failed report hashes"));
    let original_hash = format_sha256(
        &AxisCharacterizationReport::from_json(&axis_source)
            .expect("original report parses")
            .sha256()
            .expect("original report hashes"),
    );
    let manifest_source = String::from_utf8(manifest_source)
        .expect("manifest is UTF-8")
        .replace(&original_hash, &failed_hash);
    assert_eq!(
        AcquisitionManifest::from_json_with_evidence(
            manifest_source.as_bytes(),
            &rig_source,
            &failed_axis_source,
        ),
        Err(ManifestError::AxisCharacterizationNotPassed)
    );
}
