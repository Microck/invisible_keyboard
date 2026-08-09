use invisible_keyboard::{
    decoder::{AbstainReason, CalibrationError, DecisionKind, HandDecoder, TapWindow, WindowError},
    layout::{Finger, Hand, all_slots},
    protocol::{FLAG_ACQUISITION_OVERRUN, FLAG_CLOCK_UNSYNCED},
    simulation::SyntheticConfig,
};

#[test]
fn generator_is_byte_deterministic_for_the_same_seed_and_trial() {
    let config = SyntheticConfig::new(Hand::Left, 0x1234_5678);

    let first = config.tap_window(all_slots()[0].position, 42);
    let second = config.tap_window(all_slots()[0].position, 42);

    assert_eq!(first, second);
}

#[test]
fn calibrated_decoder_recovers_all_34_synthetic_positions_after_small_remount() {
    for hand in [Hand::Left, Hand::Right] {
        let training = SyntheticConfig::new(hand, 1);
        let decoder = HandDecoder::calibrate(hand, &training.calibration_examples(5, 20))
            .expect("complete synthetic calibration fits");
        let test = SyntheticConfig::new(hand, 2)
            .with_noise(12)
            .with_remount_per_mille(80);

        for (slot_index, slot) in all_slots()
            .iter()
            .filter(|slot| slot.position.hand == hand)
            .enumerate()
        {
            for trial in 0..20 {
                let window = test.tap_window(
                    slot.position,
                    trial + u64::try_from(slot_index).expect("slot index fits u64") * 100,
                );
                let decision = decoder.decode(&window);
                assert_eq!(decision.position, Some(slot.position));

                match slot.usage {
                    Some(expected) => assert_eq!(decision.kind, DecisionKind::Key(expected)),
                    None => assert_eq!(
                        decision.kind,
                        DecisionKind::Abstain(AbstainReason::ReservedPosition)
                    ),
                }
            }
        }
    }
}

#[test]
fn ordinary_shared_hand_motion_does_not_become_a_tap() {
    let config = SyntheticConfig::new(Hand::Left, 3).with_shared_motion(4_000);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("calibration fits");

    for trial in 0..100 {
        let decision = decoder.decode(&config.idle_window(trial));
        assert_eq!(
            decision.kind,
            DecisionKind::Abstain(AbstainReason::NoIntent)
        );
    }
}

#[test]
fn simultaneous_finger_impulses_abstain() {
    let config = SyntheticConfig::new(Hand::Left, 4);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("calibration fits");
    let first = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let second = all_slots()
        .iter()
        .find(|slot| slot.label == "D")
        .expect("D exists")
        .position;

    let decision = decoder.decode(&config.simultaneous_window(first, second, 7));
    assert_eq!(
        decision.kind,
        DecisionKind::Abstain(AbstainReason::AmbiguousFinger)
    );
}

#[test]
fn any_sensor_dropout_abstains_instead_of_guessing() {
    let config = SyntheticConfig::new(Hand::Right, 5);
    let decoder = HandDecoder::calibrate(Hand::Right, &config.calibration_examples(5, 20))
        .expect("calibration fits");
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "M")
        .expect("M exists")
        .position;
    let dropped = SyntheticConfig::new(Hand::Right, 6).with_missing_sensor(4);

    let decision = decoder.decode(&dropped.tap_window(position, 1));
    assert_eq!(
        decision.kind,
        DecisionKind::Abstain(AbstainReason::MissingSensor)
    );
}

#[test]
fn incomplete_calibration_is_rejected() {
    let config = SyntheticConfig::new(Hand::Left, 7);
    let mut examples = config.calibration_examples(5, 20);
    let missing = all_slots()[0].position;
    examples.retain(|example| example.position != Some(missing));

    assert!(matches!(
        HandDecoder::calibrate(Hand::Left, &examples),
        Err(CalibrationError::MissingPosition { position }) if position == missing
    ));
}

#[test]
fn calibration_for_the_wrong_hand_is_rejected() {
    let config = SyntheticConfig::new(Hand::Right, 8);

    assert!(matches!(
        HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20)),
        Err(CalibrationError::WrongHand { .. })
    ));
}

#[test]
fn excessive_inter_sensor_sample_skew_abstains() {
    let config = SyntheticConfig::new(Hand::Left, 9);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("calibration fits");
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let mut frames = config.tap_window(position, 1).frames().to_vec();
    for frame in &mut frames {
        frame.sensors[0].sample_time_offset_us = 4_000;
        frame.sensors[5].sample_time_offset_us = -2_000;
    }
    let skewed = TapWindow::new(frames).expect("sensor times remain monotonic");

    assert_eq!(
        decoder.decode(&skewed).kind,
        DecisionKind::Abstain(AbstainReason::TimingSkew)
    );
}

#[test]
fn per_sensor_sample_time_regression_is_structurally_rejected() {
    let config = SyntheticConfig::new(Hand::Left, 10);
    let mut frames = config.idle_window(1).frames()[..2].to_vec();
    frames[0].sensors[0].sample_time_offset_us = 30_000;
    frames[1].sensors[0].sample_time_offset_us = -30_000;

    assert!(matches!(
        TapWindow::new(frames),
        Err(WindowError::SensorTimeRegression { sensor: 0 })
    ));
}

