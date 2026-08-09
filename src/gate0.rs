//! Source-bound Gate 0A stream-integrity scoring for one physical hand.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    gate0_trace::Gate0TraceEvidence,
    layout::Hand,
    manifest::{AcquisitionManifest, ManifestError, format_sha256},
    protocol::{
        ALL_SENSORS_VALID, FLAG_ACQUISITION_OVERRUN, FrameIdentity, SENSOR_COUNT, SensorFrame,
    },
    session::{
        ControllerStream, FinalizedSession, SessionError, SessionRecordKind, SessionSummary,
    },
};

/// Stable schema identity for a source-bound Gate 0A scorecard.
pub const GATE0A_REPORT_SCHEMA: &str = "invisible-keyboard-gate0a-report-v6";

/// The roadmap requires at least 60 minutes of timed one-hand exposure.
pub const MIN_TIMED_EXPOSURE_US: u64 = 60 * 60 * 1_000_000;

/// At the selected 200 aggregate frames/s, 60 minutes schedules this many frames.
pub const MIN_TIMED_FRAME_COUNT: u64 = 720_000;

/// The roadmap requires a clean extra second after the timed exposure.
pub const MIN_CLEAN_TRAILING_EXPOSURE_US: u64 = 1_000_000;

/// The roadmap's direct recorded inter-sensor spread ceiling.
pub const MAX_SAMPLE_SPREAD_US: u64 = 5_000;

const VALID_RECEIPT_NUMERATOR: u128 = 999;
const VALID_RECEIPT_DENOMINATOR: u128 = 1_000;

/// PASS/FAIL/INCONCLUSIVE status shared by the overall gate and each criterion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Gate0Status {
    Pass,
    Fail,
    Inconclusive,
}

impl fmt::Display for Gate0Status {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Inconclusive => "inconclusive",
        })
    }
}

/// The one immutable controller target a one-hand capture command must open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OneHandCaptureTarget {
    pub hand: Hand,
    pub source: ControllerStream,
    pub identity: FrameIdentity,
}

/// Per-sensor timing facts derived from valid sensor readings.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SensorTimingMetrics {
    pub sensor_index: u8,
    pub valid_samples: u64,
    pub interval_samples: u64,
    pub observed_odr_hz: Option<f64>,
    pub interval_jitter_rms_us: Option<f64>,
    pub sample_offset_min_us: Option<i16>,
    pub sample_offset_max_us: Option<i16>,
    pub sample_offset_mean_us: Option<f64>,
    pub nonincreasing_sample_timestamps: u64,
}

/// Aggregate statistics over each frame's maximum minus minimum sample offset.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SampleOffsetSpreadMetrics {
    pub observed_frames: u64,
    pub max_us: Option<u64>,
    pub mean_us: Option<f64>,
    pub frames_over_limit: u64,
}

/// Every fact derived solely from the immutable manifest and finalized session.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gate0SoftwareMetrics {
    pub total_records: u64,
    pub decoded_frames: u64,
    pub invalid_records: u64,
    pub valid_frames: u64,
    pub accounted_frame_opportunities: u64,
    pub identity_mismatch_frames: u64,
    pub undeclared_source_records: u64,
    pub missing_frames: u64,
    pub duplicate_frames: u64,
    pub reordered_frames: u64,
    pub device_time_regressions: u64,
    pub host_time_regressions: u64,
    pub record_host_time_regressions: u64,
    pub sensor_timestamp_regressions: u64,
    pub frames_with_invalid_sensors: u64,
    pub invalid_sensor_readings: u64,
    pub clock_unsynced_frames: u64,
    pub acquisition_overruns: u64,
    pub sequence_gap_events: u64,
    pub fault_accounted_gap_events: u64,
    pub unexplained_gap_events: u64,
    pub unexplained_missing_frames: u64,
    pub capture_exposure_us: Option<u64>,
    pub timed_scheduled_frames: u64,
    pub clean_trailing_exposure_us: Option<u64>,
    pub sensors: [SensorTimingMetrics; SENSOR_COUNT],
    pub sample_offset_spread: SampleOffsetSpreadMetrics,
}

