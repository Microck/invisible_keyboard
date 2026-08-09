use invisible_keyboard::{
    artifact::{
        ArtifactDomain, ArtifactSensorBinding, ArtifactSource, ArtifactSourceRole,
        CalibrationArtifact, CalibrationArtifactInput, HandClockFit, KeyboardIdentity,
        KeyboardLayoutConfirmationReceipt,
    },
    clock::{ClockFit, ClockSample},
    decoder::HandDecoder,
    layout::{Hand, PHYSICAL_POSITION_COUNT, all_slots},
    manifest::{SensorManifest, SensorRole},
    prompts::PromptCorpus,
    simulation::SyntheticConfig,
};

fn hash(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn clock_fit(hand: Hand) -> HandClockFit {
    let boot_byte = match hand {
        Hand::Left => 0x11,
        Hand::Right => 0x22,
    };
    let samples = (0..20_u64)
        .map(|index| ClockSample {
            device_time_us: index * 10_000,
            host_time_ns: 5_000_000_000 + index * 10_000_000,
        })
        .collect::<Vec<_>>();
    HandClockFit {
        hand,
        fit: ClockFit::fit([boot_byte; 16], &samples).expect("clock fixture fits"),
    }
}

fn sensor_bindings() -> Vec<ArtifactSensorBinding> {
    [Hand::Left, Hand::Right]
        .into_iter()
        .flat_map(|hand| {
            [
                SensorRole::Thumb,
                SensorRole::Index,
                SensorRole::Middle,
                SensorRole::Ring,
                SensorRole::Pinky,
                SensorRole::Dorsal,
            ]
            .into_iter()
            .enumerate()
            .map(move |(sensor_index, role)| {
                let side = match hand {
                    Hand::Left => "left",
                    Hand::Right => "right",
                };
                let sensor_index = u8::try_from(sensor_index).expect("fixture index fits u8");
                ArtifactSensorBinding {
                    hand,
                    sensor: SensorManifest {
                        sensor_index,
                        mux_channel: sensor_index,
                        role,
                        mount_id: format!("{side}-{role:?}-mount-v1").to_lowercase(),
                        wiring_id: format!("{side}-{role:?}-wiring-v1").to_lowercase(),
                        interrupt_pin: format!("{side}-int{sensor_index}"),
                        axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
                    },
                }
            })
        })
        .collect()
}

fn build_artifact(manifest_hash: [u8; 32]) -> CalibrationArtifact {
    let left_config = SyntheticConfig::new(Hand::Left, 100);
    let right_config = SyntheticConfig::new(Hand::Right, 200);
    let left = HandDecoder::calibrate(Hand::Left, &left_config.calibration_examples(5, 20))
        .expect("left calibration fits");
    let right = HandDecoder::calibrate(Hand::Right, &right_config.calibration_examples(5, 20))
        .expect("right calibration fits");

    CalibrationArtifact::new(CalibrationArtifactInput {
        acquisition_manifest_sha256: manifest_hash,
        fitting_capture_journal_sha256: hash(31),
        fitting_evidence_sha256: hash(32),
        prompt_corpus_sha256: PromptCorpus::v1().sha256(),
        neutral_evidence_sha256: hash(3),
        calibration_matching_rule_sha256: hash(4),
        sensor_bindings: sensor_bindings(),
        keyboard: KeyboardIdentity {
            name: "Fixture USB Keyboard".to_owned(),
            physical_path: "fixture/input0".to_owned(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            logical_layout: "us-qwerty-v1".to_owned(),
            layout_confirmation: KeyboardLayoutConfirmationReceipt::us_qwerty(1, 1),
        },
        sources: vec![
            ArtifactSource::new(
                hash(10),
                ArtifactSourceRole::Training,
                ArtifactDomain::Keyboard,
                vec![10, 11],
            ),
            ArtifactSource::new(
                hash(11),
                ArtifactSourceRole::Training,
                ArtifactDomain::PassiveSurface,
                vec![30],
            ),
            ArtifactSource::new(
                hash(12),
                ArtifactSourceRole::ThresholdSelection,
                ArtifactDomain::PassiveSurface,
                vec![31],
            ),
        ],
        clock_fits: vec![clock_fit(Hand::Left), clock_fit(Hand::Right)],
        left_decoder: Some(left),
        right_decoder: Some(right),
        accepted_position_counts: vec![3; PHYSICAL_POSITION_COUNT],
        rejected_examples: 4,
        setup_seconds: 95,
        created_unix_ns: 1_786_233_600_000_000_000,
        requires_wear_session_confirmation: true,
    })
    .expect("complete artifact is valid")
}

#[test]
fn artifact_round_trip_preserves_decoder_behavior_and_identity() {
    let artifact = build_artifact(hash(1));
    let bytes = artifact.canonical_json().expect("artifact serializes");
    let recovered = CalibrationArtifact::from_json(&bytes).expect("artifact parses");
    let recovered_bytes = recovered
        .canonical_json()
        .expect("recovered artifact serializes");
    if bytes != recovered_bytes {
        let index = bytes
            .iter()
            .zip(&recovered_bytes)
            .position(|(left, right)| left != right)
            .unwrap_or(bytes.len().min(recovered_bytes.len()));
        panic!(
            "canonical artifact changed at byte {index}: {:?} != {:?}",
            &bytes[index.saturating_sub(32)..(index + 64).min(bytes.len())],
            &recovered_bytes[index.saturating_sub(32)..(index + 64).min(recovered_bytes.len())]
        );
    }

    assert_eq!(
        artifact.fingerprint().expect("artifact hashes"),
        recovered.fingerprint().expect("recovered artifact hashes")
    );
    assert_eq!(
        artifact
            .locked_identity()
            .expect("artifact identity binds")
            .fingerprint(),
        &artifact.fingerprint().expect("artifact hashes")
    );
    for hand in [Hand::Left, Hand::Right] {
        let config = SyntheticConfig::new(hand, 300);
        let position = all_slots()
            .iter()
            .find(|slot| slot.position.hand == hand)
            .expect("hand has positions")
            .position;
        let window = config.tap_window(position, 1);
        assert_eq!(
            artifact
                .decoder(hand)
                .expect("decoder exists")
                .decode(&window),
            recovered
                .decoder(hand)
                .expect("recovered decoder exists")
                .decode(&window)
        );
    }
}

#[test]
fn manifest_change_changes_artifact_fingerprint() {
    assert_ne!(
        build_artifact(hash(1)).fingerprint().expect("first hashes"),
        build_artifact(hash(2))
            .fingerprint()
            .expect("second hashes")
    );
}

#[test]
fn layout_confirmation_receipt_is_part_of_the_artifact_fingerprint() {
    let original = build_artifact(hash(1));
    let original_fingerprint = original.fingerprint().expect("original artifact hashes");

    let mut changed_instruction = original.clone();
    changed_instruction.keyboard.layout_confirmation =
        KeyboardLayoutConfirmationReceipt::us_qwerty(2, 1);
    changed_instruction
        .validate()
        .expect("a different source instruction is still a valid receipt");

    let mut changed_accept_time = original;
    changed_accept_time.keyboard.layout_confirmation =
        KeyboardLayoutConfirmationReceipt::us_qwerty(1, 2);
    changed_accept_time
        .validate()
        .expect("a different source accept time is still a valid receipt");

    assert_ne!(
        changed_instruction
            .fingerprint()
            .expect("changed instruction hashes"),
        original_fingerprint
    );
    assert_ne!(
        changed_accept_time
            .fingerprint()
            .expect("changed accept time hashes"),
        original_fingerprint
    );
}

#[test]
fn unknown_fields_and_incomplete_coverage_fail_closed() {
    let artifact = build_artifact(hash(1));
    let mut source = String::from_utf8(artifact.canonical_json().expect("serializes"))
        .expect("artifact JSON is UTF-8");
    source.insert_str(source.len() - 1, ",\"keyboard_angle\":15");
    assert!(CalibrationArtifact::from_json(source.as_bytes()).is_err());

    let mut incomplete = artifact;
    incomplete.accepted_position_counts[5] = 0;
    assert!(incomplete.validate().is_err());
}

#[test]
fn one_session_may_supply_disjoint_roles_but_blocks_cannot_overlap() {
    let mut artifact = build_artifact(hash(1));
    artifact.sources[2].session_sha256 = artifact.sources[0].session_sha256.clone();
    artifact
        .validate()
        .expect("one append-only capture may contain disjoint evidence blocks");
    assert_eq!(
        artifact
            .locked_identity()
            .expect("source sessions deduplicate")
            .source_sessions()
            .len(),
        2
    );

    let mut artifact = build_artifact(hash(1));
    artifact.sources[2].block_ids = vec![10];
    assert!(artifact.validate().is_err());
}

#[test]
fn hand_decoders_clock_fits_and_sensor_bindings_must_have_the_same_topology() {
    let mut artifact = build_artifact(hash(1));
    artifact.clock_fits.pop();
    assert!(artifact.validate().is_err());

    let mut artifact = build_artifact(hash(1));
    artifact.sensor_bindings.pop();
    assert!(artifact.validate().is_err());

    let mut artifact = build_artifact(hash(1));
    artifact.sensor_bindings.swap(0, 1);
    assert!(artifact.validate().is_err());
}

#[test]
fn every_sensor_provenance_field_and_matching_rule_is_part_of_artifact_identity() {
    let original = build_artifact(hash(1));
    let original_fingerprint = original.fingerprint().expect("original hashes");

    let mut changes = Vec::new();
    let mut changed_mount = build_artifact(hash(1));
    changed_mount.sensor_bindings[0].sensor.mount_id = "left-thumb-remount-v2".to_owned();
    changes.push(changed_mount);

    let mut changed_wiring = build_artifact(hash(1));
    changed_wiring.sensor_bindings[0].sensor.wiring_id = "left-thumb-wiring-v2".to_owned();
    changes.push(changed_wiring);

    let mut changed_interrupt = build_artifact(hash(1));
    changed_interrupt.sensor_bindings[0].sensor.interrupt_pin = "left-int6".to_owned();
    changes.push(changed_interrupt);

    let mut changed_mux = build_artifact(hash(1));
    changed_mux.sensor_bindings[0].sensor.mux_channel = 6;
    changes.push(changed_mux);

    let mut changed_axis_map = build_artifact(hash(1));
    changed_axis_map.sensor_bindings[0].sensor.axis_map = [0, -1, 0, 1, 0, 0, 0, 0, 1];
    changes.push(changed_axis_map);

    let mut changed_matching_rule = build_artifact(hash(1));
    changed_matching_rule.calibration_matching_rule_sha256 = "05".repeat(32);
    changes.push(changed_matching_rule);

    for changed in changes {
        assert_ne!(
            changed.fingerprint().expect("changed artifact hashes"),
            original_fingerprint
        );
    }
}

#[test]
fn legacy_per_hand_mount_ids_and_invalid_matching_rule_hash_fail_closed() {
    let artifact = build_artifact(hash(1));
    let mut source = String::from_utf8(artifact.canonical_json().expect("serializes"))
        .expect("artifact JSON is UTF-8");
    source.insert_str(
        source.len() - 1,
        ",\"mount_ids\":[\"strap-left-a\",\"strap-right-a\"]",
    );
    assert!(CalibrationArtifact::from_json(source.as_bytes()).is_err());

    let mut invalid_rule = artifact;
    invalid_rule.calibration_matching_rule_sha256 = "not-a-sha256".to_owned();
    assert!(invalid_rule.validate().is_err());
}
