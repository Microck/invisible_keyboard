//! Count-level held-out validation, model-use admission, and evidence binding.

use std::{collections::BTreeSet, error::Error, fmt};

use serde::{Deserialize, Serialize};

pub use crate::calibration_engine::{HeldOutValidation, WearSessionConfirmation};

use crate::{
    calibration::{BlockId, HeldOutValidationMetrics, LockedArtifactIdentity},
    layout::{KeyUsage, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots, position_index},
    manifest::{format_sha256, parse_sha256, sha256},
};

pub const VALIDATION_REPORT_SCHEMA: &str = "invisible-keyboard-validation-report-v2";
pub const TRUTH_CLASS_COUNT: usize = PHYSICAL_POSITION_COUNT + 1;
pub const PREDICTION_CLASS_COUNT: usize = 29;
pub const NULL_PREDICTION_COLUMN: usize = PREDICTION_CLASS_COUNT - 1;
pub const MIN_VALIDATION_EXAMPLES_PER_POSITION: u32 = 3;
/// Minimum observed ordinary-motion exposure in held-out evidence.
pub const MIN_HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS: u64 = 60_000;
pub const MAX_VALIDATION_REPORT_BYTES: usize = 262_144;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TruthClass {
    Position(PhysicalPosition),
    Null,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PredictionClass {
    Key(KeyUsage),
    Null,
}

/// Rows are 34 physical truths plus null. Columns are 28 Core-28 outputs plus
/// null. Reserved positions remain distinct truth rows but have no output
/// column because correct product behavior is abstention.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationMatrix {
    cells: Vec<u32>,
}

impl Default for ValidationMatrix {
    fn default() -> Self {
        Self::new()
    }
}

impl ValidationMatrix {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cells: vec![0; TRUTH_CLASS_COUNT * PREDICTION_CLASS_COUNT],
        }
    }

    pub fn observe(&mut self, truth: TruthClass, prediction: PredictionClass) {
        let row = truth_row(truth);
        let column = prediction_column(prediction);
        let index = row * PREDICTION_CLASS_COUNT + column;
        self.cells[index] = self.cells[index].saturating_add(1);
    }

    #[must_use]
    pub fn cells(&self) -> &[u32] {
        &self.cells
    }

    #[must_use]
    pub fn count(&self, truth_row: usize, prediction_column: usize) -> u32 {
        self.cells[truth_row * PREDICTION_CLASS_COUNT + prediction_column]
    }

    #[must_use]
    pub fn truth_total(&self, truth_row: usize) -> u64 {
        (0..PREDICTION_CLASS_COUNT)
            .map(|column| u64::from(self.count(truth_row, column)))
            .sum()
    }

    /// Clear one truth row for deterministic negative fixtures.
    pub fn clear_truth_row(&mut self, truth_row: usize) {
        let start = truth_row * PREDICTION_CLASS_COUNT;
        self.cells[start..start + PREDICTION_CLASS_COUNT].fill(0);
    }

    fn validate_shape(&self) -> Result<(), ValidationReportError> {
        let expected = TRUTH_CLASS_COUNT * PREDICTION_CLASS_COUNT;
        if self.cells.len() != expected {
            return Err(ValidationReportError::InvalidMatrixShape {
                expected,
                actual: self.cells.len(),
            });
        }
        Ok(())
    }

    fn metrics(&self) -> HeldOutValidationMetrics {
        let mut f1_sum = 0_u64;
        let mut minimum_recall = 10_000_u16;
        let mut enabled_count = 0_u64;
        for (truth_index, slot) in all_slots().iter().enumerate() {
            let Some(usage) = slot.usage else {
                continue;
            };
            let column = prediction_column(PredictionClass::Key(usage));
            let true_positive = u64::from(self.count(truth_index, column));
            let false_negative = self.truth_total(truth_index) - true_positive;
            let false_positive = (0..TRUTH_CLASS_COUNT)
                .filter(|&row| row != truth_index)
                .map(|row| u64::from(self.count(row, column)))
                .sum::<u64>();
            let recall_denominator = true_positive + false_negative;
            let recall = basis_points(true_positive, recall_denominator);
            let f1 = basis_points(
                true_positive.saturating_mul(2),
                true_positive
                    .saturating_mul(2)
                    .saturating_add(false_positive)
                    .saturating_add(false_negative),
            );
            minimum_recall = minimum_recall.min(recall);
            f1_sum += u64::from(f1);
            enabled_count += 1;
        }
        HeldOutValidationMetrics::new(
            u16::try_from(f1_sum / enabled_count).expect("basis-point average fits u16"),
            minimum_recall,
        )
        .expect("derived basis points are bounded")
    }
}

