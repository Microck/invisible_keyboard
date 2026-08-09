use invisible_keyboard::{
    decoder::{AbstainReason, DecisionKind, HandDecoder},
    layout::{Hand, KeyUsage, all_slots},
    output::{
        EventState, KeyEvent, OutputAdapter, OutputGateError, OutputReceipt, RecordingOutput,
        ShutdownSignal,
    },
    pipeline::WINDOW_FRAMES,
    protocol::{ALL_SENSORS_VALID, SENSOR_COUNT, SensorFrame, SensorReading},
    runtime::{
        LiveRuntime, RuntimeConfigurationError, RuntimeError, RuntimeOutputError,
        RuntimeStartError, TransportFault,
    },
    simulation::SyntheticConfig,
    transport::encode_transport_frame,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestOutputError {
    Startup,
    PlannedFailure,
    Neutralization,
}

#[derive(Default)]
struct FaultInjectingOutput {
    fail_startup: bool,
    fail_neutralization: bool,
    pressed: bool,
    release_all_calls: u64,
}

impl OutputAdapter for FaultInjectingOutput {
    type Error = TestOutputError;

    fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error> {
        if event.state == EventState::Up {
            return Err(TestOutputError::PlannedFailure);
        }
        self.pressed = true;
        Ok(OutputReceipt { event, ordinal: 0 })
    }

    fn release_all(&mut self) -> Result<(), Self::Error> {
        self.release_all_calls += 1;
        if self.fail_startup && self.release_all_calls == 1 {
            return Err(TestOutputError::Startup);
        }
        if self.fail_neutralization && self.release_all_calls > 1 {
            return Err(TestOutputError::Neutralization);
        }
        self.pressed = false;
        Ok(())
    }
}

fn armed_one_hand<A: OutputAdapter>(decoder: HandDecoder, output: A) -> LiveRuntime<A> {
    let hand = decoder.hand();
    let mut runtime = LiveRuntime::one_hand(decoder, output)
        .unwrap_or_else(|_| panic!("one-hand runtime reaches neutral startup"));
    runtime
        .source_connected(hand)
        .expect("the declared one-hand source connects");
    runtime
        .acquire_run_ownership()
        .expect("the declared source permits run ownership");
    runtime
}

fn armed_two_hand<A: OutputAdapter>(
    left: HandDecoder,
    right: HandDecoder,
    output: A,
) -> LiveRuntime<A> {
    let mut runtime = LiveRuntime::two_hand(left, right, output)
        .unwrap_or_else(|_| panic!("two-hand runtime reaches neutral startup"));
    for hand in [Hand::Left, Hand::Right] {
        runtime
            .source_connected(hand)
            .expect("both declared sources connect");
    }
    runtime
        .acquire_run_ownership()
        .expect("both sources permit run ownership");
    runtime
}

fn append_window(bytes: &mut Vec<u8>, window: &invisible_keyboard::decoder::TapWindow) {
    for frame in window.frames() {
        bytes.extend(encoded_transport(frame));
    }
}

fn encoded_transport(frame: &SensorFrame) -> Vec<u8> {
    encode_transport_frame(frame).expect("valid frame encodes")
}

#[test]
fn one_and_two_hand_runtimes_arm_only_for_their_declared_sources() {
    let left_config = SyntheticConfig::new(Hand::Left, 0x0053_4f55_5243_454c);
    let right_config = SyntheticConfig::new(Hand::Right, 0x0053_4f55_5243_4552);
    let left = HandDecoder::calibrate(Hand::Left, &left_config.calibration_examples(3, 12))
        .expect("left calibration fits");
    let right = HandDecoder::calibrate(Hand::Right, &right_config.calibration_examples(3, 12))
        .expect("right calibration fits");

    let mut one = LiveRuntime::one_hand(left.clone(), RecordingOutput::new())
        .expect("recording output reaches neutral startup");
    assert!(!one.is_armed());
    assert_eq!(one.output().release_all_calls(), 1);
    assert_eq!(
        one.source_connected(Hand::Right),
        Err(OutputGateError::UnexpectedSource)
    );
    assert_eq!(
        one.acquire_run_ownership(),
        Err(OutputGateError::SourcesUnavailable)
    );
    one.source_connected(Hand::Left)
        .expect("the declared hand connects");
    one.acquire_run_ownership()
        .expect("one declared source arms the runtime");
    assert!(one.is_armed());

    let mut two = LiveRuntime::two_hand(left, right, RecordingOutput::new())
        .expect("recording output reaches neutral startup");
    two.source_connected(Hand::Left)
        .expect("left belongs to the two-hand topology");
    assert_eq!(
        two.acquire_run_ownership(),
        Err(OutputGateError::SourcesUnavailable)
    );
    two.source_connected(Hand::Right)
        .expect("right belongs to the two-hand topology");
    two.acquire_run_ownership()
        .expect("both declared sources arm the runtime");
    assert!(two.is_armed());
}

#[test]
fn runtime_construction_fails_when_startup_neutralization_fails() {
    let config = SyntheticConfig::new(Hand::Left, 0x0053_5441_5254_5550);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(3, 12))
        .expect("left calibration fits");
    let output = FaultInjectingOutput {
        fail_startup: true,
        ..FaultInjectingOutput::default()
    };

    assert!(matches!(
        LiveRuntime::one_hand(decoder, output),
        Err(RuntimeStartError::Output(TestOutputError::Startup))
    ));
}

