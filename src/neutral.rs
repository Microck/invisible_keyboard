//! Hardware-independent neutral-window statistics bound to immutable capture evidence.

use std::{collections::BTreeSet, error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    clock::ClockError,
    clock_epoch::{ClockEpochError, ClockEpochFitError, ClockEpochSet, QualifiedOfflineClocks},
    decoder::{AbstainReason, FrameIntegrity, FrameIntegrityError, WindowError},
    layout::Hand,
    manifest::{
        AcquisitionManifest, FrameCanonicalizationError, FrameCanonicalizer, ManifestError,
        ManifestHand, format_sha256, parse_sha256, sha256,
    },
    protocol::{ALL_SENSORS_VALID, SENSOR_COUNT, SensorFrame},
    replay::MAX_MAPPED_RECEIPT_DELTA_NS,
    session::{FinalizedSession, RecordDecodeError, SessionError, SessionRecordKind},
};

/// Version 1 report discriminator.
pub const NEUTRAL_EVIDENCE_SCHEMA: &str = "invisible-keyboard-neutral-evidence-v1";

/// Maximum report size accepted before JSON parsing.
pub const MAX_NEUTRAL_REPORT_BYTES: usize = 1_048_576;

/// Enough samples for stable population variance without assuming one sensor ODR.
pub const MIN_NEUTRAL_SAMPLES_PER_HAND: u64 = 32;

/// Minimum fitted-clock span represented by each hand's selected samples.
pub const MIN_NEUTRAL_SPAN_NS: u64 = 100_000_000;

/// Canonical protocol index of the dorsal reference sensor.
pub const DORSAL_SENSOR_INDEX: usize = SENSOR_COUNT - 1;

// These gates are dimensionless fractions of evidence-derived gravity, signed
// raw range, or the manifest period. They intentionally do not identify a
// sensor model or assume a vendor sensitivity table.
const MAX_ACCELERATION_NOISE_RATIO: f64 = 0.05;
const MAX_GYROSCOPE_BIAS_FRACTION: f64 = 0.01;
const MAX_GYROSCOPE_NOISE_FRACTION: f64 = 0.005;
const MAX_SAMPLE_TIME_JITTER_FRACTION: f64 = 0.10;
const MAX_SAMPLE_TIME_SKEW_FRACTION: f64 = 1.0;
const MAX_RELATIVE_TILT_JITTER_DEGREES: f64 = 5.0;
const MAX_SHARED_ACCELERATION_MOTION_RATIO: f64 = 0.05;
const MAX_SHARED_GYROSCOPE_MOTION_FRACTION: f64 = 0.005;
const SIGNED_RAW_SCALE: f64 = 32_768.0;
const MIN_GRAVITY_NORM_COUNTS: f64 = 1.0;

/// Why the caller selected a record range for neutral evidence.
///
/// This annotation identifies the capture block. It is not a claimed key
/// state and never overrides a stability fault derived from the samples.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NeutralWindowRole {
    Idle,
    Neutral,
}

/// One caller-identified, half-open record interval in the source session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NeutralRecordWindow {
    pub block_id: String,
    pub window_id: String,
    pub role: NeutralWindowRole,
    pub start_record: u64,
    pub end_record_exclusive: u64,
}

/// A binary verdict backed by the adjacent explicit fault list.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StabilityVerdict {
    Stable,
    Fault,
}

/// Per-sensor conditions that prevent a stable neutral verdict.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorFault {
    DegenerateGravityVector,
    ExcessiveAccelerationNoise,
    ExcessiveGyroscopeBias,
    ExcessiveGyroscopeNoise,
    ExcessiveSampleTimeJitter,
    ExcessiveSampleTimeSkew,
}

/// Conditions that prevent a finger-to-dorsal tilt estimate from being stable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrientationFault {
    UnobservableGravity,
    ExcessiveTiltJitter,
}

/// Hand-level faults name the exact child or common-mode statistic involved.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "sensor_index")]
pub enum HandFault {
    Sensor(u8),
    FingerToDorsal(u8),
    SharedAccelerationMotion,
    SharedGyroscopeMotion,
}

/// Top-level faults preserve which hand prevented acceptance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "hand")]
pub enum EvidenceFault {
    HandNotStable(Hand),
}

/// Coverage derived for one hand within one explicit record window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowHandCoverage {
    pub hand: Hand,
    pub sample_count: u64,
    pub first_mapped_time_ns: u64,
    pub last_mapped_time_ns: u64,
}

impl WindowHandCoverage {
    /// Duration is computed from accepted mapped timestamps, never supplied by
    /// a caller or inferred from a record count.
    #[must_use]
    pub const fn derived_duration_ns(&self) -> u64 {
        self.last_mapped_time_ns
            .saturating_sub(self.first_mapped_time_ns)
    }
}

/// Inspectable identity and coverage for one selected window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NeutralWindowReport {
    pub block_id: String,
    pub window_id: String,
    pub role: NeutralWindowRole,
    pub start_record: u64,
    pub end_record_exclusive: u64,
    /// Canonical order: left, right.
    pub hands: [WindowHandCoverage; 2],
}

impl NeutralWindowReport {
    /// Conservative simultaneous coverage uses the shorter derived hand span.
    #[must_use]
    pub fn derived_duration_ns(&self) -> u64 {
        self.hands
            .iter()
            .map(WindowHandCoverage::derived_duration_ns)
            .min()
            .unwrap_or(0)
    }
}

/// Population statistics for one canonical sensor channel.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SensorNeutralStatistics {
    pub sensor_index: u8,
    pub sample_count: u64,
    /// Mean canonical acceleration. Under stable neutral evidence this is the
    /// observable gravity vector in raw counts.
    pub acceleration_mean_gravity: [f64; 3],
    pub acceleration_variance: [f64; 3],
    pub acceleration_noise_energy: f64,
    /// Mean canonical angular velocity, which is the neutral gyro bias.
    pub gyroscope_mean_bias: [f64; 3],
    pub gyroscope_variance: [f64; 3],
    pub gyroscope_noise_energy: f64,
    pub sample_time_offset_mean_us: f64,
    pub sample_time_offset_variance_us2: f64,
    pub sample_time_jitter_rms_us: f64,
    pub sample_time_skew_to_dorsal_mean_us: f64,
    pub sample_time_skew_to_dorsal_variance_us2: f64,
    pub verdict: StabilityVerdict,
    pub faults: Vec<SensorFault>,
}

/// Tilt-only relative orientation derived from paired gravity vectors.
///
/// Static accelerometers cannot observe rotation around gravity, so this type
/// deliberately reports an angle between vectors rather than a quaternion or
/// a full 3D orientation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FingerDorsalOrientationStatistics {
    pub finger_sensor_index: u8,
    pub total_sample_count: u64,
    pub observable_sample_count: u64,
    pub mean_tilt_degrees: Option<f64>,
    pub tilt_variance_degrees2: Option<f64>,
    pub tilt_jitter_rms_degrees: Option<f64>,
    pub verdict: StabilityVerdict,
    pub faults: Vec<OrientationFault>,
}

/// Common-mode motion and residual noise after removing each sensor's mean.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SharedMotionStatistics {
    pub sample_count: u64,
    pub acceleration_shared_motion_energy: f64,
    pub acceleration_independent_noise_energy: f64,
    pub gyroscope_shared_motion_energy: f64,
    pub gyroscope_independent_noise_energy: f64,
}