fn basis_points(numerator: u64, denominator: u64) -> u16 {
    if denominator == 0 {
        return 0;
    }
    u16::try_from(numerator.saturating_mul(10_000) / denominator)
        .expect("ratio basis points fit u16")
}

fn truth_row(truth: TruthClass) -> usize {
    match truth {
        TruthClass::Position(position) => {
            position_index(position).expect("validation truth belongs to the physical layout")
        }
        TruthClass::Null => PHYSICAL_POSITION_COUNT,
    }
}

fn prediction_column(prediction: PredictionClass) -> usize {
    match prediction {
        PredictionClass::Key(usage) => all_slots()
            .iter()
            .filter_map(|slot| slot.usage)
            .position(|candidate| candidate == usage)
            .expect("validation prediction belongs to Core-28"),
        PredictionClass::Null => NULL_PREDICTION_COLUMN,
    }
}

/// Evidence needed to construct a versioned held-out report.
pub struct ValidationReportInput {
    pub artifact_sha256: [u8; 32],
    pub acquisition_manifest_sha256: [u8; 32],
    pub evaluator_version: String,
    pub session_sha256: Vec<[u8; 32]>,
    pub evidence_sha256: Vec<[u8; 32]>,
    pub block_ids: Vec<u32>,
    pub confusion: ValidationMatrix,
    pub ordinary_motion_exposure_ms: u64,
    pub key_state_neutral: bool,
}

/// Immutable validation evidence. Aggregate metrics are always derived.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationReport {
    pub schema: String,
    pub artifact_sha256: String,
    pub acquisition_manifest_sha256: String,
    pub evaluator_version: String,
    pub session_sha256: Vec<String>,
    pub evidence_sha256: Vec<String>,
    pub block_ids: Vec<u32>,
    pub confusion: ValidationMatrix,
    pub ordinary_motion_exposure_ms: u64,
    pub key_state_neutral: bool,
}

impl ValidationReport {
    /// Construct a structurally valid report. Held-out coverage and identity
    /// binding remain separate checks because incomplete evidence must remain
    /// inspectable rather than becoming an unparseable file.
    pub fn new(input: ValidationReportInput) -> Result<Self, ValidationReportError> {
        let report = Self {
            schema: VALIDATION_REPORT_SCHEMA.to_owned(),
            artifact_sha256: format_sha256(&input.artifact_sha256),
            acquisition_manifest_sha256: format_sha256(&input.acquisition_manifest_sha256),
            evaluator_version: input.evaluator_version,
            session_sha256: input.session_sha256.iter().map(format_sha256).collect(),
            evidence_sha256: input.evidence_sha256.iter().map(format_sha256).collect(),
            block_ids: input.block_ids,
            confusion: input.confusion,
            ordinary_motion_exposure_ms: input.ordinary_motion_exposure_ms,
            key_state_neutral: input.key_state_neutral,
        };
        report.validate_structure()?;
        Ok(report)
    }

    pub fn from_json(source: &[u8]) -> Result<Self, ValidationReportError> {
        if source.len() > MAX_VALIDATION_REPORT_BYTES {
            return Err(ValidationReportError::TooLarge {
                actual: source.len(),
            });
        }
        let report = serde_json::from_slice::<Self>(source)
            .map_err(|error| ValidationReportError::Json(error.to_string()))?;
        report.validate_structure()?;
        Ok(report)
    }

