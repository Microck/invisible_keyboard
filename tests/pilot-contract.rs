use std::sync::LazyLock;

use invisible_keyboard::{
    layout::{Finger, Hand, all_slots},
    manifest::{AcquisitionManifest, SensorRole, format_id128, format_sha256, sha256},
    pilot::{
        PILOT_TRUTH_SCHEMA, PilotContact, PilotDisposition, PilotInterval, PilotNegativeInterval,
        PilotNegativeKind, PilotReport, PilotTruth, PilotTruthSource, reduce_pilot,
    },
    protocol::{
        ALL_SENSORS_VALID, FLAG_ACQUISITION_OVERRUN, SENSOR_COUNT, SensorFrame, SensorReading,
    },
    session::{ControllerStream, FinalizedSession, SessionHeader, SessionWriter},
};

const TRUTH_BYTES: &[u8] =
    b"independent optical contact observations; test fixture, not hardware evidence";
const MAPPING_BYTES: &[u8] = b"archived synchronization observations in controller microseconds";
const PERIOD: u64 = 5_000;
const END: u64 = 526_000_000;
const FIRST_TAP: u64 = 4_000_000;
const SPACING: u64 = 600_000;
const FINGERS: [Finger; 5] = [
    Finger::Thumb,
    Finger::Index,
    Finger::Middle,
    Finger::Ring,
    Finger::Pinky,
];

struct Fixture {
    manifest: AcquisitionManifest,
    session: FinalizedSession,
    truth: PilotTruth,
}

fn manifest() -> AcquisitionManifest {
    let sensors: Vec<_> = ["thumb", "index", "middle", "ring", "pinky", "dorsal"]
        .iter()
        .enumerate()
        .map(|(index, role)| {
            serde_json::json!({
                "sensor_index": index, "mux_channel": index, "role": role,
                "mount_id": format!("left-{role}-mount"),
                "wiring_id": format!("left-{role}-wiring"),
                "interrupt_pin": format!("left-{role}-int1"),
                "axis_map": [1,0,0,0,1,0,0,0,1],
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "invisible-keyboard-acquisition-manifest-v4",
        "layout_id": "invisible-keyboard-core28-v1", "protocol_version": 2,
        "hands": [{"hand":"left", "controller_id":"pilot-left-001",
            "boot_id":"11111111111111111111111111111111", "sensors": sensors}],
        "controller_board":"Seeed XIAO nRF52840 Plus SKU 102010672",
        "firmware_build":"pilot-contract-firmware", "host_build":"pilot-contract-host",
        "sensor_model":"LSM6DS3TR-C", "sensors_per_hand":6, "sensor_odr_hz":208,
        "accelerometer_range_g":4, "gyroscope_range_dps":1000,
        "filter_profile":"gate0-raw-v1", "i2c_bus_hz":400000, "nominal_period_us":PERIOD,
        "prompt_corpus_sha256": format_sha256(&[0x41;32]),
        "study_id":"pilot-contract", "split_id":"engineering-only",
        "participant_alias":"pilot-contract-person", "consent_record":"synthetic-test",
        "retention_policy":"local-test-only", "rig_evidence_sha256":format_sha256(&[0x42;32]),
        "axis_characterization_sha256":format_sha256(&[0x43;32]),
        "calibration_artifact_sha256":null, "calibration_validation_sha256":null,
    }))
    .unwrap();
    AcquisitionManifest::from_json(&bytes).unwrap()
}

fn contacts() -> Vec<PilotContact> {
    all_slots()
        .iter()
        .filter(|slot| slot.position.hand == Hand::Left)
        .flat_map(|slot| std::iter::repeat_n(slot.position, 10))
        .enumerate()
        .map(|(index, position)| PilotContact {
            time_us: FIRST_TAP + index as u64 * SPACING,
            position,
        })
        .collect()
}

fn session_with(
    manifest: &AcquisitionManifest,
    mut modify: impl FnMut(&mut SensorFrame) -> bool,
) -> FinalizedSession {
    let identity = manifest.hands[0].frame_identity().unwrap();
    let header = SessionHeader {
        nominal_period_us: PERIOD as u32,
        manifest_sha256: manifest.sha256().unwrap(),
        session_id: [0x31; 16],
    };
    let mut writer = SessionWriter::new(Vec::new(), header).unwrap();
    let contacts = contacts();
    for sequence in 0..(END / PERIOD) as u32 {
        let time = u64::from(sequence) * PERIOD;
        let mut frame = SensorFrame {
            hand: Hand::Left,
            controller_id_sha256: identity.controller_id_sha256,
            boot_id: identity.boot_id,
            flags: 0,
            valid_sensor_mask: ALL_SENSORS_VALID,
            sequence,
            device_time_us: time,
            sensors: [SensorReading::default(); SENSOR_COUNT],
        };
        if time >= FIRST_TAP
            && (time - FIRST_TAP).is_multiple_of(SPACING)
            && let Some(contact) = contacts.get(((time - FIRST_TAP) / SPACING) as usize)
        {
            let sensor = FINGERS
                .iter()
                .position(|finger| *finger == contact.position.finger)
                .unwrap();
            frame.sensors[sensor].acceleration[0] = 200;
        }
        if modify(&mut frame) {
            writer
                .append_frame(ControllerStream::Left, &frame, time * 1_000)
                .unwrap();
        }
    }
    writer.finish_finalized().unwrap()
}

fn truth_for(manifest: &AcquisitionManifest, session: &FinalizedSession) -> PilotTruth {
    let kinds = [
        PilotNegativeKind::MouseUse,
        PilotNegativeKind::PhoneHandling,
        PilotNegativeKind::Scratching,
        PilotNegativeKind::Gesturing,
        PilotNegativeKind::ObjectHandling,
        PilotNegativeKind::HandRepositioning,
        PilotNegativeKind::QuietRest,
    ];
    PilotTruth {
        schema: PILOT_TRUTH_SCHEMA.to_owned(),
        session_sha256: format_sha256(&session.sha256()),
        manifest_sha256: format_sha256(&manifest.sha256().unwrap()),
        session_id: format_id128(&session.header().session_id),
        hand: Hand::Left,
        source: PilotTruthSource::OpticalTracker,
        truth_artifact_sha256: format_sha256(&sha256(TRUTH_BYTES)),
        clock_mapping_sha256: format_sha256(&sha256(MAPPING_BYTES)),
        maximum_alignment_error_us: 5_000,
        neutral: PilotInterval {
            start_us: 0,
            end_us: 3_000_000,
        },
        observation: PilotInterval {
            start_us: 3_000_000,
            end_us: END,
        },
        contacts: contacts(),
        negatives: kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| PilotNegativeInterval {
                kind,
                interval: PilotInterval {
                    start_us: 106_000_000 + index as u64 * 60_000_000,
                    end_us: 166_000_000 + index as u64 * 60_000_000,
                },
            })
            .collect(),
    }
}