/// Complete derived neutral evidence for one hand.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandNeutralReport {
    pub hand: Hand,
    pub sample_count: u64,
    pub first_mapped_time_ns: u64,
    pub last_mapped_time_ns: u64,
    pub sensors: [SensorNeutralStatistics; SENSOR_COUNT],
    pub finger_to_dorsal: [FingerDorsalOrientationStatistics; DORSAL_SENSOR_INDEX],
    pub shared_motion: SharedMotionStatistics,
    pub verdict: StabilityVerdict,
    pub faults: Vec<HandFault>,
}

impl HandNeutralReport {
    #[must_use]
    pub const fn derived_duration_ns(&self) -> u64 {
        self.last_mapped_time_ns
            .saturating_sub(self.first_mapped_time_ns)
    }
}

/// Versioned, source-bound neutral evidence report.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NeutralEvidenceReport {
    pub schema: String,
    pub source_session_sha256: String,
    pub source_manifest_sha256: String,
    pub source_clock_sha256: String,
    pub session_id: [u8; 16],
    pub nominal_period_us: u32,
    pub windows: Vec<NeutralWindowReport>,
    /// Canonical order: left, right.
    pub hands: [HandNeutralReport; 2],
    pub verdict: StabilityVerdict,
    pub faults: Vec<EvidenceFault>,
}

impl NeutralEvidenceReport {
    /// Derive a report from immutable source objects and explicit record ranges.
    ///
    /// The API has no key-state field and no caller-supplied duration. Every
    /// duration exposed by the result comes from accepted clock-mapped samples.
    ///
    /// # Errors
    ///
    /// Returns the first provenance, integrity, timing, coverage, or report
    /// consistency failure. No frame or sensor reading is silently discarded.
    pub fn derive(
        manifest: &AcquisitionManifest,
        clock_epochs: &ClockEpochSet,
        session: &FinalizedSession,
        windows: &[NeutralRecordWindow],
    ) -> Result<Self, NeutralEvidenceError> {
        require_two_hands(manifest)?;
        validate_input_windows(windows)?;
        if session.summary().quiesce_requests != 0 || session.summary().quiesce_receipts != 0 {
            let mut reader = session.reader();
            let mut record_index = 0;
            while let Some(record) = reader
                .next_record()
                .map_err(NeutralEvidenceError::Session)?
            {
                if matches!(
                    record.kind,
                    SessionRecordKind::QuiesceRequest | SessionRecordKind::QuiesceReceipt
                ) {
                    return Err(NeutralEvidenceError::UnexpectedControlRecord {
                        record_index,
                        kind: record.kind,
                    });
                }
                record_index += 1;
            }
        }
        let qualified =
            clock_epochs
                .qualify_session(manifest, session)
                .map_err(|error| match error {
                    ClockEpochFitError::Epoch(error) => NeutralEvidenceError::ClockEpoch(error),
                    error => NeutralEvidenceError::ClockQualification(error),
                })?;
        Self::derive_qualified(&qualified, windows)
    }

    pub(crate) fn derive_qualified(
        qualified: &QualifiedOfflineClocks<'_>,
        windows: &[NeutralRecordWindow],
    ) -> Result<Self, NeutralEvidenceError> {
        let manifest = qualified.manifest();
        let clock_epochs = qualified.epochs();
        let session = qualified.session();
        manifest
            .validate()
            .map_err(NeutralEvidenceError::Manifest)?;
        require_two_hands(manifest)?;
        let canonicalizer =
            FrameCanonicalizer::new(manifest).map_err(NeutralEvidenceError::Manifest)?;
        validate_input_windows(windows)?;

        let mut reader = session.reader();
        let header = *reader.header();
        if header.nominal_period_us != manifest.nominal_period_us {
            return Err(NeutralEvidenceError::NominalPeriodMismatch {
                manifest_us: manifest.nominal_period_us,
                session_us: header.nominal_period_us,
            });
        }
        let clock_fits = [
            clock_epochs
                .fit_for(Hand::Left)
                .expect("bound clock set contains left"),
            clock_epochs
                .fit_for(Hand::Right)
                .expect("bound clock set contains right"),
        ];
        let mut hand_accumulators = [HandAccumulator::default(), HandAccumulator::default()];
        let mut stream_trackers = [StreamTracker::default(), StreamTracker::default()];
        let mut window_states = vec![WindowCoverageState::default(); windows.len()];
        let mut active_window = 0_usize;
        let mut record_index = 0_u64;

        while let Some(record) = reader
            .next_record()
            .map_err(NeutralEvidenceError::Session)?
        {
            match record.kind {
                SessionRecordKind::Decoded => {}
                SessionRecordKind::Malformed
                | SessionRecordKind::Oversized
                | SessionRecordKind::Unterminated => {
                    return Err(NeutralEvidenceError::InvalidCandidate {
                        record_index,
                        kind: record.kind,
                    });
                }
                SessionRecordKind::QuiesceRequest | SessionRecordKind::QuiesceReceipt => {
                    return Err(NeutralEvidenceError::UnexpectedControlRecord {
                        record_index,
                        kind: record.kind,
                    });
                }
            }
            let frame =
                record
                    .decode()
                    .map_err(|error| NeutralEvidenceError::InvalidDecodedRecord {
                        record_index,
                        error,
                    })?;
            let frame = canonicalizer.canonicalize(&frame).map_err(|error| {
                NeutralEvidenceError::FrameCanonicalization {
                    record_index,
                    error,
                }
            })?;
            let hand_index = frame.hand.index();
            stream_trackers[hand_index].observe_raw(record_index, &frame)?;
            let mapped_time_ns = clock_fits[hand_index]
                .map_device_time_ns(frame.boot_id, frame.device_time_us)
                .map_err(|error| NeutralEvidenceError::ClockMapping {
                    record_index,
                    hand: frame.hand,
                    error,
                })?;
            let receipt_delta_ns = mapped_time_ns.abs_diff(record.host_time_ns);
            if receipt_delta_ns > MAX_MAPPED_RECEIPT_DELTA_NS {
                return Err(NeutralEvidenceError::MappedReceiptDeltaTooLarge {
                    record_index,
                    hand: frame.hand,
                    actual_ns: receipt_delta_ns,
                });
            }
            stream_trackers[hand_index].observe_mapped(record_index, frame.hand, mapped_time_ns)?;

            while active_window < windows.len()
                && record_index >= windows[active_window].end_record_exclusive
            {
                active_window += 1;
            }
            if active_window < windows.len() && record_index >= windows[active_window].start_record
            {
                hand_accumulators[hand_index].observe(&frame, mapped_time_ns);
                window_states[active_window].hands[hand_index].observe(mapped_time_ns);
            }
            record_index += 1;
        }

        let last_window = windows.last().expect("input windows are nonempty");
        if last_window.end_record_exclusive > record_index {
            return Err(NeutralEvidenceError::WindowOutsideSession {
                window_index: windows.len() - 1,
                end_record_exclusive: last_window.end_record_exclusive,
                session_record_count: record_index,
            });
        }

        let window_reports = build_window_reports(windows, &window_states)?;
        let left = derive_hand_report(
            Hand::Left,
            &hand_accumulators[0],
            manifest.nominal_period_us,
        )?;
        let right = derive_hand_report(
            Hand::Right,
            &hand_accumulators[1],
            manifest.nominal_period_us,
        )?;
        let hands = [left, right];
        let faults = evidence_faults(&hands);
        let verdict = verdict_for(&faults);
        let value = Self {
            schema: NEUTRAL_EVIDENCE_SCHEMA.to_owned(),
            source_session_sha256: format_sha256(&session.sha256()),
            source_manifest_sha256: format_sha256(&header.manifest_sha256),
            source_clock_sha256: format_sha256(
                &clock_epochs
                    .fingerprint()
                    .map_err(NeutralEvidenceError::ClockEpoch)?,
            ),
            session_id: header.session_id,
            nominal_period_us: header.nominal_period_us,
            windows: window_reports,
            hands,
            verdict,
            faults,
        };
        value.validate()?;
        Ok(value)
    }