#[test]
fn arbitrary_transport_chunks_reach_one_exact_key_pair() {
    let config = SyntheticConfig::new(Hand::Left, 0x5255_4e54);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut runtime = armed_one_hand(decoder, RecordingOutput::new());
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let mut stream = Vec::new();
    append_window(&mut stream, &config.idle_window(100));
    append_window(&mut stream, &config.tap_window(position, 101));
    append_window(&mut stream, &config.idle_window(102));

    let chunk_sizes = [1, 2, 7, 31, 113, 5, 64];
    let mut offset = 0;
    let mut result_index = 0;
    let mut decisions = Vec::new();
    while offset < stream.len() {
        let size = chunk_sizes[result_index % chunk_sizes.len()];
        let end = (offset + size).min(stream.len());
        decisions.extend(
            runtime
                .push_bytes(&stream[offset..end])
                .into_iter()
                .map(|result| result.expect("valid stream result").decision.kind),
        );
        offset = end;
        result_index += 1;
    }

    assert_eq!(decisions, [DecisionKind::Key(KeyUsage::A)]);
    assert_eq!(runtime.output().events().len(), 2);
    assert_eq!(runtime.output().events()[0].state, EventState::Down);
    assert_eq!(runtime.output().events()[1].state, EventState::Up);
    assert!(runtime.pressed().is_empty());
}