fn fixture() -> &'static Fixture {
    static FIXTURE: LazyLock<Fixture> = LazyLock::new(|| {
        let manifest = manifest();
        let session = session_with(&manifest, |_| true);
        let truth = truth_for(&manifest, &session);
        Fixture {
            manifest,
            session,
            truth,
        }
    });
    &FIXTURE
}

fn reduce(
    manifest: &AcquisitionManifest,
    session: &FinalizedSession,
    truth: &PilotTruth,
) -> PilotReport {
    reduce_pilot(manifest, session, truth, TRUTH_BYTES, MAPPING_BYTES).unwrap()
}

fn baseline() -> &'static PilotReport {
    static REPORT: LazyLock<PilotReport> = LazyLock::new(|| {
        let f = fixture();
        reduce(&f.manifest, &f.session, &f.truth)
    });
    &REPORT
}

#[test]
fn complete_independent_taps_include_reserved_positions_and_match_at_causal_emission_time() {
    let f = fixture();
    let report = baseline();
    assert_eq!(report.disposition, PilotDisposition::SupportsTwoHandBuild);
    assert_eq!((report.raw.correct, report.corrected.correct), (170, 170));
    assert_eq!(
        (
            report.corrected.missing,
            report.corrected.wrong_finger,
            report.corrected.extra_events
        ),
        (0, 0, 0)
    );
    assert_eq!(report.corrected.positions.len(), 17);
    assert!(
        report
            .corrected
            .positions
            .iter()
            .all(|position| position.contacts == 10 && position.correct == 10)
    );
    assert!(
        all_slots()
            .iter()
            .filter(|slot| slot.position.hand == Hand::Left && slot.usage.is_none())
            .all(|slot| report
                .corrected
                .positions
                .iter()
                .any(|outcome| outcome.position == slot.position && outcome.correct == 10))
    );
    for (event, contact) in report.corrected.events.iter().zip(&f.truth.contacts) {
        assert_eq!(event.time_us, contact.time_us + PERIOD);
        assert_eq!(event.finger, contact.position.finger);
    }
    let encoded = report.canonical_json().unwrap();
    assert_eq!(
        serde_json::from_slice::<PilotReport>(&encoded).unwrap(),
        *report
    );
}