    /// Parse and structurally validate one bounded report.
    pub fn from_json(source: &[u8]) -> Result<Self, NeutralEvidenceError> {
        if source.len() > MAX_NEUTRAL_REPORT_BYTES {
            return Err(NeutralEvidenceError::ReportTooLarge {
                actual: source.len(),
            });
        }
        let value = serde_json::from_slice::<Self>(source)
            .map_err(|error| NeutralEvidenceError::Json(error.to_string()))?;
        value.validate()?;
        Ok(value)
    }

    /// Serialize the validated typed report to one compact JSON identity.
    pub fn canonical_json(&self) -> Result<Vec<u8>, NeutralEvidenceError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|error| NeutralEvidenceError::Serialization(error.to_string()))
    }

    /// Hash the complete canonical report.
    pub fn fingerprint(&self) -> Result<[u8; 32], NeutralEvidenceError> {
        Ok(sha256(&self.canonical_json()?))
    }

    /// Re-derive the report and compare every field to its immutable sources.
    ///
    /// Structural validation alone cannot prove a finite statistic was not
    /// edited. This method is the source-bound mutation check.
    pub fn verify_sources(
        &self,
        manifest: &AcquisitionManifest,
        clock_epochs: &ClockEpochSet,
        session: &FinalizedSession,
        windows: &[NeutralRecordWindow],
    ) -> Result<(), NeutralEvidenceError> {
        self.validate()?;
        let expected = Self::derive(manifest, clock_epochs, session, windows)?;
        if self != &expected {
            return Err(NeutralEvidenceError::ReportMismatch);
        }
        Ok(())
    }

    pub(crate) fn verify_qualified_sources(
        &self,
        qualified: &QualifiedOfflineClocks<'_>,
        windows: &[NeutralRecordWindow],
    ) -> Result<(), NeutralEvidenceError> {
        self.validate()?;
        if self != &Self::derive_qualified(qualified, windows)? {
            return Err(NeutralEvidenceError::ReportMismatch);
        }
        Ok(())
    }

    /// Validate all version, shape, finite-number, count, and verdict invariants.
    pub fn validate(&self) -> Result<(), NeutralEvidenceError> {
        if self.schema != NEUTRAL_EVIDENCE_SCHEMA {
            return Err(NeutralEvidenceError::InvalidReportSchema);
        }
        for (value, field) in [
            (&self.source_session_sha256, "source_session_sha256"),
            (&self.source_manifest_sha256, "source_manifest_sha256"),
            (&self.source_clock_sha256, "source_clock_sha256"),
        ] {
            if parse_sha256(value).is_none() {
                return Err(NeutralEvidenceError::InvalidReportHash(field));
            }
        }
        if self.session_id == [0; 16] {
            return Err(NeutralEvidenceError::InvalidReport(
                "session_id must be nonzero",
            ));
        }
        if self.nominal_period_us == 0 {
            return Err(NeutralEvidenceError::InvalidReport(
                "nominal_period_us must be nonzero",
            ));
        }
        validate_report_windows(&self.windows)?;
        if self.hands[0].hand != Hand::Left || self.hands[1].hand != Hand::Right {
            return Err(NeutralEvidenceError::InvalidReport(
                "hands must be ordered left then right",
            ));
        }

        for (hand_index, hand) in self.hands.iter().enumerate() {
            validate_hand_report(hand, self.nominal_period_us)?;
            let window_sample_count = self
                .windows
                .iter()
                .map(|window| window.hands[hand_index].sample_count)
                .sum::<u64>();
            if window_sample_count != hand.sample_count {
                return Err(NeutralEvidenceError::InvalidReport(
                    "window and hand sample counts disagree",
                ));
            }
            let first = self
                .windows
                .first()
                .expect("validated report has windows")
                .hands[hand_index]
                .first_mapped_time_ns;
            let last = self
                .windows
                .last()
                .expect("validated report has windows")
                .hands[hand_index]
                .last_mapped_time_ns;
            if hand.first_mapped_time_ns != first || hand.last_mapped_time_ns != last {
                return Err(NeutralEvidenceError::InvalidReport(
                    "window and hand mapped bounds disagree",
                ));
            }
        }

        let expected_faults = evidence_faults(&self.hands);
        if self.faults != expected_faults || self.verdict != verdict_for(&expected_faults) {
            return Err(NeutralEvidenceError::InvalidReport(
                "top-level verdict and faults disagree",
            ));
        }
        Ok(())
    }

    /// Sum conservative simultaneous duration across disjoint source windows.
    #[must_use]
    pub fn derived_duration_ns(&self) -> u64 {
        self.windows.iter().fold(0_u64, |duration, window| {
            duration.saturating_add(window.derived_duration_ns())
        })
    }

    /// Return derived duration only when every stability gate passed.
    #[must_use]
    pub fn derived_stable_duration_ns(&self) -> u64 {
        if self.verdict == StabilityVerdict::Stable {
            self.derived_duration_ns()
        } else {
            0
        }
    }

    /// Compare a later policy threshold with derived evidence duration.
    ///
    /// The argument is a requirement, not a claimed observed duration.
    #[must_use]
    pub fn meets_stable_duration_ns(&self, required_duration_ns: u64) -> bool {
        self.verdict == StabilityVerdict::Stable
            && self.derived_duration_ns() >= required_duration_ns
    }
}

fn require_two_hands(manifest: &AcquisitionManifest) -> Result<(), NeutralEvidenceError> {
    for manifest_hand in [ManifestHand::Left, ManifestHand::Right] {
        let hand = Hand::from(manifest_hand);
        if !manifest
            .hands
            .iter()
            .any(|candidate| candidate.hand == manifest_hand)
        {
            return Err(NeutralEvidenceError::MissingHand(hand));
        }
    }
    Ok(())
}

fn validate_input_windows(windows: &[NeutralRecordWindow]) -> Result<(), NeutralEvidenceError> {
    validate_window_sequence(windows, |window| {
        (
            window.block_id.as_str(),
            window.window_id.as_str(),
            window.start_record,
            window.end_record_exclusive,
        )
    })
}