/// Fixed set of Gate 0A criteria. No omitted field can disappear in a vector.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Gate0Criteria {
    pub session_integrity: Gate0Status,
    pub manifest_session_binding: Gate0Status,
    pub one_hand_identity: Gate0Status,
    pub minimum_duration: Gate0Status,
    pub minimum_frame_count: Gate0Status,
    pub valid_receipt: Gate0Status,
    pub sequence_accounting: Gate0Status,
    pub timestamp_continuity: Gate0Status,
    pub explicit_fault_accounting: Gate0Status,
    pub timing_metrics_reported: Gate0Status,
    pub sample_spread: Gate0Status,
    pub clean_trailing_exposure: Gate0Status,
    pub external_evidence_binding: Gate0Status,
    pub logic_analyzer_timing: Gate0Status,
    pub deterministic_quiesce: Gate0Status,
}

impl Gate0Criteria {
    /// FAIL dominates INCONCLUSIVE, which dominates PASS.
    #[must_use]
    pub fn overall(&self) -> Gate0Status {
        let statuses = [
            self.session_integrity,
            self.manifest_session_binding,
            self.one_hand_identity,
            self.minimum_duration,
            self.minimum_frame_count,
            self.valid_receipt,
            self.sequence_accounting,
            self.timestamp_continuity,
            self.explicit_fault_accounting,
            self.timing_metrics_reported,
            self.sample_spread,
            self.clean_trailing_exposure,
            self.external_evidence_binding,
            self.logic_analyzer_timing,
            self.deterministic_quiesce,
        ];
        if statuses.contains(&Gate0Status::Fail) {
            Gate0Status::Fail
        } else if statuses.contains(&Gate0Status::Inconclusive) {
            Gate0Status::Inconclusive
        } else {
            Gate0Status::Pass
        }
    }
}

/// Immutable, source-bound result of one Gate 0A score.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gate0Report {
    schema: &'static str,
    source_manifest_sha256: String,
    source_session_sha256: String,
    raw_capture_sha256: String,
    capture_sidecar_sha256: String,
    session_id: [u8; 16],
    hand: Hand,
    metrics: Gate0SoftwareMetrics,
    trace: Gate0TraceEvidence,
    criteria: Gate0Criteria,
    verdict: Gate0Status,
}

impl Gate0Report {
    #[must_use]
    pub fn schema(&self) -> &str {
        self.schema
    }

    #[must_use]
    pub fn source_manifest_sha256(&self) -> &str {
        &self.source_manifest_sha256
    }

    #[must_use]
    pub fn source_session_sha256(&self) -> &str {
        &self.source_session_sha256
    }

    #[must_use]
    pub const fn session_id(&self) -> [u8; 16] {
        self.session_id
    }

    #[must_use]
    pub const fn hand(&self) -> Hand {
        self.hand
    }

    #[must_use]
    pub const fn metrics(&self) -> &Gate0SoftwareMetrics {
        &self.metrics
    }

    #[must_use]
    pub const fn trace(&self) -> &Gate0TraceEvidence {
        &self.trace
    }

    #[must_use]
    pub const fn criteria(&self) -> &Gate0Criteria {
        &self.criteria
    }

    #[must_use]
    pub const fn verdict(&self) -> Gate0Status {
        self.verdict
    }

    /// Serialize the derived report to compact JSON without accepting caller edits.
    pub fn canonical_json(&self) -> Result<Vec<u8>, Gate0Error> {
        serde_json::to_vec(self).map_err(|error| Gate0Error::Serialization(error.to_string()))
    }
}

/// Input or internal-consistency errors that prevent scoring.
#[derive(Debug)]
pub enum Gate0Error {
    Manifest(ManifestError),
    ExpectedOneHand { actual: usize },
    Session(SessionError),
    AccountingMismatch(&'static str),
    Serialization(String),
}

impl fmt::Display for Gate0Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => error.fmt(formatter),
            Self::ExpectedOneHand { actual } => {
                write!(
                    formatter,
                    "Gate 0A manifest has {actual} hands; expected exactly one"
                )
            }
            Self::Session(error) => error.fmt(formatter),
            Self::AccountingMismatch(field) => {
                write!(formatter, "finalized-session replay disagrees on {field}")
            }
            Self::Serialization(error) => {
                write!(formatter, "Gate 0A report serialization failed: {error}")
            }
        }
    }
}

