//! Reproducible synthetic stress scorecards.
//!
//! These reports validate the reference stack against an explicit generator.
//! They never qualify physical sensing or product usability.

use crate::{
    clock::percentile,
    decoder::{AbstainReason, CalibrationError, DecisionKind, HandDecoder},
    layout::{Finger, Hand, PhysicalPosition, all_slots},
    output::{EventState, KeyStateMachine, RecordingOutput, emit_events},
    pipeline::{CausalPipeline, DECISION_DELAY_FRAMES},
    simulation::SyntheticConfig,
};

const WINDOW_EXPOSURE_US: u64 = 80_000;
const ORDINARY_WINDOWS_PER_HAND: u64 = 45_000;

/// Counts for one declared synthetic perturbation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticScenarioReport {
    pub name: &'static str,
    pub intentional_positions: u64,
    pub position_errors: u64,
    pub key_errors: u64,
    pub abstentions: u64,
    pub reserved_rejections: u64,
    pub false_activations: u64,
}

/// First observed failure boundary in one deterministic synthetic sweep.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticBreakpointReport {
    pub dimension: &'static str,
    pub unit: &'static str,
    pub levels_tested: usize,
    pub last_zero_failure_level: Option<u32>,
    pub first_failure_level: Option<u32>,
    pub failures_at_first_level: u64,
}

/// Device-time latency through the deterministic synthetic software path.
///
/// Each sample starts at the first contact-bearing generated frame and ends
/// when the in-memory output adapter accepts the resulting key-down event.
/// It includes causal window completion and the post-trigger decision delay.
/// It does not measure host execution time or the kernel uinput path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimulatedSensorToOutputLatencyReport {
    pub evidence_label: &'static str,
    pub limitations: &'static str,
    pub sample_count: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

/// Hardware-independent stress evidence produced by the built-in generator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntheticStressReport {
    pub evidence_label: &'static str,
    pub limitations: &'static str,
    pub nominal: SyntheticScenarioReport,
    pub scenarios: Vec<SyntheticScenarioReport>,
    pub breakpoints: Vec<SyntheticBreakpointReport>,
    pub simulated_sensor_to_output_latency: SimulatedSensorToOutputLatencyReport,
    pub ordinary_motion_exposure_us: u64,
    pub ordinary_motion_false_activations: u64,
    pub ordinary_motion_zero_event_upper_95_milli_per_hour: Option<u64>,
    pub simultaneous_windows: u64,
    pub simultaneous_abstentions: u64,
    pub dropout_windows: u64,
    pub dropout_abstentions: u64,
    pub whole_hand_dropout_windows: u64,
    pub whole_hand_dropout_abstentions: u64,
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Nominal,
    Noise,
    Remount,
    PacketLoss,
    ClockDrift,
}

impl Scenario {
    const fn name(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Noise => "noise",
            Self::Remount => "remount",
            Self::PacketLoss => "packet-loss",
            Self::ClockDrift => "clock-drift",
        }
    }

    fn config(self, hand: Hand, seed: u64) -> SyntheticConfig {
        let base = SyntheticConfig::new(hand, seed);
        match self {
            Self::Nominal => base.with_noise(12).with_remount_per_mille(80),
            Self::Noise => base.with_noise(800),
            Self::Remount => base.with_noise(12).with_remount_per_mille(300),
            Self::PacketLoss => base.with_packet_loss_stride(4),
            Self::ClockDrift => base.with_clock_drift_ppm(10_000),
        }
    }
}