fn validate_window_sequence<T>(
    windows: &[T],
    fields: impl Fn(&T) -> (&str, &str, u64, u64),
) -> Result<(), NeutralEvidenceError> {
    if windows.is_empty() {
        return Err(NeutralEvidenceError::InvalidWindow {
            window_index: 0,
            reason: "at least one window is required",
        });
    }
    let mut identities = BTreeSet::new();
    let mut previous_end = 0_u64;
    for (window_index, window) in windows.iter().enumerate() {
        let (block_id, window_id, start_record, end_record_exclusive) = fields(window);
        if block_id.trim().is_empty() || window_id.trim().is_empty() {
            return Err(NeutralEvidenceError::InvalidWindow {
                window_index,
                reason: "block and window identities must be nonempty",
            });
        }
        if start_record >= end_record_exclusive {
            return Err(NeutralEvidenceError::InvalidWindow {
                window_index,
                reason: "record interval must be nonempty",
            });
        }
        if window_index > 0 && start_record < previous_end {
            return Err(NeutralEvidenceError::InvalidWindow {
                window_index,
                reason: "windows must be sorted and nonoverlapping",
            });
        }
        if !identities.insert((block_id, window_id)) {
            return Err(NeutralEvidenceError::DuplicateWindowIdentity { window_index });
        }
        previous_end = end_record_exclusive;
    }
    Ok(())
}

fn build_window_reports(
    windows: &[NeutralRecordWindow],
    states: &[WindowCoverageState],
) -> Result<Vec<NeutralWindowReport>, NeutralEvidenceError> {
    windows
        .iter()
        .zip(states)
        .enumerate()
        .map(|(window_index, (window, state))| {
            let left = state.hands[0].report(Hand::Left).ok_or(
                NeutralEvidenceError::MissingWindowHand {
                    window_index,
                    hand: Hand::Left,
                },
            )?;
            let right = state.hands[1].report(Hand::Right).ok_or(
                NeutralEvidenceError::MissingWindowHand {
                    window_index,
                    hand: Hand::Right,
                },
            )?;
            Ok(NeutralWindowReport {
                block_id: window.block_id.clone(),
                window_id: window.window_id.clone(),
                role: window.role,
                start_record: window.start_record,
                end_record_exclusive: window.end_record_exclusive,
                hands: [left, right],
            })
        })
        .collect()
}

fn validate_report_windows(windows: &[NeutralWindowReport]) -> Result<(), NeutralEvidenceError> {
    validate_window_sequence(windows, |window| {
        (
            window.block_id.as_str(),
            window.window_id.as_str(),
            window.start_record,
            window.end_record_exclusive,
        )
    })?;
    for window in windows {
        if window.hands[0].hand != Hand::Left || window.hands[1].hand != Hand::Right {
            return Err(NeutralEvidenceError::InvalidReport(
                "window hands must be ordered left then right",
            ));
        }
        for coverage in &window.hands {
            if coverage.sample_count == 0
                || coverage.last_mapped_time_ns < coverage.first_mapped_time_ns
            {
                return Err(NeutralEvidenceError::InvalidReport(
                    "window hand coverage is empty or regresses",
                ));
            }
        }
    }
    Ok(())
}

fn derive_hand_report(
    hand: Hand,
    accumulator: &HandAccumulator,
    nominal_period_us: u32,
) -> Result<HandNeutralReport, NeutralEvidenceError> {
    let sample_count = u64::try_from(accumulator.frames.len()).unwrap_or(u64::MAX);
    if sample_count < MIN_NEUTRAL_SAMPLES_PER_HAND {
        return Err(NeutralEvidenceError::TooFewSamples {
            hand,
            actual: sample_count,
            minimum: MIN_NEUTRAL_SAMPLES_PER_HAND,
        });
    }
    let first_mapped_time_ns = accumulator
        .frames
        .first()
        .expect("minimum samples are nonzero")
        .mapped_time_ns;
    let last_mapped_time_ns = accumulator
        .frames
        .last()
        .expect("minimum samples are nonzero")
        .mapped_time_ns;
    let span_ns = last_mapped_time_ns.saturating_sub(first_mapped_time_ns);
    if span_ns < MIN_NEUTRAL_SPAN_NS {
        return Err(NeutralEvidenceError::EvidenceSpanTooShort {
            hand,
            actual_ns: span_ns,
            minimum_ns: MIN_NEUTRAL_SPAN_NS,
        });
    }

    let sensors = std::array::from_fn(|sensor_index| {
        derive_sensor_statistics(sensor_index, &accumulator.frames, nominal_period_us)
    });
    let finger_to_dorsal = std::array::from_fn(|sensor_index| {
        derive_orientation_statistics(sensor_index, &accumulator.frames)
    });
    let shared_motion = derive_shared_motion(&accumulator.frames, &sensors);
    let faults = hand_faults(&sensors, &finger_to_dorsal, &shared_motion);
    let verdict = verdict_for(&faults);
    Ok(HandNeutralReport {
        hand,
        sample_count,
        first_mapped_time_ns,
        last_mapped_time_ns,
        sensors,
        finger_to_dorsal,
        shared_motion,
        verdict,
        faults,
    })
}

fn derive_sensor_statistics(
    sensor_index: usize,
    frames: &[SelectedFrame],
    nominal_period_us: u32,
) -> SensorNeutralStatistics {
    let mut accumulator = SensorAccumulator::default();
    for frame in frames {
        accumulator
            .acceleration
            .observe(frame.acceleration[sensor_index]);
        accumulator.gyroscope.observe(frame.gyroscope[sensor_index]);
        accumulator
            .sample_time_offset
            .observe(frame.sample_time_offset_us[sensor_index]);
        accumulator.sample_time_skew_to_dorsal.observe(
            frame.sample_time_offset_us[sensor_index]
                - frame.sample_time_offset_us[DORSAL_SENSOR_INDEX],
        );
    }
    let acceleration_mean_gravity = accumulator.acceleration.mean();
    let acceleration_variance = accumulator.acceleration.variance();
    let acceleration_noise_energy = vector_sum(acceleration_variance);
    let gyroscope_mean_bias = accumulator.gyroscope.mean();
    let gyroscope_variance = accumulator.gyroscope.variance();
    let gyroscope_noise_energy = vector_sum(gyroscope_variance);
    let sample_time_offset_variance_us2 = accumulator.sample_time_offset.variance();
    let sample_time_jitter_rms_us = sample_time_offset_variance_us2.sqrt();
    let sample_time_skew_to_dorsal_variance_us2 = accumulator.sample_time_skew_to_dorsal.variance();
    let sample_time_skew_to_dorsal_mean_us = accumulator.sample_time_skew_to_dorsal.mean;

    let mut statistics = SensorNeutralStatistics {
        sensor_index: u8::try_from(sensor_index).expect("six-sensor index fits u8"),
        sample_count: accumulator.acceleration.axes[0].count,
        acceleration_mean_gravity,
        acceleration_variance,
        acceleration_noise_energy,
        gyroscope_mean_bias,
        gyroscope_variance,
        gyroscope_noise_energy,
        sample_time_offset_mean_us: accumulator.sample_time_offset.mean,
        sample_time_offset_variance_us2,
        sample_time_jitter_rms_us,
        sample_time_skew_to_dorsal_mean_us,
        sample_time_skew_to_dorsal_variance_us2,
        verdict: StabilityVerdict::Stable,
        faults: Vec::new(),
    };
    statistics.faults = sensor_faults(&statistics, nominal_period_us);
    statistics.verdict = verdict_for(&statistics.faults);
    statistics
}