#[test]
fn malformed_transport_forces_independent_all_up_and_resets_context() {
    let config = SyntheticConfig::new(Hand::Right, 0x0046_4155_4c54);
    let decoder = HandDecoder::calibrate(Hand::Right, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut runtime = armed_one_hand(decoder, RecordingOutput::new());

    assert_eq!(
        runtime.push_bytes(&[2, 0]),
        [Err(RuntimeError::Transport(TransportFault::Malformed))]
    );
    assert_eq!(runtime.output().release_all_calls(), 2);
    assert_eq!(runtime.buffered_frames(Hand::Right), Some(0));
    assert!(runtime.pressed().is_empty());
    assert!(runtime.output_neutrality_confirmed());
    assert_eq!(runtime.take_neutralization_failure(), None);
}

#[test]
fn malformed_transport_remains_primary_when_neutralization_fails() {
    let config = SyntheticConfig::new(Hand::Right, 0x5052_494d_4152_5901);
    let decoder = HandDecoder::calibrate(Hand::Right, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let output = FaultInjectingOutput {
        fail_neutralization: true,
        ..FaultInjectingOutput::default()
    };
    let mut runtime = armed_one_hand(decoder, output);

    assert_eq!(
        runtime.push_bytes(&[2, 0]),
        [Err(RuntimeError::Transport(TransportFault::Malformed))]
    );
    assert_eq!(runtime.output().release_all_calls, 2);
    assert!(!runtime.is_armed());
    assert!(runtime.pressed().is_empty());
    assert!(!runtime.output_neutrality_confirmed());
    assert_eq!(
        runtime.take_neutralization_failure(),
        Some(RuntimeOutputError {
            event: None,
            cause: TestOutputError::Neutralization,
            recovery_error: None,
        })
    );
    assert_eq!(
        runtime.acquire_run_ownership(),
        Err(OutputGateError::SessionStopped)
    );
}

#[test]
fn bad_frame_checksum_never_enters_the_decoder() {
    let config = SyntheticConfig::new(Hand::Left, 0x0043_5243_3332);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut runtime = armed_one_hand(decoder, RecordingOutput::new());
    let frame = SensorFrame {
        hand: Hand::Left,
        controller_id_sha256: [0x11; 32],
        boot_id: [0x31; 16],
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence: 1,
        device_time_us: 5_000,
        sensors: [SensorReading::default(); SENSOR_COUNT],
    };
    let mut transport = encoded_transport(&frame);
    transport[1] ^= 1;

    assert!(matches!(
        runtime.push_bytes(&transport).as_slice(),
        [Err(RuntimeError::Frame(_))]
    ));
    assert_eq!(runtime.output().release_all_calls(), 2);
    assert_eq!(runtime.buffered_frames(Hand::Left), Some(0));
}

#[test]
fn sequence_gap_reaches_one_explicit_abstention_and_forces_all_up() {
    let config = SyntheticConfig::new(Hand::Left, 0x5345_5155_454e_4345);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut runtime = armed_one_hand(decoder, RecordingOutput::new());
    let mut frames = config.idle_window(12).frames().to_vec();
    frames[8].sequence = frames[8].sequence.wrapping_add(1);
    let stream = frames
        .iter()
        .flat_map(encoded_transport)
        .collect::<Vec<_>>();

    let results = runtime.push_bytes(&stream);

    assert_eq!(results.len(), 1);
    assert!(matches!(
        &results[0],
        Ok(step)
            if step.decision.kind
                == DecisionKind::Abstain(AbstainReason::SequenceDiscontinuity)
                && step.events.is_empty()
    ));
    assert_eq!(runtime.output().release_all_calls(), 2);
    assert_eq!(runtime.buffered_frames(Hand::Left), Some(0));
    assert!(runtime.pressed().is_empty());
}

#[test]
fn evidence_abstention_remains_primary_when_neutralization_fails() {
    let config = SyntheticConfig::new(Hand::Left, 0x5052_494d_4152_5902);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let output = FaultInjectingOutput {
        fail_neutralization: true,
        ..FaultInjectingOutput::default()
    };
    let mut runtime = armed_one_hand(decoder, output);
    let mut frames = config.idle_window(12).frames().to_vec();
    frames[8].sequence = frames[8].sequence.wrapping_add(1);
    let stream = frames
        .iter()
        .flat_map(encoded_transport)
        .collect::<Vec<_>>();

    let results = runtime.push_bytes(&stream);

    assert_eq!(results.len(), 1);
    assert!(matches!(
        &results[0],
        Ok(step)
            if step.decision.kind
                == DecisionKind::Abstain(AbstainReason::SequenceDiscontinuity)
                && step.events.is_empty()
    ));
    assert_eq!(runtime.output().release_all_calls, 2);
    assert!(!runtime.is_armed());
    assert!(runtime.pressed().is_empty());
    assert!(!runtime.output_neutrality_confirmed());
    assert_eq!(
        runtime.take_neutralization_failure(),
        Some(RuntimeOutputError {
            event: None,
            cause: TestOutputError::Neutralization,
            recovery_error: None,
        })
    );
    assert_eq!(
        runtime.acquire_run_ownership(),
        Err(OutputGateError::SessionStopped)
    );
}

#[test]
fn disconnect_preserves_unterminated_evidence_and_forces_all_up() {
    let config = SyntheticConfig::new(Hand::Left, 0x0045_4f46);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut runtime = armed_one_hand(decoder, RecordingOutput::new());

    assert!(runtime.push_bytes(&[3, 1]).is_empty());
    assert_eq!(
        runtime.disconnect(),
        [Err(RuntimeError::Transport(TransportFault::Unterminated))]
    );
    assert_eq!(runtime.output().release_all_calls(), 3);
    assert_eq!(runtime.buffered_frames(Hand::Left), Some(0));
}

#[test]
fn watchdog_timeout_resets_context_and_invokes_independent_all_up() {
    let config = SyntheticConfig::new(Hand::Left, 0x0057_4154_4348);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut runtime = armed_one_hand(decoder, RecordingOutput::new());
    let window = config.idle_window(10);
    let mut partial = Vec::new();
    for frame in &window.frames()[..8] {
        partial.extend(encoded_transport(frame));
    }
    assert!(runtime.push_bytes(&partial).is_empty());
    assert_eq!(runtime.buffered_frames(Hand::Left), Some(8));

    runtime
        .watchdog_timeout()
        .expect("recording output cannot fail");

    assert_eq!(runtime.buffered_frames(Hand::Left), Some(0));
    assert_eq!(runtime.output().release_all_calls(), 2);
    assert!(runtime.pressed().is_empty());
}

#[test]
fn source_loss_revokes_run_ownership_until_the_declared_source_returns() {
    let config = SyntheticConfig::new(Hand::Left, 0x534f_5552_4345_4c4f);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(3, 12))
        .expect("left calibration fits");
    let mut runtime = armed_one_hand(decoder, RecordingOutput::new());

    runtime
        .source_lost(Hand::Left)
        .expect("source loss forces neutral");

    assert!(!runtime.is_armed());
    assert!(runtime.pressed().is_empty());
    assert_eq!(runtime.output().release_all_calls(), 2);
    assert_eq!(
        runtime.acquire_run_ownership(),
        Err(OutputGateError::SourcesUnavailable)
    );
    runtime
        .source_connected(Hand::Left)
        .expect("the declared source reconnects");
    runtime
        .acquire_run_ownership()
        .expect("the reconnected source permits a fresh run");
    assert!(runtime.is_armed());
}

#[test]
fn planned_shutdown_stops_rearming_and_leaves_final_state_neutral() {
    let config = SyntheticConfig::new(Hand::Right, 0x5348_5554_444f_574e);
    let decoder = HandDecoder::calibrate(Hand::Right, &config.calibration_examples(3, 12))
        .expect("right calibration fits");

    for signal in [ShutdownSignal::Interrupt, ShutdownSignal::Terminate] {
        let mut runtime = armed_one_hand(decoder.clone(), RecordingOutput::new());
        runtime
            .shutdown(signal)
            .expect("recording output accepts planned shutdown");

        assert!(!runtime.is_armed());
        assert!(runtime.pressed().is_empty());
        assert_eq!(runtime.output().release_all_calls(), 2);
        assert_eq!(
            runtime.acquire_run_ownership(),
            Err(OutputGateError::SessionStopped)
        );
    }
}

#[test]
fn adapter_failure_after_key_down_forces_independent_all_up() {
    let config = SyntheticConfig::new(Hand::Left, 0x4f55_5450_5554);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let mut runtime = armed_one_hand(decoder, FaultInjectingOutput::default());
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let mut stream = Vec::new();
    append_window(&mut stream, &config.idle_window(20));
    append_window(&mut stream, &config.tap_window(position, 21));
    append_window(&mut stream, &config.idle_window(22));

    let results = runtime.push_bytes(&stream);

    assert!(results.iter().any(|result| {
        matches!(
            result,
            Err(RuntimeError::Output(RuntimeOutputError {
                event: Some(KeyEvent {
                    state: EventState::Up,
                    ..
                }),
                cause: TestOutputError::PlannedFailure,
                recovery_error: None,
            }))
        )
    }));
    assert!(!runtime.output().pressed);
    assert_eq!(runtime.output().release_all_calls, 2);
    assert!(runtime.pressed().is_empty());
    assert!(runtime.output_neutrality_confirmed());
    assert_eq!(runtime.take_neutralization_failure(), None);
}

#[test]
fn adapter_failure_preserves_the_cause_and_independent_recovery_failure() {
    let config = SyntheticConfig::new(Hand::Left, 0x5245_434f_5645_5259);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
        .expect("complete synthetic calibration fits");
    let output = FaultInjectingOutput {
        fail_neutralization: true,
        ..FaultInjectingOutput::default()
    };
    let mut runtime = armed_one_hand(decoder, output);
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let mut stream = Vec::new();
    append_window(&mut stream, &config.idle_window(20));
    append_window(&mut stream, &config.tap_window(position, 21));
    append_window(&mut stream, &config.idle_window(22));

    let failure = runtime
        .push_bytes(&stream)
        .into_iter()
        .find_map(Result::err)
        .expect("the key-up adapter failure is returned");

    assert!(matches!(
        failure,
        RuntimeError::Output(RuntimeOutputError {
            event: Some(KeyEvent {
                state: EventState::Up,
                ..
            }),
            cause: TestOutputError::PlannedFailure,
            recovery_error: Some(TestOutputError::Neutralization),
        })
    ));
    assert!(runtime.pressed().is_empty());
    assert!(!runtime.output_neutrality_confirmed());
    assert!(!runtime.is_armed());
    assert_eq!(
        runtime.acquire_run_ownership(),
        Err(OutputGateError::SessionStopped)
    );
}

#[test]
fn two_hand_runtime_dispatches_both_decoders_through_one_output_state() {
    let left_config = SyntheticConfig::new(Hand::Left, 0x4c45_4654);
    let right_config = SyntheticConfig::new(Hand::Right, 0x0052_4947_4854);
    let left = HandDecoder::calibrate(Hand::Left, &left_config.calibration_examples(5, 20))
        .expect("left calibration fits");
    let right = HandDecoder::calibrate(Hand::Right, &right_config.calibration_examples(5, 20))
        .expect("right calibration fits");
    let mut runtime = armed_two_hand(left, right, RecordingOutput::new());
    let left_position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let right_position = all_slots()
        .iter()
        .find(|slot| slot.label == "H")
        .expect("H exists")
        .position;
    let mut stream = Vec::new();
    append_window(&mut stream, &left_config.idle_window(100));
    append_window(&mut stream, &left_config.tap_window(left_position, 101));
    append_window(&mut stream, &left_config.idle_window(102));
    append_window(&mut stream, &right_config.idle_window(200));
    append_window(&mut stream, &right_config.tap_window(right_position, 201));
    append_window(&mut stream, &right_config.idle_window(202));

    let decisions = runtime
        .push_bytes(&stream)
        .into_iter()
        .map(|step| step.expect("two-hand stream stays valid").decision.kind)
        .collect::<Vec<_>>();

    assert_eq!(
        decisions,
        [
            DecisionKind::Key(KeyUsage::A),
            DecisionKind::Key(KeyUsage::H)
        ]
    );
    assert_eq!(runtime.output().events().len(), 4);
    assert!(runtime.pressed().is_empty());
    for hand in [Hand::Left, Hand::Right] {
        assert!(
            runtime
                .buffered_frames(hand)
                .is_some_and(|frames| (1..=WINDOW_FRAMES).contains(&frames))
        );
    }
}

#[test]
fn two_hand_runtime_rejects_swapped_or_duplicate_decoder_topology() {
    let left_config = SyntheticConfig::new(Hand::Left, 0x4c45_4654_544f_504f);
    let right_config = SyntheticConfig::new(Hand::Right, 0x5249_4748_544f_504f);
    let left = HandDecoder::calibrate(Hand::Left, &left_config.calibration_examples(3, 12))
        .expect("left calibration fits");
    let right = HandDecoder::calibrate(Hand::Right, &right_config.calibration_examples(3, 12))
        .expect("right calibration fits");

    assert!(matches!(
        LiveRuntime::two_hand(right.clone(), left.clone(), RecordingOutput::new()),
        Err(RuntimeStartError::Configuration(
            RuntimeConfigurationError::ExpectedHand {
                expected: Hand::Left,
                actual: Hand::Right
            }
        ))
    ));
    assert!(matches!(
        LiveRuntime::two_hand(left.clone(), left, RecordingOutput::new()),
        Err(RuntimeStartError::Configuration(
            RuntimeConfigurationError::ExpectedHand {
                expected: Hand::Right,
                actual: Hand::Left
            }
        ))
    ));
}

#[test]
fn decoded_frame_runtime_keeps_raw_offsets_and_projects_commit_time() {
    let config = SyntheticConfig::new(Hand::Left, 0x5255_4e54).with_noise(0);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20)).unwrap();
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .unwrap()
        .position;
    let idle = config.idle_window(100).frames()[0].sensors;
    let impulse = config.tap_window(position, 101).frames()[15].sensors;
    let template = config.idle_window(100).frames()[0];
    for slope_ns_per_us in [999_u64, 1_001] {
        let mut runtime = armed_one_hand(decoder.clone(), RecordingOutput::new());
        let mut key_step = None;
        for index in 0..=48_u32 {
            let mut frame = template;
            frame.sequence = index;
            frame.device_time_us = 1_000_000 + u64::from(index) * 5_000 + u64::from(index >= 20);
            frame.sensors = if index == 32 { impulse } else { idle };
            if index == 20 {
                frame.sensors[0].sample_time_offset_us = -5_000;
            }
            let output_time_us = (5_000_000_000 + frame.device_time_us * slope_ns_per_us) / 1_000;
            if let Some(step) = runtime.push_frame(frame, output_time_us) {
                let step = step.expect(
                    "valid raw sensor progress remains valid under either output-clock slope",
                );
                assert_eq!(step.decision.decision_time_us, output_time_us);
                assert!(
                    key_step.replace(step).is_none(),
                    "the impulse commits only once"
                );
            }
        }
        let step = key_step.expect("the stream commits the synthetic A tap");
        let commit_time_us =
            (5_000_000_000 + (1_000_000 + 35 * 5_000 + 1) * slope_ns_per_us) / 1_000;
        assert_eq!(step.decision.kind, DecisionKind::Key(KeyUsage::A));
        assert_eq!(step.decision.decision_time_us, commit_time_us);
        assert_eq!(
            step.events,
            [
                KeyEvent::down(KeyUsage::A, commit_time_us),
                KeyEvent::up(KeyUsage::A, commit_time_us)
            ]
        );
        assert_eq!(runtime.output().events(), step.events);
        assert!(runtime.pressed().is_empty());
        let mut invalid = template;
        invalid.sequence = 49;
        invalid.device_time_us = 1_000_000 + 49 * 5_000 + 1;
        invalid.valid_sensor_mask = 0;
        let invalid_output_time_us =
            (5_000_000_000 + invalid.device_time_us * slope_ns_per_us) / 1_000;
        let invalid_step = runtime
            .push_frame(invalid, invalid_output_time_us)
            .unwrap()
            .unwrap();
        assert_eq!(
            invalid_step.decision.kind,
            DecisionKind::Abstain(AbstainReason::MissingSensor)
        );
        assert_eq!(
            invalid_step.decision.decision_time_us,
            invalid_output_time_us
        );
        assert!(invalid_step.events.is_empty());
        assert!(!runtime.is_armed());
        assert_eq!(runtime.output().release_all_calls(), 2);
        assert!(runtime.output_neutrality_confirmed());
    }
}

