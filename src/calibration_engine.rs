//! Persistent calibration evidence, deterministic model fitting, and held-out validation.
//!
//! The capture adapter records a strict sensor session and a plan-bound journal
//! of prompt identities and measured facts. This module derives positions,
//! roles, and blocks from the canonical plan before fitting or validation. The
//! serialized evidence object is diagnostic output, never admission authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};

use crate::{
    artifact::{
        ArtifactDomain, ArtifactError, ArtifactSensorBinding, ArtifactSource, ArtifactSourceRole,
        CalibrationArtifact, CalibrationArtifactInput, HandClockFit, KeyboardIdentity,
        US_QWERTY_LAYOUT_ID,
    },
    calibration::{
        BlockId, HeldOutValidationMetrics, KEYBOARD_EXAMPLES_PER_POSITION, KeyboardPrecheckMetrics,
        LockedArtifactIdentity, SETUP_TIME_CAP_SECONDS, SURFACE_EXAMPLES_PER_POSITION,
    },
    calibration_capture::{
        CalibrationCaptureError, CalibrationCaptureJournal, CalibrationMatchingRule,
        build_calibration_evidence_qualified, qualify_capture_clocks, verify_matching_rule_sources,
    },
    clock_epoch::{ClockEpochError, ClockEpochSet, QualifiedOfflineClocks},
    decoder::{
        CalibrationError, CalibrationExample, DecisionKind, HandDecoder, TapWindow, WindowError,
    },
    labels::{INTENT_WINDOW_END_NS, INTENT_WINDOW_START_NS},
    layout::{
        Hand, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots, keyboard_warm_start_eligible,
        position_index,
    },
    manifest::{
        AcquisitionManifest, FrameCanonicalizationError, FrameCanonicalizer, ManifestError,
        ManifestHand, ObservedTopologyError, format_sha256, parse_sha256, sha256,
    },
    neutral::{NeutralEvidenceError, NeutralEvidenceReport, NeutralRecordWindow, StabilityVerdict},
    output::{ApplyAnomaly, KeyStateMachine},
    pipeline::{CausalPipeline, DECISION_DELAY_FRAMES, PipelineError},
    prompts::PromptCorpus,
    session::{
        FinalizedSession, RecordDecodeError, SessionError, SessionRecordKind, SessionSummary,
    },
    validation::{
        HeldOutValidationError, PredictionClass, TruthClass, ValidationEvidenceError,
        ValidationMatrix, ValidationReport, ValidationReportError, ValidationReportInput,
    },
};

pub const CALIBRATION_EVIDENCE_SCHEMA: &str = "invisible-keyboard-calibration-evidence-v4";
pub const CALIBRATION_EVALUATOR_VERSION: &str = "core28-continuous-held-out-evaluator-v1";

const WINDOW_FRAMES: usize = crate::pipeline::WINDOW_FRAMES;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidencePurpose {
    Fitting,
    HeldOutValidation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceRole {
    Training,
    ThresholdSelection,
    HeldOutValidation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceDomain {
    Keyboard,
    PassiveSurface,
}

/// Provenance for a negative window with no physical-position truth.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceNullKind {
    Quiet,
    OrdinaryMotion,
}

/// One accepted tap or idle window in a strict sensor session.
///
/// `position: None` denotes an idle or ordinary-motion negative. The capture
/// adapter selects `window_end_record` near `trigger_host_time_ns`; replay
/// verifies that relationship before rebuilding the causal window.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationObservation {
    pub(crate) role: EvidenceRole,
    pub(crate) domain: EvidenceDomain,
    pub(crate) block_id: u32,
    pub(crate) position: Option<PhysicalPosition>,
    pub(crate) null_kind: Option<EvidenceNullKind>,
    pub(crate) hand: Hand,
    pub(crate) window_end_record: u64,
    pub(crate) trigger_host_time_ns: u64,
}

impl CalibrationObservation {
    #[must_use]
    pub const fn role(&self) -> EvidenceRole {
        self.role
    }

    #[must_use]
    pub const fn domain(&self) -> EvidenceDomain {
        self.domain
    }

    #[must_use]
    pub const fn block_id(&self) -> u32 {
        self.block_id
    }

    #[must_use]
    pub const fn position(&self) -> Option<PhysicalPosition> {
        self.position
    }

    #[must_use]
    pub const fn null_kind(&self) -> Option<EvidenceNullKind> {
        self.null_kind
    }

    #[must_use]
    pub const fn hand(&self) -> Hand {
        self.hand
    }

    #[must_use]
    pub const fn window_end_record(&self) -> u64 {
        self.window_end_record
    }

    #[must_use]
    pub const fn trigger_host_time_ns(&self) -> u64 {
        self.trigger_host_time_ns
    }
}

/// Privacy-minimized sidecar for one append-only calibration session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationEvidence {
    schema: String,
    purpose: EvidencePurpose,
    acquisition_manifest_sha256: String,
    clock_epochs_sha256: String,
    session_sha256: String,
    capture_journal_sha256: String,
    session_id: [u8; 16],
    prompt_corpus_sha256: String,
    calibration_matching_rule_sha256: String,
    keyboard: KeyboardIdentity,
    setup_seconds: u32,
    created_unix_ns: u64,
    rejected_examples: u32,
    ordinary_motion_exposure_ms: u64,
    key_state_neutral: bool,
    observations: Vec<CalibrationObservation>,
}