fn derive_orientation_statistics(
    sensor_index: usize,
    frames: &[SelectedFrame],
) -> FingerDorsalOrientationStatistics {
    let mut tilt = ScalarMoments::default();
    for frame in frames {
        let finger = frame.acceleration[sensor_index];
        let dorsal = frame.acceleration[DORSAL_SENSOR_INDEX];
        let denominator = vector_norm(finger) * vector_norm(dorsal);
        if denominator <= MIN_GRAVITY_NORM_COUNTS * MIN_GRAVITY_NORM_COUNTS {
            continue;
        }
        let cosine = (vector_dot(finger, dorsal) / denominator).clamp(-1.0, 1.0);
        tilt.observe(cosine.acos().to_degrees());
    }
    let total_sample_count = u64::try_from(frames.len()).unwrap_or(u64::MAX);
    let observable_sample_count = tilt.count;
    let mean_tilt_degrees = (observable_sample_count > 0).then_some(tilt.mean);
    let tilt_variance_degrees2 = (observable_sample_count > 0).then(|| tilt.variance());
    let tilt_jitter_rms_degrees = tilt_variance_degrees2.map(f64::sqrt);
    let mut statistics = FingerDorsalOrientationStatistics {
        finger_sensor_index: u8::try_from(sensor_index).expect("finger index fits u8"),
        total_sample_count,
        observable_sample_count,
        mean_tilt_degrees,
        tilt_variance_degrees2,
        tilt_jitter_rms_degrees,
        verdict: StabilityVerdict::Stable,
        faults: Vec::new(),
    };
    statistics.faults = orientation_faults(&statistics);
    statistics.verdict = verdict_for(&statistics.faults);
    statistics
}

fn derive_shared_motion(
    frames: &[SelectedFrame],
    sensors: &[SensorNeutralStatistics; SENSOR_COUNT],
) -> SharedMotionStatistics {
    let mut acceleration_shared_motion_energy = 0.0;
    let mut acceleration_independent_noise_energy = 0.0;
    let mut gyroscope_shared_motion_energy = 0.0;
    let mut gyroscope_independent_noise_energy = 0.0;
    for frame in frames {
        let acceleration_centered = std::array::from_fn::<_, SENSOR_COUNT, _>(|sensor_index| {
            vector_sub(
                frame.acceleration[sensor_index],
                sensors[sensor_index].acceleration_mean_gravity,
            )
        });
        let gyroscope_centered = std::array::from_fn::<_, SENSOR_COUNT, _>(|sensor_index| {
            vector_sub(
                frame.gyroscope[sensor_index],
                sensors[sensor_index].gyroscope_mean_bias,
            )
        });
        let shared_acceleration = vector_mean(&acceleration_centered);
        let shared_gyroscope = vector_mean(&gyroscope_centered);
        acceleration_shared_motion_energy += vector_squared_norm(shared_acceleration);
        gyroscope_shared_motion_energy += vector_squared_norm(shared_gyroscope);
        for sensor_index in 0..SENSOR_COUNT {
            acceleration_independent_noise_energy += vector_squared_norm(vector_sub(
                acceleration_centered[sensor_index],
                shared_acceleration,
            ));
            gyroscope_independent_noise_energy += vector_squared_norm(vector_sub(
                gyroscope_centered[sensor_index],
                shared_gyroscope,
            ));
        }
    }
    let count = frames.len() as f64;
    acceleration_shared_motion_energy /= count;
    gyroscope_shared_motion_energy /= count;
    acceleration_independent_noise_energy /= count * SENSOR_COUNT as f64;
    gyroscope_independent_noise_energy /= count * SENSOR_COUNT as f64;
    SharedMotionStatistics {
        sample_count: u64::try_from(frames.len()).unwrap_or(u64::MAX),
        acceleration_shared_motion_energy,
        acceleration_independent_noise_energy,
        gyroscope_shared_motion_energy,
        gyroscope_independent_noise_energy,
    }
}

fn validate_hand_report(
    hand: &HandNeutralReport,
    nominal_period_us: u32,
) -> Result<(), NeutralEvidenceError> {
    if hand.sample_count < MIN_NEUTRAL_SAMPLES_PER_HAND {
        return Err(NeutralEvidenceError::InvalidReport(
            "hand sample count is below the minimum",
        ));
    }
    if hand.derived_duration_ns() < MIN_NEUTRAL_SPAN_NS {
        return Err(NeutralEvidenceError::InvalidReport(
            "hand mapped span is below the minimum",
        ));
    }
    for (sensor_index, sensor) in hand.sensors.iter().enumerate() {
        validate_sensor_statistics(sensor, sensor_index, hand.sample_count, nominal_period_us)?;
    }
    for (sensor_index, orientation) in hand.finger_to_dorsal.iter().enumerate() {
        validate_orientation_statistics(orientation, sensor_index, hand.sample_count)?;
    }
    validate_shared_motion(&hand.shared_motion, hand.sample_count)?;
    require_finite(
        hand.sensors
            .iter()
            .map(|sensor| vector_norm(sensor.acceleration_mean_gravity))
            .sum::<f64>(),
        "derived shared-motion statistics",
    )?;
    let expected_faults = hand_faults(&hand.sensors, &hand.finger_to_dorsal, &hand.shared_motion);
    if hand.faults != expected_faults || hand.verdict != verdict_for(&expected_faults) {
        return Err(NeutralEvidenceError::InvalidReport(
            "hand verdict and faults disagree",
        ));
    }
    Ok(())
}

fn validate_sensor_statistics(
    sensor: &SensorNeutralStatistics,
    expected_index: usize,
    expected_count: u64,
    nominal_period_us: u32,
) -> Result<(), NeutralEvidenceError> {
    if usize::from(sensor.sensor_index) != expected_index || sensor.sample_count != expected_count {
        return Err(NeutralEvidenceError::InvalidReport(
            "sensor identity or sample count disagrees with its hand",
        ));
    }
    for value in sensor
        .acceleration_mean_gravity
        .iter()
        .chain(&sensor.acceleration_variance)
        .chain(&sensor.gyroscope_mean_bias)
        .chain(&sensor.gyroscope_variance)
        .copied()
        .chain([
            sensor.acceleration_noise_energy,
            sensor.gyroscope_noise_energy,
            sensor.sample_time_offset_mean_us,
            sensor.sample_time_offset_variance_us2,
            sensor.sample_time_jitter_rms_us,
            sensor.sample_time_skew_to_dorsal_mean_us,
            sensor.sample_time_skew_to_dorsal_variance_us2,
        ])
    {
        require_finite(value, "sensor statistics")?;
    }
    if sensor
        .acceleration_variance
        .iter()
        .any(|value| *value < 0.0)
        || sensor.gyroscope_variance.iter().any(|value| *value < 0.0)
        || sensor.acceleration_noise_energy < 0.0
        || sensor.gyroscope_noise_energy < 0.0
        || sensor.sample_time_offset_variance_us2 < 0.0
        || sensor.sample_time_jitter_rms_us < 0.0
        || sensor.sample_time_skew_to_dorsal_variance_us2 < 0.0
    {
        return Err(NeutralEvidenceError::InvalidReport(
            "variance, energy, and jitter values must be nonnegative",
        ));
    }

    for value in [
        vector_norm(sensor.acceleration_mean_gravity),
        sensor.acceleration_noise_energy.sqrt(),
        vector_norm(sensor.gyroscope_mean_bias),
        sensor.gyroscope_noise_energy.sqrt(),
    ] {
        require_finite(value, "derived sensor statistics")?;
    }

    let expected_faults = sensor_faults(sensor, nominal_period_us);
    if sensor.faults != expected_faults || sensor.verdict != verdict_for(&expected_faults) {
        return Err(NeutralEvidenceError::InvalidReport(
            "sensor verdict and faults disagree",
        ));
    }
    Ok(())
}