#[test]
fn misbound_session_manifest_truth_and_mapping_bytes_are_rejected() {
    let f = fixture();
    for field in [
        "session_sha256",
        "manifest_sha256",
        "session_id",
        "truth_artifact_sha256",
        "clock_mapping_sha256",
    ] {
        let mut value = serde_json::to_value(&f.truth).unwrap();
        value[field] = serde_json::Value::String(if field == "session_id" {
            "00".repeat(16)
        } else {
            "00".repeat(32)
        });
        let truth: PilotTruth = serde_json::from_value(value).unwrap();
        assert!(
            reduce_pilot(&f.manifest, &f.session, &truth, TRUTH_BYTES, MAPPING_BYTES).is_err(),
            "{field}"
        );
    }
    assert!(
        reduce_pilot(
            &f.manifest,
            &f.session,
            &f.truth,
            &b"altered recording"[..],
            MAPPING_BYTES
        )
        .is_err()
    );
    assert!(
        reduce_pilot(
            &f.manifest,
            &f.session,
            &f.truth,
            TRUTH_BYTES,
            &b"altered mapping"[..]
        )
        .is_err()
    );
    let mut manifest = f.manifest.clone();
    manifest.hands[0].boot_id = "22222222222222222222222222222222".to_owned();
    let mut truth = f.truth.clone();
    truth.manifest_sha256 = format_sha256(&manifest.sha256().unwrap());
    assert!(reduce_pilot(&manifest, &f.session, &truth, TRUTH_BYTES, MAPPING_BYTES).is_err());
}

#[test]
fn incomplete_or_overlapping_truth_cannot_publish_a_report() {
    let f = fixture();
    let mut cases = Vec::new();
    let mut truth = f.truth.clone();
    truth.contacts.pop();
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.negatives.pop();
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.maximum_alignment_error_us = 5_001;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.neutral.end_us -= 1;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.observation.end_us -= PERIOD;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.observation.end_us += PERIOD;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.negatives[1].interval.start_us -= 1;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.negatives[0].interval.start_us = FIRST_TAP;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.negatives[0].interval.end_us -= 1;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.contacts[0].position.hand = Hand::Right;
    cases.push(truth);
    let mut truth = f.truth.clone();
    truth.contacts[1].time_us = truth.contacts[0].time_us + 499_999;
    cases.push(truth);
    for truth in cases {
        assert!(reduce_pilot(&f.manifest, &f.session, &truth, TRUTH_BYTES, MAPPING_BYTES).is_err());
    }
    let mut prompted = serde_json::to_value(&f.truth).unwrap();
    prompted["source"] = "prompt-schedule".into();
    assert!(PilotTruth::from_json(&serde_json::to_vec(&prompted).unwrap()).is_err());
}

#[test]
fn contact_labels_do_not_train_thresholds_or_choose_detection_fingers() {
    let f = fixture();
    let mut truth = f.truth.clone();
    let positions: Vec<_> = truth
        .contacts
        .iter()
        .map(|contact| contact.position)
        .collect();
    for (index, contact) in truth.contacts.iter_mut().enumerate() {
        contact.position = positions[(index + 30) % positions.len()];
    }
    let report = reduce(&f.manifest, &f.session, &truth);
    assert_eq!(report.corrected.events, baseline().corrected.events);
    assert_eq!(report.raw.events, baseline().raw.events);
    for (a, b) in report
        .corrected
        .fingers
        .iter()
        .zip(&baseline().corrected.fingers)
    {
        assert_eq!(
            (a.neutral_max_energy, a.low_threshold, a.high_threshold),
            (b.neutral_max_energy, b.low_threshold, b.high_threshold)
        );
    }
    assert!(report.corrected.wrong_finger > 0);
    assert!(report.corrected.extra_events > 0);
    assert_eq!(report.disposition, PilotDisposition::Stop);
}

