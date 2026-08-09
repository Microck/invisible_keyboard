//! Causal personal calibration and an inspectable per-finger baseline decoder.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    layout::{Finger, Hand, KeyUsage, PhysicalPosition, all_slots, usage_for_position},
    protocol::{
        ALL_SENSORS_VALID, FLAG_ACQUISITION_OVERRUN, FLAG_CLOCK_UNSYNCED, SENSOR_COUNT, SensorFrame,
    },
};

const FINGER_SENSOR_COUNT: usize = 5;
const DORSAL_SENSOR_INDEX: usize = 5;
pub const FEATURE_COUNT: usize = 7;
const MIN_POSITION_EXAMPLES: usize = 3;
const MIN_IDLE_EXAMPLES: usize = 3;
pub const FINGER_ENERGY_RATIO: f64 = 1.5;
pub const DISTANCE_MARGIN: f64 = 1.25;
pub const VARIANCE_FLOOR: f64 = 0.0025;

/// Provisional maximum sample-time spread within one six-sensor aggregate.
///
/// Six free-running 208 Hz sensors can differ by almost one 4.81 ms period.
/// Version 1 records that phase explicitly and uses the Gate 0 ceiling here.
pub const MAX_INTER_SENSOR_SKEW_US: i32 = 5_000;

/// Bounded integrity state for one identity-locked controller stream.
///
/// Keep this state across capture stages and clock admission: a new window or
/// worker does not reset the controller's sequence or sample-time history.
/// Aggregate and valid per-sensor times must strictly advance in raw device
/// microseconds. A clock mapping cannot establish controller continuity.
#[derive(Debug, Default)]
pub(crate) struct FrameIntegrity {
    previous_sequence: Option<u32>,
    previous_device_time_us: Option<u64>,
    previous_sensor_times_us: [Option<u64>; SENSOR_COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameIntegrityError {
    Quality(AbstainReason),
    Structure(WindowError),
}

impl FrameIntegrity {
    pub(crate) fn observe(&mut self, frame: &SensorFrame) -> Result<(), FrameIntegrityError> {
        if let Some(reason) = frame_quality_issue(frame) {
            return Err(FrameIntegrityError::Quality(reason));
        }
        if self
            .expected_sequence()
            .is_some_and(|expected| frame.sequence != expected)
        {
            return Err(FrameIntegrityError::Quality(
                AbstainReason::SequenceDiscontinuity,
            ));
        }
        self.observe_timestamps(frame)
            .map_err(FrameIntegrityError::Structure)?;
        self.previous_sequence = Some(frame.sequence);
        Ok(())
    }

    /// The next raw sequence, unchanged when admission rejects a frame.
    pub(crate) fn expected_sequence(&self) -> Option<u32> {
        self.previous_sequence
            .map(|previous| previous.wrapping_add(1))
    }

    fn observe_timestamps(&mut self, frame: &SensorFrame) -> Result<(), WindowError> {
        if self
            .previous_device_time_us
            .is_some_and(|previous| frame.device_time_us <= previous)
        {
            return Err(WindowError::TimeRegression);
        }
        let mut sensor_times_us = [None; SENSOR_COUNT];
        for (sensor, sample_time_us) in sensor_times_us.iter_mut().enumerate() {
            if frame.valid_sensor_mask & (1_u8 << sensor) == 0 {
                continue;
            }
            let time_us = frame
                .device_time_us
                .checked_add_signed(i64::from(frame.sensors[sensor].sample_time_offset_us))
                .ok_or(WindowError::SampleTimeOutOfRange { sensor })?;
            if self.previous_sensor_times_us[sensor].is_some_and(|previous| time_us <= previous) {
                return Err(WindowError::SensorTimeRegression { sensor });
            }
            *sample_time_us = Some(time_us);
        }
        self.previous_device_time_us = Some(frame.device_time_us);
        for (previous, current) in self
            .previous_sensor_times_us
            .iter_mut()
            .zip(sensor_times_us)
        {
            if current.is_some() {
                *previous = current;
            }
        }
        Ok(())
    }
}

/// A causal sensor window. The final frame is the decision time; no later
/// frame may be added after a decision has been scored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TapWindow {
    frames: Vec<SensorFrame>,
    hand: Hand,
}

/// A validated view over one or two adjacent frame slices.
///
/// `VecDeque::as_slices` exposes rolling storage in this shape. Keeping the
/// view borrowed lets the live pipeline validate and decode its logical frame
/// order without collecting a new owned window for every sample.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BorrowedTapWindow<'a> {
    first: &'a [SensorFrame],
    second: &'a [SensorFrame],
    hand: Hand,
}