impl Error for Gate0Error {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::ExpectedOneHand { .. } | Self::AccountingMismatch(_) | Self::Serialization(_) => {
                None
            }
        }
    }
}

impl From<ManifestError> for Gate0Error {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<SessionError> for Gate0Error {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

/// Resolve the single physical source and immutable wire identity for Gate 0A.
///
/// This helper is the CLI seam. One physical source supplies the one declared
/// hand to both the transport and session writer.
pub fn one_hand_capture_target(
    manifest: &AcquisitionManifest,
) -> Result<OneHandCaptureTarget, Gate0Error> {
    let mut identities = manifest.frame_identities()?;
    if identities.len() != 1 {
        return Err(Gate0Error::ExpectedOneHand {
            actual: identities.len(),
        });
    }
    let identity = identities.pop().expect("one-hand identity exists");
    Ok(OneHandCaptureTarget {
        hand: identity.hand,
        source: ControllerStream::from(identity.hand),
        identity,
    })
}

/// Derive and score every Gate 0A fact available from canonical evidence.
///
/// Physical evidence is mandatory and can only come from the repository-owned
/// raw DSLogic reducer. The scorer accepts no caller-authored aggregate path.
pub fn score_gate0a(
    manifest: &AcquisitionManifest,
    session: &FinalizedSession,
    trace: &Gate0TraceEvidence,
) -> Result<Gate0Report, Gate0Error> {
    let target = one_hand_capture_target(manifest)?;
    let manifest_sha256 = manifest.sha256()?;
    let session_sha256 = session.sha256();
    let summary = *session.summary();
    let target_summary = hand_summary(&summary, target.hand);

    let mut sequence = SequenceTracker::default();
    let mut timing = TimingAccumulator::default();
    let mut first_device_time_us = None;
    let mut last_device_time_us = None;
    let mut timed_cutoff_us = None;
    let mut timed_scheduled_frames = 0_u64;
    let mut previous_target_host_time_ns = None;
    let mut previous_record_host_time_ns = None;
    let mut clean_tail_start_us = None;
    let mut clean_tail_last_us = None;
    let mut scanned_records = 0_u64;
    let mut scanned_invalid_records = 0_u64;
    let mut scanned_decoded_frames = 0_u64;
    let mut valid_frames = 0_u64;
    let mut identity_mismatch_frames = 0_u64;
    let mut undeclared_source_records = 0_u64;
    let mut record_host_time_regressions = 0_u64;
    let mut sequence_gap_events = 0_u64;
    let mut fault_accounted_gap_events = 0_u64;
    let mut unexplained_gap_events = 0_u64;
    let mut unexplained_missing_frames = 0_u64;
    let mut invalid_target_candidate_since_in_order = false;

    let mut reader = session.reader();
    while let Some(record) = reader.next_record()? {
        scanned_records = scanned_records.saturating_add(1);
        let record_host_regressed =
            previous_record_host_time_ns.is_some_and(|previous| record.host_time_ns < previous);
        if record_host_regressed {
            record_host_time_regressions = record_host_time_regressions.saturating_add(1);
        }
        previous_record_host_time_ns = Some(record.host_time_ns);

        if matches!(
            record.kind,
            SessionRecordKind::QuiesceRequest | SessionRecordKind::QuiesceReceipt
        ) {
            continue;
        }

        if record.source != target.source {
            undeclared_source_records = undeclared_source_records.saturating_add(1);
            reset_clean_tail(&mut clean_tail_start_us, &mut clean_tail_last_us);
            if record.kind != SessionRecordKind::Decoded || record.decode().is_err() {
                scanned_invalid_records = scanned_invalid_records.saturating_add(1);
            } else {
                scanned_decoded_frames = scanned_decoded_frames.saturating_add(1);
            }
            continue;
        }
        if record.kind != SessionRecordKind::Decoded {
            scanned_invalid_records = scanned_invalid_records.saturating_add(1);
            invalid_target_candidate_since_in_order = true;
            reset_clean_tail(&mut clean_tail_start_us, &mut clean_tail_last_us);
            continue;
        }
        let Ok(frame) = record.decode() else {
            scanned_invalid_records = scanned_invalid_records.saturating_add(1);
            invalid_target_candidate_since_in_order = true;
            reset_clean_tail(&mut clean_tail_start_us, &mut clean_tail_last_us);
            continue;
        };
        scanned_decoded_frames = scanned_decoded_frames.saturating_add(1);

        let frame_identity_matches = frame.identity() == target.identity;
        if !frame_identity_matches {
            identity_mismatch_frames = identity_mismatch_frames.saturating_add(1);
        }
        let sequence_observation = sequence.observe(frame.sequence);
        if let SequenceObservation::Gap(missing) = sequence_observation {
            sequence_gap_events = sequence_gap_events.saturating_add(1);
            if frame.flags & FLAG_ACQUISITION_OVERRUN != 0
                || invalid_target_candidate_since_in_order
            {
                fault_accounted_gap_events = fault_accounted_gap_events.saturating_add(1);
            } else {
                unexplained_gap_events = unexplained_gap_events.saturating_add(1);
                unexplained_missing_frames = unexplained_missing_frames.saturating_add(missing);
            }
        }
        if matches!(
            sequence_observation,
            SequenceObservation::First | SequenceObservation::InOrder | SequenceObservation::Gap(_)
        ) {
            invalid_target_candidate_since_in_order = false;
        }
        let device_time_regressed =
            last_device_time_us.is_some_and(|previous| frame.device_time_us < previous);
        let target_host_time_regressed =
            previous_target_host_time_ns.is_some_and(|previous| record.host_time_ns < previous);
        previous_target_host_time_ns = Some(record.host_time_ns);

        let first_time = *first_device_time_us.get_or_insert(frame.device_time_us);
        let cutoff =
            *timed_cutoff_us.get_or_insert(first_time.saturating_add(MIN_TIMED_EXPOSURE_US));
        if frame.device_time_us < cutoff {
            timed_scheduled_frames =
                timed_scheduled_frames.saturating_add(sequence_observation.scheduled_increment());
        }
        last_device_time_us = Some(frame.device_time_us);

        let frame_timing = frame_identity_matches.then(|| timing.observe(&frame));
        let frame_is_fresh = !matches!(
            sequence_observation,
            SequenceObservation::Duplicate | SequenceObservation::Reordered
        );
        let frame_is_valid = frame_identity_matches
            && frame.flags == 0
            && frame.valid_sensor_mask == ALL_SENSORS_VALID
            && frame_is_fresh
            && !device_time_regressed
            && !target_host_time_regressed
            && !record_host_regressed
            && frame_timing.is_some_and(|timing| !timing.nonincreasing_sample_timestamp);
        if frame_is_valid {
            valid_frames = valid_frames.saturating_add(1);
        }

        let frame_is_clean = frame_is_valid
            && frame_timing.is_some_and(|timing| timing.spread_us <= MAX_SAMPLE_SPREAD_US);
        if !frame_is_clean {
            reset_clean_tail(&mut clean_tail_start_us, &mut clean_tail_last_us);
            continue;
        }
        if matches!(sequence_observation, SequenceObservation::Gap(_)) {
            reset_clean_tail(&mut clean_tail_start_us, &mut clean_tail_last_us);
        }
        if frame.device_time_us >= cutoff {
            clean_tail_start_us.get_or_insert(frame.device_time_us);
            clean_tail_last_us = Some(frame.device_time_us);
        }
    }

    verify_replay_counts(
        &summary,
        scanned_records,
        scanned_invalid_records,
        scanned_decoded_frames,
    )?;

    let capture_exposure_us = exposure_us(
        first_device_time_us,
        last_device_time_us,
        manifest.nominal_period_us,
    );
    let clean_trailing_exposure_us = exposure_us(
        clean_tail_start_us,
        clean_tail_last_us,
        manifest.nominal_period_us,
    );
    // The analyzer sees leading transport drops that a session-only sequence
    // tracker cannot infer before its first decoded frame. Use every captured
    // D8 aggregate as the valid-receipt denominator.
    let accounted_frame_opportunities = trace.metrics().snapshots;
    let sensors = timing.sensor_metrics();
    let sample_offset_spread = timing.spread.metrics();
    let sensor_timestamp_regressions = sensors
        .iter()
        .map(|sensor| sensor.nonincreasing_sample_timestamps)
        .sum();

    let binding_status = status(trace.binds(&manifest_sha256, &session_sha256));
    let logic_status = status(binding_status == Gate0Status::Pass && trace.is_clean());

    let other_summary = hand_summary(&summary, opposite(target.hand));
    let one_hand_identity = identity_mismatch_frames == 0
        && undeclared_source_records == 0
        && target_summary.received > 0
        && other_summary.received == 0;
    let timing_metrics_reported = sensors.iter().all(|sensor| {
        sensor.observed_odr_hz.is_some()
            && sensor.interval_jitter_rms_us.is_some()
            && sensor.sample_offset_min_us.is_some()
            && sensor.sample_offset_max_us.is_some()
            && sensor.sample_offset_mean_us.is_some()
    });
    let sample_spread = status(
        sample_offset_spread
            .max_us
            .is_some_and(|spread| spread <= MAX_SAMPLE_SPREAD_US)
            && trace.max_snapshot_spread_us() <= MAX_SAMPLE_SPREAD_US,
    );
    let criteria = Gate0Criteria {
        session_integrity: Gate0Status::Pass,
        manifest_session_binding: status(
            session.header().manifest_sha256 == manifest_sha256
                && session.header().nominal_period_us == manifest.nominal_period_us,
        ),
        one_hand_identity: status(one_hand_identity),
        minimum_duration: status(
            capture_exposure_us.is_some_and(|duration| duration >= MIN_TIMED_EXPOSURE_US),
        ),
        minimum_frame_count: status(timed_scheduled_frames >= MIN_TIMED_FRAME_COUNT),
        valid_receipt: status(valid_receipt_passes(
            valid_frames,
            accounted_frame_opportunities,
        )),
        sequence_accounting: status(
            target_summary.received > 0
                && target_summary.duplicates == 0
                && target_summary.reordered == 0,
        ),
        timestamp_continuity: status(
            target_summary.device_time_regressions == 0
                && target_summary.host_time_regressions == 0
                && record_host_time_regressions == 0
                && sensor_timestamp_regressions == 0,
        ),
        explicit_fault_accounting: status(unexplained_gap_events == 0),
        timing_metrics_reported: status(timing_metrics_reported),
        sample_spread,
        clean_trailing_exposure: status(
            clean_trailing_exposure_us
                .is_some_and(|duration| duration >= MIN_CLEAN_TRAILING_EXPOSURE_US),
        ),
        external_evidence_binding: binding_status,
        logic_analyzer_timing: logic_status,
        deterministic_quiesce: status(
            binding_status == Gate0Status::Pass && trace.deterministic_quiesce(),
        ),
    };
    let metrics = Gate0SoftwareMetrics {
        total_records: summary.total_records,
        decoded_frames: target_summary
            .received
            .saturating_add(other_summary.received),
        invalid_records: summary.invalid_records,
        valid_frames,
        accounted_frame_opportunities,
        identity_mismatch_frames,
        undeclared_source_records,
        missing_frames: target_summary.missing,
        duplicate_frames: target_summary.duplicates,
        reordered_frames: target_summary.reordered,
        device_time_regressions: target_summary.device_time_regressions,
        host_time_regressions: target_summary.host_time_regressions,
        record_host_time_regressions,
        sensor_timestamp_regressions,
        frames_with_invalid_sensors: target_summary.frames_with_invalid_sensors,
        invalid_sensor_readings: target_summary.invalid_sensor_readings,
        clock_unsynced_frames: target_summary.clock_unsynced,
        acquisition_overruns: target_summary.acquisition_overruns,
        sequence_gap_events,
        fault_accounted_gap_events,
        unexplained_gap_events,
        unexplained_missing_frames,
        capture_exposure_us,
        timed_scheduled_frames,
        clean_trailing_exposure_us,
        sensors,
        sample_offset_spread,
    };
    Ok(Gate0Report {
        schema: GATE0A_REPORT_SCHEMA,
        source_manifest_sha256: format_sha256(&manifest_sha256),
        source_session_sha256: format_sha256(&session_sha256),
        raw_capture_sha256: format_sha256(trace.raw_capture_sha256()),
        capture_sidecar_sha256: format_sha256(trace.sidecar_sha256()),
        session_id: session.header().session_id,
        hand: target.hand,
        metrics,
        trace: trace.clone(),
        criteria,
        verdict: criteria.overall(),
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct RunningMoments {
    count: u64,
    mean: f64,
    sum_squared_deviation: f64,
}

impl RunningMoments {
    fn observe(&mut self, value: f64) {
        self.count = self.count.saturating_add(1);
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let next_delta = value - self.mean;
        self.sum_squared_deviation += delta * next_delta;
    }

    fn mean(self) -> Option<f64> {
        (self.count > 0).then_some(self.mean)
    }

    fn population_standard_deviation(self) -> Option<f64> {
        (self.count > 0).then(|| {
            (self.sum_squared_deviation / self.count as f64)
                .max(0.0)
                .sqrt()
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SensorTimingAccumulator {
    first_sample_time_us: Option<i128>,
    previous_sample_time_us: Option<i128>,
    intervals: RunningMoments,
    offsets: RunningMoments,
    offset_min_us: Option<i16>,
    offset_max_us: Option<i16>,
    nonincreasing_sample_timestamps: u64,
}

impl SensorTimingAccumulator {
    fn observe(&mut self, sample_time_us: i128, offset_us: i16) -> bool {
        self.first_sample_time_us.get_or_insert(sample_time_us);
        self.offsets.observe(f64::from(offset_us));
        self.offset_min_us = Some(
            self.offset_min_us
                .map_or(offset_us, |current| current.min(offset_us)),
        );
        self.offset_max_us = Some(
            self.offset_max_us
                .map_or(offset_us, |current| current.max(offset_us)),
        );

        let nonincreasing = self
            .previous_sample_time_us
            .is_some_and(|previous| sample_time_us <= previous);
        if nonincreasing {
            self.nonincreasing_sample_timestamps =
                self.nonincreasing_sample_timestamps.saturating_add(1);
        } else if let Some(previous) = self.previous_sample_time_us {
            self.intervals.observe((sample_time_us - previous) as f64);
        }
        self.previous_sample_time_us = Some(sample_time_us);
        nonincreasing
    }

    fn metrics(self, sensor_index: usize) -> SensorTimingMetrics {
        let valid_samples = self.offsets.count;
        let observed_odr_hz = match (self.first_sample_time_us, self.previous_sample_time_us) {
            (Some(first), Some(last)) if valid_samples >= 2 && last > first => {
                Some((valid_samples.saturating_sub(1) as f64) * 1_000_000.0 / (last - first) as f64)
            }
            _ => None,
        };
        SensorTimingMetrics {
            sensor_index: u8::try_from(sensor_index).expect("six sensor indices fit u8"),
            valid_samples,
            interval_samples: self.intervals.count,
            observed_odr_hz,
            interval_jitter_rms_us: self.intervals.population_standard_deviation(),
            sample_offset_min_us: self.offset_min_us,
            sample_offset_max_us: self.offset_max_us,
            sample_offset_mean_us: self.offsets.mean(),
            nonincreasing_sample_timestamps: self.nonincreasing_sample_timestamps,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SpreadAccumulator {
    moments: RunningMoments,
    max_us: Option<u64>,
    frames_over_limit: u64,
}

impl SpreadAccumulator {
    fn observe(&mut self, spread_us: u64) {
        self.moments.observe(spread_us as f64);
        self.max_us = Some(
            self.max_us
                .map_or(spread_us, |current| current.max(spread_us)),
        );
        if spread_us > MAX_SAMPLE_SPREAD_US {
            self.frames_over_limit = self.frames_over_limit.saturating_add(1);
        }
    }

    fn metrics(self) -> SampleOffsetSpreadMetrics {
        SampleOffsetSpreadMetrics {
            observed_frames: self.moments.count,
            max_us: self.max_us,
            mean_us: self.moments.mean(),
            frames_over_limit: self.frames_over_limit,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TimingAccumulator {
    sensors: [SensorTimingAccumulator; SENSOR_COUNT],
    spread: SpreadAccumulator,
}

impl TimingAccumulator {
    fn observe(&mut self, frame: &SensorFrame) -> FrameTimingObservation {
        let offsets = frame.sensors.map(|sensor| sensor.sample_time_offset_us);
        let minimum_offset = offsets.iter().copied().min().expect("six offsets");
        let maximum_offset = offsets.iter().copied().max().expect("six offsets");
        let spread_us = u64::try_from(i32::from(maximum_offset) - i32::from(minimum_offset))
            .expect("maximum offset is not less than minimum");
        self.spread.observe(spread_us);

        let mut nonincreasing_sample_timestamp = false;
        for (sensor_index, sensor) in frame.sensors.iter().enumerate() {
            if frame.valid_sensor_mask & (1_u8 << sensor_index) == 0 {
                continue;
            }
            let sample_time_us =
                i128::from(frame.device_time_us) + i128::from(sensor.sample_time_offset_us);
            nonincreasing_sample_timestamp |=
                self.sensors[sensor_index].observe(sample_time_us, sensor.sample_time_offset_us);
        }
        FrameTimingObservation {
            spread_us,
            nonincreasing_sample_timestamp,
        }
    }

    fn sensor_metrics(self) -> [SensorTimingMetrics; SENSOR_COUNT] {
        std::array::from_fn(|sensor_index| self.sensors[sensor_index].metrics(sensor_index))
    }
}

#[derive(Clone, Copy, Debug)]
struct FrameTimingObservation {
    spread_us: u64,
    nonincreasing_sample_timestamp: bool,
}

#[derive(Clone, Copy, Debug)]
enum SequenceObservation {
    First,
    InOrder,
    Gap(u64),
    Duplicate,
    Reordered,
}

impl SequenceObservation {
    const fn scheduled_increment(self) -> u64 {
        match self {
            Self::First | Self::InOrder => 1,
            Self::Gap(missing) => missing.saturating_add(1),
            Self::Duplicate | Self::Reordered => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SequenceTracker {
    last_in_order: Option<u32>,
}

impl SequenceTracker {
    fn observe(&mut self, current: u32) -> SequenceObservation {
        let Some(previous) = self.last_in_order else {
            self.last_in_order = Some(current);
            return SequenceObservation::First;
        };
        if current == previous {
            return SequenceObservation::Duplicate;
        }
        let expected = previous.wrapping_add(1);
        let forward_delta = current.wrapping_sub(expected);
        if forward_delta == 0 {
            self.last_in_order = Some(current);
            SequenceObservation::InOrder
        } else if forward_delta < (1_u32 << 31) {
            self.last_in_order = Some(current);
            SequenceObservation::Gap(u64::from(forward_delta))
        } else {
            SequenceObservation::Reordered
        }
    }
}

fn verify_replay_counts(
    summary: &SessionSummary,
    scanned_records: u64,
    scanned_invalid_records: u64,
    scanned_decoded_frames: u64,
) -> Result<(), Gate0Error> {
    if scanned_records != summary.total_records {
        return Err(Gate0Error::AccountingMismatch("total record count"));
    }
    if scanned_invalid_records != summary.invalid_records {
        return Err(Gate0Error::AccountingMismatch("invalid record count"));
    }
    if scanned_decoded_frames != summary.left.received.saturating_add(summary.right.received) {
        return Err(Gate0Error::AccountingMismatch("decoded frame count"));
    }
    Ok(())
}

const fn hand_summary(summary: &SessionSummary, hand: Hand) -> crate::session::HandStreamSummary {
    match hand {
        Hand::Left => summary.left,
        Hand::Right => summary.right,
    }
}

const fn opposite(hand: Hand) -> Hand {
    match hand {
        Hand::Left => Hand::Right,
        Hand::Right => Hand::Left,
    }
}

const fn status(condition: bool) -> Gate0Status {
    if condition {
        Gate0Status::Pass
    } else {
        Gate0Status::Fail
    }
}

fn valid_receipt_passes(valid: u64, opportunities: u64) -> bool {
    opportunities > 0
        && u128::from(valid) * VALID_RECEIPT_DENOMINATOR
            >= u128::from(opportunities) * VALID_RECEIPT_NUMERATOR
}

fn exposure_us(first: Option<u64>, last: Option<u64>, nominal_period_us: u32) -> Option<u64> {
    match (first, last) {
        (Some(first), Some(last)) if last >= first => Some(
            last.saturating_sub(first)
                .saturating_add(u64::from(nominal_period_us)),
        ),
        _ => None,
    }
}

fn reset_clean_tail(start: &mut Option<u64>, last: &mut Option<u64>) {
    *start = None;
    *last = None;
}