impl CalibrationEvidence {
    pub(crate) fn new(input: CalibrationEvidenceInput) -> Result<Self, CalibrationEvidenceError> {
        let evidence = Self {
            schema: CALIBRATION_EVIDENCE_SCHEMA.to_owned(),
            purpose: input.purpose,
            acquisition_manifest_sha256: format_sha256(&input.acquisition_manifest_sha256),
            clock_epochs_sha256: format_sha256(&input.clock_epochs_sha256),
            session_sha256: format_sha256(&input.session_sha256),
            capture_journal_sha256: format_sha256(&input.capture_journal_sha256),
            session_id: input.session_id,
            prompt_corpus_sha256: format_sha256(&PromptCorpus::v1().sha256()),
            calibration_matching_rule_sha256: format_sha256(
                &input.calibration_matching_rule_sha256,
            ),
            keyboard: input.keyboard,
            setup_seconds: input.setup_seconds,
            created_unix_ns: input.created_unix_ns,
            rejected_examples: input.rejected_examples,
            ordinary_motion_exposure_ms: input.ordinary_motion_exposure_ms,
            key_state_neutral: input.key_state_neutral,
            observations: input.observations,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), CalibrationEvidenceError> {
        if self.schema != CALIBRATION_EVIDENCE_SCHEMA {
            return Err(CalibrationEvidenceError::InvalidSchema);
        }
        for (value, field) in [
            (
                &self.acquisition_manifest_sha256,
                "acquisition_manifest_sha256",
            ),
            (&self.clock_epochs_sha256, "clock_epochs_sha256"),
            (&self.session_sha256, "session_sha256"),
            (&self.capture_journal_sha256, "capture_journal_sha256"),
            (&self.prompt_corpus_sha256, "prompt_corpus_sha256"),
            (
                &self.calibration_matching_rule_sha256,
                "calibration_matching_rule_sha256",
            ),
        ] {
            if parse_sha256(value).is_none() {
                return Err(CalibrationEvidenceError::InvalidSha256(field));
            }
        }
        if parse_sha256(&self.prompt_corpus_sha256) != Some(PromptCorpus::v1().sha256()) {
            return Err(CalibrationEvidenceError::PromptCorpusMismatch);
        }
        if self.session_id == [0; 16] {
            return Err(CalibrationEvidenceError::InvalidSessionIdentity);
        }
        if self.setup_seconds == 0
            || self.setup_seconds > SETUP_TIME_CAP_SECONDS
            || self.created_unix_ns == 0
        {
            return Err(CalibrationEvidenceError::InvalidLifecycle);
        }
        if self.keyboard.name.trim().is_empty()
            || self.keyboard.physical_path.trim().is_empty()
            || self.keyboard.logical_layout != US_QWERTY_LAYOUT_ID
            || self.keyboard.layout_confirmation.validate().is_err()
            || self.keyboard.layout_confirmation.layout_id != self.keyboard.logical_layout
        {
            return Err(CalibrationEvidenceError::InvalidKeyboardIdentity);
        }
        if self.observations.is_empty() {
            return Err(CalibrationEvidenceError::MissingObservations);
        }
        match self.purpose {
            EvidencePurpose::Fitting if self.ordinary_motion_exposure_ms != 0 => {
                return Err(CalibrationEvidenceError::UnexpectedOrdinaryMotionExposure);
            }
            _ => {}
        }

        let mut record_ends = BTreeSet::new();
        let mut blocks = BTreeMap::new();
        let mut roles = BTreeSet::new();
        let mut ordinary_motion_hands = [false; 2];
        for observation in &self.observations {
            let role_allowed = match self.purpose {
                EvidencePurpose::Fitting => matches!(
                    observation.role,
                    EvidenceRole::Training | EvidenceRole::ThresholdSelection
                ),
                EvidencePurpose::HeldOutValidation => {
                    observation.role == EvidenceRole::HeldOutValidation
                }
            };
            if !role_allowed {
                return Err(CalibrationEvidenceError::RolePurposeMismatch);
            }
            let expected_domain = match observation.role {
                EvidenceRole::ThresholdSelection => Some(EvidenceDomain::PassiveSurface),
                EvidenceRole::HeldOutValidation => Some(EvidenceDomain::PassiveSurface),
                EvidenceRole::Training => None,
            };
            if expected_domain.is_some_and(|expected| observation.domain != expected) {
                return Err(CalibrationEvidenceError::RoleDomainMismatch {
                    role: observation.role,
                    expected: expected_domain.expect("checked expected domain exists"),
                    actual: observation.domain,
                });
            }
            match (observation.position, observation.null_kind) {
                (Some(_), None) => {}
                (None, Some(_)) => {}
                _ => return Err(CalibrationEvidenceError::InvalidNullProvenance),
            }
            if observation.position.is_none() {
                match observation.role {
                    EvidenceRole::Training => {
                        return Err(CalibrationEvidenceError::TrainingNeedsPosition);
                    }
                    EvidenceRole::ThresholdSelection => {
                        if observation.null_kind != Some(EvidenceNullKind::Quiet) {
                            return Err(CalibrationEvidenceError::InvalidNullProvenance);
                        }
                    }
                    EvidenceRole::HeldOutValidation => {
                        if observation.null_kind == Some(EvidenceNullKind::OrdinaryMotion) {
                            ordinary_motion_hands[match observation.hand {
                                Hand::Left => 0,
                                Hand::Right => 1,
                            }] = true;
                        }
                    }
                }
            }
            if observation.block_id == 0 {
                return Err(CalibrationEvidenceError::InvalidBlockId);
            }
            if let Some(position) = observation.position
                && (position.hand != observation.hand || position_index(position).is_none())
            {
                return Err(CalibrationEvidenceError::InvalidPosition(position));
            }
            if !record_ends.insert(observation.window_end_record) {
                return Err(CalibrationEvidenceError::DuplicateWindowEnd(
                    observation.window_end_record,
                ));
            }
            let identity = (observation.role, observation.domain);
            if blocks
                .insert(observation.block_id, identity)
                .is_some_and(|existing| existing != identity)
            {
                return Err(CalibrationEvidenceError::BlockRoleMismatch(
                    observation.block_id,
                ));
            }
            roles.insert(observation.role);
        }
        match self.purpose {
            EvidencePurpose::Fitting if !roles.contains(&EvidenceRole::Training) => {
                Err(CalibrationEvidenceError::MissingFittingRole)
            }
            EvidencePurpose::HeldOutValidation if blocks.len() != 1 => {
                Err(CalibrationEvidenceError::ValidationNeedsOneBlock)
            }
            EvidencePurpose::HeldOutValidation
                if ordinary_motion_hands.iter().any(|observed| !observed) =>
            {
                Err(CalibrationEvidenceError::MissingOrdinaryMotionObservations)
            }
            _ => Ok(()),
        }
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, CalibrationEvidenceError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|error| CalibrationEvidenceError::Serialization(error.to_string()))
    }

    pub fn fingerprint(&self) -> Result<[u8; 32], CalibrationEvidenceError> {
        Ok(sha256(&self.canonical_json()?))
    }

    #[must_use]
    pub const fn purpose(&self) -> EvidencePurpose {
        self.purpose
    }

    #[must_use]
    pub fn capture_journal_sha256(&self) -> &str {
        &self.capture_journal_sha256
    }

    #[must_use]
    pub fn calibration_matching_rule_sha256(&self) -> &str {
        &self.calibration_matching_rule_sha256
    }

    #[must_use]
    pub const fn session_id(&self) -> [u8; 16] {
        self.session_id
    }

    #[must_use]
    pub const fn keyboard(&self) -> &KeyboardIdentity {
        &self.keyboard
    }

    #[must_use]
    pub const fn setup_seconds(&self) -> u32 {
        self.setup_seconds
    }

    #[must_use]
    pub const fn created_unix_ns(&self) -> u64 {
        self.created_unix_ns
    }

    #[must_use]
    pub const fn rejected_examples(&self) -> u32 {
        self.rejected_examples
    }

    #[must_use]
    pub const fn ordinary_motion_exposure_ms(&self) -> u64 {
        self.ordinary_motion_exposure_ms
    }

    #[must_use]
    pub const fn key_state_neutral(&self) -> bool {
        self.key_state_neutral
    }

    #[must_use]
    pub fn observations(&self) -> &[CalibrationObservation] {
        &self.observations
    }