impl<'a> BorrowedTapWindow<'a> {
    pub(crate) fn new(
        first: &'a [SensorFrame],
        second: &'a [SensorFrame],
    ) -> Result<Self, WindowError> {
        let hand = first
            .first()
            .or_else(|| second.first())
            .ok_or(WindowError::Empty)?
            .hand;
        let window = Self {
            first,
            second,
            hand,
        };
        let mut continuity = FrameIntegrity::default();
        for frame in window.iter() {
            if frame.hand != hand {
                return Err(WindowError::MixedHands {
                    expected: hand,
                    actual: frame.hand,
                });
            }
            continuity.observe_timestamps(frame)?;
        }
        Ok(window)
    }

    fn iter(self) -> impl Iterator<Item = &'a SensorFrame> + Clone {
        self.first.iter().chain(self.second)
    }

    fn frame(self, index: usize) -> &'a SensorFrame {
        match self.first.get(index) {
            Some(frame) => frame,
            None => &self.second[index - self.first.len()],
        }
    }

    fn len(self) -> usize {
        self.first.len() + self.second.len()
    }

    const fn hand(self) -> Hand {
        self.hand
    }

    fn decision_time_us(self) -> u64 {
        self.second
            .last()
            .or_else(|| self.first.last())
            .expect("validated windows are nonempty")
            .device_time_us
    }
}

impl TapWindow {
    /// Validate a nonempty, one-hand window with strictly advancing raw times.
    ///
    /// # Errors
    ///
    /// Returns [`WindowError`] for an empty window, mixed hands, nonincreasing
    /// raw frame or sensor times, or an out-of-range sensor timestamp.
    /// Sequence gaps remain valid and visible because
    /// the decoder must handle captured packet loss explicitly.
    pub fn new(frames: Vec<SensorFrame>) -> Result<Self, WindowError> {
        let hand = BorrowedTapWindow::new(&frames, &[])?.hand();
        Ok(Self { frames, hand })
    }

    /// Return the hand shared by every frame.
    #[must_use]
    pub const fn hand(&self) -> Hand {
        self.hand
    }

    /// Return the frame slice in causal order.
    #[must_use]
    pub fn frames(&self) -> &[SensorFrame] {
        &self.frames
    }

    fn borrowed(&self) -> BorrowedTapWindow<'_> {
        BorrowedTapWindow {
            first: &self.frames,
            second: &[],
            hand: self.hand,
        }
    }
}

/// Structural failures that prevent a frame sequence from being a window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowError {
    Empty,
    MixedHands { expected: Hand, actual: Hand },
    TimeRegression,
    SampleTimeOutOfRange { sensor: usize },
    SensorTimeRegression { sensor: usize },
}

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("tap window is empty"),
            Self::MixedHands { expected, actual } => write!(
                formatter,
                "tap window mixes {expected:?} and {actual:?} frames"
            ),
            Self::TimeRegression => {
                formatter.write_str("tap window device time does not strictly advance")
            }
            Self::SampleTimeOutOfRange { sensor } => {
                write!(formatter, "tap window sensor {sensor} time is out of range")
            }
            Self::SensorTimeRegression { sensor } => {
                write!(
                    formatter,
                    "tap window sensor {sensor} sample time does not strictly advance"
                )
            }
        }
    }
}

impl Error for WindowError {}

/// One calibration window and its prompted physical truth.
///
/// `None` means an armed no-tap or ordinary-motion negative.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalibrationExample {
    pub position: Option<PhysicalPosition>,
    pub window: TapWindow,
}

/// A successful key decision or an explicit abstention.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionKind {
    Key(KeyUsage),
    Abstain(AbstainReason),
}

/// Why a causal window did not produce a key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbstainReason {
    NoIntent,
    SequenceDiscontinuity,
    MissingSensor,
    AcquisitionFault,
    TimingSkew,
    AmbiguousFinger,
    LowConfidence,
    ReservedPosition,
    WrongHand,
}

impl AbstainReason {
    /// Return whether this reason invalidates the evidence stream rather than
    /// merely withholding a low-confidence key decision.
    #[must_use]
    pub const fn invalidates_live_output(self) -> bool {
        matches!(
            self,
            Self::SequenceDiscontinuity
                | Self::MissingSensor
                | Self::AcquisitionFault
                | Self::TimingSkew
                | Self::WrongHand
        )
    }
}

/// Inspectable result from the baseline decoder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    pub kind: DecisionKind,
    pub position: Option<PhysicalPosition>,
    pub decision_time_us: u64,
    pub best_distance: Option<f64>,
    pub distance_margin: Option<f64>,
}

impl Decision {
    fn abstain(window: BorrowedTapWindow<'_>, reason: AbstainReason) -> Self {
        Self {
            kind: DecisionKind::Abstain(reason),
            position: None,
            decision_time_us: window.decision_time_us(),
            best_distance: None,
            distance_margin: None,
        }
    }
}

