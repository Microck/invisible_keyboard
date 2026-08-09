
#[test]
fn export_clock_domain_cli_fixtures() {
    use invisible_keyboard::{layout::KeyUsage, output::KeyEvent};

    let root = PathBuf::from(std::env::var_os("IKB_CLOCK_SMOKE_FIXTURES").expect("export path"));
    fs::create_dir_all(&root).expect("fixture directory creates");
    let fixture = LiveCliFixture::new(root.as_os_str(), "clock-domain-cli");
    let artifact = CalibrationArtifact::from_json(&fs::read(&fixture.artifact).unwrap()).unwrap();
    let validation = ValidationReport::from_json(
        &fs::read(fixture.calibration_output.join("validation-report.json")).unwrap(),
    ).unwrap();
    let mut runtime_manifest = read_bound_manifest(&fixture.calibration_manifest);
    runtime_manifest.calibration_artifact_sha256 = Some(format_sha256(&artifact.fingerprint().unwrap()));
    runtime_manifest.calibration_validation_sha256 = Some(format_sha256(&validation.fingerprint().unwrap()));
    let runtime_manifest_path = root.join("runtime-manifest.json");
    let manifest_source = runtime_manifest.canonical_json().unwrap();
    AcquisitionManifest::from_json_with_evidence(
        &manifest_source,
        &fs::read(&fixture.rig_evidence).unwrap(),
        &fs::read(&fixture.axis_characterization).unwrap(),
    ).expect("runtime manifest retains its real synthetic evidence binding");
    fs::write(&runtime_manifest_path, manifest_source).unwrap();

    let configs = [
        SyntheticConfig::new(Hand::Left, 0x434c_494c).with_noise(0),
        SyntheticConfig::new(Hand::Right, 0x434c_4952).with_noise(0),
    ];
    let positions = ["A", "H"].map(|label| {
        all_slots().iter().find(|slot| slot.label == label).unwrap().position
    });
    let idle = std::array::from_fn::<_, 2, _>(|hand| configs[hand].idle_window(101).frames()[0].sensors);
    let impulses = std::array::from_fn::<_, 2, _>(|hand| {
        configs[hand].tap_window(positions[hand], 100).frames()[15].sensors
    });
    let relative = |path: &Path| path.strip_prefix(&root).unwrap().to_str().unwrap().to_owned();
    let mut cases = Vec::new();
    for (name, slope, repeated_raw_sensor) in [
        ("healthy-negative-drift", 999_u64, false),
        ("invalid-positive-drift", 1_001_u64, true),
    ] {
        let mut writer = SessionWriter::new(Vec::new(), SessionHeader {
            nominal_period_us: runtime_manifest.nominal_period_us,
            manifest_sha256: runtime_manifest.sha256().unwrap(),
            session_id: [0x96; 16],
        }).unwrap();
        for sequence in 0..=100_u32 {
            for hand_index in 0..2 {
                let identity = runtime_manifest.hands[hand_index].frame_identity().unwrap();
                let mut frame = SensorFrame {
                    hand: identity.hand,
                    controller_id_sha256: identity.controller_id_sha256,
                    boot_id: identity.boot_id,
                    flags: 0,
                    valid_sensor_mask: ALL_SENSORS_VALID,
                    sequence,
                    device_time_us: if repeated_raw_sensor {
                        100_000 + u64::from(sequence) * 5_000
                    } else {
                        1_000_000 + u64::from(sequence) * 5_000 + u64::from(sequence >= 61)
                    },
                    sensors: if sequence == 80 { impulses[hand_index] } else { idle[hand_index] },
                };
                if (repeated_raw_sensor && sequence == 1) || (!repeated_raw_sensor && sequence == 61) {
                    frame.sensors[0].sample_time_offset_us = -5_000;
                }
                let receipt_ns = 5_000_000_000 + hand_index as u64 * 1_000_000 + frame.device_time_us * slope;
                writer.append_frame(ControllerStream::from(frame.hand), &frame, receipt_ns).unwrap();
            }
        }
        let session = writer.finish_finalized().unwrap();
        let clocks = ClockEpochSet::fit_session(&runtime_manifest, &session).unwrap();
        for hand in [Hand::Left, Hand::Right] {
            assert!((clocks.fit_for(hand).unwrap().report().slope_ns_per_us - slope as f64).abs() < 0.000_001);
        }
        let session_path = root.join(format!("{name}.iks"));
        let clocks_path = root.join(format!("{name}-clocks.json"));
        fs::write(&session_path, session.bytes()).unwrap();
        fs::write(&clocks_path, clocks.canonical_json().unwrap()).unwrap();
        let commit_us = (5_000_000_000 + (1_000_000 + 83 * 5_000 + 1) * slope) / 1_000;
        let expected_events = [
            KeyEvent::down(KeyUsage::A, commit_us),
            KeyEvent::up(KeyUsage::A, commit_us),
            KeyEvent::down(KeyUsage::H, commit_us + 1_000),
            KeyEvent::up(KeyUsage::H, commit_us + 1_000),
        ].iter().enumerate().map(|(ordinal, event)| {
            format!("event={ordinal} usage={:?} state={:?} time_us={}", event.usage, event.state, event.time_us)
        }).collect::<Vec<_>>();
        cases.push(serde_json::json!({
            "name": name,
            "slope_ns_per_us": slope,
            "accept": !repeated_raw_sensor,
            "session_sha256": format_sha256(&sha256(session.bytes())),
            "session": relative(&session_path),
            "arguments": [
                relative(&fixture.calibration_manifest),
                relative(&fixture.rig_evidence),
                relative(&fixture.axis_characterization),
                relative(&runtime_manifest_path),
                relative(&clocks_path),
                relative(&fixture.artifact),
                relative(&fixture.calibration_output),
                relative(&session_path),
            ],
            "expected_events": expected_events,
        }));
    }
    fs::write(root.join("clock-domain-cases.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "hardware_evidence": false,
        "cases": cases,
    })).unwrap()).unwrap();
}
