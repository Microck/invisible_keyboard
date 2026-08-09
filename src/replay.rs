//! Artifact-bound deterministic two-hand session replay.

use std::{error::Error, fmt, time::Instant};

use crate::{
    artifact::{ArtifactError, CalibrationArtifact},
    clock::{ClockError, percentile},
    clock_epoch::{ClockEpochError, ClockEpochFitError, ClockEpochSet},
    decoder::{AbstainReason, DecisionKind, FrameIntegrity, FrameIntegrityError, WindowError},
    device_transport::DEFAULT_MAX_PENDING_CANDIDATES,
    layout::Hand,
    manifest::{
        AcquisitionManifest, FrameCanonicalizationError, FrameCanonicalizer, ManifestError,
        parse_sha256,
    },
    output::{ApplyAnomaly, FaultReason, OutputAdapter, OutputGateError},
    pipeline::{DECISION_DELAY_FRAMES, PipelineError},
    protocol::{FrameError, SensorFrame},
    runtime::{
        LiveRuntime, RuntimeConfigurationError, RuntimeError, RuntimeOutputError,
        RuntimeStartError, RuntimeStep, TransportFault,
    },
    session::{FinalizedSession, RawSessionRecord, SessionError, SessionRecordKind},
    transport::TransportCandidate,
    validation::HeldOutValidation,
};

/// Frames may arrive this far after their fitted sensor time and still enter
/// the bounded host-time merger.
pub const MAX_MERGE_LATENESS_NS: u64 = 20_000_000;

/// Maximum records retained behind the replay/live merge watermark.
///
/// The canonical CLI feeds live decoding through `DeviceTransport::default()`,
/// so using its hard backlog limit keeps both merge layers on one bound. The
/// fixed two-hand 200 Hz profile normally needs only eight records for this
/// 20 ms window; 64 also admits the transport's full valid burst.
pub const MAX_MERGE_PENDING_RECORDS: usize = DEFAULT_MAX_PENDING_CANDIDATES;

/// A fitted controller time farther from host receipt than this is treated as
/// a broken epoch or transport path, not silently reordered.
pub const MAX_MAPPED_RECEIPT_DELTA_NS: u64 = 20_000_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LatencyPercentiles {
    pub p50_ns: u64,
    pub p90_ns: u64,
    pub p95_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
}

impl LatencyPercentiles {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let p50_ns = percentile(&mut samples, 50);
        let p90_ns = percentile(&mut samples, 90);
        let p95_ns = percentile(&mut samples, 95);
        let p99_ns = percentile(&mut samples, 99);
        let max_ns = *samples.iter().max().expect("nonempty latency samples");
        Self {
            p50_ns,
            p90_ns,
            p95_ns,
            p99_ns,
            max_ns,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CalibratedRuntimeFault {
    Transport(TransportFault),
    Frame(FrameError),
    Pipeline(PipelineError),
    UnconfiguredHand(Hand),
    State(ApplyAnomaly),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalibratedReplayReport {
    pub total_records: u64,
    pub decoded_frames: u64,
    pub left_frames: u64,
    pub right_frames: u64,
    pub invalid_candidates: u64,
    pub reordered_records: u64,
    pub decisions: u64,
    pub key_decisions: u64,
    pub abstentions: u64,
    pub output_events: u64,
    pub runtime_faults: Vec<CalibratedRuntimeFault>,
    pub fixed_post_trigger_delay_us: u64,
    pub processing_latency: LatencyPercentiles,
    pub mapped_receipt_delta: LatencyPercentiles,
    pub final_key_state_neutral: bool,
}

impl CalibratedReplayReport {
    #[must_use]
    pub const fn evidence_label(&self) -> &'static str {
        "replay-tested"
    }

    #[must_use]
    pub const fn limitation(&self) -> &'static str {
        "Recorded or generated replay does not establish physical typing feasibility."
    }
}

pub struct CalibratedReplayExecution<A> {
    pub report: CalibratedReplayReport,
    pub output: A,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeBindingError {
    Manifest(ManifestError),
    Artifact(ArtifactError),
    ExpectedTwoHands,
    CalibrationManifestMismatch,
    HeldOutValidationArtifactMismatch,
    ArtifactReferenceMismatch,
    HeldOutValidationReferenceMismatch,
    AcquisitionSettingMismatch,
    SensorProvenanceMismatch { hand: Hand, sensor_index: u8 },
}

impl fmt::Display for RuntimeBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => write!(formatter, "runtime manifest is invalid: {error}"),
            Self::Artifact(error) => write!(formatter, "runtime artifact is invalid: {error}"),
            Self::ExpectedTwoHands => formatter.write_str("runtime requires left and right hands"),
            Self::CalibrationManifestMismatch => {
                formatter.write_str("artifact belongs to another calibration manifest")
            }
            Self::HeldOutValidationArtifactMismatch => {
                formatter.write_str("held-out validation belongs to another artifact")
            }
            Self::ArtifactReferenceMismatch => {
                formatter.write_str("runtime manifest does not name this artifact")
            }
            Self::HeldOutValidationReferenceMismatch => formatter
                .write_str("runtime manifest does not name this held-out validation report"),
            Self::AcquisitionSettingMismatch => formatter
                .write_str("runtime acquisition settings differ from the calibration acquisition"),
            Self::SensorProvenanceMismatch { hand, sensor_index } => write!(
                formatter,
                "runtime {hand:?} sensor {sensor_index} physical provenance differs from calibration"
            ),
        }
    }
}