/// Reasons a calibration artifact cannot be constructed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CalibrationError {
    WrongHand {
        expected: Hand,
        actual: Hand,
    },
    UnusableExample {
        index: usize,
        reason: AbstainReason,
    },
    MissingPosition {
        position: PhysicalPosition,
    },
    InsufficientPositionExamples {
        position: PhysicalPosition,
        actual: usize,
    },
    InsufficientIdleExamples {
        actual: usize,
    },
    MissingThresholdFinger {
        finger: Finger,
    },
    ActingFingerMismatch {
        position: PhysicalPosition,
        observed: Finger,
    },
    IntentNotSeparable {
        maximum_idle_energy: f64,
        minimum_tap_energy: f64,
    },
}

impl fmt::Display for CalibrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongHand { expected, actual } => write!(
                formatter,
                "calibration expected {expected:?} but received {actual:?}"
            ),
            Self::UnusableExample { index, reason } => write!(
                formatter,
                "calibration example {index} has unusable sensor evidence: {reason:?}"
            ),
            Self::MissingPosition { position } => {
                write!(formatter, "calibration has no examples for {position:?}")
            }
            Self::InsufficientPositionExamples { position, actual } => write!(
                formatter,
                "calibration has {actual} examples for {position:?}; expected at least {MIN_POSITION_EXAMPLES}"
            ),
            Self::InsufficientIdleExamples { actual } => write!(
                formatter,
                "calibration has {actual} idle examples; expected at least {MIN_IDLE_EXAMPLES}"
            ),
            Self::MissingThresholdFinger { finger } => write!(
                formatter,
                "threshold selection has no tap example for {finger:?}"
            ),
            Self::ActingFingerMismatch { position, observed } => write!(
                formatter,
                "calibration for {position:?} was dominated by {observed:?}"
            ),
            Self::IntentNotSeparable {
                maximum_idle_energy,
                minimum_tap_energy,
            } => write!(
                formatter,
                "calibration intent overlaps: maximum idle energy {maximum_idle_energy:.3} >= minimum tap energy {minimum_tap_energy:.3}"
            ),
        }
    }
}

impl Error for CalibrationError {}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PositionModel {
    position: PhysicalPosition,
    centroid: [f64; FEATURE_COUNT],
}

#[derive(Clone, Copy, Debug)]
struct WindowAnalysis {
    energies: [f64; FINGER_SENSOR_COUNT],
    peaks: [usize; FINGER_SENSOR_COUNT],
}

impl WindowAnalysis {
    fn strongest_sensor(self) -> usize {
        let mut best = 0;
        for index in 1..FINGER_SENSOR_COUNT {
            if self.energies[index] > self.energies[best] {
                best = index;
            }
        }
        best
    }

    fn strongest_and_second(self) -> (usize, f64, f64) {
        let strongest = self.strongest_sensor();
        let mut second = 0.0_f64;
        for (index, energy) in self.energies.into_iter().enumerate() {
            if index != strongest {
                second = second.max(energy);
            }
        }
        (strongest, self.energies[strongest], second)
    }
}

/// Personal per-hand nearest-centroid baseline.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandDecoder {
    hand: Hand,
    intent_threshold: f64,
    variance: [f64; FEATURE_COUNT],
    models: Vec<PositionModel>,
}

impl HandDecoder {
    /// Fit the complete 17-position geometry for one hand, including reserved
    /// positions and explicit idle examples.
    ///
    /// # Errors
    ///
    /// Returns [`CalibrationError`] if examples mix hands, omit any physical
    /// position, have too few samples, identify the wrong acting sensor, or do
    /// not separate prompted taps from idle energy.
    pub fn calibrate(
        hand: Hand,
        examples: &[CalibrationExample],
    ) -> Result<Self, CalibrationError> {
        Self::calibrate_split(hand, examples, examples)
    }