    pub fn validate_structure(&self) -> Result<(), ValidationReportError> {
        if self.schema != VALIDATION_REPORT_SCHEMA {
            return Err(ValidationReportError::InvalidSchema);
        }
        require_hash(&self.artifact_sha256, "artifact_sha256")?;
        require_hash(
            &self.acquisition_manifest_sha256,
            "acquisition_manifest_sha256",
        )?;
        if self.evaluator_version.trim().is_empty() {
            return Err(ValidationReportError::MissingEvaluatorVersion);
        }
        if self.session_sha256.is_empty() {
            return Err(ValidationReportError::MissingSessions);
        }
        let mut sessions = BTreeSet::new();
        for hash in &self.session_sha256 {
            require_hash(hash, "session_sha256")?;
            if !sessions.insert(hash.as_str()) {
                return Err(ValidationReportError::DuplicateSession);
            }
        }
        if self.evidence_sha256.len() != self.session_sha256.len() {
            return Err(ValidationReportError::EvidenceSessionCountMismatch);
        }
        let mut evidence = BTreeSet::new();
        for hash in &self.evidence_sha256 {
            require_hash(hash, "evidence_sha256")?;
            if !evidence.insert(hash.as_str()) {
                return Err(ValidationReportError::DuplicateEvidence);
            }
        }
        if self.block_ids.is_empty() {
            return Err(ValidationReportError::MissingBlocks);
        }
        let mut blocks = BTreeSet::new();
        for &block in &self.block_ids {
            if block == 0 {
                return Err(ValidationReportError::InvalidBlockId);
            }
            if !blocks.insert(block) {
                return Err(ValidationReportError::DuplicateBlock(block));
            }
        }
        self.confusion.validate_shape()?;
        if self.ordinary_motion_exposure_ms == 0 {
            return Err(ValidationReportError::MissingOrdinaryMotionExposure);
        }
        Ok(())
    }

    /// Bind the report to one locked artifact and prove class coverage.
    pub fn validate_against(
        &self,
        identity: &LockedArtifactIdentity,
        validation_block: BlockId,
        training_block_ids: &[u32],
    ) -> Result<HeldOutValidationMetrics, ValidationEvidenceError> {
        if parse_sha256(&self.artifact_sha256) != Some(*identity.fingerprint()) {
            return Err(ValidationEvidenceError::ArtifactMismatch);
        }
        if parse_sha256(&self.acquisition_manifest_sha256) != Some(*identity.manifest_sha256()) {
            return Err(ValidationEvidenceError::ManifestMismatch);
        }
        for session in &self.session_sha256 {
            let hash = parse_sha256(session).ok_or(ValidationEvidenceError::InvalidReport)?;
            if identity.source_sessions().contains(&hash) {
                return Err(ValidationEvidenceError::TrainingSessionOverlap);
            }
        }
        if !self.block_ids.contains(&validation_block.value()) {
            return Err(ValidationEvidenceError::MissingValidationBlock);
        }
        for &block in &self.block_ids {
            if training_block_ids.contains(&block) {
                return Err(ValidationEvidenceError::TrainingBlockOverlap(block));
            }
        }
        for (index, slot) in all_slots().iter().enumerate() {
            let actual = self.confusion.truth_total(index);
            if actual < u64::from(MIN_VALIDATION_EXAMPLES_PER_POSITION) {
                return Err(ValidationEvidenceError::MissingPositionEvidence {
                    position: slot.position,
                    actual: u32::try_from(actual).unwrap_or(u32::MAX),
                });
            }
        }
        if self.confusion.truth_total(PHYSICAL_POSITION_COUNT) == 0 {
            return Err(ValidationEvidenceError::MissingNullEvidence);
        }
        Ok(self.confusion.metrics())
    }