    #[must_use]
    pub fn block_ids(&self) -> Vec<u32> {
        self.observations
            .iter()
            .map(|observation| observation.block_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

pub(crate) struct CalibrationEvidenceInput {
    pub(crate) purpose: EvidencePurpose,
    pub(crate) acquisition_manifest_sha256: [u8; 32],
    pub(crate) clock_epochs_sha256: [u8; 32],
    pub(crate) session_sha256: [u8; 32],
    pub(crate) capture_journal_sha256: [u8; 32],
    pub(crate) session_id: [u8; 16],
    pub(crate) calibration_matching_rule_sha256: [u8; 32],
    pub(crate) keyboard: KeyboardIdentity,
    pub(crate) setup_seconds: u32,
    pub(crate) created_unix_ns: u64,
    pub(crate) rejected_examples: u32,
    pub(crate) ordinary_motion_exposure_ms: u64,
    pub(crate) key_state_neutral: bool,
    pub(crate) observations: Vec<CalibrationObservation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CalibrationEvidenceError {
    Serialization(String),
    InvalidSchema,
    InvalidSha256(&'static str),
    PromptCorpusMismatch,
    InvalidSessionIdentity,
    InvalidLifecycle,
    InvalidKeyboardIdentity,
    MissingObservations,
    InvalidNullProvenance,
    UnexpectedOrdinaryMotionExposure,
    RolePurposeMismatch,
    RoleDomainMismatch {
        role: EvidenceRole,
        expected: EvidenceDomain,
        actual: EvidenceDomain,
    },
    TrainingNeedsPosition,
    MissingOrdinaryMotionObservations,
    InvalidBlockId,
    InvalidPosition(PhysicalPosition),
    DuplicateWindowEnd(u64),
    BlockRoleMismatch(u32),
    MissingFittingRole,
    ValidationNeedsOneBlock,
}

impl fmt::Display for CalibrationEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(error) => {
                write!(
                    formatter,
                    "calibration evidence serialization failed: {error}"
                )
            }
            Self::InvalidSchema => formatter.write_str("calibration evidence schema is invalid"),
            Self::InvalidSha256(field) => {
                write!(formatter, "calibration field {field} is not SHA-256")
            }
            Self::PromptCorpusMismatch => {
                formatter.write_str("calibration prompt corpus does not match version 1")
            }
            Self::InvalidSessionIdentity => formatter.write_str("calibration session ID is zero"),
            Self::InvalidLifecycle => {
                formatter.write_str("calibration lifecycle metadata is invalid")
            }
            Self::InvalidKeyboardIdentity => {
                formatter.write_str("calibration keyboard identity is invalid")
            }
            Self::MissingObservations => {
                formatter.write_str("calibration evidence has no observations")
            }
            Self::InvalidNullProvenance => formatter
                .write_str("calibration null windows must distinguish quiet from ordinary motion"),
            Self::UnexpectedOrdinaryMotionExposure => formatter
                .write_str("fitting evidence cannot claim ordinary-motion validation exposure"),
            Self::RolePurposeMismatch => formatter
                .write_str("calibration observation role does not match the bundle purpose"),
            Self::RoleDomainMismatch {
                role,
                expected,
                actual,
            } => write!(
                formatter,
                "calibration {role:?} observation uses {actual:?}; expected {expected:?}"
            ),
            Self::TrainingNeedsPosition => {
                formatter.write_str("calibration training observations need a physical position")
            }
            Self::MissingOrdinaryMotionObservations => formatter
                .write_str("held-out validation needs explicit ordinary-motion observations"),
            Self::InvalidBlockId => formatter.write_str("calibration block ID must be nonzero"),
            Self::InvalidPosition(position) => {
                write!(formatter, "calibration position {position:?} is invalid")
            }
            Self::DuplicateWindowEnd(record) => write!(
                formatter,
                "calibration reuses session record {record} as a window end"
            ),
            Self::BlockRoleMismatch(block) => write!(
                formatter,
                "calibration block {block} crosses a role or domain boundary"
            ),
            Self::MissingFittingRole => formatter
                .write_str("fitting evidence needs training and threshold-selection blocks"),
            Self::ValidationNeedsOneBlock => {
                formatter.write_str("held-out validation must use exactly one block")
            }
        }
    }
}

impl Error for CalibrationEvidenceError {}

#[derive(Clone, Copy, Debug)]
struct CapturedFrame {
    record_ordinal: u64,
    hand_ordinal: u64,
    host_time_ns: u64,
    frame: crate::protocol::SensorFrame,
}

struct BoundEvidence<'a> {
    session_sha256: [u8; 32],
    frames: Vec<CapturedFrame>,
    observations: Vec<(&'a CalibrationObservation, CalibrationExample)>,
}

#[derive(Clone, Copy, Debug)]
struct CausalDecision {
    hand: Hand,
    hand_ordinal: u64,
    prediction: PredictionClass,
    matched: bool,
}

struct ContinuousValidation {
    confusion: ValidationMatrix,
    causal_predictions: Vec<PredictionClass>,
    ordinary_motion_exposure_ms: u64,
    key_state_neutral: bool,
}

pub struct CalibrationFitResult {
    pub artifact: CalibrationArtifact,
    pub accepted_windows: usize,
    /// Canonical derived sidecar retained for diagnostics and audit output.
    pub diagnostic_evidence: CalibrationEvidence,
}

/// Opaque proof that one artifact passed finite, held-out validation.
///
/// This value authorizes deterministic replay and model use. It does not prove
/// that either wearable is still in the validated position and grants no live
/// output authority. Its private constructor lives beside the sole raw-evidence
/// evaluator, so a parsed or caller-created report is never admission authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeldOutValidation {
    artifact_sha256: [u8; 32],
    validation_report_sha256: [u8; 32],
    metrics: HeldOutValidationMetrics,
}

impl HeldOutValidation {
    fn from_engine_evaluation(
        identity: &LockedArtifactIdentity,
        report: &ValidationReport,
        metrics: HeldOutValidationMetrics,
    ) -> Result<Self, HeldOutValidationError> {
        if !report.passes_held_out_validation(metrics) {
            return Err(HeldOutValidationError::ThresholdsFailed {
                metrics,
                reserved_activations: report.reserved_activations(),
                ordinary_motion_activations: report.ordinary_motion_activations(),
                ordinary_motion_exposure_ms: report.ordinary_motion_exposure_ms,
                key_state_neutral: report.key_state_neutral,
            });
        }
        Ok(Self {
            artifact_sha256: *identity.fingerprint(),
            validation_report_sha256: report
                .fingerprint()
                .map_err(HeldOutValidationError::Report)?,
            metrics,
        })
    }

    #[must_use]
    pub const fn artifact_sha256(self) -> [u8; 32] {
        self.artifact_sha256
    }

    #[must_use]
    pub const fn validation_report_sha256(self) -> [u8; 32] {
        self.validation_report_sha256
    }

    #[must_use]
    pub const fn metrics(self) -> HeldOutValidationMetrics {
        self.metrics
    }

    /// Create one live-only capability after the application has obtained a
    /// fresh, explicit user statement that neither wearable moved.
    ///
    /// The returned capability cannot be serialized, cloned, or copied. A
    /// live runtime consumes it, so saved validation files cannot be passed to
    /// the live constructor directly.
    #[must_use]
    pub const fn user_attests_both_wearables_have_not_moved(&self) -> WearSessionConfirmation {
        WearSessionConfirmation {
            artifact_sha256: self.artifact_sha256,
            validation_report_sha256: self.validation_report_sha256,
        }
    }
}

/// Single-use live authority based only on a fresh user attestation.
///
/// This capability says `user-attested-unmoved`, never sensor-verified. Its
/// private hash binding prevents using it with another artifact or report.
#[derive(Debug, Eq, PartialEq)]
pub struct WearSessionConfirmation {
    artifact_sha256: [u8; 32],
    validation_report_sha256: [u8; 32],
}

impl WearSessionConfirmation {
    #[must_use]
    pub(crate) const fn artifact_sha256(&self) -> [u8; 32] {
        self.artifact_sha256
    }

    #[must_use]
    pub(crate) const fn validation_report_sha256(&self) -> [u8; 32] {
        self.validation_report_sha256
    }
}

/// Immutable source objects required to fit one locked artifact.
pub struct CalibrationFitInput<'a> {
    pub manifest: &'a AcquisitionManifest,
    pub clocks: &'a ClockEpochSet,
    pub journal: &'a CalibrationCaptureJournal,
    pub session: &'a FinalizedSession,
    pub neutral_report: &'a NeutralEvidenceReport,
}

pub struct CalibrationValidationResult {
    pub report: ValidationReport,
    pub metrics: HeldOutValidationMetrics,
    pub evaluated_windows: usize,
    /// Predictions emitted by the causal stream decoder before label matching.
    pub causal_predictions: Vec<PredictionClass>,
    /// Canonical derived sidecar retained for diagnostics and audit output.
    pub diagnostic_evidence: CalibrationEvidence,
    admission: Result<HeldOutValidation, HeldOutValidationError>,
}

impl CalibrationValidationResult {
    /// Report whether this evaluation minted model-use authority.
    #[must_use]
    pub fn heldout_validation_passed(&self) -> bool {
        self.admission.is_ok()
    }

    /// Return model-use authority only when this exact raw held-out evaluation
    /// passed every frozen threshold. Failed evaluation remains inspectable
    /// through the public report and metrics while retaining its typed cause.
    pub fn held_out_validation(&self) -> Result<HeldOutValidation, HeldOutValidationError> {
        self.admission.clone()
    }
}

/// Evidence-backed keyboard signal gate run before passive-surface fitting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardPrecheckResult {
    pub metrics: KeyboardPrecheckMetrics,
    pub evaluated_windows: usize,
    pub weak_positions: Vec<PhysicalPosition>,
}

/// Verify the keyboard-labelled signal block before asking for surface taps.
///
/// This verifies only warm-start acting-finger ownership. Runtime intent is
/// selected later from a disjoint passive-surface tap/quiet block.
pub fn evaluate_keyboard_precheck(
    manifest: &AcquisitionManifest,
    clocks: &ClockEpochSet,
    journal: &CalibrationCaptureJournal,
    session: &FinalizedSession,
) -> Result<KeyboardPrecheckResult, CalibrationEngineError> {
    let qualified = qualify_capture_clocks(manifest, clocks, session)
        .map_err(CalibrationEngineError::Capture)?;
    let (evidence, _) = build_calibration_evidence_qualified(&qualified, journal)
        .map_err(CalibrationEngineError::Capture)?;
    evaluate_keyboard_precheck_from_evidence(&qualified, &evidence)
}