    /// Fit position models from training evidence and select the intent
    /// threshold from a disjoint evidence block.
    ///
    /// Keeping these sets separate prevents threshold-selection examples from
    /// silently changing the position centroids they are meant to check.
    pub fn calibrate_split(
        hand: Hand,
        training_examples: &[CalibrationExample],
        threshold_examples: &[CalibrationExample],
    ) -> Result<Self, CalibrationError> {
        let positions = all_slots()
            .iter()
            .filter(|slot| slot.position.hand == hand)
            .map(|slot| slot.position)
            .collect::<Vec<_>>();
        let mut samples = positions
            .iter()
            .copied()
            .map(|position| (position, Vec::<[f64; FEATURE_COUNT]>::new()))
            .collect::<Vec<_>>();
        for (example_index, example) in training_examples.iter().enumerate() {
            if example.window.hand() != hand {
                return Err(CalibrationError::WrongHand {
                    expected: hand,
                    actual: example.window.hand(),
                });
            }
            let window = example.window.borrowed();
            if let Some(reason) = quality_issue(window) {
                return Err(CalibrationError::UnusableExample {
                    index: example_index,
                    reason,
                });
            }
            let analysis = analyze(window);
            if let Some(position) = example.position {
                if position.hand != hand {
                    return Err(CalibrationError::WrongHand {
                        expected: hand,
                        actual: position.hand,
                    });
                }
                let expected_sensor = position.finger.sensor_index();
                let observed_sensor = analysis.strongest_sensor();
                if observed_sensor != expected_sensor {
                    return Err(CalibrationError::ActingFingerMismatch {
                        position,
                        observed: finger_for_sensor(observed_sensor),
                    });
                }
                let feature =
                    extract_feature(window, expected_sensor, analysis.peaks[expected_sensor]);
                let target = samples
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == position)
                    .ok_or(CalibrationError::MissingPosition { position })?;
                target.1.push(feature);
            }
        }

        let mut models = Vec::with_capacity(positions.len());
        for (position, position_samples) in &samples {
            if position_samples.is_empty() {
                return Err(CalibrationError::MissingPosition {
                    position: *position,
                });
            }
            if position_samples.len() < MIN_POSITION_EXAMPLES {
                return Err(CalibrationError::InsufficientPositionExamples {
                    position: *position,
                    actual: position_samples.len(),
                });
            }
            models.push(PositionModel {
                position: *position,
                centroid: mean_feature(position_samples),
            });
        }

        let variance = pooled_variance(&samples, &models);
        let intent_threshold = select_intent_threshold(hand, threshold_examples)?;