impl Error for RuntimeBindingError {}

enum PendingPayload {
    Frame(SensorFrame),
    Candidate(TransportCandidate),
}

struct PendingRecord {
    sort_time_ns: u64,
    receipt_time_ns: u64,
    ordinal: u64,
    payload: PendingPayload,
}

struct ReplayAccumulator {
    report: CalibratedReplayReport,
    processing_samples: Vec<u64>,
    receipt_delta_samples: Vec<u64>,
    highest_processed_ordinal: Option<u64>,
}

impl ReplayAccumulator {
    fn new(nominal_period_us: u32) -> Self {
        Self {
            report: CalibratedReplayReport {
                total_records: 0,
                decoded_frames: 0,
                left_frames: 0,
                right_frames: 0,
                invalid_candidates: 0,
                reordered_records: 0,
                decisions: 0,
                key_decisions: 0,
                abstentions: 0,
                output_events: 0,
                runtime_faults: Vec::new(),
                fixed_post_trigger_delay_us: u64::from(nominal_period_us)
                    * u64::from(DECISION_DELAY_FRAMES),
                processing_latency: LatencyPercentiles::default(),
                mapped_receipt_delta: LatencyPercentiles::default(),
                final_key_state_neutral: false,
            },
            processing_samples: Vec::new(),
            receipt_delta_samples: Vec::new(),
            highest_processed_ordinal: None,
        }
    }

    fn observe_step(&mut self, step: RuntimeStep) {
        self.report.decisions += 1;
        match step.decision.kind {
            DecisionKind::Key(_) => self.report.key_decisions += 1,
            DecisionKind::Abstain(_) => self.report.abstentions += 1,
        }
        self.report.output_events += u64::try_from(step.events.len()).unwrap_or(u64::MAX);
    }

    fn observe_fault<E>(&mut self, error: RuntimeError<E>) -> Result<(), CalibratedReplayError<E>> {
        let fault = match error {
            RuntimeError::Transport(error) => CalibratedRuntimeFault::Transport(error),
            RuntimeError::Frame(error) => CalibratedRuntimeFault::Frame(error),
            RuntimeError::Pipeline(error) => CalibratedRuntimeFault::Pipeline(error),
            RuntimeError::UnconfiguredHand(hand) => CalibratedRuntimeFault::UnconfiguredHand(hand),
            RuntimeError::State(error) => CalibratedRuntimeFault::State(error),
            RuntimeError::Gate(error) => return Err(CalibratedReplayError::Gate(error)),
            RuntimeError::Output(error) => return Err(CalibratedReplayError::Output(error)),
        };
        self.report.runtime_faults.push(fault);
        Ok(())
    }