#[test]
fn matching_is_inclusive_at_both_boundaries_but_never_accepts_early_contacts() {
    let f = fixture();
    for (shift, expected_correct) in [
        (-195_000_i64, 170),
        (5_000, 170),
        (-195_001, 169),
        (5_001, 169),
    ] {
        let mut truth = f.truth.clone();
        truth.contacts[0].time_us = FIRST_TAP.checked_add_signed(shift).unwrap();
        let report = reduce(&f.manifest, &f.session, &truth);
        assert_eq!(report.corrected.correct, expected_correct, "shift {shift}");
        assert_eq!(report.corrected.extra_events, 170 - expected_correct);
        assert_eq!(report.corrected.missing, 170 - expected_correct);
        assert_eq!(report.corrected.events, baseline().corrected.events);
        if expected_correct < 170 {
            assert_eq!(report.disposition, PilotDisposition::Stop);
        }
    }
}

#[test]
fn every_post_neutral_event_counts_even_outside_negative_intervals_and_later_motion_does_not_leak()
{
    let f = fixture();
    let extra_time = 105_800_000;
    let session = session_with(&f.manifest, |frame| {
        if frame.device_time_us == extra_time || frame.device_time_us == END - 100_000 {
            frame.sensors[0].acceleration[0] = 20_000;
        }
        true
    });
    let truth = truth_for(&f.manifest, &session);
    let report = reduce(&f.manifest, &session, &truth);
    assert_eq!(report.corrected.correct, 170);
    assert_eq!(report.corrected.extra_events, 2);
    assert_eq!(
        report
            .corrected
            .negatives
            .iter()
            .map(|interval| interval.extra_events)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        &report.corrected.events[..170],
        &baseline().corrected.events
    );
    assert_eq!(report.corrected.fingers, baseline().corrected.fingers);
    assert_eq!(report.disposition, PilotDisposition::Stop);
}

#[test]
fn activity_boundaries_do_not_reset_high_confirmation_or_latched_rearming() {
    let f = fixture();
    let boundary = f.truth.negatives[0].interval.start_us;
    let session = session_with(&f.manifest, |frame| {
        if [boundary - PERIOD, boundary + 10_000, boundary + 120_000]
            .contains(&frame.device_time_us)
        {
            frame.sensors[0].acceleration[0] = 200;
        }
        true
    });
    let report = reduce(&f.manifest, &session, &truth_for(&f.manifest, &session));
    assert_eq!(report.corrected.correct, 170);
    assert_eq!(report.corrected.extra_events, 2);
    assert_eq!(report.corrected.events[170].time_us, boundary);
    assert_eq!(report.corrected.events[171].time_us, boundary + 125_000);
    assert_eq!(report.corrected.negatives[0].extra_events, 2);
    assert_eq!(report.disposition, PilotDisposition::Stop);
}

#[test]
fn dorsal_subtraction_and_measured_axis_rotation_share_the_canonical_signal() {
    let f = fixture();
    let mut manifest = f.manifest.clone();
    // Finger 0 rotates raw Y to canonical X; the others already use X.
    manifest.hands[0].sensors[0].axis_map = [0, 1, 0, -1, 0, 0, 0, 0, 1];
    let session = session_with(&manifest, |frame| {
        if frame.device_time_us == 1_000_000 {
            for sensor in &mut frame.sensors {
                sensor.acceleration[0] = 1_000;
            }
        }
        // Encode canonical vectors in the inverse measured mount rotation.
        let x = frame.sensors[0].acceleration[0];
        frame.sensors[0].acceleration = [0, x, 0];
        true
    });
    let truth = truth_for(&manifest, &session);
    let report = reduce(&manifest, &session, &truth);
    assert_eq!(report.corrected.correct, 170);
    assert_eq!(report.corrected.events, baseline().corrected.events);
    assert_eq!(report.raw.correct, 0);
    assert_eq!(report.raw.missing, 170);
    assert!(
        report
            .raw
            .fingers
            .iter()
            .all(|finger| finger.neutral_max_energy == 1_000_000)
    );
    assert!(
        report
            .corrected
            .fingers
            .iter()
            .all(|finger| finger.neutral_max_energy == 0)
    );
    assert_eq!(report.disposition, PilotDisposition::SupportsTwoHandBuild);
}