/// Run the fixed-seed synthetic stress suite.
///
/// # Errors
///
/// Returns [`CalibrationError`] if the generator no longer supplies a complete
/// and separable calibration contract. That is a regression in the fixture,
/// not evidence about physical sensing.
pub fn run_synthetic_stress_suite() -> Result<SyntheticStressReport, CalibrationError> {
    let left_decoder = decoder_for(Hand::Left, 0x1000)?;
    let right_decoder = decoder_for(Hand::Right, 0x2000)?;

    let nominal = evaluate_scenario(Scenario::Nominal, &left_decoder, &right_decoder, 0x3000);
    let scenarios = [
        Scenario::Noise,
        Scenario::Remount,
        Scenario::PacketLoss,
        Scenario::ClockDrift,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, scenario)| {
        evaluate_scenario(
            scenario,
            &left_decoder,
            &right_decoder,
            0x4000 + u64::try_from(index).unwrap_or(u64::MAX - 0x4000),
        )
    })
    .collect();

    let (ordinary_motion_exposure_us, ordinary_motion_false_activations) =
        evaluate_ordinary_motion(&left_decoder, &right_decoder);
    let (simultaneous_windows, simultaneous_abstentions) =
        evaluate_simultaneous(&left_decoder, &right_decoder);
    let (dropout_windows, dropout_abstentions) = evaluate_dropout(&left_decoder, &right_decoder);
    let (whole_hand_dropout_windows, whole_hand_dropout_abstentions) =
        evaluate_whole_hand_dropout(&left_decoder, &right_decoder);
    let breakpoints = evaluate_breakpoints(&left_decoder, &right_decoder);
    let simulated_sensor_to_output_latency =
        evaluate_simulated_sensor_to_output_latency(&left_decoder, &right_decoder);

    Ok(SyntheticStressReport {
        evidence_label: "simulated",
        limitations: "This deterministic orthogonal-signal generator does not establish that real finger motion contains separable key information.",
        nominal,
        scenarios,
        breakpoints,
        simulated_sensor_to_output_latency,
        ordinary_motion_exposure_us,
        ordinary_motion_false_activations,
        ordinary_motion_zero_event_upper_95_milli_per_hour: (ordinary_motion_false_activations
            == 0)
            .then_some(3_000_u64.saturating_mul(3_600_000_000) / ordinary_motion_exposure_us),
        simultaneous_windows,
        simultaneous_abstentions,
        dropout_windows,
        dropout_abstentions,
        whole_hand_dropout_windows,
        whole_hand_dropout_abstentions,
    })
}

fn evaluate_simulated_sensor_to_output_latency(
    left_decoder: &HandDecoder,
    right_decoder: &HandDecoder,
) -> SimulatedSensorToOutputLatencyReport {
    let samples = all_slots()
        .iter()
        .filter_map(|slot| slot.usage.map(|usage| (slot.position, usage)))
        .enumerate()
        .map(|(trial, (position, expected_usage))| {
            let decoder = match position.hand {
                Hand::Left => left_decoder,
                Hand::Right => right_decoder,
            };
            let trial = u64::try_from(trial).expect("Core-28 trial index fits u64");
            let config = SyntheticConfig::new(position.hand, 0xe000 + trial)
                .with_noise(0)
                .with_shared_motion(0);
            simulated_contact_to_output_latency_us(
                decoder,
                &config,
                position,
                expected_usage,
                trial,
            )
        })
        .collect();

    SimulatedSensorToOutputLatencyReport::from_samples(samples)
}

fn simulated_contact_to_output_latency_us(
    decoder: &HandDecoder,
    config: &SyntheticConfig,
    position: PhysicalPosition,
    expected_usage: crate::layout::KeyUsage,
    trial: u64,
) -> u64 {
    let tap = config.tap_window(position, trial);
    let idle = config.idle_window(trial);
    let source_event_time_us = tap
        .frames()
        .iter()
        .zip(idle.frames())
        .find_map(|(tap_frame, idle_frame)| {
            (tap_frame.sensors != idle_frame.sensors).then_some(tap_frame.device_time_us)
        })
        .expect("synthetic tap has a contact-bearing frame");

    // Synthetic trials are one second apart for fixture isolation. Rebase the
    // next generated idle frames onto the tap's sampling cadence so the stream
    // remains continuous while preserving their generated sensor contents.
    let period_us = tap.frames()[1]
        .device_time_us
        .checked_sub(tap.frames()[0].device_time_us)
        .expect("synthetic frame time is monotonic");
    let next_time_us = tap
        .frames()
        .last()
        .expect("synthetic tap window is nonempty")
        .device_time_us
        .saturating_add(period_us);
    let continuation = config.idle_window(trial.saturating_add(1));
    let continuation_origin_us = continuation.frames()[0].device_time_us;
    let mut stream = tap.frames().to_vec();
    stream.extend(
        continuation
            .frames()
            .iter()
            .take(usize::from(DECISION_DELAY_FRAMES))
            .copied()
            .map(|mut frame| {
                frame.device_time_us = next_time_us
                    .saturating_add(frame.device_time_us.saturating_sub(continuation_origin_us));
                frame
            }),
    );

    let mut pipeline = CausalPipeline::new(decoder.clone());
    let mut keys = KeyStateMachine::new();
    let mut output = RecordingOutput::new();
    let mut emitted_key_down_time_us = None;
    for frame in stream {
        let Some(decision) = pipeline
            .push(frame)
            .expect("generated latency stream satisfies the pipeline contract")
        else {
            continue;
        };
        let DecisionKind::Key(actual_usage) = decision.kind else {
            panic!("generated latency tap must produce a key decision");
        };
        assert_eq!(
            actual_usage, expected_usage,
            "latency tap decoded wrong key"
        );

        let applied = keys.tap(actual_usage, decision.decision_time_us);
        assert!(
            applied.anomaly.is_none(),
            "latency tap must preserve neutral key state"
        );
        let receipts = emit_events(&mut output, &applied.events)
            .expect("recording output adapter is infallible");
        assert_eq!(receipts.len(), 2, "Core-28 tap emits one down/up pair");
        assert_eq!(receipts[0].event.state, EventState::Down);
        assert_eq!(receipts[1].event.state, EventState::Up);
        assert!(
            emitted_key_down_time_us
                .replace(receipts[0].event.time_us)
                .is_none(),
            "one synthetic tap must emit exactly once"
        );
    }
    assert_eq!(output.events().len(), 2, "adapter accepts one down/up pair");
    assert!(
        keys.pressed().is_empty(),
        "tap must finish in neutral state"
    );

    emitted_key_down_time_us
        .expect("pipeline emits the generated tap")
        .checked_sub(source_event_time_us)
        .expect("output event cannot precede synthetic contact")
}