    fn observe_order(&mut self, ordinal: u64) {
        if self
            .highest_processed_ordinal
            .is_some_and(|highest| ordinal < highest)
        {
            self.report.reordered_records += 1;
        }
        self.highest_processed_ordinal = Some(
            self.highest_processed_ordinal
                .map_or(ordinal, |highest| highest.max(ordinal)),
        );
    }

    fn finish(mut self, final_key_state_neutral: bool) -> CalibratedReplayReport {
        self.report.processing_latency = LatencyPercentiles::from_samples(self.processing_samples);
        self.report.mapped_receipt_delta =
            LatencyPercentiles::from_samples(self.receipt_delta_samples);
        self.report.final_key_state_neutral = final_key_state_neutral;
        self.report
    }
}

/// Replay one recorded session through the exact personal decoders and output
/// adapter after held-out validation, provenance, and clock identities bind.
/// This path does not grant or claim live wear-session authority.
pub fn run_calibrated_replay<A: OutputAdapter>(
    calibration_manifest: &AcquisitionManifest,
    runtime_manifest: &AcquisitionManifest,
    clocks: &ClockEpochSet,
    artifact: &CalibrationArtifact,
    held_out_validation: &HeldOutValidation,
    session: &FinalizedSession,
    output: A,
) -> Result<CalibratedReplayExecution<A>, CalibratedReplayError<A::Error>> {
    verify_runtime_binding(
        calibration_manifest,
        runtime_manifest,
        artifact,
        held_out_validation,
    )
    .map_err(CalibratedReplayError::Binding)?;
    let canonicalizer = FrameCanonicalizer::new(runtime_manifest)
        .map_err(RuntimeBindingError::Manifest)
        .map_err(CalibratedReplayError::Binding)?;
    let mut reader = session.reader();
    let qualified =
        clocks
            .qualify_session(runtime_manifest, session)
            .map_err(|error| match error {
                ClockEpochFitError::Epoch(error) => CalibratedReplayError::ClockEpoch(error),
                error => CalibratedReplayError::ClockQualification(error),
            })?;
    let clocks = qualified.epochs();
    let left = artifact
        .decoder(Hand::Left)
        .ok_or(CalibratedReplayError::MissingDecoder(Hand::Left))?
        .clone();
    let right = artifact
        .decoder(Hand::Right)
        .ok_or(CalibratedReplayError::MissingDecoder(Hand::Right))?
        .clone();
    let mut runtime = LiveRuntime::two_hand(left, right, output).map_err(replay_start_error)?;
    runtime
        .source_connected(Hand::Left)
        .map_err(CalibratedReplayError::Gate)?;
    runtime
        .source_connected(Hand::Right)
        .map_err(CalibratedReplayError::Gate)?;
    runtime
        .acquire_run_ownership()
        .map_err(CalibratedReplayError::Gate)?;
    let mut accumulator = ReplayAccumulator::new(runtime_manifest.nominal_period_us);
    let mut pending: Vec<PendingRecord> = Vec::new();
    let processing = (|| {
        let mut previous_receipt = None;
        let mut ordinal = 0_u64;
        let mut raw_integrity = std::array::from_fn(|_| FrameIntegrity::default());
        while let Some(record) = reader
            .next_record()
            .map_err(CalibratedReplayError::Session)?
        {
            if previous_receipt.is_some_and(|previous| record.host_time_ns < previous) {
                return Err(CalibratedReplayError::HostReceiptRegression { ordinal });
            }
            previous_receipt = Some(record.host_time_ns);
            accumulator.report.total_records += 1;
            let scheduled = schedule_record(
                record,
                ordinal,
                &canonicalizer,
                clocks,
                &mut raw_integrity,
                &mut accumulator,
            )?;
            let watermark = previous_receipt
                .expect("receipt was just set")
                .saturating_sub(MAX_MERGE_LATENESS_NS);
            let required = pending
                .iter()
                .filter(|record| record.sort_time_ns > watermark)
                .count()
                + usize::from(scheduled.sort_time_ns > watermark);
            if required > MAX_MERGE_PENDING_RECORDS {
                return Err(CalibratedReplayError::PendingCapacityExceeded {
                    maximum: MAX_MERGE_PENDING_RECORDS,
                    required,
                });
            }
            pending.push(scheduled);
            flush_ready(
                &mut pending,
                Some(watermark),
                &mut runtime,
                &mut accumulator,
            )?;
            ordinal += 1;
        }
        flush_ready(&mut pending, None, &mut runtime, &mut accumulator)
    })();
    if let Err(error) = processing {
        // The processing fault is the terminal cause. Make one all-up attempt,
        // but do not let a recovery failure replace the evidence failure.
        runtime.force_neutral_preserving_primary(FaultReason::DecoderReset);
        return Err(error);
    }
    runtime
        .revoke_run_ownership()
        .map_err(replay_lifecycle_error)?;
    let final_key_state_neutral = runtime.pressed().is_empty();
    let output = runtime.into_output();
    Ok(CalibratedReplayExecution {
        report: accumulator.finish(final_key_state_neutral),
        output,
    })
}

