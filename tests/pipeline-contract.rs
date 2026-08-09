use invisible_keyboard::{
    decoder::{AbstainReason, DecisionKind, HandDecoder, WindowError},
    layout::{Hand, KeyUsage, all_slots},
    pipeline::{CausalPipeline, PipelineError},
    protocol::SensorFrame,
    simulation::SyntheticConfig,
};

fn feed_window(
    pipeline: &mut CausalPipeline,
    window: &invisible_keyboard::decoder::TapWindow,
) -> Vec<DecisionKind> {
    window
        .frames()
        .iter()
        .copied()
        .filter_map(|frame| pipeline.push(frame).expect("synthetic stream is valid"))
        .map(|decision| decision.kind)
        .collect()
}

fn feed_rebased(
    pipeline: &mut CausalPipeline,
    frames: &[SensorFrame],
    next_sequence: &mut u32,
    next_time_us: &mut u64,
) -> Vec<DecisionKind> {
    frames
        .iter()
        .copied()
        .filter_map(|mut frame| {
            frame.sequence = *next_sequence;
            frame.device_time_us = *next_time_us;
            *next_sequence = next_sequence.wrapping_add(1);
            *next_time_us = next_time_us.saturating_add(5_000);
            pipeline
                .push(frame)
                .expect("rebased fixture remains a valid stream")
        })
        .map(|decision| decision.kind)
        .collect()
}

#[test]
fn continuous_stream_emits_exactly_one_key_decision_per_tap() {
    let config = SyntheticConfig::new(Hand::Left, 0x5049_5045);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut pipeline = CausalPipeline::new(decoder);
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;

    let mut decisions = Vec::new();
    decisions.extend(feed_window(&mut pipeline, &config.idle_window(100)));
    decisions.extend(feed_window(
        &mut pipeline,
        &config.tap_window(position, 101),
    ));
    decisions.extend(feed_window(&mut pipeline, &config.idle_window(102)));
    decisions.extend(feed_window(&mut pipeline, &config.idle_window(103)));
    decisions.extend(feed_window(
        &mut pipeline,
        &config.tap_window(position, 104),
    ));
    decisions.extend(feed_window(&mut pipeline, &config.idle_window(105)));

    assert_eq!(
        decisions,
        [
            DecisionKind::Key(KeyUsage::A),
            DecisionKind::Key(KeyUsage::A),
        ]
    );
}

#[test]
fn retained_short_impulse_is_timestamped_at_the_commit_frame() {
    let config = SyntheticConfig::new(Hand::Left, 0x0052_504c_594c).with_noise(0);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .unwrap()
        .position;
    let mut pipeline = CausalPipeline::new(decoder);
    let impulse = config.tap_window(position, 100).frames()[15];
    let idle = config.idle_window(101).frames()[0];
    for index in 0..19 {
        let mut frame = if index == 0 { impulse } else { idle };
        frame.sequence = index;
        frame.device_time_us = 25_000 + u64::from(index) * 5_000;
        let decision = pipeline.push(frame).expect("monotonic impulse stream");
        if index < 18 {
            assert_eq!(decision, None, "fixed delay must include all three frames");
        } else {
            let decision = decision.expect("retained impulse commits after three frames");
            assert_eq!(decision.kind, DecisionKind::Key(KeyUsage::A));
            assert_eq!(decision.decision_time_us, frame.device_time_us);
            assert_eq!(decision.decision_time_us, 115_000);
        }
    }
}