fn evaluate_keyboard_precheck_from_evidence(
    qualified: &QualifiedOfflineClocks<'_>,
    evidence: &CalibrationEvidence,
) -> Result<KeyboardPrecheckResult, CalibrationEngineError> {
    if evidence.purpose != EvidencePurpose::Fitting {
        return Err(CalibrationEngineError::WrongPurpose);
    }
    let bound = bind_finalized_evidence(qualified, evidence)?;
    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut evaluated_windows = 0;
    for (observation, example) in bound.observations {
        if observation.role != EvidenceRole::Training
            || observation.domain != EvidenceDomain::Keyboard
        {
            continue;
        }
        match observation.hand {
            Hand::Left => left.push(example),
            Hand::Right => right.push(example),
        }
        evaluated_windows += 1;
    }
    for (hand, examples) in [
        (Hand::Left, left.as_slice()),
        (Hand::Right, right.as_slice()),
    ] {
        HandDecoder::validate_acting_fingers(hand, examples)
            .map_err(|error| CalibrationEngineError::Decoder { hand, error })?;
    }
    Ok(KeyboardPrecheckResult {
        metrics: KeyboardPrecheckMetrics::new(10_000, 10_000)
            .expect("complete keyboard precheck is exactly 100 percent"),
        evaluated_windows,
        weak_positions: Vec::new(),
    })
}

/// Fit a locked two-hand artifact from one bound fitting session.
pub fn fit_calibration(
    input: CalibrationFitInput<'_>,
) -> Result<CalibrationFitResult, CalibrationEngineError> {
    let CalibrationFitInput {
        manifest,
        clocks,
        journal,
        session,
        neutral_report,
    } = input;
    if journal.purpose != EvidencePurpose::Fitting {
        return Err(CalibrationEngineError::WrongPurpose);
    }
    journal
        .validate_complete_surface_plan()
        .map_err(CalibrationEngineError::Capture)?;
    let qualified = qualify_capture_clocks(manifest, clocks, session)
        .map_err(CalibrationEngineError::Capture)?;
    let (evidence, capture_report) = build_calibration_evidence_qualified(&qualified, journal)
        .map_err(CalibrationEngineError::Capture)?;
    fit_calibration_from_evidence(
        &qualified,
        evidence,
        &capture_report.matching_rule,
        neutral_report,
    )
}

fn fit_calibration_from_evidence(
    qualified: &QualifiedOfflineClocks<'_>,
    evidence: CalibrationEvidence,
    matching_rule: &CalibrationMatchingRule,
    neutral_report: &NeutralEvidenceReport,
) -> Result<CalibrationFitResult, CalibrationEngineError> {
    let manifest = qualified.manifest();
    let clocks = qualified.epochs();
    let session = qualified.session();
    if evidence.purpose != EvidencePurpose::Fitting {
        return Err(CalibrationEngineError::WrongPurpose);
    }
    let bound = bind_finalized_evidence(qualified, &evidence)?;
    matching_rule
        .validate()
        .map_err(CalibrationEngineError::MatchingRule)?;
    let matching_rule_sha256 = matching_rule
        .fingerprint()
        .map_err(CalibrationEngineError::MatchingRule)?;
    let manifest_sha256 = manifest
        .sha256()
        .map_err(CalibrationEngineError::Manifest)?;
    if parse_sha256(&evidence.calibration_matching_rule_sha256) != Some(matching_rule_sha256)
        || parse_sha256(&matching_rule.acquisition_manifest_sha256) != Some(manifest_sha256)
    {
        return Err(CalibrationEngineError::MatchingRuleIdentityMismatch);
    }
    if neutral_report.verdict != StabilityVerdict::Stable {
        return Err(CalibrationEngineError::NeutralNotStable);
    }
    let neutral_windows = neutral_report
        .windows
        .iter()
        .map(|window| NeutralRecordWindow {
            block_id: window.block_id.clone(),
            window_id: window.window_id.clone(),
            role: window.role,
            start_record: window.start_record,
            end_record_exclusive: window.end_record_exclusive,
        })
        .collect::<Vec<_>>();
    neutral_report
        .verify_qualified_sources(qualified, &neutral_windows)
        .map_err(CalibrationEngineError::NeutralEvidence)?;
    verify_matching_rule_sources(matching_rule, manifest, session, &neutral_windows)
        .map_err(CalibrationEngineError::MatchingRule)?;
    let neutral_report_sha256 = neutral_report
        .fingerprint()
        .map_err(CalibrationEngineError::NeutralEvidence)?;
    let mut left_training = Vec::new();
    let mut right_training = Vec::new();
    let mut left_threshold = Vec::new();
    let mut right_threshold = Vec::new();
    let mut accepted_position_counts = vec![0_u16; PHYSICAL_POSITION_COUNT];
    let mut keyboard_counts = [0_u16; PHYSICAL_POSITION_COUNT];
    let mut surface_counts = [0_u16; PHYSICAL_POSITION_COUNT];
    let mut keyboard_blocks: [BTreeSet<u32>; PHYSICAL_POSITION_COUNT] =
        std::array::from_fn(|_| BTreeSet::new());

    for (observation, example) in &bound.observations {
        let target = match (observation.role, observation.domain, observation.hand) {
            (EvidenceRole::Training, EvidenceDomain::PassiveSurface, Hand::Left) => {
                Some(&mut left_training)
            }
            (EvidenceRole::Training, EvidenceDomain::PassiveSurface, Hand::Right) => {
                Some(&mut right_training)
            }
            (EvidenceRole::ThresholdSelection, EvidenceDomain::PassiveSurface, Hand::Left) => {
                Some(&mut left_threshold)
            }
            (EvidenceRole::ThresholdSelection, EvidenceDomain::PassiveSurface, Hand::Right) => {
                Some(&mut right_threshold)
            }
            (EvidenceRole::Training, EvidenceDomain::Keyboard, _) => None,
            (EvidenceRole::ThresholdSelection, EvidenceDomain::Keyboard, _) => {
                return Err(CalibrationEngineError::ThresholdDomainMismatch);
            }
            (EvidenceRole::HeldOutValidation, _, _) => {
                return Err(CalibrationEngineError::WrongPurpose);
            }
        };
        if let Some(target) = target {
            target.push(example.clone());
        }
        if observation.role == EvidenceRole::Training
            && let Some(position) = observation.position
        {
            let index =
                position_index(position).expect("validated evidence position belongs to layout");
            match observation.domain {
                EvidenceDomain::Keyboard => {
                    keyboard_counts[index] = keyboard_counts[index].saturating_add(1);
                    keyboard_blocks[index].insert(observation.block_id);
                }
                EvidenceDomain::PassiveSurface => {
                    surface_counts[index] = surface_counts[index].saturating_add(1);
                    accepted_position_counts[index] =
                        accepted_position_counts[index].saturating_add(1);
                }
            }
        }
    }

    for (index, slot) in all_slots().iter().enumerate() {
        if keyboard_warm_start_eligible(slot.position)
            && (keyboard_counts[index] < u16::from(KEYBOARD_EXAMPLES_PER_POSITION)
                || keyboard_blocks[index].len() < 2)
        {
            return Err(CalibrationEngineError::CoverageIncomplete {
                domain: EvidenceDomain::Keyboard,
                position: slot.position,
                actual: keyboard_counts[index],
            });
        }
        if surface_counts[index] < u16::from(SURFACE_EXAMPLES_PER_POSITION) {
            return Err(CalibrationEngineError::CoverageIncomplete {
                domain: EvidenceDomain::PassiveSurface,
                position: slot.position,
                actual: surface_counts[index],
            });
        }
    }

    let left_decoder = HandDecoder::calibrate_split(Hand::Left, &left_training, &left_threshold)
        .map_err(|error| CalibrationEngineError::Decoder {
            hand: Hand::Left,
            error,
        })?;
    let right_decoder =
        HandDecoder::calibrate_split(Hand::Right, &right_training, &right_threshold).map_err(
            |error| CalibrationEngineError::Decoder {
                hand: Hand::Right,
                error,
            },
        )?;

    let sources = artifact_sources(&evidence, bound.session_sha256);
    let artifact = CalibrationArtifact::new(CalibrationArtifactInput {
        acquisition_manifest_sha256: manifest_sha256,
        prompt_corpus_sha256: PromptCorpus::v1().sha256(),
        neutral_evidence_sha256: neutral_report_sha256,
        calibration_matching_rule_sha256: matching_rule_sha256,
        fitting_capture_journal_sha256: parse_sha256(&evidence.capture_journal_sha256)
            .expect("validated derived evidence has a capture-journal hash"),
        fitting_evidence_sha256: evidence
            .fingerprint()
            .map_err(CalibrationEngineError::Evidence)?,
        sensor_bindings: artifact_sensor_bindings(manifest),
        keyboard: evidence.keyboard.clone(),
        sources,
        clock_fits: clocks
            .epochs
            .iter()
            .map(|epoch| HandClockFit {
                hand: epoch.hand,
                fit: epoch.fit,
            })
            .collect(),
        left_decoder: Some(left_decoder),
        right_decoder: Some(right_decoder),
        accepted_position_counts,
        rejected_examples: evidence.rejected_examples,
        setup_seconds: evidence.setup_seconds,
        created_unix_ns: evidence.created_unix_ns,
        requires_wear_session_confirmation: true,
    })
    .map_err(CalibrationEngineError::Artifact)?;

    let accepted_windows = bound.observations.len();
    drop(bound);
    Ok(CalibrationFitResult {
        artifact,
        accepted_windows,
        diagnostic_evidence: evidence,
    })
}