#[test]
fn byte_and_candidate_runtime_paths_preserve_raw_commit_timestamps() {
    let config = SyntheticConfig::new(Hand::Left, 0x5255_4e54).with_noise(0);
    let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20)).unwrap();
    let position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .unwrap()
        .position;
    let idle = config.idle_window(100).frames()[0].sensors;
    let impulse = config.tap_window(position, 101).frames()[15].sensors;
    let template = config.idle_window(100).frames()[0];
    let mut bytes_runtime = armed_one_hand(decoder.clone(), RecordingOutput::new());
    let mut candidate_runtime = armed_one_hand(decoder, RecordingOutput::new());
    let mut byte_steps = Vec::new();
    let mut candidate_steps = Vec::new();
    for index in 0..=48_u32 {
        let mut frame = template;
        frame.sequence = index;
        frame.device_time_us = 1_000_000 + u64::from(index) * 5_000;
        frame.sensors = if index == 32 { impulse } else { idle };
        byte_steps.extend(
            bytes_runtime
                .push_bytes(&encoded_transport(&frame))
                .into_iter()
                .map(Result::unwrap),
        );
        if let Some(step) = candidate_runtime.push_candidate(
            invisible_keyboard::transport::TransportCandidate::Decoded(
                invisible_keyboard::protocol::encode_frame(&frame)
                    .unwrap()
                    .to_vec(),
            ),
        ) {
            candidate_steps.push(step.unwrap());
        }
    }
    assert_eq!(byte_steps, candidate_steps);
    assert_eq!(byte_steps.len(), 1);
    assert_eq!(byte_steps[0].decision.kind, DecisionKind::Key(KeyUsage::A));
    let commit_time_us = 1_000_000 + 35 * 5_000;
    assert_eq!(byte_steps[0].decision.decision_time_us, commit_time_us);
    assert_eq!(
        bytes_runtime.output().events(),
        [
            KeyEvent::down(KeyUsage::A, commit_time_us),
            KeyEvent::up(KeyUsage::A, commit_time_us)
        ]
    );
    assert_eq!(
        bytes_runtime.output().events(),
        candidate_runtime.output().events()
    );
}