fn validate_orientation_statistics(
    orientation: &FingerDorsalOrientationStatistics,
    expected_index: usize,
    expected_count: u64,
) -> Result<(), NeutralEvidenceError> {
    if usize::from(orientation.finger_sensor_index) != expected_index
        || orientation.total_sample_count != expected_count
        || orientation.observable_sample_count > orientation.total_sample_count
    {
        return Err(NeutralEvidenceError::InvalidReport(
            "finger-to-dorsal identity or sample count is invalid",
        ));
    }
    for value in [
        orientation.mean_tilt_degrees,
        orientation.tilt_variance_degrees2,
        orientation.tilt_jitter_rms_degrees,
    ]
    .into_iter()
    .flatten()
    {
        require_finite(value, "finger-to-dorsal statistics")?;
        if value < 0.0 {
            return Err(NeutralEvidenceError::InvalidReport(
                "finger-to-dorsal statistics must be nonnegative",
            ));
        }
    }
    let has_statistics = orientation.mean_tilt_degrees.is_some()
        && orientation.tilt_variance_degrees2.is_some()
        && orientation.tilt_jitter_rms_degrees.is_some();
    if has_statistics != (orientation.observable_sample_count > 0) {
        return Err(NeutralEvidenceError::InvalidReport(
            "finger-to-dorsal optional statistics disagree",
        ));
    }
    let expected_faults = orientation_faults(orientation);
    if orientation.faults != expected_faults || orientation.verdict != verdict_for(&expected_faults)
    {
        return Err(NeutralEvidenceError::InvalidReport(
            "finger-to-dorsal verdict and faults disagree",
        ));
    }
    Ok(())
}

fn validate_shared_motion(
    shared: &SharedMotionStatistics,
    expected_count: u64,
) -> Result<(), NeutralEvidenceError> {
    if shared.sample_count != expected_count {
        return Err(NeutralEvidenceError::InvalidReport(
            "shared-motion sample count disagrees with its hand",
        ));
    }
    for value in [
        shared.acceleration_shared_motion_energy,
        shared.acceleration_independent_noise_energy,
        shared.gyroscope_shared_motion_energy,
        shared.gyroscope_independent_noise_energy,
    ] {
        require_finite(value, "shared-motion statistics")?;
        if value < 0.0 {
            return Err(NeutralEvidenceError::InvalidReport(
                "shared-motion energies must be nonnegative",
            ));
        }
    }
    Ok(())
}

fn sensor_faults(sensor: &SensorNeutralStatistics, nominal_period_us: u32) -> Vec<SensorFault> {
    let gravity_norm = vector_norm(sensor.acceleration_mean_gravity);
    let acceleration_noise_ratio = (gravity_norm > MIN_GRAVITY_NORM_COUNTS)
        .then(|| sensor.acceleration_noise_energy.sqrt() / gravity_norm);
    let gyroscope_bias_fraction = vector_norm(sensor.gyroscope_mean_bias) / SIGNED_RAW_SCALE;
    let gyroscope_noise_fraction = sensor.gyroscope_noise_energy.sqrt() / SIGNED_RAW_SCALE;
    let nominal_period_us = f64::from(nominal_period_us);
    let mut faults = Vec::new();
    if acceleration_noise_ratio.is_none() {
        faults.push(SensorFault::DegenerateGravityVector);
    }
    if acceleration_noise_ratio.is_some_and(|ratio| ratio > MAX_ACCELERATION_NOISE_RATIO) {
        faults.push(SensorFault::ExcessiveAccelerationNoise);
    }
    if gyroscope_bias_fraction > MAX_GYROSCOPE_BIAS_FRACTION {
        faults.push(SensorFault::ExcessiveGyroscopeBias);
    }
    if gyroscope_noise_fraction > MAX_GYROSCOPE_NOISE_FRACTION {
        faults.push(SensorFault::ExcessiveGyroscopeNoise);
    }
    if sensor.sample_time_jitter_rms_us / nominal_period_us > MAX_SAMPLE_TIME_JITTER_FRACTION {
        faults.push(SensorFault::ExcessiveSampleTimeJitter);
    }
    if sensor.sample_time_skew_to_dorsal_mean_us.abs() / nominal_period_us
        > MAX_SAMPLE_TIME_SKEW_FRACTION
    {
        faults.push(SensorFault::ExcessiveSampleTimeSkew);
    }
    faults
}

fn orientation_faults(orientation: &FingerDorsalOrientationStatistics) -> Vec<OrientationFault> {
    let mut faults = Vec::new();
    if orientation.observable_sample_count != orientation.total_sample_count {
        faults.push(OrientationFault::UnobservableGravity);
    }
    if orientation
        .tilt_jitter_rms_degrees
        .is_some_and(|jitter| jitter > MAX_RELATIVE_TILT_JITTER_DEGREES)
    {
        faults.push(OrientationFault::ExcessiveTiltJitter);
    }
    faults
}

fn hand_faults(
    sensors: &[SensorNeutralStatistics; SENSOR_COUNT],
    orientations: &[FingerDorsalOrientationStatistics; DORSAL_SENSOR_INDEX],
    shared_motion: &SharedMotionStatistics,
) -> Vec<HandFault> {
    let mut faults = Vec::new();
    for sensor in sensors {
        if sensor.verdict == StabilityVerdict::Fault {
            faults.push(HandFault::Sensor(sensor.sensor_index));
        }
    }
    for orientation in orientations {
        if orientation.verdict == StabilityVerdict::Fault {
            faults.push(HandFault::FingerToDorsal(orientation.finger_sensor_index));
        }
    }
    let mean_gravity_norm = sensors
        .iter()
        .map(|sensor| vector_norm(sensor.acceleration_mean_gravity))
        .sum::<f64>()
        / SENSOR_COUNT as f64;
    let shared_acceleration_ratio = (mean_gravity_norm > MIN_GRAVITY_NORM_COUNTS)
        .then(|| shared_motion.acceleration_shared_motion_energy.sqrt() / mean_gravity_norm);
    if shared_acceleration_ratio.is_none_or(|ratio| ratio > MAX_SHARED_ACCELERATION_MOTION_RATIO) {
        faults.push(HandFault::SharedAccelerationMotion);
    }
    if shared_motion.gyroscope_shared_motion_energy.sqrt() / SIGNED_RAW_SCALE
        > MAX_SHARED_GYROSCOPE_MOTION_FRACTION
    {
        faults.push(HandFault::SharedGyroscopeMotion);
    }
    faults
}