        Ok(Self {
            hand,
            intent_threshold,
            variance,
            models,
        })
    }

    /// Return the maximum finger-to-dorsal energy for one named finger.
    ///
    /// Calibration capture uses this same signal definition to find a peak
    /// near a physical-key label. It is not a hardware-validated tap detector.
    #[must_use]
    pub fn contact_energy(window: &TapWindow, finger: Finger) -> f64 {
        analyze(window.borrowed()).energies[finger.sensor_index()]
    }

    /// Return the dynamic, position-independent finger-to-dorsal energies.
    ///
    /// Each value is the largest squared change from the oldest frame in the
    /// causal window. Removing the window's starting vector prevents static
    /// ring orientation from acting as contact evidence.
    #[must_use]
    pub fn causal_contact_energies(window: &TapWindow) -> [u64; FINGER_SENSOR_COUNT] {
        causal_contact_energies_borrowed(window.borrowed(), true)
    }

    /// Return common hand-motion energy without using a prompted finger.
    ///
    /// This signal is the largest squared change in the six-sensor mean from
    /// the oldest frame. It detects hand movement that cancels out of the
    /// finger-to-dorsal contact signal.
    #[must_use]
    pub fn common_motion_energy(window: &TapWindow) -> u64 {
        let window = window.borrowed();
        let baseline = common_sensor_vector(window.frame(0));
        window
            .iter()
            .map(|frame| squared_delta(common_sensor_vector(frame), baseline))
            .max()
            .unwrap_or(0)
    }

    /// Report a raw signed-range endpoint. Such a window cannot prove normal
    /// motion because the true amplitude may have exceeded the sensor range.
    #[must_use]
    pub fn has_saturated_sample(window: &TapWindow) -> bool {
        window.borrowed().iter().any(|frame| {
            frame.sensors.iter().any(|sensor| {
                sensor
                    .acceleration
                    .into_iter()
                    .chain(sensor.angular_velocity)
                    .any(|value| value == i16::MIN || value == i16::MAX)
            })
        })
    }

    /// Return whether this window satisfies the same stream-quality contract
    /// used by calibration and live decoding.
    #[must_use]
    pub fn usable_for_contact(window: &TapWindow) -> bool {
        quality_issue(window.borrowed()).is_none()
    }

    /// Return the fitted intent threshold for inspectable capture logic.
    #[must_use]
    pub const fn intent_threshold(&self) -> f64 {
        self.intent_threshold
    }

    /// Check acting-finger ownership without selecting a runtime threshold.
    ///
    /// This is the complete keyboard warm-start precheck. Runtime intent is
    /// selected later from a disjoint passive tap/quiet block.
    pub fn validate_acting_fingers(
        hand: Hand,
        examples: &[CalibrationExample],
    ) -> Result<(), CalibrationError> {
        validate_acting_fingers(hand, examples)
    }

    /// Decode one causal window without language or future context.
    #[must_use]
    pub fn decode(&self, window: &TapWindow) -> Decision {
        self.decode_borrowed(window.borrowed())
    }

    /// Decode an already validated borrowed window without allocating.
    pub(crate) fn decode_borrowed(&self, window: BorrowedTapWindow<'_>) -> Decision {
        if window.hand() != self.hand {
            return Decision::abstain(window, AbstainReason::WrongHand);
        }
        if let Some(reason) = quality_issue(window) {
            return Decision::abstain(window, reason);
        }

        let analysis = analyze(window);
        let (sensor, strongest, second) = analysis.strongest_and_second();
        if strongest < self.intent_threshold {
            return Decision::abstain(window, AbstainReason::NoIntent);
        }
        if second > 0.0 && strongest / second < FINGER_ENERGY_RATIO {
            return Decision::abstain(window, AbstainReason::AmbiguousFinger);
        }

        let feature = extract_feature(window, sensor, analysis.peaks[sensor]);
        let finger = finger_for_sensor(sensor);
        let mut best = None;
        let mut second_distance = f64::INFINITY;
        for model in self
            .models
            .iter()
            .filter(|model| model.position.finger == finger)
        {
            let candidate_distance = distance(feature, model.centroid, self.variance);
            match best {
                None => best = Some((model.position, candidate_distance)),
                Some((_, best_distance))
                    if candidate_distance.total_cmp(&best_distance).is_lt() =>
                {
                    second_distance = best_distance;
                    best = Some((model.position, candidate_distance));
                }
                Some(_) if candidate_distance.total_cmp(&second_distance).is_lt() => {
                    second_distance = candidate_distance;
                }
                Some(_) => {}
            }
        }

        let (position, best_distance) = best.expect("calibrated finger has position models");
        assert!(
            second_distance.is_finite(),
            "calibrated finger has at least two position models"
        );
        let margin = second_distance / best_distance.max(f64::EPSILON);
        if margin < DISTANCE_MARGIN {
            return Decision {
                kind: DecisionKind::Abstain(AbstainReason::LowConfidence),
                position: None,
                decision_time_us: window.decision_time_us(),
                best_distance: Some(best_distance),
                distance_margin: Some(margin),
            };
        }

        let kind = usage_for_position(position).map_or(
            DecisionKind::Abstain(AbstainReason::ReservedPosition),
            DecisionKind::Key,
        );
        Decision {
            kind,
            position: Some(position),
            decision_time_us: window.decision_time_us(),
            best_distance: Some(best_distance),
            distance_margin: Some(margin),
        }
    }

    /// Return the hand this calibration artifact accepts.
    #[must_use]
    pub const fn hand(&self) -> Hand {
        self.hand
    }

    /// Validate a decoder recovered from a persistent calibration artifact.
    ///
    /// # Errors
    ///
    /// Rejects a missing, duplicated, reordered, nonfinite, or impossible
    /// model before it can enter the live path.
    pub fn validate_artifact(&self) -> Result<(), DecoderArtifactError> {
        if !self.intent_threshold.is_finite() || self.intent_threshold <= 0.0 {
            return Err(DecoderArtifactError::InvalidIntentThreshold);
        }
        if self
            .variance
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(DecoderArtifactError::InvalidVariance);
        }
        let expected = all_slots()
            .iter()
            .filter(|slot| slot.position.hand == self.hand)
            .map(|slot| slot.position)
            .collect::<Vec<_>>();
        if self.models.len() != expected.len() {
            return Err(DecoderArtifactError::InvalidModelCount {
                expected: expected.len(),
                actual: self.models.len(),
            });
        }
        for (index, (model, expected_position)) in self.models.iter().zip(expected).enumerate() {
            if model.position != expected_position {
                return Err(DecoderArtifactError::PositionMismatch {
                    index,
                    expected: expected_position,
                    actual: model.position,
                });
            }
            if model.centroid.iter().any(|value| !value.is_finite()) {
                return Err(DecoderArtifactError::NonFiniteCentroid { index });
            }
        }
        Ok(())
    }
}

/// Why persisted decoder parameters cannot enter live decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecoderArtifactError {
    InvalidIntentThreshold,
    InvalidVariance,
    InvalidModelCount {
        expected: usize,
        actual: usize,
    },
    PositionMismatch {
        index: usize,
        expected: PhysicalPosition,
        actual: PhysicalPosition,
    },
    NonFiniteCentroid {
        index: usize,
    },
}

impl fmt::Display for DecoderArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIntentThreshold => {
                formatter.write_str("decoder intent threshold is invalid")
            }
            Self::InvalidVariance => formatter.write_str("decoder variance is invalid"),
            Self::InvalidModelCount { expected, actual } => write!(
                formatter,
                "decoder has {actual} position models; expected {expected}"
            ),
            Self::PositionMismatch {
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "decoder model {index} is {actual:?}; expected {expected:?}"
            ),
            Self::NonFiniteCentroid { index } => {
                write!(formatter, "decoder model {index} has a nonfinite centroid")
            }
        }
    }
}