pub fn verify_runtime_binding(
    calibration_manifest: &AcquisitionManifest,
    runtime_manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    held_out_validation: &HeldOutValidation,
) -> Result<(), RuntimeBindingError> {
    verify_runtime_binding_hashes(
        calibration_manifest,
        runtime_manifest,
        artifact,
        held_out_validation.artifact_sha256(),
        held_out_validation.validation_report_sha256(),
    )
}

pub(crate) fn verify_runtime_binding_hashes(
    calibration_manifest: &AcquisitionManifest,
    runtime_manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    held_out_artifact_sha256: [u8; 32],
    held_out_validation_report_sha256: [u8; 32],
) -> Result<(), RuntimeBindingError> {
    calibration_manifest
        .validate()
        .map_err(RuntimeBindingError::Manifest)?;
    runtime_manifest
        .validate()
        .map_err(RuntimeBindingError::Manifest)?;
    artifact.validate().map_err(RuntimeBindingError::Artifact)?;
    if runtime_manifest.hands.len() != 2 {
        return Err(RuntimeBindingError::ExpectedTwoHands);
    }
    let calibration_hash = calibration_manifest
        .sha256()
        .map_err(RuntimeBindingError::Manifest)?;
    if parse_sha256(&artifact.acquisition_manifest_sha256) != Some(calibration_hash) {
        return Err(RuntimeBindingError::CalibrationManifestMismatch);
    }
    let artifact_hash = artifact
        .fingerprint()
        .map_err(RuntimeBindingError::Artifact)?;
    if held_out_artifact_sha256 != artifact_hash {
        return Err(RuntimeBindingError::HeldOutValidationArtifactMismatch);
    }
    if runtime_manifest
        .calibration_artifact_sha256
        .as_deref()
        .and_then(parse_sha256)
        != Some(artifact_hash)
    {
        return Err(RuntimeBindingError::ArtifactReferenceMismatch);
    }
    if runtime_manifest
        .calibration_validation_sha256
        .as_deref()
        .and_then(parse_sha256)
        != Some(held_out_validation_report_sha256)
    {
        return Err(RuntimeBindingError::HeldOutValidationReferenceMismatch);
    }
    verify_artifact_sensor_bindings(calibration_manifest, artifact)?;
    verify_acquisition_compatibility(calibration_manifest, runtime_manifest)?;
    Ok(())
}

fn verify_artifact_sensor_bindings(
    calibration: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
) -> Result<(), RuntimeBindingError> {
    let mut artifact_bindings = artifact.sensor_bindings.iter();
    for hand in &calibration.hands {
        let expected_hand = Hand::from(hand.hand);
        for sensor in &hand.sensors {
            let binding =
                artifact_bindings
                    .next()
                    .ok_or(RuntimeBindingError::SensorProvenanceMismatch {
                        hand: expected_hand,
                        sensor_index: sensor.sensor_index,
                    })?;
            if binding.hand != expected_hand || binding.sensor != *sensor {
                return Err(RuntimeBindingError::SensorProvenanceMismatch {
                    hand: expected_hand,
                    sensor_index: sensor.sensor_index,
                });
            }
        }
    }
    if let Some(extra) = artifact_bindings.next() {
        return Err(RuntimeBindingError::SensorProvenanceMismatch {
            hand: extra.hand,
            sensor_index: extra.sensor.sensor_index,
        });
    }
    Ok(())
}