/// Evaluate and, only on success, admit a locked artifact from raw held-out data.
///
/// This is the sole public admission service. It binds the immutable manifest,
/// clock epochs, artifact, canonical plan-bound journal, and finalized sensor
/// session, then exposes an inspectable report and an opaque model-use capability.
pub fn validate_calibration(
    manifest: &AcquisitionManifest,
    clocks: &ClockEpochSet,
    artifact: &CalibrationArtifact,
    journal: &CalibrationCaptureJournal,
    session: &FinalizedSession,
) -> Result<CalibrationValidationResult, CalibrationEngineError> {
    if journal.purpose != EvidencePurpose::HeldOutValidation {
        return Err(CalibrationEngineError::WrongPurpose);
    }
    journal
        .validate_complete_surface_plan()
        .map_err(CalibrationEngineError::Capture)?;
    journal
        .surface_plan()
        .ok_or(CalibrationEngineError::Capture(
            CalibrationCaptureError::MissingSurfacePlan,
        ))?
        .validate_locked_artifact(
            artifact
                .fingerprint()
                .map_err(CalibrationEngineError::Artifact)?,
        )
        .map_err(CalibrationEngineError::Capture)?;
    let validation_session_sha256 = session.sha256();
    if artifact
        .sources
        .iter()
        .any(|source| parse_sha256(&source.session_sha256) == Some(validation_session_sha256))
    {
        return Err(CalibrationEngineError::ValidationEvidence(
            ValidationEvidenceError::TrainingSessionOverlap,
        ));
    }
    let qualified = qualify_capture_clocks(manifest, clocks, session)
        .map_err(CalibrationEngineError::Capture)?;
    let (evidence, _) = build_calibration_evidence_qualified(&qualified, journal)
        .map_err(CalibrationEngineError::Capture)?;
    validate_calibration_from_evidence(&qualified, artifact, evidence)
}

fn validate_calibration_from_evidence(
    qualified: &QualifiedOfflineClocks<'_>,
    artifact: &CalibrationArtifact,
    evidence: CalibrationEvidence,
) -> Result<CalibrationValidationResult, CalibrationEngineError> {
    let manifest = qualified.manifest();
    let clocks = qualified.epochs();
    if evidence.purpose != EvidencePurpose::HeldOutValidation {
        return Err(CalibrationEngineError::WrongPurpose);
    }
    let bound = bind_finalized_evidence(qualified, &evidence)?;
    if parse_sha256(&artifact.acquisition_manifest_sha256)
        != Some(
            manifest
                .sha256()
                .map_err(CalibrationEngineError::Manifest)?,
        )
    {
        return Err(CalibrationEngineError::ArtifactManifestMismatch);
    }
    if artifact.keyboard != evidence.keyboard {
        return Err(CalibrationEngineError::KeyboardIdentityMismatch);
    }
    if artifact.calibration_matching_rule_sha256 != evidence.calibration_matching_rule_sha256 {
        return Err(CalibrationEngineError::MatchingRuleIdentityMismatch);
    }

    let continuous = evaluate_continuous_validation(clocks, artifact, &bound)?;

    let artifact_identity = artifact
        .locked_identity()
        .map_err(CalibrationEngineError::Artifact)?;
    let artifact_sha256 = *artifact_identity.fingerprint();
    let block_ids = evidence.block_ids();
    let validation_block = BlockId::new(block_ids[0]);
    let training_block_ids = artifact
        .sources
        .iter()
        .flat_map(|source| source.block_ids.iter().copied())
        .collect::<Vec<_>>();
    let report = ValidationReport::new(ValidationReportInput {
        artifact_sha256,
        acquisition_manifest_sha256: manifest
            .sha256()
            .map_err(CalibrationEngineError::Manifest)?,
        evaluator_version: CALIBRATION_EVALUATOR_VERSION.to_owned(),
        session_sha256: vec![bound.session_sha256],
        evidence_sha256: vec![
            evidence
                .fingerprint()
                .map_err(CalibrationEngineError::Evidence)?,
        ],
        block_ids,
        confusion: continuous.confusion,
        ordinary_motion_exposure_ms: continuous.ordinary_motion_exposure_ms,
        key_state_neutral: continuous.key_state_neutral,
    })
    .map_err(CalibrationEngineError::ValidationReport)?;
    let metrics = report
        .validate_against(&artifact_identity, validation_block, &training_block_ids)
        .map_err(CalibrationEngineError::ValidationEvidence)?;
    let admission = HeldOutValidation::from_engine_evaluation(&artifact_identity, &report, metrics);

    let evaluated_windows = bound.observations.len();
    drop(bound);
    Ok(CalibrationValidationResult {
        report,
        metrics,
        evaluated_windows,
        causal_predictions: continuous.causal_predictions,
        diagnostic_evidence: evidence,
        admission,
    })
}

/// Decode the complete held-out stream before consulting labels.
///
/// Labels only match already-emitted causal decisions to intent windows. Each
/// unmatched key decision is therefore observable in the null truth row as a
/// spontaneous activation instead of disappearing between selected windows.
fn evaluate_continuous_validation(
    clocks: &ClockEpochSet,
    artifact: &CalibrationArtifact,
    bound: &BoundEvidence<'_>,
) -> Result<ContinuousValidation, CalibrationEngineError> {
    let left_decoder = artifact
        .decoder(Hand::Left)
        .ok_or(CalibrationEngineError::MissingDecoder(Hand::Left))?
        .clone();
    let right_decoder = artifact
        .decoder(Hand::Right)
        .ok_or(CalibrationEngineError::MissingDecoder(Hand::Right))?
        .clone();
    let mut left_pipeline = CausalPipeline::new(left_decoder);
    let mut right_pipeline = CausalPipeline::new(right_decoder);
    let mut key_state = KeyStateMachine::new();
    let mut scheduled = Vec::with_capacity(bound.frames.len());

    for captured in &bound.frames {
        let fit = clocks
            .fit_for(captured.frame.hand)
            .expect("bound clock epochs cover every observed hand");
        let mapped_time_ns = fit
            .map_device_time_ns(captured.frame.boot_id, captured.frame.device_time_us)
            .map_err(|error| {
                CalibrationEngineError::Clock(ClockEpochError::Clock {
                    hand: captured.frame.hand,
                    error,
                })
            })?;
        scheduled.push((mapped_time_ns, *captured));
    }
    scheduled.sort_by_key(|(mapped_time_ns, captured)| (*mapped_time_ns, captured.record_ordinal));

    let mut decisions = Vec::new();
    for (mapped_time_ns, captured) in scheduled {
        let frame = captured.frame;
        let emitted = match frame.hand {
            Hand::Left => left_pipeline.push(frame),
            Hand::Right => right_pipeline.push(frame),
        }
        .map_err(CalibrationEngineError::Pipeline)?;
        let Some(decision) = emitted else {
            continue;
        };
        let prediction = match decision.kind {
            DecisionKind::Key(usage) => {
                let applied = key_state.tap(usage, mapped_time_ns / 1_000);
                if let Some(anomaly) = applied.anomaly {
                    return Err(CalibrationEngineError::KeyState(anomaly));
                }
                PredictionClass::Key(usage)
            }
            DecisionKind::Abstain(_) => PredictionClass::Null,
        };
        decisions.push(CausalDecision {
            hand: captured.frame.hand,
            hand_ordinal: captured.hand_ordinal,
            prediction,
            matched: false,
        });
    }

    let causal_predictions = decisions
        .iter()
        .map(|decision| decision.prediction)
        .collect::<Vec<_>>();
    let mut confusion = ValidationMatrix::new();
    let mut intent_observations = bound
        .observations
        .iter()
        .filter(|(observation, _)| observation.position.is_some())
        .collect::<Vec<_>>();
    intent_observations.sort_by_key(|(observation, _)| {
        (
            observation.trigger_host_time_ns,
            observation.window_end_record,
        )
    });
    for (observation, _) in intent_observations {
        let end = frame_by_record(&bound.frames, observation.window_end_record)
            .expect("bound observation window end exists");
        let delay = u64::from(DECISION_DELAY_FRAMES);
        let matched = decisions
            .iter_mut()
            .filter(|decision| {
                !decision.matched
                    && decision.hand == observation.hand
                    && decision.hand_ordinal.abs_diff(end.hand_ordinal) <= delay
            })
            .min_by_key(|decision| decision.hand_ordinal.abs_diff(end.hand_ordinal));
        let prediction = matched.map_or(PredictionClass::Null, |decision| {
            decision.matched = true;
            decision.prediction
        });
        confusion.observe(
            TruthClass::Position(
                observation
                    .position
                    .expect("intent observations have physical positions"),
            ),
            prediction,
        );
    }
    for (observation, _) in bound
        .observations
        .iter()
        .filter(|(observation, _)| observation.position.is_none())
    {
        let _ = observation;
        confusion.observe(TruthClass::Null, PredictionClass::Null);
    }
    for decision in decisions.iter().filter(|decision| !decision.matched) {
        if let PredictionClass::Key(usage) = decision.prediction {
            confusion.observe(TruthClass::Null, PredictionClass::Key(usage));
        }
    }

    Ok(ContinuousValidation {
        confusion,
        causal_predictions,
        ordinary_motion_exposure_ms: recorded_exposure_ms(bound)?,
        key_state_neutral: key_state.pressed().is_empty(),
    })
}