fn evidence_faults(hands: &[HandNeutralReport; 2]) -> Vec<EvidenceFault> {
    hands
        .iter()
        .filter(|hand| hand.verdict == StabilityVerdict::Fault)
        .map(|hand| EvidenceFault::HandNotStable(hand.hand))
        .collect()
}

fn require_finite(value: f64, field: &'static str) -> Result<(), NeutralEvidenceError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(NeutralEvidenceError::NonFiniteReport(field))
    }
}

fn verdict_for<T>(faults: &[T]) -> StabilityVerdict {
    if faults.is_empty() {
        StabilityVerdict::Stable
    } else {
        StabilityVerdict::Fault
    }
}

fn vector_sum(vector: [f64; 3]) -> f64 {
    vector.into_iter().sum()
}

fn vector_dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.into_iter().zip(right).map(|(a, b)| a * b).sum()
}

fn vector_squared_norm(vector: [f64; 3]) -> f64 {
    vector_dot(vector, vector)
}

fn vector_norm(vector: [f64; 3]) -> f64 {
    vector_squared_norm(vector).sqrt()
}

fn vector_sub(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| left[axis] - right[axis])
}

fn vector_mean<const N: usize>(vectors: &[[f64; 3]; N]) -> [f64; 3] {
    std::array::from_fn(|axis| vectors.iter().map(|vector| vector[axis]).sum::<f64>() / N as f64)
}

#[derive(Clone, Copy, Debug, Default)]
struct ScalarMoments {
    count: u64,
    mean: f64,
    squared_deviation_sum: f64,
}

impl ScalarMoments {
    fn observe(&mut self, value: f64) {
        self.count += 1;
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let updated_delta = value - self.mean;
        self.squared_deviation_sum += delta * updated_delta;
    }