#[test]
fn absent_physical_signal_stops_instead_of_turning_truth_into_successful_events() {
    let f = fixture();
    let session = session_with(&f.manifest, |frame| {
        frame.sensors = [SensorReading::default(); SENSOR_COUNT];
        true
    });
    let truth = truth_for(&f.manifest, &session);
    let report = reduce(&f.manifest, &session, &truth);
    assert_eq!(
        (
            report.corrected.correct,
            report.corrected.missing,
            report.corrected.extra_events
        ),
        (0, 170, 0)
    );
    assert!(report.corrected.events.is_empty());
    assert!(
        report
            .corrected
            .fingers
            .iter()
            .all(|finger| finger.median_signal_noise_energy_ratio == 0.0)
    );
    assert_eq!(report.disposition, PilotDisposition::Stop);
}

#[test]
fn bad_saturated_missing_and_identity_faults_cannot_support_a_build() {
    let f = fixture();
    let session = session_with(&f.manifest, |frame| {
        if frame.device_time_us == 2_000_000 {
            frame.flags = FLAG_ACQUISITION_OVERRUN;
        }
        if frame.device_time_us == 2_500_000 {
            frame.sensors[0].acceleration[0] = i16::MAX;
        }
        true
    });
    let report = reduce(&f.manifest, &session, &truth_for(&f.manifest, &session));
    assert_eq!(report.bad_frames, 1);
    assert_eq!(report.saturated_frames, 1);
    assert_eq!(report.corrected.fingers[0].saturated_frames, 1);
    assert_eq!(report.disposition, PilotDisposition::Stop);

    let session = session_with(&f.manifest, |frame| frame.device_time_us != 1_000_000);
    assert!(
        reduce_pilot(
            &f.manifest,
            &session,
            &truth_for(&f.manifest, &session),
            TRUTH_BYTES,
            MAPPING_BYTES
        )
        .is_err()
    );
    let session = session_with(&f.manifest, |frame| {
        if frame.device_time_us == 1_000_000 {
            frame.boot_id = [0x77; 16];
        }
        true
    });
    assert!(
        reduce_pilot(
            &f.manifest,
            &session,
            &truth_for(&f.manifest, &session),
            TRUTH_BYTES,
            MAPPING_BYTES
        )
        .is_err()
    );
    let mut manifest = f.manifest.clone();
    manifest.hands[0].sensors[0].role = SensorRole::Dorsal;
    assert!(reduce_pilot(&manifest, &f.session, &f.truth, TRUTH_BYTES, MAPPING_BYTES).is_err());
    let mut manifest = f.manifest.clone();
    let mut right = manifest.hands[0].clone();
    right.hand = invisible_keyboard::manifest::ManifestHand::Right;
    right.controller_id = "pilot-right-002".to_owned();
    manifest.hands.push(right);
    assert!(reduce_pilot(&manifest, &f.session, &f.truth, TRUTH_BYTES, MAPPING_BYTES).is_err());
}

#[test]
fn screening_uses_overall_and_per_position_recall_without_discarding_failed_trials() {
    let f = fixture();
    for (omitted, supports) in [
        (
            (0..170).filter(|index| index % 10 == 0).collect::<Vec<_>>(),
            true,
        ),
        (
            (0..170)
                .filter(|index| index % 10 == 0 || *index == 1)
                .collect(),
            false,
        ),
        (vec![0, 1], true),
        (vec![0, 1, 2], false),
    ] {
        let session = session_with(&f.manifest, |frame| {
            if frame.device_time_us >= FIRST_TAP {
                let delta = frame.device_time_us - FIRST_TAP;
                if delta.is_multiple_of(SPACING) && omitted.contains(&((delta / SPACING) as usize))
                {
                    frame.sensors = [SensorReading::default(); SENSOR_COUNT];
                }
            }
            true
        });
        let report = reduce(&f.manifest, &session, &truth_for(&f.manifest, &session));
        assert_eq!(report.corrected.correct, 170 - omitted.len());
        assert_eq!(report.corrected.missing, omitted.len());
        assert_eq!(report.corrected.extra_events, 0);
        assert_eq!(
            report.disposition == PilotDisposition::SupportsTwoHandBuild,
            supports
        );
    }
}