impl Error for DecoderArtifactError {}

fn select_intent_threshold(
    hand: Hand,
    examples: &[CalibrationExample],
) -> Result<f64, CalibrationError> {
    let mut idle_count = 0;
    let mut observed_fingers = [false; FINGER_SENSOR_COUNT];
    let mut maximum_idle_energy = 0.0_f64;
    let mut minimum_tap_energy = f64::INFINITY;

    for (example_index, example) in examples.iter().enumerate() {
        if example.window.hand() != hand {
            return Err(CalibrationError::WrongHand {
                expected: hand,
                actual: example.window.hand(),
            });
        }
        let window = example.window.borrowed();
        if let Some(reason) = quality_issue(window) {
            return Err(CalibrationError::UnusableExample {
                index: example_index,
                reason,
            });
        }
        let analysis = analyze(window);
        match example.position {
            None => {
                idle_count += 1;
                maximum_idle_energy =
                    maximum_idle_energy.max(analysis.energies.into_iter().fold(0.0_f64, f64::max));
            }
            Some(position) => {
                if position.hand != hand {
                    return Err(CalibrationError::WrongHand {
                        expected: hand,
                        actual: position.hand,
                    });
                }
                let expected_sensor = position.finger.sensor_index();
                let observed_sensor = analysis.strongest_sensor();
                if observed_sensor != expected_sensor {
                    return Err(CalibrationError::ActingFingerMismatch {
                        position,
                        observed: finger_for_sensor(observed_sensor),
                    });
                }
                observed_fingers[expected_sensor] = true;
                minimum_tap_energy = minimum_tap_energy.min(analysis.energies[expected_sensor]);
            }
        }
    }

    if idle_count < MIN_IDLE_EXAMPLES {
        return Err(CalibrationError::InsufficientIdleExamples { actual: idle_count });
    }
    for (sensor, observed) in observed_fingers.into_iter().enumerate() {
        if !observed {
            return Err(CalibrationError::MissingThresholdFinger {
                finger: finger_for_sensor(sensor),
            });
        }
    }
    if minimum_tap_energy <= maximum_idle_energy {
        return Err(CalibrationError::IntentNotSeparable {
            maximum_idle_energy,
            minimum_tap_energy,
        });
    }
    Ok(f64::midpoint(minimum_tap_energy, maximum_idle_energy))
}

fn validate_acting_fingers(
    hand: Hand,
    examples: &[CalibrationExample],
) -> Result<(), CalibrationError> {
    for (example_index, example) in examples.iter().enumerate() {
        if example.window.hand() != hand {
            return Err(CalibrationError::WrongHand {
                expected: hand,
                actual: example.window.hand(),
            });
        }
        let window = example.window.borrowed();
        if let Some(reason) = quality_issue(window) {
            return Err(CalibrationError::UnusableExample {
                index: example_index,
                reason,
            });
        }
        let Some(position) = example.position else {
            continue;
        };
        if position.hand != hand {
            return Err(CalibrationError::WrongHand {
                expected: hand,
                actual: position.hand,
            });
        }
        let observed_sensor = analyze(window).strongest_sensor();
        if observed_sensor != position.finger.sensor_index() {
            return Err(CalibrationError::ActingFingerMismatch {
                position,
                observed: finger_for_sensor(observed_sensor),
            });
        }
    }
    Ok(())
}

/// Apply the window's acquisition-integrity policy to a single ingress frame.
pub(crate) fn frame_quality_issue(frame: &SensorFrame) -> Option<AbstainReason> {
    acquisition_quality_issue(std::iter::once(frame))
}

fn quality_issue(window: BorrowedTapWindow<'_>) -> Option<AbstainReason> {
    if window
        .iter()
        .zip(window.iter().skip(1))
        .any(|(prior, next)| next.sequence != prior.sequence.wrapping_add(1))
    {
        return Some(AbstainReason::SequenceDiscontinuity);
    }
    acquisition_quality_issue(window.iter())
}

fn acquisition_quality_issue<'a>(
    frames: impl Iterator<Item = &'a SensorFrame> + Clone,
) -> Option<AbstainReason> {
    if frames
        .clone()
        .any(|frame| frame.valid_sensor_mask != ALL_SENSORS_VALID)
    {
        return Some(AbstainReason::MissingSensor);
    }
    if frames
        .clone()
        .any(|frame| frame.flags & (FLAG_CLOCK_UNSYNCED | FLAG_ACQUISITION_OVERRUN) != 0)
    {
        return Some(AbstainReason::AcquisitionFault);
    }
    if maximum_timing_skew_us(frames) > MAX_INTER_SENSOR_SKEW_US {
        return Some(AbstainReason::TimingSkew);
    }
    None
}