impl SimulatedSensorToOutputLatencyReport {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        assert!(
            !samples.is_empty(),
            "latency score requires Core-28 samples"
        );
        let sample_count = u64::try_from(samples.len()).expect("latency sample count fits u64");
        let p50_us = percentile(&mut samples, 50);
        let p95_us = percentile(&mut samples, 95);
        let p99_us = percentile(&mut samples, 99);
        let max_us = *samples.iter().max().expect("latency samples are nonempty");
        Self {
            evidence_label: "simulated",
            limitations: "Synthetic device-time only; this is not host processing time and not a kernel uinput round-trip.",
            sample_count,
            p50_us,
            p95_us,
            p99_us,
            max_us,
        }
    }
}

fn decoder_for(hand: Hand, seed: u64) -> Result<HandDecoder, CalibrationError> {
    let config = SyntheticConfig::new(hand, seed);
    HandDecoder::calibrate(hand, &config.calibration_examples(5, 20))
}

fn evaluate_scenario(
    scenario: Scenario,
    left_decoder: &HandDecoder,
    right_decoder: &HandDecoder,
    seed: u64,
) -> SyntheticScenarioReport {
    let left_config = scenario.config(Hand::Left, seed);
    let right_config = scenario.config(Hand::Right, seed.saturating_add(1));
    evaluate_configs(
        scenario.name(),
        left_decoder,
        right_decoder,
        &left_config,
        &right_config,
    )
}

fn evaluate_configs(
    name: &'static str,
    left_decoder: &HandDecoder,
    right_decoder: &HandDecoder,
    left_config: &SyntheticConfig,
    right_config: &SyntheticConfig,
) -> SyntheticScenarioReport {
    let mut report = SyntheticScenarioReport {
        name,
        intentional_positions: 0,
        position_errors: 0,
        key_errors: 0,
        abstentions: 0,
        reserved_rejections: 0,
        false_activations: 0,
    };

    for hand in [Hand::Left, Hand::Right] {
        let decoder = match hand {
            Hand::Left => left_decoder,
            Hand::Right => right_decoder,
        };
        let config = match hand {
            Hand::Left => left_config,
            Hand::Right => right_config,
        };
        for (trial, slot) in all_slots()
            .iter()
            .filter(|slot| slot.position.hand == hand)
            .enumerate()
        {
            report.intentional_positions += 1;
            let window = config.tap_window(
                slot.position,
                u64::try_from(trial).expect("position trial fits u64"),
            );
            let decision = decoder.decode(&window);
            if decision.position != Some(slot.position) {
                report.position_errors += 1;
            }
            match (slot.usage, decision.kind) {
                (Some(expected), DecisionKind::Key(actual)) if expected == actual => {}
                (Some(_), DecisionKind::Key(_)) => report.key_errors += 1,
                (None, DecisionKind::Abstain(AbstainReason::ReservedPosition)) => {
                    report.reserved_rejections += 1;
                }
                (None, DecisionKind::Key(_)) => report.false_activations += 1,
                (_, DecisionKind::Abstain(_)) => report.abstentions += 1,
            }
        }
    }
    report
}