fn recorded_exposure_ms(bound: &BoundEvidence<'_>) -> Result<u64, CalibrationEngineError> {
    let mut intervals: [Vec<(u64, u64)>; 2] = std::array::from_fn(|_| Vec::new());
    for (observation, _) in bound.observations.iter().filter(|(observation, _)| {
        observation.position.is_none()
            && observation.null_kind == Some(EvidenceNullKind::OrdinaryMotion)
    }) {
        let end_index = frame_index_by_record(&bound.frames, observation.window_end_record).ok_or(
            CalibrationEngineError::MissingWindowEnd(observation.window_end_record),
        )?;
        let mut times = preceding_hand_frames(&bound.frames, end_index, observation.hand)
            .map(|frame| frame.host_time_ns)
            .collect::<Vec<_>>();
        if times.len() != WINDOW_FRAMES {
            return Err(CalibrationEngineError::StreamIntegrity);
        }
        times.reverse();
        let hand_index = match observation.hand {
            Hand::Left => 0,
            Hand::Right => 1,
        };
        intervals[hand_index].push((times[0], times[WINDOW_FRAMES - 1]));
    }

    intervals
        .iter_mut()
        .map(|hand_intervals| merged_duration_ns(hand_intervals) / 1_000_000)
        .min()
        .ok_or(CalibrationEngineError::StreamIntegrity)
}

fn merged_duration_ns(intervals: &mut [(u64, u64)]) -> u64 {
    intervals.sort_unstable_by_key(|interval| interval.0);
    let mut total = 0_u64;
    let mut current = None::<(u64, u64)>;
    for &(start, end) in intervals.iter() {
        current = match current {
            None => Some((start, end)),
            Some((current_start, current_end)) if start <= current_end => {
                Some((current_start, current_end.max(end)))
            }
            Some((current_start, current_end)) => {
                total = total.saturating_add(current_end.saturating_sub(current_start));
                Some((start, end))
            }
        };
    }
    if let Some((start, end)) = current {
        total = total.saturating_add(end.saturating_sub(start));
    }
    total
}

fn bind_finalized_evidence<'a>(
    qualified: &QualifiedOfflineClocks<'_>,
    evidence: &'a CalibrationEvidence,
) -> Result<BoundEvidence<'a>, CalibrationEngineError> {
    let manifest = qualified.manifest();
    let clocks = qualified.epochs();
    let session = qualified.session();
    evidence
        .validate()
        .map_err(CalibrationEngineError::Evidence)?;
    manifest
        .validate()
        .map_err(CalibrationEngineError::Manifest)?;
    let manifest_sha256 = manifest
        .sha256()
        .map_err(CalibrationEngineError::Manifest)?;
    if parse_sha256(&evidence.acquisition_manifest_sha256) != Some(manifest_sha256) {
        return Err(CalibrationEngineError::ManifestIdentityMismatch);
    }
    let session_sha256 = session.sha256();
    if parse_sha256(&evidence.session_sha256) != Some(session_sha256) {
        return Err(CalibrationEngineError::SessionIdentityMismatch);
    }
    let clock_sha256 = clocks
        .fingerprint()
        .map_err(CalibrationEngineError::Clock)?;
    if parse_sha256(&evidence.clock_epochs_sha256) != Some(clock_sha256) {
        return Err(CalibrationEngineError::ClockIdentityMismatch);
    }

    reject_control_records(session.summary())?;
    manifest
        .validate_observed_topology(
            session.summary().left.received,
            session.summary().right.received,
        )
        .map_err(CalibrationEngineError::Topology)?;
    require_clean_stream(session.summary())?;
    if session.header().session_id != evidence.session_id {
        return Err(CalibrationEngineError::SessionIdentityMismatch);
    }
    let canonicalizer =
        FrameCanonicalizer::new(manifest).map_err(CalibrationEngineError::Manifest)?;

    let mut reader = session.reader();
    let mut frames = Vec::new();
    let mut record_ordinal = 0_u64;
    let mut hand_ordinals = [0_u64; 2];
    while let Some(record) = reader
        .next_record()
        .map_err(CalibrationEngineError::Session)?
    {
        match record.kind {
            SessionRecordKind::Decoded => {
                let frame =
                    record
                        .decode()
                        .map_err(|error| CalibrationEngineError::DecodedRecord {
                            record_ordinal,
                            error,
                        })?;
                let frame = canonicalizer.canonicalize(&frame).map_err(|error| {
                    CalibrationEngineError::FrameCanonicalization {
                        record_ordinal,
                        error,
                    }
                })?;
                let hand_index = match frame.hand {
                    Hand::Left => 0,
                    Hand::Right => 1,
                };
                frames.push(CapturedFrame {
                    record_ordinal,
                    hand_ordinal: hand_ordinals[hand_index],
                    host_time_ns: record.host_time_ns,
                    frame,
                });
                hand_ordinals[hand_index] = hand_ordinals[hand_index].saturating_add(1);
            }
            SessionRecordKind::Malformed
            | SessionRecordKind::Oversized
            | SessionRecordKind::Unterminated => {
                // The clean-stream summary check above rejects these before
                // reconstruction. Keep this match exhaustive so no record
                // taxonomy can become an implicit skip.
            }
            SessionRecordKind::QuiesceRequest | SessionRecordKind::QuiesceReceipt => {
                return Err(CalibrationEngineError::UnexpectedControlRecord { kind: record.kind });
            }
        }
        record_ordinal += 1;
    }

    let observations = bind_observations(&frames, &evidence.observations)?;
    Ok(BoundEvidence {
        session_sha256,
        frames,
        observations,
    })
}

fn bind_observations<'a>(
    frames: &[CapturedFrame],
    observations: &'a [CalibrationObservation],
) -> Result<Vec<(&'a CalibrationObservation, CalibrationExample)>, CalibrationEngineError> {
    // Capture selection normally reserves these source records before it
    // derives evidence. Admission repeats the check against the reconstructed
    // raw windows so persisted diagnostics can never make selector bookkeeping
    // authoritative. This set is intentionally local: fitting and held-out
    // sessions bind independently, while every truth window within one bind
    // invocation shares it.
    let mut truth_window_records = BTreeSet::new();
    let mut bound = Vec::with_capacity(observations.len());
    for observation in observations {
        let example = rebuild_example(frames, observation, &mut truth_window_records)?;
        bound.push((observation, example));
    }
    Ok(bound)
}