    fn variance(self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            (self.squared_deviation_sum / self.count as f64).max(0.0)
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct VectorMoments {
    axes: [ScalarMoments; 3],
}

impl VectorMoments {
    fn observe(&mut self, value: [f64; 3]) {
        for (axis, value) in self.axes.iter_mut().zip(value) {
            axis.observe(value);
        }
    }

    fn mean(self) -> [f64; 3] {
        self.axes.map(|axis| axis.mean)
    }

    fn variance(self) -> [f64; 3] {
        self.axes.map(ScalarMoments::variance)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SensorAccumulator {
    acceleration: VectorMoments,
    gyroscope: VectorMoments,
    sample_time_offset: ScalarMoments,
    sample_time_skew_to_dorsal: ScalarMoments,
}

#[derive(Clone, Debug)]
struct SelectedFrame {
    mapped_time_ns: u64,
    acceleration: [[f64; 3]; SENSOR_COUNT],
    gyroscope: [[f64; 3]; SENSOR_COUNT],
    sample_time_offset_us: [f64; SENSOR_COUNT],
}

#[derive(Clone, Debug, Default)]
struct HandAccumulator {
    frames: Vec<SelectedFrame>,
}

impl HandAccumulator {
    fn observe(&mut self, frame: &SensorFrame, mapped_time_ns: u64) {
        let acceleration = frame
            .sensors
            .map(|sensor| sensor.acceleration.map(f64::from));
        let gyroscope = frame
            .sensors
            .map(|sensor| sensor.angular_velocity.map(f64::from));
        let sample_time_offset_us = frame
            .sensors
            .map(|sensor| f64::from(sensor.sample_time_offset_us));
        self.frames.push(SelectedFrame {
            mapped_time_ns,
            acceleration,
            gyroscope,
            sample_time_offset_us,
        });
    }
}

#[derive(Debug, Default)]
struct StreamTracker {
    raw: FrameIntegrity,
    previous_mapped_time_ns: Option<u64>,
}

impl StreamTracker {
    fn observe_raw(
        &mut self,
        record_index: u64,
        frame: &SensorFrame,
    ) -> Result<(), NeutralEvidenceError> {
        self.raw.observe(frame).map_err(|error| match error {
            FrameIntegrityError::Quality(reason) => {
                // Preserve the neutral API's acquisition-error precedence
                // when one frame carries both flags and missing sensors.
                if frame.flags != 0 {
                    return NeutralEvidenceError::AcquisitionFault {
                        record_index,
                        hand: frame.hand,
                        flags: frame.flags,
                    };
                }
                match reason {
                    AbstainReason::MissingSensor => {
                        let missing = (!frame.valid_sensor_mask) & ALL_SENSORS_VALID;
                        NeutralEvidenceError::MissingSensor {
                            record_index,
                            hand: frame.hand,
                            sensor_index: u8::try_from(missing.trailing_zeros())
                                .expect("six-bit sensor index fits u8"),
                        }
                    }
                    AbstainReason::SequenceDiscontinuity => {
                        NeutralEvidenceError::SequenceDiscontinuity {
                            record_index,
                            hand: frame.hand,
                            expected: self
                                .raw
                                .expected_sequence()
                                .expect("sequence faults require an established predecessor"),
                            actual: frame.sequence,
                        }
                    }
                    reason => NeutralEvidenceError::RawFrameQuality {
                        record_index,
                        hand: frame.hand,
                        reason,
                    },
                }
            }
            FrameIntegrityError::Structure(WindowError::TimeRegression) => {
                NeutralEvidenceError::DeviceTimeNotIncreasing {
                    record_index,
                    hand: frame.hand,
                }
            }
            FrameIntegrityError::Structure(error) => NeutralEvidenceError::RawFrameStructure {
                record_index,
                hand: frame.hand,
                error,
            },
        })
    }

    fn observe_mapped(
        &mut self,
        record_index: u64,
        hand: Hand,
        mapped_time_ns: u64,
    ) -> Result<(), NeutralEvidenceError> {
        if self
            .previous_mapped_time_ns
            .is_some_and(|previous| mapped_time_ns <= previous)
        {
            return Err(NeutralEvidenceError::MappedTimeNotIncreasing { record_index, hand });
        }
        self.previous_mapped_time_ns = Some(mapped_time_ns);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WindowHandCoverageState {
    sample_count: u64,
    first_mapped_time_ns: Option<u64>,
    last_mapped_time_ns: Option<u64>,
}

impl WindowHandCoverageState {
    fn observe(&mut self, mapped_time_ns: u64) {
        self.sample_count += 1;
        self.first_mapped_time_ns.get_or_insert(mapped_time_ns);
        self.last_mapped_time_ns = Some(mapped_time_ns);
    }

    fn report(self, hand: Hand) -> Option<WindowHandCoverage> {
        Some(WindowHandCoverage {
            hand,
            sample_count: self.sample_count,
            first_mapped_time_ns: self.first_mapped_time_ns?,
            last_mapped_time_ns: self.last_mapped_time_ns?,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct WindowCoverageState {
    hands: [WindowHandCoverageState; 2],
}

/// Why source material cannot produce or verify neutral evidence.
#[derive(Debug)]
pub enum NeutralEvidenceError {
    Manifest(ManifestError),
    ClockEpoch(ClockEpochError),
    ClockQualification(ClockEpochFitError),
    Session(SessionError),
    ReportTooLarge {
        actual: usize,
    },
    Json(String),
    Serialization(String),
    InvalidReportSchema,
    InvalidReportHash(&'static str),
    InvalidReport(&'static str),
    NonFiniteReport(&'static str),
    MissingHand(Hand),
    InvalidWindow {
        window_index: usize,
        reason: &'static str,
    },
    DuplicateWindowIdentity {
        window_index: usize,
    },
    WindowOutsideSession {
        window_index: usize,
        end_record_exclusive: u64,
        session_record_count: u64,
    },
    MissingWindowHand {
        window_index: usize,
        hand: Hand,
    },
    NominalPeriodMismatch {
        manifest_us: u32,
        session_us: u32,
    },
    InvalidCandidate {
        record_index: u64,
        kind: SessionRecordKind,
    },
    UnexpectedControlRecord {
        record_index: u64,
        kind: SessionRecordKind,
    },
    InvalidDecodedRecord {
        record_index: u64,
        error: RecordDecodeError,
    },
    FrameCanonicalization {
        record_index: u64,
        error: FrameCanonicalizationError,
    },
    AcquisitionFault {
        record_index: u64,
        hand: Hand,
        flags: u32,
    },
    MissingSensor {
        record_index: u64,
        hand: Hand,
        sensor_index: u8,
    },
    SequenceDiscontinuity {
        record_index: u64,
        hand: Hand,
        expected: u32,
        actual: u32,
    },
    DeviceTimeNotIncreasing {
        record_index: u64,
        hand: Hand,
    },
    RawFrameQuality {
        record_index: u64,
        hand: Hand,
        reason: AbstainReason,
    },
    RawFrameStructure {
        record_index: u64,
        hand: Hand,
        error: WindowError,
    },
    ClockMapping {
        record_index: u64,
        hand: Hand,
        error: ClockError,
    },
    MappedTimeNotIncreasing {
        record_index: u64,
        hand: Hand,
    },
    MappedReceiptDeltaTooLarge {
        record_index: u64,
        hand: Hand,
        actual_ns: u64,
    },
    TooFewSamples {
        hand: Hand,
        actual: u64,
        minimum: u64,
    },
    EvidenceSpanTooShort {
        hand: Hand,
        actual_ns: u64,
        minimum_ns: u64,
    },
    ReportMismatch,
}

impl fmt::Display for NeutralEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => write!(formatter, "neutral manifest is invalid: {error}"),
            Self::ClockEpoch(error) => {
                write!(formatter, "neutral clock epochs are invalid: {error}")
            }
            Self::ClockQualification(error) => {
                write!(formatter, "neutral clock qualification failed: {error}")
            }
            Self::Session(error) => write!(formatter, "neutral session is invalid: {error}"),
            Self::ReportTooLarge { actual } => write!(
                formatter,
                "neutral report is {actual} bytes; maximum is {MAX_NEUTRAL_REPORT_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "neutral report JSON is invalid: {error}"),
            Self::Serialization(error) => {
                write!(formatter, "neutral report serialization failed: {error}")
            }
            Self::InvalidReportSchema => formatter.write_str("neutral report schema is invalid"),
            Self::InvalidReportHash(field) => {
                write!(
                    formatter,
                    "neutral report {field} is not a canonical SHA-256"
                )
            }
            Self::InvalidReport(reason) => write!(formatter, "neutral report is invalid: {reason}"),
            Self::NonFiniteReport(field) => {
                write!(
                    formatter,
                    "neutral report {field} contains a nonfinite value"
                )
            }
            Self::MissingHand(hand) => write!(formatter, "neutral manifest is missing {hand:?}"),
            Self::InvalidWindow {
                window_index,
                reason,
            } => write!(
                formatter,
                "neutral window {window_index} is invalid: {reason}"
            ),
            Self::DuplicateWindowIdentity { window_index } => write!(
                formatter,
                "neutral window {window_index} repeats a block/window identity"
            ),
            Self::WindowOutsideSession {
                window_index,
                end_record_exclusive,
                session_record_count,
            } => write!(
                formatter,
                "neutral window {window_index} ends at record {end_record_exclusive}, but the session has {session_record_count} records"
            ),
            Self::MissingWindowHand { window_index, hand } => write!(
                formatter,
                "neutral window {window_index} contains no {hand:?} samples"
            ),
            Self::NominalPeriodMismatch {
                manifest_us,
                session_us,
            } => write!(
                formatter,
                "neutral manifest period {manifest_us} us does not match session period {session_us} us"
            ),
            Self::InvalidCandidate { record_index, kind } => write!(
                formatter,
                "neutral source record {record_index} is a {kind:?} candidate"
            ),
            Self::UnexpectedControlRecord { record_index, kind } => write!(
                formatter,
                "neutral source record {record_index} is a {kind:?} Gate 0A control record"
            ),
            Self::InvalidDecodedRecord {
                record_index,
                error,
            } => write!(
                formatter,
                "neutral decoded record {record_index} is invalid: {error}"
            ),
            Self::FrameCanonicalization {
                record_index,
                error,
            } => write!(
                formatter,
                "neutral decoded record {record_index} cannot enter canonical sensor coordinates: {error}"
            ),
            Self::AcquisitionFault {
                record_index,
                hand,
                flags,
            } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} carries acquisition faults {flags:#010x}"
            ),
            Self::MissingSensor {
                record_index,
                hand,
                sensor_index,
            } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} is missing sensor {sensor_index}"
            ),
            Self::SequenceDiscontinuity {
                record_index,
                hand,
                expected,
                actual,
            } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} has sequence {actual}; expected {expected}"
            ),
            Self::DeviceTimeNotIncreasing { record_index, hand } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} has nonincreasing device time"
            ),
            Self::RawFrameQuality {
                record_index,
                hand,
                reason,
            } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} has invalid raw acquisition quality: {reason:?}"
            ),
            Self::RawFrameStructure {
                record_index,
                hand,
                error,
            } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} has invalid raw timing: {error}"
            ),
            Self::ClockMapping {
                record_index,
                hand,
                error,
            } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} cannot map through its clock: {error}"
            ),
            Self::MappedTimeNotIncreasing { record_index, hand } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} has nonincreasing mapped time"
            ),
            Self::MappedReceiptDeltaTooLarge {
                record_index,
                hand,
                actual_ns,
            } => write!(
                formatter,
                "neutral record {record_index} for {hand:?} differs from receipt by {actual_ns} ns"
            ),
            Self::TooFewSamples {
                hand,
                actual,
                minimum,
            } => write!(
                formatter,
                "neutral {hand:?} has {actual} samples; minimum is {minimum}"
            ),
            Self::EvidenceSpanTooShort {
                hand,
                actual_ns,
                minimum_ns,
            } => write!(
                formatter,
                "neutral {hand:?} spans {actual_ns} ns; minimum is {minimum_ns} ns"
            ),
            Self::ReportMismatch => {
                formatter.write_str("neutral report does not match its immutable sources")
            }
        }
    }
}

impl Error for NeutralEvidenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::ClockEpoch(error) => Some(error),
            Self::ClockQualification(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::InvalidDecodedRecord { error, .. } => Some(error),
            Self::FrameCanonicalization { error, .. } => Some(error),
            Self::ClockMapping { error, .. } => Some(error),
            Self::RawFrameStructure { error, .. } => Some(error),
            _ => None,
        }
    }
}