fn evaluate_breakpoints(
    left_decoder: &HandDecoder,
    right_decoder: &HandDecoder,
) -> Vec<SyntheticBreakpointReport> {
    let noise_levels = [
        0, 250, 500, 1_000, 2_000, 4_000, 6_000, 8_000, 12_000, 16_000,
    ];
    let remount_levels = [0, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1_000];
    let contact_loss_levels = [0, 1, 2, 3, 4];
    let impulse_levels = [0, 250, 500, 1_000, 2_000, 4_000, 8_000, 12_000];
    let bias_levels = [0, 250, 500, 1_000, 2_000, 4_000, 8_000, 12_000, 16_000];

    vec![
        characterize_breakpoint("noise-amplitude", "sensor-counts", &noise_levels, |level| {
            let amplitude = i16::try_from(level).unwrap_or(i16::MAX);
            position_failures(
                left_decoder,
                right_decoder,
                SyntheticConfig::new(Hand::Left, 0x8000).with_noise(amplitude),
                SyntheticConfig::new(Hand::Right, 0x8001).with_noise(amplitude),
            )
        }),
        characterize_breakpoint("remount-axis-mix", "per-mille", &remount_levels, |level| {
            let amount = u16::try_from(level).unwrap_or(u16::MAX);
            position_failures(
                left_decoder,
                right_decoder,
                SyntheticConfig::new(Hand::Left, 0x9000).with_remount_per_mille(amount),
                SyntheticConfig::new(Hand::Right, 0x9001).with_remount_per_mille(amount),
            )
        }),
        characterize_breakpoint(
            "contact-tail-loss",
            "samples",
            &contact_loss_levels,
            |level| {
                let samples = usize::try_from(level).unwrap_or(usize::MAX);
                position_failures(
                    left_decoder,
                    right_decoder,
                    SyntheticConfig::new(Hand::Left, 0xa000).with_contact_tail_loss(samples),
                    SyntheticConfig::new(Hand::Right, 0xa001).with_contact_tail_loss(samples),
                )
            },
        ),
        characterize_breakpoint(
            "unmodeled-finger-impulse",
            "sensor-counts",
            &impulse_levels,
            |level| {
                unmodeled_impulse_false_activations(
                    left_decoder,
                    right_decoder,
                    i16::try_from(level).unwrap_or(i16::MAX),
                )
            },
        ),
        characterize_breakpoint(
            "differential-bias-ramp",
            "sensor-counts",
            &bias_levels,
            |level| {
                let amplitude = i16::try_from(level).unwrap_or(i16::MAX);
                position_failures(
                    left_decoder,
                    right_decoder,
                    SyntheticConfig::new(Hand::Left, 0xc000).with_differential_bias_ramp(amplitude),
                    SyntheticConfig::new(Hand::Right, 0xc001)
                        .with_differential_bias_ramp(amplitude),
                )
            },
        ),
    ]
}

fn characterize_breakpoint(
    dimension: &'static str,
    unit: &'static str,
    levels: &[u32],
    mut failures_at: impl FnMut(u32) -> u64,
) -> SyntheticBreakpointReport {
    let mut last_zero_failure_level = None;
    let mut first_failure_level = None;
    let mut failures_at_first_level = 0;
    for &level in levels {
        let failures = failures_at(level);
        if failures == 0 && first_failure_level.is_none() {
            last_zero_failure_level = Some(level);
        } else if failures > 0 && first_failure_level.is_none() {
            first_failure_level = Some(level);
            failures_at_first_level = failures;
        }
    }
    SyntheticBreakpointReport {
        dimension,
        unit,
        levels_tested: levels.len(),
        last_zero_failure_level,
        first_failure_level,
        failures_at_first_level,
    }
}

fn position_failures(
    left_decoder: &HandDecoder,
    right_decoder: &HandDecoder,
    left_config: SyntheticConfig,
    right_config: SyntheticConfig,
) -> u64 {
    evaluate_configs(
        "breakpoint",
        left_decoder,
        right_decoder,
        &left_config,
        &right_config,
    )
    .position_errors
}