fn rebuild_example(
    frames: &[CapturedFrame],
    observation: &CalibrationObservation,
    truth_window_records: &mut BTreeSet<u64>,
) -> Result<CalibrationExample, CalibrationEngineError> {
    let end_index = frame_index_by_record(frames, observation.window_end_record).ok_or(
        CalibrationEngineError::MissingWindowEnd(observation.window_end_record),
    )?;
    let end = &frames[end_index];
    if end.frame.hand != observation.hand {
        return Err(CalibrationEngineError::WindowHandMismatch {
            record_ordinal: observation.window_end_record,
            expected: observation.hand,
            actual: end.frame.hand,
        });
    }
    let raw_offset = i128::from(end.host_time_ns) - i128::from(observation.trigger_host_time_ns);
    let offset =
        i64::try_from(raw_offset).map_err(|_| CalibrationEngineError::AlignmentOutOfRange {
            record_ordinal: observation.window_end_record,
        })?;
    if !(INTENT_WINDOW_START_NS..=INTENT_WINDOW_END_NS).contains(&offset) {
        return Err(CalibrationEngineError::AlignmentOutOfRange {
            record_ordinal: observation.window_end_record,
        });
    }

    let selected = preceding_hand_frames(frames, end_index, observation.hand).collect::<Vec<_>>();
    if selected.len() != WINDOW_FRAMES {
        return Err(CalibrationEngineError::ShortWindow {
            record_ordinal: observation.window_end_record,
            actual: selected.len(),
        });
    }
    if let Some(overlap) = selected
        .iter()
        .map(|frame| frame.record_ordinal)
        .find(|record_ordinal| truth_window_records.contains(record_ordinal))
    {
        return Err(CalibrationEngineError::TruthWindowRecordOverlap(overlap));
    }
    truth_window_records.extend(selected.iter().map(|frame| frame.record_ordinal));

    let mut selected = selected
        .into_iter()
        .map(|frame| frame.frame)
        .collect::<Vec<_>>();
    selected.reverse();
    let window = TapWindow::new(selected).map_err(CalibrationEngineError::Window)?;
    Ok(CalibrationExample {
        position: observation.position,
        window,
    })
}

fn frame_index_by_record(frames: &[CapturedFrame], record_ordinal: u64) -> Option<usize> {
    frames
        .binary_search_by_key(&record_ordinal, |frame| frame.record_ordinal)
        .ok()
}

fn frame_by_record(frames: &[CapturedFrame], record_ordinal: u64) -> Option<&CapturedFrame> {
    frame_index_by_record(frames, record_ordinal).map(|index| &frames[index])
}

fn preceding_hand_frames(
    frames: &[CapturedFrame],
    end_index: usize,
    hand: Hand,
) -> impl Iterator<Item = &CapturedFrame> {
    frames[..=end_index]
        .iter()
        .rev()
        .filter(move |frame| frame.frame.hand == hand)
        .take(WINDOW_FRAMES)
}

fn require_clean_stream(summary: &SessionSummary) -> Result<(), CalibrationEngineError> {
    if !summary.is_clean_two_hand_sensor_stream() {
        return Err(CalibrationEngineError::StreamIntegrity);
    }
    Ok(())
}

fn reject_control_records(summary: &SessionSummary) -> Result<(), CalibrationEngineError> {
    if summary.quiesce_requests != 0 {
        return Err(CalibrationEngineError::UnexpectedControlRecord {
            kind: SessionRecordKind::QuiesceRequest,
        });
    }
    if summary.quiesce_receipts != 0 {
        return Err(CalibrationEngineError::UnexpectedControlRecord {
            kind: SessionRecordKind::QuiesceReceipt,
        });
    }
    Ok(())
}

fn artifact_sources(
    evidence: &CalibrationEvidence,
    session_sha256: [u8; 32],
) -> Vec<ArtifactSource> {
    let mut groups = BTreeMap::<(EvidenceRole, EvidenceDomain), BTreeSet<u32>>::new();
    for observation in &evidence.observations {
        groups
            .entry((observation.role, observation.domain))
            .or_default()
            .insert(observation.block_id);
    }
    groups
        .into_iter()
        .map(|((role, domain), blocks)| {
            let role = match role {
                EvidenceRole::Training => ArtifactSourceRole::Training,
                EvidenceRole::ThresholdSelection => ArtifactSourceRole::ThresholdSelection,
                EvidenceRole::HeldOutValidation => {
                    unreachable!("fitting evidence excludes held-out observations")
                }
            };
            let domain = match domain {
                EvidenceDomain::Keyboard => ArtifactDomain::Keyboard,
                EvidenceDomain::PassiveSurface => ArtifactDomain::PassiveSurface,
            };
            ArtifactSource::new(session_sha256, role, domain, blocks.into_iter().collect())
        })
        .collect()
}

fn artifact_sensor_bindings(manifest: &AcquisitionManifest) -> Vec<ArtifactSensorBinding> {
    manifest
        .hands
        .iter()
        .flat_map(|hand_manifest| {
            let hand = match hand_manifest.hand {
                ManifestHand::Left => Hand::Left,
                ManifestHand::Right => Hand::Right,
            };
            hand_manifest
                .sensors
                .iter()
                .cloned()
                .map(move |sensor| ArtifactSensorBinding { hand, sensor })
        })
        .collect()
}

#[derive(Debug)]
pub enum CalibrationEngineError {
    Evidence(CalibrationEvidenceError),
    Capture(CalibrationCaptureError),
    Manifest(ManifestError),
    Topology(ObservedTopologyError),
    Clock(ClockEpochError),
    Session(SessionError),
    DecodedRecord {
        record_ordinal: u64,
        error: RecordDecodeError,
    },
    FrameCanonicalization {
        record_ordinal: u64,
        error: FrameCanonicalizationError,
    },
    Window(WindowError),
    Pipeline(PipelineError),
    KeyState(ApplyAnomaly),
    Decoder {
        hand: Hand,
        error: CalibrationError,
    },
    Artifact(ArtifactError),
    ValidationReport(ValidationReportError),
    ValidationEvidence(ValidationEvidenceError),
    MatchingRule(CalibrationCaptureError),
    NeutralEvidence(NeutralEvidenceError),
    WrongPurpose,
    ThresholdDomainMismatch,
    ManifestIdentityMismatch,
    SessionIdentityMismatch,
    ClockIdentityMismatch,
    UnexpectedControlRecord {
        kind: SessionRecordKind,
    },
    ArtifactManifestMismatch,
    KeyboardIdentityMismatch,
    MatchingRuleIdentityMismatch,
    NeutralNotStable,
    StreamIntegrity,
    MissingWindowEnd(u64),
    WindowHandMismatch {
        record_ordinal: u64,
        expected: Hand,
        actual: Hand,
    },
    AlignmentOutOfRange {
        record_ordinal: u64,
    },
    ShortWindow {
        record_ordinal: u64,
        actual: usize,
    },
    TruthWindowRecordOverlap(u64),
    MissingDecoder(Hand),
    CoverageIncomplete {
        domain: EvidenceDomain,
        position: PhysicalPosition,
        actual: u16,
    },
}