    #[must_use]
    pub fn reserved_activations(&self) -> u64 {
        all_slots()
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.usage.is_none())
            .map(|(row, _)| {
                (0..NULL_PREDICTION_COLUMN)
                    .map(|column| u64::from(self.confusion.count(row, column)))
                    .sum::<u64>()
            })
            .sum()
    }

    #[must_use]
    pub fn ordinary_motion_activations(&self) -> u64 {
        (0..NULL_PREDICTION_COLUMN)
            .map(|column| u64::from(self.confusion.count(PHYSICAL_POSITION_COUNT, column)))
            .sum()
    }

    #[must_use]
    pub fn passes_held_out_validation(&self, metrics: HeldOutValidationMetrics) -> bool {
        metrics.passes()
            && self.reserved_activations() == 0
            && self.ordinary_motion_activations() == 0
            && self.ordinary_motion_exposure_ms >= MIN_HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS
            && self.key_state_neutral
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, ValidationReportError> {
        self.validate_structure()?;
        serde_json::to_vec(self)
            .map_err(|error| ValidationReportError::Serialization(error.to_string()))
    }

    pub fn fingerprint(&self) -> Result<[u8; 32], ValidationReportError> {
        Ok(sha256(&self.canonical_json()?))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum HeldOutValidationError {
    Report(ValidationReportError),
    ThresholdsFailed {
        metrics: HeldOutValidationMetrics,
        reserved_activations: u64,
        ordinary_motion_activations: u64,
        ordinary_motion_exposure_ms: u64,
        key_state_neutral: bool,
    },
}

fn require_hash(value: &str, field: &'static str) -> Result<(), ValidationReportError> {
    parse_sha256(value)
        .map(|_| ())
        .ok_or(ValidationReportError::InvalidSha256(field))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationReportError {
    TooLarge { actual: usize },
    Json(String),
    Serialization(String),
    InvalidSchema,
    InvalidSha256(&'static str),
    MissingEvaluatorVersion,
    MissingSessions,
    DuplicateSession,
    EvidenceSessionCountMismatch,
    DuplicateEvidence,
    MissingBlocks,
    InvalidBlockId,
    DuplicateBlock(u32),
    InvalidMatrixShape { expected: usize, actual: usize },
    MissingOrdinaryMotionExposure,
}

impl fmt::Display for ValidationReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                formatter,
                "validation report is {actual} bytes; maximum is {MAX_VALIDATION_REPORT_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "validation report JSON is invalid: {error}"),
            Self::Serialization(error) => {
                write!(formatter, "validation report serialization failed: {error}")
            }
            Self::InvalidSchema => formatter.write_str("validation report schema is invalid"),
            Self::InvalidSha256(field) => {
                write!(formatter, "validation field {field} is not SHA-256")
            }
            Self::MissingEvaluatorVersion => {
                formatter.write_str("validation evaluator version is empty")
            }
            Self::MissingSessions => formatter.write_str("validation has no source sessions"),
            Self::DuplicateSession => formatter.write_str("validation repeats a source session"),
            Self::EvidenceSessionCountMismatch => formatter
                .write_str("validation evidence hashes do not match the source-session count"),
            Self::DuplicateEvidence => {
                formatter.write_str("validation repeats a source evidence sidecar")
            }
            Self::MissingBlocks => formatter.write_str("validation has no block IDs"),
            Self::InvalidBlockId => formatter.write_str("validation block IDs must be nonzero"),
            Self::DuplicateBlock(block) => write!(formatter, "validation repeats block {block}"),
            Self::InvalidMatrixShape { expected, actual } => write!(
                formatter,
                "validation matrix has {actual} cells; expected {expected}"
            ),
            Self::MissingOrdinaryMotionExposure => {
                formatter.write_str("validation has no ordinary-motion exposure")
            }
        }
    }
}

impl Error for ValidationReportError {}

impl fmt::Display for HeldOutValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Report(error) => write!(formatter, "validation report is invalid: {error}"),
            Self::ThresholdsFailed {
                metrics,
                reserved_activations,
                ordinary_motion_activations,
                ordinary_motion_exposure_ms,
                key_state_neutral,
            } => write!(
                formatter,
                "held-out validation failed: macro_f1={} minimum_recall={} reserved_activations={} ordinary_motion_activations={} ordinary_motion_exposure_ms={} key_state_neutral={}",
                metrics.macro_f1_basis_points,
                metrics.minimum_recall_basis_points,
                reserved_activations,
                ordinary_motion_activations,
                ordinary_motion_exposure_ms,
                key_state_neutral
            ),
        }
    }
}

impl Error for HeldOutValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Report(error) => Some(error),
            Self::ThresholdsFailed { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationEvidenceError {
    InvalidReport,
    ArtifactMismatch,
    ManifestMismatch,
    TrainingSessionOverlap,
    MissingValidationBlock,
    TrainingBlockOverlap(u32),
    MissingPositionEvidence {
        position: PhysicalPosition,
        actual: u32,
    },
    MissingNullEvidence,
}

impl fmt::Display for ValidationEvidenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReport => formatter.write_str("validation report is invalid"),
            Self::ArtifactMismatch => {
                formatter.write_str("validation belongs to another model artifact")
            }
            Self::ManifestMismatch => {
                formatter.write_str("validation belongs to another acquisition manifest")
            }
            Self::TrainingSessionOverlap => {
                formatter.write_str("validation reuses an artifact source session")
            }
            Self::MissingValidationBlock => {
                formatter.write_str("validation omits the reserved held-out block")
            }
            Self::TrainingBlockOverlap(block) => {
                write!(formatter, "validation reuses training block {block}")
            }
            Self::MissingPositionEvidence { position, actual } => write!(
                formatter,
                "validation has {actual} examples for {position:?}; expected at least {MIN_VALIDATION_EXAMPLES_PER_POSITION}"
            ),
            Self::MissingNullEvidence => {
                formatter.write_str("validation has no null/ordinary-motion examples")
            }
        }
    }
}

impl Error for ValidationEvidenceError {}