fn maximum_timing_skew_us<'a>(frames: impl Iterator<Item = &'a SensorFrame>) -> i32 {
    frames
        .map(|frame| {
            let mut minimum = i32::MAX;
            let mut maximum = i32::MIN;
            for sensor in &frame.sensors {
                let offset = i32::from(sensor.sample_time_offset_us);
                minimum = minimum.min(offset);
                maximum = maximum.max(offset);
            }
            maximum - minimum
        })
        .max()
        .unwrap_or(0)
}

fn analyze(window: BorrowedTapWindow<'_>) -> WindowAnalysis {
    let mut energies = [0.0_f64; FINGER_SENSOR_COUNT];
    let mut peaks = [0_usize; FINGER_SENSOR_COUNT];
    for sensor in 0..FINGER_SENSOR_COUNT {
        for (frame_index, frame) in window.iter().enumerate() {
            let vector = relative_vector(frame, sensor);
            let energy = vector.into_iter().map(|value| value * value).sum();
            if energy > energies[sensor] {
                energies[sensor] = energy;
                peaks[sensor] = frame_index;
            }
        }
    }
    WindowAnalysis { energies, peaks }
}

fn extract_feature(
    window: BorrowedTapWindow<'_>,
    sensor: usize,
    peak_index: usize,
) -> [f64; FEATURE_COUNT] {
    let mut vector = relative_vector(window.frame(peak_index), sensor);
    let norm = vector
        .into_iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt()
        .max(f64::EPSILON);
    for value in &mut vector {
        *value /= norm;
    }

    let mut feature = [0.0_f64; FEATURE_COUNT];
    feature[..6].copy_from_slice(&vector);
    feature[6] = count_as_f64(peak_index) / count_as_f64((window.len() - 1).max(1));
    feature
}

fn relative_vector(frame: &SensorFrame, sensor: usize) -> [f64; 6] {
    let finger = frame.sensors[sensor];
    let dorsal = frame.sensors[DORSAL_SENSOR_INDEX];
    [
        f64::from(finger.acceleration[0]) - f64::from(dorsal.acceleration[0]),
        f64::from(finger.acceleration[1]) - f64::from(dorsal.acceleration[1]),
        f64::from(finger.acceleration[2]) - f64::from(dorsal.acceleration[2]),
        f64::from(finger.angular_velocity[0]) - f64::from(dorsal.angular_velocity[0]),
        f64::from(finger.angular_velocity[1]) - f64::from(dorsal.angular_velocity[1]),
        f64::from(finger.angular_velocity[2]) - f64::from(dorsal.angular_velocity[2]),
    ]
}

/// The same causal energy used by calibration, with dorsal subtraction
/// disabled only for the diagnostic pilot's raw-signal ablation.
pub(crate) fn causal_contact_energies_borrowed(
    window: BorrowedTapWindow<'_>,
    subtract_dorsal: bool,
) -> [u64; FINGER_SENSOR_COUNT] {
    let vector = |frame: &SensorFrame, sensor: usize| {
        if subtract_dorsal {
            relative_vector_i64(frame, sensor)
        } else {
            let reading = frame.sensors[sensor];
            [
                i64::from(reading.acceleration[0]),
                i64::from(reading.acceleration[1]),
                i64::from(reading.acceleration[2]),
                i64::from(reading.angular_velocity[0]),
                i64::from(reading.angular_velocity[1]),
                i64::from(reading.angular_velocity[2]),
            ]
        }
    };
    std::array::from_fn(|sensor| {
        let baseline = vector(window.frame(0), sensor);
        window
            .iter()
            .map(|frame| squared_delta(vector(frame, sensor), baseline))
            .max()
            .unwrap_or(0)
    })
}

fn relative_vector_i64(frame: &SensorFrame, sensor: usize) -> [i64; 6] {
    let finger = frame.sensors[sensor];
    let dorsal = frame.sensors[DORSAL_SENSOR_INDEX];
    [
        i64::from(finger.acceleration[0]) - i64::from(dorsal.acceleration[0]),
        i64::from(finger.acceleration[1]) - i64::from(dorsal.acceleration[1]),
        i64::from(finger.acceleration[2]) - i64::from(dorsal.acceleration[2]),
        i64::from(finger.angular_velocity[0]) - i64::from(dorsal.angular_velocity[0]),
        i64::from(finger.angular_velocity[1]) - i64::from(dorsal.angular_velocity[1]),
        i64::from(finger.angular_velocity[2]) - i64::from(dorsal.angular_velocity[2]),
    ]
}