impl fmt::Display for CalibrationEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Evidence(error) => write!(formatter, "calibration evidence failed: {error}"),
            Self::Capture(error) => write!(formatter, "calibration capture failed: {error}"),
            Self::Manifest(error) => write!(formatter, "calibration manifest failed: {error}"),
            Self::Topology(error) => write!(formatter, "calibration topology failed: {error}"),
            Self::Clock(error) => write!(formatter, "calibration clock binding failed: {error}"),
            Self::Session(error) => write!(formatter, "calibration session failed: {error}"),
            Self::DecodedRecord {
                record_ordinal,
                error,
            } => write!(
                formatter,
                "calibration record {record_ordinal} failed to decode: {error}"
            ),
            Self::FrameCanonicalization {
                record_ordinal,
                error,
            } => write!(
                formatter,
                "calibration record {record_ordinal} cannot enter canonical sensor coordinates: {error}"
            ),
            Self::Window(error) => write!(formatter, "calibration window failed: {error}"),
            Self::Pipeline(error) => write!(formatter, "calibration pipeline failed: {error}"),
            Self::KeyState(error) => {
                write!(formatter, "calibration key state failed: {error:?}")
            }
            Self::Decoder { hand, error } => {
                write!(formatter, "{hand:?} calibration failed: {error}")
            }
            Self::Artifact(error) => write!(formatter, "calibration artifact failed: {error}"),
            Self::ValidationReport(error) => {
                write!(formatter, "calibration validation report failed: {error}")
            }
            Self::ValidationEvidence(error) => {
                write!(formatter, "calibration validation evidence failed: {error}")
            }
            Self::MatchingRule(error) => {
                write!(formatter, "calibration matching rule failed: {error}")
            }
            Self::NeutralEvidence(error) => {
                write!(formatter, "calibration neutral evidence failed: {error}")
            }
            Self::WrongPurpose => formatter.write_str("calibration evidence has the wrong purpose"),
            Self::ThresholdDomainMismatch => formatter
                .write_str("threshold-selection evidence must come from the passive surface"),
            Self::ManifestIdentityMismatch => {
                formatter.write_str("calibration evidence does not bind the manifest")
            }
            Self::SessionIdentityMismatch => {
                formatter.write_str("calibration evidence does not bind the session")
            }
            Self::ClockIdentityMismatch => {
                formatter.write_str("calibration evidence does not bind the clock epochs")
            }
            Self::UnexpectedControlRecord { kind } => write!(
                formatter,
                "calibration session contains a {kind:?} Gate 0A control record"
            ),
            Self::ArtifactManifestMismatch => {
                formatter.write_str("calibration artifact does not bind the validation manifest")
            }
            Self::KeyboardIdentityMismatch => {
                formatter.write_str("held-out evidence uses a different physical keyboard")
            }
            Self::MatchingRuleIdentityMismatch => {
                formatter.write_str("calibration evidence does not bind the frozen matching rule")
            }
            Self::NeutralNotStable => {
                formatter.write_str("neutral evidence did not pass every stability gate")
            }
            Self::StreamIntegrity => {
                formatter.write_str("calibration session failed the lossless stream-integrity gate")
            }
            Self::MissingWindowEnd(record) => write!(
                formatter,
                "calibration window end record {record} does not exist"
            ),
            Self::WindowHandMismatch {
                record_ordinal,
                expected,
                actual,
            } => write!(
                formatter,
                "calibration record {record_ordinal} is {actual:?}; expected {expected:?}"
            ),
            Self::AlignmentOutOfRange { record_ordinal } => write!(
                formatter,
                "calibration record {record_ordinal} is outside the frozen label-alignment window"
            ),
            Self::ShortWindow {
                record_ordinal,
                actual,
            } => write!(
                formatter,
                "calibration record {record_ordinal} has {actual} preceding frames; expected {WINDOW_FRAMES}"
            ),
            Self::TruthWindowRecordOverlap(record_ordinal) => write!(
                formatter,
                "calibration truth windows reuse source record {record_ordinal}"
            ),
            Self::MissingDecoder(hand) => {
                write!(formatter, "calibration artifact has no {hand:?} decoder")
            }
            Self::CoverageIncomplete {
                domain,
                position,
                actual,
            } => write!(
                formatter,
                "calibration {domain:?} coverage for {position:?} has {actual} accepted windows"
            ),
        }
    }
}

impl Error for CalibrationEngineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Evidence(error) => Some(error),
            Self::Capture(error) => Some(error),
            Self::Manifest(error) => Some(error),
            Self::Topology(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::DecodedRecord { error, .. } => Some(error),
            Self::FrameCanonicalization { error, .. } => Some(error),
            Self::Window(error) => Some(error),
            Self::Pipeline(error) => Some(error),
            Self::Decoder { error, .. } => Some(error),
            Self::Artifact(error) => Some(error),
            Self::ValidationReport(error) => Some(error),
            Self::ValidationEvidence(error) => Some(error),
            Self::MatchingRule(error) => Some(error),
            Self::NeutralEvidence(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::Finger,
        protocol::{ALL_SENSORS_VALID, SENSOR_COUNT, SensorFrame, SensorReading},
    };

    const FRAME_PERIOD_NS: u64 = 5_000_000;

    fn captured_frame(record_ordinal: u64, hand: Hand, hand_ordinal: u64) -> CapturedFrame {
        CapturedFrame {
            record_ordinal,
            hand_ordinal,
            host_time_ns: 1_000_000_000 + record_ordinal * FRAME_PERIOD_NS,
            frame: SensorFrame {
                hand,
                controller_id_sha256: [1 + hand as u8; 32],
                boot_id: [1 + hand as u8; 16],
                flags: 0,
                valid_sensor_mask: ALL_SENSORS_VALID,
                sequence: u32::try_from(hand_ordinal).expect("test hand ordinal fits u32"),
                device_time_us: hand_ordinal * FRAME_PERIOD_NS / 1_000,
                sensors: [SensorReading::default(); SENSOR_COUNT],
            },
        }
    }

    fn left_frames(count: u64) -> Vec<CapturedFrame> {
        (0..count)
            .map(|ordinal| captured_frame(ordinal, Hand::Left, ordinal))
            .collect()
    }

    fn interleaved_frames(per_hand: u64) -> Vec<CapturedFrame> {
        (0..per_hand)
            .flat_map(|hand_ordinal| {
                [Hand::Left, Hand::Right].into_iter().map(move |hand| {
                    let record_ordinal = hand_ordinal * 2 + hand as u64;
                    captured_frame(record_ordinal, hand, hand_ordinal)
                })
            })
            .collect()
    }

    fn target_observation(window_end_record: u64, block_id: u32) -> CalibrationObservation {
        CalibrationObservation {
            role: EvidenceRole::Training,
            domain: EvidenceDomain::PassiveSurface,
            block_id,
            position: Some(PhysicalPosition {
                hand: Hand::Left,
                finger: Finger::Index,
                slot: 0,
            }),
            null_kind: None,
            hand: Hand::Left,
            window_end_record,
            trigger_host_time_ns: 1_000_000_000 + window_end_record * FRAME_PERIOD_NS,
        }
    }

    #[test]
    fn binder_rejects_intersecting_target_truth_windows() {
        let frames = left_frames(32);
        let observations = [target_observation(19, 1), target_observation(31, 2)];

        assert!(matches!(
            bind_observations(&frames, &observations),
            Err(CalibrationEngineError::TruthWindowRecordOverlap(19))
        ));
    }

    #[test]
    fn binder_rejects_target_intersection_with_threshold_null_truth() {
        let frames = left_frames(32);
        let target = target_observation(19, 1);
        let threshold_null = CalibrationObservation {
            role: EvidenceRole::ThresholdSelection,
            domain: EvidenceDomain::PassiveSurface,
            block_id: 2,
            position: None,
            null_kind: Some(EvidenceNullKind::Quiet),
            hand: Hand::Left,
            window_end_record: 31,
            trigger_host_time_ns: 1_000_000_000 + 31 * FRAME_PERIOD_NS,
        };

        assert!(matches!(
            bind_observations(&frames, &[target, threshold_null]),
            Err(CalibrationEngineError::TruthWindowRecordOverlap(19))
        ));
    }

    #[test]
    fn binder_allows_simultaneous_opposite_hand_truth_windows() {
        let frames = interleaved_frames(16);
        let left = target_observation(30, 1);
        let right = CalibrationObservation {
            position: Some(PhysicalPosition {
                hand: Hand::Right,
                finger: Finger::Index,
                slot: 0,
            }),
            hand: Hand::Right,
            window_end_record: 31,
            trigger_host_time_ns: 1_000_000_000 + 31 * FRAME_PERIOD_NS,
            ..left
        };

        assert_eq!(
            bind_observations(&frames, &[left, right])
                .expect("opposite-hand source records are disjoint")
                .len(),
            2
        );
    }

    #[test]
    fn clean_stream_gate_rejects_gate0_controls_before_generic_integrity() {
        for (summary, expected) in [
            (
                SessionSummary {
                    quiesce_requests: 1,
                    ..SessionSummary::default()
                },
                SessionRecordKind::QuiesceRequest,
            ),
            (
                SessionSummary {
                    quiesce_receipts: 1,
                    ..SessionSummary::default()
                },
                SessionRecordKind::QuiesceReceipt,
            ),
        ] {
            assert!(matches!(
                reject_control_records(&summary),
                Err(CalibrationEngineError::UnexpectedControlRecord { kind }) if kind == expected
            ));
        }
    }
}