#[test]
fn idle_stream_never_emits_a_decision() {
    let config = SyntheticConfig::new(Hand::Right, 0x1d1e);
    let decoder = HandDecoder::calibrate(Hand::Right, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut pipeline = CausalPipeline::new(decoder);

    for trial in 100..110 {
        assert!(feed_window(&mut pipeline, &config.idle_window(trial)).is_empty());
    }
}

#[test]
fn reserved_position_emits_one_explicit_abstention() {
    let config = SyntheticConfig::new(Hand::Right, 0x5e5e_4eed);
    let decoder = HandDecoder::calibrate(Hand::Right, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut pipeline = CausalPipeline::new(decoder);
    let position = all_slots()
        .iter()
        .find(|slot| slot.position.hand == Hand::Right && slot.usage.is_none())
        .expect("right hand has a reserved position")
        .position;

    let mut decisions = feed_window(&mut pipeline, &config.idle_window(200));
    decisions.extend(feed_window(
        &mut pipeline,
        &config.tap_window(position, 201),
    ));
    decisions.extend(feed_window(&mut pipeline, &config.idle_window(202)));

    assert_eq!(
        decisions,
        [DecisionKind::Abstain(AbstainReason::ReservedPosition)]
    );
}

#[test]
fn wrong_hand_frame_resets_the_pipeline_and_returns_an_error() {
    let left = SyntheticConfig::new(Hand::Left, 1);
    let right = SyntheticConfig::new(Hand::Right, 2);
    let decoder = HandDecoder::calibrate(Hand::Left, &left.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut pipeline = CausalPipeline::new(decoder);

    for frame in left.idle_window(10).frames().iter().take(8).copied() {
        assert_eq!(pipeline.push(frame), Ok(None));
    }
    let wrong = right.idle_window(11).frames()[0];
    assert_eq!(
        pipeline.push(wrong),
        Err(PipelineError::WrongHand {
            expected: Hand::Left,
            actual: Hand::Right,
        })
    );
    assert_eq!(pipeline.buffered_frames(), 0);
}

#[test]
fn contact_before_four_idle_windows_cannot_emit_a_repeat() {
    let config = SyntheticConfig::new(Hand::Left, 0x5eed_1d1e);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut pipeline = CausalPipeline::new(decoder);
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let idle = config.idle_window(300);
    let tap = config.tap_window(position, 301);
    let strong_contact = [tap.frames()[15]; 4];
    let mut next_sequence = 0;
    let mut next_time_us = 0;

    let mut decisions = feed_rebased(
        &mut pipeline,
        idle.frames(),
        &mut next_sequence,
        &mut next_time_us,
    );
    decisions.extend(feed_rebased(
        &mut pipeline,
        &strong_contact,
        &mut next_sequence,
        &mut next_time_us,
    ));
    assert_eq!(decisions, [DecisionKind::Key(KeyUsage::A)]);

    // Sixteen samples flush the prior contact and count as one idle window.
    // Two more samples reach three idle windows, which must not rearm output.
    decisions.extend(feed_rebased(
        &mut pipeline,
        idle.frames(),
        &mut next_sequence,
        &mut next_time_us,
    ));
    decisions.extend(feed_rebased(
        &mut pipeline,
        &idle.frames()[..2],
        &mut next_sequence,
        &mut next_time_us,
    ));
    decisions.extend(feed_rebased(
        &mut pipeline,
        &strong_contact,
        &mut next_sequence,
        &mut next_time_us,
    ));
    assert_eq!(decisions, [DecisionKind::Key(KeyUsage::A)]);

    // Nineteen idle samples flush that contact and provide four clean rolling
    // windows. A later four-frame contact can now produce one new decision.
    decisions.extend(feed_rebased(
        &mut pipeline,
        idle.frames(),
        &mut next_sequence,
        &mut next_time_us,
    ));
    decisions.extend(feed_rebased(
        &mut pipeline,
        &idle.frames()[..3],
        &mut next_sequence,
        &mut next_time_us,
    ));
    decisions.extend(feed_rebased(
        &mut pipeline,
        &strong_contact,
        &mut next_sequence,
        &mut next_time_us,
    ));
    assert_eq!(
        decisions,
        [
            DecisionKind::Key(KeyUsage::A),
            DecisionKind::Key(KeyUsage::A),
        ]
    );
}

#[test]
fn quality_fault_during_repeat_suppression_is_still_emitted() {
    let config = SyntheticConfig::new(Hand::Left, 0x5afe_7a11);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut pipeline = CausalPipeline::new(decoder);
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let idle = config.idle_window(350);
    let tap = config.tap_window(position, 351);
    let strong_contact = [tap.frames()[15]; 4];
    let mut next_sequence = 0;
    let mut next_time_us = 0;

    let mut decisions = feed_rebased(
        &mut pipeline,
        idle.frames(),
        &mut next_sequence,
        &mut next_time_us,
    );
    decisions.extend(feed_rebased(
        &mut pipeline,
        &strong_contact,
        &mut next_sequence,
        &mut next_time_us,
    ));
    assert_eq!(decisions, [DecisionKind::Key(KeyUsage::A)]);

    // The emitted key puts the pipeline into repeat suppression. A transport
    // gap in that state is still evidence poison and must reach the runtime.
    let mut gapped = idle.frames()[0];
    gapped.sequence = next_sequence.wrapping_add(1);
    gapped.device_time_us = next_time_us;
    let decision = pipeline
        .push(gapped)
        .expect("a sequence gap is a decoded abstention, not a window error")
        .expect("quality faults must not be swallowed during suppression");

    assert_eq!(
        decision.kind,
        DecisionKind::Abstain(AbstainReason::SequenceDiscontinuity)
    );
}

#[test]
fn device_time_regression_discards_all_buffered_context() {
    let config = SyntheticConfig::new(Hand::Right, 0x71ae);
    let decoder = HandDecoder::calibrate(Hand::Right, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut pipeline = CausalPipeline::new(decoder);
    let frames = config.idle_window(400);

    for frame in frames.frames().iter().take(8).copied() {
        assert_eq!(pipeline.push(frame), Ok(None));
    }
    let mut regressed = frames.frames()[8];
    regressed.device_time_us = frames.frames()[7].device_time_us - 1;

    assert_eq!(
        pipeline.push(regressed),
        Err(PipelineError::InvalidWindow(WindowError::TimeRegression))
    );
    assert_eq!(pipeline.buffered_frames(), 0);

    // The reset also removes the old time baseline, so a new stream can start
    // from an arbitrary device epoch after reconnection.
    assert_eq!(pipeline.push(frames.frames()[0]), Ok(None));
    assert_eq!(pipeline.buffered_frames(), 1);
}