fn common_sensor_vector(frame: &SensorFrame) -> [i64; 6] {
    let mut total = [0_i64; 6];
    for sensor in frame.sensors {
        for (axis, value) in sensor
            .acceleration
            .into_iter()
            .chain(sensor.angular_velocity)
            .enumerate()
        {
            total[axis] += i64::from(value);
        }
    }
    total.map(|value| value / SENSOR_COUNT as i64)
}

fn squared_delta(value: [i64; 6], baseline: [i64; 6]) -> u64 {
    value
        .into_iter()
        .zip(baseline)
        .map(|(value, baseline)| {
            let delta = i128::from(value - baseline);
            u64::try_from(delta * delta).unwrap_or(u64::MAX)
        })
        .fold(0_u64, u64::saturating_add)
}

fn mean_feature(features: &[[f64; FEATURE_COUNT]]) -> [f64; FEATURE_COUNT] {
    let mut mean = [0.0_f64; FEATURE_COUNT];
    for feature in features {
        for axis in 0..FEATURE_COUNT {
            mean[axis] += feature[axis];
        }
    }
    for value in &mut mean {
        *value /= count_as_f64(features.len());
    }
    mean
}

fn distance(
    feature: [f64; FEATURE_COUNT],
    centroid: [f64; FEATURE_COUNT],
    variance: [f64; FEATURE_COUNT],
) -> f64 {
    let mut total = 0.0;
    for axis in 0..FEATURE_COUNT {
        let residual = feature[axis] - centroid[axis];
        total += residual * residual / variance[axis];
    }
    total
}

fn pooled_variance(
    samples: &[(PhysicalPosition, Vec<[f64; FEATURE_COUNT]>)],
    models: &[PositionModel],
) -> [f64; FEATURE_COUNT] {
    let mut variance = [0.0_f64; FEATURE_COUNT];
    let mut residual_count = 0_usize;
    for ((_, position_samples), model) in samples.iter().zip(models) {
        for feature in position_samples {
            for axis in 0..FEATURE_COUNT {
                let residual = feature[axis] - model.centroid[axis];
                variance[axis] += residual * residual;
            }
            residual_count += 1;
        }
    }
    for value in &mut variance {
        *value = (*value / count_as_f64(residual_count)).max(VARIANCE_FLOOR);
    }
    variance
}

fn count_as_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).expect("decoder collection length fits u32"))
}

fn finger_for_sensor(sensor: usize) -> Finger {
    Finger::from_sensor_index(sensor).expect("only finger sensor indexes enter classification")
}

#[cfg(test)]
mod tests {
    use super::{BorrowedTapWindow, FrameIntegrity, FrameIntegrityError, HandDecoder, WindowError};
    use crate::{
        layout::{Hand, all_slots},
        protocol::{ALL_SENSORS_VALID, SENSOR_COUNT, SensorFrame, SensorReading},
        simulation::SyntheticConfig,
    };

    #[test]
    fn borrowed_split_window_reuses_storage_and_preserves_decode_order() {
        let config = SyntheticConfig::new(Hand::Left, 0xb0_2200);
        let decoder = HandDecoder::calibrate(Hand::Left, &config.calibration_examples(5, 20))
            .expect("complete synthetic calibration fits");
        let position = all_slots()
            .iter()
            .find(|slot| slot.label == "A")
            .expect("A exists")
            .position;
        let owned = config.tap_window(position, 42);
        let frames = owned.frames();
        let split = 7;
        let borrowed = BorrowedTapWindow::new(&frames[..split], &frames[split..])
            .expect("split slices preserve causal order");

        assert!(std::ptr::eq(borrowed.frame(0), &frames[0]));
        assert!(std::ptr::eq(borrowed.frame(split), &frames[split]));
        assert_eq!(decoder.decode_borrowed(borrowed), decoder.decode(&owned));
    }

    #[test]
    fn repeated_raw_aggregate_time_rejects_even_when_every_sensor_advances() {
        let mut frame = SensorFrame {
            hand: Hand::Left,
            controller_id_sha256: [0x11; 32],
            boot_id: [0x31; 16],
            flags: 0,
            valid_sensor_mask: ALL_SENSORS_VALID,
            sequence: 20,
            device_time_us: 100_000,
            sensors: [SensorReading {
                sample_time_offset_us: -1_000,
                ..SensorReading::default()
            }; SENSOR_COUNT],
        };
        let mut integrity = FrameIntegrity::default();
        integrity
            .observe(&frame)
            .expect("first raw frame is healthy");
        frame.sequence += 1;
        for sensor in &mut frame.sensors {
            sensor.sample_time_offset_us += 1;
        }
        assert_eq!(
            integrity.observe(&frame),
            Err(FrameIntegrityError::Structure(WindowError::TimeRegression))
        );
    }
}