fn verify_acquisition_compatibility(
    calibration: &AcquisitionManifest,
    runtime: &AcquisitionManifest,
) -> Result<(), RuntimeBindingError> {
    if calibration.hands.len() != runtime.hands.len() {
        return Err(RuntimeBindingError::AcquisitionSettingMismatch);
    }
    for (calibration_hand, runtime_hand) in calibration.hands.iter().zip(&runtime.hands) {
        if calibration_hand.hand != runtime_hand.hand {
            return Err(RuntimeBindingError::AcquisitionSettingMismatch);
        }
        for (calibration_sensor, runtime_sensor) in
            calibration_hand.sensors.iter().zip(&runtime_hand.sensors)
        {
            if calibration_sensor != runtime_sensor {
                return Err(RuntimeBindingError::SensorProvenanceMismatch {
                    hand: Hand::from(calibration_hand.hand),
                    sensor_index: calibration_sensor.sensor_index,
                });
            }
        }
    }

    // These values identify a new boot or capture record but do not change
    // frame interpretation. Normalize only this explicit allowlist so future
    // manifest fields fail closed until they are classified deliberately.
    let mut normalized_runtime = runtime.clone();
    for (calibration_hand, runtime_hand) in
        calibration.hands.iter().zip(&mut normalized_runtime.hands)
    {
        runtime_hand.boot_id.clone_from(&calibration_hand.boot_id);
    }
    normalized_runtime
        .study_id
        .clone_from(&calibration.study_id);
    normalized_runtime
        .split_id
        .clone_from(&calibration.split_id);
    normalized_runtime
        .consent_record
        .clone_from(&calibration.consent_record);
    normalized_runtime
        .retention_policy
        .clone_from(&calibration.retention_policy);
    normalized_runtime.calibration_artifact_sha256 =
        calibration.calibration_artifact_sha256.clone();
    normalized_runtime.calibration_validation_sha256 =
        calibration.calibration_validation_sha256.clone();
    if normalized_runtime != *calibration {
        return Err(RuntimeBindingError::AcquisitionSettingMismatch);
    }
    Ok(())
}

fn schedule_record<E>(
    record: RawSessionRecord,
    ordinal: u64,
    canonicalizer: &FrameCanonicalizer,
    clocks: &ClockEpochSet,
    raw_integrity: &mut [FrameIntegrity; 2],
    accumulator: &mut ReplayAccumulator,
) -> Result<PendingRecord, CalibratedReplayError<E>> {
    let receipt_time_ns = record.host_time_ns;
    if record.kind != SessionRecordKind::Decoded {
        let candidate = candidate_from_record(record, ordinal)?;
        accumulator.report.invalid_candidates += 1;
        return Ok(PendingRecord {
            sort_time_ns: receipt_time_ns,
            receipt_time_ns,
            ordinal,
            payload: PendingPayload::Candidate(candidate),
        });
    }

    let mut frame = match record.decode() {
        Ok(frame) => frame,
        Err(_) => {
            accumulator.report.invalid_candidates += 1;
            return Ok(PendingRecord {
                sort_time_ns: receipt_time_ns,
                receipt_time_ns,
                ordinal,
                payload: PendingPayload::Candidate(TransportCandidate::Decoded(record.payload)),
            });
        }
    };
    frame = canonicalizer
        .canonicalize(&frame)
        .map_err(|error| CalibratedReplayError::FrameCanonicalization { ordinal, error })?;
    raw_integrity[frame.hand as usize]
        .observe(&frame)
        .map_err(|error| match error {
            FrameIntegrityError::Quality(reason) => CalibratedReplayError::RawFrameQuality {
                ordinal,
                hand: frame.hand,
                reason,
            },
            FrameIntegrityError::Structure(error) => CalibratedReplayError::RawFrameStructure {
                ordinal,
                hand: frame.hand,
                error,
            },
        })?;
    let fit = clocks
        .fit_for(frame.hand)
        .ok_or(CalibratedReplayError::MissingClock(frame.hand))?;
    let mapped_time_ns = fit
        .map_device_time_ns(frame.boot_id, frame.device_time_us)
        .map_err(|error| CalibratedReplayError::Clock {
            hand: frame.hand,
            error,
        })?;
    let receipt_delta_ns = mapped_time_ns.abs_diff(receipt_time_ns);
    if receipt_delta_ns > MAX_MAPPED_RECEIPT_DELTA_NS {
        return Err(CalibratedReplayError::MappedReceiptDeltaTooLarge {
            ordinal,
            actual_ns: receipt_delta_ns,
        });
    }
    accumulator.receipt_delta_samples.push(receipt_delta_ns);
    accumulator.report.decoded_frames += 1;
    match frame.hand {
        Hand::Left => accumulator.report.left_frames += 1,
        Hand::Right => accumulator.report.right_frames += 1,
    }
    Ok(PendingRecord {
        sort_time_ns: mapped_time_ns,
        receipt_time_ns,
        ordinal,
        payload: PendingPayload::Frame(frame),
    })
}