#[test]
fn repeated_sensor_timestamp_at_allowed_skew_boundary_is_rejected() {
    for hand in [Hand::Left, Hand::Right] {
        let mut frames = SyntheticConfig::new(hand, 10).idle_window(1).frames()[..2].to_vec();
        for (index, frame) in frames.iter_mut().enumerate() {
            frame.device_time_us = 1_000_000 + index as u64 * 5_000;
            for sensor in &mut frame.sensors {
                sensor.sample_time_offset_us = 0;
            }
        }
        frames[1].sensors[0].sample_time_offset_us = -5_000;
        assert!(matches!(
            TapWindow::new(frames),
            Err(WindowError::SensorTimeRegression { sensor: 0 })
        ));
    }
}

#[test]
fn forward_sensor_timestamp_at_allowed_skew_boundary_remains_accepted() {
    for hand in [Hand::Left, Hand::Right] {
        let config = SyntheticConfig::new(hand, 10);
        let decoder = HandDecoder::calibrate(hand, &config.calibration_examples(5, 20))
            .expect("ordinary calibration fits");
        let mut frames = config.idle_window(1).frames()[..2].to_vec();
        for (index, frame) in frames.iter_mut().enumerate() {
            frame.device_time_us = 1_000_000 + index as u64 * 5_001;
            for sensor in &mut frame.sensors {
                sensor.sample_time_offset_us = 0;
            }
        }
        frames[1].sensors[0].sample_time_offset_us = -5_000;
        let window = TapWindow::new(frames).expect("every raw sensor timestamp advances");
        assert_ne!(
            decoder.decode(&window).kind,
            DecisionKind::Abstain(AbstainReason::TimingSkew)
        );
    }
}

#[test]
fn sequence_discontinuity_abstains_instead_of_classifying_across_loss() {
    let config = SyntheticConfig::new(Hand::Left, 13);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let mut frames = config.tap_window(position, 2).frames().to_vec();
    frames.remove(7);
    let lossy = TapWindow::new(frames).expect("loss remains visible in sequence numbers");

    assert_eq!(
        decoder.decode(&lossy).kind,
        DecisionKind::Abstain(AbstainReason::SequenceDiscontinuity)
    );
}

#[test]
fn calibration_rejects_a_window_with_missing_sensor_evidence() {
    let config = SyntheticConfig::new(Hand::Left, 11);
    let mut examples = config.calibration_examples(5, 20);
    let mut frames = examples[0].window.frames().to_vec();
    frames[0].valid_sensor_mask &= !(1_u8 << 2);
    examples[0].window = TapWindow::new(frames).expect("missing evidence is structurally valid");

    assert!(matches!(
        HandDecoder::calibrate(Hand::Left, &examples),
        Err(CalibrationError::UnusableExample {
            index: 0,
            reason: AbstainReason::MissingSensor,
        })
    ));
}

#[test]
fn calibration_rejects_excessive_inter_sensor_timing_skew() {
    let config = SyntheticConfig::new(Hand::Right, 12);
    let mut examples = config.calibration_examples(5, 20);
    let mut frames = examples[0].window.frames().to_vec();
    for frame in &mut frames {
        frame.sensors[0].sample_time_offset_us = 6_000;
        frame.sensors[5].sample_time_offset_us = 0;
    }
    examples[0].window = TapWindow::new(frames).expect("skewed evidence remains monotonic");

    assert!(matches!(
        HandDecoder::calibrate(Hand::Right, &examples),
        Err(CalibrationError::UnusableExample {
            index: 0,
            reason: AbstainReason::TimingSkew,
        })
    ));
}

#[test]
fn every_acquisition_flag_poisoning_a_window_forces_abstention() {
    let config = SyntheticConfig::new(Hand::Left, 14);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;

    for flag in [FLAG_CLOCK_UNSYNCED, FLAG_ACQUISITION_OVERRUN] {
        let mut frames = config
            .tap_window(position, u64::from(flag))
            .frames()
            .to_vec();
        frames[7].flags = flag;
        let poisoned = TapWindow::new(frames).expect("flag does not change window structure");

        assert_eq!(
            decoder.decode(&poisoned).kind,
            DecisionKind::Abstain(AbstainReason::AcquisitionFault)
        );
    }
}

#[test]
fn calibration_rejects_acquisition_fault_evidence() {
    let config = SyntheticConfig::new(Hand::Right, 15);
    let mut examples = config.calibration_examples(5, 20);
    let mut frames = examples[3].window.frames().to_vec();
    frames[2].flags = FLAG_ACQUISITION_OVERRUN;
    examples[3].window = TapWindow::new(frames).expect("flagged window remains structural");

    assert!(matches!(
        HandDecoder::calibrate(Hand::Right, &examples),
        Err(CalibrationError::UnusableExample {
            index: 3,
            reason: AbstainReason::AcquisitionFault,
        })
    ));
}

#[test]
fn split_calibration_keeps_model_fitting_and_threshold_selection_disjoint() {
    let config = SyntheticConfig::new(Hand::Left, 16);
    let training = config.calibration_examples(5, 0);
    let threshold = config.calibration_examples(1, 6);
    let decoder = HandDecoder::calibrate_split(Hand::Left, &training, &threshold)
        .expect("training can omit idle when threshold evidence is separate");
    assert!(decoder.intent_threshold() > 0.0);

    let mut missing_pinky = threshold;
    missing_pinky.retain(|example| {
        example
            .position
            .is_none_or(|position| position.finger != Finger::Pinky)
    });
    assert!(matches!(
        HandDecoder::calibrate_split(Hand::Left, &training, &missing_pinky),
        Err(CalibrationError::MissingThresholdFinger {
            finger: Finger::Pinky,
        })
    ));
}