fn unmodeled_impulse_false_activations(
    left_decoder: &HandDecoder,
    right_decoder: &HandDecoder,
    amplitude: i16,
) -> u64 {
    let mut false_activations = 0;
    for hand in [Hand::Left, Hand::Right] {
        let decoder = match hand {
            Hand::Left => left_decoder,
            Hand::Right => right_decoder,
        };
        let config = SyntheticConfig::new(hand, 0xb000 + u64::from(hand as u8));
        for (trial, finger) in [
            Finger::Thumb,
            Finger::Index,
            Finger::Middle,
            Finger::Ring,
            Finger::Pinky,
        ]
        .into_iter()
        .enumerate()
        {
            let window = config.unmodeled_finger_impulse_window(
                finger,
                amplitude,
                u64::try_from(trial).unwrap_or(u64::MAX),
            );
            if matches!(decoder.decode(&window).kind, DecisionKind::Key(_)) {
                false_activations += 1;
            }
        }
    }
    false_activations
}

fn evaluate_ordinary_motion(left_decoder: &HandDecoder, right_decoder: &HandDecoder) -> (u64, u64) {
    let mut false_activations = 0;
    for hand in [Hand::Left, Hand::Right] {
        let decoder = match hand {
            Hand::Left => left_decoder,
            Hand::Right => right_decoder,
        };
        let config =
            SyntheticConfig::new(hand, 0x5000 + u64::from(hand as u8)).with_shared_motion(4_000);
        for trial in 0..ORDINARY_WINDOWS_PER_HAND {
            if matches!(
                decoder.decode(&config.idle_window(trial)).kind,
                DecisionKind::Key(_)
            ) {
                false_activations += 1;
            }
        }
    }
    (
        WINDOW_EXPOSURE_US * ORDINARY_WINDOWS_PER_HAND,
        false_activations,
    )
}

fn evaluate_simultaneous(left: &HandDecoder, right: &HandDecoder) -> (u64, u64) {
    let mut abstentions = 0;
    for hand in [Hand::Left, Hand::Right] {
        let decoder = match hand {
            Hand::Left => left,
            Hand::Right => right,
        };
        let positions = two_finger_positions(hand);
        let config = SyntheticConfig::new(hand, 0x6000 + u64::from(hand as u8));
        let decision = decoder.decode(&config.simultaneous_window(positions.0, positions.1, 1));
        if decision.kind == DecisionKind::Abstain(AbstainReason::AmbiguousFinger) {
            abstentions += 1;
        }
    }
    (2, abstentions)
}

fn evaluate_dropout(left: &HandDecoder, right: &HandDecoder) -> (u64, u64) {
    let mut windows = 0;
    let mut abstentions = 0;
    for hand in [Hand::Left, Hand::Right] {
        let decoder = match hand {
            Hand::Left => left,
            Hand::Right => right,
        };
        let position = all_slots()
            .iter()
            .find(|slot| slot.position.hand == hand && slot.usage.is_some())
            .expect("each hand has an enabled position")
            .position;
        for sensor in 0..6 {
            windows += 1;
            let config = SyntheticConfig::new(
                hand,
                0x7000 + u64::from(hand as u8) + u64::try_from(sensor).expect("sensor fits"),
            )
            .with_missing_sensor(sensor);
            let decision = decoder.decode(&config.tap_window(position, 1));
            if decision.kind == DecisionKind::Abstain(AbstainReason::MissingSensor) {
                abstentions += 1;
            }
        }
    }
    (windows, abstentions)
}

fn evaluate_whole_hand_dropout(left: &HandDecoder, right: &HandDecoder) -> (u64, u64) {
    let mut abstentions = 0;
    for hand in [Hand::Left, Hand::Right] {
        let decoder = match hand {
            Hand::Left => left,
            Hand::Right => right,
        };
        let position = all_slots()
            .iter()
            .find(|slot| slot.position.hand == hand && slot.usage.is_some())
            .expect("each hand has an enabled position")
            .position;
        let config =
            SyntheticConfig::new(hand, 0xd000 + u64::from(hand as u8)).with_all_sensors_missing();
        if decoder.decode(&config.tap_window(position, 1)).kind
            == DecisionKind::Abstain(AbstainReason::MissingSensor)
        {
            abstentions += 1;
        }
    }
    (2, abstentions)
}

fn two_finger_positions(hand: Hand) -> (PhysicalPosition, PhysicalPosition) {
    let mut positions = all_slots()
        .iter()
        .filter(|slot| slot.position.hand == hand && slot.usage.is_some())
        .map(|slot| slot.position);
    let first = positions.next().expect("hand has enabled positions");
    let second = positions
        .find(|position| position.finger != first.finger)
        .expect("hand has more than one enabled finger");
    (first, second)
}