fn candidate_from_record<E>(
    record: RawSessionRecord,
    ordinal: u64,
) -> Result<TransportCandidate, CalibratedReplayError<E>> {
    Ok(match record.kind {
        SessionRecordKind::Decoded => TransportCandidate::Decoded(record.payload),
        SessionRecordKind::Malformed => TransportCandidate::Malformed {
            encoded: record.payload,
        },
        SessionRecordKind::Oversized => TransportCandidate::Oversized {
            prefix: record.payload,
            actual_len: usize::try_from(record.original_len).map_err(|_| {
                CalibratedReplayError::OriginalLengthOutOfRange(record.original_len)
            })?,
        },
        SessionRecordKind::Unterminated => TransportCandidate::Unterminated {
            encoded: record.payload,
        },
        SessionRecordKind::QuiesceRequest | SessionRecordKind::QuiesceReceipt => {
            return Err(CalibratedReplayError::UnexpectedControlRecord {
                ordinal,
                kind: record.kind,
            });
        }
    })
}

fn flush_ready<A: OutputAdapter>(
    pending: &mut Vec<PendingRecord>,
    watermark: Option<u64>,
    runtime: &mut LiveRuntime<A>,
    accumulator: &mut ReplayAccumulator,
) -> Result<(), CalibratedReplayError<A::Error>> {
    loop {
        let next = pending
            .iter()
            .enumerate()
            .filter(|(_, record)| watermark.is_none_or(|limit| record.sort_time_ns <= limit))
            .min_by_key(|(_, record)| (record.sort_time_ns, record.receipt_time_ns, record.ordinal))
            .map(|(index, _)| index);
        let Some(index) = next else {
            return Ok(());
        };
        let record = pending.remove(index);
        accumulator.observe_order(record.ordinal);
        let started = Instant::now();
        let result = match record.payload {
            PendingPayload::Frame(frame) => runtime.push_frame(frame, record.sort_time_ns / 1_000),
            PendingPayload::Candidate(candidate) => runtime.push_candidate(candidate),
        };
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        accumulator.processing_samples.push(elapsed);
        if let Some(result) = result {
            match result {
                Ok(step) => accumulator.observe_step(step),
                Err(error) => accumulator.observe_fault(error)?,
            }
        }
    }
}

#[derive(Debug)]
pub enum CalibratedReplayError<E> {
    Binding(RuntimeBindingError),
    ClockEpoch(ClockEpochError),
    ClockQualification(ClockEpochFitError),
    Session(SessionError),
    RuntimeConfiguration(RuntimeConfigurationError),
    Gate(OutputGateError),
    MissingDecoder(Hand),
    MissingClock(Hand),
    UnexpectedControlRecord {
        ordinal: u64,
        kind: SessionRecordKind,
    },
    HostReceiptRegression {
        ordinal: u64,
    },
    PendingCapacityExceeded {
        maximum: usize,
        required: usize,
    },
    RawFrameQuality {
        ordinal: u64,
        hand: Hand,
        reason: AbstainReason,
    },
    RawFrameStructure {
        ordinal: u64,
        hand: Hand,
        error: WindowError,
    },
    FrameCanonicalization {
        ordinal: u64,
        error: FrameCanonicalizationError,
    },
    MappedReceiptDeltaTooLarge {
        ordinal: u64,
        actual_ns: u64,
    },
    OriginalLengthOutOfRange(u64),
    Clock {
        hand: Hand,
        error: ClockError,
    },
    Output(RuntimeOutputError<E>),
}

impl<E: fmt::Display> fmt::Display for CalibratedReplayError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding(error) => error.fmt(formatter),
            Self::ClockEpoch(error) => {
                write!(formatter, "runtime clock epochs are invalid: {error}")
            }
            Self::ClockQualification(error) => {
                write!(formatter, "runtime clock qualification failed: {error}")
            }
            Self::Session(error) => write!(formatter, "runtime session is invalid: {error}"),
            Self::RuntimeConfiguration(error) => {
                write!(formatter, "runtime decoder topology is invalid: {error:?}")
            }
            Self::Gate(error) => write!(formatter, "runtime output run gate failed: {error:?}"),
            Self::MissingDecoder(hand) => write!(formatter, "artifact has no {hand:?} decoder"),
            Self::MissingClock(hand) => write!(formatter, "clock epochs have no {hand:?} fit"),
            Self::UnexpectedControlRecord { ordinal, kind } => write!(
                formatter,
                "runtime replay record {ordinal} is a {kind:?} Gate 0A control record"
            ),
            Self::HostReceiptRegression { ordinal } => {
                write!(formatter, "host receipt time regresses at record {ordinal}")
            }
            Self::PendingCapacityExceeded { maximum, required } => write!(
                formatter,
                "runtime merge needs {required} pending records; maximum is {maximum}"
            ),
            Self::RawFrameQuality {
                ordinal,
                hand,
                reason,
            } => write!(
                formatter,
                "runtime {hand:?} record {ordinal} raw frame quality failed: {reason:?}"
            ),
            Self::RawFrameStructure {
                ordinal,
                hand,
                error,
            } => write!(
                formatter,
                "runtime {hand:?} record {ordinal} raw frame is invalid: {error}"
            ),
            Self::FrameCanonicalization { ordinal, error } => write!(
                formatter,
                "record {ordinal} cannot enter canonical sensor coordinates: {error}"
            ),
            Self::MappedReceiptDeltaTooLarge { ordinal, actual_ns } => write!(
                formatter,
                "record {ordinal} mapped/receipt delta is {actual_ns} ns; maximum is {MAX_MAPPED_RECEIPT_DELTA_NS} ns"
            ),
            Self::OriginalLengthOutOfRange(length) => {
                write!(
                    formatter,
                    "captured original length {length} does not fit usize"
                )
            }
            Self::Clock { hand, error } => {
                write!(formatter, "runtime {hand:?} clock mapping failed: {error}")
            }
            Self::Output(error) => {
                write!(formatter, "runtime output failed: {}", error.cause)?;
                if let Some(recovery_error) = &error.recovery_error {
                    write!(
                        formatter,
                        "; independent all-up recovery failed: {recovery_error}"
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl<E: Error + 'static> Error for CalibratedReplayError<E> {}

fn replay_start_error<E>(error: RuntimeStartError<E>) -> CalibratedReplayError<E> {
    match error {
        RuntimeStartError::Configuration(error) => {
            CalibratedReplayError::RuntimeConfiguration(error)
        }
        RuntimeStartError::Output(cause) => CalibratedReplayError::Output(RuntimeOutputError {
            event: None,
            cause,
            recovery_error: None,
        }),
    }
}

fn replay_lifecycle_error<E>(error: RuntimeError<E>) -> CalibratedReplayError<E> {
    match error {
        RuntimeError::Gate(error) => CalibratedReplayError::Gate(error),
        RuntimeError::Output(error) => CalibratedReplayError::Output(error),
        _ => unreachable!("run revocation cannot produce a decoder fault"),
    }
}

impl fmt::Display for RuntimeConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedHand { expected, actual } => {
                write!(formatter, "expected {expected:?} decoder, got {actual:?}")
            }
        }
    }
}
